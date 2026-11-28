//! The host channel: sysd and one app host process (`andruna-app --host`).
//!
//! sysd creates two anonymous pipes for each host and passes one end of
//! each in the process's handle list ([ADR 0036](../../../docs/decisions/0036-appcontainer-file-access-and-sysd-channel.md)):
//! the host reads [`ToHost`] messages from one and writes [`FromHost`]
//! messages to the other. Nothing is named, so nothing else on the machine
//! can open the channel.
//!
//! Frames use the length prefix and header layout of [`crate::proto`]
//! (`u32` length, `u16` version, `u16` kind, `u32` id), with their own
//! version [`HOST_PROTOCOL_VERSION`] and kinds; the id is always 0. Body
//! encoding is the same: little-endian integers, `u8` booleans, strings as
//! `u16` byte length and UTF-8, lists as `u16` count and items.
//!
//! The exchange:
//!
//! 1. host: [`FromHost::Hello`] as soon as it runs.
//! 2. sysd: [`ToHost::Attach`]: the package, what to run and the boot
//!    image sysd mapped into the process before it was resumed.
//! 3. host: [`FromHost::Running`] just before the runtime starts, or
//!    [`FromHost::Refused`] (and exit) when the assignment cannot run.
//! 4. host, any time: [`FromHost::MissingFunctions`]. The app's standard
//!    output and error are not on this channel: they are pipes sysd reads
//!    itself, so nothing written before the process ends is lost.
//! 5. host: [`FromHost::Exit`] with the exit status and, when the app
//!    crashed, a [`Crash`] report; then the process ends.
//!
//! sysd may send [`ToHost::Stop`] at any time; the host ends with status 0
//! without a crash report. A host that ends without [`FromHost::Exit`]
//! (killed, or a fault in the host itself) is reported by sysd from its
//! exit code.

use std::io::{self, Read, Write};

use crate::proto::{DecodeError, HEADER_LEN, read_frame, write_frame};

/// Version of the host channel protocol.
pub const HOST_PROTOCOL_VERSION: u16 = 1;

/// Longest crash summary, in bytes.
pub const MAX_SUMMARY: usize = 8192;

mod kind {
    pub const ATTACH: u16 = 0x0101;
    pub const STOP: u16 = 0x0102;

    pub const HELLO: u16 = 0x0181;
    pub const RUNNING: u16 = 0x0182;
    pub const REFUSED: u16 = 0x0183;
    pub const MISSING: u16 = 0x0184;
    pub const EXIT: u16 = 0x0185;
}

/// What the host runs for the package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// `main` of `class` with the class path (Windows paths), as
    /// `dalvikvm` runs it. Tag 1.
    Main {
        classpath: Vec<String>,
        class: String,
        args: Vec<String>,
    },
    /// The Android framework entry (`ActivityThread.main` through an
    /// `app_process`-like start), with the activity to open (`None`: the
    /// launcher activity). Tag 2.
    Framework { activity: Option<String> },
}

/// One run of file pages in the boot image view: bytes
/// `[offset, offset+len)` of the file at `path` are at `[addr, addr+len)`.
/// `path` is relative to the ART root with `/` separators (for example
/// `javalib/x86_64/boot.oat`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extent {
    pub path: String,
    pub offset: u64,
    pub addr: u64,
    pub len: u64,
}

/// The boot image view sysd mapped into the process before it ran:
/// `[base, base+size)`, read-only, holding [`BootImage::extents`] and
/// zeros elsewhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootImage {
    pub base: u64,
    pub size: u64,
    pub extents: Vec<Extent>,
}

/// The assignment of a host process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attach {
    pub package: String,
    /// The package's data directory (Windows path), mounted as the guest
    /// root.
    pub data_dir: String,
    /// The ART root (Windows path).
    pub art_root: String,
    pub entry: Entry,
    /// `int`, `jit` or `aot`.
    pub mode: String,
    /// Extra runtime options (`-X...`).
    pub runtime_options: Vec<String>,
    pub boot_image: Option<BootImage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToHost {
    Attach(Box<Attach>),
    /// End the app now (status 0, no crash report).
    Stop,
}

