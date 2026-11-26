//! Small helpers shared by the modules.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Locks a mutex, ignoring poisoning. lx state stays consistent across a
/// panic because every export catches panics at the boundary and each
/// operation updates its tables only after the Windows call succeeded.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn read<T>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(PoisonError::into_inner)
}

pub fn write<T>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(PoisonError::into_inner)
}

/// A buffer supplied by a caller of an lx export.
///
/// lx never dereferences it. It is handed to Windows, which validates user
/// buffers itself and fails the I/O (`EFAULT`) when they are not accessible,
/// as the Linux kernel does.
#[derive(Clone, Copy, Debug)]
pub struct UserBuf {
    ptr: *mut u8,
    len: usize,
}

impl UserBuf {
    /// # Safety
    ///
    /// No Rust reference may alias `[ptr, ptr+len)` while the buffer is in
    /// use: Windows writes into it on reads. An inaccessible range is not
    /// undefined behavior; the I/O then fails with `EFAULT`.
    pub unsafe fn new(ptr: *mut u8, len: usize) -> Self {
        Self { ptr, len }
    }

    pub fn from_mut(buf: &mut [u8]) -> Self {
        Self {
            ptr: buf.as_mut_ptr(),
            len: buf.len(),
        }
    }

    pub fn from_ref(buf: &[u8]) -> Self {
        // Only used for writes, where Windows reads from the buffer.
        Self {
            ptr: buf.as_ptr() as *mut u8,
            len: buf.len(),
        }
    }

    pub fn ptr(&self) -> *mut u8 {
        self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}
