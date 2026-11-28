//! Binder in sysd: the broker (andruna-binder) and the guest processes
//! that use it, with the identity sysd assigns to each (ADR 0025).
//!
//! sysd knows every process it starts: [`Binder::start_native`] records
//! the child's pid with its uid before the child can open `/dev/binder`,
//! and the broker refuses processes it does not know. servicemanager
//! (frameworks/native, built by `aosp/tools/build_guest.py binder`) is
//! started this way at start-up when its binary exists (see
//! [`Binder::start_servicemanager_from_env`]); once it is the context
//! manager, processes sysd starts see `servicemanager.ready=true`, the
//! property libbinder waits for before it looks up services.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use andruna_binder::host::Server;
use andruna_binder::transport::PIPE_ENV;

/// The uid of `system` (servicemanager and other system processes).
pub const AID_SYSTEM: u32 = 1000;

/// How long the broker waits for sysd to record a child that connects
/// right after it was created.
const REGISTER_WAIT: Duration = Duration::from_secs(2);

/// How long sysd waits for servicemanager to become the context manager.
pub const SERVICEMANAGER_WAIT: Duration = Duration::from_secs(30);

/// The property libbinder waits for before using servicemanager.
pub const READY_PROPERTY: &str = "servicemanager.ready";

/// The binder broker of this sysd.
pub struct Binder {
    server: Arc<Server>,
    uids: Arc<Mutex<HashMap<u32, u32>>>,
    servicemanager_ready: AtomicBool,
}

/// Where servicemanager and its libraries are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceManagerConfig {
    /// andruna-elf.
    pub elf_runner: PathBuf,
    /// The servicemanager ELF file.
    pub exe: PathBuf,
    /// Its library directories.
    pub lib_dirs: Vec<PathBuf>,
}

impl ServiceManagerConfig {
    /// `ANDRUNA_SERVICEMANAGER` (the ELF file; libraries from
    /// `ANDRUNA_LIB_PATH`, `;`-separated) if set, else the system root
    /// next to the executable (`exe_dir`): `system\bin\servicemanager`
    /// with `system\lib64`, when that file exists (`ANDRUNA_SYSTEM_ROOT`
    /// names another system root). andruna-elf is `ANDRUNA_ELF`, else
    /// `andruna-elf.exe` in `exe_dir`. `None` when there is no
    /// servicemanager.
    pub fn locate(exe_dir: &Path) -> Option<Self> {
        let elf_runner = std::env::var_os("ANDRUNA_ELF")
            .map(PathBuf::from)
            .unwrap_or_else(|| exe_dir.join("andruna-elf.exe"));
        if let Some(sm) = std::env::var_os("ANDRUNA_SERVICEMANAGER") {
            let lib_dirs = std::env::var("ANDRUNA_LIB_PATH")
                .unwrap_or_default()
                .split(';')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
            return Some(Self {
                elf_runner,
                exe: sm.into(),
                lib_dirs,
            });
        }
        let system = std::env::var_os("ANDRUNA_SYSTEM_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| exe_dir.join("system"));
        Self::in_system_root(elf_runner, &system)
    }

    /// servicemanager in a system root (`bin`, `lib64`), as staged by
    /// `build_guest.py binder` in `out/guest/<arch>/system-root`.
    pub fn in_system_root(elf_runner: PathBuf, system: &Path) -> Option<Self> {
        let exe = system.join("bin").join("servicemanager");
        exe.is_file().then(|| Self {
            elf_runner,
            exe,
            lib_dirs: vec![system.join("lib64")],
        })
    }
}

impl std::fmt::Debug for Binder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binder")
            .field("pipe", &self.server.pipe_name())
            .finish()
    }
}

/// The pipe name of the broker of sysd instance `instance`.
pub fn pipe_name(instance: &str) -> String {
    format!(r"\\.\pipe\Andruna.binder.{}.{instance}", std::process::id())
}

