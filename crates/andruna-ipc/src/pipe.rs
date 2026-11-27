//! The named-pipe transport: endpoint names, pipe security, the server's
//! listener and the client.

use std::fmt;
use std::fs::File;
use std::io;
use std::time::{Duration, Instant};

use crate::proto::{self, DecodeError, Request, Response};
use crate::sys::{self, SecurityDescriptor, Sid};

/// Instance used by the shipped product. Other instances exist so tests
/// and development builds can run next to it without sharing state.
pub const DEFAULT_INSTANCE: &str = "default";

/// Checks an instance name: 1 to 40 ASCII letters, digits, `-` or `_`.
pub fn is_valid_instance(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Names of the per-user, per-session sysd objects.
///
/// sysd is one per user and Windows session: the names contain the user's
/// SID and the session id. The pipe namespace is global, so the user SID in
/// the name alone does not stop another user from creating it first; the
/// server therefore creates its first instance with
/// `FILE_FLAG_FIRST_PIPE_INSTANCE`, and the client checks the user of the
/// server process ([`Client::connect`]).
#[derive(Clone, Debug)]
pub struct Endpoint {
    user: Sid,
    session: u32,
    instance: String,
}

impl Endpoint {
    pub fn for_current_user(instance: &str) -> io::Result<Self> {
        if !is_valid_instance(instance) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid instance name `{instance}`"),
            ));
        }
        Ok(Self {
            user: sys::current_user_sid()?,
            session: sys::current_session_id()?,
            instance: instance.to_string(),
        })
    }

    pub fn user(&self) -> &Sid {
        &self.user
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// `\\.\pipe\Andruna.sysd.<user SID>.<session>.<instance>`
    pub fn pipe_name(&self) -> String {
        format!(
            r"\\.\pipe\Andruna.sysd.{}.{}.{}",
            self.user.to_string_sid(),
            self.session,
            self.instance
        )
    }

    /// `Local\Andruna.sysd.<user SID>.<instance>`: the single-instance
    /// mutex, in the session's own object namespace.
    pub fn mutex_name(&self) -> String {
        format!(
            r"Local\Andruna.sysd.{}.{}",
            self.user.to_string_sid(),
            self.instance
        )
    }
}

/// Who may open the pipe.
///
/// Today only the current user. App processes will run in per-package
/// AppContainers ([security](../../docs/spec/04-security.md)); when a
/// container needs to reach sysd over this pipe, its package SID is added
/// with [`PipeSecurity::allow_app_container`], which also lowers the
/// pipe's mandatory label so low-integrity container processes can write.
#[derive(Clone, Debug)]
pub struct PipeSecurity {
    user: String,
    app_containers: Vec<String>,
}

impl PipeSecurity {
    /// Full access for the endpoint's user, nobody else (protected DACL,
    /// so nothing is inherited).
    pub fn current_user_only(endpoint: &Endpoint) -> Self {
        Self {
            user: endpoint.user.to_string_sid(),
            app_containers: Vec::new(),
        }
    }

    /// Hook for AppContainer isolation: grants read and write to the
    /// AppContainer SID `sid` (`S-1-15-2-...`). Not used yet.
    pub fn allow_app_container(&mut self, sid: &str) -> io::Result<()> {
        if !sid.starts_with("S-1-15-2-")
            || !sid[2..].bytes().all(|b| b.is_ascii_digit() || b == b'-')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("not an AppContainer SID: `{sid}`"),
            ));
        }
        self.app_containers.push(sid.to_string());
        Ok(())
    }

    /// The SDDL string of the pipe's security descriptor.
    pub fn sddl(&self) -> String {
        let mut s = format!("O:{0}D:P(A;;GA;;;{0})", self.user);
        for ac in &self.app_containers {
            s.push_str(&format!("(A;;GRGW;;;{ac})"));
        }
        if !self.app_containers.is_empty() {
            s.push_str("S:(ML;;NW;;;LW)");
        }
        s
    }
}

/// The server side: accepts one client at a time on a pipe it owns.
pub struct PipeListener {
    name: Vec<u16>,
    sd: SecurityDescriptor,
    pending: std::os::windows::io::OwnedHandle,
}

impl PipeListener {
    /// Creates the first pipe instance. Fails with `PermissionDenied` (or
    /// `ERROR_ACCESS_DENIED`) when the name already exists.
    pub fn bind(endpoint: &Endpoint, security: &PipeSecurity) -> io::Result<Self> {
        let name = sys::wide(&endpoint.pipe_name());
        let sd = SecurityDescriptor::from_sddl(&security.sddl())?;
        let pending = sys::create_pipe_instance(&name, true, &sd)?;
        Ok(Self { name, sd, pending })
    }

    /// Blocks until a client connects and returns the connection. A new
    /// instance is created for the next client before this returns.
    pub fn accept(&mut self) -> io::Result<Connection> {
        sys::connect_pipe_instance(&self.pending)?;
        let next = sys::create_pipe_instance(&self.name, false, &self.sd)?;
        let handle = std::mem::replace(&mut self.pending, next);
        let peer_pid = sys::pipe_client_pid(&handle).unwrap_or(0);
        Ok(Connection {
            file: File::from(handle),
            peer_pid,
        })
    }

    /// A handle that unblocks [`PipeListener::accept`] from another thread.
    pub fn waker(&self) -> Waker {
        Waker {
            name: self.name.clone(),
        }
    }
}

/// Unblocks a listener by connecting and closing a client.
#[derive(Clone)]
pub struct Waker {
    name: Vec<u16>,
}

