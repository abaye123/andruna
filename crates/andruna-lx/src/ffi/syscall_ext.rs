//! The system calls added in the second conformance round: pipes,
//! `poll`/`select`/`epoll`, eventfd, timers, Unix domain sockets, resource
//! limits, wall-clock time and file modes. [`super::syscall::dispatch`]
//! forwards every call it does not handle itself here.
//!
//! # Safety
//!
//! As for [`super::syscall`]: pointer arguments satisfy the contract of
//! the system call named by the number.

use core::ffi::c_char;
use std::sync::Arc;

use super::{read_arg, write_arg};
use crate::errno::{Errno, SysResult};
use crate::fd::{FDS, OpenFile};
use crate::linux::fcntl::{AT_FDCWD, AT_SYMLINK_NOFOLLOW};
use crate::linux::poll::*;
use crate::linux::signal::SigSet;
use crate::linux::syscall_nr::Sysno;
use crate::linux::time::{CLOCK_REALTIME, Timespec};
use crate::rlimit::Rlimit;
use crate::socket::{self as sock, Addr, Name, SendAnc, Socket};
use crate::timer::{self, ItimerSpec, ItimerVal, SigEvent};
use crate::util::UserBuf;
use crate::{eventfd, fs, pipe, poll};

/// Dispatches one of the calls of this module.
///
/// # Safety
///
/// See the module documentation.
pub(crate) unsafe fn dispatch(sysno: Sysno, a: [i64; 6]) -> SysResult<i64> {
    let int = |i: usize| a[i] as i32;
    let uint = |i: usize| a[i] as u32;
    let ptr = |i: usize| a[i] as usize;
    // SAFETY: path arguments are NUL-terminated strings (contract).
    let path = |p: usize| unsafe { super::path_arg(p as *const c_char) };
    let zero = |r: SysResult<()>| r.map(|()| 0);
    // SAFETY: every call below forwards pointer arguments under the
    // contract of this function.
    unsafe {
        match sysno {
            Sysno::Pipe2 => sys_pipe(ptr(0), uint(1)),
            Sysno::Pipe => sys_pipe(ptr(0), 0),
            Sysno::Ppoll => {
                let t = opt_timespec(ptr(2))?;
                let mask = opt_sigmask(ptr(3), ptr(4))?;
                poll::with_sigmask(mask, || sys_poll(ptr(0), ptr(1), t))
            }
            Sysno::Poll => sys_poll(ptr(0), ptr(1), ms_timeout(int(2))),
            Sysno::Pselect6 => {
                let t = opt_timespec(ptr(4))?;
                // The sixth argument points to { const sigset_t*, size_t }.
                let mask = if ptr(5) == 0 {
                    None
                } else {
                    let [set, size] = read_arg(ptr(5) as *const [u64; 2])?;
                    opt_sigmask(set as usize, size as usize)?
                };
                poll::with_sigmask(mask, || sys_select(int(0), [ptr(1), ptr(2), ptr(3)], t))
            }
            Sysno::Select => {
                let t = if ptr(4) == 0 {
                    None
                } else {
                    let [s, us] = read_arg(ptr(4) as *const [i64; 2])?;
                    if s < 0 || !(0..1_000_000).contains(&us) {
                        return Err(Errno::EINVAL);
                    }
                    Some((i128::from(s) * 1_000_000 + i128::from(us)) * 1000)
                };
                sys_select(int(0), [ptr(1), ptr(2), ptr(3)], t)
            }
            Sysno::EpollCreate1 => poll::epoll_create1(uint(0)).map(i64::from),
            Sysno::EpollCreate => {
                if int(0) <= 0 {
                    return Err(Errno::EINVAL);
                }
                poll::epoll_create1(0).map(i64::from)
            }
            Sysno::EpollCtl => {
                let e = poll::epoll_of(int(0))?;
                let ev = if int(1) == EPOLL_CTL_DEL {
                    None
                } else {
                    let v = read_arg(ptr(3) as *const EpollEvent)?;
                    Some((v.events, v.data))
                };
                let ep = e.downcast::<poll::Epoll>().ok_or(Errno::EINVAL)?;
                zero(ep.ctl(&e, int(1), int(2), ev))
            }
            Sysno::EpollWait => sys_epoll_wait(int(0), ptr(1), int(2), ms_timeout(int(3)), None),
            Sysno::EpollPwait => {
                let mask = opt_sigmask(ptr(4), ptr(5))?;
                sys_epoll_wait(int(0), ptr(1), int(2), ms_timeout(int(3)), mask)
            }
            Sysno::EpollPwait2 => {
                let t = opt_timespec(ptr(3))?;
                let mask = opt_sigmask(ptr(4), ptr(5))?;
                sys_epoll_wait(int(0), ptr(1), int(2), t, mask)
            }
            Sysno::Eventfd2 => eventfd::eventfd2(uint(0), uint(1)).map(i64::from),
            Sysno::Eventfd => eventfd::eventfd2(uint(0), 0).map(i64::from),
            Sysno::TimerfdCreate => timer::timerfd_create(int(0), uint(1)).map(i64::from),
            Sysno::TimerfdSettime => {
                let new = read_arg(ptr(2) as *const ItimerSpec)?;
                let old = timer::timerfd_settime(int(0), uint(1), &new)?;
                write_opt(ptr(3) as *mut ItimerSpec, old)
            }
            Sysno::TimerfdGettime => {
                let cur = timer::timerfd_gettime(int(0))?;
                write_arg(ptr(1) as *mut ItimerSpec, cur).map(|()| 0)
            }
            Sysno::TimerCreate => {
                let ev = if ptr(1) == 0 {
                    None
                } else {
                    Some(read_arg(ptr(1) as *const SigEvent)?)
                };
                if ptr(2) == 0 {
                    return Err(Errno::EFAULT);
                }
                let id = timer::timer_create(int(0), ev)?;
                if let Err(e) = write_arg(ptr(2) as *mut i32, id) {
                    let _ = timer::timer_delete(id);
                    return Err(e);
                }
                Ok(0)
            }
            Sysno::TimerSettime => {
                let new = read_arg(ptr(2) as *const ItimerSpec)?;
                let old = timer::timer_settime(int(0), int(1), &new)?;
                write_opt(ptr(3) as *mut ItimerSpec, old)
            }
            Sysno::TimerGettime => {
                let cur = timer::timer_gettime(int(0))?;
                write_arg(ptr(1) as *mut ItimerSpec, cur).map(|()| 0)
            }
            Sysno::TimerGetoverrun => timer::timer_getoverrun(int(0)).map(i64::from),
            Sysno::TimerDelete => zero(timer::timer_delete(int(0))),
            Sysno::Getitimer => {
                let v = timer::getitimer(int(0))?;
                write_arg(ptr(1) as *mut ItimerVal, v).map(|()| 0)
            }
            Sysno::Setitimer => {
                let new = if ptr(1) == 0 {
                    ItimerVal::default()
                } else {
                    read_arg(ptr(1) as *const ItimerVal)?
                };
                let old = timer::setitimer(int(0), &new)?;
                write_opt(ptr(2) as *mut ItimerVal, old)
            }
            Sysno::Alarm => {
                let new = ItimerVal {
                    interval: [0, 0],
                    value: [i64::from(uint(0)), 0],
                };
                let old = timer::setitimer(timer::ITIMER_REAL, &new)?;
                // Round to the nearest second, at least 1 when armed.
                let mut s = old.value[0] + i64::from(old.value[1] >= 500_000);
                if s == 0 && old.value[1] > 0 {
                    s = 1;
                }
                Ok(s)
            }
            Sysno::ClockSettime => {
                let ts = read_arg(ptr(1) as *const Timespec)?;
                crate::time::clock_getres(int(0))?;
                if int(0) != CLOCK_REALTIME || !ts.has_valid_nsec() || ts.tv_sec < 0 {
                    return Err(Errno::EINVAL);
                }
                // Setting the Windows clock is not this process's to do,
                // as for a Linux process without CAP_SYS_TIME.
                Err(Errno::EPERM)
            }
            Sysno::Settimeofday => Err(Errno::EPERM),
            Sysno::Gettimeofday => {
                let ns = crate::time::clock_gettime(CLOCK_REALTIME)?.as_nanos();
                if ptr(0) != 0 {
                    let us = ns / 1000;
                    let tv = [(us / 1_000_000) as i64, (us % 1_000_000) as i64];
                    write_arg(ptr(0) as *mut [i64; 2], tv)?;
                }
                if ptr(1) != 0 {
                    // struct timezone: minutes west and DST type, which
                    // Linux keeps at 0 unless set.
                    write_arg(ptr(1) as *mut [i32; 2], [0, 0])?;
                }
                Ok(0)
            }
            Sysno::Time => {
                let t = crate::time::clock_gettime(CLOCK_REALTIME)?.tv_sec;
                if ptr(0) != 0 {
                    write_arg(ptr(0) as *mut i64, t)?;
                }
                Ok(t)
            }
            Sysno::Prlimit64 => {
                let new = if ptr(2) == 0 {
                    None
                } else {
                    Some(read_arg(ptr(2) as *const Rlimit)?)
                };
                let old = crate::rlimit::prlimit(int(0), int(1), new)?;
                write_opt(ptr(3) as *mut Rlimit, old)
            }
            Sysno::Getrlimit => {
                let old = crate::rlimit::prlimit(0, int(0), None)?;
                write_arg(ptr(1) as *mut Rlimit, old).map(|()| 0)
            }
            Sysno::Setrlimit => {
                let new = read_arg(ptr(1) as *const Rlimit)?;
                crate::rlimit::prlimit(0, int(0), Some(new)).map(|_| 0)
            }
            Sysno::Umask => Ok(i64::from(fs::umask(uint(0)))),
            Sysno::Fchmod => zero(fs::fchmod(int(0), uint(1))),
            Sysno::Fchmodat => zero(fs::fchmodat(int(0), path(ptr(1))?, uint(2), 0)),
            Sysno::Fchmodat2 => zero(fs::fchmodat(int(0), path(ptr(1))?, uint(2), uint(3))),
            Sysno::Chmod => zero(fs::fchmodat(AT_FDCWD, path(ptr(0))?, uint(1), 0)),
            Sysno::Fchown => zero(fs::fchown(int(0), uint(1), uint(2))),
            Sysno::Fchownat => zero(fs::fchownat(
                int(0),
                path(ptr(1))?,
                uint(2),
                uint(3),
                uint(4),
            )),
            Sysno::Chown => zero(fs::fchownat(AT_FDCWD, path(ptr(0))?, uint(1), uint(2), 0)),
            Sysno::Lchown => zero(fs::fchownat(
                AT_FDCWD,
                path(ptr(0))?,
                uint(1),
                uint(2),
                AT_SYMLINK_NOFOLLOW,
            )),
            Sysno::Socket => sock::socket(int(0), int(1), int(2)).map(i64::from),
            Sysno::Socketpair => {
                if ptr(3) == 0 {
                    return Err(Errno::EFAULT);
                }
                let fds = sock::socketpair(int(0), int(1), int(2))?;
                write_fd_pair(ptr(3), fds)
            }
            Sysno::Bind => {
                let addr = read_addr(ptr(1), uint(2))?;
                with_socket(int(0), |_, s| s.bind(addr)).map(|()| 0)
            }
            Sysno::Listen => with_socket(int(0), |_, s| s.listen(int(1))).map(|()| 0),
            Sysno::Connect => {
                let addr = read_addr(ptr(1), uint(2))?;
                with_socket(int(0), |f, s| s.connect(f, addr)).map(|()| 0)
            }
            Sysno::Accept => sys_accept(int(0), ptr(1), ptr(2), 0),
            Sysno::Accept4 => sys_accept(int(0), ptr(1), ptr(2), int(3)),
            Sysno::Getsockname => {
                let n = with_socket(int(0), |_, s| Ok(s.name()))?;
                write_addr(ptr(1), ptr(2), &Name::sockaddr(n.as_ref())).map(|()| 0)
            }
            Sysno::Getpeername => {
                let n = with_socket(int(0), |_, s| s.peer_name())?;
                write_addr(ptr(1), ptr(2), &Name::sockaddr(n.as_ref())).map(|()| 0)
            }
            Sysno::Sendto => {
                let data = user_bytes(ptr(1), ptr(2))?;
                let to = if ptr(4) == 0 {
                    None
                } else {
                    Some(read_addr(ptr(4), uint(5))?)
                };
                with_socket(int(0), |f, s| {
                    s.send(f, &data, SendAnc::default(), to, int(3))
                })
                .map(|n| n as i64)
            }
            Sysno::Recvfrom => {
                let r = with_socket(int(0), |f, s| s.recv(f, ptr(2), int(3)))?;
                pipe::copy_to_user(UserBuf::new(ptr(1) as *mut u8, ptr(2)), 0, &r.data)?;
                if ptr(4) != 0 {
                    write_addr(ptr(4), ptr(5), &Name::sockaddr(r.from.as_ref()))?;
                }
                Ok(r.len as i64)
            }
            Sysno::Getppid => Ok(i64::from(crate::process::getppid())),
            Sysno::Readv => sys_iov(int(0), ptr(1), int(2), false),
            Sysno::Writev => sys_iov(int(0), ptr(1), int(2), true),
            Sysno::Sendmsg => sys_sendmsg(int(0), ptr(1), int(2)),
            Sysno::Recvmsg => sys_recvmsg(int(0), ptr(1), int(2)),
            Sysno::Shutdown => with_socket(int(0), |_, s| s.shutdown(int(1))).map(|()| 0),
            Sysno::Setsockopt => {
                let val = user_bytes(ptr(3), ptr(4).min(256))?;
                with_socket(int(0), |_, s| s.setsockopt(int(1), int(2), &val)).map(|()| 0)
            }
            Sysno::Getsockopt => {
                let v = with_socket(int(0), |_, s| s.getsockopt(int(1), int(2)))?;
                let len = read_arg(ptr(4) as *const i32)?;
                if len < 0 {
                    return Err(Errno::EINVAL);
                }
                let n = v.len().min(len as usize);
                pipe::copy_to_user(UserBuf::new(ptr(3) as *mut u8, n), 0, &v[..n])?;
                write_arg(ptr(4) as *mut i32, n as i32).map(|()| 0)
            }
            // Calls `super::syscall::dispatch` handles itself.
            _ => Err(Errno::ENOSYS),
        }
    }
}

