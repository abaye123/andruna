//! Golden payloads of protocol version 1.
//!
//! `tests/golden/v1.txt` lists one message per line: a name, the request id
//! and the payload (header and body, without the length prefix) in hex. This
//! test checks that the encoder produces exactly those bytes and that the
//! decoder reads them back. Clients written in other languages (the manager
//! in `ui/`) test their codec against the same file, so the two sides cannot
//! drift apart. `ANDRUNA_BLESS=1 cargo test -p andruna-ipc --test golden`
//! rewrites the file after an intended change.

use andruna_ipc::{
    CompileStatus, ErrorCode, InstallOutcome, PackageDetails, PackageInfo, Request, Response,
    Status,
};

fn requests() -> Vec<(&'static str, u32, Request)> {
    vec![
        ("request.ping", 1, Request::Ping),
        ("request.status", 2, Request::Status),
        (
            "request.start_package",
            0x0102_0304,
            Request::StartPackage {
                package: "com.example.app".into(),
            },
        ),
        (
            "request.focus_window",
            7,
            Request::FocusWindow {
                package: "org.example.x_1".into(),
            },
        ),
        ("request.list_packages", 9, Request::ListPackages),
    ]
}

fn responses() -> Vec<(&'static str, u32, Response)> {
    vec![
        ("response.pong", 1, Response::Pong { protocol: 1 }),
        (
            "response.status",
            2,
            Response::Status(Status {
                pid: 1234,
                uptime_ms: 90_061_000,
                connections: 2,
                hosts: 1,
                idle_timeout_ms: 30_000,
                version: "0.1.0".into(),
            }),
        ),
        (
            "response.started",
            3,
            Response::Started {
                pid: 77,
                already_running: true,
            },
        ),
        ("response.focused", 4, Response::Focused),
        (
            "response.packages_empty",
            5,
            Response::Packages { packages: vec![] },
        ),
        (
            "response.packages",
            6,
            Response::Packages {
                packages: vec!["a.b".into(), "com.example.\u{5d0}".into()],
            },
        ),
        (
            "response.error_not_installed",
            7,
            Response::error(
                ErrorCode::NotInstalled,
                "package `org.example.none` is not installed",
            ),
        ),
        (
            "response.error_unknown_code",
            8,
            Response::error(ErrorCode::Other(999), ""),
        ),
    ]
}

/// The package manager messages (kinds 0x0006 to 0x000A, 0x8006 to 0x8009,
/// error codes 11 to 18). They are in their own file, `v1-pkg.txt`, so a
/// client that covers `v1.txt` exactly keeps passing until it adds them.
fn pkg_requests() -> Vec<(&'static str, u32, Request)> {
    vec![
        (
            "request.inspect_package",
            10,
            Request::InspectPackage {
                paths: vec![
                    "C:\\Users\\u\\Downloads\\base.apk".into(),
                    "C:\\Users\\u\\Downloads\\split_config.he.apk".into(),
                ],
                locale: "he-IL".into(),
            },
        ),
        (
            "request.install_package",
            11,
            Request::InstallPackage {
                paths: vec!["C:\\x\\app.apk".into()],
            },
        ),
        (
            "request.uninstall_package",
            12,
            Request::UninstallPackage {
                package: "com.example.app".into(),
                keep_data: true,
            },
        ),
        (
            "request.list_package_info",
            13,
            Request::ListPackageInfo {
                locale: "en-US".into(),
            },
        ),
        (
            "request.start_activity",
            14,
            Request::StartActivity {
                package: "com.example.app".into(),
                activity: "com.example.app.Second".into(),
            },
        ),
    ]
}

