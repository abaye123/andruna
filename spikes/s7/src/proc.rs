//! A child process with piped standard streams and timestamped output.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, ResumeThread, TerminateProcess, WaitForSingleObject,
};

use crate::win::{self, Container, Owned};

pub struct Proc {
    pub child: win::Child,
    pub started: Instant,
    stdin: Option<File>,
    lines: Receiver<(Instant, String)>,
    pub stdout: Arc<Mutex<String>>,
    pub stderr: Arc<Mutex<String>>,
}

impl Proc {
    pub fn spawn(
        cmdline: &str,
        container: Option<&Container>,
        suspended: bool,
        inherit: &[HANDLE],
    ) -> io::Result<Proc> {
        let (in_r, in_w) = win::pipe(false)?;
        let (out_r, out_w) = win::pipe(true)?;
        let (err_r, err_w) = win::pipe(true)?;
        let started = Instant::now();
        let child = win::spawn(
            cmdline,
            container,
            suspended,
            [in_r.0, out_w.0, err_w.0],
            inherit,
        )?;
        drop((in_r, out_w, err_w));
        let file = |o: Owned| {
            let h = o.0;
            std::mem::forget(o);
            // SAFETY: ownership moves from `o` to the File.
            unsafe { File::from_raw_handle(h.0) }
        };
        let (tx, lines) = channel();
        let stdout = Arc::new(Mutex::new(String::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        let out = file(out_r);
        let so = stdout.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines() {
                let Ok(l) = l else { break };
                so.lock().unwrap().push_str(&format!("{l}\n"));
                let _ = tx.send((Instant::now(), l));
            }
        });
        let mut err = file(err_r);
        let se = stderr.clone();
        std::thread::spawn(move || {
            let mut b = [0u8; 4096];
            while let Ok(n) = err.read(&mut b) {
                if n == 0 {
                    break;
                }
                se.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&b[..n]));
            }
        });
        Ok(Proc {
            child,
            started,
            stdin: Some(file(in_w)),
            lines,
            stdout,
            stderr,
        })
    }

    pub fn resume(&self) {
        // SAFETY: a thread handle we own.
        unsafe {
            ResumeThread(self.child.thread.0);
        }
    }

    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let f = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("stdin closed"))?;
        f.write_all(data)?;
        f.flush()
    }

    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// The first output line containing `marker`, and when it arrived.
    pub fn wait_line(&self, marker: &str, timeout: Duration) -> Option<(Instant, String)> {
        let end = Instant::now() + timeout;
        loop {
            let left = end.checked_duration_since(Instant::now())?;
            match self.lines.recv_timeout(left) {
                Ok((t, l)) if l.contains(marker) => return Some((t, l)),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }

    /// Exit code, or None on timeout (the process is then killed).
    pub fn wait(&mut self, timeout: Duration) -> Option<u32> {
        self.close_stdin();
        // SAFETY: a process handle we own.
        unsafe {
            if WaitForSingleObject(self.child.process.0, timeout.as_millis() as u32)
                != WAIT_OBJECT_0
            {
                let _ = TerminateProcess(self.child.process.0, 99);
                return None;
            }
            let mut code = 0;
            let _ = GetExitCodeProcess(self.child.process.0, &mut code);
            // Let the reader threads drain the pipes.
            std::thread::sleep(Duration::from_millis(20));
            Some(code)
        }
    }

    /// (working set, private commit) in bytes.
    pub fn memory(&self) -> (usize, usize) {
        let mut c = PROCESS_MEMORY_COUNTERS_EX2 {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
            ..Default::default()
        };
        // SAFETY: the buffer is a PROCESS_MEMORY_COUNTERS_EX2 with cb set.
        unsafe {
            let _ = GetProcessMemoryInfo(
                self.child.process.0,
                &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
                c.cb,
            );
        }
        (c.WorkingSetSize, c.PrivateUsage)
    }

    pub fn out(&self) -> String {
        self.stdout.lock().unwrap().clone()
    }

    pub fn err(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }
}

pub fn ms(a: Instant, b: Instant) -> f64 {
    b.duration_since(a).as_secs_f64() * 1e3
}

pub fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// "median [min-max]" of a sample.
pub fn stats(v: &[f64]) -> String {
    let mut s = v.to_vec();
    let m = median(&mut s);
    format!(
        "{m:.1} ms [{:.1}-{:.1}]",
        s.first().unwrap_or(&f64::NAN),
        s.last().unwrap_or(&f64::NAN)
    )
}

pub fn quote(s: &str) -> String {
    if s.contains(' ') || s.is_empty() {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}
