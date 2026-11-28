//! Host process supervision: start a package's app process, track it,
//! focus its window, stop it, notice how it ended.
//!
//! Each package has at most one process: `andruna-app --host`, created
//! suspended inside the package's AppContainer and sysd's kill-on-close
//! job, with the boot image view mapped before it runs ([`crate::apps`],
//! ADR 0035, ADR 0036). Packages come from a [`PackageStore`]. A process
//! that ends with a crash (not stopped by sysd) is published as an
//! `AppCrashed` event with the crash summary.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use andruna_ipc::{
    ErrorCode, EventKind, MissingFunction, is_valid_activity_name, is_valid_package_name,
};

use crate::activity::Activity;
use crate::apps::bootimage::BootImage;
use crate::apps::store::{PackageStore, RecordStore};
use crate::apps::{self, ExitReport, HostConfig, HostProcess, StartContext, StartError};
use crate::events::Events;
use crate::sys::{self, Job};
use crate::{Layout, diagnostics, error, info, warn};

/// Most host processes sysd runs at once.
pub const MAX_HOSTS: usize = 32;

/// How long a stopped app may take to end before it is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(2);

/// A failed supervision request: the protocol error code and a message.
#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: ErrorCode,
    pub message: String,
}

impl Failure {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The boot image section, prepared at the first start.
enum Boot {
    Unprepared,
    Ready(Option<Arc<BootImage>>),
}

pub struct Supervisor {
    config: HostConfig,
    layout: Layout,
    store: Box<dyn PackageStore>,
    job: Job,
    /// Running hosts by package name.
    hosts: Mutex<HashMap<String, Arc<HostProcess>>>,
    /// Serializes starts, so a package never gets two processes.
    starting: Mutex<()>,
    boot: Mutex<Boot>,
    /// Runtime files granted to the `andrunaRuntime` capability once.
    granted: AtomicBool,
    /// How each package's last process ended.
    exits: Mutex<HashMap<String, ExitReport>>,
    exited: Condvar,
    activity: Arc<Activity>,
    /// Crash reports go here.
    events: Arc<Events>,
    /// Set by [`Supervisor::shutdown`]: hosts ending after it did not crash.
    stopping: AtomicBool,
}

impl Supervisor {
    /// A supervisor that starts `host_exe` with the ART root next to it
    /// (`art\`), packages from the [`RecordStore`] of `layout`.
    pub fn new(host_exe: PathBuf, layout: Layout, activity: Arc<Activity>) -> io::Result<Self> {
        let dir = host_exe.parent().map(Path::to_path_buf).unwrap_or_default();
        let config = HostConfig {
            exe: host_exe,
            ..HostConfig::beside(&dir)
        };
        let store = Box::new(RecordStore::new(layout.clone()));
        Self::with(config, layout, store, activity)
    }

    pub fn with(
        config: HostConfig,
        layout: Layout,
        store: Box<dyn PackageStore>,
        activity: Arc<Activity>,
    ) -> io::Result<Self> {
        Ok(Self {
            config,
            layout,
            store,
            job: Job::new()?,
            hosts: Mutex::new(HashMap::new()),
            starting: Mutex::new(()),
            boot: Mutex::new(Boot::Unprepared),
            granted: AtomicBool::new(false),
            exits: Mutex::new(HashMap::new()),
            exited: Condvar::new(),
            activity,
            events: Arc::new(Events::new()),
            stopping: AtomicBool::new(false),
        })
    }

    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    /// The event stream that host crashes are published to.
    pub fn events(&self) -> &Arc<Events> {
        &self.events
    }

    /// The package whose host process has this process id.
    pub fn package_for_pid(&self, pid: u32) -> Option<String> {
        self.hosts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(_, h)| h.pid == pid)
            .map(|(name, _)| name.clone())
    }

    /// Starts one activity of `package`. When the package already runs, its
    /// window is focused; delivering the activity to a running app needs the
    /// activity manager, which does not exist yet.
    pub fn start_activity(
        self: &Arc<Self>,
        package: &str,
        activity: &str,
    ) -> Result<(u32, bool), Failure> {
        if !is_valid_activity_name(activity) {
            return Err(Failure::new(
                ErrorCode::InvalidPackageName,
                format!("`{activity}` is not a valid activity name"),
            ));
        }
        self.start_with(package, Some(activity))
    }

    /// Starts `package`, or focuses it when it already runs. Returns the
    /// host's process id and whether it was already running.
    pub fn start(self: &Arc<Self>, package: &str) -> Result<(u32, bool), Failure> {
        self.start_with(package, None)
    }

