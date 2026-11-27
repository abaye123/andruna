//! Hooks for other modules into install and uninstall.
//!
//! The AppContainer work (spike S7, security spec "one AppContainer per
//! package") plugs in here: at install it creates the package's
//! AppContainer profile (name derived from the package name and
//! [`PackageRecord::install_salt`]) and grants the container access to the
//! package's directories; at uninstall it deletes the profile. sysd passes
//! its implementation in [`crate::store::StoreConfig::hooks`].
//!
//! Order at install: the package's files are complete in their final
//! directory and the data directory exists; [`InstallHooks::installed`]
//! runs; then Start menu entries and the database record are written. A
//! hook error rolls the install back (an update restores the previous
//! version) and fails it with the hook's message. At uninstall,
//! [`InstallHooks::removed`] runs after the database record is gone and
//! before the files are deleted.

use std::path::Path;

use crate::db::PackageRecord;

/// A package whose files are in place.
pub struct InstalledPackage<'a> {
    pub record: &'a PackageRecord,
    /// `app\<package>\`: APKs, native libraries, compiled code, icon.
    pub app_dir: &'a Path,
    /// `data\<package>\`: the app's private data (kept across updates).
    pub data_dir: &'a Path,
    /// True when an installed version is being replaced.
    pub update: bool,
}

pub trait InstallHooks: Send + Sync {
    /// Called when a package's files are in place, before it is recorded.
    fn installed(&self, _p: &InstalledPackage<'_>) -> Result<(), String> {
        Ok(())
    }

    /// Called when a package is removed (also with `keep_data`).
    fn removed(&self, _record: &PackageRecord, _keep_data: bool) {}
}

/// No hooks.
pub struct NoHooks;

impl InstallHooks for NoHooks {}
