//! JAR signing (APK signature scheme v1).
//!
//! `META-INF/MANIFEST.MF` lists a digest per entry; each signer's
//! `META-INF/<name>.SF` lists digests of the manifest (whole, or per
//! section); `META-INF/<name>.RSA|.EC|.DSA` is a PKCS #7 SignedData block
//! with the signer's certificate and the signature over the `.SF` file (or
//! over signed attributes that carry its digest). Every entry outside the
//! signature files must be covered by the manifest, as Android requires.

use std::collections::HashMap;

use super::algo::{self, Hash, Kind};
use super::asn1::{self, Reader};
use crate::error::{Code, Error, Result};
use crate::zip::Archive;

fn invalid(m: impl Into<String>) -> Error {
    Error::new(Code::InvalidSignature, m)
}

/// One manifest section.
#[derive(Debug)]
pub struct Section<'a> {
    /// The raw bytes, including the blank line that ends it.
    pub raw: &'a [u8],
    pub attrs: HashMap<String, String>,
}

/// Splits a JAR manifest into sections and reads their attributes
/// (continuation lines start with one space).
pub fn sections(data: &[u8]) -> Result<Vec<Section<'_>>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut pos = 0;
    let mut attrs: HashMap<String, String> = HashMap::new();
    let mut last_key: Option<String> = None;
    while pos < data.len() {
        let end = data[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| pos + i + 1)
            .unwrap_or(data.len());
        let line = &data[pos..end];
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        pos = end;
        if text.is_empty() {
            out.push(Section {
                raw: &data[start..pos],
                attrs: std::mem::take(&mut attrs),
            });
            last_key = None;
            start = pos;
            continue;
        }
        let text = std::str::from_utf8(text).map_err(|_| invalid("manifest is not UTF-8"))?;
        if let Some(cont) = text.strip_prefix(' ') {
            let k = last_key
                .as_ref()
                .ok_or_else(|| invalid("manifest continuation without attribute"))?;
            if let Some(v) = attrs.get_mut(k) {
                v.push_str(cont);
            }
            continue;
        }
        let (k, v) = text
            .split_once(": ")
            .ok_or_else(|| invalid(format!("manifest line `{text}` is not an attribute")))?;
        attrs.insert(k.to_string(), v.to_string());
        last_key = Some(k.to_string());
    }
    if start < data.len() {
        out.push(Section {
            raw: &data[start..],
            attrs,
        });
    }
    Ok(out)
}

/// Standard base64 (with padding) decoding.
pub fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.trim().bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Digest attribute prefixes, strongest first, as Android checks them.
const DIGESTS: [(&str, Hash); 4] = [
    ("SHA-512", Hash::Sha512),
    ("SHA-384", Hash::Sha384),
    ("SHA-256", Hash::Sha256),
    ("SHA1", Hash::Sha1),
];

/// Checks the strongest digest attribute `<alg><suffix>` in `attrs`
/// against `data`. `None` when there is no supported digest attribute.
fn check_digest(attrs: &HashMap<String, String>, suffix: &str, data: &[u8]) -> Option<bool> {
    for (name, h) in DIGESTS {
        let key = format!("{name}{suffix}");
        let found = attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&key))
            .map(|(_, v)| v);
        if let Some(v) = found {
            return Some(base64(v).is_some_and(|d| d == h.digest(data)));
        }
    }
    None
}

fn is_signature_file(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("META-INF/") else {
        return false;
    };
    if rest.contains('/') {
        return false;
    }
    let upper = rest.to_ascii_uppercase();
    upper == "MANIFEST.MF"
        || upper.ends_with(".SF")
        || upper.ends_with(".RSA")
        || upper.ends_with(".DSA")
        || upper.ends_with(".EC")
        || upper.starts_with("SIG-")
}

const OID_SIGNED_DATA: &[u64] = &[1, 2, 840, 113549, 1, 7, 2];
const OID_MESSAGE_DIGEST: &[u64] = &[1, 2, 840, 113549, 1, 9, 4];

