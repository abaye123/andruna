//! `poll`/`ppoll`, `select`/`pselect6` and `epoll`.
//!
//! Readiness comes from each object's [`FileOps::poll`]; waits sleep in
//! [`crate::waitq`] and re-check after every state change.
//!
//! # Deviations from Linux
//!
//! - `EPOLLET`: an object is reported when it is ready and new events
//!   happened on it since its last report (its event generation changed,
//!   [`FileOps::poll_generation`]), as Linux reports a new edge for every
//!   wake-up of the object. Pipes, eventfd and timerfd keep their own
//!   generation; other objects (sockets) use the process-wide change count,
//!   so they may be reported again after a change elsewhere, never missed.
//! - `ppoll`, `pselect6` and `epoll_pwait` install the temporary signal
//!   mask for the wait; a signal it unblocks ends the wait with `EINTR` but
//!   is delivered only once the caller's own mask allows it.
//! - `EPOLLEXCLUSIVE` and `EPOLLWAKEUP` are accepted and have no effect
//!   (every waiter is woken; there is no suspend to prevent).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use crate::errno::{Errno, SysResult};
use crate::fd::{FDS, FileOps, OpenFile};
use crate::linux::fcntl::O_RDWR;
use crate::linux::poll::*;
use crate::linux::signal::SigSet;
use crate::linux::stat::StatData;
use crate::util::lock;
use crate::waitq::{self, Deadline};

/// Runs `f` with the calling thread's signal mask set to `mask` (when
/// given), restoring the old mask afterwards.
pub fn with_sigmask<T>(mask: Option<SigSet>, f: impl FnOnce() -> SysResult<T>) -> SysResult<T> {
    use crate::linux::signal::SIG_SETMASK;
    let Some(m) = mask else {
        return f();
    };
    let old = crate::signal::sigprocmask(SIG_SETMASK, Some(m), 8)?;
    let r = f();
    let _ = crate::signal::sigprocmask(SIG_SETMASK, Some(old), 8);
    r
}

/// Readiness of descriptor `fd` for `events` (plus the events always
/// reported); `POLLNVAL` for a closed descriptor.
fn revents(fd: i32, events: u32) -> u32 {
    match FDS.get(fd) {
        Ok(f) => f.ops().poll() & (events | POLLERR | POLLHUP),
        Err(_) => POLLNVAL,
    }
}

/// `ppoll`/`poll` on `fds`; returns how many have events. The timeout is
/// in nanoseconds (`None`: none).
pub fn poll(fds: &mut [PollFd], timeout_ns: Option<i128>) -> SysResult<usize> {
    let deadline = waitq::deadline_in(timeout_ns);
    let r = waitq::wait_until(deadline, || {
        let mut n = 0;
        for p in fds.iter_mut() {
            p.revents = if p.fd < 0 {
                0
            } else {
                revents(p.fd, u32::from(p.events as u16)) as i16
            };
            if p.revents != 0 {
                n += 1;
            }
        }
        if n > 0 || timeout_ns == Some(0) {
            Ok(Some(n))
        } else {
            Ok(None)
        }
    });
    match r {
        Err(Errno::ETIMEDOUT) => Ok(0),
        r => r,
    }
}

/// `fd_set`s of `select`, as bitmaps of `nfds` bits.
pub struct FdSets<'a> {
    pub read: Option<&'a mut [u64]>,
    pub write: Option<&'a mut [u64]>,
    pub except: Option<&'a mut [u64]>,
}

const SELECT_READ: u32 = POLLIN | POLLRDNORM | POLLRDBAND | POLLHUP | POLLERR;
const SELECT_WRITE: u32 = POLLOUT | POLLWRNORM | POLLWRBAND | POLLERR;
const SELECT_EXCEPT: u32 = POLLPRI;

fn bit(set: &Option<&mut [u64]>, i: usize) -> bool {
    set.as_ref()
        .is_some_and(|s| s[i / 64] & (1 << (i % 64)) != 0)
}

