//! What the supervisor needs to know about an installed package.
//!
//! The supervisor reads packages through [`PackageStore`]. sysd uses
//! [`DatabaseStore`]: the package manager's database (andruna-pkg, the
//! packages installed from APKs, which start through the framework entry),
//! and for packages not in it the hand-installed main-class programs of
//! [`RecordStore`] (development and tests: a class's `main` runs as
//! `dalvikvm` would run it).
//!
//! [`RecordStore`] reads one record per package,
//! `system\packages\<package>.txt`, UTF-8 `key=value` lines:
//!
//! ```text
//! salt=<32 lowercase hex digits>     the AppContainer salt (required)
//! code=<file>[;<file>...]            dex or jar files in app\<package>\
//! main=<class>                       the class whose main runs
//! args=<arg>[;<arg>...]              optional arguments of main
//! mode=int|jit|aot                   optional, default jit
//! permissions=<name>[,<name>...]     optional granted permissions
//! ```
//!
//! A package without `main` is started through the framework entry, which
//! the host refuses until the framework is part of the runtime.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use andruna_ipc::is_valid_package_name;

use crate::Layout;
use crate::appcontainer::Salt;

/// What runs when the package starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launchable {
    /// A class's `main`; `code` are absolute paths.
    Main {
        code: Vec<PathBuf>,
        class: String,
        args: Vec<String>,
    },
    /// The framework's activity start.
    Framework,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledPackage {
    pub name: String,
    pub salt: Salt,
    pub launch: Launchable,
    pub mode: String,
    pub permissions: Vec<String>,
}

/// The installed packages, as the supervisor sees them.
pub trait PackageStore: Send + Sync {
    /// The package, `Ok(None)` when it is not installed; an error when its
    /// record cannot be read.
    fn get(&self, package: &str) -> io::Result<Option<InstalledPackage>>;
}

/// The minimal record reader (see the module documentation).
#[derive(Clone, Debug)]
pub struct RecordStore {
    layout: Layout,
}

impl RecordStore {
    pub fn new(layout: Layout) -> Self {
        Self { layout }
    }

    pub fn record_path(layout: &Layout, package: &str) -> PathBuf {
        layout
            .root
            .join("system")
            .join("packages")
            .join(format!("{package}.txt"))
    }

    /// Writes a record (for tools and tests that install by hand).
    pub fn write(layout: &Layout, p: &InstalledPackage) -> io::Result<()> {
        let mut s = format!("salt={}\n", p.salt.as_str());
        if let Launchable::Main { code, class, args } = &p.launch {
            let app = layout.app_dir().join(&p.name);
            let rel: Vec<String> = code
                .iter()
                .map(|c| {
                    c.strip_prefix(&app)
                        .unwrap_or(c)
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            s.push_str(&format!("code={}\nmain={class}\n", rel.join(";")));
            if !args.is_empty() {
                s.push_str(&format!("args={}\n", args.join(";")));
            }
        }
        s.push_str(&format!("mode={}\n", p.mode));
        if !p.permissions.is_empty() {
            s.push_str(&format!("permissions={}\n", p.permissions.join(",")));
        }
        let path = Self::record_path(layout, &p.name);
        if let Some(d) = path.parent() {
            fs::create_dir_all(d)?;
        }
        fs::write(path, s)
    }
}

fn invalid(path: &Path, m: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: {m}", path.display()),
    )
}

/// Parses a record of `package` whose code lives in `app_dir`.
pub fn parse_record(
    package: &str,
    app_dir: &Path,
    path: &Path,
    text: &str,
) -> io::Result<InstalledPackage> {
    let (mut salt, mut code, mut main, mut args, mut mode, mut perms) =
        (None, None, None, Vec::new(), "jit".to_string(), Vec::new());
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(invalid(path, format!("not key=value: {line}")));
        };
        match k.trim() {
            "salt" => salt = Some(Salt::parse(v.trim()).map_err(|e| invalid(path, e))?),
            "code" => code = Some(v.trim().to_string()),
            "main" => main = Some(v.trim().to_string()),
            "args" => args = v.split(';').map(str::to_string).collect(),
            "mode" => mode = v.trim().to_string(),
            "permissions" => {
                perms = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            _ => {}
        }
    }
    let salt = salt.ok_or_else(|| invalid(path, "no salt"))?;
    if !matches!(mode.as_str(), "int" | "jit" | "aot") {
        return Err(invalid(path, format!("unknown mode {mode}")));
    }
    let launch = match main {
        None => Launchable::Framework,
        Some(class) => {
            let code = code.ok_or_else(|| invalid(path, "main without code"))?;
            let mut files = Vec::new();
            for c in code.split(';').filter(|c| !c.is_empty()) {
                let p = Path::new(c);
                if p.is_absolute()
                    || p.components()
                        .any(|x| !matches!(x, std::path::Component::Normal(_)))
                {
                    return Err(invalid(path, format!("code outside the package: {c}")));
                }
                files.push(app_dir.join(p));
            }
            if files.is_empty() {
                return Err(invalid(path, "empty code"));
            }
            Launchable::Main {
                code: files,
                class,
                args,
            }
        }
    };
    Ok(InstalledPackage {
        name: package.to_string(),
        salt,
        launch,
        mode,
        permissions: perms,
    })
}

