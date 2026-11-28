//! Per-package AppContainer isolation
//! ([security](../../../docs/spec/04-security.md#isolation-one-appcontainer-per-package),
//! ADR 0007, spike S7).
//!
//! Every package runs in its own AppContainer. This module owns the
//! Windows side of that:
//!
//! - **Profiles.** [`install`] creates the package's AppContainer profile
//!   and prepares its directories; [`uninstall`] deletes the profile. The
//!   profile name is `Andruna.<hash>`, where the hash is SHA-256 of the
//!   package name and a per-install [`Salt`], so a reinstall gets a new
//!   identity and the name fits the 64-character limit.
//! - **Files.** The package's data directory (`data\<package>`) gets a
//!   protected DACL (the user, SYSTEM, the package: full access) and a low
//!   mandatory label, so the low-integrity container can write there and no
//!   other package can read it. Its APK directory (`app\<package>`) is
//!   read-only for the package. The runtime files (install directory: ART
//!   root, boot image, executables) are readable through one capability of
//!   our own, `andrunaRuntime` ([`grant_runtime`]); every Andruna container
//!   holds it, other AppContainers on the machine do not.
//! - **Capabilities** follow granted Android permissions
//!   ([`capabilities_for`]).
//! - **Launch.** [`launch`] starts a process in the container with
//!   `STARTUPINFOEX`: `SECURITY_CAPABILITIES`, an explicit handle list (the
//!   child inherits exactly the handles given), optionally a job it starts
//!   in and suspended (to map the boot image first). Every process sysd
//!   creates goes through [`launch`] or holds [`spawn_lock`], so the
//!   inheritable handle duplicates of one launch never leak into another
//!   child.
//! - **Identity.** [`process_sid`] and [`pid_sid`] read the AppContainer SID
//!   from a process token, for the Binder broker's `getCallingUid`.
//!
//! Hooks for the package manager: call [`Salt::generate`] once per install
//! and store it with the package, then [`install`] after the files are in
//! place; [`uninstall`] before removing the directories; [`sandbox`] to get
//! the identity of an installed package at launch.
//!
//! Every failure is reported as a [`SandboxError`] naming what failed;
//! nothing falls back to running outside the container.

mod win;

