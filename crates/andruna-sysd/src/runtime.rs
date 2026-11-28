//! The runtime files in the install directory: `andruna-app.exe` and the
//! ART root `art\` next to sysd (installer/README.md).
//!
//! Two things are prepared once per install and checked again at every sysd
//! start ([ADR 0036](../../../docs/decisions/0036-appcontainer-file-access-and-sysd-channel.md),
//! [ADR 0043](../../../docs/decisions/0043-runtime-in-the-installer-and-boot-image-at-install.md)):
//!
//! - the inheritable read-and-execute ACE for the `andrunaRuntime`
//!   capability on the install directory, so that app containers can load
//!   the runtime;
//! - the boot image (`art\javalib\<isa>\boot.art` and its companions),
//!   written by `andruna-app --compile-boot` on the user's PC. The installer
//!   runs `andruna-sysd --prepare-runtime`, which does both and waits; a sysd
//!   that starts and finds no boot image (a failed or interrupted install
//!   step) builds it in the background.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{Layout, appcontainer, info, warn};

/// Upper bound for writing the boot image.
pub const BOOT_IMAGE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// `CREATE_NO_WINDOW`: andruna-app is a console program; sysd has no
/// console, and the user must not see one appear.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The runtime as installed next to sysd.
#[derive(Clone, Debug)]
pub struct Runtime {
    /// The install directory (where sysd, andruna-app and `art\` are).
    pub install_dir: PathBuf,
    pub app_exe: PathBuf,
    pub art_root: PathBuf,
}

impl Runtime {
    /// The runtime in `dir`, when `andruna-app.exe` and `art\` are there.
    pub fn in_dir(dir: &Path) -> Option<Self> {
        let r = Self {
            install_dir: dir.to_path_buf(),
            app_exe: dir.join("andruna-app.exe"),
            art_root: dir.join("art"),
        };
        (r.app_exe.is_file() && r.art_root.join("lib64").join("libart.so").is_file()).then_some(r)
    }

    /// The runtime next to this executable.
    pub fn beside_exe() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        Self::in_dir(exe.parent()?)
    }

    /// The boot image directory, `art\javalib\<isa>`.
    pub fn boot_image_dir(&self) -> PathBuf {
        self.art_root
            .join("javalib")
            .join(andruna_pkg::compile::isa())
    }

    /// The primary boot image file.
    pub fn boot_image(&self) -> PathBuf {
        self.boot_image_dir().join("boot.art")
    }

    pub fn has_boot_image(&self) -> bool {
        self.boot_image().is_file()
    }

    /// Lets every Andruna container read and execute the install directory
    /// (idempotent).
    pub fn grant(&self) -> Result<(), String> {
        appcontainer::grant_runtime(&self.install_dir).map_err(|e| e.to_string())
    }

    /// Writes the boot image with `andruna-app --compile-boot`, using
    /// `scratch` as the guest root. A failed run leaves no partial image.
    pub fn compile_boot_image(&self, scratch: &Path) -> Result<Duration, String> {
        std::fs::create_dir_all(scratch)
            .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
        let err_log = scratch.join("compile-boot.err.log");
        let out = std::fs::File::create(scratch.join("compile-boot.log"))
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        let err = std::fs::File::create(&err_log)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        let start = Instant::now();
        let mut child = {
            use std::os::windows::process::CommandExt as _;
            Command::new(&self.app_exe)
                .arg("--art-root")
                .arg(&self.art_root)
                .arg("--data")
                .arg(scratch)
                .arg("--compile-boot")
                .stdin(Stdio::null())
                .stdout(out)
                .stderr(err)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map_err(|e| format!("cannot start {}: {e}", self.app_exe.display()))?
        };
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if start.elapsed() > BOOT_IMAGE_TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    self.remove_partial_image();
                    return Err(format!(
                        "dex2oat did not finish in {} s",
                        BOOT_IMAGE_TIMEOUT.as_secs()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => return Err(e.to_string()),
            }
        };
        let took = start.elapsed();
        if status.success() && self.has_boot_image() {
            return Ok(took);
        }
        self.remove_partial_image();
        let tail = std::fs::read_to_string(&err_log).unwrap_or_default();
        let tail: Vec<&str> = tail.lines().rev().take(5).collect();
        Err(format!(
            "andruna-app --compile-boot exited with {status}: {}",
            tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
        ))
    }

    fn remove_partial_image(&self) {
        let _ = std::fs::remove_dir_all(self.boot_image_dir());
    }
}

