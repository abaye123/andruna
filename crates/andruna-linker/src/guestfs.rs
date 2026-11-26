//! Objects opened by guest path, through andruna-lx's file system.

use andruna_lx::linux::fcntl::{AT_FDCWD, O_CLOEXEC, O_RDONLY};

use crate::error::{Error, Result};
use crate::image::SegmentSource;

#[cfg(target_arch = "x86_64")]
const SYS_PREAD64: i64 = 17;
#[cfg(target_arch = "aarch64")]
const SYS_PREAD64: i64 = 67;

/// Whether `path` names a file in the guest file system.
pub fn exists(path: &str) -> bool {
    andruna_lx::fs::fstatat(AT_FDCWD, path.as_bytes(), 0).is_ok()
}

/// A file opened through lx; segments can be file-mapped by lx when it
/// supports it, otherwise they are read with `pread`.
pub struct GuestFile {
    fd: i32,
}

impl GuestFile {
    pub fn open(path: &str) -> Result<Self> {
        let fd = andruna_lx::fs::openat(AT_FDCWD, path.as_bytes(), O_RDONLY | O_CLOEXEC, 0)
            .map_err(|e| Error::new(format!("cannot open \"{path}\": errno {}", e.0)))?;
        Ok(Self { fd })
    }
}

impl Drop for GuestFile {
    fn drop(&mut self) {
        let _ = andruna_lx::fs::close(self.fd);
    }
}

impl SegmentSource for GuestFile {
    unsafe fn copy_to(&mut self, off: u64, len: u64, dst: *mut u8) -> Result<()> {
        let mut done = 0u64;
        while done < len {
            // SAFETY: `dst + done` is writable for `len - done` bytes
            // (caller's contract).
            let n = unsafe {
                crate::syscall::lx_raw(
                    SYS_PREAD64,
                    [
                        i64::from(self.fd),
                        dst.add(done as usize) as i64,
                        (len - done) as i64,
                        (off + done) as i64,
                        0,
                        0,
                    ],
                )
            };
            if n <= 0 {
                return Err(Error::new(format!("read failed: {}", if n == 0 { "end of file".to_string() } else { format!("errno {}", -n) })));
            }
            done += n as u64;
        }
        Ok(())
    }

    fn lx_fd(&self) -> Option<i32> {
        Some(self.fd)
    }
}