    fn boot_image(&self) -> Option<Arc<BootImage>> {
        if !self.config.map_boot_image {
            return None;
        }
        let mut g = self.boot.lock().unwrap_or_else(|e| e.into_inner());
        if let Boot::Ready(b) = &*g {
            return b.clone();
        }
        let t = Instant::now();
        let b = match BootImage::open(
            &self.config.art_root,
            &self.layout.root.join("cache").join("boot-image"),
        ) {
            Ok(Some(b)) => {
                info!(
                    "boot_image_ready",
                    base = format!("{:#x}", b.layout.base),
                    size = b.layout.size,
                    extents = b.layout.extents.len(),
                    view_file = b.view_file.display(),
                    ms = format!("{:.1}", t.elapsed().as_secs_f64() * 1000.0)
                );
                Some(Arc::new(b))
            }
            Ok(None) => {
                info!(
                    "boot_image_absent",
                    art_root = self.config.art_root.display()
                );
                None
            }
            Err(e) => {
                // Apps still run: ART then reads the image files itself.
                error!("boot_image_failed", error = e);
                None
            }
        };
        *g = Boot::Ready(b.clone());
        b
    }

    /// Lets the containers read the runtime files (idempotent; the
    /// installer does the same).
    fn grant_runtime(&self) {
        if self.granted.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut dirs: Vec<PathBuf> = vec![self.config.art_root.clone()];
        if let Some(d) = self.config.exe.parent()
            && !self.config.art_root.starts_with(d)
        {
            dirs.push(d.to_path_buf());
        }
        for d in dirs {
            if let Err(e) = crate::appcontainer::grant_runtime(&d) {
                warn!("grant_runtime_failed", dir = d.display(), error = e);
            }
        }
    }