/// `pselect6`/`select`: replaces the sets with the ready descriptors and
/// returns how many bits are set.
pub fn select(nfds: usize, sets: FdSets<'_>, timeout_ns: Option<i128>) -> SysResult<usize> {
    let FdSets {
        mut read,
        mut write,
        mut except,
    } = sets;
    // Validate the descriptors first: EBADF for any closed one.
    let mut wanted = Vec::new();
    for i in 0..nfds {
        let (r, w, e) = (bit(&read, i), bit(&write, i), bit(&except, i));
        if r || w || e {
            FDS.get(i as i32)?;
            wanted.push((i, r, w, e));
        }
    }
    let deadline = waitq::deadline_in(timeout_ns);
    let res = waitq::wait_until(deadline, || {
        let mut out = Vec::new();
        for &(i, r, w, e) in &wanted {
            let ev = FDS.get(i as i32).map(|f| f.ops().poll())?;
            let (rr, ww, ee) = (
                r && ev & SELECT_READ != 0,
                w && ev & SELECT_WRITE != 0,
                e && ev & SELECT_EXCEPT != 0,
            );
            if rr || ww || ee {
                out.push((i, rr, ww, ee));
            }
        }
        if !out.is_empty() || timeout_ns == Some(0) {
            Ok(Some(out))
        } else {
            Ok(None)
        }
    });
    let ready = match res {
        Err(Errno::ETIMEDOUT) => Vec::new(),
        r => r?,
    };
    for s in [&mut read, &mut write, &mut except].into_iter().flatten() {
        s.fill(0);
    }
    let mut n = 0;
    for (i, r, w, e) in ready {
        for (on, set) in [(r, &mut read), (w, &mut write), (e, &mut except)] {
            if on && let Some(s) = set.as_mut() {
                s[i / 64] |= 1 << (i % 64);
                n += 1;
            }
        }
    }
    Ok(n)
}

#[derive(Debug)]
struct Interest {
    file: Weak<OpenFile>,
    events: u32,
    data: u64,
    /// `EPOLLONESHOT` after a report: nothing is reported until
    /// `EPOLL_CTL_MOD`.
    disarmed: bool,
    /// `EPOLLET`: the object's event generation at the last report.
    reported: Option<u64>,
}

/// An epoll instance.
#[derive(Debug, Default)]
pub struct Epoll {
    /// Keyed by descriptor number and description, as on Linux.
    interest: Mutex<HashMap<(i32, usize), Interest>>,
}

fn key(fd: i32, f: &Arc<OpenFile>) -> (i32, usize) {
    (fd, Arc::as_ptr(f) as usize)
}

const EPOLL_ALWAYS: u32 = EPOLLERR | EPOLLHUP;
const EPOLL_FLAGS: u32 = EPOLLET | EPOLLONESHOT | EPOLLEXCLUSIVE | EPOLLWAKEUP;

impl Epoll {
    /// `epoll_ctl`.
    pub fn ctl(
        &self,
        me: &Arc<OpenFile>,
        op: i32,
        fd: i32,
        ev: Option<(u32, u64)>,
    ) -> SysResult<()> {
        let f = FDS.get(fd)?;
        if Arc::ptr_eq(&f, me) {
            return Err(Errno::EINVAL);
        }
        // Only objects with readiness can be watched; regular files and
        // directories cannot, as on Linux.
        let any: &dyn core::any::Any = f.ops();
        if any.is::<crate::fs::file::RegularFile>() || any.is::<crate::fs::file::Directory>() {
            return Err(Errno::EPERM);
        }
        let k = key(fd, &f);
        let mut m = lock(&self.interest);
        // Drop entries whose description is gone.
        m.retain(|_, i| i.file.strong_count() > 0);
        match op {
            EPOLL_CTL_ADD => {
                let (events, data) = ev.ok_or(Errno::EFAULT)?;
                if m.contains_key(&k) {
                    return Err(Errno::EEXIST);
                }
                m.insert(
                    k,
                    Interest {
                        file: Arc::downgrade(&f),
                        events,
                        data,
                        disarmed: false,
                        reported: None,
                    },
                );
            }
            EPOLL_CTL_MOD => {
                let (events, data) = ev.ok_or(Errno::EFAULT)?;
                let i = m.get_mut(&k).ok_or(Errno::ENOENT)?;
                if events & EPOLLEXCLUSIVE != 0 || i.events & EPOLLEXCLUSIVE != 0 {
                    return Err(Errno::EINVAL);
                }
                i.events = events;
                i.data = data;
                i.disarmed = false;
                i.reported = None;
            }
            EPOLL_CTL_DEL => {
                m.remove(&k).ok_or(Errno::ENOENT)?;
            }
            _ => return Err(Errno::EINVAL),
        }
        drop(m);
        waitq::notify();
        Ok(())
    }

