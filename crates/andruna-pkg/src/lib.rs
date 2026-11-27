//! `andruna-pkg`: the package manager backend used by andruna-sysd.
//!
//! [components](../../docs/spec/03-components.md#andruna-pkg-package-manager-backend):
//! APK, split and bundle handling, signature verification, ABI and split
//! selection, dex2oat at install, Start menu shortcut, icon,
//! AppUserModelID and uninstall entry. See README.md and
//! `docs/plan/phase2/pkg.md`.
//!
//! - [`zip`]: the archive reader (with the checks Android applies).
//! - [`axml`]: Android binary XML (`AndroidManifest.xml`, compiled XML
//!   resources).
//! - [`arsc`]: the resource table (`resources.arsc`) and resource lookup by
//!   configuration (locale, density).
//! - [`manifest`]: the manifest model (package, versions, SDKs,
//!   activities, permissions).
//! - [`sig`]: APK signature verification (schemes v1, v2, v3, v3.1).
//! - [`icon`]: the launcher icon rendered to `.ico` and `.png`.
//! - `sys`: Windows API wrappers; all `unsafe` code of the crate.

pub mod arsc;
pub mod axml;
pub mod error;
pub mod icon;
pub mod manifest;
pub mod sig;
pub mod sys;
pub mod zip;

#[cfg(feature = "testkit")]
pub mod testkit;
#[cfg(all(test, not(feature = "testkit")))]
compile_error!("andruna-pkg's tests need the `testkit` feature (enabled by its dev-dependency)");

pub use error::{Code, Error, Result};

/// The Android API level Andruna presents (ADR 0019:
/// `ro.build.version.sdk=37`).
pub const PLATFORM_SDK: u32 = 37;
