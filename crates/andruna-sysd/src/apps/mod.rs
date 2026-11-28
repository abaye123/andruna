//! The app process lifecycle: start a package's host process, talk to it,
//! notice how it ended.
//!
//! A start ([`start_host`]), following ADR 0035 and ADR 0036:
//!
//! 1. Two anonymous pipes for the host channel (`andruna_ipc::host`) and
//!    two for the app's standard output and error.
//! 2. `andruna-app --host` is created **suspended** in the package's
//!    AppContainer and in sysd's kill-on-close job
//!    ([`crate::appcontainer::launch`]); it inherits exactly its four pipe
//!    ends; `ANDRUNA_HOST_CHANNEL` tells it the channel's handle values.
//! 3. sysd maps the read-only boot image view at the image base into it
//!    ([`bootimage::BootImage::map_into`]), then resumes it.
//! 4. On the host's `Hello`, sysd sends `Attach` (package, data directory,
//!    entry, the view's layout) and waits for `Running` or `Refused`.
//! 5. While it runs, sysd logs its output (keeping the last lines of
//!    standard error) and stores its missing-function reports.
//! 6. When it ends, sysd builds an [`ExitReport`]: the host's `Exit`
//!    status if it sent one, the process exit code, and a crash report
//!    from standard error ([`crash`]) when it did not end cleanly.
//!
//! The supervisor ([`crate::supervisor`]) keeps one process per package
//! and publishes crashes as events.

pub mod bootimage;
pub mod crash;
pub mod store;
mod win;

pub use win::Memory;

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::os::windows::io::{AsHandle, BorrowedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use andruna_ipc::host::{self, Attach, Crash, Entry, FromHost, ToHost};

use crate::Layout;
use crate::appcontainer::{self, ContainedProcess, Launch, SandboxError};
use crate::{error, info, warn};

use bootimage::BootImage;
use store::{InstalledPackage, Launchable};

/// The environment variable with the host channel's handle values.
pub const CHANNEL_ENV: &str = "ANDRUNA_HOST_CHANNEL";

/// How long a host may take from creation to `Running`.
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(20);

/// Output lines logged per process; the rest is counted only.
const MAX_LOGGED_LINES: usize = 2000;

/// How app hosts are started.
#[derive(Clone, Debug)]
pub struct HostConfig {
    /// `andruna-app.exe`.
    pub exe: PathBuf,
    /// The ART root (`lib64`, `bin`, `javalib`).
    pub art_root: PathBuf,
    /// Map the boot image view into each host (ADR 0035). Off, ART reads
    /// the image files itself.
    pub map_boot_image: bool,
    /// Extra runtime options for every app.
    pub runtime_options: Vec<String>,
    /// Extra environment entries (`NAME=value`) for every app, for
    /// diagnostics such as `ANDRUNA_PROFILE=1`.
    pub environment: Vec<String>,
}

impl HostConfig {
    /// The production layout: `andruna-app.exe` and `art\` next to sysd's
    /// own executable.
    pub fn beside(dir: &Path) -> Self {
        Self {
            exe: dir.join("andruna-app.exe"),
            art_root: dir.join("art"),
            map_boot_image: true,
            runtime_options: Vec::new(),
            environment: Vec::new(),
        }
    }
}

/// Times of one start, from just before process creation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StartTimes {
    /// Creation of the suspended process.
    pub created_ms: f64,
    /// Mapping the boot image view (0 when not mapped).
    pub mapped_ms: f64,
    /// Resumed.
    pub resumed_ms: f64,
    /// `Hello` received.
    pub hello_ms: f64,
    /// `Running` received (the runtime starts now).
    pub running_ms: f64,
}

/// How a host ended.
#[derive(Clone, Debug, PartialEq)]
pub struct ExitReport {
    pub package: String,
    pub pid: u32,
    /// The process exit code.
    pub exit_code: u32,
    /// The status the app ended with (`Exit` from the host), if it sent one.
    pub status: Option<i32>,
    /// Set when the app crashed (not for a clean exit, nor when sysd
    /// stopped it).
    pub crash: Option<Crash>,
    /// sysd stopped or killed it.
    pub stopped: bool,
    /// From start to end.
    pub run_time: Duration,
    pub times: StartTimes,
}

/// A failed start.
#[derive(Debug)]
pub enum StartError {
    /// The process could not be created (or its view not mapped).
    Launch(String),
    /// The host refused the assignment, or ended before it ran.
    Refused(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Launch(m) | Self::Refused(m) => f.write_str(m),
        }
    }
}