fn hash_from_oid(oid: &[u8]) -> Option<Hash> {
    let table: [(&[u64], Hash); 4] = [
        (&[1, 3, 14, 3, 2, 26], Hash::Sha1),
        (&[2, 16, 840, 1, 101, 3, 4, 2, 1], Hash::Sha256),
        (&[2, 16, 840, 1, 101, 3, 4, 2, 2], Hash::Sha384),
        (&[2, 16, 840, 1, 101, 3, 4, 2, 3], Hash::Sha512),
    ];
    table
        .iter()
        .find(|(o, _)| asn1::oid(o) == oid)
        .map(|(_, h)| *h)
}

/// The signature kind and (for algorithms that name one) hash of a
/// signature algorithm OID.
fn sig_alg_from_oid(oid: &[u8]) -> Option<(Kind, Option<Hash>)> {
    let table: [(&[u64], Kind, Option<Hash>); 10] = [
        (&[1, 2, 840, 113549, 1, 1, 1], Kind::RsaPkcs1, None),
        (
            &[1, 2, 840, 113549, 1, 1, 5],
            Kind::RsaPkcs1,
            Some(Hash::Sha1),
        ),
        (
            &[1, 2, 840, 113549, 1, 1, 11],
            Kind::RsaPkcs1,
            Some(Hash::Sha256),
        ),
        (
            &[1, 2, 840, 113549, 1, 1, 12],
            Kind::RsaPkcs1,
            Some(Hash::Sha384),
        ),
        (
            &[1, 2, 840, 113549, 1, 1, 13],
            Kind::RsaPkcs1,
            Some(Hash::Sha512),
        ),
        (&[1, 2, 840, 10045, 2, 1], Kind::Ecdsa, None),
        (&[1, 2, 840, 10045, 4, 1], Kind::Ecdsa, Some(Hash::Sha1)),
        (
            &[1, 2, 840, 10045, 4, 3, 2],
            Kind::Ecdsa,
            Some(Hash::Sha256),
        ),
        (
            &[1, 2, 840, 10045, 4, 3, 3],
            Kind::Ecdsa,
            Some(Hash::Sha384),
        ),
        (
            &[1, 2, 840, 10045, 4, 3, 4],
            Kind::Ecdsa,
            Some(Hash::Sha512),
        ),
    ];
    table
        .iter()
        .find(|(o, _, _)| asn1::oid(o) == oid)
        .map(|(_, k, h)| (*k, *h))
}

