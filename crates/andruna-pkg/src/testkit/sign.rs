//! APK signing for tests: keys and self-signed certificates generated at
//! test time, JAR signing (v1) and APK Signature Schemes v2, v3 and v3.1
//! with an optional rotation lineage.

use rand::rngs::OsRng;
use rsa::RsaPrivateKey;
use rsa::pkcs8::EncodePublicKey;
use rsa::signature::{SignatureEncoding, Signer};
use sha2::{Digest as _, Sha256};

use super::zipw::{ZipEntry, ZipWriter};
use crate::sig::algo::Hash;
use crate::sig::block;

/// DER helpers.
pub mod der {
    pub fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
        let mut v = vec![tag];
        let n = value.len();
        if n < 0x80 {
            v.push(n as u8);
        } else {
            let bytes: Vec<u8> = n
                .to_be_bytes()
                .iter()
                .copied()
                .skip_while(|&b| b == 0)
                .collect();
            v.push(0x80 | bytes.len() as u8);
            v.extend(bytes);
        }
        v.extend_from_slice(value);
        v
    }
    pub fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
        tlv(0x30, &parts.concat())
    }
    pub fn set(parts: &[Vec<u8>]) -> Vec<u8> {
        tlv(0x31, &parts.concat())
    }
    pub fn oid(arcs: &[u64]) -> Vec<u8> {
        tlv(0x06, &crate::sig::asn1::oid(arcs))
    }
    pub fn int(v: &[u8]) -> Vec<u8> {
        let mut b = v.to_vec();
        if b.first().is_some_and(|&x| x & 0x80 != 0) {
            b.insert(0, 0);
        }
        tlv(0x02, &b)
    }
    pub fn null() -> Vec<u8> {
        vec![0x05, 0x00]
    }
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Rsa2048,
    P256,
}

#[derive(Clone)]
enum Private {
    Rsa(Box<RsaPrivateKey>),
    P256(p256::ecdsa::SigningKey),
}

/// A signing key with a self-signed certificate.
#[derive(Clone)]
pub struct TestKey {
    private: Private,
    pub spki: Vec<u8>,
    pub cert: Vec<u8>,
}

const OID_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 1];
const OID_SHA256_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 11];
const OID_ECDSA_SHA256: &[u64] = &[1, 2, 840, 10045, 4, 3, 2];
const OID_SHA256: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 2, 1];

impl TestKey {
    /// Generates a key and a self-signed certificate with common name `cn`.
    pub fn generate(kind: KeyKind, cn: &str) -> Self {
        let (private, spki) = match kind {
            KeyKind::Rsa2048 => {
                let k = RsaPrivateKey::new(&mut OsRng, 2048).expect("RSA key generation");
                let spki = k
                    .to_public_key()
                    .to_public_key_der()
                    .unwrap()
                    .as_bytes()
                    .to_vec();
                (Private::Rsa(Box::new(k)), spki)
            }
            KeyKind::P256 => {
                let k = p256::ecdsa::SigningKey::random(&mut OsRng);
                let point = k.verifying_key().to_encoded_point(false);
                let mut bits = vec![0u8];
                bits.extend_from_slice(point.as_bytes());
                let spki = der::seq(&[
                    der::seq(&[
                        der::oid(&[1, 2, 840, 10045, 2, 1]),
                        der::oid(&[1, 2, 840, 10045, 3, 1, 7]),
                    ]),
                    der::tlv(0x03, &bits),
                ]);
                (Private::P256(k), spki)
            }
        };
        let mut key = Self {
            private,
            spki,
            cert: Vec::new(),
        };
        key.cert = key.self_signed(cn);
        key
    }

    fn sig_alg_der(&self) -> Vec<u8> {
        match self.private {
            Private::Rsa(_) => der::seq(&[der::oid(OID_SHA256_RSA), der::null()]),
            Private::P256(_) => der::seq(&[der::oid(OID_ECDSA_SHA256)]),
        }
    }

