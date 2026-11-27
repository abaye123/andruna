//! The guest file tree of an app process and the ART module on disk.
//!
//! | Guest path | Windows directory |
//! | --- | --- |
//! | `/` | the data directory (`--data`) |
//! | `/apex/com.android.art` | the ART root (`lib64`, `bin`, `javalib`) |
//! | `/apex/com.android.i18n/etc/icu` | `<ART root>/icu` (ICU's data file) |
//! | `/data/app/cp<N>` | the directory of class path entry N |
//!
//! The ART root is what `python aosp/tools/build_guest.py art` stages in
//! `out/guest/<arch>/art-root`: bionic, libc++ and ART's libraries in
//! `lib64`, `dalvikvm` and `dex2oat` in `bin`, the boot class path jars
//! (and, after `--compile-boot`, the boot image in `javalib/<isa>`) in
//! `javalib`, ICU's data in `icu`.

use std::path::{Path, PathBuf};

use crate::{BOOT_JARS, ICU_DATA, ISA};

/// The ART module on disk.
#[derive(Clone, Debug)]
pub struct ArtRoot {
    pub dir: PathBuf,
}

impl ArtRoot {
    /// `--art-root`, else `ANDRUNA_ART_ROOT`, else `art` next to the
    /// executable.
    pub fn locate(given: Option<&Path>, exe_dir: Option<&Path>) -> Result<Self, String> {
        let dir = match given {
            Some(d) => d.to_path_buf(),
            None => match std::env::var_os("ANDRUNA_ART_ROOT") {
                Some(d) => PathBuf::from(d),
                None => exe_dir
                    .map(|d| d.join("art"))
                    .ok_or("cannot find the ART root; give --art-root")?,
            },
        };
        let r = ArtRoot { dir };
        r.validate()?;
        Ok(r)
    }

    /// Checks that the pieces every run needs are present.
    pub fn validate(&self) -> Result<(), String> {
        let mut missing = vec![];
        for p in [
            self.lib().join("libart.so"),
            self.lib().join("libc.so"),
            self.bin().join("dalvikvm"),
        ]
        .into_iter()
        .chain(BOOT_JARS.iter().map(|j| self.javalib().join(j)))
        {
            if !p.is_file() {
                missing.push(p.display().to_string());
            }
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "the ART root {} is incomplete; missing: {}",
                self.dir.display(),
                missing.join(", ")
            ))
        }
    }

    pub fn lib(&self) -> PathBuf {
        self.dir.join("lib64")
    }

    pub fn bin(&self) -> PathBuf {
        self.dir.join("bin")
    }

    pub fn javalib(&self) -> PathBuf {
        self.dir.join("javalib")
    }

    pub fn icu(&self) -> PathBuf {
        self.dir.join("icu")
    }

    /// The primary boot image file for this architecture.
    pub fn boot_image(&self) -> PathBuf {
        self.javalib().join(ISA).join("boot.art")
    }
}

/// An app process's guest file tree.
#[derive(Clone, Debug)]
pub struct Layout {
    pub art: ArtRoot,
    /// The Windows directory mounted as `/`.
    pub data: PathBuf,
}

/// Directories every guest tree has (relative to `/`).
const DIRS: &[&str] = &[
    "dev",
    "tmp",
    "proc",
    "system/bin",
    "system/framework",
    "data/local/tmp",
    "data/app",
    "apex/com.android.art",
    "apex/com.android.i18n/etc/icu",
    "apex/com.android.tzdata",
];

impl Layout {
    pub fn new(art: ArtRoot, data: Option<&Path>) -> Self {
        let data = data
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::env::temp_dir().join("andruna-app"));
        Layout { art, data }
    }

    /// The guest path of each class path entry: entry N is
    /// `/data/app/cp<N>/<file name>`.
    pub fn guest_classpath(&self, classpath: &[PathBuf]) -> Result<Vec<String>, String> {
        classpath
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| format!("bad class path entry {}", p.display()))?;
                if name.contains(':') {
                    return Err(format!("class path entry names cannot contain ':': {name}"));
                }
                Ok(format!("/data/app/cp{i}/{name}"))
            })
            .collect()
    }

    /// Guest directory -> Windows directory, for `classpath`.
    pub fn mounts(&self, classpath: &[PathBuf]) -> Result<Vec<(String, PathBuf)>, String> {
        let mut m = vec![
            ("/".to_string(), self.data.clone()),
            (crate::ART_APEX.to_string(), self.art.dir.clone()),
            (ICU_DATA.to_string(), self.art.icu()),
        ];
        for (i, p) in classpath.iter().enumerate() {
            if !p.is_file() {
                return Err(format!("class path entry {} not found", p.display()));
            }
            let dir = absolute(p)?
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| format!("bad class path entry {}", p.display()))?;
            m.push((format!("/data/app/cp{i}"), dir));
        }
        Ok(m)
    }

    /// Creates the directories and mounts the tree with andruna-lx, and
    /// gives the guest the standard streams.
    pub fn install(&self, classpath: &[PathBuf]) -> Result<(), String> {
        for d in DIRS {
            std::fs::create_dir_all(self.data.join(d))
                .map_err(|e| format!("cannot create {}: {e}", self.data.join(d).display()))?;
        }
        std::fs::create_dir_all(self.art.javalib().join(ISA)).map_err(|e| e.to_string())?;
        for (guest, host) in self.mounts(classpath)? {
            let host = absolute(&host)?;
            andruna_lx::fs::mount::mount(&guest, &host)
                .map_err(|e| format!("mount {guest}: errno {}", e.0))?;
        }
        andruna_lx::fs::stream::install_std_streams()
            .map_err(|e| format!("standard streams: errno {}", e.0))
    }

    /// Creates `<dir>/oat/<isa>` next to a class path entry (dex2oat's
    /// output directory for it).
    pub fn ensure_oat_dir(&self, entry: &Path) -> Result<(), String> {
        let dir = absolute(entry)?
            .parent()
            .map(|d| d.join("oat").join(ISA))
            .ok_or("bad class path entry")?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))
    }
}

/// An absolute Windows path without the `\\?\` prefix.
fn absolute(p: &Path) -> Result<PathBuf, String> {
    let a = std::path::absolute(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let s = a.to_string_lossy();
    Ok(PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("andruna-app-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn incomplete_art_root_is_reported() {
        let d = tmp("incomplete");
        let e = ArtRoot::locate(Some(&d), None).unwrap_err();
        assert!(e.contains("libart.so") && e.contains("core-oj.jar"), "{e}");
    }

    #[test]
    fn classpath_entries_map_to_their_directories() {
        let d = tmp("cp");
        std::fs::write(d.join("hello.dex"), b"dex").unwrap();
        let l = Layout::new(ArtRoot { dir: d.join("art") }, Some(&d.join("data")));
        let cp = vec![d.join("hello.dex")];
        assert_eq!(
            l.guest_classpath(&cp).unwrap(),
            vec!["/data/app/cp0/hello.dex".to_string()]
        );
        let m = l.mounts(&cp).unwrap();
        assert_eq!(m[0].0, "/");
        assert_eq!(m[1].0, "/apex/com.android.art");
        assert_eq!(m[2].0, ICU_DATA);
        assert_eq!(m[3], ("/data/app/cp0".to_string(), absolute(&d).unwrap()));
        assert!(l.mounts(&[d.join("missing.dex")]).is_err());
    }
}