impl Waker {
    pub fn wake(&self) {
        let _ = sys::open_pipe_client(&self.name);
    }
}

/// One accepted client connection.
pub struct Connection {
    file: File,
    peer_pid: u32,
}

impl Connection {
    /// Process id of the client (0 if Windows did not report it).
    pub fn peer_pid(&self) -> u32 {
        self.peer_pid
    }

    /// Reads one frame; `Ok(None)` when the client closed the pipe.
    pub fn read_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        proto::read_frame(&mut self.file)
    }

    pub fn write_frame(&mut self, payload: &[u8]) -> io::Result<()> {
        proto::write_frame(&mut self.file, payload)
    }
}

/// Client errors.
#[derive(Debug)]
pub enum Error {
    /// No sysd is listening (within the wait time).
    NotRunning,
    /// The pipe's server process runs as another user.
    WrongServerUser {
        pid: u32,
    },
    /// The server closed the connection before answering.
    Closed,
    /// The answer could not be decoded.
    Decode(DecodeError),
    /// The answer does not belong to the request.
    UnexpectedId {
        expected: u32,
        got: u32,
    },
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => write!(f, "andruna-sysd is not running"),
            Self::WrongServerUser { pid } => write!(
                f,
                "the sysd pipe is served by process {pid}, which runs as another user"
            ),
            Self::Closed => write!(f, "andruna-sysd closed the connection"),
            Self::Decode(e) => write!(f, "bad answer from andruna-sysd: {e}"),
            Self::UnexpectedId { expected, got } => write!(
                f,
                "answer for request {got} received while waiting for {expected}"
            ),
            Self::Io(e) => write!(f, "pipe error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;

/// A connection to sysd.
pub struct Client {
    file: File,
    server_pid: u32,
    next_id: u32,
}

impl Client {
    /// Connects to the endpoint's pipe, waiting up to `wait` for it to
    /// appear or for a free instance. The server process must run as the
    /// same user as this process.
    pub fn connect(endpoint: &Endpoint, wait: Duration) -> Result<Self, Error> {
        let name = sys::wide(&endpoint.pipe_name());
        let deadline = Instant::now() + wait;
        let handle = loop {
            match sys::open_pipe_client(&name) {
                Ok(h) => break h,
                Err(e) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(match e.raw_os_error() {
                            Some(ERROR_FILE_NOT_FOUND) | Some(ERROR_PIPE_BUSY) => Error::NotRunning,
                            _ => Error::Io(e),
                        });
                    }
                    match e.raw_os_error() {
                        Some(ERROR_PIPE_BUSY) => {
                            let ms = (deadline - now).as_millis().clamp(1, 1000) as u32;
                            sys::wait_named_pipe(&name, ms);
                        }
                        Some(ERROR_FILE_NOT_FOUND) => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        _ => return Err(Error::Io(e)),
                    }
                }
            }
        };
        let server_pid = sys::pipe_server_pid(&handle)?;
        match sys::process_user_sid(server_pid) {
            Ok(sid) if &sid == endpoint.user() => {}
            _ => return Err(Error::WrongServerUser { pid: server_pid }),
        }
        Ok(Self {
            file: File::from(handle),
            server_pid,
            next_id: 1,
        })
    }

    /// Process id of the sysd that serves this connection.
    pub fn server_pid(&self) -> u32 {
        self.server_pid
    }

    /// Sends a request and waits for its answer.
    pub fn request(&mut self, req: &Request) -> Result<Response, Error> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        proto::write_frame(&mut self.file, &req.encode(id))?;
        let payload = proto::read_frame(&mut self.file)?.ok_or(Error::Closed)?;
        let (got, resp) = Response::decode(&payload).map_err(Error::Decode)?;
        if got != id {
            return Err(Error::UnexpectedId { expected: id, got });
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names() {
        assert!(is_valid_instance("default"));
        assert!(is_valid_instance("test-12_a"));
        assert!(!is_valid_instance(""));
        assert!(!is_valid_instance("a\\b"));
        assert!(!is_valid_instance(&"a".repeat(41)));
        assert!(Endpoint::for_current_user("bad name").is_err());
    }

    #[test]
    fn names_contain_user_and_instance() {
        let e = Endpoint::for_current_user("unit").unwrap();
        let sid = e.user().to_string_sid();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        assert!(e.pipe_name().starts_with(r"\\.\pipe\Andruna.sysd.S-1-5-"));
        assert!(e.pipe_name().ends_with(".unit"));
        assert_eq!(e.mutex_name(), format!(r"Local\Andruna.sysd.{sid}.unit"));
    }

    #[test]
    fn security_descriptor() {
        let e = Endpoint::for_current_user("unit").unwrap();
        let mut s = PipeSecurity::current_user_only(&e);
        let user = e.user().to_string_sid();
        assert_eq!(s.sddl(), format!("O:{user}D:P(A;;GA;;;{user})"));
        SecurityDescriptor::from_sddl(&s.sddl()).unwrap();

        assert!(s.allow_app_container("S-1-5-32-544").is_err());
        assert!(s.allow_app_container("S-1-15-2-1-x)(A;;GA;;;WD").is_err());
        s.allow_app_container("S-1-15-2-1-2-3-4-5-6-7").unwrap();
        assert!(
            s.sddl()
                .ends_with("(A;;GRGW;;;S-1-15-2-1-2-3-4-5-6-7)S:(ML;;NW;;;LW)")
        );
        SecurityDescriptor::from_sddl(&s.sddl()).unwrap();
    }
}