    fn self_signed(&self, cn: &str) -> Vec<u8> {
        let name = der::seq(&[der::set(&[der::seq(&[
            der::oid(&[2, 5, 4, 3]),
            der::tlv(0x0c, cn.as_bytes()),
        ])])]);
        let mut serial = [0u8; 8];
        serial.copy_from_slice(&Sha256::digest(&self.spki)[..8]);
        serial[0] &= 0x7f;
        let validity = der::seq(&[
            der::tlv(0x17, b"260101000000Z"),
            der::tlv(0x18, b"20991231000000Z"),
        ]);
        let tbs = der::seq(&[
            der::tlv(0xa0, &der::int(&[2])),
            der::int(&serial),
            self.sig_alg_der(),
            name.clone(),
            validity,
            name,
            self.spki.clone(),
        ]);
        let sig = self.sign_sha256(&tbs);
        let mut bits = vec![0u8];
        bits.extend(sig);
        der::seq(&[tbs, self.sig_alg_der(), der::tlv(0x03, &bits)])
    }

    /// The APK signature algorithm ID this key signs with.
    pub fn algorithm_id(&self) -> u32 {
        match self.private {
            Private::Rsa(_) => 0x0103,
            Private::P256(_) => 0x0201,
        }
    }

    /// Signs with SHA-256 (RSA PKCS #1 v1.5 or DER ECDSA).
    pub fn sign_sha256(&self, data: &[u8]) -> Vec<u8> {
        match &self.private {
            Private::Rsa(k) => {
                let sk = rsa::pkcs1v15::SigningKey::<Sha256>::new((**k).clone());
                sk.sign(data).to_vec()
            }
            Private::P256(k) => {
                let s: p256::ecdsa::DerSignature = k.sign(data);
                s.to_bytes().to_vec()
            }
        }
    }

    pub fn cert_sha256(&self) -> String {
        crate::sig::cert_sha256(&self.cert)
    }
}

/// What to sign with.
#[derive(Clone)]
pub struct SignConfig {
    pub key: TestKey,
    pub v1: bool,
    pub v2: bool,
    pub v3: bool,
    pub v31: bool,
    /// Rotation: the previous keys, oldest first, with the flags granted
    /// to each; written as a proof-of-rotation lineage ending with `key`.
    pub rotated_from: Vec<(TestKey, u32)>,
    /// `X-Android-APK-Signed` in the `.SF` file (when v2 or v3 is on).
    pub v1_names_v2: bool,
    /// The v3 SDK range.
    pub v3_sdk: (u32, u32),
    /// Leave the APK Signing Block out (to test stripping protection).
    pub strip_block: bool,
    /// Sign v1 and v2 with this key instead (to test signer mismatches).
    pub legacy_override: Option<TestKey>,
}

impl SignConfig {
    /// The key of v1 and v2: the oldest key of a rotation, as apksig
    /// signs (older platforms only know v1/v2 and the original signer).
    pub fn legacy_key(&self) -> &TestKey {
        self.legacy_override
            .as_ref()
            .or(self.rotated_from.first().map(|(k, _)| k))
            .unwrap_or(&self.key)
    }

    pub fn new(key: TestKey) -> Self {
        Self {
            key,
            v1: false,
            v2: true,
            v3: true,
            v31: false,
            rotated_from: Vec::new(),
            v1_names_v2: true,
            v3_sdk: (28, i32::MAX as u32),
            strip_block: false,
            legacy_override: None,
        }
    }
}

fn lp(b: &[u8]) -> Vec<u8> {
    let mut v = (b.len() as u32).to_le_bytes().to_vec();
    v.extend_from_slice(b);
    v
}

fn lps(items: &[Vec<u8>]) -> Vec<u8> {
    lp(&items.iter().map(|i| lp(i)).collect::<Vec<_>>().concat())
}

fn jar_section(name: &str, digest: &[u8]) -> String {
    format!("Name: {name}\r\nSHA-256-Digest: {}\r\n\r\n", base64(digest))
}

