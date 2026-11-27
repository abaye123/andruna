//! The guest side of the write and read streams (see [`crate::broker`]):
//! packing a `BC_*` stream for the broker, and installing the file
//! descriptors of a received `BR_*` stream.

use crate::broker::alloc::align8;
use crate::broker::errno::*;
use crate::uapi::*;

const OBJ_SIZE: usize = core::mem::size_of::<flat_binder_object>();

/// A packed write stream.
#[derive(Debug, Default)]
pub struct Packed {
    pub bytes: Vec<u8>,
    /// For each command: (end in `bytes`, end in the original stream).
    ends: Vec<(usize, usize)>,
}

impl Packed {
    /// The bytes of the original stream that correspond to `packed`
    /// consumed bytes (only whole commands are ever consumed).
    pub fn original_consumed(&self, packed: usize) -> usize {
        self.ends
            .iter()
            .take_while(|(p, _)| *p <= packed)
            .last()
            .map_or(0, |(_, o)| *o)
    }
}

/// Packs the guest's write stream `orig`: the data and offsets of each
/// transaction are copied inline, and each file descriptor object gets the
/// Windows handle `fd_handle` gives for its descriptor (0 when it has
/// none, which the broker refuses).
///
/// # Safety
///
/// The data and offsets pointers of the transactions in `orig` must be
/// readable for their sizes (they are the calling process's own memory, as
/// for the kernel's `copy_from_user`).
pub unsafe fn pack(
    orig: &[u8],
    fd_handle: &mut dyn FnMut(i32) -> Option<u32>,
) -> Result<Packed, (i32, usize)> {
    let mut out = Packed::default();
    let mut pos = 0usize;
    while pos < orig.len() {
        let start = pos;
        let cmd: u32 = read_at(orig, pos).ok_or((EFAULT, start))?;
        if !is_command(cmd) {
            return Err((EINVAL, start));
        }
        let size = ioc_size(cmd);
        let end = pos + 4 + size;
        if end > orig.len() {
            return Err((EFAULT, start));
        }
        out.bytes.extend_from_slice(&orig[pos..end]);
        pos = end;
        if matches!(
            cmd,
            BC_TRANSACTION | BC_REPLY | BC_TRANSACTION_SG | BC_REPLY_SG
        ) {
            let tr: binder_transaction_data = read_at(orig, start + 4).unwrap();
            let ds = usize::try_from(tr.data_size).map_err(|_| (EINVAL, start))?;
            let os = usize::try_from(tr.offsets_size).map_err(|_| (EINVAL, start))?;
            if ds > 0x4000_0000 || os > 0x4000_0000 {
                return Err((EINVAL, start));
            }
            let at = out.bytes.len();
            let dlen = align8(ds).unwrap();
            out.bytes.resize(at + dlen + align8(os).unwrap(), 0);
            if ds > 0 {
                // SAFETY: the caller's readable data of `ds` bytes
                // (contract); the destination was just sized.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        tr.buffer as usize as *const u8,
                        out.bytes.as_mut_ptr().add(at),
                        ds,
                    )
                };
            }
            if os > 0 {
                // SAFETY: as above, for the offsets.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        tr.offsets as usize as *const u8,
                        out.bytes.as_mut_ptr().add(at + dlen),
                        os,
                    )
                };
            }
            // File descriptors become Windows handles.
            for i in 0..os / 8 {
                let o: u64 = read_at(&out.bytes, at + dlen + i * 8).unwrap();
                let Ok(o) = usize::try_from(o) else { continue };
                if o.checked_add(OBJ_SIZE).is_none_or(|e| e > ds) {
                    continue;
                }
                let mut obj: flat_binder_object = read_at(&out.bytes, at + o).unwrap();
                if obj.hdr_type == BINDER_TYPE_FD {
                    let fd = obj.binder as u32 as i32;
                    obj.binder = u64::from(fd_handle(fd).unwrap_or(0));
                    write_at(&mut out.bytes, at + o, &obj);
                }
            }
        }
        out.ends.push((out.bytes.len(), pos));
    }
    Ok(out)
}

/// Installs the file descriptors of the transactions in the received
/// stream `read`: each file descriptor object holds a Windows handle the
/// broker duplicated into this process, which `install` turns into a
/// descriptor (or -1); the descriptor is written into the object.
///
/// # Safety
///
/// The buffers the stream points to must be this process's writable
/// receive buffer, as delivered by the broker.
pub unsafe fn install_fds(read: &[u8], install: &mut dyn FnMut(u32) -> i32) {
    let mut pos = 0usize;
    while let Some(cmd) = read_at::<u32>(read, pos) {
        let size = ioc_size(cmd);
        if matches!(cmd, BR_TRANSACTION | BR_TRANSACTION_SEC_CTX | BR_REPLY)
            && let Some(tr) = read_at::<binder_transaction_data>(read, pos + 4)
        {
            let n = tr.offsets_size as usize / 8;
            for i in 0..n {
                let op = (tr.offsets as usize + i * 8) as *const u64;
                // SAFETY: the offsets array inside the receive buffer
                // (contract).
                let o = unsafe { op.read_unaligned() } as usize;
                if o.checked_add(OBJ_SIZE)
                    .is_none_or(|e| e > tr.data_size as usize)
                {
                    continue;
                }
                let objp = (tr.buffer as usize + o) as *mut flat_binder_object;
                // SAFETY: inside the delivered data (checked above).
                let mut obj = unsafe { objp.read_unaligned() };
                if obj.hdr_type == BINDER_TYPE_FD {
                    let fd = install(obj.binder as u32);
                    obj.binder = u64::from(fd as u32);
                    // SAFETY: as above; the buffer is writable here.
                    unsafe { objp.write_unaligned(obj) };
                }
            }
        }
        pos += 4 + size;
    }
}
