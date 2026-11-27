//! The APK Signing Block and APK Signature Schemes v2, v3 and v3.1.
//!
//! The block sits right before the central directory:
//!
//! ```text
//! u64 size (excluding this field)
//! pairs: u64 length, u32 ID, value
//! u64 size (same)
//! "APK Sig Block 42"
//! ```
//!
//! A scheme's value is a length-prefixed sequence of signers. Each signer
//! signs, over its `signed data`, the chunked digests of the three zip
//! sections (entries, central directory, end record with the central
//! directory offset replaced by the block offset), its certificates and
//! attributes. Verification follows apksig: the strongest supported
//! algorithm's signature must verify, signatures and digests must list the
//! same algorithms, the first certificate's public key must be the signer's
//! key, and every digest the signer lists for a supported algorithm must
//! match the content.

use std::collections::HashMap;

use sha2::{Digest as _, Sha256, Sha512};

use super::algo::{self, Algorithm, Hash};
use super::asn1;
use super::{LineageEntry, hex};
use crate::error::{Code, Error, Result};
use crate::zip::Archive;

pub const MAGIC: &[u8; 16] = b"APK Sig Block 42";
pub const V2_ID: u32 = 0x7109_871a;
pub const V3_ID: u32 = 0xf053_68c0;
pub const V31_ID: u32 = 0x1b93_ad61;
/// v2 signed attribute: the APK was also signed with the named scheme
/// (stripping protection).
pub const STRIPPING_PROTECTION_ATTR: u32 = 0xbeef_f00d;
/// v3 signed attribute: the proof-of-rotation lineage.
pub const PROOF_OF_ROTATION_ATTR: u32 = 0x3ba0_6f8c;

const CHUNK: usize = 1 << 20;

fn invalid(m: impl Into<String>) -> Error {
    Error::new(Code::InvalidSignature, m)
}

/// A length-prefixed reader over a byte slice.
#[derive(Clone, Copy)]
pub(crate) struct Lp<'a>(pub &'a [u8]);

impl<'a> Lp<'a> {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn u32(&mut self) -> Result<u32> {
        let b = self
            .0
            .get(..4)
            .ok_or_else(|| invalid("signature data is truncated"))?;
        self.0 = &self.0[4..];
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.u32()? as usize;
        let b = self
            .0
            .get(..n)
            .ok_or_else(|| invalid("signature data is truncated"))?;
        self.0 = &self.0[n..];
        Ok(b)
    }
    pub fn seq(&mut self) -> Result<Lp<'a>> {
        self.bytes().map(Lp)
    }
}

/// The signing block's location and its ID-value pairs.
#[derive(Debug)]
pub struct SigningBlock<'a> {
    pub offset: u64,
    pub pairs: HashMap<u32, &'a [u8]>,
}

/// Finds the APK Signing Block. `Ok(None)` when the APK has none.
pub fn find<'a>(apk: &Archive<'a>) -> Result<Option<SigningBlock<'a>>> {
    let d = apk.bytes();
    let cd = apk.cd_offset() as usize;
    if cd < 32 || &d[cd - 16..cd] != MAGIC {
        return Ok(None);
    }
    let size = u64::from_le_bytes(d[cd - 24..cd - 16].try_into().unwrap_or_default());
    let total = size
        .checked_add(8)
        .filter(|&t| t >= 32 && t <= cd as u64)
        .ok_or_else(|| invalid("APK Signing Block size is out of range"))?;
    let start = cd - total as usize;
    if start < apk.entries_end() as usize {
        return Err(invalid("APK Signing Block overlaps the zip entries"));
    }
    let head = u64::from_le_bytes(d[start..start + 8].try_into().unwrap_or_default());
    if head != size {
        return Err(invalid(
            "APK Signing Block sizes in header and footer differ",
        ));
    }
    let mut pairs = HashMap::new();
    let mut p = &d[start + 8..cd - 24];
    while !p.is_empty() {
        if p.len() < 8 {
            return Err(invalid("APK Signing Block pair is truncated"));
        }
        let len = u64::from_le_bytes(p[..8].try_into().unwrap_or_default());
        if len < 4 || len > (p.len() - 8) as u64 {
            return Err(invalid("APK Signing Block pair length is out of range"));
        }
        let len = len as usize;
        let id = u32::from_le_bytes(p[8..12].try_into().unwrap_or_default());
        pairs.insert(id, &p[12..8 + len]);
        p = &p[8 + len..];
    }
    Ok(Some(SigningBlock {
        offset: start as u64,
        pairs,
    }))
}

