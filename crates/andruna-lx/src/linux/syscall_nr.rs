//! System call numbers for the calls lx dispatches through `syscall()`.
//!
//! x86_64 numbers are from `asm-x86/asm/unistd_64.h`; arm64 uses the
//! generic table (`asm-generic/unistd.h`). Both tables are compiled on every
//! architecture; [`NATIVE`] selects the one of the build.

/// The system calls `syscall()` understands, independent of numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sysno {
    Read,
    Write,
    Openat,
    Close,
    Fstat,
    Newfstatat,
    Lseek,
    Mmap,
    Mprotect,
    Munmap,
    Madvise,
    Pread64,
    Pwrite64,
    SchedYield,
    Dup,
    Dup3,
    Fcntl,
    Nanosleep,
    Getpid,
    Gettid,
    Futex,
    ClockGettime,
    ClockGetres,
    ClockNanosleep,
    Getcwd,
    Chdir,
    Mremap,
    RtSigaction,
    RtSigprocmask,
    RtSigreturn,
    RtSigpending,
    RtSigtimedwait,
    RtSigqueueinfo,
    RtTgsigqueueinfo,
    RtSigsuspend,
    Sigaltstack,
    Kill,
    Tkill,
    Tgkill,
    Getdents64,
    Unlinkat,
    Mkdirat,
    Renameat,
    Renameat2,
    Symlinkat,
    Readlinkat,
    Ftruncate,
    Truncate,
    Fsync,
    Fdatasync,
    Faccessat,
    Faccessat2,
    // Only on x86_64 (arm64 has no legacy forms).
    Open,
    Stat,
    Lstat,
    Dup2,
    Pause,
    Unlink,
    Mkdir,
    Rmdir,
    Rename,
    Symlink,
    Readlink,
    Access,
}