use std::fmt;
use std::io;
use std::os::windows::io::{AsHandle, BorrowedHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use win::Sid;

use crate::Layout;

/// The capability that lets a container read Andruna's runtime files.
pub const RUNTIME_CAPABILITY: &str = "andrunaRuntime";

/// `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`.
const READ_EXECUTE: u32 = 0x0012_00A9;

#[derive(Debug)]
pub enum SandboxError {
    InvalidPackage(String),
    InvalidSalt,
    /// Creating, deriving or deleting the profile failed.
    Profile {
        package: String,
        source: io::Error,
    },
    /// Setting the ACL or label of a path failed.
    Access {
        path: PathBuf,
        source: io::Error,
    },
    /// Creating the process failed.
    Launch {
        exe: PathBuf,
        source: io::Error,
    },
    /// Reading a process token failed.
    Identity(io::Error),
    /// Other system failure (hashing, random numbers, capability SIDs).
    System(io::Error),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPackage(p) => write!(f, "`{p}` is not a valid package name"),
            Self::InvalidSalt => write!(f, "invalid AppContainer salt"),
            Self::Profile { package, source } => {
                write!(f, "AppContainer profile of {package}: {source}")
            }
            Self::Access { path, source } => {
                write!(f, "cannot set access on {}: {source}", path.display())
            }
            Self::Launch { exe, source } => write!(
                f,
                "cannot start {} in its AppContainer: {source}",
                exe.display()
            ),
            Self::Identity(e) => write!(f, "cannot read the process token: {e}"),
            Self::System(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SandboxError {}

/// The per-install salt of a package's AppContainer identity: 16 random
/// bytes as 32 lowercase hex digits. Stored with the package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Salt(String);

impl Salt {
    pub fn generate() -> Result<Self, SandboxError> {
        let b = win::random(16).map_err(SandboxError::System)?;
        Ok(Self(hex(&b)))
    }

    pub fn parse(s: &str) -> Result<Self, SandboxError> {
        if s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(Self(s.to_string()))
        } else {
            Err(SandboxError::InvalidSalt)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The AppContainer profile name of `package` with `salt`:
/// `Andruna.` and the first 128 bits of SHA-256(package, NUL, salt) in hex.
pub fn profile_name(package: &str, salt: &Salt) -> Result<String, SandboxError> {
    if !andruna_ipc::is_valid_package_name(package) {
        return Err(SandboxError::InvalidPackage(package.to_string()));
    }
    let mut input = package.as_bytes().to_vec();
    input.push(0);
    input.extend_from_slice(salt.0.as_bytes());
    let h = win::sha256(&input).map_err(SandboxError::System)?;
    Ok(format!("Andruna.{}", hex(&h[..16])))
}

/// A package's AppContainer identity.
#[derive(Clone, Debug)]
pub struct Sandbox {
    pub package: String,
    /// The profile name ([`profile_name`]).
    pub profile: String,
    /// The AppContainer SID (`S-1-15-2-...`).
    pub sid: Sid,
}

/// The identity of an installed package (derived; the profile is not
/// touched).
pub fn sandbox(package: &str, salt: &Salt) -> Result<Sandbox, SandboxError> {
    let profile = profile_name(package, salt)?;
    let sid = win::derive_profile_sid(&profile).map_err(|e| SandboxError::Profile {
        package: package.to_string(),
        source: e,
    })?;
    Ok(Sandbox {
        package: package.to_string(),
        profile,
        sid,
    })
}

/// Install-time hook: creates the package's AppContainer profile (an
/// existing one is reused) and prepares its directories
/// ([`prepare_dirs`]).
pub fn install(layout: &Layout, package: &str, salt: &Salt) -> Result<Sandbox, SandboxError> {
    let profile = profile_name(package, salt)?;
    let (sid, created) = win::create_profile(
        &profile,
        &format!("Andruna: {package}"),
        &format!("Android package {package} run by Andruna"),
    )
    .map_err(|e| SandboxError::Profile {
        package: package.to_string(),
        source: e,
    })?;
    crate::info!(
        "appcontainer_profile",
        package = package,
        profile = profile,
        sid = sid.to_string_sid(),
        created = created
    );
    let sb = Sandbox {
        package: package.to_string(),
        profile,
        sid,
    };
    prepare_dirs(layout, &sb)?;
    Ok(sb)
}

/// Uninstall hook: deletes the profile (a missing one is fine). The
/// package's directories are removed by the caller.
pub fn uninstall(package: &str, salt: &Salt) -> Result<(), SandboxError> {
    let profile = profile_name(package, salt)?;
    win::delete_profile(&profile).map_err(|e| SandboxError::Profile {
        package: package.to_string(),
        source: e,
    })?;
    crate::info!("appcontainer_deleted", package = package, profile = profile);
    Ok(())
}

/// `data\<package>`: the package's private data (writable by it).
pub fn data_dir(layout: &Layout, package: &str) -> PathBuf {
    layout.root.join("data").join(package)
}

/// `app\<package>`: its APKs and compiled code (readable by it).
pub fn app_dir(layout: &Layout, package: &str) -> PathBuf {
    layout.app_dir().join(package)
}

fn ensure_dir(path: &Path, sddl: &str) -> Result<(), SandboxError> {
    let err = |e| SandboxError::Access {
        path: path.to_path_buf(),
        source: e,
    };
    if path.is_dir() {
        win::set_descriptor(path, sddl).map_err(err)
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(err)?;
        }
        match win::create_dir_with(path, sddl) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                win::set_descriptor(path, sddl).map_err(err)
            }
            r => r.map_err(err),
        }
    }
}

/// Security descriptor of the data directory.
pub fn data_dir_sddl(user: &Sid, package: &Sid) -> String {
    format!(
        "D:P(A;OICI;FA;;;{u})(A;OICI;FA;;;SY)(A;OICI;FA;;;{p})S:(ML;OICI;NW;;;LW)",
        u = user.to_string_sid(),
        p = package.to_string_sid()
    )
}

/// Security descriptor of the APK directory.
pub fn app_dir_sddl(user: &Sid, package: &Sid) -> String {
    format!(
        "D:P(A;OICI;FA;;;{u})(A;OICI;FA;;;SY)(A;OICI;{READ_EXECUTE:#x};;;{p})",
        u = user.to_string_sid(),
        p = package.to_string_sid()
    )
}

/// Creates (or re-secures) the package's data and APK directories. Applied
/// to existing directories, the new entries propagate to their contents.
pub fn prepare_dirs(layout: &Layout, sb: &Sandbox) -> Result<(), SandboxError> {
    let user = win::current_user_sid().map_err(SandboxError::Identity)?;
    ensure_dir(
        &data_dir(layout, &sb.package),
        &data_dir_sddl(&user, &sb.sid),
    )?;
    ensure_dir(&app_dir(layout, &sb.package), &app_dir_sddl(&user, &sb.sid))
}

/// Lets every Andruna container read and execute `dir` and its contents
/// (the install directory with the ART root and the boot image). Called by
/// the installer or at sysd start; granting again is harmless.
pub fn grant_runtime(dir: &Path) -> Result<(), SandboxError> {
    let cap = Capability::Runtime.sid()?;
    win::add_ace(dir, &cap, READ_EXECUTE).map_err(|e| SandboxError::Access {
        path: dir.to_path_buf(),
        source: e,
    })
}

/// A capability in the container's token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    /// Read Andruna's runtime files; every container has it.
    Runtime,
    /// Outgoing internet connections (`INTERNET`).
    InternetClient,
    /// Local network, both directions (Android local network access).
    PrivateNetworkClientServer,
}

impl Capability {
    pub fn name(self) -> &'static str {
        match self {
            Self::Runtime => RUNTIME_CAPABILITY,
            Self::InternetClient => "internetClient",
            Self::PrivateNetworkClientServer => "privateNetworkClientServer",
        }
    }

    pub fn sid(self) -> Result<Sid, SandboxError> {
        win::capability_sid(self.name()).map_err(SandboxError::System)
    }
}

