//! Wire format of the sysd pipe protocol.
//!
//! Every message is one frame:
//!
//! ```text
//! u32 LE  payload length (header + body), 8..=MAX_FRAME
//! u16 LE  protocol version (PROTOCOL_VERSION)
//! u16 LE  message kind
//! u32 LE  request id (a response echoes the id of its request)
//! ...     body, defined per kind
//! ```
//!
//! Body fields are little-endian integers, `u8` booleans (0 or 1) and
//! strings as `u16 LE` byte length followed by UTF-8. Decoding never
//! panics: any malformed input is a [`DecodeError`]. Trailing bytes after a
//! body are an error, so a future version cannot be misread as this one.
//!
//! A server that receives a frame with another version answers with
//! [`ErrorCode::UnsupportedVersion`] in its own version and closes the
//! connection.

use std::fmt;
use std::io::{self, Read, Write};

/// The protocol version this crate speaks.
pub const PROTOCOL_VERSION: u16 = 1;

/// Largest accepted payload (header + body) in bytes.
pub const MAX_FRAME: usize = 1 << 20;

/// Size of the header that starts every payload.
pub const HEADER_LEN: usize = 8;

/// Longest accepted Android package name, in bytes.
pub const MAX_PACKAGE_NAME: usize = 255;

mod kind {
    pub const PING: u16 = 0x0001;
    pub const STATUS: u16 = 0x0002;
    pub const START_PACKAGE: u16 = 0x0003;
    pub const FOCUS_WINDOW: u16 = 0x0004;
    pub const LIST_PACKAGES: u16 = 0x0005;

    pub const PONG: u16 = 0x8001;
    pub const STATUS_REPLY: u16 = 0x8002;
    pub const STARTED: u16 = 0x8003;
    pub const FOCUSED: u16 = 0x8004;
    pub const PACKAGES: u16 = 0x8005;
    pub const ERROR: u16 = 0x80FF;
}

/// A request from a client (launcher, manager) to sysd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Liveness check; answered with [`Response::Pong`].
    Ping,
    /// Server state; answered with [`Response::Status`].
    Status,
    /// Start the package's launcher activity, or bring its window forward
    /// when it already runs; answered with [`Response::Started`].
    StartPackage { package: String },
    /// Bring the package's window to the foreground; answered with
    /// [`Response::Focused`].
    FocusWindow { package: String },
    /// The installed packages; answered with [`Response::Packages`].
    ListPackages,
}

/// State reported by [`Response::Status`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// Process id of sysd.
    pub pid: u32,
    /// Milliseconds since sysd started.
    pub uptime_ms: u64,
    /// Open client connections, including the one that asked.
    pub connections: u32,
    /// Running host processes.
    pub hosts: u32,
    /// Idle time after which sysd exits, in milliseconds.
    pub idle_timeout_ms: u64,
    /// The sysd build version (Cargo package version).
    pub version: String,
}

/// A response from sysd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Pong {
        /// The server's protocol version.
        protocol: u16,
    },
    Status(Status),
    Started {
        /// Process id of the host process that runs the package.
        pid: u32,
        /// True when the package was already running and was focused.
        already_running: bool,
    },
    Focused,
    Packages {
        /// Installed package names, sorted.
        packages: Vec<String>,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
}

/// Reason codes of [`Response::Error`]. Unknown codes from a newer server
/// decode as [`ErrorCode::Other`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    /// The frame's protocol version is not supported.
    UnsupportedVersion,
    /// The frame could not be decoded.
    Malformed,
    /// The message kind is not a request this server knows.
    UnknownRequest,
    /// The package name is not a valid Android package name.
    InvalidPackageName,
    /// The package is not installed.
    NotInstalled,
    /// The package has no running process.
    NotRunning,
    /// The package runs but has no window to focus.
    NoWindow,
    /// The host process could not be started.
    HostStartFailed,
    /// The server is at its connection or process limit.
    Busy,
    /// Any other failure inside sysd.
    Internal,
    /// A code this version does not know.
    Other(u16),
}

impl ErrorCode {
    pub fn to_u16(self) -> u16 {
        match self {
            Self::UnsupportedVersion => 1,
            Self::Malformed => 2,
            Self::UnknownRequest => 3,
            Self::InvalidPackageName => 4,
            Self::NotInstalled => 5,
            Self::NotRunning => 6,
            Self::NoWindow => 7,
            Self::HostStartFailed => 8,
            Self::Busy => 9,
            Self::Internal => 10,
            Self::Other(c) => c,
        }
    }

