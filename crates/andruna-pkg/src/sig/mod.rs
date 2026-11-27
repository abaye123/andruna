//! APK signature verification.
//!
//! Placeholder: the verifier (schemes v1, v2, v3, v3.1 with apksig
//! semantics) is being written; until it lands every APK is rejected with
//! [`Code::UnsupportedSignature`].

use serde::{Deserialize, Serialize};

use crate::error::{Code, Result, err};
use crate::zip::Archive;

/// The highest signature scheme that verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scheme {
    V1,
    V2,
    V3,
    V31,
}

/// One certificate in a signing lineage (v3 proof of rotation), oldest
/// first, with the capabilities the newer signers granted it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageEntry {
    /// SHA-256 of the DER certificate, lowercase hex.
    pub cert_sha256: String,
    /// Capability flags as in apksig (`installed data` = 1,
    /// `shared user id` = 2, `permission` = 4, `rollback` = 8, `auth` = 16).
    pub flags: u32,
}

/// What verification established about who signed a package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigningDetails {
    pub scheme: Scheme,
    /// SHA-256 of each signer's DER certificate, lowercase hex. Android
    /// packages normally have exactly one.
    pub signers: Vec<String>,
    /// The signers' DER certificates, in the order of `signers`.
    pub certificates: Vec<Vec<u8>>,
    /// The v3 rotation lineage, oldest first, ending with the current
    /// signer; empty without rotation.
    pub lineage: Vec<LineageEntry>,
}

impl SigningDetails {
    /// Whether a package signed as `self` may update one installed with
    /// `old`: same signer set, or `old`'s signer is in `self`'s lineage with
    /// the installed-data capability.
    pub fn can_update(&self, old: &SigningDetails) -> bool {
        if self.signers == old.signers {
            return true;
        }
        if old.signers.len() != 1 {
            return false;
        }
        self.lineage
            .iter()
            .any(|e| e.cert_sha256 == old.signers[0] && e.flags & 1 != 0)
    }
}

/// Options of [`verify`].
#[derive(Clone, Copy, Debug)]
pub struct VerifyOptions {
    /// The package's `minSdkVersion`.
    pub min_sdk: u32,
    /// The platform's API level.
    pub platform_sdk: u32,
}

/// Verifies the APK's signatures as Android would install it on
/// `platform_sdk`.
pub fn verify(_apk: &Archive<'_>, _opts: VerifyOptions) -> Result<SigningDetails> {
    err(
        Code::UnsupportedSignature,
        "signature verification is not implemented yet",
    )
}
