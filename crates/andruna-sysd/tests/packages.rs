//! Package manager requests over the real pipe: inspect, install, list,
//! start activity, uninstall, with APKs built and signed at test time.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use andruna_ipc::{
    Client, CompileStatus, Endpoint, ErrorCode, PipeListener, PipeSecurity, Request, Response,
};
use andruna_pkg::testkit::apk::{ActivitySpec, ApkSpec};
use andruna_pkg::testkit::sign::{KeyKind, SignConfig, TestKey};
use andruna_sysd::{Activity, Context, Layout, Server, Supervisor};

struct Fixture {
    endpoint: Endpoint,
    root: PathBuf,
    ctx: Arc<Context>,
    server: Option<Server>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let instance = format!("p{}-{name}", std::process::id());
        let root = std::env::temp_dir().join(format!("andruna-sysd-{instance}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let endpoint = Endpoint::for_current_user(&instance).unwrap();
        let listener =
            PipeListener::bind(&endpoint, &PipeSecurity::current_user_only(&endpoint)).unwrap();
        let layout = Layout::new(root.clone());
        let activity = Arc::new(Activity::new(Duration::from_secs(3600)));
        let supervisor = Arc::new(
            Supervisor::new(
                PathBuf::from(env!("CARGO_BIN_EXE_andruna-sysd")),
                layout.clone(),
                activity.clone(),
            )
            .unwrap(),
        );
        let store = Arc::new(andruna_sysd::open_store(&layout, false).unwrap());
        let ctx = Arc::new(Context {
            layout,
            activity,
            supervisor,
            store,
            started: Instant::now(),
        });
        let server = Server::spawn(listener, ctx.clone()).unwrap();
        Self {
            endpoint,
            root,
            ctx,
            server: Some(server),
        }
    }

    fn client(&self) -> Client {
        Client::connect(&self.endpoint, Duration::from_secs(5)).unwrap()
    }

    fn apk(&self, name: &str, spec: &ApkSpec, key: &TestKey) -> String {
        let p = self.root.join(name);
        fs::write(&p, spec.build(&SignConfig::new(key.clone()))).unwrap();
        p.display().to_string()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.ctx.supervisor.shutdown();
        if let Some(s) = self.server.take() {
            s.stop();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Shortcuts in the test root's Start menu folder.
fn links(f: &Fixture) -> usize {
    fs::read_dir(f.root.join("start-menu"))
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "lnk"))
                .count()
        })
        .unwrap_or(0)
}

fn code(r: Response) -> ErrorCode {
    match r {
        Response::Error { code, .. } => code,
        r => panic!("expected an error, got {r:?}"),
    }
}