/// What kind of crash ended the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashKind {
    /// An exception no Java code caught. Code 1.
    UncaughtException,
    /// A fault in native or compiled code that no handler took (a fatal
    /// signal). Code 2.
    NativeFault,
    /// The runtime aborted (`abort`, a failed check). Code 3.
    Abort,
    /// A non-zero exit without a recognized report. Code 4.
    Other,
}

impl CrashKind {
    fn to_u8(self) -> u8 {
        match self {
            Self::UncaughtException => 1,
            Self::NativeFault => 2,
            Self::Abort => 3,
            Self::Other => 4,
        }
    }
    fn from_u8(v: u8) -> Result<Self, DecodeError> {
        Ok(match v {
            1 => Self::UncaughtException,
            2 => Self::NativeFault,
            3 => Self::Abort,
            4 => Self::Other,
            k => return Err(DecodeError::UnknownKind(u16::from(k))),
        })
    }
}

/// A crash report: its kind and a summary for the user and the log (the
/// exception and its first frames, or the fault and where it happened).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crash {
    pub kind: CrashKind,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromHost {
    Hello {
        pid: u32,
        version: String,
    },
    Running,
    /// The assignment cannot run; the host exits after this.
    Refused {
        message: String,
    },
    /// Missing-function counters (`<area>:<entry point>`, calls).
    MissingFunctions {
        entries: Vec<(String, u64)>,
    },
    /// The app ended with `code` (its `exit_group` status). `crash` is set
    /// when the host itself recognized a crash; sysd adds what the app
    /// wrote to standard error.
    Exit {
        code: i32,
        crash: Option<Crash>,
    },
}

struct W(Vec<u8>);

impl W {
    fn new(kind: u16) -> Self {
        let mut v = Vec::with_capacity(64);
        v.extend_from_slice(&HOST_PROTOCOL_VERSION.to_le_bytes());
        v.extend_from_slice(&kind.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
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
    fn str(&mut self, s: &str) {
        let mut end = s.len().min(u16::MAX as usize);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        self.u16(end as u16);
        self.0.extend_from_slice(&s.as_bytes()[..end]);
    }
    fn strs(&mut self, v: &[String]) {
        let n = v.len().min(u16::MAX as usize);
        self.u16(n as u16);
        for s in &v[..n] {
            self.str(s);
        }
    }
}

struct R<'a>(&'a [u8]);

impl<'a> R<'a> {
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
    fn strs(&mut self) -> Result<Vec<String>, DecodeError> {
        let n = self.u16()? as usize;
        // Each string needs at least its two length bytes.
        if n > self.0.len() / 2 {
            return Err(DecodeError::Truncated);
        }
        (0..n).map(|_| self.str()).collect()
    }
    fn end(&self) -> Result<(), DecodeError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes)
        }
    }
}

fn header(payload: &[u8]) -> Result<(u16, R<'_>), DecodeError> {
    if payload.len() < HEADER_LEN {
        return Err(DecodeError::Truncated);
    }
    let mut r = R(payload);
    let version = r.u16()?;
    if version != HOST_PROTOCOL_VERSION {
        return Err(DecodeError::UnsupportedVersion(version));
    }
    let kind = r.u16()?;
    let _id = r.u32()?;
    Ok((kind, r))
}

