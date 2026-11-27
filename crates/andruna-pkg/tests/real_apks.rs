//! APKs made by real tools:
//!
//! - `tests/fixtures/real/fixture.apk`, built from `src/` by aapt2, d8,
//!   zipalign and apksigner (`build.py`); committed, 13 KB.
//! - An open-source app from F-Droid (Binary Eye 1.76.1, MIT license),
//!   downloaded at test time and checked against a pinned SHA-256; the test
//!   is skipped offline unless `ANDRUNA_REQUIRE_NETWORK=1`.
//!
//! The dex2oat step runs for real when `ANDRUNA_APP_EXE` (andruna-app.exe)
//! and `ANDRUNA_ART_ROOT` (an ART root with a boot image) are set; it is
//! skipped otherwise, unless `ANDRUNA_REQUIRE_GUEST=1`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use andruna_pkg::apk;
use andruna_pkg::arsc::ResTable;
use andruna_pkg::compile::{Compiler, Outcome, isa};
use andruna_pkg::sig::Scheme;
use andruna_pkg::store::{Store, StoreConfig};
use andruna_pkg::sys::MappedFile;
use andruna_pkg::zip::Archive;
use sha2::{Digest, Sha256};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/real/fixture.apk")
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("andruna-pkg-real-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn pixel(png: &[u8], x: u32, y: u32) -> [u8; 4] {
    let p = tiny_skia::Pixmap::decode_png(png).unwrap();
    let c = p.pixel(x, y).unwrap().demultiply();
    [c.red(), c.green(), c.blue(), c.alpha()]
}

#[test]
fn fixture_from_aapt2_d8_and_apksigner() {
    let p = apk::parse_file(&fixture()).unwrap();
    let m = &p.manifest;
    assert_eq!(m.package, "org.andruna.fixture");
    assert_eq!(m.version_code, 3);
    assert_eq!(m.version_name.as_deref(), Some("1.2.3"));
    assert_eq!((m.min_sdk, m.target_sdk), (26, 35));
    assert!(m.has_code);
    let perms: Vec<_> = m.permissions.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        perms,
        ["android.permission.INTERNET", "android.permission.VIBRATE"]
    );
    let launchers: Vec<_> = m.launcher_activities().map(|a| a.name.as_str()).collect();
    assert_eq!(launchers, ["org.andruna.fixture.MainActivity"]);
    assert_eq!(m.activities.len(), 2);
    assert_eq!(p.signing.scheme, Scheme::V3);
    assert_eq!(p.signing.signers.len(), 1);
    assert!(p.abis.is_empty());

    let map = MappedFile::open(&fixture()).unwrap();
    let a = Archive::parse(&map).unwrap();
    let table = ResTable::parse(&a.read_name("resources.arsc").unwrap().unwrap()).unwrap();
    let labels = apk::labels(Some(&table), m.label.as_ref());
    assert_eq!(
        apk::label_for(&labels, "en-US").as_deref(),
        Some("Andruna Fixture")
    );
    assert_eq!(
        apk::label_for(&labels, "he-IL").as_deref(),
        Some("\u{05de}\u{05ea}\u{05e7}\u{05df} \u{05d1}\u{05d3}\u{05d9}\u{05e7}\u{05d4}")
    );
    let icon = andruna_pkg::icon::render(&a, Some(&table), m.icon.unwrap()).unwrap();
    assert_eq!(icon.kind, "adaptive-icon");
    // The 36 dp foreground square covers the middle half of the 72 dp
    // visible area; the background shows around it.
    assert_eq!(pixel(&icon.png256, 128, 128), [0x07, 0x30, 0x42, 255]);
    assert_eq!(pixel(&icon.png256, 128, 40), [0x3D, 0xDC, 0x84, 255]);
    assert_eq!(pixel(&icon.png256, 0, 0)[3], 0);
}

fn compiler_from_env(scratch: &Path) -> Option<Compiler> {
    let exe = std::env::var_os("ANDRUNA_APP_EXE").map(PathBuf::from);
    let root = std::env::var_os("ANDRUNA_ART_ROOT").map(PathBuf::from);
    match (exe, root) {
        (Some(exe), Some(root)) if exe.is_file() && root.is_dir() => Some(Compiler {
            app_exe: Some(exe),
            art_root: Some(root),
            scratch: scratch.join("dex2oat"),
            timeout: Duration::from_secs(600),
        }),
        _ => {
            assert!(
                std::env::var_os("ANDRUNA_REQUIRE_GUEST").is_none(),
                "ANDRUNA_REQUIRE_GUEST is set but ANDRUNA_APP_EXE / ANDRUNA_ART_ROOT are not usable"
            );
            None
        }
    }
}

