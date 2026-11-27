//! Windows integration of an installed package: Start menu shortcuts with
//! the package's AppUserModelID and icon, and an entry in Windows
//! "Installed apps" (ADR 0027, proposed).
//!
//! - One shortcut per launcher activity, in the Start menu's `Programs`
//!   folder, named after the label. It starts `andruna-launch.exe
//!   <package>`; shortcuts of further launcher activities pass
//!   `--activity <class>` too.
//! - Every shortcut carries the AppUserModelID [`app_user_model_id`], so
//!   the app's windows (which set the same ID, D-5) group under its
//!   taskbar button and pinning works.
//! - The uninstall entry `HKCU\...\Uninstall\Andruna.App.<package>` makes
//!   the app appear in "Installed apps"; uninstalling there runs
//!   `andruna-launch.exe --uninstall <package>`, which asks for
//!   confirmation and sends the uninstall request to sysd.

use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::db::PackageRecord;
use crate::error::{Code, Error, Result};
use crate::sys::{self, RegValue, Shortcut};

/// The registry key (under `HKEY_CURRENT_USER`) of the product's
/// uninstall entries.
pub const UNINSTALL_ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

const AUMID_PREFIX: &str = "Andruna.App.";
/// AppUserModelIDs are limited to 128 characters.
const AUMID_MAX: usize = 128;

