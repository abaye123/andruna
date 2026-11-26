//! `cargo xtask fetch` and `cargo xtask patch`: AOSP sources at a pinned tag.
//!
//! The lock file (`aosp/aosp.lock`) names the remote, the release tag and,
//! per project, the commit the tag resolves to. `fetch` makes shallow clones
//! of exactly that tag and refuses a clone whose commit differs from the
//! lock. `patch` applies `patches/<project path>/*.patch` in file-name order
//! and records each applied patch (name and SHA-256) in the checkout's `.git`
//! directory, so a second run only applies new patches.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::Options;
use crate::util::{self, Context, Error, Result};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    pub remote: String,
    pub tag: String,
    #[serde(rename = "project")]
    pub projects: Vec<Project>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    /// Repository name on the remote, for example `platform/system/libbase`.
    pub name: String,
    /// Checkout path below the AOSP directory, for example `system/libbase`.
    pub path: String,
    /// The commit the tag resolves to (the peeled tag).
    pub commit: String,
    /// Why the project is needed.
    #[serde(default)]
    pub purpose: String,
}

impl Lock {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).context(|| format!("cannot read {}", path.display()))?;
        Self::parse(&text).context(|| format!("invalid lock file {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let lock: Lock = toml::from_str(text).map_err(|e| Error::msg(e.to_string()))?;
        lock.validate()?;
        Ok(lock)
    }

    fn validate(&self) -> Result<()> {
        let mut paths = BTreeSet::new();
        for p in &self.projects {
            if p.commit.len() != 40 || !p.commit.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::msg(format!(
                    "project {}: commit must be a 40-digit hex SHA-1",
                    p.name
                )));
            }
            let path_ok = !p.path.is_empty()
                && !p.path.starts_with('/')
                && p.path
                    .split('/')
                    .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains('\\'));
            if !path_ok {
                return Err(Error::msg(format!(
                    "project {}: invalid path `{}`",
                    p.name, p.path
                )));
            }
            if !paths.insert(p.path.as_str()) {
                return Err(Error::msg(format!("duplicate project path `{}`", p.path)));
            }
        }
        Ok(())
    }
}

/// Which lock, which checkout directory and which projects to act on.
pub struct Selection {
    lock_path: PathBuf,
    aosp_dir: PathBuf,
    only: Vec<String>,
}

impl Selection {
    pub fn from_options(opts: &mut Options) -> Result<Self> {
        let root = util::repo_root();
        Ok(Self {
            lock_path: opts
                .value("--lock")?
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("aosp").join("aosp.lock")),
            aosp_dir: opts
                .value("--aosp-dir")?
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("third_party").join("aosp")),
            only: opts.values("--project")?,
        })
    }

    fn projects<'a>(&self, lock: &'a Lock) -> Result<Vec<&'a Project>> {
        if self.only.is_empty() {
            return Ok(lock.projects.iter().collect());
        }
        self.only
            .iter()
            .map(|want| {
                lock.projects
                    .iter()
                    .find(|p| &p.path == want || &p.name == want)
                    .ok_or_else(|| Error::msg(format!("project `{want}` is not in the lock")))
            })
            .collect()
    }
}

fn git() -> Command {
    let mut c = Command::new("git");
    // Never prompt for credentials; fail instead.
    c.env("GIT_TERMINAL_PROMPT", "0");
    c
}

/// The commit checked out in `dir`, or `None` when it is not a git checkout.
fn head_commit(dir: &Path) -> Result<Option<String>> {
    if !dir.join(".git").exists() {
        return Ok(None);
    }
    util::output(git().arg("-C").arg(dir).args(["rev-parse", "HEAD"])).map(Some)
}

