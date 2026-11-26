//! `cargo xtask fetch` and `cargo xtask patch`: AOSP sources at a pinned tag.
//!
//! The lock file (`aosp/aosp.lock`) names the remote, the release tag and,
//! per project, the commit the tag resolves to. `fetch` makes shallow clones
//! of exactly that tag and refuses a clone whose commit differs from the
//! lock. `patch` applies `patches/<project path>/*.patch` in file-name order
//! and records each applied patch (name and SHA-256) in the checkout's `.git`
//! directory, so a second run only applies new patches.
//!
//! A project can list `exclude` directories that are never written to the
//! checkout, because Windows cannot store some names below them (for
//! example `test?.properties`). `fetch` clones with `--no-checkout`, checks
//! every name in the tree, and refuses a name Windows cannot store unless
//! it is below an excluded directory (paths that differ only in case are
//! warnings, since git checks them out). The exclusions use a cone-mode sparse
//! checkout with a sparse index, so git's NTFS protection stays on (see
//! [`configure_sparse`]).

use std::collections::{BTreeMap, BTreeSet};
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
    /// Directories (relative to the project) that are not checked out.
    #[serde(default)]
    pub exclude: Vec<String>,
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
            if !relative_path_ok(&p.path) {
                return Err(Error::msg(format!(
                    "project {}: invalid path `{}`",
                    p.name, p.path
                )));
            }
            if !paths.insert(p.path.as_str()) {
                return Err(Error::msg(format!("duplicate project path `{}`", p.path)));
            }
            let mut excludes = BTreeSet::new();
            for e in &p.exclude {
                if !relative_path_ok(e) || e.chars().any(char::is_control) {
                    return Err(Error::msg(format!(
                        "project {}: invalid exclude `{e}`",
                        p.name
                    )));
                }
                if !excludes.insert(e.as_str()) {
                    return Err(Error::msg(format!(
                        "project {}: duplicate exclude `{e}`",
                        p.name
                    )));
                }
            }
            for e in &p.exclude {
                if let Some(o) = p.exclude.iter().find(|o| *o != e && is_under(e, o)) {
                    return Err(Error::msg(format!(
                        "project {}: exclude `{e}` is inside exclude `{o}`",
                        p.name
                    )));
                }
            }
        }
        Ok(())
    }
}

/// A relative `/`-separated path without empty, `.` or `..` components and
/// without backslashes.
fn relative_path_ok(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains('\\'))
}

/// Which lock, which checkout directory and which projects to act on.
pub struct Selection {
    lock_path: PathBuf,
    aosp_dir: PathBuf,
    only: Vec<String>,
}

impl Selection {
    /// The default lock and checkout directory, all projects.
    pub fn defaults() -> Self {
        let root = util::repo_root();
        Self {
            lock_path: root.join("aosp").join("aosp.lock"),
            aosp_dir: root.join("third_party").join("aosp"),
            only: Vec::new(),
        }
    }

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
                let names = tree_names(&dest)?;
                check_names(p, &names)?;
                let patterns = cone_patterns(&p.exclude, &names);
                if sparse_state(&dest)? != patterns {
                    println!("{}: updating the excluded paths", p.path);
                    if patterns.is_empty() {
                        util::run(
                            git()
                                .arg("-C")
                                .arg(&dest)
                                .args(["sparse-checkout", "disable"]),
                        )?;
                    } else {
                        configure_sparse(&dest, &patterns)?;
                        util::run(
                            git()
                                .arg("-C")
                                .arg(&dest)
                                .args(["sparse-checkout", "reapply"]),
                        )?;
                    }
                }
                verify_checkout(&dest, p, &names)?;
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
                    "clone",
                    "--quiet",
                    "--depth",
                    "1",
                    "--no-tags",
                    "--single-branch",
                    "--branch",
                    &lock.tag,
                    // Nothing is written until the commit and the names in
                    // its tree are checked and the exclusions are set up.
                    "--no-checkout",
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
        let names = tree_names(&partial)?;
        if let Err(e) = check_names(p, &names) {
            util::remove_dir_all_if_exists(&partial)?;
            return Err(e);
        }
        let patterns = cone_patterns(&p.exclude, &names);
        if !patterns.is_empty() {
            configure_sparse(&partial, &patterns)?;
        }
        // Fills the index from HEAD and writes the files the sparse-checkout
        // patterns (if any) include. The index is new, so with `index.sparse`
        // it is created sparse and the excluded directories enter it as one
        // tree entry each: git never sees the names below them.
        util::run(
            git()
                .arg("-C")
                .arg(&partial)
                .args(["reset", "--quiet", "--hard", "HEAD"]),
        )?;
        verify_checkout(&partial, p, &names)?;
        util::rename_with_retry(&partial, &dest)
            .context(|| format!("cannot move into {}", dest.display()))?;
        if p.exclude.is_empty() {
            println!("{}: fetched {}", p.path, short(&got));
        } else {
            println!(
                "{}: fetched {} without {} excluded path(s)",
                p.path,
                short(&got),
                p.exclude.len()
            );
        }
    }
    Ok(())
}

