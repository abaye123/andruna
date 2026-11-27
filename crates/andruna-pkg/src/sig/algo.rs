//! Signature algorithms of the APK Signature Schemes and of JAR signing,
//! verified with RustCrypto implementations.

use rsa::RsaPublicKey;
use rsa::pkcs8::DecodePublicKey;
use rsa::signature::Verifier;
use rsa::signature::hazmat::PrehashVerifier;
use sha1::Sha1;
use sha2::{Digest as _, Sha256, Sha384, Sha512};

use super::asn1::{self, Reader};
use crate::error::{Code, Error, Result};

/// A hash function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha1 => Sha1::digest(data).to_vec(),
            Self::Sha256 => Sha256::digest(data).to_vec(),
            Self::Sha384 => Sha384::digest(data).to_vec(),
            Self::Sha512 => Sha512::digest(data).to_vec(),
        }
    }
}

/// How the signature is computed from the hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    RsaPkcs1,
    RsaPss,
    Ecdsa,
    Dsa,
}

/// An APK Signature Scheme v2/v3 signature algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Algorithm {
    pub id: u32,
    pub kind: Kind,
    pub hash: Hash,
    /// The chunked content digest it signs (SHA-256 or SHA-512 chunks).
    pub content: Hash,
}

/// The algorithm for an ID, or `None` for IDs this crate does not verify:
/// unknown IDs and the verity variants (0x0421, 0x0423, 0x0425), whose
/// digest is a Merkle tree root; signers always carry a non-verity
/// signature next to them.
pub fn algorithm(id: u32) -> Option<Algorithm> {
    let (kind, hash) = match id {
        0x0101 => (Kind::RsaPss, Hash::Sha256),
        0x0102 => (Kind::RsaPss, Hash::Sha512),
        0x0103 => (Kind::RsaPkcs1, Hash::Sha256),
        0x0104 => (Kind::RsaPkcs1, Hash::Sha512),
        0x0201 => (Kind::Ecdsa, Hash::Sha256),
        0x0202 => (Kind::Ecdsa, Hash::Sha512),
        0x0301 => (Kind::Dsa, Hash::Sha256),
        _ => return None,
    };
    Some(Algorithm {
        id,
        kind,
        hash,
        content: hash,
    })
}

fn invalid(m: impl Into<String>) -> Error {
    Error::new(Code::InvalidSignature, m)
}

fn unsupported(m: impl Into<String>) -> Error {
    Error::new(Code::UnsupportedSignature, m)
}

const OID_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 1];
const OID_EC: &[u64] = &[1, 2, 840, 10045, 2, 1];
const OID_DSA: &[u64] = &[1, 2, 840, 10040, 4, 1];
const OID_P256: &[u64] = &[1, 2, 840, 10045, 3, 1, 7];
const OID_P384: &[u64] = &[1, 3, 132, 0, 34];
const OID_P521: &[u64] = &[1, 3, 132, 0, 35];

/// The key type of a SubjectPublicKeyInfo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyType {
    Rsa,
    P256,
    P384,
    P521,
    Dsa,
    Other,
}

pub fn key_type(spki: &[u8]) -> Option<KeyType> {
    let mut r = Reader(spki);
    let s = r.expect(asn1::SEQUENCE)?;
    let mut s = s.reader();
    let alg = s.expect(asn1::SEQUENCE)?;
    let mut a = alg.reader();
    let oid = a.expect(asn1::OID)?.value;
    Some(if oid == asn1::oid(OID_RSA) {
        KeyType::Rsa
    } else if oid == asn1::oid(OID_DSA) {
        KeyType::Dsa
    } else if oid == asn1::oid(OID_EC) {
        let curve = a.expect(asn1::OID)?.value;
        if curve == asn1::oid(OID_P256) {
            KeyType::P256
        } else if curve == asn1::oid(OID_P384) {
            KeyType::P384
        } else if curve == asn1::oid(OID_P521) {
            KeyType::P521
        } else {
            KeyType::Other
        }
    } else {
        KeyType::Other
    })
}

