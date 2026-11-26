//! Build orchestration for Andruna. Run as `cargo xtask <command>`.

mod aosp;
mod build_cmd;
mod toolchain;
mod util;

use std::path::PathBuf;
use std::process::ExitCode;

use util::{Error, Result};

const USAGE: &str = "\
Usage: cargo xtask <command> [options]

Commands:
  build [--arch x64|arm64] [--release]
        Build the Rust workspace for <arch>-pc-windows-gnullvm with llvm-mingw.
        The default arch is the host's.
  fetch [--project <path>]... [--lock <file>] [--aosp-dir <dir>]
        Shallow-clone the AOSP projects in aosp/aosp.lock at the pinned tag
        into third_party/aosp/<path>, and verify each commit. Projects that
        are already present at the locked commit are skipped.
  patch [--project <path>]... [--reset] [--lock <file>] [--aosp-dir <dir>] [--patches-dir <dir>]
        Apply patches/<path>/*.patch in file-name order with `git apply`.
        Patches already applied (recorded with their SHA-256) are skipped.
        --reset first restores each checkout to the locked commit, which is
        needed after a patch changed or was removed.
  toolchain [--dir <dir>] [--force]
        Download and install the pinned llvm-mingw release, verified by
        SHA-256, into <dir> (default: $ANDRUNA_LLVM_MINGW, else
        C:\\tools\\llvm-mingw). Skipped when the same release is installed.
  help  Show this text.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    let Some((command, rest)) = args.split_first() else {
        print!("{USAGE}");
        return Ok(());
    };
    let mut opts = Options::parse(rest)?;
    match command.as_str() {
        "build" => {
            let arch = match opts.value("--arch")? {
                Some(a) => build_cmd::Arch::parse(&a)?,
                None => build_cmd::Arch::host(),
            };
            let release = opts.flag("--release");
            opts.finish()?;
            build_cmd::build(arch, release)
        }
        "fetch" => {
            let sel = aosp::Selection::from_options(&mut opts)?;
            opts.finish()?;
            aosp::fetch(&sel)
        }
        "patch" => {
            let sel = aosp::Selection::from_options(&mut opts)?;
            let patches_dir = opts
                .value("--patches-dir")?
                .map(PathBuf::from)
                .unwrap_or_else(|| util::repo_root().join("patches"));
            let reset = opts.flag("--reset");
            opts.finish()?;
            aosp::patch(&sel, &patches_dir, reset)
        }
        "toolchain" => {
            let dir = opts.value("--dir")?.map(PathBuf::from);
            let force = opts.flag("--force");
            opts.finish()?;
            toolchain::install(dir, force)
        }
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(Error::msg(format!(
            "unknown command `{other}`; run `cargo xtask help`"
        ))),
    }
}

/// A minimal option parser: `--name value`, `--name=value` and `--flag`.
pub struct Options {
    items: Vec<(String, Option<String>)>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut items = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if !a.starts_with("--") {
                return Err(Error::msg(format!("unexpected argument `{a}`")));
            }
            if let Some((k, v)) = a.split_once('=') {
                items.push((k.to_string(), Some(v.to_string())));
            } else if args.get(i + 1).is_some_and(|n| !n.starts_with("--")) {
                items.push((a.clone(), Some(args[i + 1].clone())));
                i += 1;
            } else {
                items.push((a.clone(), None));
            }
            i += 1;
        }
        Ok(Self { items })
    }

    /// Removes and returns every value given for `name`.
    pub fn values(&mut self, name: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut err = None;
        self.items.retain(|(k, v)| {
            if k != name {
                return true;
            }
            match v {
                Some(v) => out.push(v.clone()),
                None => err = Some(Error::msg(format!("`{name}` needs a value"))),
            }
            false
        });
        match err {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    /// Removes and returns the single value given for `name`, if any.
    pub fn value(&mut self, name: &str) -> Result<Option<String>> {
        let mut v = self.values(name)?;
        if v.len() > 1 {
            return Err(Error::msg(format!("`{name}` given more than once")));
        }
        Ok(v.pop())
    }

    /// Removes a flag without a value; true when it was present.
    ///
    /// A flag directly followed by a non-option word would have been parsed
    /// as `--flag value`; that word is reported as unexpected by `finish`.
    pub fn flag(&mut self, name: &str) -> bool {
        let mut found = false;
        let mut stray = Vec::new();
        self.items.retain(|(k, v)| {
            if k != name {
                return true;
            }
            found = true;
            if let Some(v) = v {
                stray.push(v.clone());
            }
            false
        });
        for s in stray {
            self.items.push((s, None));
        }
        found
    }

    /// Fails if any option was not consumed.
    fn finish(self) -> Result<()> {
        match self.items.first() {
            None => Ok(()),
            Some((k, _)) => Err(Error::msg(format!("unexpected argument `{k}`"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(a: &[&str]) -> Options {
        Options::parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn parses_values_and_flags() {
        let mut o = opts(&["--arch", "arm64", "--release", "--project=art"]);
        assert_eq!(o.value("--arch").unwrap().as_deref(), Some("arm64"));
        assert!(o.flag("--release"));
        assert_eq!(o.values("--project").unwrap(), vec!["art"]);
        o.finish().unwrap();
    }

    #[test]
    fn repeated_values() {
        let mut o = opts(&["--project", "art", "--project", "bionic"]);
        assert_eq!(o.values("--project").unwrap(), vec!["art", "bionic"]);
    }

    #[test]
    fn rejects_unknown_and_positional() {
        assert!(Options::parse(&["art".to_string()]).is_err());
        let o = opts(&["--bogus"]);
        assert!(o.finish().is_err());
        let mut o = opts(&["--release", "stray"]);
        assert!(o.flag("--release"));
        assert!(o.finish().is_err());
    }

    #[test]
    fn value_needs_argument() {
        let mut o = opts(&["--arch"]);
        assert!(o.value("--arch").is_err());
    }
}
