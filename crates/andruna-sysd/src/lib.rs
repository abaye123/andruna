//! `andruna-sysd`: the Andruna system server.
//!
//! One sysd runs per user and Windows session
//! ([architecture](../../docs/spec/02-architecture.md#processes)). This is
//! the skeleton of the process: lifetime, logging, the launcher pipe, and
//! supervision of host processes. Binder, the system services and the
//! compositor are added in their phases.
//!
//! - Single instance: a named mutex in the session namespace
//!   (`Local\Andruna.sysd.<user SID>.<instance>`); a second sysd exits with
//!   [`EXIT_ALREADY_RUNNING`].
//! - Headless: no console (windows subsystem) and one hidden top-level
//!   window that receives logoff notifications ([`sys::create_control_window`]).
//! - Logs: `%LOCALAPPDATA%\Andruna\logs\sysd.log`, rotated ([`log`]).
//! - Lifetime: exits after [`DEFAULT_IDLE_TIMEOUT`] with no client
//!   connected and no host running ([`activity`]).
//! - Pipe: the launcher protocol of `andruna-ipc` ([`server`]).
//! - Hosts: started in a kill-on-close job ([`supervisor`]); the host is a
//!   **placeholder** until andruna-app exists ([`host`]).

pub mod activity;
pub mod appcontainer;
pub mod diagnostics;
pub mod events;
pub mod host;
pub mod l10n;
pub mod log;
pub mod packages;
pub mod server;
pub mod supervisor;
mod sys;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use andruna_ipc::{DEFAULT_INSTANCE, Endpoint, PipeListener, PipeSecurity};

pub use activity::Activity;
pub use server::{Context, Server};
pub use supervisor::Supervisor;

/// Idle time after which sysd exits.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub const EXIT_OK: i32 = 0;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_ALREADY_RUNNING: i32 = 3;
pub const EXIT_STARTUP_FAILED: i32 = 4;

/// The data directory tree ([file layout](../../docs/spec/02-architecture.md#data-on-disk)).
#[derive(Clone, Debug)]
pub struct Layout {
    /// `%LOCALAPPDATA%\Andruna`
    pub root: PathBuf,
}

