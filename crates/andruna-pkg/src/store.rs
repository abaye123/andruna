//! The per-user package store: install, update, uninstall, inspection.
//!
//! Layout under the data root (`%LOCALAPPDATA%\Andruna`):
//!
//! | Path | Contents |
//! | --- | --- |
//! | `system\packages.json` | The package database ([`crate::db`]) |
//! | `app\<package>\` | `base.apk`, `split_<name>.apk`, `lib\<abi>\*.so`, `oat\<isa>\base.odex`, `icon.ico`, `icon.png` |
//! | `data\<package>\` | The app's private data |
//! | `cache\inspect\` | Icons of inspected (not installed) APKs |
//!
//! An install builds the complete package directory in
//! `app\.staging-<random>\` from copies of the given files (so the files
//! verified are the files installed), then swaps it in with renames and
//! records it; any failure before the record is written rolls back, and an
//! update restores the previous version. Installs and uninstalls are
//! serialized.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::apk::{self, ApkSet, ParsedApk};
use crate::arsc::ResTable;
use crate::compile::{Compiler, Outcome};
use crate::db::{Database, Launcher, PackageRecord};
use crate::desktop::{Desktop, LauncherShortcut};
use crate::error::{Code, Error, Result};
use crate::hooks::{InstallHooks, InstalledPackage};
use crate::sys::{self, MappedFile};
use crate::zip::{self, Archive};
use crate::{icon, manifest};

/// How the store is set up.
pub struct StoreConfig {
    /// The data root (`%LOCALAPPDATA%\Andruna`).
    pub root: PathBuf,
    pub compiler: Compiler,
    /// Start menu and "Installed apps" integration; `None` writes none.
    pub desktop: Option<Desktop>,
    pub hooks: Arc<dyn InstallHooks>,
    /// ABIs this PC runs, most preferred first ([`apk::supported_abis`]).
    pub abis: Vec<String>,
    /// The user's UI locale tag (`he-IL`), for shortcut names.
    pub ui_locale: String,
}

impl StoreConfig {
    /// A configuration for `root` without desktop integration or compiler.
    pub fn new(root: PathBuf) -> Self {
        Self {
            compiler: Compiler {
                app_exe: None,
                art_root: None,
                scratch: root.join("cache").join("dex2oat"),
                timeout: Duration::from_secs(600),
            },
            root,
            desktop: None,
            hooks: Arc::new(crate::hooks::NoHooks),
            abis: apk::supported_abis(),
            ui_locale: "en".into(),
        }
    }
}

/// What [`Store::inspect`] reports.
#[derive(Clone, Debug)]
pub struct Inspection {
    pub set: ApkSet,
    pub label: String,
    /// A 256 px PNG of the icon in `cache\inspect\`.
    pub icon_png: Option<PathBuf>,
    pub installed_version_code: Option<u64>,
}

/// What [`Store::install`] reports.
#[derive(Clone, Debug)]
pub struct InstallReport {
    pub record: PackageRecord,
    pub updated: bool,
    pub compile: Outcome,
    pub shortcuts: usize,
    /// Non-fatal problems (icon, shortcuts, uninstall entry).
    pub warnings: Vec<String>,
}

pub struct Store {
    cfg: StoreConfig,
    db: Mutex<Database>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    if sys::random_bytes(&mut b).is_err() {
        // Not for secrecy here (directory names); the time is unique enough.
        b = now_ms()
            .to_le_bytes()
            .iter()
            .cycle()
            .take(n)
            .copied()
            .collect();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn io_err(what: &Path) -> impl Fn(std::io::Error) -> Error + '_ {
    move |e| Error::new(Code::Io, format!("{}: {e}", what.display()))
}

fn dir_size(p: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => total += dir_size(&e.path()),
                Ok(_) => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                Err(_) => {}
            }
        }
    }
    total
}

