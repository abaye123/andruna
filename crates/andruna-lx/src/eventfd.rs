//! `eventfd2`: a 64-bit counter with blocking reads and writes.

use std::sync::{Arc, Mutex};

use crate::errno::{Errno, SysResult};
use crate::fd::{FDS, FileOps, OpenFile};
use crate::linux::fcntl::{O_NONBLOCK, O_RDWR};
use crate::linux::poll::*;
use crate::linux::stat::StatData;
use crate::pipe::{copy_from_user, copy_to_user};
use crate::util::{UserBuf, lock};
use crate::waitq;

const MAX: u64 = u64::MAX - 1;

/// An eventfd object.
#[derive(Debug)]
pub struct EventFd {
    count: Mutex<u64>,
    semaphore: bool,
    /// Event generation for edge-triggered epoll.
    events: core::sync::atomic::AtomicU64,
}

impl EventFd {
    fn changed(&self) {
        self.events
            .fetch_add(1, core::sync::atomic::Ordering::SeqCst);
        waitq::notify();
    }
}

impl FileOps for EventFd {
    fn read(&self, file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        if buf.len() < 8 {
            return Err(Errno::EINVAL);
        }
        let v = waitq::wait_until(None, || {
            let mut c = lock(&self.count);
            if *c > 0 {
                let v = if self.semaphore { 1 } else { *c };
                *c -= v;
                return Ok(Some(v));
            }
            if file.status_flags() & O_NONBLOCK != 0 {
                return Err(Errno::EAGAIN);
            }
            Ok(None)
        })?;
        self.changed();
        copy_to_user(buf, 0, &v.to_ne_bytes())?;
        Ok(8)
    }

    fn write(&self, file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        if buf.len() < 8 {
            return Err(Errno::EINVAL);
        }
        let b = copy_from_user(buf, 0, 8)?;
        let v = u64::from_ne_bytes(b.try_into().map_err(|_| Errno::EINVAL)?);
        if v == u64::MAX {
            return Err(Errno::EINVAL);
        }
        waitq::wait_until(None, || {
            let mut c = lock(&self.count);
            if MAX - *c >= v {
                *c += v;
                return Ok(Some(()));
            }
            if file.status_flags() & O_NONBLOCK != 0 {
                return Err(Errno::EAGAIN);
            }
            Ok(None)
        })?;
        self.changed();
        Ok(8)
    }

    fn stat(&self) -> SysResult<StatData> {
        Ok(crate::poll::anon_stat())
    }

    fn kind_name(&self) -> &'static str {
        "eventfd"
    }

    fn poll_generation(&self) -> u64 {
        self.events.load(core::sync::atomic::Ordering::SeqCst)
    }

    fn poll(&self) -> u32 {
        let c = *lock(&self.count);
        let mut ev = 0;
        if c > 0 {
            ev |= POLLIN | POLLRDNORM;
        }
        if c < MAX {
            ev |= POLLOUT | POLLWRNORM;
        }
        ev
    }
}

/// `eventfd2`.
pub fn eventfd2(initval: u32, flags: u32) -> SysResult<i32> {
    if flags & !(EFD_CLOEXEC | EFD_NONBLOCK | EFD_SEMAPHORE) != 0 {
        return Err(Errno::EINVAL);
    }
    let f = Arc::new(OpenFile::new(
        Box::new(EventFd {
            count: Mutex::new(u64::from(initval)),
            semaphore: flags & EFD_SEMAPHORE != 0,
            events: core::sync::atomic::AtomicU64::new(0),
        }),
        O_RDWR | (flags & EFD_NONBLOCK),
    ));
    FDS.insert(f, flags & EFD_CLOEXEC != 0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs;

    fn rd(fd: i32) -> SysResult<u64> {
        let mut b = [0u8; 8];
        fs::read(fd, UserBuf::from_mut(&mut b))?;
        Ok(u64::from_ne_bytes(b))
    }

    fn wr(fd: i32, v: u64) -> SysResult<usize> {
        fs::write(fd, UserBuf::from_ref(&v.to_ne_bytes()))
    }

    #[test]
    fn counter_and_semaphore() {
        let fd = eventfd2(3, EFD_NONBLOCK).unwrap();
        assert_eq!(wr(fd, 2), Ok(8));
        assert_eq!(rd(fd), Ok(5));
        assert_eq!(rd(fd), Err(Errno::EAGAIN));
        assert_eq!(wr(fd, u64::MAX), Err(Errno::EINVAL));
        assert_eq!(wr(fd, MAX), Ok(8));
        assert_eq!(wr(fd, 1), Err(Errno::EAGAIN));
        fs::close(fd).unwrap();

        let fd = eventfd2(2, EFD_SEMAPHORE).unwrap();
        assert_eq!(rd(fd), Ok(1));
        assert_eq!(rd(fd), Ok(1));
        let t = std::thread::spawn(move || rd(fd));
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(wr(fd, 1), Ok(8));
        assert_eq!(t.join().unwrap(), Ok(1));
        assert_eq!(eventfd2(0, 0x8000_0000), Err(Errno::EINVAL));
        fs::close(fd).unwrap();
    }
}