/// The chunked content digest of the APK for the given hash, as the
/// schemes define it.
pub fn content_digest(
    apk_bytes: &[u8],
    block_offset: u64,
    cd_offset: u64,
    eocd_offset: u64,
    hash: Hash,
) -> Vec<u8> {
    let mut eocd = apk_bytes[eocd_offset as usize..].to_vec();
    eocd[16..20].copy_from_slice(&(block_offset as u32).to_le_bytes());
    let sections: [&[u8]; 3] = [
        &apk_bytes[..block_offset as usize],
        &apk_bytes[cd_offset as usize..eocd_offset as usize],
        &eocd,
    ];
    let count: usize = sections.iter().map(|s| s.len().div_ceil(CHUNK)).sum();
    let mut top = Vec::with_capacity(5 + count * 64);
    top.push(0x5a);
    top.extend_from_slice(&(count as u32).to_le_bytes());
    for s in sections {
        for chunk in s.chunks(CHUNK) {
            let len = (chunk.len() as u32).to_le_bytes();
            match hash {
                Hash::Sha512 => {
                    let mut h = Sha512::new();
                    h.update([0xa5]);
                    h.update(len);
                    h.update(chunk);
                    top.extend_from_slice(&h.finalize());
                }
                _ => {
                    let mut h = Sha256::new();
                    h.update([0xa5]);
                    h.update(len);
                    h.update(chunk);
                    top.extend_from_slice(&h.finalize());
                }
            }
        }
    }
    match hash {
        Hash::Sha512 => Sha512::digest(&top).to_vec(),
        _ => Sha256::digest(&top).to_vec(),
    }
}

/// Lazily computed content digests per hash.
pub struct Contents<'a> {
    apk: &'a Archive<'a>,
    block_offset: u64,
    cache: HashMap<&'static str, Vec<u8>>,
}

impl<'a> Contents<'a> {
    pub fn new(apk: &'a Archive<'a>, block_offset: u64) -> Self {
        Self {
            apk,
            block_offset,
            cache: HashMap::new(),
        }
    }
    fn get(&mut self, h: Hash) -> &[u8] {
        let key = if h == Hash::Sha512 { "512" } else { "256" };
        let apk = self.apk;
        let off = self.block_offset;
        self.cache.entry(key).or_insert_with(|| {
            content_digest(apk.bytes(), off, apk.cd_offset(), apk.eocd_offset(), h)
        })
    }
}

/// One verified signer.
#[derive(Clone, Debug)]
pub struct VerifiedSigner {
    pub certificate: Vec<u8>,
    pub min_sdk: u32,
    pub max_sdk: u32,
    /// Signed additional attributes: ID -> value.
    pub attributes: Vec<(u32, Vec<u8>)>,
}

/// Algorithm preference, as apksig: SHA-512 content digests over SHA-256,
/// then by ID for a stable choice.
fn strength(a: &Algorithm) -> (u8, u32) {
    (u8::from(a.content == Hash::Sha512), a.id)
}

/// Verifies one signer of scheme v2 (`v3 == false`) or v3/v3.1.
fn verify_signer(mut s: Lp<'_>, v3: bool, contents: &mut Contents<'_>) -> Result<VerifiedSigner> {
    let signed_data = s.bytes()?;
    let (outer_min, outer_max) = if v3 {
        (s.u32()?, s.u32()?)
    } else {
        (0, u32::MAX)
    };
    let mut sigs = s.seq()?;
    let public_key = s.bytes()?;
    if !s.is_empty() && !v3 {
        // Trailing data inside a v2 signer is tolerated, as in apksig.
    }
    let mut sig_algos = Vec::new();
    let mut best: Option<(Algorithm, &[u8])> = None;
    while !sigs.is_empty() {
        let mut one = sigs.seq()?;
        let id = one.u32()?;
        let sig = one.bytes()?;
        sig_algos.push(id);
        if let Some(a) = algo::algorithm(id)
            && a.kind != algo::Kind::Dsa
            && best.is_none_or(|(b, _)| strength(&a) > strength(&b))
        {
            best = Some((a, sig));
        }
    }
    if sig_algos.is_empty() {
        return Err(invalid("signer has no signatures"));
    }
    let Some((alg, sig)) = best else {
        return Err(Error::new(
            Code::UnsupportedSignature,
            format!("no supported signature algorithm among {sig_algos:04x?}"),
        ));
    };
    algo::verify(alg.kind, alg.hash, public_key, signed_data, sig)?;

    // The signature verified: the signed data can be trusted now.
    let mut sd = Lp(signed_data);
    let mut digests = sd.seq()?;
    let mut digest_algos = Vec::new();
    let mut expected = None;
    while !digests.is_empty() {
        let mut one = digests.seq()?;
        let id = one.u32()?;
        let d = one.bytes()?;
        digest_algos.push(id);
        if id == alg.id {
            expected = Some(d);
        }
    }
    if digest_algos != sig_algos {
        return Err(invalid(
            "signature algorithms differ between the digests and the signatures records",
        ));
    }
    let mut certs = sd.seq()?;
    let first = certs
        .bytes()
        .map_err(|_| invalid("signer has no certificate"))?;
    let cert =
        asn1::certificate(first).ok_or_else(|| invalid("signer certificate is malformed"))?;
    if cert.spki != public_key {
        return Err(invalid(
            "public key mismatch between the certificate and the signature record",
        ));
    }
    let (min_sdk, max_sdk) = if v3 {
        let (a, b) = (sd.u32()?, sd.u32()?);
        if (a, b) != (outer_min, outer_max) {
            return Err(invalid(
                "SDK versions differ between the signed and unsigned data",
            ));
        }
        (a, b)
    } else {
        (0, u32::MAX)
    };
    let mut attrs = sd.seq()?;
    let mut attributes = Vec::new();
    while !attrs.is_empty() {
        let mut one = attrs.seq()?;
        let id = one.u32()?;
        attributes.push((id, one.0.to_vec()));
    }
    let expected = expected.ok_or_else(|| invalid("no digest for the verified algorithm"))?;
    if contents.get(alg.content) != expected {
        return Err(invalid(format!(
            "APK Signature Scheme {} digest of the contents does not match",
            if v3 { "v3" } else { "v2" }
        )));
    }
    Ok(VerifiedSigner {
        certificate: first.to_vec(),
        min_sdk,
        max_sdk,
        attributes,
    })
}

