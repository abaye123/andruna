//! The staged environment: where things are, the AppContainer profile and
//! the grants that let the container reach them.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::args::Args;
use crate::win::{self, Sid};

/// The profile used by every test.
pub const PROFILE: &str = "andruna.s7.test";
/// The custom capability that stands for "may read Andruna's runtime
/// files".
pub const RUNTIME_CAPABILITY: &str = "andrunaRuntime";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grant {
    /// `ALL APPLICATION PACKAGES` (S-1-15-2-1) read and execute.
    Aap,
    /// The custom capability SID of `andrunaRuntime`.
    Cap,
    /// The package's own AppContainer SID.
    Pkg,
    /// Nothing: the container should fail to start ART.
    None,
}

impl Grant {
    pub fn parse(s: &str) -> io::Result<Grant> {
        Ok(match s {
            "aap" => Grant::Aap,
            "cap" => Grant::Cap,
            "pkg" => Grant::Pkg,
            "none" => Grant::None,
            _ => return Err(io::Error::other(format!("unknown grant {s}"))),
        })
    }
}

pub struct Env {
    pub repo: PathBuf,
    pub art_root: PathBuf,
    pub app_src: PathBuf,
    pub dex_src: PathBuf,
    pub stage: PathBuf,
}

impl Env {
    pub fn new(a: &Args) -> Env {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .map(|p| PathBuf::from(p.display().to_string().trim_start_matches(r"\\?\")))
            .unwrap();
        Env {
            art_root: a
                .opt("--art-root")
                .map(PathBuf::from)
                .unwrap_or_else(|| repo.join(r"out\guest\x64\art-root")),
            app_src: a
                .opt("--app")
                .map(PathBuf::from)
                .unwrap_or_else(|| repo.join(r"target\release\andruna-app.exe")),
            dex_src: repo.join(r"out\s1\java"),
            stage: repo.join(r"out\s7"),
            repo,
        }
    }

    pub fn bin(&self) -> PathBuf {
        self.stage.join("bin")
    }

    pub fn dex(&self) -> PathBuf {
        self.stage.join("dex")
    }

    pub fn data(&self, name: &str) -> PathBuf {
        self.stage.join("data").join(name)
    }

    pub fn app(&self) -> PathBuf {
        self.bin().join("andruna-app.exe")
    }

    pub fn s7(&self) -> PathBuf {
        self.bin().join("s7.exe")
    }

    /// The directories the container reads (runtime files).
    pub fn runtime_dirs(&self) -> Vec<PathBuf> {
        vec![self.art_root.clone(), self.bin(), self.dex()]
    }

    /// Copies the executables and the dex files into the stage.
    pub fn stage_files(&self) -> io::Result<()> {
        std::fs::create_dir_all(self.bin())?;
        std::fs::create_dir_all(self.dex())?;
        // Copy only what changed: a fresh copy of an executable is scanned
        // again by the antivirus at its first start, which would distort
        // the start times.
        let copy = |from: &Path, to: &Path| -> io::Result<()> {
            let (a, b) = (std::fs::metadata(from)?, std::fs::metadata(to).ok());
            let same =
                b.is_some_and(|b| b.len() == a.len() && b.modified().ok() >= a.modified().ok());
            if !same {
                std::fs::copy(from, to)?;
            }
            Ok(())
        };
        copy(&self.app_src, &self.app())?;
        let me = std::env::current_exe()?;
        if me != self.s7() {
            copy(&me, &self.s7())?;
        }
        for e in std::fs::read_dir(&self.dex_src)? {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "dex") {
                copy(&p, &self.dex().join(p.file_name().unwrap()))?;
            }
        }
        Ok(())
    }
}

pub struct Prepared {
    pub sid: Sid,
    pub runtime_cap: Sid,
    pub profile_ms: f64,
    pub grant_ms: f64,
    pub data_ms: f64,
    pub data: PathBuf,
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// Removes every spike grant from the runtime directories.
pub fn revoke_all(env: &Env, sid: &Sid, cap: &Sid) -> io::Result<()> {
    for d in env.runtime_dirs() {
        for s in [
            &win::all_app_packages(),
            cap,
            sid,
            &win::all_restricted_app_packages(),
        ] {
            win::set_access(&d, s, 0, true)?;
        }
    }
    Ok(())
}

/// Creates the profile, sets the runtime grant and a fresh data directory
/// `data/<name>` that only the package (and the user) can use.
pub fn prepare(env: &Env, grant: Grant, lpac: bool, name: &str) -> io::Result<Prepared> {
    env.stage_files()?;
    let t = Instant::now();
    let (sid, _) = win::create_profile(PROFILE)?;
    let profile_ms = ms(t);
    let cap = win::capability_sid(RUNTIME_CAPABILITY)?;
    revoke_all(env, &sid, &cap)?;
    let t = Instant::now();
    let who = match grant {
        Grant::Aap if lpac => Some(win::all_restricted_app_packages()),
        Grant::Aap => Some(win::all_app_packages()),
        Grant::Cap => Some(cap.clone()),
        Grant::Pkg => Some(sid.clone()),
        Grant::None => None,
    };
    if let Some(w) = &who {
        for d in env.runtime_dirs() {
            win::set_access(&d, w, win::FILE_GENERIC_READ_EXECUTE, false)?;
        }
    }
    let grant_ms = ms(t);
    let data = env.data(name);
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(data.parent().unwrap())?;
    let user = andruna_ipc::Endpoint::for_current_user("s7test")?
        .user()
        .to_string_sid();
    let t = Instant::now();
    // Protected DACL: the user, SYSTEM and the package only; low label so
    // the low-integrity container process can write.
    win::create_dir_sd(
        &data,
        &format!(
            "D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)(A;OICI;FA;;;{})S:(ML;OICI;NW;;;LW)",
            sid.string()
        ),
    )?;
    let data_ms = ms(t);
    Ok(Prepared {
        sid,
        runtime_cap: cap,
        profile_ms,
        grant_ms,
        data_ms,
        data,
    })
}

pub fn cleanup(a: &Args) -> io::Result<()> {
    let env = Env::new(a);
    let (sid, _) = win::create_profile(PROFILE)?;
    let cap = win::capability_sid(RUNTIME_CAPABILITY)?;
    revoke_all(&env, &sid, &cap)?;
    let _ = std::fs::remove_dir_all(env.stage.join("data"));
    win::delete_profile(PROFILE)?;
    println!("cleaned up: grants revoked, profile {PROFILE} deleted");
    Ok(())
}

/// Counts files and directories under `p`.
pub fn count_tree(p: &Path) -> (usize, usize) {
    let mut files = 0;
    let mut dirs = 0;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        dirs += 1;
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    files += 1;
                }
            }
        }
    }
    (files, dirs)
}
