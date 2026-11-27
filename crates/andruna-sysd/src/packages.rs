//! The installed-package list.
//!
//! The package database (`system\`, owned by andruna-pkg) does not exist
//! yet. Until it does, a package counts as installed when its install
//! directory `app\<package>\` exists ([file layout](../../docs/spec/02-architecture.md#data-on-disk)),
//! which is where andruna-pkg puts the APK. Nothing installs packages yet,
//! so on a real machine the list is empty, and that is what is reported.

use std::fs;
use std::io;
use std::path::Path;

use andruna_ipc::is_valid_package_name;

/// Names of the installed packages, sorted. A missing `app` directory
/// means no packages. Entries that are not directories or not valid
/// package names are ignored.
pub fn installed(app_dir: &Path) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(app_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str()
            && is_valid_package_name(name)
        {
            out.push(name.to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// Whether `package` (already validated) is installed.
pub fn is_installed(app_dir: &Path, package: &str) -> bool {
    is_valid_package_name(package) && app_dir.join(package).is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_only_valid_package_directories() {
        let dir = std::env::temp_dir().join(format!("andruna-pkg-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(installed(&dir).unwrap(), Vec::<String>::new());
        fs::create_dir_all(dir.join("org.example.b")).unwrap();
        fs::create_dir_all(dir.join("com.example.a")).unwrap();
        fs::create_dir_all(dir.join("not a package")).unwrap();
        fs::write(dir.join("com.example.file"), b"").unwrap();
        assert_eq!(
            installed(&dir).unwrap(),
            vec!["com.example.a".to_string(), "org.example.b".to_string()]
        );
        assert!(is_installed(&dir, "com.example.a"));
        assert!(!is_installed(&dir, "com.example.file"));
        assert!(!is_installed(&dir, "com.example.missing"));
        assert!(!is_installed(&dir, ".."));
        fs::remove_dir_all(&dir).unwrap();
    }
}