/// Removes a directory tree, first renaming it aside so that a partial
/// delete never leaves a half package under its real name.
fn remove_tree(p: &Path) {
    if !p.exists() {
        return;
    }
    let trash = p.with_file_name(format!(".trash-{}", random_hex(8)));
    let target = if std::fs::rename(p, &trash).is_ok() {
        trash
    } else {
        p.to_path_buf()
    };
    let _ = std::fs::remove_dir_all(target);
}

/// Copies the inputs into `dir` as `in-<n>.apk`, extracting APKs from
/// bundle files (`.apks`, `.xapk`, `.apkm`, `.zip`).
fn stage_inputs(paths: &[PathBuf], dir: &Path) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir).map_err(io_err(dir))?;
    let mut out = Vec::new();
    for p in paths {
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(ext.as_str(), "apks" | "xapk" | "apkm" | "zip") {
            let map = MappedFile::open(p).map_err(io_err(p))?;
            let z = Archive::parse(&map).map_err(|e| e.context(p.display()))?;
            for e in z.entries() {
                if e.name.to_ascii_lowercase().ends_with(".apk") && zip::is_safe_name(&e.name) {
                    let dst = dir.join(format!("in-{}.apk", out.len()));
                    std::fs::write(&dst, z.read(e)?).map_err(io_err(&dst))?;
                    out.push(dst);
                }
            }
        } else {
            let dst = dir.join(format!("in-{}.apk", out.len()));
            std::fs::copy(p, &dst).map_err(io_err(p))?;
            out.push(dst);
        }
    }
    if out.is_empty() {
        return Err(Error::new(Code::InvalidSplits, "no APK was given"));
    }
    Ok(out)
}

/// Labels, icon and activity labels from the base APK's resources.
struct Presentation {
    labels: Vec<(String, String)>,
    activity_labels: Vec<Vec<(String, String)>>,
    icon: std::result::Result<icon::Icon, String>,
}

fn presentation(base: &ParsedApk) -> Result<Presentation> {
    let map = MappedFile::open(&base.path).map_err(io_err(&base.path))?;
    let a = Archive::parse(&map)?;
    let table = match a.find("resources.arsc") {
        Some(e) => Some(ResTable::parse(&a.read(e)?)?),
        None => None,
    };
    let m = &base.manifest;
    let labels = apk::labels(table.as_ref(), m.label.as_ref());
    let activity_labels = m
        .launcher_activities()
        .map(|act| apk::labels(table.as_ref(), act.label.as_ref()))
        .collect();
    let icon = match m.icon.or(m.round_icon) {
        Some(id) => icon::render(&a, table.as_ref(), id),
        None => Err("the manifest names no icon".into()),
    };
    Ok(Presentation {
        labels,
        activity_labels,
        icon,
    })
}

/// Extracts `lib/<abi>/*.so` of `apk` into `dir`.
fn extract_libs(apk: &Path, abi: &str, dir: &Path) -> Result<u64> {
    let map = MappedFile::open(apk).map_err(io_err(apk))?;
    let a = Archive::parse(&map)?;
    let prefix = format!("lib/{abi}/");
    let mut n = 0;
    for e in a.entries() {
        let Some(file) = e.name.strip_prefix(&prefix) else {
            continue;
        };
        if file.contains('/') || !file.ends_with(".so") || !zip::is_safe_name(file) {
            continue;
        }
        std::fs::create_dir_all(dir).map_err(io_err(dir))?;
        let dst = dir.join(file);
        std::fs::write(&dst, a.read(e)?).map_err(io_err(&dst))?;
        n += 1;
    }
    Ok(n)
}

impl Store {
    pub fn open(cfg: StoreConfig) -> Result<Self> {
        let db = Database::open(&cfg.root.join("system").join("packages.json"))?;
        let store = Self {
            cfg,
            db: Mutex::new(db),
        };
        store.clean_leftovers();
        Ok(store)
    }

    pub fn config(&self) -> &StoreConfig {
        &self.cfg
    }

    pub fn app_root(&self) -> PathBuf {
        self.cfg.root.join("app")
    }

