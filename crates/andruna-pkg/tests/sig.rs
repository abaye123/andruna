//! Signature verification against APKs signed at test time with freshly
//! generated keys (RSA 2048 and P-256), and, when the Android SDK's
//! apksigner is installed, cross-checks in both directions.

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use andruna_pkg::Code;
use andruna_pkg::sig::{self, Scheme, SigningDetails, VerifyOptions};
use andruna_pkg::testkit::sign::{KeyKind, SignConfig, TestKey, sign};
use andruna_pkg::testkit::zipw::ZipEntry;
use andruna_pkg::zip::Archive;

fn rsa() -> &'static TestKey {
    static K: OnceLock<TestKey> = OnceLock::new();
    K.get_or_init(|| TestKey::generate(KeyKind::Rsa2048, "Andruna test RSA"))
}

fn ec() -> &'static TestKey {
    static K: OnceLock<TestKey> = OnceLock::new();
    K.get_or_init(|| TestKey::generate(KeyKind::P256, "Andruna test EC"))
}

fn entries() -> Vec<ZipEntry> {
    vec![
        ZipEntry::stored("AndroidManifest.xml", b"<manifest/>".to_vec()).align(4),
        ZipEntry::deflated(
            "classes.dex",
            (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect(),
        ),
        ZipEntry::stored("res/raw/a.bin", vec![3u8; 3000]).align(4),
    ]
}

fn verify(apk: &[u8]) -> andruna_pkg::Result<SigningDetails> {
    let a = Archive::parse(apk)?;
    sig::verify(&a, VerifyOptions::default())
}

fn cfg(key: &TestKey) -> SignConfig {
    SignConfig::new(key.clone())
}

#[test]
fn accepts_each_scheme_and_key_type() {
    for key in [rsa(), ec()] {
        let mut c = cfg(key);
        c.v1 = true;
        let d = verify(&sign(&entries(), &c)).unwrap();
        assert_eq!(d.scheme, Scheme::V3);
        assert_eq!(d.signers, vec![key.cert_sha256()]);

        c.v3 = false;
        assert_eq!(verify(&sign(&entries(), &c)).unwrap().scheme, Scheme::V2);

        c.v2 = false;
        c.v1_names_v2 = false;
        let d = verify(&sign(&entries(), &c)).unwrap();
        assert_eq!(d.scheme, Scheme::V1);
        assert_eq!(d.signers, vec![key.cert_sha256()]);

        let mut c = cfg(key);
        c.v31 = true;
        assert_eq!(verify(&sign(&entries(), &c)).unwrap().scheme, Scheme::V31);
    }
}

#[test]
fn rejects_unsigned() {
    let mut c = cfg(rsa());
    c.v2 = false;
    c.v3 = false;
    let e = verify(&sign(&entries(), &c)).unwrap_err();
    assert_eq!(e.code, Code::NoSignature);
}

#[test]
fn rejects_tampered_content_in_every_section() {
    for v1 in [false, true] {
        let mut c = cfg(ec());
        c.v1 = v1;
        let apk = sign(&entries(), &c);
        let a = Archive::parse(&apk).unwrap();
        let dex = a.find("classes.dex").unwrap().clone();
        let raw = a.find("res/raw/a.bin").unwrap().clone();
        let cd = a.cd_offset() as usize;
        drop(a);
        // A byte in stored data, a byte in deflated data (fails the CRC
        // or the digest), a central directory byte (an entry's time).
        for pos in [
            raw.data_offset as usize + 10,
            dex.data_offset as usize + 100,
            cd + 12,
        ] {
            let mut bad = apk.clone();
            bad[pos] ^= 1;
            let e = verify(&bad).unwrap_err();
            assert!(
                matches!(e.code, Code::InvalidSignature | Code::InvalidArchive),
                "pos {pos}: {e}"
            );
        }
    }
}

#[test]
fn rejects_tampered_signature_and_wrong_key() {
    let apk = sign(&entries(), &cfg(rsa()));
    let a = Archive::parse(&apk).unwrap();
    let cd = a.cd_offset() as usize;
    drop(a);
    // Flip bytes across the signing block: every change must be caught
    // (signature, signed data, certificate, public key, lengths).
    let start = apk.len() - (apk.len() - cd) - 2000;
    let mut caught = 0;
    for pos in (start..cd - 24).step_by(97) {
        let mut bad = apk.clone();
        bad[pos] ^= 0x10;
        if verify(&bad).is_err() {
            caught += 1;
        } else {
            panic!("a flipped byte at {pos} in the signing block was not detected");
        }
    }
    assert!(caught > 10);
}

#[test]
fn stripping_protection() {
    // v1 says the APK had a v2 signature, but the block is gone.
    let mut c = cfg(rsa());
    c.v1 = true;
    c.strip_block = true;
    let e = verify(&sign(&entries(), &c)).unwrap_err();
    assert_eq!(e.code, Code::InvalidSignature);
    assert!(e.message.contains("missing"), "{e}");
}

#[test]
fn rotation_lineage_and_updates() {
    let old = ec().clone();
    let new = rsa().clone();
    let mut c = cfg(&new);
    c.rotated_from = vec![(old.clone(), 1)];
    let rotated = verify(&sign(&entries(), &c)).unwrap();
    assert_eq!(rotated.signers, vec![new.cert_sha256()]);
    assert_eq!(rotated.lineage.len(), 2);
    assert_eq!(rotated.lineage[0].cert_sha256, old.cert_sha256());

    let installed_old = verify(&sign(&entries(), &cfg(&old))).unwrap();
    let installed_new = verify(&sign(&entries(), &cfg(&new))).unwrap();
    assert!(
        rotated.can_update(&installed_old),
        "rotation from the installed key"
    );
    assert!(rotated.can_update(&installed_new));
    assert!(
        !installed_new.can_update(&installed_old),
        "different key, no lineage"
    );
    assert!(!installed_old.can_update(&rotated), "no rollback");

    // A v3 signer that differs from the v2 signer without lineage.
    let mut odd = cfg(&new);
    odd.legacy_override = Some(old.clone());
    let e = verify(&sign(&entries(), &odd)).unwrap_err();
    assert_eq!(e.code, Code::InvalidSignature);
    assert!(e.message.contains("lineage"), "{e}");

    // Without the installed-data capability the old signer cannot be
    // replaced.
    c.rotated_from = vec![(old.clone(), 0)];
    let no_cap = verify(&sign(&entries(), &c)).unwrap();
    assert!(!no_cap.can_update(&installed_old));
}

#[test]
fn v3_signer_outside_the_platform_range_falls_back_to_v2() {
    let mut c = cfg(ec());
    c.v3_sdk = (40, 50);
    let d = verify(&sign(&entries(), &c)).unwrap();
    assert_eq!(d.scheme, Scheme::V2);
}

// ---- apksigner cross-checks (skipped without the Android SDK) ----

fn apksigner() -> Option<PathBuf> {
    let sdk = std::env::var_os("ANDROID_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("Android\\Sdk"))
        })?;
    let mut tools: Vec<PathBuf> = std::fs::read_dir(sdk.join("build-tools"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    tools.sort();
    let p = tools.last()?.join("apksigner.bat");
    p.is_file().then_some(p)
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("andruna-pkg-sig-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

#[test]
fn apksigner_accepts_what_the_testkit_signs() {
    let Some(tool) = apksigner() else {
        eprintln!("skipped: apksigner not found");
        return;
    };
    for (i, key) in [rsa(), ec()].into_iter().enumerate() {
        let mut c = cfg(key);
        c.v1 = true;
        c.rotated_from = if i == 0 {
            vec![(ec().clone(), 1)]
        } else {
            vec![]
        };
        let path = scratch(&format!("ours{i}.apk"));
        std::fs::write(&path, sign(&entries(), &c)).unwrap();
        let out = Command::new("cmd")
            .arg("/c")
            .arg(&tool)
            .args(["verify", "--verbose", "--min-sdk-version", "21"])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "apksigner rejected our APK: {text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains("Verified using v3 scheme (APK Signature Scheme v3): true"),
            "{text}"
        );
    }
}

#[test]
fn verifies_what_apksigner_signs() {
    let Some(tool) = apksigner() else {
        eprintln!("skipped: apksigner not found");
        return;
    };
    let keytool = std::env::var_os("JAVA_HOME")
        .map(|j| PathBuf::from(j).join("bin\\keytool.exe"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("keytool"));
    for (alg, extra) in [
        ("RSA", &["-keysize", "2048"][..]),
        ("EC", &["-groupname", "secp256r1"][..]),
    ] {
        let ks = scratch(&format!("k{alg}.p12"));
        let _ = std::fs::remove_file(&ks);
        let st = Command::new(&keytool)
            .args(["-genkeypair", "-keystore"])
            .arg(&ks)
            .args([
                "-storetype",
                "PKCS12",
                "-storepass",
                "testpass",
                "-alias",
                "k",
                "-keyalg",
                alg,
            ])
            .args(extra)
            .args(["-dname", "CN=apksigner test", "-validity", "1000"])
            .output()
            .unwrap();
        assert!(
            st.status.success(),
            "{}",
            String::from_utf8_lossy(&st.stderr)
        );
        let mut c = cfg(rsa());
        c.v2 = false;
        c.v3 = false;
        let unsigned = scratch(&format!("in{alg}.apk"));
        std::fs::write(&unsigned, sign(&entries(), &c)).unwrap();
        let oldest = if alg == "EC" { "18" } else { "16" };
        for (min_sdk, want) in [(oldest, Scheme::V3), ("24", Scheme::V3)] {
            let signed = scratch(&format!("out{alg}{min_sdk}.apk"));
            let st = Command::new("cmd")
                .arg("/c")
                .arg(&tool)
                .args(["sign", "--ks"])
                .arg(&ks)
                .args([
                    "--ks-pass",
                    "pass:testpass",
                    "--min-sdk-version",
                    min_sdk,
                    "--out",
                ])
                .arg(&signed)
                .arg(&unsigned)
                .output()
                .unwrap();
            assert!(
                st.status.success(),
                "{}",
                String::from_utf8_lossy(&st.stderr)
            );
            let bytes = std::fs::read(&signed).unwrap();
            let d = verify(&bytes).unwrap_or_else(|e| panic!("{alg} min {min_sdk}: {e}"));
            assert_eq!(d.scheme, want);
            // v1 only, as an old toolchain would sign: strip v2/v3 with
            // apksigner's own flags.
            let v1_only = scratch(&format!("v1{alg}{min_sdk}.apk"));
            let st = Command::new("cmd")
                .arg("/c")
                .arg(&tool)
                .args(["sign", "--ks"])
                .arg(&ks)
                .args([
                    "--ks-pass",
                    "pass:testpass",
                    "--min-sdk-version",
                    min_sdk,
                    "--v2-signing-enabled",
                    "false",
                    "--v3-signing-enabled",
                    "false",
                    "--v1-signing-enabled",
                    "true",
                    "--out",
                ])
                .arg(&v1_only)
                .arg(&unsigned)
                .output()
                .unwrap();
            assert!(
                st.status.success(),
                "{}",
                String::from_utf8_lossy(&st.stderr)
            );
            let d = verify(&std::fs::read(&v1_only).unwrap())
                .unwrap_or_else(|e| panic!("v1 {alg}: {e}"));
            assert_eq!(d.scheme, Scheme::V1);
            // Tampering with a signed APK from apksigner is caught.
            let mut bad = bytes.clone();
            let a = Archive::parse(&bytes).unwrap();
            let e = a.find("res/raw/a.bin").unwrap().data_offset as usize;
            drop(a);
            bad[e] ^= 1;
            assert!(verify(&bad).is_err());
        }
    }
}
