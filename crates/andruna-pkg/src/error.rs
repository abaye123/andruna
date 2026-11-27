//! Errors of the package manager, with stable codes.
//!
//! Every rejection of a package names one [`Code`]. The codes are stable:
//! sysd maps them to pipe protocol error codes and the manager maps those to
//! user-facing text. The message is English detail for logs and
//! diagnostics, never shown alone to a user.

use std::fmt;
use std::io;

/// Why an operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    /// Reading or writing a file failed.
    Io,
    /// The file is not a zip archive an APK can be, or the archive is
    /// damaged (bad central directory, overlapping entries, bad CRC,
    /// unsupported compression, zip64, duplicate entry names).
    InvalidArchive,
    /// An entry is not aligned as Android requires (for example
    /// `resources.arsc` compressed or not 4-byte aligned in an APK that
    /// targets API 30 or later).
    Alignment,
    /// `AndroidManifest.xml` is missing or is not valid binary XML, or a
    /// required attribute is missing or invalid.
    InvalidManifest,
    /// `resources.arsc` is malformed.
    InvalidResources,
    /// The APK has no signature of a supported scheme.
    NoSignature,
    /// A signature, digest or certificate does not verify.
    InvalidSignature,
    /// The APK uses a signature algorithm or scheme feature that is not
    /// implemented.
    UnsupportedSignature,
    /// An update is not signed by the installed package's signer (or a
    /// signer the installed one rotated to).
    SignerMismatch,
    /// The set of APKs (base and splits) does not form one package: package
    /// names, version codes or signers differ, a split is missing or
    /// duplicated.
    InvalidSplits,
    /// The package needs a newer Android API level than Andruna provides.
    SdkTooNew,
    /// The package contains native code only for CPU architectures this PC
    /// cannot run.
    IncompatibleAbi,
    /// The update has a lower version code than the installed package.
    Downgrade,
    /// The package is not installed.
    NotInstalled,
    /// The package database is damaged or of an unknown version.
    Database,
    /// The ahead-of-time compilation step failed.
    CompileFailed,
    /// Creating Start menu shortcuts, icons or the uninstall entry failed.
    Desktop,
}

impl Code {
    /// A short stable identifier, used in logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::InvalidArchive => "invalid_archive",
            Self::Alignment => "alignment",
            Self::InvalidManifest => "invalid_manifest",
            Self::InvalidResources => "invalid_resources",
            Self::NoSignature => "no_signature",
            Self::InvalidSignature => "invalid_signature",
            Self::UnsupportedSignature => "unsupported_signature",
            Self::SignerMismatch => "signer_mismatch",
            Self::InvalidSplits => "invalid_splits",
            Self::SdkTooNew => "sdk_too_new",
            Self::IncompatibleAbi => "incompatible_abi",
            Self::Downgrade => "downgrade",
            Self::NotInstalled => "not_installed",
            Self::Database => "database",
            Self::CompileFailed => "compile_failed",
            Self::Desktop => "desktop",
        }
    }
}

/// An error with its [`Code`] and an English detail message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: Code,
    pub message: String,
}

impl Error {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Prefixes the message with context (for example the file name).
    pub fn context(mut self, what: impl fmt::Display) -> Self {
        self.message = format!("{what}: {}", self.message);
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code.as_str())
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::new(Code::Io, e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Shorthand for an error of a given code.
pub(crate) fn err<T>(code: Code, message: impl Into<String>) -> Result<T> {
    Err(Error::new(code, message))
}