    pub fn app_dir(&self, package: &str) -> PathBuf {
        self.app_root().join(package)
    }

    pub fn data_dir(&self, package: &str) -> PathBuf {
        self.cfg.root.join("data").join(package)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Database> {
        self.db.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Removes staging, trash and old directories a crash left behind.
    fn clean_leftovers(&self) {
        if let Ok(rd) = std::fs::read_dir(self.app_root()) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with(".staging-")
                    || name.starts_with(".trash-")
                    || name.starts_with(".old-")
                {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        let _ = std::fs::remove_dir_all(self.cfg.root.join("cache").join("inspect"));
    }

    /// The installed packages, sorted by name.
    pub fn list(&self) -> Vec<PackageRecord> {
        self.lock().packages().cloned().collect()
    }

    pub fn get(&self, package: &str) -> Option<PackageRecord> {
        self.lock().get(package).cloned()
    }

    /// Parses and verifies an APK set without installing it.
    pub fn inspect(&self, paths: &[PathBuf], locale: &str) -> Result<Inspection> {
        let dir = self
            .cfg
            .root
            .join("cache")
            .join("inspect")
            .join(random_hex(8));
        let result = (|| {
            let staged = stage_inputs(paths, &dir.join("in"))?;
            let apks = staged
                .iter()
                .map(|p| apk::parse_file(p))
                .collect::<Result<Vec<_>>>()?;
            let set = apk::assemble(apks, &self.cfg.abis)?;
            let pres = presentation(&set.base)?;
            let icon_png = match &pres.icon {
                Ok(i) => {
                    let p = dir.join("icon.png");
                    std::fs::write(&p, &i.png256).map_err(io_err(&p))?;
                    Some(p)
                }
                Err(_) => None,
            };
            let label =
                apk::label_for(&pres.labels, locale).unwrap_or_else(|| set.package().to_string());
            let installed_version_code = self.get(set.package()).map(|r| r.version_code);
            Ok(Inspection {
                set,
                label,
                icon_png,
                installed_version_code,
            })
        })();
        // The staged copies are not needed once parsed; the icon stays
        // until the next start.
        let _ = std::fs::remove_dir_all(dir.join("in"));
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        result
    }

    /// Installs or updates a package from an APK set.
    pub fn install(&self, paths: &[PathBuf]) -> Result<InstallReport> {
        let mut db = self.lock();
        let app_root = self.app_root();
        std::fs::create_dir_all(&app_root).map_err(io_err(&app_root))?;
        let staging = app_root.join(format!(".staging-{}", random_hex(8)));
        let result = self.install_locked(&mut db, paths, &staging);
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        result
    }

    fn install_locked(
        &self,
        db: &mut Database,
        paths: &[PathBuf],
        staging: &Path,
    ) -> Result<InstallReport> {
        let staged = stage_inputs(paths, &staging.join("in"))?;
        let apks = staged
            .iter()
            .map(|p| apk::parse_file(p))
            .collect::<Result<Vec<_>>>()?;
        let set = apk::assemble(apks, &self.cfg.abis)?;
        let package = set.package().to_string();
        let old = db.get(&package).cloned();
        if let Some(old) = &old {
            if !set.base.signing.can_update(&old.signing) {
                return Err(Error::new(
                    Code::SignerMismatch,
                    format!(
                        "the update of {package} is signed by another key than the installed app"
                    ),
                ));
            }
            if set.base.manifest.version_code < old.version_code {
                return Err(Error::new(
                    Code::Downgrade,
                    format!(
                        "version code {} is older than the installed {}",
                        set.base.manifest.version_code, old.version_code
                    ),
                ));
            }
        }
        // Final file names.
        let base_path = staging.join("base.apk");
        std::fs::rename(&set.base.path, &base_path).map_err(io_err(&base_path))?;
        let mut set = set;
        set.base.path = base_path.clone();
        for s in &mut set.splits {
            let name = s.manifest.split.clone().unwrap_or_default();
            let safe: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let dst = staging.join(format!("split_{safe}.apk"));
            std::fs::rename(&s.path, &dst).map_err(io_err(&dst))?;
            s.path = dst;
        }
        let _ = std::fs::remove_dir_all(staging.join("in"));

        let mut warnings = Vec::new();
        // Native libraries.
        if let Some(abi) = &set.abi
            && set.base.manifest.extract_native_libs
        {
            let dir = staging.join("lib").join(abi);
            for a in set.all() {
                extract_libs(&a.path, abi, &dir)?;
            }
        }
        // Icon.
        let pres = presentation(&set.base)?;
        let has_icon = match &pres.icon {
            Ok(i) => {
                std::fs::write(staging.join("icon.ico"), &i.ico).map_err(io_err(staging))?;
                std::fs::write(staging.join("icon.png"), &i.png256).map_err(io_err(staging))?;
                true
            }
            Err(e) => {
                warnings.push(format!("no icon: {e}"));
                false
            }
        };
        // Code.
        let compile = if set.base.manifest.has_code {
            self.cfg.compiler.compile(&base_path)
        } else {
            Outcome::NoCode
        };
        let now = now_ms();
        let m = &set.base.manifest;
        let launchers: Vec<Launcher> = m
            .launcher_activities()
            .zip(pres.activity_labels.iter())
            .map(|(a, l)| Launcher {
                activity: a.name.clone(),
                labels: l.clone(),
                shortcut: None,
            })
            .collect();
        let mut rec = PackageRecord {
            package: package.clone(),
            uid: db.uid_for(&package),
            install_salt: old
                .as_ref()
                .map(|o| o.install_salt.clone())
                .unwrap_or_else(|| random_hex(16)),
            version_code: m.version_code,
            version_name: m.version_name.clone().unwrap_or_default(),
            labels: pres.labels.clone(),
            min_sdk: m.min_sdk,
            target_sdk: m.target_sdk,
            signing: set.base.signing.clone(),
            splits: set
                .splits
                .iter()
                .filter_map(|s| s.manifest.split.clone())
                .collect(),
            abi: set.abi.clone(),
            has_code: m.has_code,
            launchers,
            permissions: m.permissions.iter().map(|p| p.name.clone()).collect(),
            installed_ms: old.as_ref().map(|o| o.installed_ms).unwrap_or(now),
            updated_ms: now,
            size_bytes: dir_size(staging),
            has_icon,
            compile: compile.as_str().into(),
            uninstall_key: None,
        };

        // Swap the directories.
        let app_dir = self.app_dir(&package);
        let old_dir = self.app_root().join(format!(".old-{}", random_hex(8)));
        let had_dir = app_dir.exists();
        if had_dir {
            std::fs::rename(&app_dir, &old_dir).map_err(|e| {
                Error::new(
                    Code::Io,
                    format!(
                        "cannot replace {} (is the app running?): {e}",
                        app_dir.display()
                    ),
                )
            })?;
        }
        let restore = |new_in_place: bool| {
            if new_in_place {
                let _ = std::fs::rename(&app_dir, staging);
            }
            if had_dir {
                let _ = std::fs::rename(&old_dir, &app_dir);
            }
        };
        if let Err(e) = std::fs::rename(staging, &app_dir) {
            restore(false);
            return Err(io_err(&app_dir)(e));
        }
        let data_dir = self.data_dir(&package);
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            restore(true);
            return Err(io_err(&data_dir)(e));
        }
        let hook = self.cfg.hooks.installed(&InstalledPackage {
            record: &rec,
            app_dir: &app_dir,
            data_dir: &data_dir,
            update: old.is_some(),
        });
        if let Err(e) = hook {
            restore(true);
            return Err(Error::new(Code::Io, format!("install hook failed: {e}")));
        }

        // Desktop integration (non-fatal).
        let mut shortcuts = 0;
        if let Some(d) = &self.cfg.desktop {
            let label =
                apk::label_for(&rec.labels, &self.cfg.ui_locale).unwrap_or_else(|| package.clone());
            let icon = has_icon.then(|| app_dir.join("icon.ico"));
            let old_links: Vec<PathBuf> = old
                .iter()
                .flat_map(|o| o.launchers.iter().filter_map(|l| l.shortcut.clone()))
                .collect();
            d.remove_shortcuts(&old_links);
            let multi = rec.launchers.len() > 1;
            let wanted: Vec<LauncherShortcut<'_>> = rec
                .launchers
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let own = apk::label_for(&l.labels, &self.cfg.ui_locale);
                    let name = match own {
                        Some(n) if n != label => n,
                        _ if multi && i > 0 => {
                            format!(
                                "{label} ({})",
                                l.activity.rsplit('.').next().unwrap_or(&l.activity)
                            )
                        }
                        _ => label.clone(),
                    };
                    LauncherShortcut {
                        activity: &l.activity,
                        name,
                        primary: i == 0,
                    }
                })
                .collect();
            match d.create_shortcuts(&package, &wanted, icon.as_deref(), &label, &old_links) {
                Ok(paths) => {
                    shortcuts = paths.len();
                    for (l, p) in rec.launchers.iter_mut().zip(paths) {
                        l.shortcut = Some(p);
                    }
                }
                Err(e) => warnings.push(format!("shortcuts: {e}")),
            }
            match d.write_uninstall_entry(&rec, &label, icon.as_deref(), &app_dir) {
                Ok(k) => rec.uninstall_key = k,
                Err(e) => warnings.push(format!("uninstall entry: {e}")),
            }
        }

        if let Err(e) = db.put(rec.clone()) {
            if let Some(d) = &self.cfg.desktop {
                d.remove_shortcuts(
                    &rec.launchers
                        .iter()
                        .filter_map(|l| l.shortcut.clone())
                        .collect::<Vec<_>>(),
                );
                if old.is_none()
                    && let Some(k) = &rec.uninstall_key
                {
                    d.remove_uninstall_entry(k);
                }
            }
            restore(true);
            return Err(e);
        }
        if had_dir {
            remove_tree(&old_dir);
        }
        Ok(InstallReport {
            record: rec,
            updated: old.is_some(),
            compile,
            shortcuts,
            warnings,
        })
    }

