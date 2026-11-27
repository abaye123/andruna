//! The package manager requests, served by andruna-pkg's store.

use std::path::PathBuf;

use andruna_ipc::{
    CompileStatus, ErrorCode, InstallOutcome, PackageDetails, PackageInfo, Response,
    is_valid_package_name,
};
use andruna_pkg::Code;
use andruna_pkg::compile::Outcome;
use andruna_pkg::desktop::app_user_model_id;
use andruna_pkg::store::Store;

use crate::server::Context;
use crate::{info, warn};

/// The pipe error code for a package manager error code.
pub fn error_code(c: Code) -> ErrorCode {
    match c {
        Code::InvalidArchive | Code::Alignment | Code::InvalidManifest | Code::InvalidResources => {
            ErrorCode::InvalidPackage
        }
        Code::NoSignature | Code::InvalidSignature | Code::UnsupportedSignature => {
            ErrorCode::InvalidSignature
        }
        Code::SignerMismatch => ErrorCode::SignerMismatch,
        Code::InvalidSplits => ErrorCode::InvalidSplits,
        Code::SdkTooNew => ErrorCode::SdkTooNew,
        Code::IncompatibleAbi => ErrorCode::IncompatibleAbi,
        Code::Downgrade => ErrorCode::Downgrade,
        Code::NotInstalled => ErrorCode::NotInstalled,
        Code::Io | Code::Database | Code::CompileFailed | Code::Desktop => ErrorCode::InstallFailed,
    }
}

fn refuse(e: andruna_pkg::Error) -> Response {
    Response::error(error_code(e.code), e.to_string())
}

/// Paths from a client must be absolute: sysd's working directory is not
/// the client's.
fn paths(raw: &[String]) -> Result<Vec<PathBuf>, Box<Response>> {
    if raw.is_empty() {
        return Err(Box::new(Response::error(
            ErrorCode::InvalidSplits,
            "no APK was given",
        )));
    }
    raw.iter()
        .map(|p| {
            let pb = PathBuf::from(p);
            if pb.is_absolute() {
                Ok(pb)
            } else {
                Err(Box::new(Response::error(
                    ErrorCode::Malformed,
                    format!("`{p}` is not an absolute path"),
                )))
            }
        })
        .collect()
}

/// The locale for labels: the client's, else the Windows UI language.
fn locale(l: &str) -> String {
    if l.is_empty() {
        match crate::l10n::Lang::current() {
            crate::l10n::Lang::He => "he".into(),
            crate::l10n::Lang::En => "en".into(),
        }
    } else {
        l.to_string()
    }
}

fn label_of(labels: &[(String, String)], locale: &str, package: &str) -> String {
    andruna_pkg::apk::label_for(labels, locale).unwrap_or_else(|| package.to_string())
}

pub fn inspect(store: &Store, raw: &[String], loc: &str) -> Response {
    let paths = match paths(raw) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    match store.inspect(&paths, &locale(loc)) {
        Ok(i) => {
            let m = &i.set.base.manifest;
            Response::PackageDetails(PackageDetails {
                package: m.package.clone(),
                version_code: m.version_code,
                version_name: m.version_name.clone().unwrap_or_default(),
                label: i.label,
                min_sdk: m.min_sdk,
                target_sdk: m.target_sdk,
                signer_sha256: i
                    .set
                    .base
                    .signing
                    .signers
                    .first()
                    .cloned()
                    .unwrap_or_default(),
                signature_scheme: i.set.base.signing.scheme.number(),
                permissions: m.permissions.iter().map(|p| p.name.clone()).collect(),
                native_abis: i.set.abis(),
                selected_abi: i.set.abi.clone().unwrap_or_default(),
                installed_version_code: i.installed_version_code.unwrap_or(0),
                icon_png: i
                    .icon_png
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            })
        }
        Err(e) => {
            info!(
                "inspect_refused",
                code = e.code.as_str(),
                reason = e.message
            );
            refuse(e)
        }
    }
}