    fn start_with(
        self: &Arc<Self>,
        package: &str,
        activity: Option<&str>,
    ) -> Result<(u32, bool), Failure> {
        if !is_valid_package_name(package) {
            return Err(Failure::new(
                ErrorCode::InvalidPackageName,
                format!("`{package}` is not a valid package name"),
            ));
        }
        let _starting = self.starting.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pid) = self.host_pid(package) {
            let focused = sys::focus_process_window(pid);
            info!(
                "start_package",
                package = package,
                pid = pid,
                already_running = true,
                focused = focused
            );
            return Ok((pid, true));
        }
        let pkg = match self.store.get(package) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return Err(Failure::new(
                    ErrorCode::NotInstalled,
                    format!("{package} is not installed"),
                ));
            }
            Err(e) => {
                error!("package_record_failed", package = package, error = e);
                return Err(Failure::new(ErrorCode::HostStartFailed, e.to_string()));
            }
        };
        if self.hosts.lock().unwrap_or_else(|e| e.into_inner()).len() >= MAX_HOSTS {
            return Err(Failure::new(
                ErrorCode::Busy,
                format!("{MAX_HOSTS} app processes are already running"),
            ));
        }
        if !self.config.exe.is_file() {
            return Err(Failure::new(
                ErrorCode::HostStartFailed,
                format!("{} is missing", self.config.exe.display()),
            ));
        }
        self.grant_runtime();
        let boot = self.boot_image();
        let layout = self.layout.clone();
        let diag_dir = self.layout.diagnostics_dir();
        let on_missing = Arc::new(move |package: &str, entries: &[(String, u64)]| {
            let entries: Vec<MissingFunction> = entries
                .iter()
                .map(|(name, calls)| MissingFunction {
                    name: name.clone(),
                    calls: *calls,
                })
                .collect();
            if let Err(e) = diagnostics::add(&diag_dir, package, &entries) {
                warn!("missing_functions_failed", package = package, error = e);
            }
        });
        let ctx = StartContext {
            layout: &layout,
            config: &self.config,
            job: self.job.handle(),
            boot_image: boot.as_deref(),
            on_missing,
        };
        // Counted before the process exists, so its exit (which may come
        // at any time) is never counted first.
        self.activity.host_started();
        let this = Arc::clone(self);
        let on_exit = Box::new(move |h: &Arc<HostProcess>, r: ExitReport| this.ended(h, r));
        let h = match apps::start_host(&ctx, &pkg, activity, on_exit) {
            Ok(h) => h,
            Err(e) => {
                // A host that was created ends through `ended`, which
                // balances the count; one that never existed does not.
                if matches!(e, StartError::Launch(_)) {
                    self.activity.host_exited();
                }
                error!("host_start_failed", package = package, error = e);
                return Err(Failure::new(ErrorCode::HostStartFailed, e.to_string()));
            }
        };
        sys::allow_foreground(h.pid);
        let t = h.times();
        info!(
            "host_started",
            package = package,
            activity = activity.unwrap_or("-"),
            pid = h.pid,
            exe = self.config.exe.display(),
            boot_image = boot.is_some(),
            map_ms = format!("{:.3}", t.mapped_ms),
            running_ms = format!("{:.1}", t.running_ms)
        );
        let pid = h.pid;
        let mut hosts = self.hosts.lock().unwrap_or_else(|e| e.into_inner());
        // It may have ended already (a short program); `ended` then ran.
        let gone = self
            .exits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(package)
            .is_some_and(|r| r.pid == pid);
        if !gone {
            hosts.insert(package.to_string(), h);
        }
        Ok((pid, false))
    }

    /// Called once per process when it has ended.
    fn ended(&self, h: &Arc<HostProcess>, r: ExitReport) {
        {
            let mut hosts = self.hosts.lock().unwrap_or_else(|e| e.into_inner());
            if hosts.get(&r.package).is_some_and(|x| Arc::ptr_eq(x, h)) {
                hosts.remove(&r.package);
            }
        }
        let shutting_down = self.stopping.load(Ordering::SeqCst);
        info!(
            "host_exited",
            package = r.package,
            pid = r.pid,
            code = r.exit_code,
            status = r.status.map_or("-".to_string(), |s| s.to_string()),
            stopped = r.stopped || shutting_down,
            crashed = r.crash.is_some(),
            run_ms = r.run_time.as_millis()
        );
        if let Some(c) = &r.crash
            && !shutting_down
        {
            warn!(
                "app_crashed",
                package = r.package,
                pid = r.pid,
                kind = format!("{:?}", c.kind),
                summary = c.summary.replace('\n', " | ")
            );
            self.events.publish(EventKind::AppCrashed {
                package: r.package.clone(),
                pid: r.pid,
                exit_code: r.exit_code,
                reason: c.summary.clone(),
            });
        }
        self.exits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(r.package.clone(), r);
        self.exited.notify_all();
        self.activity.host_exited();
    }

    /// How process `pid` of `package` ended, waiting up to `timeout` for
    /// it to end.
    pub fn wait_exit(&self, package: &str, pid: u32, timeout: Duration) -> Option<ExitReport> {
        let deadline = Instant::now() + timeout;
        let mut g = self.exits.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(r) = g.get(package).filter(|r| r.pid == pid) {
                return Some(r.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            g = self
                .exited
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Brings the package's window to the foreground.
    pub fn focus(&self, package: &str) -> Result<(), Failure> {
        if !is_valid_package_name(package) {
            return Err(Failure::new(
                ErrorCode::InvalidPackageName,
                format!("`{package}` is not a valid package name"),
            ));
        }
        let Some(pid) = self.host_pid(package) else {
            return Err(Failure::new(
                ErrorCode::NotRunning,
                format!("{package} is not running"),
            ));
        };
        if sys::focus_process_window(pid) {
            Ok(())
        } else {
            Err(Failure::new(
                ErrorCode::NoWindow,
                format!("{package} has no window"),
            ))
        }
    }

    /// Stops the package's process (asks, then kills after
    /// [`STOP_GRACE`]). Not a crash.
    pub fn stop(&self, package: &str) -> Result<(), Failure> {
        let h = self
            .hosts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(package)
            .cloned();
        let Some(h) = h else {
            return Err(Failure::new(
                ErrorCode::NotRunning,
                format!("{package} is not running"),
            ));
        };
        info!("stop_package", package = package, pid = h.pid);
        h.stop(STOP_GRACE);
        Ok(())
    }

    /// Memory counters of the package's process, if it runs.
    pub fn host_memory(&self, package: &str) -> Option<io::Result<apps::Memory>> {
        let h = self
            .hosts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(package)
            .cloned()?;
        Some(h.memory())
    }

    /// Process id of the package's host, if it runs.
    pub fn host_pid(&self, package: &str) -> Option<u32> {
        self.hosts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(package)
            .map(|h| h.pid)
    }

    /// Ends every host process.
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        for h in self
            .hosts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            h.kill();
        }
        self.events.stop();
        self.job.terminate(1);
    }
}