/// Every file path (blobs, symbolic links, submodules) in the tree of HEAD.
fn tree_names(dir: &Path) -> Result<Vec<String>> {
    let out = util::output_raw(git().arg("-C").arg(dir).args([
        "ls-tree",
        "-r",
        "-z",
        "--name-only",
        "HEAD",
    ]))?;
    Ok(split_nul(&out))
}

fn split_nul(s: &str) -> Vec<String> {
    s.split('\0')
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether `name` is `exclude` or lies below it.
fn is_under(name: &str, exclude: &str) -> bool {
    name.strip_prefix(exclude)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

const CASE_COLLISION: &str = "differs from another path only in case";

/// Fails when an exclude is not a directory of the tree (the lock is stale)
/// or when the tree has a name Windows cannot store outside the excludes.
/// Paths that differ only in case are reported as warnings.
fn check_names(p: &Project, names: &[String]) -> Result<()> {
    for e in &p.exclude {
        if !names.iter().any(|n| is_under(n, e) && n != e) {
            return Err(Error::msg(format!(
                "{}: the excluded path `{e}` is not a directory in the tree (only \
                 directories can be excluded)",
                p.path
            )));
        }
    }
    let kept: Vec<String> = names
        .iter()
        .filter(|n| !p.exclude.iter().any(|e| is_under(n, e)))
        .cloned()
        .collect();
    let mut problems = Vec::new();
    for (path, why) in windows_name_problems(&kept) {
        if why == CASE_COLLISION {
            // git checks these out (one spelling overwrites the other and
            // git warns), so they do not stop the fetch; files the build
            // uses must not be among them.
            println!("{}: warning: {path} {why}", p.path);
        } else {
            problems.push(format!("  {path}: {why}"));
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    Err(Error::msg(format!(
        "{}: the tree has paths Windows cannot check out; add them to `exclude` in the \
         lock:\n{}",
        p.path,
        problems.join("\n")
    )))
}

/// Paths in `names` (or their leading directories) that cannot be checked
/// out on NTFS, each with the reason: a character Windows forbids, a
/// trailing dot or space, a reserved device name, or a path that differs
/// from another only in case (all spellings but the first in byte order
/// are reported). A reported directory covers the paths below it.
fn windows_name_problems(names: &[String]) -> Vec<(String, &'static str)> {
    let mut problems = BTreeMap::new();
    let mut spellings: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in names {
        let mut end = 0;
        for component in name.split('/') {
            end += component.len();
            let prefix = &name[..end];
            end += 1;
            if let Some(why) = component_problem(component) {
                problems.entry(prefix.to_string()).or_insert(why);
                break;
            }
            spellings
                .entry(prefix.to_lowercase())
                .or_default()
                .insert(prefix.to_string());
        }
    }
    for set in spellings.values().filter(|s| s.len() > 1) {
        for other in set.iter().skip(1) {
            problems.entry(other.clone()).or_insert(CASE_COLLISION);
        }
    }
    let all: Vec<_> = problems.into_iter().collect();
    all.iter()
        .filter(|(path, _)| !all.iter().any(|(o, _)| o != path && is_under(path, o)))
        .cloned()
        .collect()
}

/// Why Windows cannot store a file or directory named `c`, if it cannot.
fn component_problem(c: &str) -> Option<&'static str> {
    let forbidden = |ch: char| matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\');
    if c.chars().any(|ch| forbidden(ch) || ch.is_ascii_control()) {
        return Some("contains a character Windows does not allow in names");
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return Some("ends with a dot or a space");
    }
    // `nul.txt` and `CON ` are device names too.
    let stem = c
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let numbered = stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit();
    (numbered || ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str()))
        .then_some("is a reserved device name")
}

/// The cone-mode sparse-checkout patterns that check out every path of
/// `names` except the directories in `exclude`; empty when nothing is
/// excluded. Each ancestor of an excluded directory gets its own files
/// (`/a/` and `!/a/*/`) and every other subdirectory recursively (`/a/b/`).
/// Cone mode is needed for a sparse index; see [`configure_sparse`].
fn cone_patterns(exclude: &[String], names: &[String]) -> Vec<String> {
    if exclude.is_empty() {
        return Vec::new();
    }
    // Every directory of the tree, with its parent directory.
    let mut dirs = BTreeSet::new();
    for n in names {
        let mut end = 0;
        while let Some(i) = n[end..].find('/') {
            end += i;
            dirs.insert(&n[..end]);
            end += 1;
        }
    }
    let excluded = |d: &str| exclude.iter().any(|e| e == d);
    let ancestor = |d: &str| exclude.iter().any(|e| is_under(e, d) && e != d);
    let mut patterns = vec!["/*".to_string(), "!/*/".to_string()];
    // Depth-first in path order, so each parent comes before its children.
    let mut stack: Vec<&str> = vec![""];
    while let Some(parent) = stack.pop() {
        if !parent.is_empty() {
            let p = cone_escape(parent);
            patterns.push(format!("/{p}/"));
            patterns.push(format!("!/{p}/*/"));
        }
        let children: Vec<&str> = dirs
            .iter()
            .copied()
            .filter(|d| match d.rfind('/') {
                Some(i) => &d[..i] == parent,
                None => parent.is_empty(),
            })
            .collect();
        let mut descend = Vec::new();
        for child in children {
            if excluded(child) {
                continue;
            }
            if ancestor(child) {
                descend.push(child);
            } else {
                patterns.push(format!("/{}/", cone_escape(child)));
            }
        }
        stack.extend(descend.into_iter().rev());
    }
    patterns
}

/// Escapes the characters sparse-checkout patterns treat specially.
fn cone_escape(path: &str) -> String {
    let mut s = String::with_capacity(path.len());
    for ch in path.chars() {
        if matches!(ch, '\\' | '*' | '?' | '[' | ']' | '!' | '#' | ' ') {
            s.push('\\');
        }
        s.push(ch);
    }
    s
}

fn git_config(dir: &Path, key: &str) -> Result<String> {
    // `--default` keeps a missing key from failing the command.
    util::output(
        git()
            .arg("-C")
            .arg(dir)
            .args(["config", "--type=bool", "--default=false", key]),
    )
}

/// The sparse-checkout patterns in effect in `dir`: empty when sparse
/// checkout is off, and a marker line when it is on without cone mode and a
/// sparse index (so it never equals a wanted pattern list).
fn sparse_state(dir: &Path) -> Result<Vec<String>> {
    if git_config(dir, "core.sparseCheckout")? != "true" {
        return Ok(Vec::new());
    }
    if git_config(dir, "core.sparseCheckoutCone")? != "true"
        || git_config(dir, "index.sparse")? != "true"
    {
        return Ok(vec!["(not cone mode with a sparse index)".to_string()]);
    }
    let file = dir.join(".git").join("info").join("sparse-checkout");
    let text = fs::read_to_string(&file).context(|| format!("cannot read {}", file.display()))?;
    Ok(text
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Turns on cone-mode sparse checkout with a sparse index and writes the
/// patterns; the working tree changes with the next `reset` or `reapply`.
///
/// The index must stay sparse. On Windows, with `core.protectNTFS` on (the
/// default, and it stays on), git refuses to put a path with a name Windows
/// cannot store into the index at all, skip-worktree or not, so a non-cone
/// exclusion of a single file cannot work. A sparse index stores each
/// excluded directory as one tree entry instead.
fn configure_sparse(dir: &Path, patterns: &[String]) -> Result<()> {
    for (key, value) in [
        ("core.sparseCheckout", "true"),
        ("core.sparseCheckoutCone", "true"),
        ("index.sparse", "true"),
    ] {
        util::run(git().arg("-C").arg(dir).args(["config", key, value]))?;
    }
    let info = dir.join(".git").join("info");
    fs::create_dir_all(&info).context(|| format!("cannot create {}", info.display()))?;
    let file = info.join("sparse-checkout");
    fs::write(&file, patterns.join("\n") + "\n")
        .context(|| format!("cannot write {}", file.display()))
}

/// Checks that HEAD is the locked commit, that the index holds every path of
/// the tree outside the excluded directories as a normal entry and each
/// excluded directory as one sparse directory entry, and that no excluded
/// directory exists in the working tree.
fn verify_checkout(dir: &Path, p: &Project, names: &[String]) -> Result<()> {
    let head = head_commit(dir)?.unwrap_or_default();
    if head != p.commit {
        return Err(Error::msg(format!(
            "{}: checkout is at {head}, the lock wants {}",
            p.path, p.commit
        )));
    }
    let mut expected: BTreeSet<String> = names
        .iter()
        .filter(|n| !p.exclude.iter().any(|e| is_under(n, e)))
        .map(|n| format!("H {n}"))
        .collect();
    expected.extend(p.exclude.iter().map(|e| format!("S {e}/")));
    // `--sparse` lists sparse directory entries instead of expanding the
    // index (which would fail on the names below them). `-t` tags entries
    // that are not checked out with `S`, the others with `H`.
    let listed = util::output_raw(
        git()
            .arg("-C")
            .arg(dir)
            .args(["ls-files", "--sparse", "-t", "-z"]),
    )?;
    let index: BTreeSet<String> = split_nul(&listed).into_iter().collect();
    if index != expected {
        let unexpected: Vec<_> = index.difference(&expected).take(10).collect();
        let missing: Vec<_> = expected.difference(&index).take(10).collect();
        return Err(Error::msg(format!(
            "{}: the index does not match the tree and the excluded directories \
             (unexpected: {unexpected:?}; missing: {missing:?})",
            p.path
        )));
    }
    if let Some(e) = p
        .exclude
        .iter()
        .find(|e| dir.join(e.as_str()).symlink_metadata().is_ok())
    {
        return Err(Error::msg(format!(
            "{}: the excluded directory `{e}` exists in the working tree",
            p.path
        )));
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
    fn parses_excludes() {
        assert!(Lock::parse(SAMPLE).unwrap().projects[0].exclude.is_empty());
        let with = format!("{SAMPLE}exclude = [\"a/test?.properties\", \"b/dir\"]\n");
        let lock = Lock::parse(&with).unwrap();
        assert_eq!(lock.projects[0].exclude, ["a/test?.properties", "b/dir"]);
    }

    #[test]
    fn rejects_bad_excludes() {
        for bad in [
            "",
            "/abs",
            "a//b",
            "a/../b",
            "./a",
            "a\\\\b",
            "a/",
            "a\\u0001b",
        ] {
            let text = format!("{SAMPLE}exclude = [\"{bad}\"]\n");
            assert!(Lock::parse(&text).is_err(), "accepted `{bad}`");
        }
        let twice = format!("{SAMPLE}exclude = [\"a\", \"a\"]\n");
        assert!(Lock::parse(&twice).is_err());
        let nested = format!("{SAMPLE}exclude = [\"a/b\", \"a\"]\n");
        assert!(Lock::parse(&nested).is_err());
        let siblings = format!("{SAMPLE}exclude = [\"a/b\", \"a/bc\"]\n");
        assert!(Lock::parse(&siblings).is_ok());
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn problem_paths(v: &[&str]) -> Vec<String> {
        windows_name_problems(&names(v))
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    #[test]
    fn valid_names_have_no_problems() {
        let ok = [
            "README.md",
            "a/b/c.txt",
            "console.h",
            "COM10",
            "lpt",
            ".hidden",
            "a b/c",
            "ünïcode/€.txt",
        ];
        assert!(problem_paths(&ok).is_empty());
    }

    #[test]
    fn finds_forbidden_characters() {
        for c in ['<', '>', ':', '"', '|', '?', '*', '\\', '\u{1}'] {
            let n = format!("dir/test{c}.properties");
            assert_eq!(problem_paths(&[&n]), std::slice::from_ref(&n), "{c:?}");
        }
        // A bad directory is reported once, not per file.
        assert_eq!(problem_paths(&["a:b/x", "a:b/y"]), ["a:b"]);
    }

    #[test]
    fn finds_trailing_dots_spaces_and_devices() {
        assert_eq!(problem_paths(&["a/b./c"]), ["a/b."]);
        assert_eq!(problem_paths(&["a/b "]), ["a/b "]);
        for d in [
            "CON", "prn", "Aux.h", "nul.txt", "com1", "LPT9.log", "con .x",
        ] {
            let n = format!("x/{d}");
            assert_eq!(problem_paths(&[&n]), std::slice::from_ref(&n), "{d}");
        }
    }

    #[test]
    fn finds_case_collisions() {
        assert_eq!(
            problem_paths(&["src/Foo.java", "src/foo.java", "src/bar"]),
            ["src/foo.java"]
        );
        // A collision of directories is reported once, at the directory.
        assert_eq!(problem_paths(&["Lib/a", "lib/b", "lib/c"]), ["lib"]);
    }

    #[test]
    fn excludes_cover_problems() {
        let p = |exclude: &[&str]| Project {
            name: "platform/x".into(),
            path: "x".into(),
            commit: "0".repeat(40),
            purpose: String::new(),
            exclude: names(exclude),
        };
        let tree = names(&["ok.txt", "res/test?.properties", "Dir/a", "dir/b"]);
        assert!(check_names(&p(&[]), &tree).is_err());
        // A case collision alone is only a warning.
        assert!(check_names(&p(&["res"]), &tree).is_ok());
        assert!(check_names(&p(&["res", "dir"]), &tree).is_ok());
        assert!(check_names(&p(&["res", "Dir"]), &tree).is_ok());
        // Only directories can be excluded.
        assert!(check_names(&p(&["res/test?.properties", "dir"]), &tree).is_err());
        assert!(check_names(&p(&["res", "dir", "ok.txt"]), &tree).is_err());
        // An exclude that matches nothing is stale.
        assert!(check_names(&p(&["res", "dir", "gone"]), &tree).is_err());
        // `re` is not a parent directory of `res/...`.
        assert!(check_names(&p(&["re", "dir"]), &tree).is_err());
    }

    #[test]
    fn cone_patterns_leave_out_only_the_excluded_directories() {
        let tree = names(&[
            "top.txt",
            "a/a.txt",
            "a/b/c/bad?.txt",
            "a/b/c/ok.txt",
            "a/b/b.txt",
            "a/b/d/x",
            "a/e/y",
            "f/z",
            "g h/[w]",
        ]);
        assert!(cone_patterns(&[], &tree).is_empty());
        assert_eq!(
            cone_patterns(&names(&["a/b/c"]), &tree),
            [
                "/*", "!/*/", "/f/", "/g\\ h/", "/a/", "!/a/*/", "/a/e/", "/a/b/", "!/a/b/*/",
                "/a/b/d/",
            ]
        );
        assert_eq!(
            cone_patterns(&names(&["a/b/c", "a/e"]), &tree),
            [
                "/*", "!/*/", "/f/", "/g\\ h/", "/a/", "!/a/*/", "/a/b/", "!/a/b/*/", "/a/b/d/"
            ]
        );
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
