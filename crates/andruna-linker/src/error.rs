//! Loader errors. The message is what `dlerror` reports, in the wording of
//! bionic's linker where one exists.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    msg: String,
}

pub type Result<T> = core::result::Result<T, Error>;

impl Error {
    pub fn new(msg: impl Into<String>) -> Self {
        Self { msg: msg.into() }
    }

    /// A malformed object: `"<what>" is invalid: <why>`.
    pub fn invalid(what: &str, why: &str) -> Self {
        Self::new(format!("\"{what}\" is invalid: {why}"))
    }

    pub fn not_found(name: &str) -> Self {
        Self::new(format!("library \"{name}\" not found"))
    }

    pub fn symbol(name: &str, by: &str) -> Self {
        Self::new(format!(
            "cannot locate symbol \"{name}\" referenced by \"{by}\""
        ))
    }

    pub fn message(&self) -> &str {
        &self.msg
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Error {}

impl From<andruna_lx::errno::Errno> for Error {
    fn from(e: andruna_lx::errno::Errno) -> Self {
        Self::new(format!("system error {}", e.0))
    }
}