impl ToHost {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Stop => W::new(kind::STOP).0,
            Self::Attach(a) => {
                let mut w = W::new(kind::ATTACH);
                w.str(&a.package);
                w.str(&a.data_dir);
                w.str(&a.art_root);
                match &a.entry {
                    Entry::Main {
                        classpath,
                        class,
                        args,
                    } => {
                        w.u8(1);
                        w.strs(classpath);
                        w.str(class);
                        w.strs(args);
                    }
                    Entry::Framework { activity } => {
                        w.u8(2);
                        w.u8(u8::from(activity.is_some()));
                        w.str(activity.as_deref().unwrap_or(""));
                    }
                }
                w.str(&a.mode);
                w.strs(&a.runtime_options);
                match &a.boot_image {
                    None => w.u8(0),
                    Some(b) => {
                        w.u8(1);
                        w.u64(b.base);
                        w.u64(b.size);
                        let n = b.extents.len().min(u16::MAX as usize);
                        w.u16(n as u16);
                        for x in &b.extents[..n] {
                            w.str(&x.path);
                            w.u64(x.offset);
                            w.u64(x.addr);
                            w.u64(x.len);
                        }
                    }
                }
                w.0
            }
        }
    }

    pub fn decode(payload: &[u8]) -> Result<Self, DecodeError> {
        let (k, mut r) = header(payload)?;
        let m = match k {
            kind::STOP => Self::Stop,
            kind::ATTACH => {
                let package = r.str()?;
                let data_dir = r.str()?;
                let art_root = r.str()?;
                let entry = match r.u8()? {
                    1 => Entry::Main {
                        classpath: r.strs()?,
                        class: r.str()?,
                        args: r.strs()?,
                    },
                    2 => {
                        let has = r.bool()?;
                        let a = r.str()?;
                        Entry::Framework {
                            activity: has.then_some(a),
                        }
                    }
                    t => return Err(DecodeError::UnknownKind(u16::from(t))),
                };
                let mode = r.str()?;
                let runtime_options = r.strs()?;
                let boot_image = if r.bool()? {
                    let base = r.u64()?;
                    let size = r.u64()?;
                    let n = r.u16()? as usize;
                    if n > r.0.len() / 26 {
                        return Err(DecodeError::Truncated);
                    }
                    let mut extents = Vec::with_capacity(n);
                    for _ in 0..n {
                        extents.push(Extent {
                            path: r.str()?,
                            offset: r.u64()?,
                            addr: r.u64()?,
                            len: r.u64()?,
                        });
                    }
                    Some(BootImage {
                        base,
                        size,
                        extents,
                    })
                } else {
                    None
                };
                Self::Attach(Box::new(Attach {
                    package,
                    data_dir,
                    art_root,
                    entry,
                    mode,
                    runtime_options,
                    boot_image,
                }))
            }
            k => return Err(DecodeError::UnknownKind(k)),
        };
        r.end()?;
        Ok(m)
    }
}

fn cut(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

impl FromHost {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Hello { pid, version } => {
                let mut w = W::new(kind::HELLO);
                w.u32(*pid);
                w.str(version);
                w.0
            }
            Self::Running => W::new(kind::RUNNING).0,
            Self::Refused { message } => {
                let mut w = W::new(kind::REFUSED);
                w.str(message);
                w.0
            }
            Self::MissingFunctions { entries } => {
                let mut w = W::new(kind::MISSING);
                let n = entries.len().min(4096);
                w.u16(n as u16);
                for (name, calls) in &entries[..n] {
                    w.str(name);
                    w.u64(*calls);
                }
                w.0
            }
            Self::Exit { code, crash } => {
                let mut w = W::new(kind::EXIT);
                w.u32(*code as u32);
                match crash {
                    None => w.u8(0),
                    Some(c) => {
                        w.u8(c.kind.to_u8());
                        w.str(cut(&c.summary, MAX_SUMMARY));
                    }
                }
                w.0
            }
        }
    }

    pub fn decode(payload: &[u8]) -> Result<Self, DecodeError> {
        let (k, mut r) = header(payload)?;
        let m = match k {
            kind::HELLO => Self::Hello {
                pid: r.u32()?,
                version: r.str()?,
            },
            kind::RUNNING => Self::Running,
            kind::REFUSED => Self::Refused { message: r.str()? },
            kind::MISSING => {
                let n = r.u16()? as usize;
                if n > r.0.len() / 10 {
                    return Err(DecodeError::Truncated);
                }
                let mut entries = Vec::with_capacity(n);
                for _ in 0..n {
                    entries.push((r.str()?, r.u64()?));
                }
                Self::MissingFunctions { entries }
            }
            kind::EXIT => {
                let code = r.u32()? as i32;
                let crash = match r.u8()? {
                    0 => None,
                    k => Some(Crash {
                        kind: CrashKind::from_u8(k)?,
                        summary: r.str()?,
                    }),
                };
                Self::Exit { code, crash }
            }
            k => return Err(DecodeError::UnknownKind(k)),
        };
        r.end()?;
        Ok(m)
    }
}

