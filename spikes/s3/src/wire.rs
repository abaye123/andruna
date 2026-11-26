//! Handshake messages exchanged over the bootstrap pipe.

/// Client to broker. Nothing in it is trusted for identity.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Hello {
    pub magic: u32,
    pub claimed_pid: u32,
    pub role: u32,
    pub _r: u32,
}

/// Broker to client. Handle values are already valid in the client.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Welcome {
    pub magic: u32,
    pub id: u32,
    pub uid: u32,
    pub cpu: u32,
    pub broker_pid: u32,
    pub _r: u32,
    pub section: u64,
    pub sec_size: u64,
    pub broker_proc: u64,
    pub events: [u64; 4],
}

/// Plain-old-data marker for the types this module serializes.
///
/// # Safety
/// Implementors must be `repr(C)` with no padding-sensitive invariants and
/// valid for any bit pattern.
pub unsafe trait Pod: Copy + Default {}
// SAFETY: repr(C) integers only.
unsafe impl Pod for Hello {}
// SAFETY: repr(C) integers only.
unsafe impl Pod for Welcome {}
// SAFETY: integer arrays.
unsafe impl Pod for [u64; 3] {}
// SAFETY: integer arrays.
unsafe impl Pod for [u64; 4] {}

pub fn to_bytes<T: Pod>(t: &T) -> Vec<u8> {
    // SAFETY: T is Pod; we read exactly size_of::<T>() bytes from it.
    unsafe {
        std::slice::from_raw_parts(t as *const T as *const u8, std::mem::size_of::<T>()).to_vec()
    }
}

pub fn from_bytes<T: Pod>(b: &[u8]) -> Option<T> {
    if b.len() < std::mem::size_of::<T>() {
        return None;
    }
    // SAFETY: length checked; T is valid for any bit pattern; unaligned read.
    Some(unsafe { std::ptr::read_unaligned(b.as_ptr() as *const T) })
}