/// Adds the v1 files for `key` to `zip`.
fn sign_v1(zip: &mut ZipWriter, entries: &[ZipEntry], cfg: &SignConfig) {
    let mut mf = String::from("Manifest-Version: 1.0\r\nCreated-By: andruna testkit\r\n\r\n");
    let mut sections = Vec::new();
    for e in entries {
        let s = jar_section(&e.name, &Sha256::digest(&e.data));
        sections.push((e.name.clone(), s.clone()));
        mf.push_str(&s);
    }
    let mut sf = String::from("Signature-Version: 1.0\r\nCreated-By: andruna testkit\r\n");
    sf.push_str(&format!(
        "SHA-256-Digest-Manifest: {}\r\n",
        base64(&Sha256::digest(mf.as_bytes()))
    ));
    if cfg.v1_names_v2 && (cfg.v2 || cfg.v3) {
        let mut ids = Vec::new();
        if cfg.v2 {
            ids.push("2");
        }
        if cfg.v3 {
            ids.push("3");
        }
        sf.push_str(&format!("X-Android-APK-Signed: {}\r\n", ids.join(", ")));
    }
    sf.push_str("\r\n");
    for (name, s) in &sections {
        sf.push_str(&jar_section(name, &Sha256::digest(s.as_bytes())));
    }
    let key = cfg.legacy_key();
    let cert = crate::sig::asn1::certificate(&key.cert).unwrap();
    let sig = key.sign_sha256(sf.as_bytes());
    let sig_alg = match key.private {
        Private::Rsa(_) => der::seq(&[der::oid(OID_RSA), der::null()]),
        Private::P256(_) => der::seq(&[der::oid(OID_ECDSA_SHA256)]),
    };
    let signer_info = der::seq(&[
        der::int(&[1]),
        der::seq(&[cert.issuer.to_vec(), der::tlv(0x02, cert.serial)]),
        der::seq(&[der::oid(OID_SHA256), der::null()]),
        sig_alg,
        der::tlv(0x04, &sig),
    ]);
    let signed_data = der::seq(&[
        der::int(&[1]),
        der::set(&[der::seq(&[der::oid(OID_SHA256), der::null()])]),
        der::seq(&[der::oid(&[1, 2, 840, 113549, 1, 7, 1])]),
        der::tlv(0xa0, &key.cert),
        der::set(&[signer_info]),
    ]);
    let block = der::seq(&[
        der::oid(&[1, 2, 840, 113549, 1, 7, 2]),
        der::tlv(0xa0, &signed_data),
    ]);
    let ext = match key.private {
        Private::Rsa(_) => "RSA",
        Private::P256(_) => "EC",
    };
    zip.add(ZipEntry::deflated("META-INF/MANIFEST.MF", mf.into_bytes()));
    zip.add(ZipEntry::deflated("META-INF/CERT.SF", sf.into_bytes()));
    zip.add(ZipEntry::deflated(&format!("META-INF/CERT.{ext}"), block));
}

fn lineage(cfg: &SignConfig) -> Vec<u8> {
    let mut chain: Vec<(&TestKey, u32)> = cfg.rotated_from.iter().map(|(k, f)| (k, *f)).collect();
    chain.push((&cfg.key, 0x1f));
    let mut nodes = Vec::new();
    let mut prev: Option<&TestKey> = None;
    for (key, flags) in chain {
        let prev_alg = prev.map(|p| p.algorithm_id()).unwrap_or(0);
        let mut signed = lp(&key.cert);
        signed.extend_from_slice(&prev_alg.to_le_bytes());
        let sig = prev.map(|p| p.sign_sha256(&signed)).unwrap_or_default();
        let mut node = lp(&signed);
        node.extend_from_slice(&flags.to_le_bytes());
        node.extend_from_slice(&key.algorithm_id().to_le_bytes());
        node.extend(lp(&sig));
        nodes.push(lp(&node));
        prev = Some(key);
    }
    let mut out = 1u32.to_le_bytes().to_vec();
    out.extend(nodes.concat());
    out
}