/// Verifies a PKCS #7 SignedData block over `content` and returns the
/// signer's DER certificate.
pub fn verify_pkcs7(block: &[u8], content: &[u8]) -> Result<Vec<u8>> {
    let m = || invalid("signature block is malformed");
    let mut top = Reader(block);
    let ci = top.expect(asn1::SEQUENCE).ok_or_else(m)?;
    let mut ci = ci.reader();
    if ci.expect(asn1::OID).ok_or_else(m)?.value != asn1::oid(OID_SIGNED_DATA) {
        return Err(m());
    }
    let explicit = ci.expect(0xa0).ok_or_else(m)?;
    let sd = explicit.reader().expect(asn1::SEQUENCE).ok_or_else(m)?;
    let mut sd = sd.reader();
    sd.expect(asn1::INTEGER).ok_or_else(m)?;
    sd.expect(asn1::SET).ok_or_else(m)?;
    sd.expect(asn1::SEQUENCE).ok_or_else(m)?; // encapsulated content info
    let certs = sd.optional(0xa0);
    sd.optional(0xa1); // CRLs
    let signer_infos = sd.expect(asn1::SET).ok_or_else(m)?;
    let mut infos = signer_infos.reader();
    let si = infos.expect(asn1::SEQUENCE).ok_or_else(m)?;
    let mut si = si.reader();
    si.expect(asn1::INTEGER).ok_or_else(m)?;
    let sid = si.expect(asn1::SEQUENCE).ok_or_else(m)?;
    let mut sid = sid.reader();
    let issuer = sid.expect(asn1::SEQUENCE).ok_or_else(m)?.raw;
    let serial = sid.expect(asn1::INTEGER).ok_or_else(m)?.value;
    let digest_alg = si.expect(asn1::SEQUENCE).ok_or_else(m)?;
    let digest_oid = digest_alg.reader().expect(asn1::OID).ok_or_else(m)?.value;
    let hash = hash_from_oid(digest_oid).ok_or_else(|| {
        Error::new(
            Code::UnsupportedSignature,
            "unsupported digest algorithm in signature block",
        )
    })?;
    let signed_attrs = si.optional(0xa0);
    let sig_alg = si.expect(asn1::SEQUENCE).ok_or_else(m)?;
    let sig_oid = sig_alg.reader().expect(asn1::OID).ok_or_else(m)?.value;
    let signature = si.expect(asn1::OCTET_STRING).ok_or_else(m)?.value;
    let (kind, sig_hash) = sig_alg_from_oid(sig_oid).ok_or_else(|| {
        Error::new(
            Code::UnsupportedSignature,
            "unsupported signature algorithm in signature block",
        )
    })?;
    let hash = sig_hash.unwrap_or(hash);

    // The signer's certificate, by issuer and serial number.
    let mut cert_der = None;
    if let Some(certs) = certs {
        let mut r = certs.reader();
        while let Some(c) = r.read_tlv() {
            if let Some(parsed) = asn1::certificate(c.raw)
                && parsed.issuer == issuer
                && parsed.serial == serial
            {
                cert_der = Some(c.raw);
                break;
            }
        }
    }
    let cert_der =
        cert_der.ok_or_else(|| invalid("signature block has no certificate for its signer"))?;
    let cert = asn1::certificate(cert_der).ok_or_else(m)?;

    let signed: Vec<u8> = match signed_attrs {
        None => content.to_vec(),
        Some(attrs) => {
            // The messageDigest attribute must be the content's digest.
            let mut r = attrs.reader();
            let mut digest = None;
            while let Some(a) = r.read_tlv() {
                let mut a = a.reader();
                let oid = a.expect(asn1::OID).ok_or_else(m)?.value;
                if oid == asn1::oid(OID_MESSAGE_DIGEST) {
                    let set = a.expect(asn1::SET).ok_or_else(m)?;
                    digest = Some(set.reader().expect(asn1::OCTET_STRING).ok_or_else(m)?.value);
                }
            }
            let d = digest.ok_or_else(|| invalid("signed attributes have no message digest"))?;
            if d != hash.digest(content).as_slice() {
                return Err(invalid(
                    "signed attributes' message digest does not match the .SF file",
                ));
            }
            // The signature covers the attributes encoded as a SET.
            let mut v = attrs.raw.to_vec();
            v[0] = asn1::SET;
            v
        }
    };
    algo::verify(kind, hash, cert.spki, &signed, signature)?;
    Ok(cert_der.to_vec())
}

/// The result of v1 verification.
#[derive(Debug)]
pub struct V1 {
    /// Signer certificates, sorted by `.SF` file name.
    pub certificates: Vec<Vec<u8>>,
    /// Scheme IDs named by `X-Android-APK-Signed` (stripping protection).
    pub apk_signed_schemes: Vec<u32>,
}

