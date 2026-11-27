//! A zip reader for APKs.
//!
//! Only what an APK may contain is accepted, with the checks Android's
//! own zip code and signature verifier apply:
//!
//! - exactly one end of central directory record, whose comment reaches the
//!   end of the file; no zip64;
//! - the central directory ends where the end record starts, so nothing can
//!   hide between them (the APK Signing Block lies before the central
//!   directory and is found by its position);
//! - stored (0) or deflated (8) entries only, no encryption;
//! - each local header matches its central directory entry, local records
//!   do not overlap and lie before the central directory;
//! - entry names are unique UTF-8 without NUL ([`is_safe_name`] is checked
//!   by whoever extracts an entry to a file);
//! - sizes and CRC-32 are checked when an entry is read.
//!
//! The archive borrows its bytes; [`crate::sys::MappedFile`] maps an APK
//! file read-only for it.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::error::{Code, Result, err};

const EOCD_SIG: u32 = 0x0605_4b50;
const CD_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
const EOCD_LEN: usize = 22;
const CD_LEN: usize = 46;
const LOCAL_LEN: usize = 30;

/// Compression methods an APK may use.
pub const STORED: u16 = 0;
pub const DEFLATED: u16 = 8;

/// One entry of the central directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub method: u16,
    pub flags: u16,
    pub crc32: u32,
    pub compressed_size: u64,
    pub size: u64,
    /// Offset of the local file header.
    pub local_offset: u64,
    /// Offset of the entry's data (after the local header, name and extra).
    pub data_offset: u64,
}

impl Entry {
    /// Offset just past the entry's (compressed) data.
    pub fn data_end(&self) -> u64 {
        self.data_offset + self.compressed_size
    }

    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }
}

/// A parsed archive over borrowed bytes.
#[derive(Debug)]
pub struct Archive<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
    by_name: HashMap<String, usize>,
    cd_offset: u64,
    eocd_offset: u64,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn bad<T>(m: impl Into<String>) -> Result<T> {
    err(Code::InvalidArchive, m)
}

/// Checks that an entry name is a safe relative path.
pub fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !name.contains(':')
        && name.split('/').all(|s| s != ".." && s != ".")
}

/// Finds the end of central directory record: the last signature whose
/// comment length reaches exactly the end of the file.
fn find_eocd(data: &[u8]) -> Result<usize> {
    if data.len() < EOCD_LEN {
        return bad("file is too small to be a zip archive");
    }
    let lowest = data.len().saturating_sub(EOCD_LEN + u16::MAX as usize);
    let mut pos = data.len() - EOCD_LEN;
    loop {
        if u32_at(data, pos) == Some(EOCD_SIG) {
            let comment = u16_at(data, pos + 20).unwrap_or(0) as usize;
            if pos + EOCD_LEN + comment == data.len() {
                return Ok(pos);
            }
        }
        if pos == lowest {
            return bad("no end of central directory record (not a zip archive)");
        }
        pos -= 1;
    }
}