#[derive(Debug, Default)]
struct Progress {
    hello: Option<Duration>,
    running: Option<Duration>,
    refused: Option<String>,
    exit_status: Option<(i32, Option<Crash>)>,
    /// The channel closed (the host ended or closed it).
    closed: bool,
}

/// A running host process.
#[derive(Debug)]
pub struct HostProcess {
    pub package: String,
    pub pid: u32,
    process: ContainedProcess,
    to_host: Mutex<Option<File>>,
    start: Instant,
    times: Mutex<StartTimes>,
    stopped: AtomicBool,
    progress: Mutex<Progress>,
    changed: Condvar,
    stderr_tail: Mutex<crash::Tail>,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

impl HostProcess {
    pub fn process(&self) -> BorrowedHandle<'_> {
        self.process.process()
    }

    /// The process's memory counters now.
    pub fn memory(&self) -> io::Result<Memory> {
        win::memory(self.process.process())
    }

    pub fn times(&self) -> StartTimes {
        *self.times.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, m: &ToHost) -> io::Result<()> {
        let mut g = self.to_host.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_mut() {
            Some(w) => host::send(w, &m.encode()),
            None => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    /// Asks the host to end; kills it if it has not ended after `grace`.
    pub fn stop(&self, grace: Duration) {
        self.stopped.store(true, Ordering::SeqCst);
        if self.send(&ToHost::Stop).is_ok() && matches!(self.process.wait(grace), Ok(Some(_))) {
            return;
        }
        self.kill();
    }

    /// Ends the process now.
    pub fn kill(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.process.kill();
    }

    pub fn was_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Waits for `Running`; the refusal or early end otherwise.
    fn wait_running(&self, timeout: Duration) -> Result<(), StartError> {
        let deadline = Instant::now() + timeout;
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if p.running.is_some() {
                return Ok(());
            }
            if let Some(m) = &p.refused {
                return Err(StartError::Refused(m.clone()));
            }
            if p.closed {
                let tail = self
                    .stderr_tail
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .lines();
                let last = tail.last().cloned().unwrap_or_default();
                return Err(StartError::Refused(format!(
                    "the host ended before it ran the app{}",
                    if last.is_empty() {
                        String::new()
                    } else {
                        format!(": {last}")
                    }
                )));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(StartError::Refused(format!(
                    "the host did not start the app within {} s",
                    timeout.as_secs()
                )));
            }
            p = self
                .changed
                .wait_timeout(p, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// What the supervisor provides to a start.
pub struct StartContext<'a> {
    pub layout: &'a Layout,
    pub config: &'a HostConfig,
    pub job: BorrowedHandle<'a>,
    pub boot_image: Option<&'a BootImage>,
    /// Called with the missing-function reports of the host.
    pub on_missing: Arc<dyn Fn(&str, &[(String, u64)]) + Send + Sync>,
}

/// The data directory's guest tree and temporary directory.
fn data_dirs(layout: &Layout, package: &str) -> (PathBuf, PathBuf) {
    let data = appcontainer::data_dir(layout, package);
    (data.clone(), data.join("tmp"))
}

fn launch_error(e: SandboxError) -> StartError {
    StartError::Launch(e.to_string())
}

/// The assignment for `pkg`.
fn attach_for(
    pkg: &InstalledPackage,
    layout: &Layout,
    cfg: &HostConfig,
    boot: Option<&BootImage>,
    activity: Option<&str>,
) -> Attach {
    let entry = match &pkg.launch {
        Launchable::Main { code, class, args } => Entry::Main {
            classpath: code.iter().map(|c| c.display().to_string()).collect(),
            class: class.clone(),
            args: args.clone(),
        },
        Launchable::Framework => Entry::Framework {
            activity: activity.map(str::to_string),
        },
    };
    Attach {
        package: pkg.name.clone(),
        data_dir: data_dirs(layout, &pkg.name).0.display().to_string(),
        art_root: cfg.art_root.display().to_string(),
        entry,
        mode: pkg.mode.clone(),
        runtime_options: cfg.runtime_options.clone(),
        boot_image: boot.map(BootImage::describe),
    }
}

/// Starts the host of `pkg` and waits until the app runs. `on_exit` is
/// called once, from a sysd thread, when the process has ended.
pub fn start_host(
    ctx: &StartContext<'_>,
    pkg: &InstalledPackage,
    activity: Option<&str>,
    on_exit: Box<dyn FnOnce(&Arc<HostProcess>, ExitReport) + Send>,
) -> Result<Arc<HostProcess>, StartError> {
    let t0 = Instant::now();
    let layout = ctx.layout;
    let data = appcontainer::data_dir(layout, &pkg.name);
    // The package's profile and directories: created at install; made here
    // when missing (a hand-installed package), which is idempotent.
    let sandbox = if data.is_dir() {
        appcontainer::sandbox(&pkg.name, &pkg.salt).map_err(launch_error)?
    } else {
        appcontainer::install(layout, &pkg.name, &pkg.salt).map_err(launch_error)?
    };
    let (_, temp) = data_dirs(layout, &pkg.name);
    std::fs::create_dir_all(&temp)
        .map_err(|e| StartError::Launch(format!("cannot create {}: {e}", temp.display())))?;
    let caps = appcontainer::capabilities_for(&pkg.permissions);
    let mut env = appcontainer::container_environment(&temp);
    env.extend(ctx.config.environment.iter().cloned());

    let pipe = || io::pipe().map_err(|e| StartError::Launch(format!("cannot create a pipe: {e}")));
    let (host_in_r, host_in_w) = pipe()?;
    let (host_out_r, host_out_w) = pipe()?;
    let (out_r, out_w) = pipe()?;
    let (err_r, err_w) = pipe()?;
    let args = vec!["--host".to_string()];
    let p = appcontainer::launch(&Launch {
        sandbox: &sandbox,
        capabilities: &caps,
        exe: &ctx.config.exe,
        args: &args,
        stdio: [None, Some(out_w.as_handle()), Some(err_w.as_handle())],
        inherit: &[host_in_r.as_handle(), host_out_w.as_handle()],
        job: Some(ctx.job),
        environment: Some(&env),
        handle_env: Some(CHANNEL_ENV),
        current_dir: Some(&data),
        suspended: true,
    })
    .map_err(launch_error)?;
    // Only the child holds these ends now, so reads see its end.
    drop((host_in_r, host_out_w, out_w, err_w));
    let created = t0.elapsed();
    let mut mapped = Duration::ZERO;
    if let Some(b) = ctx.boot_image {
        let tm = Instant::now();
        if let Err(e) = b.map_into(p.process()) {
            p.kill();
            return Err(StartError::Launch(format!(
                "cannot map the boot image into the app process: {e}"
            )));
        }
        mapped = tm.elapsed();
    }
    if let Err(e) = p.resume() {
        p.kill();
        return Err(StartError::Launch(format!(
            "cannot resume the app process: {e}"
        )));
    }
    let resumed = t0.elapsed();
    let h = Arc::new(HostProcess {
        package: pkg.name.clone(),
        pid: p.pid,
        process: p,
        to_host: Mutex::new(Some(File::from(std::os::windows::io::OwnedHandle::from(
            host_in_w,
        )))),
        start: t0,
        times: Mutex::new(StartTimes {
            created_ms: ms(created),
            mapped_ms: ms(mapped),
            resumed_ms: ms(resumed),
            ..StartTimes::default()
        }),
        stopped: AtomicBool::new(false),
        progress: Mutex::new(Progress::default()),
        changed: Condvar::new(),
        stderr_tail: Mutex::new(crash::Tail::default()),
    });
    info!(
        "host_created",
        package = pkg.name,
        pid = h.pid,
        create_ms = format!("{:.2}", ms(created)),
        map_ms = format!("{:.3}", ms(mapped)),
        boot_image = ctx.boot_image.is_some()
    );

    let attach = attach_for(pkg, layout, ctx.config, ctx.boot_image, activity);
    let channel = spawn_channel(
        h.clone(),
        File::from(std::os::windows::io::OwnedHandle::from(host_out_r)),
        attach,
        ctx.on_missing.clone(),
    );
    let out = spawn_output(h.clone(), "stdout", out_r, false);
    let err = spawn_output(h.clone(), "stderr", err_r, true);
    let waiter = h.clone();
    thread::Builder::new()
        .name(format!("host-wait-{}", h.pid))
        .spawn(move || {
            let code = loop {
                match waiter.process.wait(Duration::from_secs(3600)) {
                    Ok(Some(c)) => break c,
                    Ok(None) => continue,
                    Err(e) => {
                        warn!("host_wait_failed", pid = waiter.pid, error = e);
                        break u32::MAX;
                    }
                }
            };
            // The readers end with the process's pipe ends; their last
            // lines are in the tail then.
            for t in [channel, out, err].into_iter().flatten() {
                let _ = t.join();
            }
            {
                let mut p = waiter.progress.lock().unwrap_or_else(|e| e.into_inner());
                p.closed = true;
                waiter.changed.notify_all();
            }
            let report = exit_report(&waiter, code);
            on_exit(&waiter, report);
        })
        .map_err(|e| {
            h.kill();
            StartError::Launch(format!("cannot start the wait thread: {e}"))
        })?;
    h.wait_running(ATTACH_TIMEOUT).inspect_err(|_| h.kill())?;
    Ok(h)
}

fn exit_report(h: &HostProcess, code: u32) -> ExitReport {
    let (status, host_crash) = h
        .progress
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .exit_status
        .clone()
        .map_or((None, None), |(s, c)| (Some(s), c));
    let stopped = h.was_stopped();
    let tail = h
        .stderr_tail
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .lines();
    // The status the app chose wins over the process code (they agree
    // unless something ended the host after the app).
    let effective = status.map_or(i64::from(code), i64::from);
    let crash = if stopped {
        None
    } else {
        crash::from_output(effective, &tail).map(|from_output| match host_crash {
            Some(c) if from_output.kind == andruna_ipc::host::CrashKind::Other => c,
            _ => from_output,
        })
    };
    ExitReport {
        package: h.package.clone(),
        pid: h.pid,
        exit_code: code,
        status,
        crash,
        stopped,
        run_time: h.start.elapsed(),
        times: h.times(),
    }
}

fn spawn_channel(
    h: Arc<HostProcess>,
    mut from_host: File,
    attach: Attach,
    on_missing: Arc<dyn Fn(&str, &[(String, u64)]) + Send + Sync>,
) -> Option<thread::JoinHandle<()>> {
    let mut attach = Some(attach);
    thread::Builder::new()
        .name(format!("host-channel-{}", h.pid))
        .spawn(move || {
            loop {
                let m = match host::recv_from_host(&mut from_host) {
                    Ok(Some(m)) => m,
                    Ok(None) => break,
                    Err(e) => {
                        warn!(
                            "host_channel_error",
                            package = h.package,
                            pid = h.pid,
                            error = e
                        );
                        break;
                    }
                };
                let at = h.start.elapsed();
                let mut p = h.progress.lock().unwrap_or_else(|e| e.into_inner());
                match m {
                    FromHost::Hello { pid, version } => {
                        if pid != h.pid {
                            warn!("host_pid_mismatch", expected = h.pid, got = pid);
                        }
                        p.hello = Some(at);
                        h.times.lock().unwrap_or_else(|e| e.into_inner()).hello_ms = ms(at);
                        info!(
                            "host_hello",
                            package = h.package,
                            pid = h.pid,
                            version = version
                        );
                        if let Some(a) = attach.take()
                            && let Err(e) = h.send(&ToHost::Attach(a))
                        {
                            warn!("host_attach_failed", package = h.package, error = e);
                        }
                    }
                    FromHost::Running => {
                        p.running = Some(at);
                        h.times.lock().unwrap_or_else(|e| e.into_inner()).running_ms = ms(at);
                        info!(
                            "host_running",
                            package = h.package,
                            pid = h.pid,
                            ms = format!("{:.1}", ms(at))
                        );
                    }
                    FromHost::Refused { message } => {
                        error!(
                            "host_refused",
                            package = h.package,
                            pid = h.pid,
                            message = message
                        );
                        p.refused = Some(message);
                    }
                    FromHost::MissingFunctions { entries } => {
                        drop(p);
                        on_missing(&h.package, &entries);
                        continue;
                    }
                    FromHost::Exit { code, crash } => {
                        p.exit_status = Some((code, crash));
                    }
                }
                h.changed.notify_all();
            }
            let mut p = h.progress.lock().unwrap_or_else(|e| e.into_inner());
            p.closed = true;
            h.changed.notify_all();
        })
        .ok()
}

fn spawn_output(
    h: Arc<HostProcess>,
    stream: &'static str,
    r: io::PipeReader,
    keep_tail: bool,
) -> Option<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name(format!("host-{stream}-{}", h.pid))
        .spawn(move || {
            let mut reader = BufReader::new(r);
            let mut line = Vec::new();
            let mut logged = 0usize;
            let mut dropped = 0usize;
            loop {
                line.clear();
                // A line longer than 64 KiB is cut.
                match (&mut reader).take(64 * 1024).read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let text = String::from_utf8_lossy(&line);
                let text = text.trim_end_matches(['\r', '\n']);
                if keep_tail {
                    h.stderr_tail
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(text);
                }
                if logged < MAX_LOGGED_LINES {
                    logged += 1;
                    info!(
                        "app_output",
                        package = h.package,
                        pid = h.pid,
                        stream = stream,
                        line = text
                    );
                } else {
                    dropped += 1;
                }
            }
            if dropped > 0 {
                info!(
                    "app_output_dropped",
                    package = h.package,
                    stream = stream,
                    lines = dropped
                );
            }
        })
        .ok()
}
