//! `andruna-launch.exe <package>[/<activity>]`: what Start menu shortcuts
//! and notification clicks run.
//!
//! Asks andruna-sysd to start the package (or one of its activities),
//! starting sysd first when it is not running, makes sure the manager's
//! notification bridge runs, then exits
//! ([architecture](../../docs/spec/02-architecture.md#startup-of-an-app)).
//! A windows-subsystem program, so no console window appears. Errors are
//! shown in a task dialog in the user's language and reported by the exit
//! code; `--quiet` (for scripts and tests) skips the dialog and writes the
//! message to stderr only.
//!
//! Exit codes: 0 started or focused, 1 bad command line, 2 sysd could not
//! be reached or started, 3 sysd refused the request.

#![windows_subsystem = "windows"]

mod l10n;
mod sys;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use andruna_ipc::{Client, DEFAULT_INSTANCE, Endpoint, ErrorCode, Request, Response};

use l10n::Lang;

/// How long the launcher waits for a sysd it started to accept clients.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// sysd's exit code when another instance already runs
/// (`andruna_sysd::EXIT_ALREADY_RUNNING`).
const SYSD_EXIT_ALREADY_RUNNING: u32 = 3;

const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

struct Args {
    package: String,
    /// The activity, from `<package>/<activity>`.
    activity: Option<String>,
    instance: String,
    root: Option<PathBuf>,
}

/// A failure, as a resource key with its arguments and an English detail
/// for stderr.
struct Failure {
    key: &'static str,
    args: Vec<(&'static str, String)>,
    exit: i32,
}

impl Failure {
    fn new(key: &'static str, exit: i32) -> Self {
        Self {
            key,
            args: Vec::new(),
            exit,
        }
    }
    fn arg(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.args.push((name, value.into()));
        self
    }
}

fn parse_args() -> Result<Args, Failure> {
    let mut package = None;
    let mut instance = DEFAULT_INSTANCE.to_string();
    let mut root = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--quiet" => {}
            "--instance" => instance = it.next().ok_or_else(|| Failure::new("usage", 1))?,
            "--root" => {
                root = Some(PathBuf::from(
                    it.next().ok_or_else(|| Failure::new("usage", 1))?,
                ))
            }
            s if s.starts_with("--") => return Err(Failure::new("usage", 1)),
            _ if package.is_some() => return Err(Failure::new("usage", 1)),
            _ => package = Some(a),
        }
    }
    let target: String = package.ok_or_else(|| Failure::new("usage", 1))?;
    let (package, activity) = split_target(&target);
    Ok(Args {
        package,
        activity,
        instance,
        root,
    })
}

/// Splits `<package>[/<activity>]` (Android's component name form). An
/// activity of the form `.Name` is relative to the package.
fn split_target(target: &str) -> (String, Option<String>) {
    match target.split_once('/') {
        Some((p, a)) => (p.to_string(), Some(a.to_string())),
        None => (target.to_string(), None),
    }
}

/// The manager's file name next to the launcher.
const MANAGER_EXE: &str = "Andruna.Manager.exe";

/// Makes sure the manager runs, in the background when no window is open:
/// it shows the apps' notifications and the crash dialog. The manager holds
/// the mutex `Local\Andruna.Manager.<user SID>` while it runs; when the
/// mutex exists nothing is started. Only for the product instance of sysd,
/// which is the one the manager talks to.
fn ensure_manager(args: &Args, endpoint: &Endpoint) {
    if args.instance != DEFAULT_INSTANCE || args.root.is_some() {
        return;
    }
    let mutex = format!("Local\\Andruna.Manager.{}", endpoint.user().to_string_sid());
    if sys::mutex_exists(&mutex) {
        return;
    }
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(MANAGER_EXE)))
        .filter(|p| p.is_file())
    else {
        return;
    };
    use std::os::windows::process::CommandExt;
    let _ = Command::new(exe)
        .arg("--background")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)
        .spawn();
}

/// Starts andruna-sysd from the launcher's own directory. It is started
/// outside the launcher's job where the job allows it, so closing whatever
/// started the launcher does not end sysd and its apps.
fn spawn_sysd(args: &Args, event: &sys::InheritableEvent) -> Result<Child, Failure> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("andruna-sysd.exe")))
        .ok_or_else(|| Failure::new("sysd_missing", 2).arg("file", "andruna-sysd.exe"))?;
    if !exe.is_file() {
        return Err(Failure::new("sysd_missing", 2).arg("file", exe.display().to_string()));
    }
    let make = |flags: u32| {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new(&exe);
        c.arg("--instance")
            .arg(&args.instance)
            .arg("--ready-event")
            .arg(event.value().to_string());
        if let Some(r) = &args.root {
            c.arg("--root").arg(r);
        }
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags);
        c
    };
    make(CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
        .or_else(|_| make(CREATE_NEW_PROCESS_GROUP).spawn())
        .map_err(|e| Failure::new("sysd_start_failed", 2).arg("detail", e.to_string()))
}

