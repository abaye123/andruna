//! Shared by the tests that start real app processes: where andruna-app,
//! the ART root and the test dex files are, and installing a test package.
//!
//! The ART root is `ANDRUNA_ART_ROOT` (default `out/guest/x64/art-root`),
//! the test dex files `ANDRUNA_TEST_DEX` (default `out/s1/java`),
//! andruna-app `ANDRUNA_APP_EXE` (default `target/<profile>/andruna-app.exe`).
//! When one is missing the tests report that and pass, unless
//! `ANDRUNA_REQUIRE_GUEST=1`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use andruna_sysd::Layout;
use andruna_sysd::appcontainer::{self, Salt};
use andruna_sysd::apps::HostConfig;
use andruna_sysd::apps::store::{InstalledPackage, Launchable, RecordStore};

pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn missing(what: &str) -> bool {
    if std::env::var_os("ANDRUNA_REQUIRE_GUEST").is_some() {
        panic!("{what} is missing");
    }
    eprintln!("skipped: {what} is missing");
    true
}

pub fn app_exe() -> Option<PathBuf> {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let p = std::env::var_os("ANDRUNA_APP_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let target = std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| repo().join("target"));
            target.join(profile).join("andruna-app.exe")
        });
    if p.is_file() {
        Some(p)
    } else {
        missing("andruna-app.exe");
        None
    }
}

/// andruna-app if it is built, without reporting a skip.
pub fn app_exe_quiet() -> Option<PathBuf> {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let p = std::env::var_os("ANDRUNA_APP_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let target = std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| repo().join("target"));
            target.join(profile).join("andruna-app.exe")
        });
    p.is_file().then_some(p)
}

/// ART root and the test dex directory, or None (skipped).
pub fn guest() -> Option<(PathBuf, PathBuf)> {
    let root = std::env::var_os("ANDRUNA_ART_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join(r"out\guest\x64\art-root"));
    let dex = std::env::var_os("ANDRUNA_TEST_DEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join(r"out\s1\java"));
    if !root.join(r"lib64\libart.so").is_file() || !dex.join("hello.dex").is_file() {
        missing("the ART root or the test dex files");
        return None;
    }
    Some((std::path::absolute(root).unwrap(), dex))
}

/// Copies andruna-app into `<root>\install` (the install directory, which
/// sysd grants to the runtime capability) and returns the host config.
pub fn host_config(root: &Path, art_root: &Path, map_boot_image: bool) -> Option<HostConfig> {
    let src = app_exe()?;
    let install = root.join("install");
    std::fs::create_dir_all(&install).unwrap();
    let exe = install.join("andruna-app.exe");
    std::fs::copy(&src, &exe).unwrap();
    Some(HostConfig {
        exe,
        art_root: art_root.to_path_buf(),
        map_boot_image,
        runtime_options: vec![],
        environment: std::env::var("ANDRUNA_APP_ENV")
            .ok()
            .map(|v| v.split(';').map(str::to_string).collect())
            .unwrap_or_default(),
    })
}

/// Installs a package as the package manager will: profile and
/// directories, code, record. `dex: None` makes a framework package.
pub fn install(
    layout: &Layout,
    name: &str,
    dex: Option<&Path>,
    class: &str,
    args: &[&str],
) -> Salt {
    let salt = Salt::generate().unwrap();
    appcontainer::install(layout, name, &salt).unwrap();
    let launch = match dex {
        Some(dex) => {
            let code = appcontainer::app_dir(layout, name).join(dex.file_name().unwrap());
            std::fs::copy(dex, &code).unwrap();
            Launchable::Main {
                code: vec![code],
                class: class.into(),
                args: args.iter().map(|a| a.to_string()).collect(),
            }
        }
        None => Launchable::Framework,
    };
    RecordStore::write(
        layout,
        &InstalledPackage {
            name: name.into(),
            salt: salt.clone(),
            launch,
            mode: "jit".into(),
            permissions: vec![],
        },
    )
    .unwrap();
    salt
}
