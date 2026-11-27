//! `poll`, `select` and `epoll` (`asm-generic/poll.h`, `linux/eventpoll.h`),
//! eventfd and timerfd flags.

use core::mem::{offset_of, size_of};

pub const POLLIN: u32 = 0x1;
pub const POLLPRI: u32 = 0x2;
pub const POLLOUT: u32 = 0x4;
pub const POLLERR: u32 = 0x8;
pub const POLLHUP: u32 = 0x10;
pub const POLLNVAL: u32 = 0x20;
pub const POLLRDNORM: u32 = 0x40;
pub const POLLRDBAND: u32 = 0x80;
pub const POLLWRNORM: u32 = 0x100;
pub const POLLWRBAND: u32 = 0x200;
pub const POLLRDHUP: u32 = 0x2000;

/// `struct pollfd`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PollFd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

const _: () = {
    assert!(size_of::<PollFd>() == 8);
    assert!(offset_of!(PollFd, events) == 4);
    assert!(offset_of!(PollFd, revents) == 6);
};

pub const EPOLL_CTL_ADD: i32 = 1;
pub const EPOLL_CTL_DEL: i32 = 2;
pub const EPOLL_CTL_MOD: i32 = 3;

pub const EPOLLIN: u32 = POLLIN;
pub const EPOLLOUT: u32 = POLLOUT;
pub const EPOLLERR: u32 = POLLERR;
pub const EPOLLHUP: u32 = POLLHUP;
pub const EPOLLRDHUP: u32 = POLLRDHUP;
pub const EPOLLEXCLUSIVE: u32 = 1 << 28;
pub const EPOLLWAKEUP: u32 = 1 << 29;
pub const EPOLLONESHOT: u32 = 1 << 30;
pub const EPOLLET: u32 = 1 << 31;
pub const EPOLL_CLOEXEC: u32 = 0o2000000;

/// `struct epoll_event` on x86_64: packed, 12 bytes.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct EpollEventX86_64 {
    pub events: u32,
    pub data: u64,
}

/// `struct epoll_event` on arm64: naturally aligned, 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct EpollEventAarch64 {
    pub events: u32,
    pub data: u64,
}

const _: () = {
    assert!(size_of::<EpollEventX86_64>() == 12);
    assert!(offset_of!(EpollEventX86_64, data) == 4);
    assert!(size_of::<EpollEventAarch64>() == 16);
    assert!(offset_of!(EpollEventAarch64, data) == 8);
};

#[cfg(target_arch = "x86_64")]
pub type EpollEvent = EpollEventX86_64;
#[cfg(target_arch = "aarch64")]
pub type EpollEvent = EpollEventAarch64;

pub const EFD_SEMAPHORE: u32 = 1;
pub const EFD_CLOEXEC: u32 = 0o2000000;
pub const EFD_NONBLOCK: u32 = 0o4000;

pub const TFD_CLOEXEC: u32 = 0o2000000;
pub const TFD_NONBLOCK: u32 = 0o4000;
pub const TFD_TIMER_ABSTIME: u32 = 1;
pub const TFD_TIMER_CANCEL_ON_SET: u32 = 2;

/// `fd_set` of `select`: 1024 bits.
pub const FD_SETSIZE: usize = 1024;