/// One architecture's numbering.
pub struct Table(pub &'static [(i64, Sysno)]);

impl Table {
    pub fn lookup(&self, nr: i64) -> Option<Sysno> {
        self.0.iter().find(|(n, _)| *n == nr).map(|(_, s)| *s)
    }

    pub fn number(&self, sysno: Sysno) -> Option<i64> {
        self.0.iter().find(|(_, s)| *s == sysno).map(|(n, _)| *n)
    }
}

pub const X86_64: Table = Table(&[
    (0, Sysno::Read),
    (1, Sysno::Write),
    (2, Sysno::Open),
    (3, Sysno::Close),
    (4, Sysno::Stat),
    (5, Sysno::Fstat),
    (6, Sysno::Lstat),
    (8, Sysno::Lseek),
    (9, Sysno::Mmap),
    (10, Sysno::Mprotect),
    (11, Sysno::Munmap),
    (13, Sysno::RtSigaction),
    (14, Sysno::RtSigprocmask),
    (15, Sysno::RtSigreturn),
    (17, Sysno::Pread64),
    (18, Sysno::Pwrite64),
    (21, Sysno::Access),
    (24, Sysno::SchedYield),
    (25, Sysno::Mremap),
    (28, Sysno::Madvise),
    (32, Sysno::Dup),
    (33, Sysno::Dup2),
    (34, Sysno::Pause),
    (35, Sysno::Nanosleep),
    (39, Sysno::Getpid),
    (62, Sysno::Kill),
    (72, Sysno::Fcntl),
    (74, Sysno::Fsync),
    (75, Sysno::Fdatasync),
    (76, Sysno::Truncate),
    (77, Sysno::Ftruncate),
    (79, Sysno::Getcwd),
    (80, Sysno::Chdir),
    (82, Sysno::Rename),
    (83, Sysno::Mkdir),
    (84, Sysno::Rmdir),
    (87, Sysno::Unlink),
    (88, Sysno::Symlink),
    (89, Sysno::Readlink),
    (127, Sysno::RtSigpending),
    (128, Sysno::RtSigtimedwait),
    (129, Sysno::RtSigqueueinfo),
    (130, Sysno::RtSigsuspend),
    (131, Sysno::Sigaltstack),
    (186, Sysno::Gettid),
    (200, Sysno::Tkill),
    (202, Sysno::Futex),
    (217, Sysno::Getdents64),
    (228, Sysno::ClockGettime),
    (229, Sysno::ClockGetres),
    (230, Sysno::ClockNanosleep),
    (234, Sysno::Tgkill),
    (257, Sysno::Openat),
    (258, Sysno::Mkdirat),
    (262, Sysno::Newfstatat),
    (263, Sysno::Unlinkat),
    (264, Sysno::Renameat),
    (266, Sysno::Symlinkat),
    (267, Sysno::Readlinkat),
    (269, Sysno::Faccessat),
    (292, Sysno::Dup3),
    (297, Sysno::RtTgsigqueueinfo),
    (316, Sysno::Renameat2),
    (439, Sysno::Faccessat2),
]);

pub const AARCH64: Table = Table(&[
    (17, Sysno::Getcwd),
    (23, Sysno::Dup),
    (24, Sysno::Dup3),
    (25, Sysno::Fcntl),
    (34, Sysno::Mkdirat),
    (35, Sysno::Unlinkat),
    (36, Sysno::Symlinkat),
    (38, Sysno::Renameat),
    (45, Sysno::Truncate),
    (46, Sysno::Ftruncate),
    (48, Sysno::Faccessat),
    (49, Sysno::Chdir),
    (56, Sysno::Openat),
    (57, Sysno::Close),
    (61, Sysno::Getdents64),
    (62, Sysno::Lseek),
    (63, Sysno::Read),
    (64, Sysno::Write),
    (67, Sysno::Pread64),
    (68, Sysno::Pwrite64),
    (78, Sysno::Readlinkat),
    (79, Sysno::Newfstatat),
    (80, Sysno::Fstat),
    (82, Sysno::Fsync),
    (83, Sysno::Fdatasync),
    (98, Sysno::Futex),
    (101, Sysno::Nanosleep),
    (113, Sysno::ClockGettime),
    (114, Sysno::ClockGetres),
    (115, Sysno::ClockNanosleep),
    (124, Sysno::SchedYield),
    (129, Sysno::Kill),
    (130, Sysno::Tkill),
    (131, Sysno::Tgkill),
    (132, Sysno::Sigaltstack),
    (133, Sysno::RtSigsuspend),
    (134, Sysno::RtSigaction),
    (135, Sysno::RtSigprocmask),
    (136, Sysno::RtSigpending),
    (137, Sysno::RtSigtimedwait),
    (138, Sysno::RtSigqueueinfo),
    (139, Sysno::RtSigreturn),
    (172, Sysno::Getpid),
    (178, Sysno::Gettid),
    (215, Sysno::Munmap),
    (216, Sysno::Mremap),
    (222, Sysno::Mmap),
    (226, Sysno::Mprotect),
    (233, Sysno::Madvise),
    (240, Sysno::RtTgsigqueueinfo),
    (276, Sysno::Renameat2),
    (439, Sysno::Faccessat2),
]);

#[cfg(target_arch = "x86_64")]
pub const NATIVE: Table = X86_64;
#[cfg(target_arch = "aarch64")]
pub const NATIVE: Table = AARCH64;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn check_unique(t: &Table) {
        let nums: HashSet<_> = t.0.iter().map(|(n, _)| *n).collect();
        let calls: HashSet<_> = t.0.iter().map(|(_, s)| *s).collect();
        assert_eq!(nums.len(), t.0.len());
        assert_eq!(calls.len(), t.0.len());
    }

    #[test]
    fn tables_are_one_to_one() {
        check_unique(&X86_64);
        check_unique(&AARCH64);
    }

    #[test]
    fn spot_checks() {
        assert_eq!(X86_64.number(Sysno::Futex), Some(202));
        assert_eq!(AARCH64.number(Sysno::Futex), Some(98));
        assert_eq!(X86_64.number(Sysno::Gettid), Some(186));
        assert_eq!(AARCH64.number(Sysno::Gettid), Some(178));
        assert_eq!(AARCH64.lookup(2), None);
    }
}
