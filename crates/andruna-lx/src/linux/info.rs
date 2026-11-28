//! System information structures: `struct utsname` (`linux/utsname.h`),
//! `struct rusage` (`linux/resource.h`) and `struct sysinfo`
//! (`linux/sysinfo.h`), LP64 layouts (the same on x86_64 and aarch64).

use core::mem::{offset_of, size_of};

/// Length of each `utsname` field, NUL included (`__NEW_UTS_LEN + 1`).
pub const UTS_LEN: usize = 65;

/// `struct new_utsname`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Utsname {
    pub sysname: [u8; UTS_LEN],
    pub nodename: [u8; UTS_LEN],
    pub release: [u8; UTS_LEN],
    pub version: [u8; UTS_LEN],
    pub machine: [u8; UTS_LEN],
    pub domainname: [u8; UTS_LEN],
}

const _: () = {
    assert!(size_of::<Utsname>() == 390);
    assert!(offset_of!(Utsname, release) == 130);
    assert!(offset_of!(Utsname, machine) == 260);
    assert!(offset_of!(Utsname, domainname) == 325);
};

/// `struct timeval` on LP64.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// `struct rusage`: two `timeval`s and fourteen `long`s.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rusage {
    pub utime: Timeval,
    pub stime: Timeval,
    pub maxrss: i64,
    pub ixrss: i64,
    pub idrss: i64,
    pub isrss: i64,
    pub minflt: i64,
    pub majflt: i64,
    pub nswap: i64,
    pub inblock: i64,
    pub oublock: i64,
    pub msgsnd: i64,
    pub msgrcv: i64,
    pub nsignals: i64,
    pub nvcsw: i64,
    pub nivcsw: i64,
}

const _: () = {
    assert!(size_of::<Rusage>() == 144);
    assert!(offset_of!(Rusage, stime) == 16);
    assert!(offset_of!(Rusage, maxrss) == 32);
    assert!(offset_of!(Rusage, minflt) == 64);
    assert!(offset_of!(Rusage, nivcsw) == 136);
};

pub const RUSAGE_SELF: i32 = 0;
pub const RUSAGE_CHILDREN: i32 = -1;
pub const RUSAGE_THREAD: i32 = 1;

/// `struct sysinfo` on LP64.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sysinfo {
    pub uptime: i64,
    pub loads: [u64; 3],
    pub totalram: u64,
    pub freeram: u64,
    pub sharedram: u64,
    pub bufferram: u64,
    pub totalswap: u64,
    pub freeswap: u64,
    pub procs: u16,
    pub pad: u16,
    pub totalhigh: u64,
    pub freehigh: u64,
    pub mem_unit: u32,
    pub _f: [u8; 4],
}

const _: () = {
    assert!(size_of::<Sysinfo>() == 112);
    assert!(offset_of!(Sysinfo, loads) == 8);
    assert!(offset_of!(Sysinfo, totalram) == 32);
    assert!(offset_of!(Sysinfo, procs) == 80);
    assert!(offset_of!(Sysinfo, totalhigh) == 88);
    assert!(offset_of!(Sysinfo, mem_unit) == 104);
};

/// Fixed-point shift of `Sysinfo::loads`.
pub const SI_LOAD_SHIFT: u32 = 16;

pub const PRIO_PROCESS: i32 = 0;
pub const PRIO_PGRP: i32 = 1;
pub const PRIO_USER: i32 = 2;

pub const MEMBARRIER_CMD_QUERY: i32 = 0;
pub const MEMBARRIER_CMD_GLOBAL: i32 = 1;
pub const MEMBARRIER_CMD_GLOBAL_EXPEDITED: i32 = 2;
pub const MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED: i32 = 4;
pub const MEMBARRIER_CMD_PRIVATE_EXPEDITED: i32 = 8;
pub const MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED: i32 = 16;
pub const MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE: i32 = 32;
pub const MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE: i32 = 64;