/// Writes an optional result structure.
///
/// # Safety
///
/// `p` is NULL or valid for writing a `T`.
unsafe fn write_opt<T>(p: *mut T, v: T) -> SysResult<i64> {
    if !p.is_null() {
        // SAFETY: contract.
        unsafe { write_arg(p, v) }?;
    }
    Ok(0)
}

/// # Safety
///
/// `p` is writable for two `int`s.
unsafe fn write_fd_pair(p: usize, fds: [i32; 2]) -> SysResult<i64> {
    // SAFETY: contract.
    if let Err(e) = unsafe { write_arg(p as *mut [i32; 2], fds) } {
        let _ = FDS.close(fds[0]);
        let _ = FDS.close(fds[1]);
        return Err(e);
    }
    Ok(0)
}

/// # Safety
///
/// `p` is NULL or writable for two `int`s.
unsafe fn sys_pipe(p: usize, flags: u32) -> SysResult<i64> {
    if p == 0 {
        return Err(Errno::EFAULT);
    }
    let fds = pipe::pipe2(flags)?;
    // SAFETY: contract.
    unsafe { write_fd_pair(p, fds) }
}

/// A `poll`-style timeout in milliseconds (negative: none) in ns.
fn ms_timeout(ms: i32) -> Option<i128> {
    (ms >= 0).then(|| i128::from(ms) * 1_000_000)
}