    pub fn from_u16(c: u16) -> Self {
        match c {
            1 => Self::UnsupportedVersion,
            2 => Self::Malformed,
            3 => Self::UnknownRequest,
            4 => Self::InvalidPackageName,
            5 => Self::NotInstalled,
            6 => Self::NotRunning,
            7 => Self::NoWindow,
            8 => Self::HostStartFailed,
            9 => Self::Busy,
            10 => Self::Internal,
            c => Self::Other(c),
        }
    }
}

/// Why a payload could not be decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Shorter than the header, or a field runs past the end.
    Truncated,
    /// Bytes left after the body.
    TrailingBytes,
    /// The header's version is not [`PROTOCOL_VERSION`].
    UnsupportedVersion(u16),
    /// The kind is not known in this direction.
    UnknownKind(u16),
    /// A string is not UTF-8.
    InvalidUtf8,
    /// A boolean is neither 0 nor 1.
    InvalidBool(u8),
    /// The payload length in the frame prefix is out of range.
    BadLength(usize),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(f, "message is truncated"),
            Self::TrailingBytes => write!(f, "message has trailing bytes"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported protocol version {v}"),
            Self::UnknownKind(k) => write!(f, "unknown message kind {k:#06x}"),
            Self::InvalidUtf8 => write!(f, "string is not UTF-8"),
            Self::InvalidBool(b) => write!(f, "invalid boolean {b}"),
            Self::BadLength(n) => write!(f, "frame length {n} is out of range"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Checks an Android package name: two or more dot-separated segments, each
/// starting with an ASCII letter and continuing with ASCII letters, digits
/// or `_`, at most [`MAX_PACKAGE_NAME`] bytes. This is the rule Android's
/// package parser applies to `package` in the manifest.
pub fn is_valid_package_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_PACKAGE_NAME {
        return false;
    }
    let mut segments = 0;
    for seg in name.split('.') {
        let mut chars = seg.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() => {}
            _ => return false,
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}

struct Writer(Vec<u8>);

impl Writer {
    fn new(kind: u16, id: u32) -> Self {
        let mut v = Vec::with_capacity(64);
        v.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        v.extend_from_slice(&kind.to_le_bytes());
        v.extend_from_slice(&id.to_le_bytes());
        Self(v)
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// Strings longer than `u16::MAX` bytes are cut at a character boundary;
    /// no field this protocol defines gets near that.
    fn str(&mut self, s: &str) {
        let mut end = s.len().min(u16::MAX as usize);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        self.u16(end as u16);
        self.0.extend_from_slice(&s.as_bytes()[..end]);
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.0.len() < n {
            return Err(DecodeError::Truncated);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(a))
    }
    fn bool(&mut self) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            b => Err(DecodeError::InvalidBool(b)),
        }
    }
    fn str(&mut self) -> Result<String, DecodeError> {
        let n = self.u16()? as usize;
        let b = self.take(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| DecodeError::InvalidUtf8)
    }
    fn end(&self) -> Result<(), DecodeError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes)
        }
    }
}

/// The header of a payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u16,
    pub kind: u16,
    pub id: u32,
}

/// Reads the header without checking the version, so a server can answer a
/// request of another version with the right id.
pub fn decode_header(payload: &[u8]) -> Result<Header, DecodeError> {
    let mut r = Reader(payload);
    Ok(Header {
        version: r.u16()?,
        kind: r.u16()?,
        id: r.u32()?,
    })
}

fn body(payload: &[u8]) -> Result<(Header, Reader<'_>), DecodeError> {
    let h = decode_header(payload)?;
    if h.version != PROTOCOL_VERSION {
        return Err(DecodeError::UnsupportedVersion(h.version));
    }
    Ok((h, Reader(&payload[HEADER_LEN..])))
}

impl Request {
    pub fn encode(&self, id: u32) -> Vec<u8> {
        let w = match self {
            Self::Ping => Writer::new(kind::PING, id),
            Self::Status => Writer::new(kind::STATUS, id),
            Self::StartPackage { package } => {
                let mut w = Writer::new(kind::START_PACKAGE, id);
                w.str(package);
                w
            }
            Self::FocusWindow { package } => {
                let mut w = Writer::new(kind::FOCUS_WINDOW, id);
                w.str(package);
                w
            }
            Self::ListPackages => Writer::new(kind::LIST_PACKAGES, id),
        };
        w.0
    }

    /// Decodes a request payload, returning the request id with it.
    pub fn decode(payload: &[u8]) -> Result<(u32, Self), DecodeError> {
        let (h, mut r) = body(payload)?;
        let req = match h.kind {
            kind::PING => Self::Ping,
            kind::STATUS => Self::Status,
            kind::START_PACKAGE => Self::StartPackage { package: r.str()? },
            kind::FOCUS_WINDOW => Self::FocusWindow { package: r.str()? },
            kind::LIST_PACKAGES => Self::ListPackages,
            k => return Err(DecodeError::UnknownKind(k)),
        };
        r.end()?;
        Ok((h.id, req))
    }
}