impl<'a> Archive<'a> {
    /// Parses the central directory and all local headers.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let eocd = find_eocd(data)?;
        if eocd >= 20 && u32_at(data, eocd - 20) == Some(ZIP64_LOCATOR_SIG) {
            return bad("zip64 archives are not supported");
        }
        let disk = u16_at(data, eocd + 4).unwrap_or(1);
        let cd_disk = u16_at(data, eocd + 6).unwrap_or(1);
        let n_disk = u16_at(data, eocd + 8).unwrap_or(0);
        let n_total = u16_at(data, eocd + 10).unwrap_or(0);
        let cd_size = u32_at(data, eocd + 12).unwrap_or(0) as u64;
        let cd_offset = u32_at(data, eocd + 16).unwrap_or(0) as u64;
        if disk != 0 || cd_disk != 0 || n_disk != n_total {
            return bad("multi-disk archives are not supported");
        }
        if n_total == u16::MAX || cd_size == u32::MAX as u64 || cd_offset == u32::MAX as u64 {
            return bad("zip64 archives are not supported");
        }
        if cd_offset + cd_size != eocd as u64 {
            return bad(format!(
                "central directory ({cd_offset} + {cd_size}) does not end at the end record ({eocd})"
            ));
        }
        let mut entries = Vec::with_capacity(n_total as usize);
        let mut by_name = HashMap::with_capacity(n_total as usize);
        let mut p = cd_offset as usize;
        for i in 0..n_total {
            if u32_at(data, p) != Some(CD_SIG) || p + CD_LEN > eocd {
                return bad(format!("central directory entry {i} is damaged"));
            }
            let flags = u16_at(data, p + 8).unwrap_or(0);
            let method = u16_at(data, p + 10).unwrap_or(0);
            let crc32 = u32_at(data, p + 16).unwrap_or(0);
            let csize = u32_at(data, p + 20).unwrap_or(0) as u64;
            let size = u32_at(data, p + 24).unwrap_or(0) as u64;
            let name_len = u16_at(data, p + 28).unwrap_or(0) as usize;
            let extra_len = u16_at(data, p + 30).unwrap_or(0) as usize;
            let comment_len = u16_at(data, p + 32).unwrap_or(0) as usize;
            let local = u32_at(data, p + 42).unwrap_or(0) as u64;
            let next = p + CD_LEN + name_len + extra_len + comment_len;
            if next > eocd {
                return bad(format!(
                    "central directory entry {i} runs past the directory"
                ));
            }
            let raw_name = &data[p + CD_LEN..p + CD_LEN + name_len];
            let name = match std::str::from_utf8(raw_name) {
                Ok(n) => n.to_string(),
                Err(_) => return bad(format!("entry {i} has a name that is not UTF-8")),
            };
            if name.is_empty() || name.contains('\0') {
                return bad(format!("entry {i} has an empty name or a name with NUL"));
            }
            if flags & 1 != 0 {
                return bad(format!("entry `{name}` is encrypted"));
            }
            if method != STORED && method != DEFLATED {
                return bad(format!("entry `{name}` uses compression method {method}"));
            }
            if csize == u32::MAX as u64 || size == u32::MAX as u64 || local == u32::MAX as u64 {
                return bad("zip64 archives are not supported");
            }
            if method == STORED && csize != size {
                return bad(format!("stored entry `{name}` has different sizes"));
            }
            // Local header.
            let lp = local as usize;
            if local >= cd_offset || u32_at(data, lp) != Some(LOCAL_SIG) {
                return bad(format!("local header of `{name}` is missing"));
            }
            let l_name_len = u16_at(data, lp + 26).unwrap_or(0) as usize;
            let l_extra_len = u16_at(data, lp + 28).unwrap_or(0) as usize;
            let l_method = u16_at(data, lp + 8).unwrap_or(0);
            if data.get(lp + LOCAL_LEN..lp + LOCAL_LEN + l_name_len) != Some(raw_name) {
                return bad(format!("local header of `{name}` names another file"));
            }
            if l_method != method {
                return bad(format!(
                    "local header of `{name}` has another compression method"
                ));
            }
            let data_offset = local + (LOCAL_LEN + l_name_len + l_extra_len) as u64;
            if data_offset + csize > cd_offset {
                return bad(format!("data of `{name}` runs into the central directory"));
            }
            if by_name.insert(name.clone(), entries.len()).is_some() {
                return bad(format!("entry `{name}` appears twice"));
            }
            entries.push(Entry {
                name,
                method,
                flags,
                crc32,
                compressed_size: csize,
                size,
                local_offset: local,
                data_offset,
            });
            p = next;
        }
        if p != eocd {
            return bad("central directory has trailing bytes");
        }
        // Local records must not overlap (overlapping entries are a zip
        // bomb and a way to show different content to different readers).
        let mut spans: Vec<(u64, u64)> = entries
            .iter()
            .map(|e| (e.local_offset, e.data_end()))
            .collect();
        spans.sort_unstable();
        for w in spans.windows(2) {
            if w[1].0 < w[0].1 {
                return bad("entries overlap");
            }
        }
        Ok(Self {
            data,
            entries,
            by_name,
            cd_offset,
            eocd_offset: eocd as u64,
        })
    }

    /// The whole file.
    pub fn bytes(&self) -> &'a [u8] {
        self.data
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.by_name.get(name).map(|&i| &self.entries[i])
    }

    /// Offset of the central directory.
    pub fn cd_offset(&self) -> u64 {
        self.cd_offset
    }

    /// Offset of the end of central directory record.
    pub fn eocd_offset(&self) -> u64 {
        self.eocd_offset
    }

    /// End of the last local record (where an APK Signing Block may start).
    pub fn entries_end(&self) -> u64 {
        self.entries.iter().map(Entry::data_end).max().unwrap_or(0)
    }

    /// The entry's data as stored (compressed or not).
    pub fn raw(&self, e: &Entry) -> &'a [u8] {
        &self.data[e.data_offset as usize..e.data_end() as usize]
    }

    /// The entry's uncompressed contents, size and CRC-32 checked. `limit`
    /// bounds the uncompressed size accepted.
    pub fn read_limited(&self, e: &Entry, limit: u64) -> Result<Cow<'a, [u8]>> {
        if e.size > limit {
            return bad(format!("entry `{}` is larger than {limit} bytes", e.name));
        }
        let raw = self.raw(e);
        let out: Cow<'a, [u8]> = match e.method {
            STORED => Cow::Borrowed(raw),
            _ => {
                let v = miniz_oxide::inflate::decompress_to_vec_with_limit(raw, e.size as usize)
                    .map_err(|_| {
                        crate::Error::new(
                            Code::InvalidArchive,
                            format!("entry `{}` does not inflate to its size", e.name),
                        )
                    })?;
                Cow::Owned(v)
            }
        };
        if out.len() as u64 != e.size {
            return bad(format!("entry `{}` has the wrong size", e.name));
        }
        if crc32fast::hash(&out) != e.crc32 {
            return bad(format!("entry `{}` fails its CRC-32 check", e.name));
        }
        Ok(out)
    }

    /// [`Self::read_limited`] with a 1 GiB bound.
    pub fn read(&self, e: &Entry) -> Result<Cow<'a, [u8]>> {
        self.read_limited(e, 1 << 30)
    }

    /// Reads an entry by name; `Ok(None)` when it does not exist.
    pub fn read_name(&self, name: &str) -> Result<Option<Cow<'a, [u8]>>> {
        match self.find(name) {
            Some(e) => self.read(e).map(Some),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::zipw::{ZipEntry, ZipWriter};

    fn sample() -> Vec<u8> {
        let mut w = ZipWriter::new();
        w.add(ZipEntry::stored("AndroidManifest.xml", b"manifest".to_vec()).align(4));
        w.add(ZipEntry::deflated("classes.dex", vec![7u8; 5000]));
        w.add(ZipEntry::stored("lib/x86_64/libx.so", vec![1u8; 100]).align(4096));
        w.finish()
    }

    #[test]
    fn reads_stored_and_deflated() {
        let data = sample();
        let a = Archive::parse(&data).unwrap();
        assert_eq!(a.entries().len(), 3);
        assert_eq!(
            &*a.read_name("AndroidManifest.xml").unwrap().unwrap(),
            b"manifest"
        );
        assert_eq!(
            &*a.read_name("classes.dex").unwrap().unwrap(),
            &[7u8; 5000][..]
        );
        let so = a.find("lib/x86_64/libx.so").unwrap();
        assert_eq!(so.data_offset % 4096, 0);
        assert_eq!(a.find("AndroidManifest.xml").unwrap().data_offset % 4, 0);
        assert!(a.read_name("missing").unwrap().is_none());
    }

    #[test]
    fn rejects_damage() {
        let data = sample();
        // Truncations never panic and never parse.
        for cut in [0, 10, data.len() - 1, data.len() - 22] {
            assert!(Archive::parse(&data[..cut]).is_err(), "cut {cut}");
        }
        // A flipped byte inside deflated data fails inflate or CRC.
        let a = Archive::parse(&data).unwrap();
        let dex = a.find("classes.dex").unwrap().clone();
        let mut bad = data.clone();
        bad[dex.data_offset as usize + 3] ^= 0x55;
        let b = Archive::parse(&bad).unwrap();
        assert!(b.read(&dex).is_err());
        // Bytes appended after the end record.
        let mut tail = data.clone();
        tail.push(0);
        assert!(Archive::parse(&tail).is_err());
    }

    #[test]
    fn rejects_unsafe_and_duplicate_names() {
        for name in ["../x", "a/../b", "/abs", "a\\b", "c:x", "a/./b", ""] {
            assert!(!is_safe_name(name), "{name}");
        }
        assert!(is_safe_name("lib/x86_64/libx.so"));
        let mut w = ZipWriter::new();
        w.add(ZipEntry::stored("a\0b", b"x".to_vec()));
        assert!(Archive::parse(&w.finish()).is_err());
        let mut w = ZipWriter::new();
        w.add(ZipEntry::stored("a", b"1".to_vec()));
        w.add(ZipEntry::stored("a", b"2".to_vec()));
        assert!(Archive::parse(&w.finish()).is_err());
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let base = sample();
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        for _ in 0..3000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let mut d = base.clone();
            let n = (x % 6) as usize + 1;
            for k in 0..n {
                let pos = ((x >> (k * 8)) as usize).wrapping_mul(2654435761) % d.len();
                d[pos] ^= (x >> 32) as u8 | 1;
            }
            if let Ok(a) = Archive::parse(&d) {
                for e in a.entries().to_vec() {
                    let _ = a.read(&e);
                }
            }
        }
    }
}
