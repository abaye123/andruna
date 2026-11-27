//! Resource limits: `prlimit64`, `getrlimit`, `setrlimit`.
//!
//! The limits describe this process as lx runs it:
//!
//! | Resource | Soft / hard | Source |
//! | --- | --- | --- |
//! | `RLIMIT_STACK` | the main thread's stack size, both | Windows' stack limits of the main thread |
//! | `RLIMIT_NOFILE` | 32768, both | lx's descriptor table ([`crate::fd::NOFILE_MAX`]) |
//! | `RLIMIT_CORE` | 0 / unlimited | no core files are written |
//! | `RLIMIT_NICE`, `RLIMIT_RTPRIO` | 0, both | real-time and raised priorities are refused (`EPERM`) |
//! | everything else | unlimited | |
//!
//! `setrlimit` follows Linux's rules (soft at most hard; raising a hard
//! limit needs uid 0).
//!
//! # Deviations from Linux
//!
//! - Only `RLIMIT_NOFILE` is enforced. Other limits are stored and
//!   reported; lowering them does not make allocations, file sizes or CPU
//!   time fail.

use std::sync::Mutex;

use crate::errno::{Errno, SysResult};
use crate::util::lock;

pub const RLIMIT_STACK: i32 = 3;
pub const RLIMIT_CORE: i32 = 4;
pub const RLIMIT_NOFILE: i32 = 7;
pub const RLIMIT_NICE: i32 = 13;
pub const RLIMIT_RTPRIO: i32 = 14;
pub const RLIM_NLIMITS: usize = 16;
pub const RLIM_INFINITY: u64 = u64::MAX;

/// `struct rlimit64`: soft (current) and hard (maximum) limit.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rlimit {
    pub cur: u64,
    pub max: u64,
}

const _: () = assert!(size_of::<Rlimit>() == 16);

static LIMITS: Mutex<Option<[Rlimit; RLIM_NLIMITS]>> = Mutex::new(None);

fn defaults() -> [Rlimit; RLIM_NLIMITS] {
    let inf = Rlimit {
        cur: RLIM_INFINITY,
        max: RLIM_INFINITY,
    };
    let mut l = [inf; RLIM_NLIMITS];
    let stack = crate::sys::thread::main_stack_size();
    l[RLIMIT_STACK as usize] = Rlimit {
        cur: stack,
        max: stack,
    };
    let nofile = crate::fd::NOFILE_MAX as u64;
    l[RLIMIT_NOFILE as usize] = Rlimit {
        cur: nofile,
        max: nofile,
    };
    l[RLIMIT_CORE as usize] = Rlimit {
        cur: 0,
        max: RLIM_INFINITY,
    };
    l[RLIMIT_NICE as usize] = Rlimit { cur: 0, max: 0 };
    l[RLIMIT_RTPRIO as usize] = Rlimit { cur: 0, max: 0 };
    l
}

fn check_pid(pid: i32) -> SysResult<()> {
    if pid == 0 || pid == crate::process::getpid() {
        Ok(())
    } else if pid < 0 {
        Err(Errno::EINVAL)
    } else {
        Err(Errno::ESRCH)
    }
}

/// `prlimit64`: sets `new` when given; returns the old limit.
pub fn prlimit(pid: i32, resource: i32, new: Option<Rlimit>) -> SysResult<Rlimit> {
    check_pid(pid)?;
    let r = usize::try_from(resource)
        .ok()
        .filter(|&r| r < RLIM_NLIMITS)
        .ok_or(Errno::EINVAL)?;
    let mut g = lock(&LIMITS);
    let l = g.get_or_insert_with(defaults);
    let old = l[r];
    if let Some(n) = new {
        if n.cur > n.max {
            return Err(Errno::EINVAL);
        }
        if n.max > old.max && crate::process::uid() != 0 {
            return Err(Errno::EPERM);
        }
        if resource == RLIMIT_NOFILE && n.max > crate::fd::NOFILE_MAX as u64 {
            // Linux refuses a hard limit above `fs.nr_open`.
            return Err(Errno::EPERM);
        }
        l[r] = n;
        if resource == RLIMIT_NOFILE {
            crate::fd::set_nofile_limit(n.cur as usize);
        }
    }
    Ok(old)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits() {
        let s = prlimit(0, RLIMIT_STACK, None).unwrap();
        assert!(s.cur >= 64 * 1024 && s.cur == s.max);
        let n = prlimit(0, RLIMIT_NOFILE, None).unwrap();
        assert_eq!(n.cur, crate::fd::NOFILE_MAX as u64);
        assert_eq!(
            prlimit(0, RLIMIT_NOFILE, Some(Rlimit { cur: 9, max: 8 })),
            Err(Errno::EINVAL)
        );
        assert_eq!(prlimit(0, 99, None), Err(Errno::EINVAL));
        assert_eq!(prlimit(1, RLIMIT_CORE, None), Err(Errno::ESRCH));
        let c = prlimit(0, RLIMIT_CORE, None).unwrap();
        assert_eq!(c.cur, 0);
        prlimit(0, RLIMIT_CORE, Some(Rlimit { cur: 0, max: 0 })).unwrap();
        assert_eq!(prlimit(0, RLIMIT_CORE, None).unwrap().max, 0);
    }
}