impl PackageStore for RecordStore {
    fn get(&self, package: &str) -> io::Result<Option<InstalledPackage>> {
        if !is_valid_package_name(package)
            || !crate::packages::is_installed(&self.layout.app_dir(), package)
        {
            return Ok(None);
        }
        let path = Self::record_path(&self.layout, package);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{package} has no package record ({})", path.display()),
                ));
            }
            Err(e) => return Err(e),
        };
        parse_record(package, &self.layout.app_dir().join(package), &path, &text).map(Some)
    }
}

/// The package database first, then hand-installed records.
pub struct DatabaseStore {
    db: std::sync::Arc<andruna_pkg::store::Store>,
    records: RecordStore,
}

impl DatabaseStore {
    pub fn new(db: std::sync::Arc<andruna_pkg::store::Store>, records: RecordStore) -> Self {
        Self { db, records }
    }
}

/// What the supervisor needs from a database record. An APK's activities
/// start through the framework entry.
pub fn from_database(r: &andruna_pkg::db::PackageRecord) -> io::Result<InstalledPackage> {
    let salt = Salt::parse(&r.install_salt)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", r.package)))?;
    Ok(InstalledPackage {
        name: r.package.clone(),
        salt,
        launch: Launchable::Framework,
        mode: "jit".into(),
        permissions: r.permissions.clone(),
    })
}

impl PackageStore for DatabaseStore {
    fn get(&self, package: &str) -> io::Result<Option<InstalledPackage>> {
        match self.db.get(package) {
            Some(r) => from_database(&r).map(Some),
            None => self.records.get(package),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records() {
        let app = Path::new(r"C:\a\app\com.example.x");
        let p = Path::new("r.txt");
        let r = parse_record(
            "com.example.x",
            app,
            p,
            "salt=00112233445566778899aabbccddeeff\ncode=hello.dex;lib/b.jar\nmain=Hello\nargs=a;b\nmode=int\npermissions=android.permission.INTERNET\n",
        )
        .unwrap();
        assert_eq!(
            r.launch,
            Launchable::Main {
                code: vec![app.join("hello.dex"), app.join(r"lib\b.jar")],
                class: "Hello".into(),
                args: vec!["a".into(), "b".into()],
            }
        );
        assert_eq!(r.mode, "int");
        assert_eq!(r.permissions, vec!["android.permission.INTERNET"]);
        let f = parse_record(
            "com.example.x",
            app,
            p,
            "salt=00112233445566778899aabbccddeeff\n",
        )
        .unwrap();
        assert_eq!(f.launch, Launchable::Framework);
        assert_eq!(f.mode, "jit");
        for bad in [
            "code=a.dex\nmain=A\n",
            "salt=xyz\n",
            "salt=00112233445566778899aabbccddeeff\nmain=A\n",
            "salt=00112233445566778899aabbccddeeff\ncode=..\\x.dex\nmain=A\n",
            "salt=00112233445566778899aabbccddeeff\ncode=C:\\x.dex\nmain=A\n",
            "salt=00112233445566778899aabbccddeeff\nmode=fast\n",
            "garbage\n",
        ] {
            assert!(parse_record("com.example.x", app, p, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn write_then_read() {
        let root = std::env::temp_dir().join(format!("andruna-sysd-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let layout = Layout::new(root.clone());
        let store = RecordStore::new(layout.clone());
        assert_eq!(store.get("com.example.y").unwrap(), None);
        fs::create_dir_all(layout.app_dir().join("com.example.y")).unwrap();
        assert!(
            store.get("com.example.y").is_err(),
            "installed without a record"
        );
        let p = InstalledPackage {
            name: "com.example.y".into(),
            salt: Salt::parse("00112233445566778899aabbccddeeff").unwrap(),
            launch: Launchable::Main {
                code: vec![layout.app_dir().join("com.example.y").join("a.dex")],
                class: "A".into(),
                args: vec![],
            },
            mode: "jit".into(),
            permissions: vec![],
        };
        RecordStore::write(&layout, &p).unwrap();
        assert_eq!(store.get("com.example.y").unwrap(), Some(p));
        let _ = fs::remove_dir_all(&root);
    }
}