/// The capabilities for a package's granted Android permissions
/// ([security](../../../docs/spec/04-security.md#isolation-one-appcontainer-per-package)).
/// Camera, microphone, location and storage are brokered by sysd and add
/// no capability.
pub fn capabilities_for<S: AsRef<str>>(granted: &[S]) -> Vec<Capability> {
    let mut caps = vec![Capability::Runtime];
    for p in granted {
        let c = match p.as_ref() {
            "android.permission.INTERNET" => Capability::InternetClient,
            "android.permission.ACCESS_LOCAL_NETWORK" => Capability::PrivateNetworkClientServer,
            _ => continue,
        };
        if !caps.contains(&c) {
            caps.push(c);
        }
    }
    caps
}

/// The environment of a container process: only what Windows itself
/// needs, with the temporary directory inside the package's data. No
/// secrets and nothing of sysd's own environment.
pub fn container_environment(temp: &Path) -> Vec<String> {
    let mut env = Vec::new();
    for name in [
        "SystemRoot",
        "windir",
        "SystemDrive",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
        // CreateProcess derives the container's own folders from these
        // and fails with ERROR_ENVVAR_NOT_FOUND without them.
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
    ] {
        if let Some(v) = std::env::var_os(name) {
            env.push(format!("{name}={}", v.to_string_lossy()));
        }
    }
    if let Some(root) = std::env::var_os("SystemRoot") {
        env.push(format!("PATH={}\\System32", root.to_string_lossy()));
    }
    env.push(format!("TEMP={}", temp.display()));
    env.push(format!("TMP={}", temp.display()));
    env.sort_by_key(|e| e.to_ascii_uppercase());
    env
}