/// # Safety
///
/// `p` is NULL or a readable `struct timespec`.
unsafe fn opt_timespec(p: usize) -> SysResult<Option<i128>> {
    if p == 0 {
        return Ok(None);
    }
    // SAFETY: contract.
    let t = unsafe { read_arg(p as *const Timespec) }?;
    if !t.is_valid_duration() {
        return Err(Errno::EINVAL);
    }
    Ok(Some(t.as_nanos()))
}

/// # Safety
///
/// `p` is NULL or a readable `sigset_t`.
unsafe fn opt_sigmask(p: usize, size: usize) -> SysResult<Option<SigSet>> {
    if p == 0 {
        return Ok(None);
    }
    if size != 8 {
        return Err(Errno::EINVAL);
    }
    // SAFETY: contract.
    Ok(Some(unsafe { read_arg(p as *const SigSet) }?))
}

/// # Safety
///
/// `fds` points to `n` `struct pollfd`.
unsafe fn sys_poll(fds: usize, n: usize, timeout: Option<i128>) -> SysResult<i64> {
    if n > crate::fd::NOFILE_MAX {
        return Err(Errno::EINVAL);
    }
    if n > 0 && fds == 0 {
        return Err(Errno::EFAULT);
    }
    let p = fds as *mut PollFd;
    // SAFETY: the caller's array of `n` entries (contract).
    let mut v: Vec<PollFd> = (0..n)
        .map(|i| unsafe { p.add(i).read_unaligned() })
        .collect();
    let r = poll::poll(&mut v, timeout)?;
    for (i, e) in v.iter().enumerate() {
        // SAFETY: as above.
        unsafe { p.add(i).write_unaligned(*e) };
    }
    Ok(r as i64)
}

