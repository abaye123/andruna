//! Host process supervision: start a process for a package, track it, focus
//! its window, notice when it exits.
//!
//! **Placeholder.** The host executable is meant to be `andruna-app.exe`,
//! which does not exist yet. Until it does, sysd starts itself in `--host`
//! mode ([`crate::host`]), a real process that shows a window saying it is
//! a placeholder. Supervision (job object, exit tracking, focus, idle
//! accounting) is the production code and stays when andruna-app replaces
//! the placeholder. AppContainer creation and the warm host pool are not
//! implemented (spike S7 decides the pool design).

use std::collections::HashMap;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use andruna_ipc::{ErrorCode, EventKind, is_valid_activity_name, is_valid_package_name};

use crate::activity::Activity;
use crate::events::Events;
use crate::sys::{self, Job};
use crate::{Layout, error, info, packages, warn};

/// Most host processes sysd runs at once.
pub const MAX_HOSTS: usize = 32;

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

pub struct Supervisor {
    host_exe: PathBuf,
    layout: Layout,
    job: Job,
    /// Running hosts by package name: the process id.
    hosts: Mutex<HashMap<String, u32>>,
    activity: Arc<Activity>,
    /// Crash reports go here.
    events: Arc<Events>,
    /// Set by [`Supervisor::shutdown`]: hosts ending after it did not crash.
    stopping: AtomicBool,
}

impl Supervisor {
    pub fn new(host_exe: PathBuf, layout: Layout, activity: Arc<Activity>) -> io::Result<Self> {
        Ok(Self {
            host_exe,
            layout,
            job: Job::new()?,
            hosts: Mutex::new(HashMap::new()),
            activity,
            events: Arc::new(Events::new()),
            stopping: AtomicBool::new(false),
        })
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
            .find(|(_, p)| **p == pid)
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
        if !packages::is_installed(&self.layout.app_dir(), package) {
            return Err(Failure::new(
                ErrorCode::NotInstalled,
                format!("{package} is not installed"),
            ));
        }
        let mut hosts = self.hosts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&pid) = hosts.get(package) {
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
        if hosts.len() >= MAX_HOSTS {
            return Err(Failure::new(
                ErrorCode::Busy,
                format!("{MAX_HOSTS} app processes are already running"),
            ));
        }
        let mut command = Command::new(&self.host_exe);
        command
            .arg("--host")
            .arg(package)
            .arg("--root")
            .arg(&self.layout.root);
        if let Some(a) = activity {
            command.arg("--activity").arg(a);
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // CREATE_NEW_PROCESS_GROUP: console signals of a parent console
            // (when sysd runs under a terminal in development) do not reach
            // the app.
            .creation_flags(0x0000_0200)
            .spawn()
            .map_err(|e| {
                error!("host_start_failed", package = package, error = e);
                Failure::new(
                    ErrorCode::HostStartFailed,
                    format!("cannot start host: {e}"),
                )
            })?;
        let pid = child.id();
        // The child runs briefly before it joins the job; the placeholder
        // creates no processes in that time. andruna-app will be created
        // suspended (or in the job directly) once AppContainer launch is
        // added.
        if let Err(e) = self.job.assign(&child) {
            error!("job_assign_failed", package = package, pid = pid, error = e);
            let _ = child.kill();
            let _ = child.wait();
            return Err(Failure::new(
                ErrorCode::HostStartFailed,
                format!("cannot supervise host: {e}"),
            ));
        }
        sys::allow_foreground(pid);
        hosts.insert(package.to_string(), pid);
        self.activity.host_started();
        info!(
            "host_started",
            package = package,
            pid = pid,
            exe = self.host_exe.display(),
            placeholder = true
        );
        drop(hosts);

        let this = Arc::clone(self);
        let package = package.to_string();
        thread::Builder::new()
            .name(format!("host-wait-{pid}"))
            .spawn(move || {
                let status = child.wait();
                {
                    let mut hosts = this.hosts.lock().unwrap_or_else(|e| e.into_inner());
                    if hosts.get(&package) == Some(&pid) {
                        hosts.remove(&package);
                    }
                }
                match status {
                    Ok(s) => {
                        let code = s.code().unwrap_or(-1);
                        info!("host_exited", package = package, pid = pid, code = code);
                        // A host that ends on its own with a non-zero code
                        // crashed; hosts ended by sysd's shutdown did not.
                        if code != 0 && !this.stopping.load(Ordering::SeqCst) {
                            this.events.publish(EventKind::AppCrashed {
                                package: package.clone(),
                                pid,
                                exit_code: code as u32,
                                reason: String::new(),
                            });
                        }
                    }
                    Err(e) => warn!("host_wait_failed", package = package, pid = pid, error = e),
                }
                this.activity.host_exited();
            })
            .map_err(|e| {
                Failure::new(
                    ErrorCode::Internal,
                    format!("cannot start wait thread: {e}"),
                )
            })?;
        Ok((pid, false))
    }

    /// Brings the package's window to the foreground.
    pub fn focus(&self, package: &str) -> Result<(), Failure> {
        if !is_valid_package_name(package) {
            return Err(Failure::new(
                ErrorCode::InvalidPackageName,
                format!("`{package}` is not a valid package name"),
            ));
        }
        let pid = self
            .hosts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(package)
            .copied();
        let Some(pid) = pid else {
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

    /// Process id of the package's host, if it runs.
    pub fn host_pid(&self, package: &str) -> Option<u32> {
        self.hosts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(package)
            .copied()
    }

    /// Ends every host process.
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.events.stop();
        self.job.terminate(1);
    }
}