    /// Removes a package; its data too unless `keep_data`.
    pub fn uninstall(&self, package: &str, keep_data: bool) -> Result<PackageRecord> {
        if !manifest::is_valid_package_name(package) {
            return Err(Error::new(
                Code::NotInstalled,
                format!("`{package}` is not a package name"),
            ));
        }
        let mut db = self.lock();
        let rec = db
            .get(package)
            .cloned()
            .ok_or_else(|| Error::new(Code::NotInstalled, format!("{package} is not installed")))?;
        // Move the files aside first: this fails while the app runs, and
        // then nothing has changed.
        let app_dir = self.app_dir(package);
        let trash = self.app_root().join(format!(".trash-{}", random_hex(8)));
        if app_dir.exists() {
            std::fs::rename(&app_dir, &trash).map_err(|e| {
                Error::new(
                    Code::Io,
                    format!(
                        "cannot remove {} (is the app running?): {e}",
                        app_dir.display()
                    ),
                )
            })?;
        }
        if let Err(e) = db.remove(package) {
            let _ = std::fs::rename(&trash, &app_dir);
            return Err(e);
        }
        if let Some(d) = &self.cfg.desktop {
            d.remove_shortcuts(
                &rec.launchers
                    .iter()
                    .filter_map(|l| l.shortcut.clone())
                    .collect::<Vec<_>>(),
            );
            if let Some(k) = &rec.uninstall_key {
                d.remove_uninstall_entry(k);
            }
        }
        self.cfg.hooks.removed(&rec, keep_data);
        let _ = std::fs::remove_dir_all(&trash);
        if !keep_data {
            remove_tree(&self.data_dir(package));
        }
        Ok(rec)
    }
}
