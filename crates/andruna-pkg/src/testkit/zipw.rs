//! A zip writer for test APKs, with zipalign-style alignment.

/// One entry to write.
#[derive(Clone, Debug)]
pub struct ZipEntry {
    pub name: String,
    pub data: Vec<u8>,
    pub deflate: bool,
    /// Alignment of the data offset for stored entries (padding in the
    /// local header's extra field, as zipalign does).
    pub align: u32,
}

impl ZipEntry {
    pub fn stored(name: &str, data: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            data,
            deflate: false,
            align: 1,
        }
    }

    pub fn deflated(name: &str, data: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            data,
            deflate: true,
            align: 1,
        }
    }

    pub fn align(mut self, a: u32) -> Self {
        self.align = a;
        self
    }
}

/// The three parts of an archive, so a signer can put the APK Signing
/// Block between the entries and the central directory.
#[derive(Clone, Debug)]
pub struct ZipParts {
    pub entries: Vec<u8>,
    pub central_directory: Vec<u8>,
    pub eocd: Vec<u8>,
}

impl ZipParts {
    /// Joins the parts, with `block` (for example an APK Signing Block)
    /// between the entries and the central directory, and fixes the
    /// central directory offset in the end record.
    pub fn join_with(&self, block: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            self.entries.len() + block.len() + self.central_directory.len() + self.eocd.len(),
        );
        out.extend_from_slice(&self.entries);
        out.extend_from_slice(block);
        out.extend_from_slice(&self.central_directory);
        let mut eocd = self.eocd.clone();
        let cd_off = (self.entries.len() + block.len()) as u32;
        eocd[16..20].copy_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&eocd);
        out
    }
}

#[derive(Default)]
pub struct ZipWriter {
    entries: Vec<ZipEntry>,
}

impl ZipWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, e: ZipEntry) -> &mut Self {
        self.entries.push(e);
        self
    }

    pub fn finish(&self) -> Vec<u8> {
        self.parts().join_with(&[])
    }

    pub fn parts(&self) -> ZipParts {
        let mut body = Vec::new();
        let mut cd = Vec::new();
        for e in &self.entries {
            let crc = crc32fast::hash(&e.data);
            let (method, payload): (u16, Vec<u8>) = if e.deflate {
                (8, miniz_oxide::deflate::compress_to_vec(&e.data, 6))
            } else {
                (0, e.data.clone())
            };
            let offset = body.len() as u32;
            let name = e.name.as_bytes();
            let mut extra_len = 0usize;
            if !e.deflate && e.align > 1 {
                let data_at = body.len() + 30 + name.len();
                let a = e.align as usize;
                extra_len = (a - data_at % a) % a;
            }
            body.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            body.extend_from_slice(&20u16.to_le_bytes()); // version needed
            body.extend_from_slice(&0u16.to_le_bytes()); // flags
            body.extend_from_slice(&method.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes()); // time
            body.extend_from_slice(&0x21u16.to_le_bytes()); // date 1980-01-01
            body.extend_from_slice(&crc.to_le_bytes());
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            body.extend_from_slice(&(name.len() as u16).to_le_bytes());
            body.extend_from_slice(&(extra_len as u16).to_le_bytes());
            body.extend_from_slice(name);
            body.extend(std::iter::repeat_n(0u8, extra_len));
            body.extend_from_slice(&payload);

            cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            cd.extend_from_slice(&20u16.to_le_bytes()); // version made by
            cd.extend_from_slice(&20u16.to_le_bytes()); // version needed
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&method.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0x21u16.to_le_bytes());
            cd.extend_from_slice(&crc.to_le_bytes());
            cd.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes()); // extra
            cd.extend_from_slice(&0u16.to_le_bytes()); // comment
            cd.extend_from_slice(&0u16.to_le_bytes()); // disk
            cd.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            cd.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            cd.extend_from_slice(&offset.to_le_bytes());
            cd.extend_from_slice(name);
        }
        let mut eocd = Vec::with_capacity(22);
        eocd.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        eocd.extend_from_slice(&0u16.to_le_bytes());
        eocd.extend_from_slice(&0u16.to_le_bytes());
        eocd.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        eocd.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        eocd.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        eocd.extend_from_slice(&(body.len() as u32).to_le_bytes());
        eocd.extend_from_slice(&0u16.to_le_bytes());
        ZipParts {
            entries: body,
            central_directory: cd,
            eocd,
        }
    }
}
