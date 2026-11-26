//! `open` flags, `fcntl` commands, `*at` flags and `lseek` whence values.
//!
//! The common flags come from `asm-generic/fcntl.h`. x86_64 uses the generic
//! values for `O_DIRECT`, `O_LARGEFILE`, `O_DIRECTORY` and `O_NOFOLLOW`;
//! arm64 overrides them in `asm-arm64/asm/fcntl.h`.

pub const O_ACCMODE: u32 = 0o3;
pub const O_RDONLY: u32 = 0o0;
pub const O_WRONLY: u32 = 0o1;
pub const O_RDWR: u32 = 0o2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_NOCTTY: u32 = 0o400;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DSYNC: u32 = 0o10000;
pub const O_ASYNC: u32 = 0o20000;
pub const O_NOATIME: u32 = 0o1000000;
pub const O_CLOEXEC: u32 = 0o2000000;
pub const __O_SYNC: u32 = 0o4000000;
pub const O_SYNC: u32 = __O_SYNC | O_DSYNC;
pub const O_PATH: u32 = 0o10000000;
pub const __O_TMPFILE: u32 = 0o20000000;

/// Flags whose values differ between the architectures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArchOpenFlags {
    pub direct: u32,
    pub largefile: u32,
    pub directory: u32,
    pub nofollow: u32,
}

pub const X86_64_OPEN_FLAGS: ArchOpenFlags = ArchOpenFlags {
    direct: 0o40000,
    largefile: 0o100000,
    directory: 0o200000,
    nofollow: 0o400000,
};

pub const AARCH64_OPEN_FLAGS: ArchOpenFlags = ArchOpenFlags {
    directory: 0o40000,
    nofollow: 0o100000,
    direct: 0o200000,
    largefile: 0o400000,
};

#[cfg(target_arch = "x86_64")]
const ARCH: ArchOpenFlags = X86_64_OPEN_FLAGS;
#[cfg(target_arch = "aarch64")]
const ARCH: ArchOpenFlags = AARCH64_OPEN_FLAGS;

pub const O_DIRECT: u32 = ARCH.direct;
pub const O_LARGEFILE: u32 = ARCH.largefile;
pub const O_DIRECTORY: u32 = ARCH.directory;
pub const O_NOFOLLOW: u32 = ARCH.nofollow;
pub const O_TMPFILE: u32 = __O_TMPFILE | O_DIRECTORY;

/// File status flags that `F_SETFL` may change (Linux `SETFL_MASK`).
pub const SETFL_MASK: u32 = O_APPEND | O_NONBLOCK | O_ASYNC | O_DIRECT | O_NOATIME;

pub const F_DUPFD: i32 = 0;
pub const F_GETFD: i32 = 1;
pub const F_SETFD: i32 = 2;
pub const F_GETFL: i32 = 3;
pub const F_SETFL: i32 = 4;
pub const F_DUPFD_CLOEXEC: i32 = 1030;

pub const FD_CLOEXEC: i32 = 1;

pub const AT_FDCWD: i32 = -100;
pub const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
pub const AT_NO_AUTOMOUNT: u32 = 0x800;
pub const AT_EMPTY_PATH: u32 = 0x1000;

pub const SEEK_SET: i32 = 0;
pub const SEEK_CUR: i32 = 1;
pub const SEEK_END: i32 = 2;
pub const SEEK_DATA: i32 = 3;
pub const SEEK_HOLE: i32 = 4;

/// Largest transfer of one `read` or `write` (Linux `MAX_RW_COUNT`,
/// `INT_MAX & PAGE_MASK`).
pub const MAX_RW_COUNT: usize = 0x7fff_f000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_flags_do_not_overlap_common_flags() {
        for arch in [X86_64_OPEN_FLAGS, AARCH64_OPEN_FLAGS] {
            let arch_bits = arch.direct | arch.largefile | arch.directory | arch.nofollow;
            assert_eq!(arch_bits.count_ones(), 4);
            let common = O_ACCMODE
                | O_CREAT
                | O_EXCL
                | O_NOCTTY
                | O_TRUNC
                | O_APPEND
                | O_NONBLOCK
                | O_DSYNC
                | O_ASYNC
                | O_NOATIME
                | O_CLOEXEC
                | __O_SYNC
                | O_PATH
                | __O_TMPFILE;
            assert_eq!(arch_bits & common, 0);
        }
    }

    #[test]
    fn known_values() {
        // Spot checks against the octal values in the UAPI headers.
        assert_eq!(O_CLOEXEC, 0x80000);
        assert_eq!(X86_64_OPEN_FLAGS.directory, 0x10000);
        assert_eq!(AARCH64_OPEN_FLAGS.directory, 0x4000);
        assert_eq!(O_SYNC, 0x101000);
    }
}
