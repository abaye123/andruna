//! Structured logging to `%LOCALAPPDATA%\Andruna\logs`.
//!
//! One line per event in logfmt style:
//!
//! ```text
//! ts=2026-11-27T10:00:00.123Z level=info pid=1234 event=start_package package=com.example.app
//! ```
//!
//! Each process kind writes its own file (`sysd.log`, `host.log`). When a
//! file passes [`MAX_FILE_BYTES`], it is renamed to `<name>.1.log` (older
//! files shift up to `<name>.<KEEP_FILES>.log`, the oldest is deleted) and a
//! new file is started. Before [`init`] runs, events are dropped, so library
//! code used by tests logs nothing.

use std::fmt::{Display, Write as _};
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Size at which a log file is rotated.
pub const MAX_FILE_BYTES: u64 = 1 << 20;
/// Rotated files kept next to the current one.
pub const KEEP_FILES: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

struct Sink {
    dir: PathBuf,
    stem: String,
    file: File,
    size: u64,
    max: u64,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

fn path_of(dir: &Path, stem: &str, n: u32) -> PathBuf {
    if n == 0 {
        dir.join(format!("{stem}.log"))
    } else {
        dir.join(format!("{stem}.{n}.log"))
    }
}

fn open(dir: &Path, stem: &str) -> std::io::Result<(File, u64)> {
    let f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path_of(dir, stem, 0))?;
    let size = f.metadata()?.len();
    Ok((f, size))
}

/// Shifts `<stem>.log` to `<stem>.1.log` and so on. Errors are ignored: a
/// second process may be rotating the same files at the same moment.
fn rotate(dir: &Path, stem: &str) {
    let _ = fs::remove_file(path_of(dir, stem, KEEP_FILES));
    for n in (0..KEEP_FILES).rev() {
        let _ = fs::rename(path_of(dir, stem, n), path_of(dir, stem, n + 1));
    }
}

impl Sink {
    fn new(dir: &Path, stem: &str, max: u64) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let (mut file, mut size) = open(dir, stem)?;
        if size >= max {
            drop(file);
            rotate(dir, stem);
            (file, size) = open(dir, stem)?;
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            stem: stem.to_string(),
            file,
            size,
            max,
        })
    }

    fn write(&mut self, line: &str) {
        if self.size + line.len() as u64 > self.max && self.size > 0 {
            rotate(&self.dir, &self.stem);
            if let Ok((f, s)) = open(&self.dir, &self.stem) {
                self.file = f;
                self.size = s;
            }
        }
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.size += line.len() as u64;
        }
    }
}

/// Starts logging to `<dir>\<stem>.log`. Only the first call has an effect.
pub fn init(dir: &Path, stem: &str) -> std::io::Result<()> {
    let sink = Sink::new(dir, stem, MAX_FILE_BYTES)?;
    let _ = SINK.set(Mutex::new(sink));
    Ok(())
}

/// Writes `v` as a logfmt value: bare when it has no spaces, quotes, `=`
/// or control characters, otherwise quoted with escapes.
fn push_value(out: &mut String, v: &str) {
    let bare = !v.is_empty()
        && !v
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '=');
    if bare {
        out.push_str(v);
        return;
    }
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Formats one log line (with the trailing newline).
pub fn format_line(ts: &str, level: Level, event: &str, fields: &[(&str, &dyn Display)]) -> String {
    let mut line = format!(
        "ts={ts} level={} pid={} event=",
        level.as_str(),
        std::process::id()
    );
    push_value(&mut line, event);
    let mut buf = String::new();
    for (k, v) in fields {
        buf.clear();
        let _ = write!(buf, "{v}");
        line.push(' ');
        line.push_str(k);
        line.push('=');
        push_value(&mut line, &buf);
    }
    line.push('\n');
    line
}

/// Writes one event. Use the [`info!`](crate::info), [`warn!`](crate::warn)
/// and [`error!`](crate::error) macros.
pub fn emit(level: Level, event: &str, fields: &[(&str, &dyn Display)]) {
    let Some(sink) = SINK.get() else { return };
    let line = format_line(&crate::sys::utc_timestamp(), level, event, fields);
    if let Ok(mut s) = sink.lock() {
        s.write(&line);
    }
}

#[macro_export]
macro_rules! info {
    ($event:expr $(, $k:ident = $v:expr)* $(,)?) => {
        $crate::log::emit($crate::log::Level::Info, $event,
            &[$((stringify!($k), &$v as &dyn ::std::fmt::Display)),*])
    };
}

#[macro_export]
macro_rules! warn {
    ($event:expr $(, $k:ident = $v:expr)* $(,)?) => {
        $crate::log::emit($crate::log::Level::Warn, $event,
            &[$((stringify!($k), &$v as &dyn ::std::fmt::Display)),*])
    };
}

#[macro_export]
macro_rules! error {
    ($event:expr $(, $k:ident = $v:expr)* $(,)?) => {
        $crate::log::emit($crate::log::Level::Error, $event,
            &[$((stringify!($k), &$v as &dyn ::std::fmt::Display)),*])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_format_quotes_values() {
        let line = format_line(
            "T",
            Level::Warn,
            "host exited",
            &[("package", &"com.example"), ("msg", &"a \"b\"\n=c")],
        );
        let pid = std::process::id();
        assert_eq!(
            line,
            format!(
                "ts=T level=warn pid={pid} event=\"host exited\" package=com.example msg=\"a \\\"b\\\"\\n=c\"\n"
            )
        );
        assert!(format_line("T", Level::Info, "x", &[("e", &"")]).contains("e=\"\""));
    }

    #[test]
    fn rotation_keeps_a_bounded_set() {
        let dir = std::env::temp_dir().join(format!("andruna-log-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut sink = Sink::new(&dir, "t", 100).unwrap();
        let line = format!("{}\n", "x".repeat(39));
        for _ in 0..40 {
            sink.write(&line);
        }
        drop(sink);
        for n in 0..=KEEP_FILES {
            let p = path_of(&dir, "t", n);
            let len = fs::metadata(&p).unwrap().len();
            assert!(len > 0 && len <= 100, "{} has {len} bytes", p.display());
        }
        assert!(!path_of(&dir, "t", KEEP_FILES + 1).exists());
        // A full file is rotated at open.
        fs::write(path_of(&dir, "t", 0), "y".repeat(200)).unwrap();
        let sink = Sink::new(&dir, "t", 100).unwrap();
        assert_eq!(sink.size, 0);
        assert_eq!(fs::metadata(path_of(&dir, "t", 1)).unwrap().len(), 200);
        drop(sink);
        fs::remove_dir_all(&dir).unwrap();
    }
}