/// Quotes one argument for the Windows command line (the rules of
/// `CommandLineToArgvW` and the C runtime).
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// What to start in a package's container.
pub struct Launch<'a> {
    pub sandbox: &'a Sandbox,
    pub capabilities: &'a [Capability],
    pub exe: &'a Path,
    /// Arguments after the program name.
    pub args: &'a [String],
    /// stdin, stdout, stderr (`None`: no handle).
    pub stdio: [Option<BorrowedHandle<'a>>; 3],
    /// Further handles the process inherits (for example its sysd pipe);
    /// their values in the child are in [`ContainedProcess::inherited`].
    pub inherit: &'a [BorrowedHandle<'a>],
    /// A job the process is created in, so it never runs outside it.
    pub job: Option<BorrowedHandle<'a>>,
    /// The environment block; `None` inherits sysd's. Use
    /// [`container_environment`].
    pub environment: Option<&'a [String]>,
    /// A variable added to `environment` that tells the process the values
    /// of its [`Launch::inherit`] handles (`;`-separated, in order).
    pub handle_env: Option<&'a str>,
    pub current_dir: Option<&'a Path>,
    /// Created suspended: map the boot image, then [`ContainedProcess::resume`].
    pub suspended: bool,
}

/// A process running in a package's AppContainer.
#[derive(Debug)]
pub struct ContainedProcess {
    pub pid: u32,
    process: OwnedHandle,
    thread: OwnedHandle,
    sid: Sid,
    inherited: Vec<usize>,
}

impl ContainedProcess {
    pub fn process(&self) -> BorrowedHandle<'_> {
        self.process.as_handle()
    }

    /// The AppContainer SID it was started with.
    pub fn app_container_sid(&self) -> &Sid {
        &self.sid
    }

    /// The child's handle values of [`Launch::inherit`], in order.
    pub fn inherited(&self) -> &[usize] {
        &self.inherited
    }

    pub fn resume(&self) -> io::Result<()> {
        win::resume(&self.thread)
    }

    /// Waits for the exit code; `None` on timeout.
    pub fn wait(&self, timeout: Duration) -> io::Result<Option<u32>> {
        let ms = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
        win::wait_process(&self.process, ms)
    }

    pub fn kill(&self) {
        win::terminate(&self.process, 1);
    }
}

/// Held while creating any process from sysd that inherits handles (see
/// the module documentation); [`launch`] takes it itself.
pub fn spawn_lock() -> std::sync::MutexGuard<'static, ()> {
    win::SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts `l.exe` in the package's AppContainer with the given
/// capabilities and handles.
pub fn launch(l: &Launch<'_>) -> Result<ContainedProcess, SandboxError> {
    let caps = l
        .capabilities
        .iter()
        .map(|c| c.sid())
        .collect::<Result<Vec<_>, _>>()?;
    let mut cmd = quote_arg(&l.exe.display().to_string());
    for a in l.args {
        cmd.push(' ');
        cmd.push_str(&quote_arg(a));
    }
    let s = win::spawn(&win::Spawn {
        application: l.exe,
        command_line: &cmd,
        app_container: &l.sandbox.sid,
        capabilities: &caps,
        stdio: l.stdio,
        inherit: l.inherit,
        job: l.job,
        environment: l.environment,
        handle_env: l.handle_env,
        current_dir: l.current_dir,
        suspended: l.suspended,
    })
    .map_err(|e| {
        crate::error!(
            "appcontainer_launch_failed",
            package = l.sandbox.package,
            exe = l.exe.display(),
            error = e
        );
        SandboxError::Launch {
            exe: l.exe.to_path_buf(),
            source: e,
        }
    })?;
    crate::info!(
        "appcontainer_launch",
        package = l.sandbox.package,
        pid = s.pid,
        sid = l.sandbox.sid.to_string_sid(),
        capabilities = l
            .capabilities
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>()
            .join(","),
        suspended = l.suspended
    );
    Ok(ContainedProcess {
        pid: s.pid,
        process: s.process,
        thread: s.thread,
        sid: l.sandbox.sid.clone(),
        inherited: s.inherited,
    })
}

