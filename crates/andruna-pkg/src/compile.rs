//! Ahead-of-time compilation at install: `andruna-app --compile`.
//!
//! andruna-app runs ART's dex2oat on andruna-linker. Given
//! `<dir>\base.apk`, it writes `<dir>\oat\<isa>\base.odex` (and `.vdex`),
//! where ART looks for the compiled code of an APK outside the dalvik
//! cache. The step needs andruna-app and an ART root with a boot image;
//! when either is missing the step reports [`Outcome::Unavailable`] and the
//! package runs with the JIT. A failing dex2oat does not fail the install
//! either ([`Outcome::Failed`]), as on Android, where an app without
//! compiled code still runs.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How compilation ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Compiled,
    NoCode,
    Unavailable(String),
    Failed(String),
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Compiled => "compiled",
            Self::NoCode => "no_code",
            Self::Unavailable(_) => "unavailable",
            Self::Failed(_) => "failed",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Unavailable(m) | Self::Failed(m) => m,
            _ => "",
        }
    }
}

/// Where the compiler is.
#[derive(Clone, Debug, Default)]
pub struct Compiler {
    /// `andruna-app.exe`; `None` when there is none.
    pub app_exe: Option<PathBuf>,
    /// The ART root passed as `--art-root`; `None` lets andruna-app use
    /// its default (`ANDRUNA_ART_ROOT`, else `art\` next to it).
    pub art_root: Option<PathBuf>,
    /// The Windows directory mounted as the guest root while compiling.
    pub scratch: PathBuf,
    /// Upper bound for one compilation.
    pub timeout: Duration,
}

/// The instruction set directory name ART uses.
pub fn isa() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    }
}

impl Compiler {
    /// The ART root andruna-app would use.
    fn effective_art_root(&self, exe: &Path) -> Option<PathBuf> {
        self.art_root
            .clone()
            .or_else(|| std::env::var_os("ANDRUNA_ART_ROOT").map(PathBuf::from))
            .or_else(|| exe.parent().map(|d| d.join("art")))
    }

    /// Compiles `apk` (a file in the package's directory).
    pub fn compile(&self, apk: &Path) -> Outcome {
        let Some(exe) = self.app_exe.as_ref().filter(|p| p.is_file()) else {
            return Outcome::Unavailable(match &self.app_exe {
                Some(p) => format!("{} not found", p.display()),
                None => "andruna-app is not installed".into(),
            });
        };
        let Some(root) = self.effective_art_root(exe).filter(|r| r.is_dir()) else {
            return Outcome::Unavailable("the ART root is not installed".into());
        };
        let boot = root.join("javalib").join(isa()).join("boot.art");
        if !boot.is_file() {
            return Outcome::Unavailable(format!(
                "the boot image ({}) is not built",
                boot.display()
            ));
        }
        if let Err(e) = std::fs::create_dir_all(&self.scratch) {
            return Outcome::Failed(format!("cannot create {}: {e}", self.scratch.display()));
        }
        let log = self.scratch.join("dex2oat.log");
        let (out, err) = match (
            std::fs::File::create(&log),
            std::fs::File::create(log.with_extension("err.log")),
        ) {
            (Ok(o), Ok(e)) => (Stdio::from(o), Stdio::from(e)),
            _ => (Stdio::null(), Stdio::null()),
        };
        let mut child = match Command::new(exe)
            .arg("--art-root")
            .arg(&root)
            .arg("--data")
            .arg(&self.scratch)
            .arg("--compile")
            .arg(apk)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return Outcome::Failed(format!("cannot start {}: {e}", exe.display())),
        };
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if start.elapsed() > self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Outcome::Failed(format!(
                        "dex2oat did not finish in {} s",
                        self.timeout.as_secs()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Outcome::Failed(e.to_string()),
            }
        };
        let odex = apk
            .parent()
            .unwrap_or(Path::new("."))
            .join("oat")
            .join(isa())
            .join(apk.file_stem().unwrap_or_default())
            .with_extension("odex");
        if status.success() && odex.is_file() {
            Outcome::Compiled
        } else {
            let tail = std::fs::read_to_string(log.with_extension("err.log")).unwrap_or_default();
            let tail: Vec<&str> = tail.lines().rev().take(5).collect();
            Outcome::Failed(format!(
                "dex2oat exited with {status}{}: {}",
                if odex.is_file() {
                    ""
                } else {
                    " and wrote no odex"
                },
                tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
            ))
        }
    }
}