/// # Safety
///
/// Each set is NULL or an `fd_set` of at least `nfds` bits.
unsafe fn sys_select(nfds: i32, sets: [usize; 3], timeout: Option<i128>) -> SysResult<i64> {
    if nfds < 0 {
        return Err(Errno::EINVAL);
    }
    let nfds = (nfds as usize).min(crate::fd::NOFILE_MAX);
    let words = nfds.div_ceil(64);
    let read = |p: usize| -> Option<Vec<u64>> {
        (p != 0).then(|| {
            (0..words)
                // SAFETY: the caller's set of at least `nfds` bits.
                .map(|i| unsafe { (p as *const u64).add(i).read_unaligned() })
                .collect()
        })
    };
    let mut v = [read(sets[0]), read(sets[1]), read(sets[2])];
    // Bits at or above nfds in the last word are not looked at.
    for s in v.iter_mut().flatten() {
        if let Some(last) = s.last_mut()
            && !nfds.is_multiple_of(64)
        {
            *last &= (1u64 << (nfds % 64)) - 1;
        }
    }
    let [r, w, e] = &mut v;
    let n = poll::select(
        nfds,
        poll::FdSets {
            read: r.as_deref_mut(),
            write: w.as_deref_mut(),
            except: e.as_deref_mut(),
        },
        timeout,
    )?;
    for (s, &p) in v.iter().zip(sets.iter()) {
        if let Some(s) = s {
            for (i, w) in s.iter().enumerate() {
                // SAFETY: as above.
                unsafe { (p as *mut u64).add(i).write_unaligned(*w) };
            }
        }
    }
    Ok(n as i64)
}

