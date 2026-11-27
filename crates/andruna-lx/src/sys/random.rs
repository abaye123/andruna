//! The Windows system random number generator.

use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};

use crate::errno::{Errno, SysResult};

/// Fills `buf` with random bytes from the system generator.
pub fn fill(buf: &mut [u8]) -> SysResult<()> {
    for chunk in buf.chunks_mut(u32::MAX as usize) {
        // SAFETY: `chunk` is a valid writable buffer; the system-preferred
        // generator needs no algorithm handle.
        let st = unsafe { BCryptGenRandom(None, chunk, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        if st.is_err() {
            return Err(Errno::from_ntstatus(st));
        }
    }
    Ok(())
}
