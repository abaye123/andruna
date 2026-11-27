//! File objects backed by Windows file handles: regular files and
//! directories.

use std::sync::Mutex;

use crate::errno::{Errno, SysResult};
use crate::fd::{FileOps, OpenFile};
use crate::linux::fcntl::*;
use crate::linux::stat::*;
use crate::linux::time::Timespec;
use crate::process;
use crate::sys::OwnedHandle;
use crate::sys::file::{self as os, REPARSE_TAG_LX_SYMLINK, REPARSE_TAG_SYMLINK, WriteAt};

use crate::sys::time::UNIX_EPOCH_AS_FILETIME;
use crate::util::{UserBuf, lock};

/// `FILE_ATTRIBUTE_READONLY`.
const ATTR_READONLY: u32 = 0x1;

/// A regular file. The offset is kept by lx, so positional I/O never moves
/// it (Windows moves a synchronous handle's position on every transfer).
#[derive(Debug)]
pub struct RegularFile {
    handle: OwnedHandle,
    pos: Mutex<u64>,
    /// The Linux path it was opened by (empty if unknown).
    path: Vec<u8>,
}

impl RegularFile {
    pub fn new(handle: OwnedHandle) -> Self {
        Self::with_path(handle, Vec::new())
    }

    pub fn with_path(handle: OwnedHandle, path: Vec<u8>) -> Self {
        Self {
            handle,
            pos: Mutex::new(0),
            path,
        }
    }

    fn size(&self) -> SysResult<u64> {
        Ok(os::info(self.handle.raw())?.size as u64)
    }
}

impl FileOps for RegularFile {
    fn read(&self, _file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        // Holding the offset lock across the transfer serializes reads and
        // writes on one description, as Linux does with `f_pos_lock`.
        let mut pos = lock(&self.pos);
        let n = os::read_at(self.handle.raw(), buf, *pos)?;
        *pos += n as u64;
        Ok(n)
    }

    fn write(&self, file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        let mut pos = lock(&self.pos);
        if file.status_flags() & O_APPEND != 0 {
            let n = os::write_at(self.handle.raw(), buf, WriteAt::End)?;
            *pos = self.size()?;
            Ok(n)
        } else {
            let n = os::write_at(self.handle.raw(), buf, WriteAt::Offset(*pos))?;
            *pos += n as u64;
            Ok(n)
        }
    }

    fn pread(&self, buf: UserBuf, offset: u64) -> SysResult<usize> {
        os::read_at(self.handle.raw(), buf, offset)
    }

    fn pwrite(&self, file: &OpenFile, buf: UserBuf, offset: u64) -> SysResult<usize> {
        // Linux appends even for pwrite when O_APPEND is set.
        let at = if file.status_flags() & O_APPEND != 0 {
            WriteAt::End
        } else {
            WriteAt::Offset(offset)
        };
        os::write_at(self.handle.raw(), buf, at)
    }

    fn seek(&self, offset: i64, whence: i32) -> SysResult<u64> {
        let mut pos = lock(&self.pos);
        let new = match whence {
            SEEK_SET => offset,
            SEEK_CUR => (*pos as i64).checked_add(offset).ok_or(Errno::EOVERFLOW)?,
            SEEK_END => (self.size()? as i64)
                .checked_add(offset)
                .ok_or(Errno::EOVERFLOW)?,
            // The file has no holes as far as lx reports: all data up to
            // the end, one hole at the end.
            SEEK_DATA | SEEK_HOLE => {
                let size = self.size()?;
                let off = u64::try_from(offset).map_err(|_| Errno::ENXIO)?;
                if off >= size {
                    return Err(Errno::ENXIO);
                }
                if whence == SEEK_DATA {
                    offset
                } else {
                    size as i64
                }
            }
            _ => return Err(Errno::EINVAL),
        };
        let new = u64::try_from(new).map_err(|_| Errno::EINVAL)?;
        *pos = new;
        Ok(new)
    }

    fn stat(&self) -> SysResult<StatData> {
        stat_of(&self.handle)
    }

    fn truncate(&self, len: u64) -> SysResult<()> {
        os::set_len(self.handle.raw(), len)
    }