impl Binder {
    /// Starts the broker on `pipe` (see [`pipe_name`]).
    pub fn start(pipe: &str) -> io::Result<Self> {
        let uids: Arc<Mutex<HashMap<u32, u32>>> = Arc::default();
        let u = uids.clone();
        let server = Server::start(
            pipe,
            Box::new(move |pid| {
                let start = Instant::now();
                loop {
                    if let Some(&uid) = u.lock().unwrap_or_else(|e| e.into_inner()).get(&pid) {
                        return Some(uid);
                    }
                    if start.elapsed() > REGISTER_WAIT {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }),
        )?;
        Ok(Self {
            server,
            uids,
            servicemanager_ready: AtomicBool::new(false),
        })
    }

    /// Whether servicemanager is the context manager (processes started
    /// from now on get `servicemanager.ready=true`).
    pub fn servicemanager_ready(&self) -> bool {
        self.servicemanager_ready.load(Ordering::Acquire)
    }

    pub fn pipe_name(&self) -> &str {
        self.server.pipe_name()
    }

    pub fn server(&self) -> &Arc<Server> {
        &self.server
    }

    /// Allows process `pid` to use binder as `uid`.
    pub fn allow(&self, pid: u32, uid: u32) {
        self.uids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(pid, uid);
    }

    /// Starts the native guest executable `exe` with `andruna-elf`
    /// (`elf_runner`), with binder as `uid`. `lib_dirs` are its library
    /// directories, `root` the Windows directory mounted as `/`.
    pub fn start_native(
        &self,
        elf_runner: &Path,
        exe: &Path,
        args: &[String],
        lib_dirs: &[PathBuf],
        root: &Path,
        uid: u32,
    ) -> io::Result<Child> {
        let child = self
            .native_command(elf_runner, exe, args, lib_dirs, root)
            .spawn()?;
        self.allow(child.id(), uid);
        Ok(child)
    }

    /// The command [`Binder::start_native`] runs, for callers that set up
    /// the standard streams themselves; they [`Binder::allow`] the child's
    /// pid right after spawning it.
    pub fn native_command(
        &self,
        elf_runner: &Path,
        exe: &Path,
        args: &[String],
        lib_dirs: &[PathBuf],
        root: &Path,
    ) -> Command {
        let mut cmd = Command::new(elf_runner);
        cmd.arg("run");
        for d in lib_dirs {
            cmd.arg("-L").arg(d);
        }
        if self.servicemanager_ready() {
            cmd.arg("--prop").arg(format!("{READY_PROPERTY}=true"));
        }
        cmd.arg("--root")
            .arg(root)
            .arg(exe)
            .args(args)
            .env(PIPE_ENV, self.pipe_name())
            .stdin(Stdio::null());
        cmd
    }

    /// Starts servicemanager as the `system` uid with guest root `root`
    /// and waits until it is the context manager (up to
    /// [`SERVICEMANAGER_WAIT`]); from then on processes this sysd starts
    /// see `servicemanager.ready=true`.
    pub fn start_servicemanager(
        &self,
        cfg: &ServiceManagerConfig,
        root: &Path,
    ) -> io::Result<Child> {
        std::fs::create_dir_all(root)?;
        let mut child = self.start_native(
            &cfg.elf_runner,
            &cfg.exe,
            &[],
            &cfg.lib_dirs,
            root,
            AID_SYSTEM,
        )?;
        let start = Instant::now();
        while !self.server.broker().has_context_manager() {
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "servicemanager exited before it became the context manager: {status}"
                )));
            }
            if start.elapsed() > SERVICEMANAGER_WAIT {
                let _ = child.kill();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "servicemanager did not become the context manager",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.servicemanager_ready.store(true, Ordering::Release);
        Ok(child)
    }

    /// Starts servicemanager when it is present
    /// ([`ServiceManagerConfig::locate`] next to this executable), with
    /// guest root `<root>\servicemanager-root`. `None` when there is none.
    pub fn start_servicemanager_from_env(&self, root: &Path) -> Option<io::Result<Child>> {
        let exe = std::env::current_exe().ok()?;
        let cfg = ServiceManagerConfig::locate(exe.parent()?)?;
        Some(self.start_servicemanager(&cfg, &root.join("servicemanager-root")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use andruna_binder::transport::{Msg, VERSION, op};

    /// A process sysd did not start is refused; one it allowed is
    /// accepted with its pid from the pipe.
    #[test]
    fn broker_accepts_only_known_processes() {
        let name = format!(r"\\.\pipe\Andruna.binder.test.{}", std::process::id());
        let b = Binder::start(&name).unwrap();
        let hello = Msg {
            op: op::HELLO as i32,
            a: VERSION,
            ..Msg::default()
        };
        let pipe = andruna_binder::sys::open_pipe(&andruna_binder::sys::wide(&name)).unwrap();
        let mut buf = [0u8; 64];
        // Not allowed: the broker closes the pipe after waiting.
        assert!(andruna_binder::sys::transact(&pipe, &hello.to_bytes(), &mut buf).is_err());
        b.allow(std::process::id(), 10_001);
        let pipe = andruna_binder::sys::open_pipe(&andruna_binder::sys::wide(&name)).unwrap();
        let n = andruna_binder::sys::transact(&pipe, &hello.to_bytes(), &mut buf).unwrap();
        let r = Msg::from_bytes(&buf[..n]).unwrap();
        assert_eq!(r.op, 0);
        assert_eq!(b.server().broker().stats().procs, 1);
    }
}