pub fn fetch(sel: &Selection) -> Result<()> {
    let lock = Lock::load(&sel.lock_path)?;
    fs::create_dir_all(&sel.aosp_dir)
        .context(|| format!("cannot create {}", sel.aosp_dir.display()))?;
    for p in sel.projects(&lock)? {
        let dest = sel.aosp_dir.join(&p.path);
        match head_commit(&dest)? {
            Some(c) if c == p.commit => {
                println!("{}: cached at {}", p.path, short(&c));
                continue;
            }
            Some(c) => {
                return Err(Error::msg(format!(
                    "{}: checkout is at {c}, the lock wants {}; delete {} and fetch again",
                    p.path,
                    p.commit,
                    dest.display()
                )));
            }
            None if dest.exists() => {
                return Err(Error::msg(format!(
                    "{} exists but is not a git checkout; delete it and fetch again",
                    dest.display()
                )));
            }
            None => {}
        }

        let partial = PathBuf::from(format!("{}.partial", dest.display()));
        util::remove_dir_all_if_exists(&partial)?;
        if let Some(parent) = partial.parent() {
            fs::create_dir_all(parent)?;
        }
        let url = format!("{}/{}", lock.remote.trim_end_matches('/'), p.name);
        println!("{}: cloning {url} at {} ({})", p.path, lock.tag, p.purpose);
        util::run(
            git()
                .args([
                    // A tag checkout is a detached HEAD by design.
                    "-c",
                    "advice.detachedHead=false",
                    "clone",
                    "--quiet",
                    "--depth",
                    "1",
                    "--no-tags",
                    "--single-branch",
                    "--branch",
                    &lock.tag,
                    // Keep files byte-exact so patches apply on every host.
                    "--config",
                    "core.autocrlf=false",
                    "--config",
                    "core.longpaths=true",
                    // AOSP uses symbolic links for shared headers (for
                    // example system/core/libutils/include). On Windows this
                    // needs Developer Mode or an elevated shell; otherwise
                    // git writes the link target as a text file.
                    "--config",
                    "core.symlinks=true",
                    &url,
                ])
                .arg(&partial),
        )?;
        let got = head_commit(&partial)?.unwrap_or_default();
        if got != p.commit {
            util::remove_dir_all_if_exists(&partial)?;
            return Err(Error::msg(format!(
                "{}: tag {} resolves to {got}, the lock says {}; the tag moved or the \
                 lock is wrong",
                p.path, lock.tag, p.commit
            )));
        }
        util::rename_with_retry(&partial, &dest)
            .context(|| format!("cannot move into {}", dest.display()))?;
        println!("{}: fetched {}", p.path, short(&got));
    }
    Ok(())
}

/// Records the applied patches of a checkout, one `<file name> <sha256>` line
/// each, in its `.git` directory so it never shows up as a change.
const STAMP: &str = "andruna-patches";

pub fn patch(sel: &Selection, patches_dir: &Path, reset: bool) -> Result<()> {
    let lock = Lock::load(&sel.lock_path)?;
    for p in sel.projects(&lock)? {
        let patches = patch_files(&patches_dir.join(&p.path))?;
        let dest = sel.aosp_dir.join(&p.path);
        if head_commit(&dest)?.is_none() {
            if patches.is_empty() {
                continue;
            }
            return Err(Error::msg(format!(
                "{} is not fetched; run `cargo xtask fetch` first",
                p.path
            )));
        }

        let stamp = dest.join(".git").join(STAMP);
        if reset {
            println!("{}: resetting the checkout to {}", p.path, short(&p.commit));
            util::run(
                git()
                    .arg("-C")
                    .arg(&dest)
                    .args(["reset", "--quiet", "--hard"]),
            )?;
            util::run(
                git()
                    .arg("-C")
                    .arg(&dest)
                    .args(["clean", "--quiet", "-fdx"]),
            )?;
            match fs::remove_file(&stamp) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(Error::msg(format!(
                        "cannot remove {}: {e}",
                        stamp.display()
                    )));
                }
                _ => {}
            }
        }

        let mut wanted = Vec::with_capacity(patches.len());
        for f in &patches {
            let name = f.file_name().unwrap_or_default().to_string_lossy();
            wanted.push(format!("{name} {}", util::sha256_file(f)?));
        }
        let applied: Vec<String> = match fs::read_to_string(&stamp) {
            Ok(s) => s.lines().map(str::to_string).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(Error::msg(format!("cannot read {}: {e}", stamp.display()))),
        };
        let first = pending_from(&applied, &wanted).ok_or_else(|| {
            Error::msg(format!(
                "{}: the patches changed since they were applied; run `cargo xtask patch \
                 --reset` to restore the checkout to the locked commit and apply them \
                 again (this discards local changes in {})",
                p.path,
                dest.display()
            ))
        })?;
        if first == patches.len() {
            if !patches.is_empty() {
                println!("{}: {} patch(es) already applied", p.path, patches.len());
            }
            continue;
        }

        for (patch, line) in patches.iter().zip(&wanted).skip(first) {
            let name = patch.file_name().unwrap_or_default().to_string_lossy();
            let apply = |check: bool| {
                let mut c = git();
                c.arg("-C")
                    .arg(&dest)
                    .args(["apply", "--whitespace=nowarn"]);
                if check {
                    c.arg("--check");
                }
                c.arg(patch);
                c
            };
            if !util::succeeds(&mut apply(true))? {
                return Err(Error::msg(format!(
                    "{}: {name} does not apply; the patch needs updating for tag {} \
                     (or the checkout has local changes)",
                    p.path, lock.tag
                )));
            }
            util::run(&mut apply(false))?;
            let mut f = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&stamp)
                .context(|| format!("cannot open {}", stamp.display()))?;
            writeln!(f, "{line}")?;
            println!("{}: applied {name}", p.path);
        }
    }
    Ok(())
}