#[test]
fn install_fixture_with_dex2oat() {
    let dir = scratch("dex2oat");
    let mut cfg = StoreConfig::new(dir.join("root"));
    let compiler = compiler_from_env(&dir);
    let have_compiler = compiler.is_some();
    if let Some(c) = compiler {
        cfg.compiler = c;
    }
    let store = Store::open(cfg).unwrap();
    let r = store.install(&[fixture()]).unwrap();
    let odex = store
        .app_dir("org.andruna.fixture")
        .join("oat")
        .join(isa())
        .join("base.odex");
    if have_compiler {
        assert_eq!(r.compile, Outcome::Compiled, "{}", r.compile.message());
        assert!(odex.is_file());
        assert!(odex.with_extension("vdex").is_file());
        eprintln!(
            "dex2oat wrote {} bytes of odex",
            std::fs::metadata(&odex).unwrap().len()
        );
    } else {
        eprintln!("dex2oat skipped: set ANDRUNA_APP_EXE and ANDRUNA_ART_ROOT to run it");
        assert_eq!(r.compile.as_str(), "unavailable");
        assert!(!odex.exists());
    }
    store.uninstall("org.andruna.fixture", false).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The pinned open-source APK.
const FDROID_FILE: &str = "de.markusfisch.android.binaryeye_184.apk";
const FDROID_SHA256: &str = "6995b9a4242ea54141ac0a31a8b933a764c84d64e45d6082a33d7d808fdabedb";
/// F-Droid moves older versions from `repo` to `archive`; the main site
/// refuses some automated clients, so mirrors are tried too.
const FDROID_URLS: &[&str] = &[
    "https://f-droid.org/repo/",
    "https://ftp.lysator.liu.se/pub/fdroid/repo/",
    "https://f-droid.org/archive/",
    "https://ftp.lysator.liu.se/pub/fdroid/archive/",
];

fn fetch_fdroid() -> Option<PathBuf> {
    let cache = Path::new(env!("CARGO_TARGET_TMPDIR")).join("downloads");
    std::fs::create_dir_all(&cache).ok()?;
    let path = cache.join(FDROID_FILE);
    let ok = |p: &Path| {
        std::fs::read(p)
            .map(|b| format!("{:x}", Sha256::digest(&b)) == FDROID_SHA256)
            .unwrap_or(false)
    };
    if ok(&path) {
        return Some(path);
    }
    for base in FDROID_URLS {
        let tmp = path.with_extension("part");
        let st = Command::new("curl.exe")
            .args(["-fsSL", "--max-time", "120", "-o"])
            .arg(&tmp)
            .arg(format!("{base}{FDROID_FILE}"))
            .status();
        if st.is_ok_and(|s| s.success()) && ok(&tmp) && std::fs::rename(&tmp, &path).is_ok() {
            return Some(path);
        }
        let _ = std::fs::remove_file(&tmp);
    }
    None
}

#[test]
fn open_source_app_from_fdroid() {
    let Some(path) = fetch_fdroid() else {
        assert!(
            std::env::var_os("ANDRUNA_REQUIRE_NETWORK").is_none(),
            "could not download {FDROID_FILE}"
        );
        eprintln!("skipped: {FDROID_FILE} could not be downloaded");
        return;
    };
    let p = apk::parse_file(&path).unwrap();
    let m = &p.manifest;
    assert_eq!(m.package, "de.markusfisch.android.binaryeye");
    assert_eq!(m.version_code, 184);
    assert_eq!(m.version_name.as_deref(), Some("1.76.1"));
    assert!(m.launcher_activities().count() >= 1);
    assert!(p.signing.scheme >= Scheme::V2, "{:?}", p.signing.scheme);
    eprintln!(
        "signature v{}, ABIs {:?}, {} activities, {} permissions",
        p.signing.scheme.number(),
        p.abis,
        m.activities.len(),
        m.permissions.len()
    );

    let dir = scratch("fdroid");
    let store = Store::open(StoreConfig::new(dir.join("root"))).unwrap();
    let i = store.inspect(std::slice::from_ref(&path), "en").unwrap();
    assert_eq!(i.label, "Binary Eye");
    let png = std::fs::read(i.icon_png.as_ref().expect("icon")).unwrap();
    let opaque = tiny_skia::Pixmap::decode_png(&png)
        .unwrap()
        .pixels()
        .iter()
        .filter(|px| px.alpha() > 0)
        .count();
    assert!(
        opaque > 256 * 256 / 4,
        "the icon has content ({opaque} pixels)"
    );
    if !p.abis.is_empty() {
        assert!(
            i.set.abi.is_some(),
            "a 64-bit ABI is selected from {:?}",
            p.abis
        );
    }
    let r = store.install(&[path]).unwrap();
    assert!(r.record.has_icon, "{:?}", r.warnings);
    assert!(store.app_dir(&m.package).join("icon.ico").is_file());
    store.uninstall(&m.package, false).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