    /// Collects up to `max` ready events.
    fn collect(&self, max: usize, consume: bool) -> Vec<(u32, u64)> {
        let mut out = Vec::new();
        let mut m = lock(&self.interest);
        m.retain(|_, i| i.file.strong_count() > 0);
        for i in m.values_mut() {
            if out.len() >= max {
                break;
            }
            if i.disarmed {
                continue;
            }
            let Some(f) = i.file.upgrade() else { continue };
            let edge = i.events & EPOLLET != 0;
            let seen = if edge { f.ops().poll_generation() } else { 0 };
            let ev = f.ops().poll() & ((i.events & !EPOLL_FLAGS) | EPOLL_ALWAYS);
            if ev != 0 && !(edge && i.reported == Some(seen)) {
                out.push((ev, i.data));
                if consume {
                    if i.events & EPOLLONESHOT != 0 {
                        i.disarmed = true;
                    }
                    if edge {
                        i.reported = Some(seen);
                    }
                }
            }
        }
        out
    }

    /// `epoll_wait`: the ready events, at most `max`.
    pub fn wait(&self, max: usize, timeout_ns: Option<i128>) -> SysResult<Vec<(u32, u64)>> {
        let deadline: Deadline = waitq::deadline_in(timeout_ns);
        let r = waitq::wait_until(deadline, || {
            let v = self.collect(max, true);
            if !v.is_empty() || timeout_ns == Some(0) {
                Ok(Some(v))
            } else {
                Ok(None)
            }
        });
        match r {
            Err(Errno::ETIMEDOUT) => Ok(Vec::new()),
            r => r,
        }
    }
}

impl FileOps for Epoll {
    fn stat(&self) -> SysResult<StatData> {
        Ok(anon_stat())
    }

    fn kind_name(&self) -> &'static str {
        "eventpoll"
    }

    fn poll(&self) -> u32 {
        if self.collect(1, false).is_empty() {
            0
        } else {
            POLLIN | POLLRDNORM
        }
    }
}

/// `stat` of an anonymous inode (epoll, eventfd, timerfd).
pub fn anon_stat() -> StatData {
    StatData {
        // anon_inodefs.
        dev: 0xe,
        ino: 0x5000,
        mode: 0o600,
        nlink: 1,
        uid: crate::process::uid(),
        gid: crate::process::gid(),
        blksize: 4096,
        ..StatData::default()
    }
}

/// `epoll_create1`.
pub fn epoll_create1(flags: u32) -> SysResult<i32> {
    if flags & !EPOLL_CLOEXEC != 0 {
        return Err(Errno::EINVAL);
    }
    let f = Arc::new(OpenFile::new(Box::<Epoll>::default(), O_RDWR));
    FDS.insert(f, flags & EPOLL_CLOEXEC != 0, 0)
}

