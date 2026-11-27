//! The install flow end to end on real files: APKs built and signed at
//! test time, a store in a temporary directory, shortcuts in a temporary
//! folder and uninstall entries under a test registry key (removed at the
//! end).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use andruna_pkg::Code;
use andruna_pkg::db::PackageRecord;
use andruna_pkg::desktop::{Desktop, app_user_model_id};
use andruna_pkg::hooks::{InstallHooks, InstalledPackage};
use andruna_pkg::store::{Store, StoreConfig};
use andruna_pkg::sys;
use andruna_pkg::testkit::apk::{ActivitySpec, ApkSpec, IconSpec};
use andruna_pkg::testkit::sign::{KeyKind, SignConfig, TestKey};
use andruna_pkg::testkit::zipw::{ZipEntry, ZipWriter};

fn key() -> &'static TestKey {
    static K: OnceLock<TestKey> = OnceLock::new();
    K.get_or_init(|| TestKey::generate(KeyKind::P256, "Install test"))
}

fn other_key() -> &'static TestKey {
    static K: OnceLock<TestKey> = OnceLock::new();
    K.get_or_init(|| TestKey::generate(KeyKind::P256, "Other"))
}

struct Env {
    dir: PathBuf,
    reg: String,
    store: Store,
}

impl Env {
    fn new(name: &str) -> Self {
        Self::with_hooks(name, Arc::new(andruna_pkg::hooks::NoHooks))
    }