/// Scratch guest root for the boot image compilation.
fn scratch(layout: &Layout) -> PathBuf {
    layout.root.join("tmp").join("boot-image")
}

/// Grants the runtime capability and writes the boot image when it is
/// missing; returns an error text when either fails. Used by
/// `--prepare-runtime` (the installer) and, in a background thread, at sysd
/// start.
pub fn prepare(rt: &Runtime, layout: &Layout) -> Result<(), String> {
    let t = Instant::now();
    rt.grant()
        .map_err(|e| format!("granting {}: {e}", appcontainer::RUNTIME_CAPABILITY))?;
    info!(
        "runtime_granted",
        dir = rt.install_dir.display(),
        ms = t.elapsed().as_millis()
    );
    if rt.has_boot_image() {
        return Ok(());
    }
    info!("boot_image_started", art_root = rt.art_root.display());
    let dir = scratch(layout);
    let r = rt.compile_boot_image(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    match r {
        Ok(took) => {
            info!("boot_image_written", ms = took.as_millis());
            Ok(())
        }
        Err(e) => {
            warn!("boot_image_failed", error = e);
            Err(e)
        }
    }
}

/// At sysd start: the same as [`prepare`], in the background, when the
/// runtime is installed next to sysd. Installs report the compile step as
/// unavailable until the boot image exists.
pub fn prepare_in_background(layout: &Layout) {
    let Some(rt) = Runtime::beside_exe() else {
        return;
    };
    if rt.has_boot_image() {
        // Granting again keeps the capability on files a repair or an
        // update added; it is quick.
        if let Err(e) = rt.grant() {
            warn!("runtime_grant_failed", error = e);
        }
        return;
    }
    let layout = layout.clone();
    let _ = std::thread::Builder::new()
        .name("prepare-runtime".into())
        .spawn(move || {
            let _ = prepare(&rt, &layout);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_needs_app_and_art_root() {
        let d = std::env::temp_dir().join(format!("andruna-sysd-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("art").join("lib64")).unwrap();
        assert!(Runtime::in_dir(&d).is_none());
        std::fs::write(d.join("andruna-app.exe"), b"").unwrap();
        assert!(Runtime::in_dir(&d).is_none());
        std::fs::write(d.join("art").join("lib64").join("libart.so"), b"").unwrap();
        let rt = Runtime::in_dir(&d).unwrap();
        assert!(!rt.has_boot_image());
        assert!(
            rt.boot_image().ends_with(
                Path::new("javalib")
                    .join(andruna_pkg::compile::isa())
                    .join("boot.art")
            )
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn failed_compile_leaves_no_image() {
        let d = std::env::temp_dir().join(format!("andruna-sysd-rt-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("art").join("lib64")).unwrap();
        std::fs::write(d.join("art").join("lib64").join("libart.so"), b"").unwrap();
        // A program that exits with an error and writes nothing.
        let cmd =
            std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
        std::fs::copy(cmd, d.join("andruna-app.exe")).unwrap();
        let rt = Runtime::in_dir(&d).unwrap();
        std::fs::create_dir_all(rt.boot_image_dir()).unwrap();
        std::fs::write(rt.boot_image_dir().join("boot.oat"), b"partial").unwrap();
        let e = rt.compile_boot_image(&d.join("scratch")).unwrap_err();
        assert!(e.contains("--compile-boot"), "{e}");
        assert!(!rt.boot_image_dir().exists());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