    /// A handle without write access cannot be flushed on Windows; Linux
    /// allows `fsync` on any descriptor, so that case succeeds (nothing was
    /// written through it).
    fn sync(&self) -> SysResult<()> {
        match os::flush(self.handle.raw()) {
            Err(Errno::EACCES) | Err(Errno::EBADF) => Ok(()),
            r => r,
        }
    }

    fn mappable(&self) -> bool {
        true
    }

    fn path(&self) -> Option<Vec<u8>> {
        (!self.path.is_empty()).then(|| self.path.clone())
    }
}

impl RegularFile {
    /// The raw handle, for lx's own use.
    pub fn handle(&self) -> &OwnedHandle {
        &self.handle
    }
}

/// A directory. Its Linux path is kept for `*at` calls relative to it.
///
/// `getdents64` reads the whole listing when it starts at offset 0 and
/// returns it from that snapshot; the offset is the index of the next
/// entry. `.` and `..` come first, as Linux file systems report them.
#[derive(Debug)]
pub struct Directory {
    handle: OwnedHandle,
    path: Vec<u8>,
    pos: Mutex<(u64, Option<Vec<DirEntry>>)>,
}

/// One entry of a listing: inode, `d_type`, name.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub ino: u64,
    pub kind: u8,
    pub name: Vec<u8>,
}

pub const DT_DIR: u8 = 4;
pub const DT_REG: u8 = 8;
pub const DT_LNK: u8 = 10;

impl Directory {
    pub fn new(handle: OwnedHandle, path: Vec<u8>) -> Self {
        Self {
            handle,
            path,
            pos: Mutex::new((0, None)),
        }
    }

    fn list(&self) -> SysResult<Vec<DirEntry>> {
        let me = os::info(self.handle.raw())?.file_id;
        let mut out = vec![
            DirEntry {
                ino: me,
                kind: DT_DIR,
                name: b".".to_vec(),
            },
            DirEntry {
                ino: me,
                kind: DT_DIR,
                name: b"..".to_vec(),
            },
        ];
        for e in os::read_dir(self.handle.raw())? {
            let dot = [u16::from(b'.')];
            let dotdot = [u16::from(b'.'), u16::from(b'.')];
            if e.name == dot || e.name == dotdot {
                if e.name == dotdot {
                    out[1].ino = e.file_id;
                }
                continue;
            }
            let reparse = e.attributes & 0x400 != 0;
            let kind = if reparse && is_symlink_tag(e.reparse_tag) {
                DT_LNK
            } else if e.attributes & 0x10 != 0 {
                DT_DIR
            } else {
                DT_REG
            };
            out.push(DirEntry {
                ino: e.file_id,
                kind,
                name: super::path::decode_component(&e.name),
            });
        }
        Ok(out)
    }
}

impl FileOps for Directory {
    fn read(&self, _file: &OpenFile, _buf: UserBuf) -> SysResult<usize> {
        Err(Errno::EISDIR)
    }

    fn pread(&self, _buf: UserBuf, _offset: u64) -> SysResult<usize> {
        Err(Errno::EISDIR)
    }

    fn seek(&self, offset: i64, whence: i32) -> SysResult<u64> {
        // Offsets are entry indexes; seeking to 0 starts a new listing.
        let mut pos = lock(&self.pos);
        let new = match whence {
            SEEK_SET => offset,
            SEEK_CUR => (pos.0 as i64).checked_add(offset).ok_or(Errno::EINVAL)?,
            _ => return Err(Errno::EINVAL),
        };
        pos.0 = u64::try_from(new).map_err(|_| Errno::EINVAL)?;
        if pos.0 == 0 {
            pos.1 = None;
        }
        Ok(pos.0)
    }

    fn sync(&self) -> SysResult<()> {
        Ok(())
    }

    fn path(&self) -> Option<Vec<u8>> {
        Some(self.path.clone())
    }

    fn getdents(&self, out: &mut Vec<u8>, max: usize) -> SysResult<()> {
        let mut pos = lock(&self.pos);
        if pos.1.is_none() || pos.0 == 0 {
            pos.1 = Some(self.list()?);
        }
        let (p, list) = &mut *pos;
        write_dirents(list.as_ref().expect("listed"), p, out, max)
    }

    fn stat(&self) -> SysResult<StatData> {
        stat_of(&self.handle)
    }

