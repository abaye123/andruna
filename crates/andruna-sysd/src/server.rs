//! The pipe server: accepts clients and answers their requests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use andruna_ipc::proto::{self, DecodeError};
use andruna_ipc::{
    Connection, ErrorCode, PROTOCOL_VERSION, PipeListener, Request, Response, Status, Waker,
};

use crate::activity::Activity;
use crate::supervisor::Supervisor;
use crate::{Layout, diagnostics, info, packages, warn};

/// Most client connections served at once.
pub const MAX_CONNECTIONS: u32 = 64;

/// What the request handlers work with.
pub struct Context {
    pub layout: Layout,
    pub activity: Arc<Activity>,
    pub supervisor: Arc<Supervisor>,
    pub started: Instant,
}

impl Context {
    /// Answers one request from an unknown process.
    pub fn handle(&self, req: Request) -> Response {
        self.handle_from(req, None)
    }

    /// Answers one request from the process `peer_pid`, when known.
    pub fn handle_from(&self, req: Request, peer_pid: Option<u32>) -> Response {
        match req {
            Request::Ping => Response::Pong {
                protocol: PROTOCOL_VERSION,
            },
            Request::Status => {
                let (connections, hosts) = self.activity.counts();
                Response::Status(Status {
                    pid: std::process::id(),
                    uptime_ms: self.started.elapsed().as_millis() as u64,
                    connections,
                    hosts,
                    idle_timeout_ms: self.activity.timeout().as_millis() as u64,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                })
            }
            Request::StartPackage { package } => match self.supervisor.start(&package) {
                Ok((pid, already_running)) => Response::Started {
                    pid,
                    already_running,
                },
                Err(f) => {
                    info!(
                        "start_package_refused",
                        package = package,
                        code = format!("{:?}", f.code),
                        reason = f.message
                    );
                    Response::error(f.code, f.message)
                }
            },
            Request::FocusWindow { package } => match self.supervisor.focus(&package) {
                Ok(()) => Response::Focused,
                Err(f) => Response::error(f.code, f.message),
            },
            Request::ListPackages => match packages::installed(&self.layout.app_dir()) {
                Ok(packages) => Response::Packages { packages },
                Err(e) => Response::error(
                    ErrorCode::Internal,
                    format!("cannot read the package directory: {e}"),
                ),
            },
            Request::WaitEvents {
                epoch,
                after,
                timeout_ms,
            } => self.supervisor.events().wait(
                epoch,
                after,
                Duration::from_millis(u64::from(timeout_ms.min(proto::MAX_WAIT_MS))),
            ),
            Request::GetMissingFunctions { package } => {
                if !andruna_ipc::is_valid_package_name(&package) {
                    return Response::error(
                        ErrorCode::InvalidPackageName,
                        format!("`{package}` is not a valid package name"),
                    );
                }
                match diagnostics::read(&self.layout.diagnostics_dir(), &package) {
                    Ok((updated_ms, entries)) => Response::MissingFunctions {
                        updated_ms,
                        entries,
                    },
                    Err(e) => Response::error(
                        ErrorCode::Internal,
                        format!("cannot read the counters: {e}"),
                    ),
                }
            }
            Request::ReportMissingFunctions { entries } => {
                // The package is the one sysd started this process for;
                // nothing the caller says about itself is trusted.
                let Some(package) = peer_pid.and_then(|p| self.supervisor.package_for_pid(p))
                else {
                    return Response::error(
                        ErrorCode::NotPermitted,
                        "only app processes started by sysd report missing functions",
                    );
                };
                match diagnostics::add(&self.layout.diagnostics_dir(), &package, &entries) {
                    Ok(()) => Response::Reported,
                    Err(e) => Response::error(
                        ErrorCode::Internal,
                        format!("cannot write the counters: {e}"),
                    ),
                }
            }
            Request::StartActivity { package, activity } => {
                match self.supervisor.start_activity(&package, &activity) {
                    Ok((pid, already_running)) => Response::Started {
                        pid,
                        already_running,
                    },
                    Err(f) => {
                        info!(
                            "start_activity_refused",
                            package = package,
                            activity = activity,
                            code = format!("{:?}", f.code),
                            reason = f.message
                        );
                        Response::error(f.code, f.message)
                    }
                }
            }
            Request::InspectPackage { .. }
            | Request::InstallPackage { .. }
            | Request::UninstallPackage { .. }
            | Request::ListPackageInfo { .. } => Response::error(
                ErrorCode::UnknownRequest,
                "package manager requests are not wired yet",
            ),
        }
    }