/// The AppUserModelID of a package: `Andruna.App.<package>`, or for names
/// that would exceed 128 characters, a prefix of the name and a hash.
pub fn app_user_model_id(package: &str) -> String {
    let id = format!("{AUMID_PREFIX}{package}");
    if id.len() <= AUMID_MAX {
        return id;
    }
    let hash: String = Sha256::digest(package.as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let keep = AUMID_MAX - AUMID_PREFIX.len() - 1 - hash.len();
    format!("{AUMID_PREFIX}{}.{hash}", &package[..keep])
}

/// A label made safe as a file name: no reserved characters, no trailing
/// dots or spaces, not a reserved device name, at most 80 characters.
pub fn file_name_for(label: &str, fallback: &str) -> String {
    let mut s: String = label
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
        .take(80)
        .collect();
    while s.ends_with(['.', ' ']) {
        s.pop();
    }
    let s = s.trim_start().to_string();
    let upper = s.to_ascii_uppercase();
    let base = upper.split('.').next().unwrap_or("");
    let reserved = matches!(base, "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && base.as_bytes()[3].is_ascii_digit());
    if s.is_empty() || reserved {
        fallback.to_string()
    } else {
        s
    }
}

/// Where and how the integration is written.
#[derive(Clone, Debug)]
pub struct Desktop {
    /// The folder for shortcuts (the Start menu's `Programs` in the
    /// product; a test directory in tests).
    pub shortcuts_dir: PathBuf,
    /// The registry key under `HKEY_CURRENT_USER` that holds uninstall
    /// entries; `None` writes none.
    pub uninstall_root: Option<String>,
    /// `andruna-launch.exe`.
    pub launch_exe: PathBuf,
}

/// A launcher to make a shortcut for.
pub struct LauncherShortcut<'a> {
    pub activity: &'a str,
    pub name: String,
    /// Whether this is the package's first launcher (started without
    /// `--activity`).
    pub primary: bool,
}

impl Desktop {
    /// The product configuration: the user's Start menu and the Windows
    /// uninstall key.
    pub fn for_current_user(launch_exe: PathBuf) -> Result<Self> {
        Ok(Self {
            shortcuts_dir: sys::programs_folder()
                .map_err(|e| Error::new(Code::Desktop, e.to_string()))?,
            uninstall_root: Some(UNINSTALL_ROOT.into()),
            launch_exe,
        })
    }

    /// Creates the shortcuts; returns the path written for each launcher.
    /// `taken` are shortcut paths this package owned before (they may be
    /// overwritten).
    pub fn create_shortcuts(
        &self,
        package: &str,
        launchers: &[LauncherShortcut<'_>],
        icon: Option<&Path>,
        description: &str,
        taken: &[PathBuf],
    ) -> Result<Vec<PathBuf>> {
        let err = |e: std::io::Error| Error::new(Code::Desktop, e.to_string());
        std::fs::create_dir_all(&self.shortcuts_dir).map_err(err)?;
        let aumid = app_user_model_id(package);
        let mut out: Vec<PathBuf> = Vec::new();
        for l in launchers {
            let base = file_name_for(&l.name, package);
            let mut path = self.shortcuts_dir.join(format!("{base}.lnk"));
            let mut n = 1;
            // Do not overwrite a shortcut that is not this package's.
            while (path.exists() && !taken.contains(&path)) || out.contains(&path) {
                n += 1;
                let suffix = if n == 2 {
                    " (Android)".to_string()
                } else {
                    format!(" (Android {n})")
                };
                path = self.shortcuts_dir.join(format!("{base}{suffix}.lnk"));
            }
            let arguments = if l.primary {
                package.to_string()
            } else {
                format!("--activity {} {package}", l.activity)
            };
            sys::write_shortcut(
                &path,
                &Shortcut {
                    target: self.launch_exe.clone(),
                    arguments,
                    icon: icon.map(Path::to_path_buf),
                    description: description.to_string(),
                    app_user_model_id: aumid.clone(),
                },
            )
            .map_err(|e| Error::new(Code::Desktop, format!("{}: {e}", path.display())))?;
            out.push(path);
        }
        Ok(out)
    }

    /// Removes shortcuts that a package created.
    pub fn remove_shortcuts(&self, paths: &[PathBuf]) {
        for p in paths {
            // Only files inside the shortcuts folder are removed.
            if p.parent() == Some(self.shortcuts_dir.as_path()) {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    /// The uninstall entry key of a package.
    pub fn uninstall_key(&self, package: &str) -> Option<String> {
        self.uninstall_root
            .as_ref()
            .map(|r| format!(r"{r}\{}", app_user_model_id(package)))
    }

    /// Writes the "Installed apps" entry; returns its key.
    pub fn write_uninstall_entry(
        &self,
        rec: &PackageRecord,
        label: &str,
        icon: Option<&Path>,
        app_dir: &Path,
    ) -> Result<Option<String>> {
        let Some(key) = self.uninstall_key(&rec.package) else {
            return Ok(None);
        };
        let uninstall = format!(
            "\"{}\" --uninstall {}",
            self.launch_exe.display(),
            rec.package
        );
        let icon = icon.map(|p| p.display().to_string()).unwrap_or_default();
        let location = app_dir.display().to_string();
        let date = yyyymmdd(rec.updated_ms);
        let size_kb = (rec.size_bytes / 1024).min(u32::MAX as u64) as u32;
        let mut values: Vec<(&str, RegValue<'_>)> = vec![
            ("DisplayName", RegValue::Str(label)),
            ("DisplayVersion", RegValue::Str(&rec.version_name)),
            ("Publisher", RegValue::Str("Android app (Andruna)")),
            ("UninstallString", RegValue::Str(&uninstall)),
            ("InstallLocation", RegValue::Str(&location)),
            ("InstallDate", RegValue::Str(&date)),
            ("NoModify", RegValue::Dword(1)),
            ("NoRepair", RegValue::Dword(1)),
            ("EstimatedSize", RegValue::Dword(size_kb)),
        ];
        if !icon.is_empty() {
            values.push(("DisplayIcon", RegValue::Str(&icon)));
        }
        sys::write_user_key(&key, &values)
            .map_err(|e| Error::new(Code::Desktop, format!("{key}: {e}")))?;
        Ok(Some(key))
    }

    pub fn remove_uninstall_entry(&self, key: &str) {
        // Only keys under the configured root are removed.
        if self
            .uninstall_root
            .as_ref()
            .is_some_and(|r| key.starts_with(r.as_str()))
        {
            let _ = sys::delete_user_key(key);
        }
    }
}

/// A Unix time in milliseconds as `YYYYMMDD` (UTC).
pub fn yyyymmdd(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aumids_fit() {
        assert_eq!(
            app_user_model_id("com.example.app"),
            "Andruna.App.com.example.app"
        );
        let long = format!("a.{}", "b".repeat(250));
        let id = app_user_model_id(&long);
        assert_eq!(id.len(), AUMID_MAX);
        assert!(id.starts_with("Andruna.App.a.bbb"));
        assert_ne!(id, app_user_model_id(&format!("a.{}", "b".repeat(249))));
    }

    #[test]
    fn file_names() {
        assert_eq!(file_name_for("My: App?", "x.y"), "My App");
        assert_eq!(file_name_for("  ...", "x.y"), "x.y");
        assert_eq!(file_name_for("CON", "x.y"), "x.y");
        assert_eq!(file_name_for("com1.txt", "x.y"), "x.y");
        assert_eq!(
            file_name_for(
                "\u{05d0}\u{05e4}\u{05dc}\u{05d9}\u{05e7}\u{05e6}\u{05d9}\u{05d4}.",
                "x.y"
            ),
            "\u{05d0}\u{05e4}\u{05dc}\u{05d9}\u{05e7}\u{05e6}\u{05d9}\u{05d4}"
        );
    }

    #[test]
    fn dates() {
        assert_eq!(yyyymmdd(0), "19700101");
        assert_eq!(yyyymmdd(1_795_000_000_000), "20261118");
        assert_eq!(yyyymmdd(951_782_400_000), "20000229");
    }
}