/// # Safety
///
/// `events` is writable for `max` `struct epoll_event`.
unsafe fn sys_epoll_wait(
    epfd: i32,
    events: usize,
    max: i32,
    timeout: Option<i128>,
    mask: Option<SigSet>,
) -> SysResult<i64> {
    if max <= 0 || max as usize > i32::MAX as usize / size_of::<EpollEvent>() {
        return Err(Errno::EINVAL);
    }
    let e = poll::epoll_of(epfd)?;
    if events == 0 {
        return Err(Errno::EFAULT);
    }
    let ep = e.downcast::<poll::Epoll>().ok_or(Errno::EINVAL)?;
    let got = poll::with_sigmask(mask, || ep.wait(max as usize, timeout))?;
    let p = events as *mut EpollEvent;
    for (i, &(ev, data)) in got.iter().enumerate() {
        // SAFETY: at most `max` entries of the caller's array.
        unsafe { p.add(i).write_unaligned(EpollEvent { events: ev, data }) };
    }
    Ok(got.len() as i64)
}

fn with_socket<T>(fd: i32, f: impl FnOnce(&OpenFile, &Socket) -> SysResult<T>) -> SysResult<T> {
    let file = sock::socket_of(fd)?;
    let s = file.downcast::<Socket>().ok_or(Errno::ENOTSOCK)?;
    f(&file, s)
}

/// Copies `len` bytes from the caller.
///
/// # Safety
///
/// `p` is NULL or readable for `len` bytes.
unsafe fn user_bytes(p: usize, len: usize) -> SysResult<Vec<u8>> {
    // SAFETY: contract.
    pipe::copy_from_user(unsafe { UserBuf::new(p as *mut u8, len) }, 0, len)
}