/// Verifies the JAR signature. `Ok(None)` when the APK has no
/// `META-INF/MANIFEST.MF` or no signature files.
pub fn verify(apk: &Archive<'_>) -> Result<Option<V1>> {
    let Some(mf_bytes) = apk.read_name("META-INF/MANIFEST.MF")? else {
        return Ok(None);
    };
    let mf = sections(&mf_bytes)?;
    let mut by_name: HashMap<&str, &Section<'_>> = HashMap::new();
    for s in mf.iter().skip(1) {
        if let Some(n) = s.attrs.get("Name") {
            by_name.insert(n.as_str(), s);
        }
    }
    let mut sf_names: Vec<&str> = apk
        .entries()
        .iter()
        .map(|e| e.name.as_str())
        .filter(|n| {
            n.starts_with("META-INF/")
                && n.to_ascii_uppercase().ends_with(".SF")
                && !n["META-INF/".len()..].contains('/')
        })
        .collect();
    sf_names.sort_unstable();
    let mut certificates = Vec::new();
    let mut schemes = Vec::new();
    for sf_name in sf_names {
        let stem = &sf_name[..sf_name.len() - 3];
        let block = ["RSA", "EC", "DSA"]
            .iter()
            .find_map(|ext| apk.find(&format!("{stem}.{ext}")));
        let Some(block) = block else {
            continue;
        };
        let sf_bytes = apk.read_name(sf_name)?.unwrap_or_default();
        let block_bytes = apk.read(block)?;
        let cert = verify_pkcs7(&block_bytes, &sf_bytes).map_err(|e| e.context(sf_name))?;
        let sf = sections(&sf_bytes)?;
        let main = sf
            .first()
            .ok_or_else(|| invalid(format!("{sf_name} is empty")))?;
        if let Some(v) = main.attrs.get("X-Android-APK-Signed") {
            schemes.extend(v.split(',').filter_map(|s| s.trim().parse::<u32>().ok()));
        }
        // The .SF covers the manifest whole, or section by section.
        let whole = check_digest(&main.attrs, "-Digest-Manifest", &mf_bytes) == Some(true);
        if !whole {
            for s in sf.iter().skip(1) {
                let Some(name) = s.attrs.get("Name") else {
                    continue;
                };
                let mfs = by_name.get(name.as_str()).ok_or_else(|| {
                    invalid(format!(
                        "{sf_name} names `{name}`, which the manifest does not"
                    ))
                })?;
                if check_digest(&s.attrs, "-Digest", mfs.raw) != Some(true) {
                    return Err(invalid(format!(
                        "{sf_name}: digest of the manifest section for `{name}` does not match"
                    )));
                }
            }
            for name in by_name.keys() {
                if !sf
                    .iter()
                    .any(|s| s.attrs.get("Name").map(String::as_str) == Some(name))
                {
                    return Err(invalid(format!("`{name}` is not signed by {sf_name}")));
                }
            }
        }
        certificates.push(cert);
    }
    if certificates.is_empty() {
        return Ok(None);
    }
    // Every entry outside the signature files is in the manifest, with a
    // matching digest.
    for e in apk.entries() {
        if e.is_dir() || is_signature_file(&e.name) {
            continue;
        }
        let s = by_name
            .get(e.name.as_str())
            .ok_or_else(|| invalid(format!("`{}` is not covered by the JAR signature", e.name)))?;
        let data = apk.read(e)?;
        match check_digest(&s.attrs, "-Digest", &data) {
            Some(true) => {}
            Some(false) => {
                return Err(invalid(format!(
                    "JAR digest of `{}` does not match",
                    e.name
                )));
            }
            None => {
                return Err(Error::new(
                    Code::UnsupportedSignature,
                    format!("`{}` has no supported digest in the manifest", e.name),
                ));
            }
        }
    }
    schemes.sort_unstable();
    schemes.dedup();
    Ok(Some(V1 {
        certificates,
        apk_signed_schemes: schemes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_manifest_sections() {
        let mf = b"Manifest-Version: 1.0\r\nCreated-By: x\r\n\r\nName: res/a\r\n very/long.png\r\nSHA-256-Digest: AAAA\r\n\r\n";
        let s = sections(mf).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].attrs["Name"], "res/avery/long.png");
        assert!(s[1].raw.ends_with(b"\r\n\r\n"));
        assert_eq!(s[0].raw.len() + s[1].raw.len(), mf.len());
    }

    #[test]
    fn base64_decodes() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGk=").unwrap(), b"hi");
        assert!(base64("a$").is_none());
    }
}