#[test]
fn install_list_start_uninstall() {
    let f = Fixture::new("flow");
    let key = TestKey::generate(KeyKind::P256, "sysd test");
    let mut spec = ApkSpec::new("org.example.sysd");
    spec.activities = vec![
        ActivitySpec::launcher(".Main"),
        ActivitySpec::launcher(".Second"),
    ];
    spec.native_abis = vec!["arm64-v8a".into()];
    spec.dex = Some(b"dex\n035\0".to_vec());
    let apk = f.apk("app.apk", &spec, &key);
    let mut c = f.client();

    // Inspect: details without installing.
    match c
        .request(&Request::InspectPackage {
            paths: vec![apk.clone()],
            locale: "he-IL".into(),
        })
        .unwrap()
    {
        Response::PackageDetails(d) => {
            assert_eq!(d.package, "org.example.sysd");
            assert_eq!(d.version_code, 1);
            assert!(d.label.starts_with('\u{05d9}'), "{}", d.label);
            assert_eq!(d.signer_sha256, key.cert_sha256());
            assert_eq!(d.signature_scheme, 3);
            assert_eq!(d.permissions, ["android.permission.INTERNET"]);
            assert_eq!(d.native_abis, ["arm64-v8a"]);
            assert_eq!(d.installed_version_code, 0);
            assert!(PathBuf::from(&d.icon_png).is_file());
        }
        r => panic!("{r:?}"),
    }
    assert_eq!(
        c.request(&Request::ListPackages).unwrap(),
        Response::Packages { packages: vec![] }
    );

    // Install.
    match c
        .request(&Request::InstallPackage {
            paths: vec![apk.clone()],
        })
        .unwrap()
    {
        Response::Installed(o) => {
            assert_eq!(o.package, "org.example.sysd");
            assert!(!o.updated);
            // No andruna-app next to the test executable: reported, not
            // faked.
            assert_eq!(o.compile, CompileStatus::Unavailable);
            assert!(!o.compile_message.is_empty());
            assert_eq!(o.shortcuts, 2);
        }
        r => panic!("{r:?}"),
    }
    assert_eq!(
        c.request(&Request::ListPackages).unwrap(),
        Response::Packages {
            packages: vec!["org.example.sysd".into()]
        }
    );
    match c
        .request(&Request::ListPackageInfo {
            locale: "en".into(),
        })
        .unwrap()
    {
        Response::PackageInfos { packages } => {
            assert_eq!(packages.len(), 1);
            let p = &packages[0];
            assert_eq!(p.label, "Test App");
            assert_eq!(
                p.launcher_activities,
                ["org.example.sysd.Main", "org.example.sysd.Second"]
            );
            assert_eq!(p.app_user_model_id, "Andruna.App.org.example.sysd");
            assert!(PathBuf::from(&p.icon_ico).is_file());
            assert!(p.size_bytes > 0);
        }
        r => panic!("{r:?}"),
    }
    // Shortcuts of a test root go to <root>\start-menu.
    // Named in the Windows UI language; one per launcher activity.
    assert_eq!(links(&f), 2);

    // Start a launcher activity (the placeholder host runs the package).
    let r = c
        .request(&Request::StartActivity {
            package: "org.example.sysd".into(),
            activity: "org.example.sysd.Second".into(),
        })
        .unwrap();
    assert!(matches!(r, Response::Started { .. }), "{r:?}");

    // A running package is not uninstalled.
    let uninstall = Request::UninstallPackage {
        package: "org.example.sysd".into(),
        keep_data: false,
    };
    assert_eq!(code(c.request(&uninstall).unwrap()), ErrorCode::Busy);
    f.ctx.supervisor.shutdown();
    let t = Instant::now();
    while f.ctx.supervisor.host_pid("org.example.sysd").is_some() {
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(c.request(&uninstall).unwrap(), Response::Uninstalled);
    assert_eq!(
        c.request(&Request::ListPackages).unwrap(),
        Response::Packages { packages: vec![] }
    );
    assert_eq!(links(&f), 0);
    assert_eq!(
        code(c.request(&uninstall).unwrap()),
        ErrorCode::NotInstalled
    );
}

#[test]
fn refusals_map_to_error_codes() {
    let f = Fixture::new("refuse");
    let key = TestKey::generate(KeyKind::P256, "sysd refusals");
    let mut c = f.client();
    let install =
        |c: &mut Client, paths: Vec<String>| c.request(&Request::InstallPackage { paths }).unwrap();

    let mut arm32 = ApkSpec::new("org.example.arm");
    arm32.native_abis = vec!["armeabi-v7a".into()];
    let p = f.apk("arm.apk", &arm32, &key);
    assert_eq!(code(install(&mut c, vec![p])), ErrorCode::IncompatibleAbi);

    let junk = f.root.join("junk.apk");
    fs::write(&junk, b"junk").unwrap();
    assert_eq!(
        code(install(&mut c, vec![junk.display().to_string()])),
        ErrorCode::InvalidPackage
    );

    let mut cfg = SignConfig::new(key.clone());
    cfg.v2 = false;
    cfg.v3 = false;
    let unsigned = f.root.join("unsigned.apk");
    fs::write(&unsigned, ApkSpec::new("org.example.u").build(&cfg)).unwrap();
    assert_eq!(
        code(install(&mut c, vec![unsigned.display().to_string()])),
        ErrorCode::InvalidSignature
    );

    assert_eq!(
        code(install(&mut c, vec!["relative.apk".into()])),
        ErrorCode::Malformed
    );
    assert_eq!(code(install(&mut c, vec![])), ErrorCode::InvalidSplits);

    let base = ApkSpec::new("org.example.ok");
    let ok = f.apk("ok.apk", &base, &key);
    assert!(matches!(install(&mut c, vec![ok]), Response::Installed(_)));
    let other = TestKey::generate(KeyKind::P256, "other");
    let mut v2 = base.clone();
    v2.version_code = 2;
    let foreign = f.apk("foreign.apk", &v2, &other);
    assert_eq!(
        code(install(&mut c, vec![foreign])),
        ErrorCode::SignerMismatch
    );
    let mut old = base.clone();
    old.version_code = 0;
    let older = f.apk("older.apk", &old, &key);
    assert_eq!(code(install(&mut c, vec![older])), ErrorCode::Downgrade);
    // The connection stays usable after refusals.
    assert!(matches!(
        c.request(&Request::Ping).unwrap(),
        Response::Pong { .. }
    ));
}
