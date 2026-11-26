//! `WaitOnAddress` / `WakeByAddress*`.

use core::ffi::c_void;
use core::sync::atomic::AtomicU32;

use windows::Win32::Foundation::ERROR_TIMEOUT;
use windows::Win32::System::Threading::{WaitOnAddress, WakeByAddressAll, WakeByAddressSingle};

/// Result of [`wait_on_address`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// Woken, the value differed, or a spurious wake-up.
    Woken,
    TimedOut,
}

/// Blocks while `*word == expected`, for at most `timeout_ms` (`None`:
/// without limit).
pub fn wait_on_address(word: &AtomicU32, expected: u32, timeout_ms: Option<u32>) -> Wait {
    // SAFETY: both pointers are valid for 4-byte reads for the duration of
    // the call (`word` is a live reference, `expected` a local).
    let r = unsafe {
        WaitOnAddress(
            word.as_ptr() as *const c_void,
            &expected as *const u32 as *const c_void,
            4,
            Some(timeout_ms.unwrap_or(u32::MAX)),
        )
    };
    match r {
        Ok(()) => Wait::Woken,
        Err(e) if e.code() == ERROR_TIMEOUT.to_hresult() => Wait::TimedOut,
        // No other failure is documented; treat it as a spurious wake-up,
        // which callers handle anyway.
        Err(_) => Wait::Woken,
    }
}

/// Wakes one thread waiting on `addr`.
pub fn wake_one(addr: usize) {
    // SAFETY: the address is only used as a key; it is not dereferenced.
    unsafe { WakeByAddressSingle(addr as *const c_void) }
}

/// Wakes every thread waiting on `addr`.
pub fn wake_all(addr: usize) {
    // SAFETY: the address is only used as a key; it is not dereferenced.
    unsafe { WakeByAddressAll(addr as *const c_void) }
}