/// # Safety
///
/// `p` is NULL or readable for `len` bytes.
unsafe fn read_addr(p: usize, len: u32) -> SysResult<Addr> {
    if len as usize > 128 {
        return Err(Errno::EINVAL);
    }
    if p == 0 {
        return Err(Errno::EFAULT);
    }
    // SAFETY: contract.
    let b = unsafe { user_bytes(p, len as usize) }?;
    sock::parse_addr(&b)
}

/// Writes a socket address and its length (`socklen_t*` in-out).
///
/// # Safety
///
/// `addr` is NULL or writable for `*len` bytes; `len` is NULL or a
/// readable and writable `socklen_t`.
unsafe fn write_addr(addr: usize, len: usize, bytes: &[u8]) -> SysResult<()> {
    if addr == 0 {
        return Ok(());
    }
    // SAFETY: contract.
    let have = unsafe { read_arg(len as *const i32) }?;
    if have < 0 {
        return Err(Errno::EINVAL);
    }
    let n = bytes.len().min(have as usize);
    // SAFETY: contract.
    pipe::copy_to_user(unsafe { UserBuf::new(addr as *mut u8, n) }, 0, &bytes[..n])?;
    // SAFETY: contract.
    unsafe { write_arg(len as *mut i32, bytes.len() as i32) }
}

/// # Safety
///
/// As for [`write_addr`].
unsafe fn sys_accept(fd: i32, addr: usize, len: usize, flags: i32) -> SysResult<i64> {
    let (nfd, peer) = sock::accept4(fd, flags)?;
    // SAFETY: contract.
    if let Err(e) = unsafe { write_addr(addr, len, &Name::sockaddr(peer.as_ref())) } {
        let _ = FDS.close(nfd);
        return Err(e);
    }
    Ok(i64::from(nfd))
}

/// `readv`/`writev`. A socket gets the buffers as one message (a
/// datagram stays whole); other files see one `read`/`write` per buffer,
/// stopping at the first short transfer, as the kernel does for files.
///
/// # Safety
///
/// `v` points to `n` readable `struct iovec`, each a valid buffer.
unsafe fn sys_iov(fd: i32, v: usize, n: i32, write: bool) -> SysResult<i64> {
    if !(0..=UIO_MAXIOV as i32).contains(&n) {
        return Err(Errno::EINVAL);
    }
    // SAFETY: contract.
    let iov = unsafe { iovecs(v as u64, n as u64) }?;
    let file = FDS.get(fd)?;
    if let Some(s) = file.downcast::<Socket>() {
        if write {
            let mut data = Vec::new();
            for [base, len] in iov {
                // SAFETY: each iovec is a readable buffer (contract).
                data.extend(unsafe { user_bytes(base as usize, len as usize) }?);
            }
            return s
                .send(&file, &data, SendAnc::default(), None, 0)
                .map(|n| n as i64);
        }
        let total: usize = iov.iter().map(|v| v[1] as usize).sum();
        let r = s.recv(&file, total, 0)?;
        let mut off = 0;
        for [base, len] in iov {
            let k = (len as usize).min(r.data.len() - off);
            // SAFETY: each iovec is a writable buffer (contract).
            let buf = unsafe { UserBuf::new(base as *mut u8, k) };
            pipe::copy_to_user(buf, 0, &r.data[off..off + k])?;
            off += k;
        }
        return Ok(off as i64);
    }
    let mut total: i64 = 0;
    for [base, len] in iov {
        if len == 0 {
            continue;
        }
        // SAFETY: each iovec is a valid buffer (contract).
        let buf = unsafe { UserBuf::new(base as *mut u8, len as usize) };
        let r = if write {
            file.write(buf)
        } else {
            file.read(buf)
        };
        match r {
            Ok(k) => {
                total += k as i64;
                if (k as u64) < len {
                    break;
                }
            }
            Err(e) if total == 0 => return Err(e),
            Err(_) => break,
        }
    }
    Ok(total)
}

/// `struct msghdr` (LP64).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct MsgHdr {
    name: u64,
    namelen: u32,
    _pad: u32,
    iov: u64,
    iovlen: u64,
    control: u64,
    controllen: u64,
    flags: i32,
    _pad2: u32,
}

