//! The binder user-space interface (`linux/android/binder.h`, 64-bit
//! protocol version 8): ioctl numbers, command and return codes and the
//! structures that cross the boundary, with layout assertions against the
//! Linux layout.

#![allow(non_camel_case_types)]

use core::mem::{offset_of, size_of};

const IOC_NRBITS: u32 = 8;
const IOC_TYPEBITS: u32 = 8;
const IOC_SIZEBITS: u32 = 14;
const IOC_NRSHIFT: u32 = 0;
const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;
const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, ty: u8, nr: u32, size: usize) -> u32 {
    (dir << IOC_DIRSHIFT)
        | ((ty as u32) << IOC_TYPESHIFT)
        | (nr << IOC_NRSHIFT)
        | ((size as u32) << IOC_SIZESHIFT)
}
const fn io(ty: u8, nr: u32) -> u32 {
    ioc(IOC_NONE, ty, nr, 0)
}
const fn ior(ty: u8, nr: u32, size: usize) -> u32 {
    ioc(IOC_READ, ty, nr, size)
}
const fn iow(ty: u8, nr: u32, size: usize) -> u32 {
    ioc(IOC_WRITE, ty, nr, size)
}
const fn iowr(ty: u8, nr: u32, size: usize) -> u32 {
    ioc(IOC_READ | IOC_WRITE, ty, nr, size)
}

/// The size field of a command or return code.
pub const fn ioc_size(cmd: u32) -> usize {
    ((cmd >> IOC_SIZESHIFT) & ((1 << IOC_SIZEBITS) - 1)) as usize
}

const fn pack(c1: u8, c2: u8, c3: u8, c4: u8) -> u32 {
    ((c1 as u32) << 24) | ((c2 as u32) << 16) | ((c3 as u32) << 8) | c4 as u32
}
const B_TYPE_LARGE: u8 = 0x85;
pub const BINDER_TYPE_BINDER: u32 = pack(b's', b'b', b'*', B_TYPE_LARGE);
pub const BINDER_TYPE_WEAK_BINDER: u32 = pack(b'w', b'b', b'*', B_TYPE_LARGE);
pub const BINDER_TYPE_HANDLE: u32 = pack(b's', b'h', b'*', B_TYPE_LARGE);
pub const BINDER_TYPE_WEAK_HANDLE: u32 = pack(b'w', b'h', b'*', B_TYPE_LARGE);
pub const BINDER_TYPE_FD: u32 = pack(b'f', b'd', b'*', B_TYPE_LARGE);
pub const BINDER_TYPE_FDA: u32 = pack(b'f', b'd', b'a', B_TYPE_LARGE);
pub const BINDER_TYPE_PTR: u32 = pack(b'p', b't', b'*', B_TYPE_LARGE);

pub const FLAT_BINDER_FLAG_PRIORITY_MASK: u32 = 0xff;
pub const FLAT_BINDER_FLAG_ACCEPTS_FDS: u32 = 0x100;
pub const FLAT_BINDER_FLAG_TXN_SECURITY_CTX: u32 = 0x1000;

pub const TF_ONE_WAY: u32 = 0x01;
pub const TF_ROOT_OBJECT: u32 = 0x04;
pub const TF_STATUS_CODE: u32 = 0x08;
pub const TF_ACCEPT_FDS: u32 = 0x10;
pub const TF_CLEAR_BUF: u32 = 0x20;
pub const TF_UPDATE_TXN: u32 = 0x40;

pub const BINDER_CURRENT_PROTOCOL_VERSION: i32 = 8;