/// Index of the first patch still to apply, or `None` when the applied
/// patches are not a prefix of the wanted ones (a patch changed, was
/// removed, or was inserted before an applied one).
fn pending_from(applied: &[String], wanted: &[String]) -> Option<usize> {
    wanted.starts_with(applied).then_some(applied.len())
}

/// The `*.patch` files in `dir`, sorted by file name; empty if `dir` is absent.
fn patch_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::msg(format!("cannot read {}: {e}", dir.display()))),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "patch") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
remote = "https://android.googlesource.com"
tag = "android-17.0.0_r1"

[[project]]
name = "platform/system/libbase"
path = "system/libbase"
commit = "fef4173e19d97cd4b4fc09a06bccbf800d0d3937"
purpose = "android-base"
"#;

    #[test]
    fn parses_sample() {
        let lock = Lock::parse(SAMPLE).unwrap();
        assert_eq!(lock.tag, "android-17.0.0_r1");
        assert_eq!(lock.projects.len(), 1);
        assert_eq!(lock.projects[0].path, "system/libbase");
    }

    #[test]
    fn rejects_bad_commit_and_path() {
        assert!(Lock::parse(&SAMPLE.replace("fef4173e", "zzzz")).is_err());
        assert!(
            Lock::parse(&SAMPLE.replace("path = \"system/libbase\"", "path = \"../x\"")).is_err()
        );
        assert!(Lock::parse(&SAMPLE.replace("purpose", "unknown_key")).is_err());
    }

    #[test]
    fn rejects_duplicate_paths() {
        let twice = format!("{SAMPLE}{}", &SAMPLE[SAMPLE.find("[[project]]").unwrap()..]);
        assert!(Lock::parse(&twice).is_err());
    }

    #[test]
    fn repository_lock_is_valid() {
        let lock = Lock::load(&util::repo_root().join("aosp").join("aosp.lock")).unwrap();
        assert!(lock.tag.starts_with("android-"));
        assert!(!lock.projects.is_empty());
    }

    #[test]
    fn pending_patches() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let wanted = s(&["0001 aa", "0002 bb"]);
        assert_eq!(pending_from(&[], &wanted), Some(0));
        assert_eq!(pending_from(&s(&["0001 aa"]), &wanted), Some(1));
        assert_eq!(pending_from(&wanted, &wanted), Some(2));
        // A changed or removed patch needs a reset.
        assert_eq!(pending_from(&s(&["0001 zz"]), &wanted), None);
        assert_eq!(pending_from(&wanted, &s(&["0001 aa"])), None);
        assert_eq!(pending_from(&s(&["0001 aa"]), &[]), None);
    }

    #[test]
    fn patch_files_are_sorted_and_filtered() {
        let dir = std::env::temp_dir().join(format!("xtask-patches-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for f in ["0002-b.patch", "0001-a.patch", "notes.txt"] {
            fs::write(dir.join(f), "").unwrap();
        }
        let names: Vec<_> = patch_files(&dir)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["0001-a.patch", "0002-b.patch"]);
        assert!(patch_files(&dir.join("missing")).unwrap().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