    /// Serves one connection until the client closes it or sends a frame
    /// that cannot be answered. Returns true when the connection became
    /// passive ([`Activity::connection_passive`]).
    pub fn serve(&self, conn: &mut Connection) -> bool {
        let mut passive = false;
        loop {
            let payload = match conn.read_frame() {
                Ok(Some(p)) => p,
                Ok(None) => return passive,
                Err(e) => {
                    warn!(
                        "client_read_failed",
                        client_pid = conn.peer_pid(),
                        error = e
                    );
                    return passive;
                }
            };
            let id = proto::decode_header(&payload).map(|h| h.id).unwrap_or(0);
            let (resp, keep_open) = match Request::decode(&payload) {
                Ok((_, req)) => {
                    if !passive && matches!(req, Request::WaitEvents { .. }) {
                        passive = true;
                        self.activity.connection_passive();
                    }
                    (self.handle_from(req, Some(conn.peer_pid())), true)
                }
                Err(DecodeError::UnsupportedVersion(v)) => (
                    Response::error(
                        ErrorCode::UnsupportedVersion,
                        format!(
                            "protocol version {v} is not supported; this server speaks {PROTOCOL_VERSION}"
                        ),
                    ),
                    false,
                ),
                Err(DecodeError::UnknownKind(k)) => (
                    Response::error(
                        ErrorCode::UnknownRequest,
                        format!("unknown request {k:#06x}"),
                    ),
                    true,
                ),
                Err(e) => (Response::error(ErrorCode::Malformed, e.to_string()), false),
            };
            if let Response::Error { code, message } = &resp
                && !keep_open
            {
                warn!(
                    "client_protocol_error",
                    client_pid = conn.peer_pid(),
                    code = format!("{code:?}"),
                    reason = message
                );
            }
            if conn.write_frame(&resp.encode(id)).is_err() || !keep_open {
                return passive;
            }
        }
    }
}

/// The accept loop on its own thread; one thread per connection.
pub struct Server {
    stopping: Arc<AtomicBool>,
    waker: Waker,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn spawn(mut listener: PipeListener, ctx: Arc<Context>) -> std::io::Result<Self> {
        let stopping = Arc::new(AtomicBool::new(false));
        let waker = listener.waker();
        let stop = stopping.clone();
        let thread = thread::Builder::new()
            .name("pipe-accept".into())
            .spawn(move || {
                loop {
                    let conn = listener.accept();
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let mut conn = match conn {
                        Ok(c) => c,
                        Err(e) => {
                            warn!("pipe_accept_failed", error = e);
                            thread::sleep(std::time::Duration::from_millis(50));
                            continue;
                        }
                    };
                    let (connections, _) = ctx.activity.counts();
                    if connections >= MAX_CONNECTIONS {
                        let busy = Response::error(ErrorCode::Busy, "too many connections");
                        let _ = conn.write_frame(&busy.encode(0));
                        continue;
                    }
                    ctx.activity.connection_opened();
                    let client_ctx = ctx.clone();
                    let spawned =
                        thread::Builder::new()
                            .name("pipe-client".into())
                            .spawn(move || {
                                if !client_ctx.serve(&mut conn) {
                                    client_ctx.activity.connection_closed();
                                }
                            });
                    if let Err(e) = spawned {
                        // The connection was dropped with the closure; the
                        // client sees a closed pipe.
                        warn!("client_thread_failed", error = e);
                        ctx.activity.connection_closed();
                    }
                }
            })?;
        Ok(Self {
            stopping,
            waker,
            thread: Some(thread),
        })
    }

    /// Stops accepting and waits for the accept thread. Open connections
    /// end with the process.
    pub fn stop(mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.waker.wake();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
