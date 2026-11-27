//! The pipe server: accepts clients and answers their requests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use andruna_ipc::proto::{self, DecodeError};
use andruna_ipc::{
    Connection, ErrorCode, PROTOCOL_VERSION, PipeListener, Request, Response, Status, Waker,
};

use crate::activity::Activity;
use crate::supervisor::Supervisor;
use crate::{Layout, info, packages, warn};

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
    /// Answers one request.
    pub fn handle(&self, req: Request) -> Response {
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
        }
    }

    /// Serves one connection until the client closes it or sends a frame
    /// that cannot be answered.
    pub fn serve(&self, conn: &mut Connection) {
        loop {
            let payload = match conn.read_frame() {
                Ok(Some(p)) => p,
                Ok(None) => return,
                Err(e) => {
                    warn!(
                        "client_read_failed",
                        client_pid = conn.peer_pid(),
                        error = e
                    );
                    return;
                }
            };
            let id = proto::decode_header(&payload).map(|h| h.id).unwrap_or(0);
            let (resp, keep_open) = match Request::decode(&payload) {
                Ok((_, req)) => (self.handle(req), true),
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
                return;
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
                                client_ctx.serve(&mut conn);
                                client_ctx.activity.connection_closed();
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