fn pkg_responses() -> Vec<(&'static str, u32, Response)> {
    vec![
        (
            "response.package_details",
            10,
            Response::PackageDetails(PackageDetails {
                package: "com.example.app".into(),
                version_code: 0x1_0000_0007,
                version_name: "1.0".into(),
                label: "\u{05d3}\u{05d5}\u{05d2}\u{05de}\u{05d4}".into(),
                min_sdk: 21,
                target_sdk: 34,
                signer_sha256: "ab".repeat(32),
                signature_scheme: 3,
                permissions: vec!["android.permission.INTERNET".into()],
                native_abis: vec!["arm64-v8a".into(), "x86_64".into()],
                selected_abi: "x86_64".into(),
                installed_version_code: 0,
                icon_png: "C:\\a\\icon.png".into(),
            }),
        ),
        (
            "response.installed",
            11,
            Response::Installed(InstallOutcome {
                package: "com.example.app".into(),
                version_code: 7,
                version_name: "1.0".into(),
                updated: false,
                compile: CompileStatus::Unavailable,
                compile_message: "andruna-app.exe not found".into(),
                shortcuts: 1,
            }),
        ),
        ("response.uninstalled", 12, Response::Uninstalled),
        (
            "response.package_infos",
            13,
            Response::PackageInfos {
                packages: vec![PackageInfo {
                    package: "com.example.app".into(),
                    label: "Example".into(),
                    version_code: 7,
                    version_name: "1.0".into(),
                    icon_ico: "C:\\a\\icon.ico".into(),
                    icon_png: "C:\\a\\icon.png".into(),
                    launcher_activities: vec!["com.example.app.Main".into()],
                    installed_ms: 1_795_000_000_000,
                    size_bytes: 123_456,
                    app_user_model_id: "Andruna.com.example.app".into(),
                }],
            },
        ),
        (
            "response.error_invalid_signature",
            14,
            Response::error(
                ErrorCode::InvalidSignature,
                "APK Signature Scheme v2 digest of chunked contents does not match",
            ),
        ),
    ]
}

fn render_pkg() -> String {
    let mut out = String::from(
        "# Golden payloads of the sysd pipe protocol, version 1: package manager messages.\n\
         # <name> <request id> <payload hex: header and body, no length prefix>\n\
         # Generated by crates/andruna-ipc/tests/golden.rs; do not edit by hand.\n",
    );
    for (name, id, req) in pkg_requests() {
        out.push_str(&format!("{name} {id} {}\n", hex(&req.encode(id))));
    }
    for (name, id, resp) in pkg_responses() {
        out.push_str(&format!("{name} {id} {}\n", hex(&resp.encode(id))));
    }
    out
}

#[test]
fn golden_package_payloads() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/v1-pkg.txt");
    let expected = render_pkg();
    if std::env::var_os("ANDRUNA_BLESS").is_some() {
        std::fs::write(&path, &expected).unwrap();
    }
    let actual = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\r\n", "\n");
    assert_eq!(actual, expected, "{} is out of date", path.display());
    for (_, id, req) in pkg_requests() {
        assert_eq!(Request::decode(&req.encode(id)), Ok((id, req)));
    }
    for (_, id, resp) in pkg_responses() {
        assert_eq!(Response::decode(&resp.encode(id)), Ok((id, resp)));
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn render() -> String {
    let mut out = String::from(
        "# Golden payloads of the sysd pipe protocol, version 1.\n\
         # <name> <request id> <payload hex: header and body, no length prefix>\n\
         # Generated by crates/andruna-ipc/tests/golden.rs; do not edit by hand.\n",
    );
    for (name, id, req) in requests() {
        out.push_str(&format!("{name} {id} {}\n", hex(&req.encode(id))));
    }
    for (name, id, resp) in responses() {
        out.push_str(&format!("{name} {id} {}\n", hex(&resp.encode(id))));
    }
    out
}

#[test]
fn golden_payloads() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/v1.txt");
    let expected = render();
    if std::env::var_os("ANDRUNA_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &expected).unwrap();
    }
    let actual = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\r\n", "\n");
    assert_eq!(actual, expected, "{} is out of date", path.display());

    for (_, id, req) in requests() {
        assert_eq!(Request::decode(&req.encode(id)), Ok((id, req)));
    }
    for (_, id, resp) in responses() {
        assert_eq!(Response::decode(&resp.encode(id)), Ok((id, resp)));
    }
}