/// Connects to the running sysd, or starts one and connects once it is
/// ready.
fn connect(args: &Args, endpoint: &Endpoint) -> Result<Client, Failure> {
    let map = |e: andruna_ipc::Error| match e {
        andruna_ipc::Error::NotRunning => Failure::new("sysd_timeout", 2),
        andruna_ipc::Error::WrongServerUser { .. } => Failure::new("sysd_untrusted", 2),
        e => Failure::new("sysd_start_failed", 2).arg("detail", e.to_string()),
    };
    match Client::connect(endpoint, Duration::ZERO) {
        Ok(c) => return Ok(c),
        Err(andruna_ipc::Error::NotRunning) => {}
        Err(e) => return Err(map(e)),
    }
    let event = sys::InheritableEvent::new()
        .map_err(|e| Failure::new("sysd_start_failed", 2).arg("detail", e.to_string()))?;
    let mut child = spawn_sysd(args, &event)?;
    let start = Instant::now();
    match sys::wait_event_or_exit(&event, &child, STARTUP_TIMEOUT) {
        sys::Wait::Signaled => {}
        sys::Wait::Exited => {
            let code = child
                .try_wait()
                .ok()
                .flatten()
                .and_then(|s| s.code())
                .unwrap_or(-1) as u32;
            // Another launcher started sysd at the same moment; that one
            // is coming up. Any other exit is a failed start.
            if code != SYSD_EXIT_ALREADY_RUNNING {
                return Err(Failure::new("sysd_start_failed", 2)
                    .arg("detail", format!("andruna-sysd exited with code {code}")));
            }
        }
        sys::Wait::TimedOut => return Err(Failure::new("sysd_timeout", 2)),
    }
    let left = STARTUP_TIMEOUT.saturating_sub(start.elapsed());
    Client::connect(endpoint, left.max(Duration::from_millis(500))).map_err(map)
}

fn start_package(args: &Args) -> Result<(), Failure> {
    let endpoint =
        Endpoint::for_current_user(&args.instance).map_err(|_| Failure::new("usage", 1))?;
    // One retry covers a sysd that was exiting on idle while we connected.
    let mut attempt = 0;
    let resp = loop {
        attempt += 1;
        let mut client = connect(args, &endpoint)?;
        sys::allow_foreground(client.server_pid());
        let request = match &args.activity {
            Some(activity) => Request::StartActivity {
                package: args.package.clone(),
                activity: activity.clone(),
            },
            None => Request::StartPackage {
                package: args.package.clone(),
            },
        };
        match client.request(&request) {
            Ok(r) => break r,
            Err(andruna_ipc::Error::Closed | andruna_ipc::Error::Io(_)) if attempt < 2 => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(Failure::new("internal", 3)
                    .arg("package", args.package.clone())
                    .arg("detail", e.to_string()));
            }
        }
    };
    match resp {
        Response::Started { .. } => {
            ensure_manager(args, &endpoint);
            Ok(())
        }
        Response::Error { code, message } => {
            let f = match code {
                ErrorCode::NotInstalled => Failure::new("not_installed", 3),
                ErrorCode::InvalidPackageName if args.activity.is_some() => {
                    Failure::new("invalid_activity", 1)
                        .arg("activity", args.activity.clone().unwrap_or_default())
                }
                ErrorCode::InvalidPackageName => Failure::new("invalid_package", 1),
                ErrorCode::UnknownRequest => Failure::new("sysd_outdated", 3),
                ErrorCode::HostStartFailed => Failure::new("host_failed", 3),
                ErrorCode::Busy => Failure::new("busy", 3),
                _ => Failure::new("internal", 3),
            };
            Err(f
                .arg("package", args.package.clone())
                .arg("detail", message))
        }
        other => Err(Failure::new("internal", 3)
            .arg("package", args.package.clone())
            .arg("detail", format!("unexpected answer {other:?}"))),
    }
}

fn report(f: &Failure, quiet: bool) {
    let args: Vec<(&str, &str)> = f.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
    eprintln!(
        "andruna-launch: {}",
        l10n::text(Lang::En, f.key, &args).replace('\n', " ")
    );
    if quiet {
        return;
    }
    let lang = Lang::current();
    let text = l10n::text(lang, f.key, &args);
    let (instruction, content) = text.split_once("\n\n").unwrap_or((&text, ""));
    sys::show_error(
        &l10n::text(lang, "title", &[]),
        instruction,
        content,
        lang.is_rtl(),
    );
}

fn main() {
    sys::make_std_handles_private();
    let quiet = std::env::args().any(|a| a == "--quiet");
    let result = parse_args().and_then(|args| {
        if !andruna_ipc::is_valid_package_name(&args.package) {
            return Err(Failure::new("invalid_package", 1).arg("package", args.package.clone()));
        }
        if let Some(a) = &args.activity
            && !andruna_ipc::is_valid_activity_name(a)
        {
            return Err(Failure::new("invalid_activity", 1)
                .arg("package", args.package.clone())
                .arg("activity", a.clone()));
        }
        start_package(&args)
    });
    if let Err(f) = result {
        report(&f, quiet);
        std::process::exit(f.exit);
    }
}