    fn dir_path(&self) -> Option<&[u8]> {
        Some(&self.path)
    }
}

/// Appends `linux_dirent64` records for `entries[*pos..]` to `out`, at most
/// `max` bytes, and advances `pos` (the index of the next entry, also the
/// `d_off` of the previous one). `EINVAL` if not even one record fits.
pub fn write_dirents(
    entries: &[DirEntry],
    pos: &mut u64,
    out: &mut Vec<u8>,
    max: usize,
) -> SysResult<()> {
    let mut i = *pos as usize;
    while let Some(e) = entries.get(i) {
        // d_ino, d_off, d_reclen, d_type, name, NUL, padded to 8.
        let len = (19 + e.name.len() + 1).next_multiple_of(8);
        if out.len() + len > max {
            if out.is_empty() {
                return Err(Errno::EINVAL);
            }
            break;
        }
        let start = out.len();
        out.extend_from_slice(&e.ino.to_le_bytes());
        out.extend_from_slice(&(i as i64 + 1).to_le_bytes());
        out.extend_from_slice(&(len as u16).to_le_bytes());
        out.push(e.kind);
        out.extend_from_slice(&e.name);
        out.resize(start + len, 0);
        i += 1;
    }
    *pos = i as u64;
    Ok(())
}

/// Whether a reparse tag is a symbolic link in the Linux sense.
pub fn is_symlink_tag(tag: u32) -> bool {
    tag == REPARSE_TAG_SYMLINK || tag == REPARSE_TAG_LX_SYMLINK
}

fn filetime_to_timespec(ft: i64) -> Timespec {
    Timespec::from_nanos((i128::from(ft) - i128::from(UNIX_EPOCH_AS_FILETIME)) * 100)
}

/// Converts Windows metadata to `stat` fields.
///
/// Windows does not store Linux permission bits. lx reports 0644 for files
/// (0444 when read-only), 0755 for directories and 0777 for symbolic links,
/// owned by the process's ids ([`process::set_ids`]).
pub fn stat_from_info(info: &os::Info) -> StatData {
    let mode = if is_symlink_tag(info.reparse_tag) {
        S_IFLNK | 0o777
    } else if info.directory {
        S_IFDIR | 0o755
    } else if info.attributes & ATTR_READONLY != 0 {
        S_IFREG | 0o444
    } else {
        S_IFREG | 0o644
    };
    StatData {
        dev: info.volume,
        ino: info.file_id,
        mode,
        nlink: info.links.max(1),
        uid: process::uid(),
        gid: process::gid(),
        rdev: 0,
        size: info.size,
        blksize: 4096,
        blocks: (info.allocation + 511) / 512,
        atime: filetime_to_timespec(info.access),
        mtime: filetime_to_timespec(info.write),
        ctime: filetime_to_timespec(info.change),
    }
}

fn stat_of(h: &OwnedHandle) -> SysResult<StatData> {
    Ok(stat_from_info(&os::info(h.raw())?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_conversion() {
        assert_eq!(
            filetime_to_timespec(UNIX_EPOCH_AS_FILETIME),
            Timespec::default()
        );
        assert_eq!(
            filetime_to_timespec(UNIX_EPOCH_AS_FILETIME + 10_000_001),
            Timespec {
                tv_sec: 1,
                tv_nsec: 100
            }
        );
        assert_eq!(
            filetime_to_timespec(UNIX_EPOCH_AS_FILETIME - 1),
            Timespec {
                tv_sec: -1,
                tv_nsec: 999_999_900
            }
        );
    }

    #[test]
    fn modes() {
        let mut i = os::Info::default();
        assert_eq!(stat_from_info(&i).mode, S_IFREG | 0o644);
        i.attributes = ATTR_READONLY;
        assert_eq!(stat_from_info(&i).mode, S_IFREG | 0o444);
        i.directory = true;
        assert_eq!(stat_from_info(&i).mode, S_IFDIR | 0o755);
        i.reparse_tag = REPARSE_TAG_SYMLINK;
        assert_eq!(stat_from_info(&i).mode, S_IFLNK | 0o777);
        i.allocation = 513;
        assert_eq!(stat_from_info(&i).blocks, 2);
    }
}
