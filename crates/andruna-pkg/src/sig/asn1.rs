//! A minimal DER reader: enough to take X.509 certificates and PKCS #7
//! SignedData apart. It returns slices of the input and never panics.

pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;
pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OCTET_STRING: u8 = 0x04;
pub const OID: u8 = 0x06;

/// One TLV.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tlv<'a> {
    pub tag: u8,
    /// The contents (value bytes).
    pub value: &'a [u8],
    /// The whole encoding (tag, length and value).
    pub raw: &'a [u8],
}

/// A cursor over a sequence of TLVs.
#[derive(Clone, Copy, Debug)]
pub struct Reader<'a>(pub &'a [u8]);

impl<'a> Reader<'a> {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Reads the next TLV (single-byte tags, definite lengths only).
    pub fn next(&mut self) -> Option<Tlv<'a>> {
        let d = self.0;
        let tag = *d.first()?;
        if tag & 0x1f == 0x1f {
            return None;
        }
        let first = *d.get(1)? as usize;
        let (len, hdr) = if first < 0x80 {
            (first, 2)
        } else {
            let n = first & 0x7f;
            if n == 0 || n > 4 {
                return None;
            }
            let mut len = 0usize;
            for i in 0..n {
                len = (len << 8) | *d.get(2 + i)? as usize;
            }
            (len, 2 + n)
        };
        let end = hdr.checked_add(len)?;
        let raw = d.get(..end)?;
        self.0 = &d[end..];
        Some(Tlv {
            tag,
            value: &raw[hdr..],
            raw,
        })
    }

    /// Reads the next TLV and checks its tag.
    pub fn expect(&mut self, tag: u8) -> Option<Tlv<'a>> {
        let t = self.next()?;
        (t.tag == tag).then_some(t)
    }

    /// Reads the next TLV if it has the given tag.
    pub fn optional(&mut self, tag: u8) -> Option<Tlv<'a>> {
        if self.0.first() == Some(&tag) {
            self.next()
        } else {
            None
        }
    }
}

impl<'a> Tlv<'a> {
    pub fn reader(&self) -> Reader<'a> {
        Reader(self.value)
    }
}

/// Encodes a dotted OID string's arcs to DER contents, for comparisons.
pub fn oid(arcs: &[u64]) -> Vec<u8> {
    let mut out = vec![(arcs[0] * 40 + arcs[1]) as u8];
    for &a in &arcs[2..] {
        let mut tmp = Vec::new();
        let mut v = a;
        tmp.push((v & 0x7f) as u8);
        v >>= 7;
        while v > 0 {
            tmp.push(0x80 | (v & 0x7f) as u8);
            v >>= 7;
        }
        tmp.reverse();
        out.extend(tmp);
    }
    out
}

/// The parts of an X.509 certificate the verifier needs.
#[derive(Clone, Debug)]
pub struct Certificate<'a> {
    pub raw: &'a [u8],
    /// The DER issuer Name (whole TLV).
    pub issuer: &'a [u8],
    /// The serial number's INTEGER contents.
    pub serial: &'a [u8],
    /// The DER SubjectPublicKeyInfo (whole TLV).
    pub spki: &'a [u8],
}

/// Parses the fields of a DER certificate. Validity and extensions are not
/// read: Android does not check them for APK signers.
pub fn certificate(der: &[u8]) -> Option<Certificate<'_>> {
    let mut top = Reader(der);
    let cert = top.expect(SEQUENCE)?;
    if !top.is_empty() {
        return None;
    }
    let mut c = cert.reader();
    let tbs = c.expect(SEQUENCE)?;
    let mut t = tbs.reader();
    t.optional(0xa0); // [0] version
    let serial = t.expect(INTEGER)?;
    t.expect(SEQUENCE)?; // signature algorithm
    let issuer = t.expect(SEQUENCE)?;
    t.expect(SEQUENCE)?; // validity
    t.expect(SEQUENCE)?; // subject
    let spki = t.expect(SEQUENCE)?;
    Some(Certificate {
        raw: cert.raw,
        issuer: issuer.raw,
        serial: serial.value,
        spki: spki.raw,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oids_encode() {
        // 1.2.840.113549.1.1.1 (rsaEncryption)
        assert_eq!(
            oid(&[1, 2, 840, 113549, 1, 1, 1]),
            [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01]
        );
    }

    #[test]
    fn reads_tlvs_and_rejects_damage() {
        let d = [0x30, 0x03, 0x02, 0x01, 0x05, 0x04, 0x81, 0x01, 0xaa];
        let mut r = Reader(&d);
        let s = r.expect(SEQUENCE).unwrap();
        assert_eq!(s.reader().expect(INTEGER).unwrap().value, [5]);
        assert_eq!(r.expect(OCTET_STRING).unwrap().value, [0xaa]);
        assert!(r.is_empty());
        for cut in 0..d.len() {
            let mut r = Reader(&d[..cut]);
            while r.next().is_some() {}
        }
        assert!(Reader(&[0x30, 0x85, 1, 1, 1, 1, 1]).next().is_none());
    }
}