pub fn install(ctx: &Context, raw: &[String]) -> Response {
    let paths = match paths(raw) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    info!("install_started", apks = paths.len());
    match ctx.store.install(&paths) {
        Ok(r) => {
            for w in &r.warnings {
                warn!("install_warning", package = r.record.package, detail = w);
            }
            info!(
                "installed",
                package = r.record.package,
                version_code = r.record.version_code,
                updated = r.updated,
                compile = r.compile.as_str(),
                compile_detail = r.compile.message(),
                shortcuts = r.shortcuts
            );
            Response::Installed(InstallOutcome {
                package: r.record.package.clone(),
                version_code: r.record.version_code,
                version_name: r.record.version_name.clone(),
                updated: r.updated,
                compile: match r.compile {
                    Outcome::Compiled => CompileStatus::Compiled,
                    Outcome::NoCode => CompileStatus::NoCode,
                    Outcome::Unavailable(_) => CompileStatus::Unavailable,
                    Outcome::Failed(_) => CompileStatus::Failed,
                },
                compile_message: r.compile.message().to_string(),
                shortcuts: r.shortcuts as u32,
            })
        }
        Err(e) => {
            info!(
                "install_refused",
                code = e.code.as_str(),
                reason = e.message
            );
            if e.code == Code::Io && e.message.contains("is the app running") {
                return Response::error(ErrorCode::Busy, e.to_string());
            }
            refuse(e)
        }
    }
}

pub fn uninstall(ctx: &Context, package: &str, keep_data: bool) -> Response {
    if !is_valid_package_name(package) {
        return Response::error(
            ErrorCode::InvalidPackageName,
            format!("`{package}` is not a valid package name"),
        );
    }
    if ctx.supervisor.host_pid(package).is_some() {
        return Response::error(
            ErrorCode::Busy,
            format!("{package} is running; close it first"),
        );
    }
    match ctx.store.uninstall(package, keep_data) {
        Ok(_) => {
            info!("uninstalled", package = package, keep_data = keep_data);
            Response::Uninstalled
        }
        Err(e) => {
            info!(
                "uninstall_refused",
                package = package,
                code = e.code.as_str(),
                reason = e.message
            );
            refuse(e)
        }
    }
}

pub fn list_info(store: &Store, loc: &str) -> Response {
    let loc = locale(loc);
    let packages = store
        .list()
        .into_iter()
        .map(|r| {
            let dir = store.app_dir(&r.package);
            let icon = |name: &str| {
                if r.has_icon {
                    dir.join(name).display().to_string()
                } else {
                    String::new()
                }
            };
            PackageInfo {
                label: label_of(&r.labels, &loc, &r.package),
                version_code: r.version_code,
                version_name: r.version_name.clone(),
                icon_ico: icon("icon.ico"),
                icon_png: icon("icon.png"),
                launcher_activities: r.launchers.iter().map(|l| l.activity.clone()).collect(),
                installed_ms: r.updated_ms,
                size_bytes: r.size_bytes,
                app_user_model_id: app_user_model_id(&r.package),
                package: r.package,
            }
        })
        .collect();
    Response::PackageInfos { packages }
}

/// The package manager's install hooks: each package gets its AppContainer
/// profile and directory security at install ([`crate::appcontainer`]),
/// from the per-install salt stored in its record; the profile is deleted
/// at uninstall.
pub struct AppContainerHooks {
    pub layout: crate::Layout,
}

impl andruna_pkg::hooks::InstallHooks for AppContainerHooks {
    fn installed(&self, p: &andruna_pkg::hooks::InstalledPackage<'_>) -> Result<(), String> {
        let salt =
            crate::appcontainer::Salt::parse(&p.record.install_salt).map_err(|e| e.to_string())?;
        crate::appcontainer::install(&self.layout, &p.record.package, &salt)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn removed(&self, record: &andruna_pkg::db::PackageRecord, _keep_data: bool) {
        // With keep_data the profile goes too: a reinstall gets a new salt
        // and so a new identity, and re-secures the kept data directory
        // for it.
        let result = crate::appcontainer::Salt::parse(&record.install_salt)
            .and_then(|salt| crate::appcontainer::uninstall(&record.package, &salt));
        if let Err(e) = result {
            warn!(
                "appcontainer_delete_failed",
                package = record.package,
                error = e
            );
        }
    }
}

pub fn list_names(store: &Store) -> Response {
    Response::Packages {
        packages: store.list().into_iter().map(|r| r.package).collect(),
    }
}