/// Verifies `sig` over `data` with the public key `spki` (DER
/// SubjectPublicKeyInfo).
pub fn verify(kind: Kind, hash: Hash, spki: &[u8], data: &[u8], sig: &[u8]) -> Result<()> {
    let kt = key_type(spki).ok_or_else(|| invalid("public key is malformed"))?;
    let bad_sig = || invalid("signature does not verify");
    match (kind, kt) {
        (Kind::RsaPkcs1 | Kind::RsaPss, KeyType::Rsa) => {
            let key = RsaPublicKey::from_public_key_der(spki)
                .map_err(|e| invalid(format!("RSA public key is malformed: {e}")))?;
            macro_rules! rsa_verify {
                ($h:ty) => {{
                    if kind == Kind::RsaPkcs1 {
                        let vk = rsa::pkcs1v15::VerifyingKey::<$h>::new(key);
                        let s = rsa::pkcs1v15::Signature::try_from(sig).map_err(|_| bad_sig())?;
                        vk.verify(data, &s).map_err(|_| bad_sig())
                    } else {
                        let vk = rsa::pss::VerifyingKey::<$h>::new(key);
                        let s = rsa::pss::Signature::try_from(sig).map_err(|_| bad_sig())?;
                        vk.verify(data, &s).map_err(|_| bad_sig())
                    }
                }};
            }
            match hash {
                Hash::Sha1 => rsa_verify!(Sha1),
                Hash::Sha256 => rsa_verify!(Sha256),
                Hash::Sha384 => rsa_verify!(Sha384),
                Hash::Sha512 => rsa_verify!(Sha512),
            }
        }
        (Kind::Ecdsa, KeyType::P256 | KeyType::P384 | KeyType::P521) => {
            let prehash = hash.digest(data);
            match kt {
                KeyType::P256 => {
                    let vk = p256::ecdsa::VerifyingKey::from_public_key_der(spki)
                        .map_err(|_| invalid("EC public key is malformed"))?;
                    let s = p256::ecdsa::Signature::from_der(sig).map_err(|_| bad_sig())?;
                    vk.verify_prehash(&prehash, &s).map_err(|_| bad_sig())
                }
                KeyType::P384 => {
                    let vk = p384::ecdsa::VerifyingKey::from_public_key_der(spki)
                        .map_err(|_| invalid("EC public key is malformed"))?;
                    let s = p384::ecdsa::Signature::from_der(sig).map_err(|_| bad_sig())?;
                    vk.verify_prehash(&prehash, &s).map_err(|_| bad_sig())
                }
                _ => {
                    let vk = p521::ecdsa::VerifyingKey::from_sec1_bytes(&ec_point(spki)?)
                        .map_err(|_| invalid("EC public key is malformed"))?;
                    let s = p521::ecdsa::Signature::from_der(sig).map_err(|_| bad_sig())?;
                    vk.verify_prehash(&prehash, &s).map_err(|_| bad_sig())
                }
            }
        }
        (Kind::Dsa, KeyType::Dsa) => Err(unsupported("DSA signatures are not supported")),
        (_, KeyType::Other) => Err(unsupported("the signer's key type is not supported")),
        _ => Err(invalid(
            "the signature algorithm does not match the key type",
        )),
    }
}

/// The EC point (BIT STRING contents without the unused-bits byte).
fn ec_point(spki: &[u8]) -> Result<Vec<u8>> {
    let mut r = Reader(spki);
    let mut s = r
        .expect(asn1::SEQUENCE)
        .ok_or_else(|| invalid("public key is malformed"))?
        .reader();
    s.expect(asn1::SEQUENCE);
    let bits = s
        .expect(asn1::BIT_STRING)
        .ok_or_else(|| invalid("public key is malformed"))?;
    Ok(bits.value.get(1..).unwrap_or_default().to_vec())
}