impl Response {
    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Error {
            code,
            message: message.into(),
        }
    }

    pub fn encode(&self, id: u32) -> Vec<u8> {
        let w = match self {
            Self::Pong { protocol } => {
                let mut w = Writer::new(kind::PONG, id);
                w.u16(*protocol);
                w
            }
            Self::Status(s) => {
                let mut w = Writer::new(kind::STATUS_REPLY, id);
                w.u32(s.pid);
                w.u64(s.uptime_ms);
                w.u32(s.connections);
                w.u32(s.hosts);
                w.u64(s.idle_timeout_ms);
                w.str(&s.version);
                w
            }
            Self::Started {
                pid,
                already_running,
            } => {
                let mut w = Writer::new(kind::STARTED, id);
                w.u32(*pid);
                w.u8(u8::from(*already_running));
                w
            }
            Self::Focused => Writer::new(kind::FOCUSED, id),
            Self::Packages { packages } => {
                let mut w = Writer::new(kind::PACKAGES, id);
                w.u32(packages.len() as u32);
                for p in packages {
                    w.str(p);
                }
                w
            }
            Self::Error { code, message } => {
                let mut w = Writer::new(kind::ERROR, id);
                w.u16(code.to_u16());
                w.str(message);
                w
            }
        };
        w.0
    }

    /// Decodes a response payload, returning the request id with it.
    pub fn decode(payload: &[u8]) -> Result<(u32, Self), DecodeError> {
        let (h, mut r) = body(payload)?;
        let resp = match h.kind {
            kind::PONG => Self::Pong { protocol: r.u16()? },
            kind::STATUS_REPLY => Self::Status(Status {
                pid: r.u32()?,
                uptime_ms: r.u64()?,
                connections: r.u32()?,
                hosts: r.u32()?,
                idle_timeout_ms: r.u64()?,
                version: r.str()?,
            }),
            kind::STARTED => Self::Started {
                pid: r.u32()?,
                already_running: r.bool()?,
            },
            kind::FOCUSED => Self::Focused,
            kind::PACKAGES => {
                let n = r.u32()? as usize;
                // Each entry needs at least its two length bytes, so a
                // count larger than that is rejected before allocating.
                if n > r.0.len() / 2 {
                    return Err(DecodeError::Truncated);
                }
                let mut packages = Vec::with_capacity(n);
                for _ in 0..n {
                    packages.push(r.str()?);
                }
                Self::Packages { packages }
            }
            kind::ERROR => Self::Error {
                code: ErrorCode::from_u16(r.u16()?),
                message: r.str()?,
            },
            k => return Err(DecodeError::UnknownKind(k)),
        };
        r.end()?;
        Ok((h.id, resp))
    }
}

/// Writes one frame (length prefix and payload) in a single write call, so
/// a message pipe reader never sees half a frame from one writer.
pub fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    if payload.len() < HEADER_LEN || payload.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            DecodeError::BadLength(payload.len()),
        ));
    }
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf)?;
    w.flush()
}

