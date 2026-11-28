//! Crash reports from what an app wrote to standard error before it ended.
//!
//! The runtime reports a crash on standard error, as it does into logcat on
//! a device: an uncaught exception as `Exception in thread "<name>" ...`
//! with its stack, a native fault through the linker's report
//! (`[andruna-linker] guest fault ...`), a fatal signal as
//! `[andruna-lx] process terminated by signal <n>`, an abort as `F/...`
//! lines. sysd keeps the last lines of each app's standard error and
//! turns them into the summary of an `AppCrashed` event.

use std::collections::VecDeque;

use andruna_ipc::host::{Crash, CrashKind};

/// Lines of standard error kept per process.
pub const TAIL_LINES: usize = 64;

/// Stack lines kept in a summary.
const STACK_LINES: usize = 8;

/// The last lines of a stream.
#[derive(Debug, Default)]
pub struct Tail(VecDeque<String>);

impl Tail {
    pub fn push(&mut self, line: &str) {
        if self.0.len() == TAIL_LINES {
            self.0.pop_front();
        }
        self.0
            .push_back(line.trim_end_matches(['\r', '\n']).to_string());
    }

    pub fn lines(&self) -> Vec<String> {
        self.0.iter().cloned().collect()
    }
}

/// The crash report for a process that ended with `code` after writing
/// `stderr` (its last lines). `None` for a clean exit (`code` 0).
pub fn from_output(code: i64, stderr: &[String]) -> Option<Crash> {
    if code == 0 {
        return None;
    }
    if let Some(i) = stderr
        .iter()
        .rposition(|l| l.starts_with("Exception in thread \""))
    {
        let head = &stderr[i];
        // `Exception in thread "main" java.lang.X: message`
        let what = head
            .splitn(2, "\" ")
            .nth(1)
            .unwrap_or(head.as_str())
            .to_string();
        let thread = head
            .strip_prefix("Exception in thread \"")
            .and_then(|r| r.split('"').next())
            .unwrap_or("?");
        let mut summary = format!("{what}\n(thread \"{thread}\")");
        for l in stderr[i + 1..]
            .iter()
            .take_while(|l| l.starts_with('\t') || l.starts_with("Caused by:"))
            .take(STACK_LINES)
        {
            summary.push('\n');
            summary.push_str(l.trim());
        }
        return Some(Crash {
            kind: CrashKind::UncaughtException,
            summary,
        });
    }
    if let Some(i) = stderr
        .iter()
        .rposition(|l| l.starts_with("[andruna-linker] guest fault"))
    {
        let summary = stderr[i..]
            .iter()
            .take_while(|l| l.starts_with("[andruna-linker]"))
            .take(STACK_LINES + 1)
            .map(|l| l.trim_start_matches("[andruna-linker] ").trim())
            .collect::<Vec<_>>()
            .join("\n");
        return Some(Crash {
            kind: CrashKind::NativeFault,
            summary,
        });
    }
    let aborted = stderr
        .iter()
        .any(|l| l.starts_with("F/") || l.contains("Runtime aborting"));
    let signal = stderr.iter().rev().find_map(|l| {
        l.strip_prefix("[andruna-lx] process terminated by signal ")
            .and_then(|n| n.trim().parse::<i32>().ok())
    });
    let last = |n: usize| {
        let v: Vec<&str> = stderr
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        v[v.len().saturating_sub(n)..].join("\n")
    };
    let kind = match signal {
        Some(6) => CrashKind::Abort,
        Some(_) => CrashKind::NativeFault,
        None if aborted => CrashKind::Abort,
        None => CrashKind::Other,
    };
    let mut summary = last(6);
    if summary.is_empty() {
        summary = describe_code(code);
    }
    Some(Crash { kind, summary })
}

/// Text for an exit code without output: NTSTATUS codes in hex.
pub fn describe_code(code: i64) -> String {
    let c = code as u32;
    if c >= 0xC000_0000 {
        format!("the process ended with status {c:#010x}")
    } else {
        format!("the process ended with code {code}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    #[test]
    fn uncaught_exception() {
        let out = lines(
            "E/dalvikvm(1): IcuRegistration: no time zone files\n\
             Exception in thread \"main\" java.lang.NullPointerException: Attempt to read from field 'int Faults.value'\n\
             \tat Faults.read(Faults.java:10)\n\
             \tat Faults.main(Faults.java:23)\n",
        );
        let c = from_output(1, &out).unwrap();
        assert_eq!(c.kind, CrashKind::UncaughtException);
        assert_eq!(
            c.summary,
            "java.lang.NullPointerException: Attempt to read from field 'int Faults.value'\n\
             (thread \"main\")\nat Faults.read(Faults.java:10)\nat Faults.main(Faults.java:23)"
        );
        assert_eq!(from_output(0, &out), None);
    }

    #[test]
    fn faults_signals_and_others() {
        let c = from_output(
            0xC000_0005,
            &lines(
                "[andruna-linker] guest fault 0xc0000005 at 0x1234 libfoo.so+0x34\n\
                 [andruna-linker]   read from address 0x0\n\
                 [andruna-linker]   #1 0x99 libart.so+0x10\nother\n",
            ),
        )
        .unwrap();
        assert_eq!(c.kind, CrashKind::NativeFault);
        assert!(
            c.summary.starts_with("guest fault 0xc0000005"),
            "{}",
            c.summary
        );
        assert_eq!(c.summary.lines().count(), 3);
        let c = from_output(
            134,
            &lines("F/art: check failed\n[andruna-lx] process terminated by signal 6\n"),
        )
        .unwrap();
        assert_eq!(c.kind, CrashKind::Abort);
        let c = from_output(
            139,
            &lines("[andruna-lx] process terminated by signal 11\n"),
        )
        .unwrap();
        assert_eq!(c.kind, CrashKind::NativeFault);
        let c = from_output(3, &[]).unwrap();
        assert_eq!(c.kind, CrashKind::Other);
        assert_eq!(c.summary, "the process ended with code 3");
        assert!(describe_code(0xC000_0409).contains("0xc0000409"));
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        let mut t = Tail::default();
        for i in 0..100 {
            t.push(&format!("{i}\r\n"));
        }
        let l = t.lines();
        assert_eq!(l.len(), TAIL_LINES);
        assert_eq!(l.last().unwrap(), "99");
    }
}
