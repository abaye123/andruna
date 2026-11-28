//! Pipes (`pipe2`): a byte channel inside this process.
//!
//! Semantics follow Linux: 64 KiB capacity, writes of at most `PIPE_BUF`
//! bytes are atomic, reads return what is there (0 at end of file once
//! every write end is closed), a write with no read end left raises
//! `SIGPIPE` and fails with `EPIPE`, `O_NONBLOCK` gives `EAGAIN`, and a
//! blocked call returns `EINTR` when a signal arrives.
//!
//! # Deviations from Linux
//!
//! - A pipe exists in this process only; there is no other process to
//!   share it with (no `fork`).
//! - `O_DIRECT` (packet mode) is refused with `EINVAL`.
//! - Opening `/proc/self/fd/N` of a pipe again is not supported.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::errno::{Errno, SysResult};
use crate::fd::{FDS, FileOps, OpenFile};
use crate::linux::fcntl::{O_CLOEXEC, O_DIRECT, O_NONBLOCK, O_RDONLY, O_WRONLY};
use crate::linux::poll::*;
use crate::linux::stat::{S_IFIFO, StatData};
use crate::util::{UserBuf, lock};
use crate::waitq;

/// `PIPE_BUF`: writes up to this size are atomic.
pub const PIPE_BUF: usize = 4096;
/// The capacity of a pipe (Linux's default, 16 pages).
pub const PIPE_CAPACITY: usize = 65536;

/// Inode numbers of pipes and sockets (`pipefs`, `sockfs`).
pub(crate) fn next_ino() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0x10000);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Default)]
struct State {
    buf: VecDeque<u8>,
    readers: usize,
    writers: usize,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    ino: u64,
    /// Event generation for edge-triggered epoll.
    events: AtomicU64,
}

impl Shared {
    /// Reports a state change of the pipe.
    fn changed(&self) {
        self.events.fetch_add(1, Ordering::SeqCst);
        waitq::notify();
    }
}

/// One end of a pipe.
#[derive(Debug)]
pub struct PipeEnd {
    shared: Arc<Shared>,
    write: bool,
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        {
            let mut s = lock(&self.shared.state);
            if self.write {
                s.writers -= 1;
            } else {
                s.readers -= 1;
            }
        }
        self.shared.changed();
    }
}

/// Copies `src` to the caller's buffer.
pub(crate) fn copy_to_user(buf: UserBuf, at: usize, src: &[u8]) -> SysResult<()> {
    if src.is_empty() {
        return Ok(());
    }
    if buf.ptr().is_null() {
        return Err(Errno::EFAULT);
    }
    // SAFETY: `at + src.len() <= buf.len()` (callers), inside the caller's
    // buffer (UserBuf contract).
    unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), buf.ptr().add(at), src.len()) };
    Ok(())
}

/// Reads `len` bytes from the caller's buffer at `at`.
pub(crate) fn copy_from_user(buf: UserBuf, at: usize, len: usize) -> SysResult<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if buf.ptr().is_null() {
        return Err(Errno::EFAULT);
    }
    // SAFETY: `at + len <= buf.len()` (callers), inside the caller's
    // buffer (UserBuf contract).
    Ok(unsafe { core::slice::from_raw_parts(buf.ptr().add(at), len) }.to_vec())
}

fn nonblock(file: &OpenFile) -> bool {
    file.status_flags() & O_NONBLOCK != 0
}

/// Raises `SIGPIPE` for the calling thread, as a write to a broken pipe or
/// socket does.
pub(crate) fn raise_sigpipe() {
    let _ = crate::signal::tgkill(
        None,
        crate::process::gettid(),
        crate::linux::signal::SIGPIPE,
        None,
    );
}

impl FileOps for PipeEnd {
    fn read(&self, file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let n = waitq::wait_until(None, || {
            let mut s = lock(&self.shared.state);
            if !s.buf.is_empty() {
                let n = buf.len().min(s.buf.len());
                let (a, b) = s.buf.as_slices();
                let first = n.min(a.len());
                copy_to_user(buf, 0, &a[..first])?;
                copy_to_user(buf, first, &b[..n - first])?;
                s.buf.drain(..n);
                return Ok(Some(n));
            }
            if s.writers == 0 {
                return Ok(Some(0));
            }
            if nonblock(file) {
                return Err(Errno::EAGAIN);
            }
            Ok(None)
        })?;
        if n > 0 {
            self.shared.changed();
        }
        Ok(n)
    }

