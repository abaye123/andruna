//! Binder in sysd: the broker (andruna-binder) and the guest processes
//! that use it, with the identity sysd assigns to each (ADR 0025).
//!
//! sysd knows every process it starts: [`Binder::start_native`] records
//! the child's pid with its uid before the child can open `/dev/binder`,
//! and the broker refuses processes it does not know. servicemanager is
//! started this way when `ANDRUNA_SERVICEMANAGER` names its ELF file (see
//! [`Binder::start_servicemanager_from_env`]).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use andruna_binder::host::Server;
use andruna_binder::transport::PIPE_ENV;

/// The uid of `system` (servicemanager and other system processes).
pub const AID_SYSTEM: u32 = 1000;

/// How long the broker waits for sysd to record a child that connects
/// right after it was created.
const REGISTER_WAIT: Duration = Duration::from_secs(2);

/// The binder broker of this sysd.
pub struct Binder {
    server: Arc<Server>,
    uids: Arc<Mutex<HashMap<u32, u32>>>,
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
        Ok(Self { server, uids })
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
        let mut cmd = Command::new(elf_runner);
        cmd.arg("run");
        for d in lib_dirs {
            cmd.arg("-L").arg(d);
        }
        cmd.arg("--root")
            .arg(root)
            .arg(exe)
            .args(args)
            .env(PIPE_ENV, self.pipe_name())
            .stdin(Stdio::null());
        let child = cmd.spawn()?;
        self.allow(child.id(), uid);
        Ok(child)
    }

    /// Starts servicemanager when `ANDRUNA_SERVICEMANAGER` names its ELF
    /// file: `ANDRUNA_ELF` is andruna-elf (default: next to sysd),
    /// `ANDRUNA_LIB_PATH` its library directories (`;`-separated), and the
    /// guest root is `<root>\servicemanager-root`. `None` when it is not
    /// configured.
    pub fn start_servicemanager_from_env(&self, root: &Path) -> Option<io::Result<Child>> {
        let sm = std::env::var_os("ANDRUNA_SERVICEMANAGER")?;
        let runner = std::env::var_os("ANDRUNA_ELF")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .map(|e| e.with_file_name("andruna-elf.exe"))
            })?;
        let libs: Vec<PathBuf> = std::env::var("ANDRUNA_LIB_PATH")
            .unwrap_or_default()
            .split(';')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        let groot = root.join("servicemanager-root");
        if let Err(e) = std::fs::create_dir_all(&groot) {
            return Some(Err(e));
        }
        Some(self.start_native(&runner, Path::new(&sm), &[], &libs, &groot, AID_SYSTEM))
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