    fn with_hooks(name: &str, hooks: Arc<dyn InstallHooks>) -> Self {
        let dir =
            std::env::temp_dir().join(format!("andruna-pkg-install-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let reg = format!(r"Software\Andruna\Tests\pkg-{}-{name}", std::process::id());
        let mut cfg = StoreConfig::new(dir.join("root"));
        cfg.desktop = Some(Desktop {
            shortcuts_dir: dir.join("Programs"),
            uninstall_root: Some(reg.clone()),
            launch_exe: PathBuf::from(r"C:\Program Files\Andruna\andruna-launch.exe"),
        });
        cfg.ui_locale = "en-US".into();
        cfg.hooks = hooks;
        cfg.abis = vec!["x86_64".into(), "arm64-v8a".into()];
        Self {
            store: Store::open(cfg).unwrap(),
            dir,
            reg,
        }
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn apk(&self, name: &str, spec: &ApkSpec, key: &TestKey) -> PathBuf {
        self.write(name, &spec.build(&SignConfig::new(key.clone())))
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = sys::delete_user_key(&self.reg);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn files(dir: &Path) -> Vec<String> {
    let mut v = Vec::new();
    fn walk(base: &Path, d: &Path, v: &mut Vec<String>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, v);
            } else {
                v.push(
                    p.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    walk(dir, dir, &mut v);
    v.sort();
    v
}

#[test]
fn install_update_uninstall() {
    let env = Env::new("full");
    let mut spec = ApkSpec::new("org.example.full");
    spec.native_abis = vec!["x86_64".into(), "arm64-v8a".into()];
    spec.dex = Some(b"dex\n035\0not a real dex".to_vec());
    let apk = env.apk("full.apk", &spec, key());

    let r = env.store.install(std::slice::from_ref(&apk)).unwrap();
    assert!(!r.updated);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(
        r.compile.as_str(),
        "unavailable",
        "no andruna-app configured"
    );
    let rec = &r.record;
    assert_eq!(rec.package, "org.example.full");
    assert_eq!(rec.abi.as_deref(), Some("x86_64"));
    assert_eq!(rec.uid, 10_000);
    assert_eq!(rec.signing.signers, vec![key().cert_sha256()]);
    assert!(rec.labels.contains(&("".into(), "Test App".into())));
    assert!(rec.labels.iter().any(|(l, _)| l == "he"));
    assert_eq!(rec.launchers[0].activity, "org.example.full.MainActivity");
    let app = env.store.app_dir("org.example.full");
    assert_eq!(
        files(&app),
        ["base.apk", "icon.ico", "icon.png", "lib/x86_64/libtest.so"]
    );
    assert!(env.store.data_dir("org.example.full").is_dir());
    assert_eq!(
        std::fs::read(app.join("base.apk")).unwrap(),
        std::fs::read(&apk).unwrap()
    );

    // The shortcut, read back through the shell.
    let link = rec.launchers[0].shortcut.clone().unwrap();
    assert_eq!(link, env.dir.join("Programs").join("Test App.lnk"));
    let s = sys::read_shortcut(&link).unwrap();
    assert_eq!(
        s.target,
        PathBuf::from(r"C:\Program Files\Andruna\andruna-launch.exe")
    );
    assert_eq!(s.arguments, "org.example.full");
    assert_eq!(s.app_user_model_id, "Andruna.App.org.example.full");
    assert_eq!(s.icon.as_deref(), Some(app.join("icon.ico").as_path()));
    // The uninstall entry.
    let key_path = rec.uninstall_key.clone().unwrap();
    assert_eq!(
        sys::read_user_string(&key_path, "DisplayName").unwrap(),
        "Test App"
    );
    assert_eq!(
        sys::read_user_string(&key_path, "UninstallString").unwrap(),
        r#""C:\Program Files\Andruna\andruna-launch.exe" --uninstall org.example.full"#
    );

    // Same version again, then a newer one: an update keeps uid, salt and
    // install time.
    let r2 = env.store.install(std::slice::from_ref(&apk)).unwrap();
    assert!(r2.updated);
    spec.version_code = 2;
    spec.version_name = "2.0".into();
    spec.labels = vec![("".into(), "Renamed".into())];
    let v2 = env.apk("v2.apk", &spec, key());
    let r3 = env.store.install(&[v2]).unwrap();
    assert!(r3.updated);
    assert_eq!(r3.record.uid, rec.uid);
    assert_eq!(r3.record.install_salt, rec.install_salt);
    assert_eq!(r3.record.installed_ms, rec.installed_ms);
    assert_eq!(r3.record.version_code, 2);
    assert!(!link.exists(), "the old shortcut is replaced");
    assert!(env.dir.join("Programs").join("Renamed.lnk").exists());
    assert_eq!(env.store.list().len(), 1);

    // Downgrade and another signer are refused; nothing changes.
    let e = env.store.install(std::slice::from_ref(&apk)).unwrap_err();
    assert_eq!(e.code, Code::Downgrade);
    spec.version_code = 3;
    let foreign = env.apk("foreign.apk", &spec, other_key());
    assert_eq!(
        env.store.install(&[foreign]).unwrap_err().code,
        Code::SignerMismatch
    );
    assert_eq!(env.store.get("org.example.full").unwrap().version_code, 2);
    assert!(app.join("base.apk").is_file());

    // A database reopened from disk has the same record.
    let reopened = Store::open(StoreConfig::new(env.dir.join("root"))).unwrap();
    assert_eq!(
        reopened.get("org.example.full"),
        env.store.get("org.example.full")
    );
    drop(reopened);

    // Uninstall with keep_data, then the data stays.
    std::fs::write(
        env.store.data_dir("org.example.full").join("prefs.xml"),
        b"x",
    )
    .unwrap();
    env.store.uninstall("org.example.full", true).unwrap();
    assert!(!app.exists());
    assert!(
        env.store
            .data_dir("org.example.full")
            .join("prefs.xml")
            .exists()
    );
    assert!(!env.dir.join("Programs").join("Renamed.lnk").exists());
    assert!(sys::read_user_string(&key_path, "DisplayName").is_err());
    assert!(env.store.list().is_empty());
    assert_eq!(
        env.store
            .uninstall("org.example.full", false)
            .unwrap_err()
            .code,
        Code::NotInstalled
    );
    // No staging or trash directories remain.
    let left: Vec<_> = std::fs::read_dir(env.store.app_root())
        .unwrap()
        .flatten()
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn uninstall_without_keep_data_removes_data() {
    let env = Env::new("nodata");
    let apk = env.apk("a.apk", &ApkSpec::new("org.example.nodata"), key());
    env.store.install(&[apk]).unwrap();
    env.store.uninstall("org.example.nodata", false).unwrap();
    assert!(!env.store.data_dir("org.example.nodata").exists());
}

#[test]
fn splits_are_selected_and_checked() {
    let env = Env::new("splits");
    let base = ApkSpec::new("org.example.split");
    let mk = |name: &str, abi: Option<&str>| {
        let mut s = ApkSpec::split_of(&base, name);
        if let Some(a) = abi {
            s.native_abis = vec![a.into()];
        }
        s
    };
    let paths = vec![
        env.apk("base.apk", &base, key()),
        env.apk("x64.apk", &mk("config.x86_64", Some("x86_64")), key()),
        env.apk("arm.apk", &mk("config.arm64_v8a", Some("arm64-v8a")), key()),
        env.apk("he.apk", &mk("config.he", None), key()),
        env.apk("xx.apk", &mk("config.xxhdpi", None), key()),
        env.apk("m.apk", &mk("config.mdpi", None), key()),
    ];
    let r = env.store.install(&paths).unwrap();
    assert_eq!(
        r.record.splits,
        ["config.he", "config.x86_64", "config.xxhdpi"]
    );
    assert_eq!(r.record.abi.as_deref(), Some("x86_64"));
    let app = env.store.app_dir("org.example.split");
    assert_eq!(
        files(&app),
        [
            "base.apk",
            "icon.ico",
            "icon.png",
            "lib/x86_64/libtest.so",
            "split_config.he.apk",
            "split_config.x86_64.apk",
            "split_config.xxhdpi.apk"
        ]
    );

    // The same set as one .apks bundle.
    let mut bundle = ZipWriter::new();
    for (i, p) in paths.iter().enumerate() {
        bundle.add(ZipEntry::stored(
            &format!("splits/{i}.apk"),
            std::fs::read(p).unwrap(),
        ));
    }
    let apks = env.write("bundle.apks", &bundle.finish());
    assert!(env.store.install(&[apks]).unwrap().updated);

    // Errors.
    let two_bases = vec![paths[0].clone(), env.apk("b2.apk", &base, key())];
    assert_eq!(
        env.store.install(&two_bases).unwrap_err().code,
        Code::InvalidSplits
    );
    let mut wrong_version = mk("config.fr", None);
    wrong_version.version_code = 9;
    let set = vec![paths[0].clone(), env.apk("wv.apk", &wrong_version, key())];
    assert_eq!(
        env.store.install(&set).unwrap_err().code,
        Code::InvalidSplits
    );
    let set = vec![
        paths[0].clone(),
        env.apk("ws.apk", &mk("config.fr", None), other_key()),
    ];
    assert_eq!(
        env.store.install(&set).unwrap_err().code,
        Code::InvalidSplits
    );
    let only_split = vec![paths[3].clone()];
    assert_eq!(
        env.store.install(&only_split).unwrap_err().code,
        Code::InvalidSplits
    );
}

#[test]
fn rejections_have_clear_codes() {
    let env = Env::new("reject");
    let mut arm32 = ApkSpec::new("org.example.arm32");
    arm32.native_abis = vec!["armeabi-v7a".into()];
    let e = env
        .store
        .install(&[env.apk("a.apk", &arm32, key())])
        .unwrap_err();
    assert_eq!(e.code, Code::IncompatibleAbi);
    assert!(e.message.contains("armeabi-v7a"), "{e}");

    let mut new_sdk = ApkSpec::new("org.example.newsdk");
    new_sdk.min_sdk = 99;
    assert_eq!(
        env.store
            .install(&[env.apk("b.apk", &new_sdk, key())])
            .unwrap_err()
            .code,
        Code::SdkTooNew
    );

    let mut compressed = ApkSpec::new("org.example.arsc");
    compressed.compress_resources = true;
    let path = env.apk("c.apk", &compressed, key());
    assert_eq!(
        env.store
            .install(std::slice::from_ref(&path))
            .unwrap_err()
            .code,
        Code::Alignment
    );
    // Below API 30 Android accepts it.
    compressed.target_sdk = 29;
    env.store
        .install(&[env.apk("c2.apk", &compressed, key())])
        .unwrap();

    let mut cfg = SignConfig::new(key().clone());
    cfg.v2 = false;
    cfg.v3 = false;
    let unsigned = env.write("u.apk", &ApkSpec::new("org.example.unsigned").build(&cfg));
    assert_eq!(
        env.store.install(&[unsigned]).unwrap_err().code,
        Code::NoSignature
    );

    // v1 only is refused from target API 30.
    cfg.v1 = true;
    let v1 = env.write("v1.apk", &ApkSpec::new("org.example.v1").build(&cfg));
    assert_eq!(
        env.store.install(&[v1]).unwrap_err().code,
        Code::InvalidSignature
    );

    let mut tampered = ApkSpec::new("org.example.tamper").build(&SignConfig::new(key().clone()));
    let n = tampered.len();
    tampered[n / 3] ^= 0x01;
    let e = env
        .store
        .install(&[env.write("t.apk", &tampered)])
        .unwrap_err();
    assert!(
        matches!(
            e.code,
            Code::InvalidSignature | Code::InvalidArchive | Code::InvalidManifest
        ),
        "{e}"
    );

    let e = env
        .store
        .install(&[env.write("junk.apk", b"not a zip")])
        .unwrap_err();
    assert_eq!(e.code, Code::InvalidArchive);
    let e = env
        .store
        .install(&[env.dir.join("missing.apk")])
        .unwrap_err();
    assert_eq!(e.code, Code::Io);
    assert_eq!(env.store.list().len(), 1);
}

#[test]
fn several_launchers_get_one_shortcut_each() {
    let env = Env::new("multi");
    let mut spec = ApkSpec::new("org.example.multi");
    spec.activities = vec![
        ActivitySpec::launcher(".Main"),
        ActivitySpec::plain(".Settings"),
        ActivitySpec::launcher(".Notes"),
    ];
    spec.icon = IconSpec::Png(0xFF00AA00);
    let r = env
        .store
        .install(&[env.apk("m.apk", &spec, key())])
        .unwrap();
    assert_eq!(r.shortcuts, 2);
    let second = r.record.launchers[1].shortcut.clone().unwrap();
    assert_eq!(second.file_name().unwrap(), "Test App (Notes).lnk");
    let s = sys::read_shortcut(&second).unwrap();
    assert_eq!(s.arguments, "org.example.multi/org.example.multi.Notes");
    assert_eq!(s.app_user_model_id, app_user_model_id("org.example.multi"));
}

#[test]
fn shortcut_names_do_not_overwrite_other_files() {
    let env = Env::new("collide");
    std::fs::create_dir_all(env.dir.join("Programs")).unwrap();
    std::fs::write(
        env.dir.join("Programs").join("Test App.lnk"),
        b"someone else's",
    )
    .unwrap();
    let r = env
        .store
        .install(&[env.apk("a.apk", &ApkSpec::new("org.example.collide"), key())])
        .unwrap();
    assert_eq!(
        r.record.launchers[0]
            .shortcut
            .as_ref()
            .unwrap()
            .file_name()
            .unwrap(),
        "Test App (Android).lnk"
    );
    assert_eq!(
        std::fs::read(env.dir.join("Programs").join("Test App.lnk")).unwrap(),
        b"someone else's"
    );
}

#[derive(Default)]
struct Recorder {
    fail: bool,
    events: Mutex<Vec<String>>,
}

impl InstallHooks for Recorder {
    fn installed(&self, p: &InstalledPackage<'_>) -> Result<(), String> {
        assert!(p.app_dir.join("base.apk").is_file());
        assert!(p.data_dir.is_dir());
        self.events.lock().unwrap().push(format!(
            "installed {} update={}",
            p.record.package, p.update
        ));
        if self.fail {
            Err("profile creation failed".into())
        } else {
            Ok(())
        }
    }
    fn removed(&self, r: &PackageRecord, keep: bool) {
        self.events
            .lock()
            .unwrap()
            .push(format!("removed {} keep={keep}", r.package));
    }
}

#[test]
fn hooks_run_and_a_failing_hook_rolls_back() {
    let rec = Arc::new(Recorder::default());
    let env = Env::with_hooks("hooks", rec.clone());
    let spec = ApkSpec::new("org.example.hook");
    env.store
        .install(&[env.apk("a.apk", &spec, key())])
        .unwrap();
    env.store.uninstall("org.example.hook", false).unwrap();
    assert_eq!(
        *rec.events.lock().unwrap(),
        [
            "installed org.example.hook update=false",
            "removed org.example.hook keep=false"
        ]
    );

    let failing = Arc::new(Recorder {
        fail: true,
        ..Default::default()
    });
    let env = Env::with_hooks("hookfail", failing);
    let e = env
        .store
        .install(&[env.apk("a.apk", &spec, key())])
        .unwrap_err();
    assert!(e.message.contains("profile creation failed"), "{e}");
    assert!(env.store.list().is_empty());
    assert!(!env.store.app_dir("org.example.hook").exists());
    assert!(
        std::fs::read_dir(env.dir.join("Programs"))
            .map(|d| d.count())
            .unwrap_or(0)
            == 0
    );
}

#[test]
fn inspect_reports_without_installing() {
    let env = Env::new("inspect");
    let mut spec = ApkSpec::new("org.example.inspect");
    spec.native_abis = vec!["arm64-v8a".into()];
    let p = env.apk("i.apk", &spec, key());
    let i = env
        .store
        .inspect(std::slice::from_ref(&p), "he-IL")
        .unwrap();
    assert_eq!(i.set.package(), "org.example.inspect");
    assert_eq!(
        i.label,
        "\u{05d9}\u{05d9}\u{05e9}\u{05d5}\u{05dd} \u{05d1}\u{05d3}\u{05d9}\u{05e7}\u{05d4}"
    );
    assert_eq!(i.set.abi.as_deref(), Some("arm64-v8a"));
    assert!(i.icon_png.as_ref().unwrap().is_file());
    assert_eq!(i.installed_version_code, None);
    assert!(env.store.list().is_empty());
    assert_eq!(env.store.inspect(&[p], "fr").unwrap().label, "Test App");
}