    fn write(&self, file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        let len = buf.len();
        if len == 0 {
            return Ok(0);
        }
        let mut done = 0usize;
        loop {
            let r = waitq::wait_until(None, || {
                let mut s = lock(&self.shared.state);
                if s.readers == 0 {
                    return Err(Errno::EPIPE);
                }
                let free = PIPE_CAPACITY - s.buf.len();
                let left = len - done;
                // Atomic writes wait for room for all of it.
                let need = if left <= PIPE_BUF { left } else { 1 };
                if free >= need {
                    let n = left.min(free);
                    let data = copy_from_user(buf, done, n)?;
                    s.buf.extend(data);
                    return Ok(Some(n));
                }
                if nonblock(file) {
                    return Err(Errno::EAGAIN);
                }
                Ok(None)
            });
            match r {
                Ok(n) => {
                    done += n;
                    self.shared.changed();
                    if done == len {
                        return Ok(done);
                    }
                }
                Err(Errno::EPIPE) => {
                    raise_sigpipe();
                    return if done > 0 {
                        Ok(done)
                    } else {
                        Err(Errno::EPIPE)
                    };
                }
                Err(e) if done > 0 && (e == Errno::EAGAIN || e == Errno::EINTR) => {
                    return Ok(done);
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn stat(&self) -> SysResult<StatData> {
        Ok(StatData {
            // pipefs.
            dev: 0xc,
            ino: self.shared.ino,
            mode: S_IFIFO | 0o600,
            nlink: 1,
            uid: crate::process::uid(),
            gid: crate::process::gid(),
            blksize: 4096,
            ..StatData::default()
        })
    }

    fn path(&self) -> Option<Vec<u8>> {
        Some(format!("pipe:[{}]", self.shared.ino).into_bytes())
    }

    fn poll_generation(&self) -> u64 {
        self.shared.events.load(Ordering::SeqCst)
    }

    fn poll(&self) -> u32 {
        let s = lock(&self.shared.state);
        if self.write {
            let mut ev = 0;
            if PIPE_CAPACITY - s.buf.len() >= PIPE_BUF {
                ev |= POLLOUT | POLLWRNORM;
            }
            if s.readers == 0 {
                ev |= POLLERR;
            }
            ev
        } else {
            let mut ev = 0;
            if !s.buf.is_empty() {
                ev |= POLLIN | POLLRDNORM;
            }
            if s.writers == 0 {
                ev |= POLLHUP;
            }
            ev
        }
    }
}

/// `pipe2`: returns the read and write descriptors.
pub fn pipe2(flags: u32) -> SysResult<[i32; 2]> {
    if flags & O_DIRECT != 0 || flags & !(O_CLOEXEC | O_NONBLOCK | O_DIRECT) != 0 {
        return Err(Errno::EINVAL);
    }
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            buf: VecDeque::new(),
            readers: 1,
            writers: 1,
        }),
        ino: next_ino(),
        events: AtomicU64::new(0),
    });
    let status = flags & O_NONBLOCK;
    let cloexec = flags & O_CLOEXEC != 0;
    let r = Arc::new(OpenFile::new(
        Box::new(PipeEnd {
            shared: shared.clone(),
            write: false,
        }),
        O_RDONLY | status,
    ));
    let w = Arc::new(OpenFile::new(
        Box::new(PipeEnd {
            shared,
            write: true,
        }),
        O_WRONLY | status,
    ));
    let rfd = FDS.insert(r, cloexec, 0)?;
    match FDS.insert(w, cloexec, 0) {
        Ok(wfd) => Ok([rfd, wfd]),
        Err(e) => {
            let _ = FDS.close(rfd);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs;

    #[test]
    fn data_eof_and_epipe() {
        let [r, w] = pipe2(O_CLOEXEC).unwrap();
        assert_eq!(fs::write(w, UserBuf::from_ref(b"hello")), Ok(5));
        let mut b = [0u8; 16];
        assert_eq!(fs::read(r, UserBuf::from_mut(&mut b)), Ok(5));
        assert_eq!(&b[..5], b"hello");
        assert_eq!(FDS.get(r).unwrap().ops().poll(), 0);
        assert_eq!(FDS.get(w).unwrap().ops().poll(), POLLOUT | POLLWRNORM);
        // A blocked reader gets the data written later.
        let t = std::thread::spawn(move || {
            let mut b = [0u8; 4];
            let n = fs::read(r, UserBuf::from_mut(&mut b));
            (n, b)
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(fs::write(w, UserBuf::from_ref(b"abcd")), Ok(4));
        let (n, got) = t.join().unwrap();
        assert_eq!(n, Ok(4));
        assert_eq!(&got, b"abcd");
        fs::close(w).unwrap();
        assert_eq!(fs::read(r, UserBuf::from_mut(&mut b)), Ok(0));
        assert_eq!(FDS.get(r).unwrap().ops().poll(), POLLHUP);
        fs::close(r).unwrap();
    }

    #[test]
    fn nonblocking_and_full() {
        let [r, w] = pipe2(O_NONBLOCK).unwrap();
        let mut b = [0u8; 8];
        assert_eq!(fs::read(r, UserBuf::from_mut(&mut b)), Err(Errno::EAGAIN));
        let big = vec![1u8; PIPE_CAPACITY + 100];
        assert_eq!(fs::write(w, UserBuf::from_ref(&big)), Ok(PIPE_CAPACITY));
        assert_eq!(
            fs::write(w, UserBuf::from_ref(&big[..1])),
            Err(Errno::EAGAIN)
        );
        let st = fs::fstat(r).unwrap();
        assert_eq!(st.mode & crate::linux::stat::S_IFMT, S_IFIFO);
        fs::close(r).unwrap();
        fs::close(w).unwrap();
        assert_eq!(pipe2(O_DIRECT), Err(Errno::EINVAL));
        assert_eq!(pipe2(0x1), Err(Errno::EINVAL));
    }
}