impl Layout {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// `%LOCALAPPDATA%\Andruna` of the current user.
    pub fn for_current_user() -> std::io::Result<Self> {
        Ok(Self::new(sys::local_app_data()?.join("Andruna")))
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Installed packages: `app\<package>\`.
    pub fn app_dir(&self) -> PathBuf {
        self.root.join("app")
    }

    /// Missing-function counters: `diagnostics\<package>.missing`.
    pub fn diagnostics_dir(&self) -> PathBuf {
        self.root.join("diagnostics")
    }
}

/// How sysd was asked to run.
#[derive(Debug)]
pub enum Mode {
    Server(ServerConfig),
    /// The placeholder host process for one package.
    Host {
        root: Option<PathBuf>,
        package: String,
        /// The activity to start (`--activity`), when not the launcher one.
        activity: Option<String>,
    },
}

#[derive(Debug)]
pub struct ServerConfig {
    pub instance: String,
    /// Data root; the user's `%LOCALAPPDATA%\Andruna` when not given.
    pub root: Option<PathBuf>,
    pub idle_timeout: Duration,
    /// Inherited event handle to signal once the pipe accepts clients.
    pub ready_event: Option<usize>,
}

pub const USAGE: &str = "\
usage: andruna-sysd [--instance <name>] [--root <dir>] [--idle-timeout-ms <n>] [--ready-event <handle>]
       andruna-sysd --host <package> [--activity <name>] [--root <dir>]";

/// Parses the command line (without the program name).
pub fn parse_args(args: &[String]) -> Result<Mode, String> {
    let mut instance = DEFAULT_INSTANCE.to_string();
    let mut root = None;
    let mut idle_timeout = DEFAULT_IDLE_TIMEOUT;
    let mut ready_event = None;
    let mut host = None;
    let mut activity = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("`{a}` needs a value"))
        };
        match a.as_str() {
            "--instance" => {
                instance = value()?;
                if !andruna_ipc::pipe::is_valid_instance(&instance) {
                    return Err(format!("invalid instance name `{instance}`"));
                }
            }
            "--root" => root = Some(PathBuf::from(value()?)),
            "--idle-timeout-ms" => {
                let v = value()?;
                let ms: u64 = v
                    .parse()
                    .map_err(|_| format!("invalid --idle-timeout-ms `{v}`"))?;
                idle_timeout = Duration::from_millis(ms);
            }
            "--ready-event" => {
                let v = value()?;
                ready_event = Some(
                    v.parse()
                        .map_err(|_| format!("invalid --ready-event `{v}`"))?,
                );
            }
            "--host" => {
                let p = value()?;
                if !andruna_ipc::is_valid_package_name(&p) {
                    return Err(format!("invalid package name `{p}`"));
                }
                host = Some(p);
            }
            "--activity" => {
                let a = value()?;
                if !andruna_ipc::is_valid_activity_name(&a) {
                    return Err(format!("invalid activity name `{a}`"));
                }
                activity = Some(a);
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    Ok(match host {
        Some(package) => Mode::Host {
            root,
            package,
            activity,
        },
        None => Mode::Server(ServerConfig {
            instance,
            root,
            idle_timeout,
            ready_event,
        }),
    })
}

fn layout(root: Option<PathBuf>) -> std::io::Result<Layout> {
    match root {
        Some(r) => Ok(Layout::new(r)),
        None => Layout::for_current_user(),
    }
}

/// Runs sysd with the given command line and returns the exit code.
pub fn main_with_args(args: &[String]) -> i32 {
    std::panic::set_hook(Box::new(|p| {
        error!("panic", detail = p);
    }));
    sys::make_std_handles_private();
    match parse_args(args) {
        Err(e) => {
            eprintln!("andruna-sysd: {e}\n{USAGE}");
            EXIT_USAGE
        }
        Ok(Mode::Host {
            root,
            package,
            activity,
        }) => match layout(root) {
            Ok(l) => host::run(&l, &package, activity.as_deref()),
            Err(_) => EXIT_STARTUP_FAILED,
        },
        Ok(Mode::Server(cfg)) => run_server(cfg),
    }
}

fn run_server(cfg: ServerConfig) -> i32 {
    let layout = match layout(cfg.root.clone()) {
        Ok(l) => l,
        Err(_) => return EXIT_STARTUP_FAILED,
    };
    let _ = log::init(&layout.logs_dir(), "sysd");
    info!(
        "starting",
        version = env!("CARGO_PKG_VERSION"),
        instance = cfg.instance,
        root = layout.root.display(),
        idle_timeout_ms = cfg.idle_timeout.as_millis()
    );
    match serve(&cfg, layout) {
        Ok(code) => code,
        Err(e) => {
            error!("startup_failed", error = e);
            EXIT_STARTUP_FAILED
        }
    }
}

fn serve(cfg: &ServerConfig, layout: Layout) -> std::io::Result<i32> {
    let endpoint = Endpoint::for_current_user(&cfg.instance)?;
    let Some(instance_mutex) = sys::create_single_instance_mutex(&endpoint.mutex_name())? else {
        info!("already_running", mutex = endpoint.mutex_name());
        return Ok(EXIT_ALREADY_RUNNING);
    };
    let listener = PipeListener::bind(&endpoint, &PipeSecurity::current_user_only(&endpoint))
        .map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("cannot create pipe {}: {e}", endpoint.pipe_name()),
            )
        })?;
    let window = sys::create_control_window()?;
    let activity = Arc::new(Activity::new(cfg.idle_timeout));
    // Placeholder: the host executable is sysd itself in --host mode until
    // andruna-app exists.
    let host_exe = std::env::current_exe()?;
    let supervisor = Arc::new(Supervisor::new(host_exe, layout.clone(), activity.clone())?);
    let ctx = Arc::new(Context {
        layout,
        activity: activity.clone(),
        supervisor: supervisor.clone(),
        started: Instant::now(),
    });
    let server = Server::spawn(listener, ctx)?;

    let idle = activity.clone();
    std::thread::Builder::new()
        .name("idle-watch".into())
        .spawn(move || {
            if idle.wait_idle() {
                info!("idle_shutdown", idle_ms = idle.timeout().as_millis());
                window.post_close();
            }
        })?;

    if let Some(ev) = cfg.ready_event
        && let Err(e) = sys::signal_inherited_event(ev)
    {
        warn!("ready_event_failed", error = e);
    }
    info!("ready", pipe = endpoint.pipe_name());

    sys::run_message_loop();

    activity.stop();
    server.stop();
    supervisor.shutdown();
    info!("stopped");
    drop(instance_mutex);
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_server_and_host_modes() {
        match parse_args(&args(&[])).unwrap() {
            Mode::Server(c) => {
                assert_eq!(c.instance, DEFAULT_INSTANCE);
                assert_eq!(c.idle_timeout, DEFAULT_IDLE_TIMEOUT);
                assert!(c.root.is_none() && c.ready_event.is_none());
            }
            m => panic!("{m:?}"),
        }
        match parse_args(&args(&[
            "--instance",
            "t1",
            "--root",
            "C:\\x",
            "--idle-timeout-ms",
            "250",
            "--ready-event",
            "1234",
        ]))
        .unwrap()
        {
            Mode::Server(c) => {
                assert_eq!(c.instance, "t1");
                assert_eq!(c.root, Some(PathBuf::from("C:\\x")));
                assert_eq!(c.idle_timeout, Duration::from_millis(250));
                assert_eq!(c.ready_event, Some(1234));
            }
            m => panic!("{m:?}"),
        }
        match parse_args(&args(&["--host", "com.example.app"])).unwrap() {
            Mode::Host { package, .. } => assert_eq!(package, "com.example.app"),
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn rejects_bad_arguments() {
        for bad in [
            &["--host", "not valid"][..],
            &["--instance", "a b"],
            &["--idle-timeout-ms", "x"],
            &["--root"],
            &["stray"],
        ] {
            assert!(parse_args(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