const _: () = {
    assert!(size_of::<MsgHdr>() == 56);
    assert!(core::mem::offset_of!(MsgHdr, iov) == 16);
    assert!(core::mem::offset_of!(MsgHdr, control) == 32);
    assert!(core::mem::offset_of!(MsgHdr, flags) == 48);
};

const CMSG_HDR: usize = 16;
const SCM_MAX_FD: usize = 253;
const UIO_MAXIOV: u64 = 1024;

fn cmsg_align(n: usize) -> usize {
    (n + 7) & !7
}

/// # Safety
///
/// `iov` points to `n` readable `struct iovec`.
unsafe fn iovecs(iov: u64, n: u64) -> SysResult<Vec<[u64; 2]>> {
    if n > UIO_MAXIOV {
        return Err(Errno::EMSGSIZE);
    }
    if n > 0 && iov == 0 {
        return Err(Errno::EFAULT);
    }
    let p = iov as *const [u64; 2];
    // SAFETY: the caller's array (contract).
    Ok((0..n as usize)
        .map(|i| unsafe { p.add(i).read_unaligned() })
        .collect())
}

/// # Safety
///
/// `msg` is a readable `struct msghdr` with valid buffers.
unsafe fn sys_sendmsg(fd: i32, msg: usize, flags: i32) -> SysResult<i64> {
    // SAFETY: contract.
    let m = unsafe { read_arg(msg as *const MsgHdr) }?;
    let mut data = Vec::new();
    // SAFETY: contract.
    for [base, len] in unsafe { iovecs(m.iov, m.iovlen) }? {
        // SAFETY: each iovec is a readable buffer (contract).
        data.extend(unsafe { user_bytes(base as usize, len as usize) }?);
    }
    let mut anc = SendAnc::default();
    if m.controllen as usize >= CMSG_HDR {
        // SAFETY: contract.
        let ctl = unsafe { user_bytes(m.control as usize, m.controllen as usize) }?;
        let mut off = 0;
        while off + CMSG_HDR <= ctl.len() {
            let len = u64::from_ne_bytes(ctl[off..off + 8].try_into().map_err(|_| Errno::EINVAL)?)
                as usize;
            let level = i32::from_ne_bytes(
                ctl[off + 8..off + 12]
                    .try_into()
                    .map_err(|_| Errno::EINVAL)?,
            );
            let ty = i32::from_ne_bytes(
                ctl[off + 12..off + 16]
                    .try_into()
                    .map_err(|_| Errno::EINVAL)?,
            );
            if len < CMSG_HDR || off + len > ctl.len() {
                return Err(Errno::EINVAL);
            }
            let body = &ctl[off + CMSG_HDR..off + len];
            match (level, ty) {
                (sock::SOL_SOCKET, sock::SCM_RIGHTS) => {
                    for c in body.chunks_exact(4) {
                        let fd = i32::from_ne_bytes(c.try_into().map_err(|_| Errno::EINVAL)?);
                        anc.fds.push(FDS.get(fd)?);
                    }
                    if anc.fds.len() > SCM_MAX_FD {
                        return Err(Errno::EINVAL);
                    }
                }
                (sock::SOL_SOCKET, sock::SCM_CREDENTIALS) => {
                    // Only this process's own credentials can be sent.
                    if body.len() < 12 {
                        return Err(Errno::EINVAL);
                    }
                    let (pid, uid, gid) = sock::own_cred();
                    let w = |i: usize| {
                        u32::from_ne_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]])
                    };
                    if w(0) as i32 != pid || w(4) != uid || w(8) != gid {
                        return Err(Errno::EPERM);
                    }
                }
                _ => return Err(Errno::EINVAL),
            }
            off += cmsg_align(len);
        }
    }
    let to = if m.name == 0 {
        None
    } else {
        // SAFETY: contract.
        Some(unsafe { read_addr(m.name as usize, m.namelen) }?)
    };
    with_socket(fd, |f, s| s.send(f, &data, anc, to, flags)).map(|n| n as i64)
}