/// `struct flat_binder_object` (also `binder_fd_object`: the fd is the low
/// 32 bits of `binder`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct flat_binder_object {
    pub hdr_type: u32,
    pub flags: u32,
    /// `binder` (a pointer) or `handle` / `fd` in the low 32 bits.
    pub binder: u64,
    pub cookie: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct binder_write_read {
    pub write_size: u64,
    pub write_consumed: u64,
    pub write_buffer: u64,
    pub read_size: u64,
    pub read_consumed: u64,
    pub read_buffer: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct binder_version {
    pub protocol_version: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct binder_node_debug_info {
    pub ptr: u64,
    pub cookie: u64,
    pub has_strong_ref: u32,
    pub has_weak_ref: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct binder_node_info_for_ref {
    pub handle: u32,
    pub strong_count: u32,
    pub weak_count: u32,
    pub reserved1: u32,
    pub reserved2: u32,
    pub reserved3: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct binder_extended_error {
    pub id: u32,
    pub command: u32,
    pub param: i32,
}

/// `struct binder_transaction_data`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct binder_transaction_data {
    /// `target.handle` (low 32 bits) or `target.ptr`.
    pub target: u64,
    pub cookie: u64,
    pub code: u32,
    pub flags: u32,
    pub sender_pid: i32,
    pub sender_euid: u32,
    pub data_size: u64,
    pub offsets_size: u64,
    /// `data.ptr.buffer`.
    pub buffer: u64,
    /// `data.ptr.offsets`.
    pub offsets: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct binder_transaction_data_secctx {
    pub transaction_data: binder_transaction_data,
    pub secctx: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct binder_transaction_data_sg {
    pub transaction_data: binder_transaction_data,
    pub buffers_size: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct binder_ptr_cookie {
    pub ptr: u64,
    pub cookie: u64,
}

/// `struct binder_handle_cookie` (packed: 12 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct binder_handle_cookie {
    pub handle: u32,
    pub cookie: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct binder_pri_desc {
    pub priority: i32,
    pub desc: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct binder_pri_ptr_cookie {
    pub priority: i32,
    pub ptr: u64,
    pub cookie: u64,
}

// Layout of the Linux 64-bit ABI (identical on x86_64 and arm64).
const _: () = {
    assert!(size_of::<flat_binder_object>() == 24);
    assert!(offset_of!(flat_binder_object, flags) == 4);
    assert!(offset_of!(flat_binder_object, binder) == 8);
    assert!(offset_of!(flat_binder_object, cookie) == 16);
    assert!(size_of::<binder_write_read>() == 48);
    assert!(offset_of!(binder_write_read, read_size) == 24);
    assert!(offset_of!(binder_write_read, read_buffer) == 40);
    assert!(size_of::<binder_version>() == 4);
    assert!(size_of::<binder_node_debug_info>() == 24);
    assert!(size_of::<binder_node_info_for_ref>() == 24);
    assert!(size_of::<binder_extended_error>() == 12);
    assert!(size_of::<binder_transaction_data>() == 64);
    assert!(offset_of!(binder_transaction_data, cookie) == 8);
    assert!(offset_of!(binder_transaction_data, code) == 16);
    assert!(offset_of!(binder_transaction_data, flags) == 20);
    assert!(offset_of!(binder_transaction_data, sender_pid) == 24);
    assert!(offset_of!(binder_transaction_data, sender_euid) == 28);
    assert!(offset_of!(binder_transaction_data, data_size) == 32);
    assert!(offset_of!(binder_transaction_data, offsets_size) == 40);
    assert!(offset_of!(binder_transaction_data, buffer) == 48);
    assert!(offset_of!(binder_transaction_data, offsets) == 56);
    assert!(size_of::<binder_transaction_data_secctx>() == 72);
    assert!(size_of::<binder_transaction_data_sg>() == 72);
    assert!(size_of::<binder_ptr_cookie>() == 16);
    assert!(size_of::<binder_handle_cookie>() == 12);
    assert!(size_of::<binder_pri_desc>() == 8);
    assert!(size_of::<binder_pri_ptr_cookie>() == 24);
};

pub const BINDER_WRITE_READ: u32 = iowr(b'b', 1, size_of::<binder_write_read>());
pub const BINDER_SET_IDLE_TIMEOUT: u32 = iow(b'b', 3, 8);
pub const BINDER_SET_MAX_THREADS: u32 = iow(b'b', 5, 4);
pub const BINDER_SET_IDLE_PRIORITY: u32 = iow(b'b', 6, 4);
pub const BINDER_SET_CONTEXT_MGR: u32 = iow(b'b', 7, 4);
pub const BINDER_THREAD_EXIT: u32 = iow(b'b', 8, 4);
pub const BINDER_VERSION: u32 = iowr(b'b', 9, size_of::<binder_version>());
pub const BINDER_GET_NODE_DEBUG_INFO: u32 = iowr(b'b', 11, size_of::<binder_node_debug_info>());
pub const BINDER_GET_NODE_INFO_FOR_REF: u32 = iowr(b'b', 12, size_of::<binder_node_info_for_ref>());
pub const BINDER_SET_CONTEXT_MGR_EXT: u32 = iow(b'b', 13, size_of::<flat_binder_object>());
pub const BINDER_FREEZE: u32 = iow(b'b', 14, 12);
pub const BINDER_GET_FROZEN_INFO: u32 = iowr(b'b', 15, 12);
pub const BINDER_ENABLE_ONEWAY_SPAM_DETECTION: u32 = iow(b'b', 16, 4);
pub const BINDER_GET_EXTENDED_ERROR: u32 = iowr(b'b', 17, size_of::<binder_extended_error>());

pub const BR_ERROR: u32 = ior(b'r', 0, 4);
pub const BR_OK: u32 = io(b'r', 1);
pub const BR_TRANSACTION_SEC_CTX: u32 = ior(b'r', 2, size_of::<binder_transaction_data_secctx>());
pub const BR_TRANSACTION: u32 = ior(b'r', 2, size_of::<binder_transaction_data>());
pub const BR_REPLY: u32 = ior(b'r', 3, size_of::<binder_transaction_data>());
pub const BR_ACQUIRE_RESULT: u32 = ior(b'r', 4, 4);
pub const BR_DEAD_REPLY: u32 = io(b'r', 5);
pub const BR_TRANSACTION_COMPLETE: u32 = io(b'r', 6);
pub const BR_INCREFS: u32 = ior(b'r', 7, size_of::<binder_ptr_cookie>());
pub const BR_ACQUIRE: u32 = ior(b'r', 8, size_of::<binder_ptr_cookie>());
pub const BR_RELEASE: u32 = ior(b'r', 9, size_of::<binder_ptr_cookie>());
pub const BR_DECREFS: u32 = ior(b'r', 10, size_of::<binder_ptr_cookie>());
pub const BR_NOOP: u32 = io(b'r', 12);
pub const BR_SPAWN_LOOPER: u32 = io(b'r', 13);
pub const BR_FINISHED: u32 = io(b'r', 14);
pub const BR_DEAD_BINDER: u32 = ior(b'r', 15, 8);
pub const BR_CLEAR_DEATH_NOTIFICATION_DONE: u32 = ior(b'r', 16, 8);
pub const BR_FAILED_REPLY: u32 = io(b'r', 17);
pub const BR_FROZEN_REPLY: u32 = io(b'r', 18);
pub const BR_ONEWAY_SPAM_SUSPECT: u32 = io(b'r', 19);

pub const BC_TRANSACTION: u32 = iow(b'c', 0, size_of::<binder_transaction_data>());
pub const BC_REPLY: u32 = iow(b'c', 1, size_of::<binder_transaction_data>());
pub const BC_ACQUIRE_RESULT: u32 = iow(b'c', 2, 4);
pub const BC_FREE_BUFFER: u32 = iow(b'c', 3, 8);
pub const BC_INCREFS: u32 = iow(b'c', 4, 4);
pub const BC_ACQUIRE: u32 = iow(b'c', 5, 4);
pub const BC_RELEASE: u32 = iow(b'c', 6, 4);
pub const BC_DECREFS: u32 = iow(b'c', 7, 4);
pub const BC_INCREFS_DONE: u32 = iow(b'c', 8, size_of::<binder_ptr_cookie>());
pub const BC_ACQUIRE_DONE: u32 = iow(b'c', 9, size_of::<binder_ptr_cookie>());
pub const BC_ATTEMPT_ACQUIRE: u32 = iow(b'c', 10, size_of::<binder_pri_desc>());
pub const BC_REGISTER_LOOPER: u32 = io(b'c', 11);
pub const BC_ENTER_LOOPER: u32 = io(b'c', 12);
pub const BC_EXIT_LOOPER: u32 = io(b'c', 13);
pub const BC_REQUEST_DEATH_NOTIFICATION: u32 = iow(b'c', 14, size_of::<binder_handle_cookie>());
pub const BC_CLEAR_DEATH_NOTIFICATION: u32 = iow(b'c', 15, size_of::<binder_handle_cookie>());
pub const BC_DEAD_BINDER_DONE: u32 = iow(b'c', 16, 8);
pub const BC_TRANSACTION_SG: u32 = iow(b'c', 17, size_of::<binder_transaction_data_sg>());
pub const BC_REPLY_SG: u32 = iow(b'c', 18, size_of::<binder_transaction_data_sg>());
pub const BC_REQUEST_FREEZE_NOTIFICATION: u32 = iow(b'c', 19, size_of::<binder_handle_cookie>());
pub const BC_CLEAR_FREEZE_NOTIFICATION: u32 = iow(b'c', 20, size_of::<binder_handle_cookie>());
pub const BC_FREEZE_NOTIFICATION_DONE: u32 = iow(b'c', 21, 8);

/// Whether `cmd` is a `BC_*` command of protocol version 8.
pub fn is_command(cmd: u32) -> bool {
    matches!(
        cmd,
        BC_TRANSACTION
            | BC_REPLY
            | BC_ACQUIRE_RESULT
            | BC_FREE_BUFFER
            | BC_INCREFS
            | BC_ACQUIRE
            | BC_RELEASE
            | BC_DECREFS
            | BC_INCREFS_DONE
            | BC_ACQUIRE_DONE
            | BC_ATTEMPT_ACQUIRE
            | BC_REGISTER_LOOPER
            | BC_ENTER_LOOPER
            | BC_EXIT_LOOPER
            | BC_REQUEST_DEATH_NOTIFICATION
            | BC_CLEAR_DEATH_NOTIFICATION
            | BC_DEAD_BINDER_DONE
            | BC_TRANSACTION_SG
            | BC_REPLY_SG
            | BC_REQUEST_FREEZE_NOTIFICATION
            | BC_CLEAR_FREEZE_NOTIFICATION
            | BC_FREEZE_NOTIFICATION_DONE
    )
}

/// Reads a plain value from `b` at `off` (unaligned), if it fits.
pub fn read_at<T: Pod>(b: &[u8], off: usize) -> Option<T> {
    let end = off.checked_add(size_of::<T>())?;
    let src = b.get(off..end)?;
    // SAFETY: `src` holds exactly `size_of::<T>()` bytes and `T: Pod`
    // accepts any bit pattern.
    Some(unsafe { core::ptr::read_unaligned(src.as_ptr().cast::<T>()) })
}

/// Appends the bytes of a plain value.
pub fn push<T: Pod>(out: &mut Vec<u8>, v: &T) {
    // SAFETY: `T: Pod` has no padding that would expose uninitialized
    // bytes (every implementor below is padding free) and is valid to read
    // as bytes.
    let b = unsafe { core::slice::from_raw_parts((v as *const T).cast::<u8>(), size_of::<T>()) };
    out.extend_from_slice(b);
}

/// Writes a plain value into `b` at `off` (unaligned), if it fits.
pub fn write_at<T: Pod>(b: &mut [u8], off: usize, v: &T) -> bool {
    let Some(end) = off.checked_add(size_of::<T>()) else {
        return false;
    };
    let Some(dst) = b.get_mut(off..end) else {
        return false;
    };
    // SAFETY: `dst` holds exactly `size_of::<T>()` bytes.
    unsafe { core::ptr::write_unaligned(dst.as_mut_ptr().cast::<T>(), *v) };
    true
}

/// Plain data: any bit pattern is a valid value, and the type has no
/// padding bytes.
///
/// # Safety
///
/// Implement only for `repr(C)` types made of integers without padding.
pub unsafe trait Pod: Copy {}

// SAFETY: integers.
unsafe impl Pod for u32 {}
// SAFETY: integers.
unsafe impl Pod for i32 {}
// SAFETY: integers.
unsafe impl Pod for u64 {}
// SAFETY: repr(C), integer fields, no padding (asserted sizes above).
unsafe impl Pod for flat_binder_object {}
// SAFETY: as above.
unsafe impl Pod for binder_write_read {}
// SAFETY: as above.
unsafe impl Pod for binder_transaction_data {}
// SAFETY: as above.
unsafe impl Pod for binder_transaction_data_secctx {}
// SAFETY: as above.
unsafe impl Pod for binder_transaction_data_sg {}
// SAFETY: as above.
unsafe impl Pod for binder_ptr_cookie {}
// SAFETY: packed, no padding.
unsafe impl Pod for binder_handle_cookie {}
// SAFETY: as above.
unsafe impl Pod for binder_pri_desc {}
// SAFETY: as above.
unsafe impl Pod for binder_version {}
// SAFETY: as above.
unsafe impl Pod for binder_node_debug_info {}
// SAFETY: as above.
unsafe impl Pod for binder_extended_error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_linux() {
        // Values from a 64-bit Linux build of binder.h.
        assert_eq!(BINDER_WRITE_READ, 0xc030_6201);
        assert_eq!(BINDER_SET_MAX_THREADS, 0x4004_6205);
        assert_eq!(BINDER_SET_CONTEXT_MGR, 0x4004_6207);
        assert_eq!(BINDER_THREAD_EXIT, 0x4004_6208);
        assert_eq!(BINDER_VERSION, 0xc004_6209);
        assert_eq!(BINDER_SET_CONTEXT_MGR_EXT, 0x4018_620d);
        assert_eq!(BINDER_ENABLE_ONEWAY_SPAM_DETECTION, 0x4004_6210);
        assert_eq!(BC_TRANSACTION, 0x4040_6300);
        assert_eq!(BC_REPLY, 0x4040_6301);
        assert_eq!(BC_FREE_BUFFER, 0x4008_6303);
        assert_eq!(BC_ENTER_LOOPER, 0x0000_630c);
        assert_eq!(BC_REQUEST_DEATH_NOTIFICATION, 0x400c_630e);
        assert_eq!(BC_TRANSACTION_SG, 0x4048_6311);
        assert_eq!(BR_TRANSACTION, 0x8040_7202);
        assert_eq!(BR_TRANSACTION_SEC_CTX, 0x8048_7202);
        assert_eq!(BR_REPLY, 0x8040_7203);
        assert_eq!(BR_NOOP, 0x0000_720c);
        assert_eq!(BR_DEAD_BINDER, 0x8008_720f);
        assert_eq!(BINDER_TYPE_BINDER, 0x7362_2a85);
        assert_eq!(BINDER_TYPE_HANDLE, 0x7368_2a85);
        assert_eq!(BINDER_TYPE_FD, 0x6664_2a85);
        assert_eq!(ioc_size(BC_TRANSACTION), 64);
    }
}
