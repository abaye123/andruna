//! `__errno`.

use std::panic::catch_unwind;

/// bionic `int* __errno(void)`: the address of the calling thread's
/// `errno`.
#[unsafe(no_mangle)]
pub extern "C" fn __errno() -> *mut i32 {
    // The cell is const-initialized and has no destructor, so this cannot
    // fail; the guard only keeps the boundary rule.
    catch_unwind(crate::errno::errno_location).unwrap_or(core::ptr::null_mut())
}