/// Sends one message.
pub fn send<W2: Write>(w: &mut W2, payload: &[u8]) -> io::Result<()> {
    write_frame(w, payload)
}

/// Receives one [`ToHost`] message; `Ok(None)` at the end of the stream.
pub fn recv_to_host<R2: Read>(r: &mut R2) -> io::Result<Option<ToHost>> {
    match read_frame(r)? {
        None => Ok(None),
        Some(p) => ToHost::decode(&p)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
    }
}

/// Receives one [`FromHost`] message; `Ok(None)` at the end of the stream.
pub fn recv_from_host<R2: Read>(r: &mut R2) -> io::Result<Option<FromHost>> {
    match read_frame(r)? {
        None => Ok(None),
        Some(p) => FromHost::decode(&p)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attach(entry: Entry, boot: bool) -> ToHost {
        ToHost::Attach(Box::new(Attach {
            package: "com.example.app".into(),
            data_dir: r"C:\d\data\com.example.app".into(),
            art_root: r"C:\p\art".into(),
            entry,
            mode: "jit".into(),
            runtime_options: vec!["-Xmx64m".into()],
            boot_image: boot.then(|| BootImage {
                base: 0x6000_0000,
                size: 0x1bc_0000,
                extents: vec![Extent {
                    path: "javalib/x86_64/boot.art".into(),
                    offset: 0,
                    addr: 0x6000_0000,
                    len: 0x4d_0000,
                }],
            }),
        }))
    }

    #[test]
    fn round_trips() {
        let to = [
            ToHost::Stop,
            attach(
                Entry::Main {
                    classpath: vec![r"C:\a\hello.dex".into(), "b.jar".into()],
                    class: "Hello".into(),
                    args: vec!["x".into()],
                },
                true,
            ),
            attach(Entry::Framework { activity: None }, false),
            attach(
                Entry::Framework {
                    activity: Some(".Main".into()),
                },
                true,
            ),
        ];
        for m in to {
            assert_eq!(ToHost::decode(&m.encode()).unwrap(), m);
        }
        let from = [
            FromHost::Hello {
                pid: 7,
                version: "0.1.0".into(),
            },
            FromHost::Running,
            FromHost::Refused {
                message: "no".into(),
            },
            FromHost::MissingFunctions {
                entries: vec![("lx:epoll_pwait2".into(), 3)],
            },
            FromHost::Exit {
                code: 0,
                crash: None,
            },
            FromHost::Exit {
                code: 1,
                crash: Some(Crash {
                    kind: CrashKind::UncaughtException,
                    summary: "java.lang.NullPointerException".into(),
                }),
            },
        ];
        for m in from {
            assert_eq!(FromHost::decode(&m.encode()).unwrap(), m);
        }
    }

    #[test]
    fn rejects_malformed() {
        let ok = FromHost::Running.encode();
        let mut bad = ok.clone();
        bad[0] = 9;
        assert!(matches!(
            FromHost::decode(&bad),
            Err(DecodeError::UnsupportedVersion(9))
        ));
        let mut trailing = ok.clone();
        trailing.push(0);
        assert!(FromHost::decode(&trailing).is_err());
        assert!(FromHost::decode(&ok[..4]).is_err());
        // Host and sysd kinds are not interchangeable.
        assert!(ToHost::decode(&ok).is_err());
        assert!(FromHost::decode(&ToHost::Stop.encode()).is_err());
        let full = attach(
            Entry::Main {
                classpath: vec!["a".into()],
                class: "C".into(),
                args: vec![],
            },
            true,
        )
        .encode();
        for n in HEADER_LEN..full.len() {
            assert!(ToHost::decode(&full[..n]).is_err(), "{n}");
        }
    }

    #[test]
    fn frames_over_a_pipe() {
        let (mut r, mut w) = std::io::pipe().unwrap();
        send(&mut w, &FromHost::Running.encode()).unwrap();
        drop(w);
        assert_eq!(recv_from_host(&mut r).unwrap(), Some(FromHost::Running));
        assert_eq!(recv_from_host(&mut r).unwrap(), None);
    }
}
