//! The package database: `system\packages.json`.
//!
//! One JSON document with a format version. Writes are atomic: the new
//! document goes to `packages.json.tmp`, is flushed to disk, then replaces
//! the old file in one rename. A reader sees the old or the new database,
//! never a mix. A database of a newer format version is not touched (the
//! error says so), so a downgrade of Andruna cannot damage it.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Code, Error, Result};
use crate::sig::SigningDetails;

/// The format version this crate reads and writes.
pub const DB_VERSION: u32 = 1;

/// The first Android uid given to an app (`Process.FIRST_APPLICATION_UID`).
pub const FIRST_APP_UID: u32 = 10_000;

/// A launcher activity and its shortcut.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launcher {
    /// Fully qualified activity (or alias) name.
    pub activity: String,
    /// Labels by locale tag, if the activity has its own.
    #[serde(default)]
    pub labels: Vec<(String, String)>,
    /// The Start menu shortcut created for it, if any.
    #[serde(default)]
    pub shortcut: Option<PathBuf>,
}

/// One installed package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRecord {
    pub package: String,
    /// The package's Android uid, stable across updates.
    pub uid: u32,
    /// Random per-install salt (hex), for identities derived from the
    /// package (the AppContainer profile, security spec).
    pub install_salt: String,
    pub version_code: u64,
    pub version_name: String,
    /// Application labels by locale tag (`""` is the default).
    pub labels: Vec<(String, String)>,
    pub min_sdk: u32,
    pub target_sdk: u32,
    pub signing: SigningDetails,
    /// Installed split names, sorted.
    pub splits: Vec<String>,
    /// The ABI of the installed native libraries.
    pub abi: Option<String>,
    pub has_code: bool,
    pub launchers: Vec<Launcher>,
    pub permissions: Vec<String>,
    pub installed_ms: u64,
    pub updated_ms: u64,
    /// Bytes of APKs, libraries and compiled code.
    pub size_bytes: u64,
    /// Whether `icon.ico` and `icon.png` exist in the package directory.
    pub has_icon: bool,
    /// How the compile step ended (`compiled`, `no_code`, `unavailable`,
    /// `failed`).
    pub compile: String,
    /// The registry key of the uninstall entry, if one was written.
    #[serde(default)]
    pub uninstall_key: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Document {
    version: u32,
    next_uid: u32,
    packages: BTreeMap<String, PackageRecord>,
}

/// The database in memory, tied to its file.
#[derive(Debug)]
pub struct Database {
    path: PathBuf,
    doc: Document,
}