/// The epoll instance behind `epfd`.
pub fn epoll_of(epfd: i32) -> SysResult<Arc<OpenFile>> {
    let f = FDS.get(epfd)?;
    if f.downcast::<Epoll>().is_none() {
        return Err(Errno::EINVAL);
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs;
    use crate::util::UserBuf;

    #[test]
    fn poll_select_and_epoll_on_a_pipe() {
        let [r, w] = crate::pipe::pipe2(0).unwrap();
        let mut p = [
            PollFd {
                fd: r,
                events: POLLIN as i16,
                revents: 0,
            },
            PollFd {
                fd: w,
                events: POLLOUT as i16,
                revents: 0,
            },
            PollFd {
                fd: -1,
                events: POLLIN as i16,
                revents: 7,
            },
        ];
        assert_eq!(poll(&mut p, Some(0)), Ok(1));
        assert_eq!(p[0].revents, 0);
        assert_eq!(p[1].revents as u32, POLLOUT);
        assert_eq!(p[2].revents, 0);

        let ep = epoll_create1(EPOLL_CLOEXEC).unwrap();
        let e = epoll_of(ep).unwrap();
        let epoll = e.downcast::<Epoll>().unwrap();
        epoll
            .ctl(&e, EPOLL_CTL_ADD, r, Some((EPOLLIN, 42)))
            .unwrap();
        assert_eq!(
            epoll.ctl(&e, EPOLL_CTL_ADD, r, Some((EPOLLIN, 42))),
            Err(Errno::EEXIST)
        );
        assert_eq!(
            epoll.ctl(&e, EPOLL_CTL_ADD, ep, Some((EPOLLIN, 0))),
            Err(Errno::EINVAL)
        );
        assert_eq!(epoll.wait(8, Some(1_000_000)), Ok(Vec::new()));

        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            fs::write(w, UserBuf::from_ref(b"x")).unwrap();
        });
        assert_eq!(epoll.wait(8, None), Ok(vec![(EPOLLIN, 42)]));
        t.join().unwrap();
        // Level-triggered: still reported.
        assert_eq!(epoll.wait(8, Some(0)), Ok(vec![(EPOLLIN, 42)]));
        assert_eq!(FDS.get(ep).unwrap().ops().poll(), POLLIN | POLLRDNORM);

        let mut rs = [0u64; 16];
        rs[r as usize / 64] |= 1 << (r as usize % 64);
        let n = select(
            r as usize + 1,
            FdSets {
                read: Some(&mut rs),
                write: None,
                except: None,
            },
            Some(0),
        );
        assert_eq!(n, Ok(1));

        // One-shot: reported once until re-armed.
        epoll
            .ctl(&e, EPOLL_CTL_MOD, r, Some((EPOLLIN | EPOLLONESHOT, 7)))
            .unwrap();
        assert_eq!(epoll.wait(8, Some(0)), Ok(vec![(EPOLLIN, 7)]));
        assert_eq!(epoll.wait(8, Some(0)), Ok(Vec::new()));
        // Edge-triggered: reported once per new event, again after more
        // data arrives, never while nothing new happened.
        epoll
            .ctl(&e, EPOLL_CTL_MOD, r, Some((EPOLLIN | EPOLLET, 8)))
            .unwrap();
        assert_eq!(epoll.wait(8, Some(0)), Ok(vec![(EPOLLIN, 8)]));
        assert_eq!(epoll.wait(8, Some(0)), Ok(Vec::new()));
        fs::write(w, UserBuf::from_ref(b"y")).unwrap();
        assert_eq!(epoll.wait(8, Some(0)), Ok(vec![(EPOLLIN, 8)]));
        assert_eq!(epoll.wait(8, Some(0)), Ok(Vec::new()));
        // Drained, then written again while nobody waited: a new edge.
        let mut two = [0u8; 2];
        assert_eq!(fs::read(r, UserBuf::from_mut(&mut two)), Ok(2));
        fs::write(w, UserBuf::from_ref(b"z")).unwrap();
        assert_eq!(epoll.wait(8, Some(0)), Ok(vec![(EPOLLIN, 8)]));
        assert_eq!(fs::read(r, UserBuf::from_mut(&mut two)), Ok(1));
        epoll.ctl(&e, EPOLL_CTL_DEL, r, None).unwrap();
        assert_eq!(epoll.ctl(&e, EPOLL_CTL_DEL, r, None), Err(Errno::ENOENT));

        for fd in [r, w, ep] {
            fs::close(fd).unwrap();
        }
    }
}
