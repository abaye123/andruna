//! APK signature verification with apksig semantics.
//!
//! Schemes, as Android installs on API 37 checks them:
//!
//! 1. The APK Signing Block ([`block`]): v3.1 (for platforms from 33), v3
//!    (from 28) and v2. Every block present is verified; the result names
//!    the highest scheme whose signer covers the platform version. A v2
//!    signer whose stripping-protection attribute names v3 requires the v3
//!    block.
//! 2. Without a signing block: JAR signing ([`v1`]). An `.SF` file whose
//!    `X-Android-APK-Signed` names scheme 2 or 3 means a signing block was
//!    stripped, and the APK is rejected.
//!
//! Not implemented: DSA keys, the verity algorithm variants (always
//! accompanied by a non-verity signature), v4 (`.idsig` files) and
//! source-stamp checks. Certificates are not checked for validity dates or
//! chains, as on Android.

pub mod algo;
pub mod asn1;
pub mod block;
pub mod v1;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::{Code, Error, Result};
use crate::zip::Archive;

/// The scheme that established the signer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scheme {
    V1,
    V2,
    V3,
    V31,
}

impl Scheme {
    /// 1, 2, 3 or 31.
    pub fn number(self) -> u8 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
            Self::V3 => 3,
            Self::V31 => 31,
        }
    }
}

/// One certificate in a signing lineage (v3 proof of rotation), oldest
/// first, with the capabilities the newer signers granted it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageEntry {
    /// SHA-256 of the DER certificate, lowercase hex.
    pub cert_sha256: String,
    /// Capability flags as in apksig (installed data 1, shared user ID 2,
    /// permission 4, rollback 8, auth 16).
    pub flags: u32,
}

/// What verification established about who signed a package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigningDetails {
    pub scheme: Scheme,
    /// SHA-256 of each signer's DER certificate, lowercase hex, sorted.
    /// Android packages normally have exactly one.
    pub signers: Vec<String>,
    /// The signers' DER certificates, in the order of `signers`.
    #[serde(skip)]
    pub certificates: Vec<Vec<u8>>,
    /// The v3 rotation lineage, oldest first, ending with the current
    /// signer; empty without rotation.
    pub lineage: Vec<LineageEntry>,
}

/// Capability flag: the newer signer may take over the installed data.
pub const CAP_INSTALLED_DATA: u32 = 1;

impl SigningDetails {
    /// Whether a package signed as `self` may update one installed with
    /// `old`: the same signer set, or (with one signer) `old`'s signer is in
    /// `self`'s lineage with the installed-data capability. Rotation
    /// backwards (the update signed by a key that the installed lineage
    /// rotated away from) is refused, as without the rollback capability
    /// on Android.
    pub fn can_update(&self, old: &SigningDetails) -> bool {
        if self.signers == old.signers {
            return true;
        }
        if old.signers.len() != 1 || self.signers.len() != 1 {
            return false;
        }
        self.lineage
            .iter()
            .any(|e| e.cert_sha256 == old.signers[0] && e.flags & CAP_INSTALLED_DATA != 0)
    }
}

/// Options of [`verify`].
#[derive(Clone, Copy, Debug)]
pub struct VerifyOptions {
    /// The platform's API level.
    pub platform_sdk: u32,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            platform_sdk: crate::PLATFORM_SDK,
        }
    }
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// SHA-256 of a certificate, lowercase hex.
pub fn cert_sha256(der: &[u8]) -> String {
    hex(&Sha256::digest(der))
}

fn details(scheme: Scheme, certs: Vec<Vec<u8>>, lineage: Vec<LineageEntry>) -> SigningDetails {
    let mut pairs: Vec<(String, Vec<u8>)> =
        certs.into_iter().map(|c| (cert_sha256(&c), c)).collect();
    pairs.sort();
    pairs.dedup_by(|a, b| a.0 == b.0);
    let (signers, certificates) = pairs.into_iter().unzip();
    SigningDetails {
        scheme,
        signers,
        certificates,
        lineage,
    }
}

/// Verifies the APK's signatures as Android would install it.
pub fn verify(apk: &Archive<'_>, opts: VerifyOptions) -> Result<SigningDetails> {
    let sdk = opts.platform_sdk;
    if let Some(b) = block::find(apk)? {
        let mut contents = block::Contents::new(apk, b.offset);
        let mut best: Option<(Scheme, block::VerifiedSigner)> = None;
        if sdk >= 33
            && let Some(v) = b.pairs.get(&block::V31_ID)
            && let Some(s) =
                block::verify_v3(v, sdk, &mut contents).map_err(|e| e.context("v3.1"))?
        {
            best = Some((Scheme::V31, s));
        }
        if sdk >= 28
            && let Some(v) = b.pairs.get(&block::V3_ID)
            && let Some(s) = block::verify_v3(v, sdk, &mut contents).map_err(|e| e.context("v3"))?
            && best.is_none()
        {
            best = Some((Scheme::V3, s));
        }
        let v2 = match b.pairs.get(&block::V2_ID) {
            Some(v) => Some(block::verify_v2(v, &mut contents)?),
            None => None,
        };
        if let Some(v2) = &v2 {
            for s in v2 {
                for (id, val) in &s.attributes {
                    if *id == block::STRIPPING_PROTECTION_ATTR
                        && val.get(..4) == Some(&3u32.to_le_bytes()[..])
                        && !b.pairs.contains_key(&block::V3_ID)
                    {
                        return Err(Error::new(
                            Code::InvalidSignature,
                            "the v2 signature says the APK was signed with v3, but the v3 signature is missing",
                        ));
                    }
                }
            }
        }
        if let Some((scheme, s)) = best {
            let mut lineage = Vec::new();
            if let Some((_, value)) = s
                .attributes
                .iter()
                .find(|(id, _)| *id == block::PROOF_OF_ROTATION_ATTR)
            {
                let (entries, last) = block::lineage(value)?;
                if last != s.certificate {
                    return Err(Error::new(
                        Code::InvalidSignature,
                        "the rotation lineage does not end with the signer's certificate",
                    ));
                }
                lineage = entries;
            }
            // The v2 signer (what older platforms see) must be the v3
            // signer or an ancestor in its lineage.
            if let Some(v2) = &v2 {
                let v3_sha = cert_sha256(&s.certificate);
                for v2s in v2 {
                    let sha = cert_sha256(&v2s.certificate);
                    if sha != v3_sha && !lineage.iter().any(|e| e.cert_sha256 == sha) {
                        return Err(Error::new(
                            Code::InvalidSignature,
                            "the v3 signer differs from the v2 signer without a signing certificate lineage",
                        ));
                    }
                }
            }
            return Ok(details(scheme, vec![s.certificate], lineage));
        }
        if let Some(v2) = v2 {
            return Ok(details(
                Scheme::V2,
                v2.into_iter().map(|s| s.certificate).collect(),
                Vec::new(),
            ));
        }
    }
    match v1::verify(apk)? {
        Some(v) => {
            if v.apk_signed_schemes.iter().any(|&s| s == 2 || s == 3) {
                return Err(Error::new(
                    Code::InvalidSignature,
                    "the JAR signature says the APK was signed with scheme v2 or v3, but that signature is missing",
                ));
            }
            Ok(details(Scheme::V1, v.certificates, Vec::new()))
        }
        None => Err(Error::new(Code::NoSignature, "the APK is not signed")),
    }
}