impl Database {
    /// Opens the database; a missing file is an empty database.
    pub fn open(path: &Path) -> Result<Self> {
        let doc = match fs::read(path) {
            Ok(bytes) => {
                #[derive(Deserialize)]
                struct Probe {
                    version: u32,
                }
                let probe: Probe = serde_json::from_slice(&bytes)
                    .map_err(|e| Error::new(Code::Database, format!("{}: {e}", path.display())))?;
                if probe.version != DB_VERSION {
                    return Err(Error::new(
                        Code::Database,
                        format!(
                            "{} has format version {}; this Andruna reads version {DB_VERSION}",
                            path.display(),
                            probe.version
                        ),
                    ));
                }
                serde_json::from_slice(&bytes)
                    .map_err(|e| Error::new(Code::Database, format!("{}: {e}", path.display())))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Document {
                version: DB_VERSION,
                next_uid: FIRST_APP_UID,
                packages: BTreeMap::new(),
            },
            Err(e) => return Err(Error::new(Code::Io, format!("{}: {e}", path.display()))),
        };
        Ok(Self {
            path: path.to_path_buf(),
            doc,
        })
    }

    pub fn get(&self, package: &str) -> Option<&PackageRecord> {
        self.doc.packages.get(package)
    }

    /// All packages, sorted by name.
    pub fn packages(&self) -> impl Iterator<Item = &PackageRecord> {
        self.doc.packages.values()
    }

    /// A new uid, or the existing one of an installed package.
    pub fn uid_for(&self, package: &str) -> u32 {
        self.get(package)
            .map(|r| r.uid)
            .unwrap_or(self.doc.next_uid)
    }

    /// Inserts or replaces a record and writes the database.
    pub fn put(&mut self, rec: PackageRecord) -> Result<()> {
        let mut doc = self.doc.clone();
        if rec.uid >= doc.next_uid {
            doc.next_uid = rec.uid + 1;
        }
        doc.packages.insert(rec.package.clone(), rec);
        self.write(&doc)?;
        self.doc = doc;
        Ok(())
    }

    /// Removes a record and writes the database.
    pub fn remove(&mut self, package: &str) -> Result<Option<PackageRecord>> {
        let mut doc = self.doc.clone();
        let old = doc.packages.remove(package);
        if old.is_some() {
            self.write(&doc)?;
            self.doc = doc;
        }
        Ok(old)
    }

    fn write(&self, doc: &Document) -> Result<()> {
        let io =
            |e: std::io::Error| Error::new(Code::Database, format!("{}: {e}", self.path.display()));
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(io)?;
        }
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let bytes = serde_json::to_vec_pretty(doc)
            .map_err(|e| Error::new(Code::Database, e.to_string()))?;
        {
            let mut f = fs::File::create(&tmp).map_err(io)?;
            f.write_all(&bytes).map_err(io)?;
            f.sync_all().map_err(io)?;
        }
        fs::rename(&tmp, &self.path).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sig::Scheme;

    fn rec(p: &str, uid: u32) -> PackageRecord {
        PackageRecord {
            package: p.into(),
            uid,
            install_salt: "00".into(),
            version_code: 1,
            version_name: "1".into(),
            labels: vec![("".into(), "L".into()), ("he".into(), "\u{05d0}".into())],
            min_sdk: 21,
            target_sdk: 34,
            signing: SigningDetails {
                scheme: Scheme::V2,
                signers: vec!["ab".into()],
                certificates: vec![],
                lineage: vec![],
            },
            splits: vec![],
            abi: None,
            has_code: true,
            launchers: vec![],
            permissions: vec![],
            installed_ms: 1,
            updated_ms: 1,
            size_bytes: 1,
            has_icon: false,
            compile: "compiled".into(),
            uninstall_key: None,
        }
    }

    #[test]
    fn round_trips_and_refuses_newer_versions() {
        let dir = std::env::temp_dir().join(format!("andruna-pkg-db-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("system").join("packages.json");
        let mut db = Database::open(&path).unwrap();
        assert_eq!(db.uid_for("a.b"), FIRST_APP_UID);
        db.put(rec("a.b", db.uid_for("a.b"))).unwrap();
        let uid = db.uid_for("c.d");
        assert_eq!(uid, FIRST_APP_UID + 1);
        db.put(rec("c.d", uid)).unwrap();
        let again = Database::open(&path).unwrap();
        assert_eq!(again.get("a.b"), db.get("a.b"));
        assert_eq!(again.packages().count(), 2);
        assert_eq!(again.uid_for("a.b"), FIRST_APP_UID);
        let mut again = again;
        assert!(again.remove("a.b").unwrap().is_some());
        // uids are not reused.
        assert_eq!(again.uid_for("e.f"), FIRST_APP_UID + 2);
        assert!(!dir.join("system").join("packages.json.tmp").exists());

        fs::write(&path, br#"{"version": 2, "next_uid": 1, "packages": {}}"#).unwrap();
        assert_eq!(Database::open(&path).unwrap_err().code, Code::Database);
        fs::write(&path, b"{ not json").unwrap();
        assert_eq!(Database::open(&path).unwrap_err().code, Code::Database);
        fs::remove_dir_all(&dir).unwrap();
    }
}