/// Verifies all signers of a v2 block.
pub fn verify_v2(value: &[u8], contents: &mut Contents<'_>) -> Result<Vec<VerifiedSigner>> {
    let mut outer = Lp(value);
    let mut signers = outer.seq()?;
    let mut out = Vec::new();
    while !signers.is_empty() {
        let s = signers.seq()?;
        out.push(
            verify_signer(s, false, contents)
                .map_err(|e| e.context(format!("v2 signer {}", out.len() + 1)))?,
        );
    }
    if out.is_empty() {
        return Err(invalid("v2 block has no signers"));
    }
    Ok(out)
}

/// Verifies the v3 (or v3.1) signer for `sdk`. `Ok(None)` when no signer
/// covers `sdk`.
pub fn verify_v3(
    value: &[u8],
    sdk: u32,
    contents: &mut Contents<'_>,
) -> Result<Option<VerifiedSigner>> {
    let mut outer = Lp(value);
    let mut signers = outer.seq()?;
    let mut found = None;
    let mut any = false;
    while !signers.is_empty() {
        let s = signers.seq()?;
        any = true;
        // Peek at the outer SDK range before the costly verification.
        let mut peek = s;
        peek.bytes()?;
        let (min, max) = (peek.u32()?, peek.u32()?);
        if sdk < min || sdk > max {
            continue;
        }
        if found.is_some() {
            return Err(invalid("more than one v3 signer for this platform version"));
        }
        found = Some(verify_signer(s, true, contents)?);
    }
    if !any {
        return Err(invalid("v3 block has no signers"));
    }
    Ok(found)
}

/// Parses and verifies a proof-of-rotation lineage; returns its entries,
/// oldest first.
pub fn lineage(value: &[u8]) -> Result<(Vec<LineageEntry>, Vec<u8>)> {
    let mut r = Lp(value);
    if r.u32()? != 1 {
        return Err(invalid("unknown proof-of-rotation version"));
    }
    let mut out = Vec::new();
    let mut last: Option<(Vec<u8>, u32)> = None;
    while !r.is_empty() {
        let mut node = r.seq()?;
        let signed = node.bytes()?;
        let flags = node.u32()?;
        let sig_alg = node.u32()?;
        let sig = node.bytes()?;
        if let Some((cert, alg_id)) = &last {
            let a = algo::algorithm(*alg_id).ok_or_else(|| {
                Error::new(
                    Code::UnsupportedSignature,
                    "unsupported lineage signature algorithm",
                )
            })?;
            let c = asn1::certificate(cert)
                .ok_or_else(|| invalid("lineage certificate is malformed"))?;
            algo::verify(a.kind, a.hash, c.spki, signed, sig)
                .map_err(|e| e.context("proof-of-rotation"))?;
        }
        let mut sd = Lp(signed);
        let cert = sd.bytes()?.to_vec();
        let signed_alg = sd.u32()?;
        if let Some((_, alg_id)) = &last
            && *alg_id != signed_alg
        {
            return Err(invalid("lineage signature algorithm mismatch"));
        }
        asn1::certificate(&cert).ok_or_else(|| invalid("lineage certificate is malformed"))?;
        out.push(LineageEntry {
            cert_sha256: hex(&Sha256::digest(&cert)),
            flags,
        });
        last = Some((cert, sig_alg));
    }
    let last_cert = last
        .map(|(c, _)| c)
        .ok_or_else(|| invalid("lineage is empty"))?;
    Ok((out, last_cert))
}
