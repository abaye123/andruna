//! `struct stat` for both architectures and the file mode bits.
//!
//! x86_64 follows bionic's `sys/stat.h` x86_64 branch (the kernel's
//! `asm-x86/asm/stat.h` 64-bit layout, 144 bytes). arm64 follows bionic's
//! `__STAT64_BODY` (`asm-generic/stat.h`, 128 bytes).

use core::mem::{offset_of, size_of};

use super::time::Timespec;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFSOCK: u32 = 0o140000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;

/// The fields lx fills, independent of the architecture's layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatData {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub size: i64,
    pub blksize: i32,
    pub blocks: i64,
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
}

/// x86_64 `struct stat`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatX86_64 {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_nlink: u64,
    pub st_mode: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub __pad0: u32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atim: Timespec,
    pub st_mtim: Timespec,
    pub st_ctim: Timespec,
    pub __pad3: [i64; 3],
}

const _: () = {
    assert!(size_of::<StatX86_64>() == 144);
    assert!(offset_of!(StatX86_64, st_dev) == 0);
    assert!(offset_of!(StatX86_64, st_ino) == 8);
    assert!(offset_of!(StatX86_64, st_nlink) == 16);
    assert!(offset_of!(StatX86_64, st_mode) == 24);
    assert!(offset_of!(StatX86_64, st_uid) == 28);
    assert!(offset_of!(StatX86_64, st_gid) == 32);
    assert!(offset_of!(StatX86_64, st_rdev) == 40);
    assert!(offset_of!(StatX86_64, st_size) == 48);
    assert!(offset_of!(StatX86_64, st_blksize) == 56);
    assert!(offset_of!(StatX86_64, st_blocks) == 64);
    assert!(offset_of!(StatX86_64, st_atim) == 72);
    assert!(offset_of!(StatX86_64, st_mtim) == 88);
    assert!(offset_of!(StatX86_64, st_ctim) == 104);
    assert!(offset_of!(StatX86_64, __pad3) == 120);
};

impl From<&StatData> for StatX86_64 {
    fn from(d: &StatData) -> Self {
        Self {
            st_dev: d.dev,
            st_ino: d.ino,
            st_nlink: u64::from(d.nlink),
            st_mode: d.mode,
            st_uid: d.uid,
            st_gid: d.gid,
            __pad0: 0,
            st_rdev: d.rdev,
            st_size: d.size,
            st_blksize: i64::from(d.blksize),
            st_blocks: d.blocks,
            st_atim: d.atime,
            st_mtim: d.mtime,
            st_ctim: d.ctime,
            __pad3: [0; 3],
        }
    }
}

/// arm64 `struct stat`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatAarch64 {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_mode: u32,
    pub st_nlink: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    pub __pad1: u64,
    pub st_size: i64,
    pub st_blksize: i32,
    pub __pad2: i32,
    pub st_blocks: i64,
    pub st_atim: Timespec,
    pub st_mtim: Timespec,
    pub st_ctim: Timespec,
    pub __unused4: u32,
    pub __unused5: u32,
}

const _: () = {
    assert!(size_of::<StatAarch64>() == 128);
    assert!(offset_of!(StatAarch64, st_dev) == 0);
    assert!(offset_of!(StatAarch64, st_ino) == 8);
    assert!(offset_of!(StatAarch64, st_mode) == 16);
    assert!(offset_of!(StatAarch64, st_nlink) == 20);
    assert!(offset_of!(StatAarch64, st_uid) == 24);
    assert!(offset_of!(StatAarch64, st_gid) == 28);
    assert!(offset_of!(StatAarch64, st_rdev) == 32);
    assert!(offset_of!(StatAarch64, st_size) == 48);
    assert!(offset_of!(StatAarch64, st_blksize) == 56);
    assert!(offset_of!(StatAarch64, st_blocks) == 64);
    assert!(offset_of!(StatAarch64, st_atim) == 72);
    assert!(offset_of!(StatAarch64, st_mtim) == 88);
    assert!(offset_of!(StatAarch64, st_ctim) == 104);
    assert!(offset_of!(StatAarch64, __unused4) == 120);
};

impl From<&StatData> for StatAarch64 {
    fn from(d: &StatData) -> Self {
        Self {
            st_dev: d.dev,
            st_ino: d.ino,
            st_mode: d.mode,
            st_nlink: d.nlink,
            st_uid: d.uid,
            st_gid: d.gid,
            st_rdev: d.rdev,
            __pad1: 0,
            st_size: d.size,
            st_blksize: d.blksize,
            __pad2: 0,
            st_blocks: d.blocks,
            st_atim: d.atime,
            st_mtim: d.mtime,
            st_ctim: d.ctime,
            __unused4: 0,
            __unused5: 0,
        }
    }
}

/// `struct stat` of the architecture being built.
#[cfg(target_arch = "x86_64")]
pub type Stat = StatX86_64;
/// `struct stat` of the architecture being built.
#[cfg(target_arch = "aarch64")]
pub type Stat = StatAarch64;