fn signer(
    key: &TestKey,
    digest: &[u8],
    v3: Option<(u32, u32)>,
    attrs: &[(u32, Vec<u8>)],
) -> Vec<u8> {
    let alg = key.algorithm_id();
    let mut digest_rec = alg.to_le_bytes().to_vec();
    digest_rec.extend(lp(digest));
    let mut sd = lps(&[digest_rec]);
    sd.extend(lps(std::slice::from_ref(&key.cert)));
    if let Some((min, max)) = v3 {
        sd.extend_from_slice(&min.to_le_bytes());
        sd.extend_from_slice(&max.to_le_bytes());
    }
    let attrs: Vec<Vec<u8>> = attrs
        .iter()
        .map(|(id, v)| {
            let mut a = id.to_le_bytes().to_vec();
            a.extend_from_slice(v);
            a
        })
        .collect();
    sd.extend(lps(&attrs));
    let sig = key.sign_sha256(&sd);
    let mut sig_rec = alg.to_le_bytes().to_vec();
    sig_rec.extend(lp(&sig));
    let mut s = lp(&sd);
    if let Some((min, max)) = v3 {
        s.extend_from_slice(&min.to_le_bytes());
        s.extend_from_slice(&max.to_le_bytes());
    }
    s.extend(lps(&[sig_rec]));
    s.extend(lp(&key.spki));
    lps(&[s])
}

/// Signs the archive `zip` holds as configured and returns the APK bytes.
pub fn sign(entries: &[ZipEntry], cfg: &SignConfig) -> Vec<u8> {
    let mut zip = ZipWriter::new();
    for e in entries {
        zip.add(e.clone());
    }
    if cfg.v1 {
        sign_v1(&mut zip, entries, cfg);
    }
    let parts = zip.parts();
    if cfg.strip_block || !(cfg.v2 || cfg.v3 || cfg.v31) {
        return parts.join_with(&[]);
    }
    // The digest does not depend on the block's contents.
    let joined = parts.join_with(&[]);
    let n = parts.entries.len() as u64;
    let digest = block::content_digest(
        &joined,
        n,
        n,
        n + parts.central_directory.len() as u64,
        Hash::Sha256,
    );
    let mut pairs: Vec<(u32, Vec<u8>)> = Vec::new();
    if cfg.v2 {
        let mut attrs = Vec::new();
        if cfg.v3 {
            attrs.push((
                block::STRIPPING_PROTECTION_ATTR,
                3u32.to_le_bytes().to_vec(),
            ));
        }
        pairs.push((
            block::V2_ID,
            signer(cfg.legacy_key(), &digest, None, &attrs),
        ));
    }
    let mut v3_attrs = Vec::new();
    if !cfg.rotated_from.is_empty() {
        v3_attrs.push((block::PROOF_OF_ROTATION_ATTR, lineage(cfg)));
    }
    if cfg.v3 {
        pairs.push((
            block::V3_ID,
            signer(&cfg.key, &digest, Some(cfg.v3_sdk), &v3_attrs),
        ));
    }
    if cfg.v31 {
        pairs.push((
            block::V31_ID,
            signer(&cfg.key, &digest, Some((33, i32::MAX as u32)), &v3_attrs),
        ));
    }
    let mut body = Vec::new();
    for (id, v) in pairs {
        body.extend_from_slice(&((v.len() + 4) as u64).to_le_bytes());
        body.extend_from_slice(&id.to_le_bytes());
        body.extend(v);
    }
    let size = (body.len() + 8 + 16) as u64;
    let mut blk = size.to_le_bytes().to_vec();
    blk.extend(body);
    blk.extend_from_slice(&size.to_le_bytes());
    blk.extend_from_slice(block::MAGIC);

    parts.join_with(&blk)
}