/// # Safety
///
/// `msg` is a readable and writable `struct msghdr` with valid buffers.
unsafe fn sys_recvmsg(fd: i32, msg: usize, flags: i32) -> SysResult<i64> {
    // SAFETY: contract.
    let mut m = unsafe { read_arg(msg as *const MsgHdr) }?;
    // SAFETY: contract.
    let iov = unsafe { iovecs(m.iov, m.iovlen) }?;
    let total: usize = iov.iter().map(|v| v[1] as usize).sum();
    let r = with_socket(fd, |f, s| s.recv(f, total, flags))?;
    let mut off = 0;
    for [base, len] in iov {
        if off >= r.data.len() {
            break;
        }
        let n = (len as usize).min(r.data.len() - off);
        // SAFETY: each iovec is a writable buffer (contract).
        let buf = unsafe { UserBuf::new(base as *mut u8, n) };
        pipe::copy_to_user(buf, 0, &r.data[off..off + n])?;
        off += n;
    }
    let mut out_flags = r.flags;
    // Ancillary data: credentials first, then descriptors, as Linux.
    let cap = m.controllen as usize;
    let mut ctl: Vec<u8> = Vec::new();
    let put = |ctl: &mut Vec<u8>, ty: i32, body: &[u8], out_flags: &mut i32| -> usize {
        let room = cap.saturating_sub(ctl.len());
        if room < CMSG_HDR {
            *out_flags |= sock::MSG_CTRUNC;
            return 0;
        }
        let fit = body.len().min(room - CMSG_HDR);
        if fit < body.len() {
            *out_flags |= sock::MSG_CTRUNC;
        }
        let start = ctl.len();
        ctl.extend_from_slice(&((CMSG_HDR + fit) as u64).to_ne_bytes());
        ctl.extend_from_slice(&sock::SOL_SOCKET.to_ne_bytes());
        ctl.extend_from_slice(&ty.to_ne_bytes());
        ctl.extend_from_slice(&body[..fit]);
        let space = cmsg_align(CMSG_HDR + fit).min(cap - start);
        ctl.resize(start + space, 0);
        fit
    };
    if let Some((pid, uid, gid)) = r.cred {
        let mut b = pid.to_ne_bytes().to_vec();
        b.extend_from_slice(&uid.to_ne_bytes());
        b.extend_from_slice(&gid.to_ne_bytes());
        put(&mut ctl, sock::SCM_CREDENTIALS, &b, &mut out_flags);
    }
    let mut installed: Vec<i32> = Vec::new();
    if !r.fds.is_empty() {
        let room = cap.saturating_sub(ctl.len());
        let fit = if room > CMSG_HDR {
            ((room - CMSG_HDR) / 4).min(r.fds.len())
        } else {
            0
        };
        let cloexec = flags & sock::MSG_CMSG_CLOEXEC != 0;
        let mut body = Vec::new();
        for f in r.fds.iter().take(fit) {
            match FDS.insert(Arc::clone(f), cloexec, 0) {
                Ok(nfd) => {
                    installed.push(nfd);
                    body.extend_from_slice(&nfd.to_ne_bytes());
                }
                Err(_) => {
                    out_flags |= sock::MSG_CTRUNC;
                    break;
                }
            }
        }
        if installed.len() < r.fds.len() {
            out_flags |= sock::MSG_CTRUNC;
        }
        if !body.is_empty() {
            put(&mut ctl, sock::SCM_RIGHTS, &body, &mut out_flags);
        }
    }
    if !ctl.is_empty() {
        // SAFETY: the caller's control buffer of `cap` bytes (contract).
        let w = pipe::copy_to_user(
            unsafe { UserBuf::new(m.control as *mut u8, ctl.len()) },
            0,
            &ctl,
        );
        if let Err(e) = w {
            for nfd in installed {
                let _ = FDS.close(nfd);
            }
            return Err(e);
        }
    }
    m.controllen = ctl.len() as u64;
    m.flags = out_flags;
    if m.name != 0 {
        let a = Name::sockaddr(r.from.as_ref());
        if r.from.is_some() {
            let n = a.len().min(m.namelen as usize);
            // SAFETY: the caller's name buffer of `namelen` bytes.
            pipe::copy_to_user(unsafe { UserBuf::new(m.name as *mut u8, n) }, 0, &a[..n])?;
            m.namelen = a.len() as u32;
        } else {
            m.namelen = 0;
        }
    }
    // SAFETY: contract.
    unsafe { write_arg(msg as *mut MsgHdr, m) }?;
    Ok(r.len as i64)
}