/// The AppContainer SID of a process, read from its token (`None`: not in
/// an AppContainer). For identifying Binder peers.
pub fn process_sid(process: BorrowedHandle<'_>) -> Result<Option<Sid>, SandboxError> {
    win::process_app_container_sid(process).map_err(SandboxError::Identity)
}

/// [`process_sid`] of process `pid`.
pub fn pid_sid(pid: u32) -> Result<Option<Sid>, SandboxError> {
    let h = win::open_process_for_query(pid).map_err(SandboxError::Identity)?;
    process_sid(h.as_handle())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn salts_and_profile_names() {
        let s = Salt::generate().unwrap();
        assert_eq!(Salt::parse(s.as_str()).unwrap(), s);
        assert!(Salt::parse("xyz").is_err());
        assert!(Salt::parse(&"A".repeat(32)).is_err());
        let a = profile_name("com.example.app", &s).unwrap();
        assert!(a.starts_with("Andruna.") && a.len() == 40, "{a}");
        assert_eq!(a, profile_name("com.example.app", &s).unwrap());
        assert_ne!(a, profile_name("com.example.other", &s).unwrap());
        let t = Salt::generate().unwrap();
        assert_ne!(a, profile_name("com.example.app", &t).unwrap());
        assert!(matches!(
            profile_name("not valid", &s),
            Err(SandboxError::InvalidPackage(_))
        ));
    }

    #[test]
    fn profile_name_is_stable() {
        // The name is persistent state: it must not change between
        // versions for the same package and salt.
        let s = Salt::parse("00112233445566778899aabbccddeeff").unwrap();
        let n = profile_name("com.example.app", &s).unwrap();
        let again = profile_name("com.example.app", &s).unwrap();
        assert_eq!(n, again);
        let mut input = b"com.example.app\0".to_vec();
        input.extend_from_slice(s.as_str().as_bytes());
        assert_eq!(
            n,
            format!("Andruna.{}", hex(&win::sha256(&input).unwrap()[..16]))
        );
    }

    #[test]
    fn sha256_known_answer() {
        assert_eq!(
            hex(&win::sha256(b"abc").unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn capabilities_follow_permissions() {
        assert_eq!(capabilities_for::<&str>(&[]), vec![Capability::Runtime]);
        assert_eq!(
            capabilities_for(&[
                "android.permission.INTERNET",
                "android.permission.CAMERA",
                "android.permission.INTERNET",
                "android.permission.ACCESS_LOCAL_NETWORK",
            ]),
            vec![
                Capability::Runtime,
                Capability::InternetClient,
                Capability::PrivateNetworkClientServer
            ]
        );
        assert_eq!(
            Capability::InternetClient.sid().unwrap().to_string_sid(),
            "S-1-15-3-1"
        );
        assert!(
            Capability::Runtime
                .sid()
                .unwrap()
                .to_string_sid()
                .starts_with("S-1-15-3-1024-")
        );
    }

    #[test]
    fn quoting() {
        assert_eq!(quote_arg("abc"), "abc");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg(r"C:\a b\c"), r#""C:\a b\c""#);
        assert_eq!(quote_arg(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
    }

    #[test]
    fn this_process_is_not_in_a_container() {
        let me = win::open_process_for_query(std::process::id()).unwrap();
        assert!(process_sid(me.as_handle()).unwrap().is_none());
    }
}