/// Reads one frame and returns its payload. A clean end of stream before
/// the first byte is `Ok(None)`.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < len.len() {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let n = u32::from_le_bytes(len) as usize;
    if !(HEADER_LEN..=MAX_FRAME).contains(&n) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            DecodeError::BadLength(n),
        ));
    }
    let mut payload = vec![0u8; n];
    r.read_exact(&mut payload)?;
    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_requests() -> Vec<Request> {
        vec![
            Request::Ping,
            Request::Status,
            Request::StartPackage {
                package: "com.example.app".into(),
            },
            Request::FocusWindow {
                package: "org.example.x_1".into(),
            },
            Request::ListPackages,
        ]
    }

    fn all_responses() -> Vec<Response> {
        vec![
            Response::Pong {
                protocol: PROTOCOL_VERSION,
            },
            Response::Status(Status {
                pid: 1234,
                uptime_ms: u64::MAX,
                connections: 2,
                hosts: 1,
                idle_timeout_ms: 30_000,
                version: "0.0.0".into(),
            }),
            Response::Started {
                pid: 77,
                already_running: true,
            },
            Response::Focused,
            Response::Packages { packages: vec![] },
            Response::Packages {
                packages: vec!["a.b".into(), "com.example.\u{5d0}".into()],
            },
            Response::error(ErrorCode::NotInstalled, "not installed"),
            Response::error(ErrorCode::Other(999), ""),
        ]
    }

    #[test]
    fn requests_round_trip() {
        for (i, req) in all_requests().into_iter().enumerate() {
            let bytes = req.encode(i as u32 + 10);
            assert_eq!(Request::decode(&bytes), Ok((i as u32 + 10, req)));
        }
    }

    #[test]
    fn responses_round_trip() {
        for (i, resp) in all_responses().into_iter().enumerate() {
            let bytes = resp.encode(i as u32);
            assert_eq!(Response::decode(&bytes), Ok((i as u32, resp)));
        }
    }

    #[test]
    fn exact_wire_layout() {
        let bytes = Request::StartPackage {
            package: "a.b".into(),
        }
        .encode(0x0102_0304);
        assert_eq!(
            bytes,
            [1, 0, 3, 0, 4, 3, 2, 1, 3, 0, b'a', b'.', b'b'],
            "version, kind, id, string length, string"
        );
        let mut framed = Vec::new();
        write_frame(&mut framed, &bytes).unwrap();
        assert_eq!(&framed[..4], &(bytes.len() as u32).to_le_bytes());
    }

    #[test]
    fn rejects_other_version_and_kinds() {
        let mut bytes = Request::Ping.encode(5);
        bytes[0] = 2;
        assert_eq!(
            Request::decode(&bytes),
            Err(DecodeError::UnsupportedVersion(2))
        );
        assert_eq!(decode_header(&bytes).unwrap().id, 5);
        // A response kind is not a request.
        let pong = Response::Pong { protocol: 1 }.encode(1);
        assert_eq!(
            Request::decode(&pong),
            Err(DecodeError::UnknownKind(0x8001))
        );
        assert_eq!(
            Response::decode(&Request::Ping.encode(1)),
            Err(DecodeError::UnknownKind(1))
        );
    }

    #[test]
    fn rejects_trailing_and_truncated() {
        let mut bytes = Request::Ping.encode(1);
        bytes.push(0);
        assert_eq!(Request::decode(&bytes), Err(DecodeError::TrailingBytes));
        let bytes = Request::StartPackage {
            package: "com.example".into(),
        }
        .encode(1);
        for cut in 0..bytes.len() {
            assert!(Request::decode(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        let mut bad_bool = Response::Started {
            pid: 1,
            already_running: false,
        }
        .encode(1);
        *bad_bool.last_mut().unwrap() = 2;
        assert_eq!(
            Response::decode(&bad_bool),
            Err(DecodeError::InvalidBool(2))
        );
    }

    #[test]
    fn huge_package_count_is_rejected_without_allocating() {
        let mut bytes = Response::Packages { packages: vec![] }.encode(1);
        bytes[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(Response::decode(&bytes), Err(DecodeError::Truncated));
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // A small deterministic generator (xorshift) stands in for a fuzzer.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..20_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let len = (x % 40) as usize;
            let mut buf = Vec::with_capacity(len);
            let mut y = x;
            for _ in 0..len {
                y = y.wrapping_mul(6364136223846793005).wrapping_add(1);
                buf.push((y >> 56) as u8);
            }
            if len >= 2 {
                buf[0] = 1;
                buf[1] = 0;
            }
            let _ = Request::decode(&buf);
            let _ = Response::decode(&buf);
        }
    }

    #[test]
    fn frames_round_trip_and_bounds() {
        let payload = Request::ListPackages.encode(9);
        let mut wire = Vec::new();
        write_frame(&mut wire, &payload).unwrap();
        write_frame(&mut wire, &payload).unwrap();
        let mut r = &wire[..];
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), payload);
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), payload);
        assert!(read_frame(&mut r).unwrap().is_none());

        let too_big = ((MAX_FRAME + 1) as u32).to_le_bytes();
        assert!(read_frame(&mut &too_big[..]).is_err());
        let too_small = 3u32.to_le_bytes();
        assert!(read_frame(&mut &too_small[..]).is_err());
        assert!(read_frame(&mut &[1u8, 0][..]).is_err(), "cut in the prefix");
        assert!(write_frame(&mut Vec::new(), &[0u8; 3]).is_err());
    }

    #[test]
    fn package_names() {
        for ok in ["com.example", "a.b", "com.example.app_2", "A.B9"] {
            assert!(is_valid_package_name(ok), "{ok}");
        }
        for bad in [
            "",
            "com",
            "com.",
            ".com",
            "com..x",
            "com.9x",
            "com.ex-ample",
            "com.ex ample",
            "com.\u{5d0}",
            "..",
            "c:\\x.y",
            "a.b/../c.d",
        ] {
            assert!(!is_valid_package_name(bad), "{bad}");
        }
        let long = format!("a.{}", "b".repeat(MAX_PACKAGE_NAME));
        assert!(!is_valid_package_name(&long));
    }
}
