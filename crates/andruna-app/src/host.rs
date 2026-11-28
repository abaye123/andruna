//! `andruna-app --host`: the app host process started by sysd.
//!
//! sysd creates the process suspended inside the package's AppContainer,
//! maps the boot image view into it and resumes it
//! ([ADR 0035](../../../docs/decisions/0035-warm-process-strategy.md)). The
//! process talks to sysd over two inherited anonymous pipes whose handle
//! values are in `ANDRUNA_HOST_CHANNEL` (`<read handle>;<write handle>`),
//! with the host protocol of `andruna_ipc::host`:
//!
//! 1. send `Hello`, receive `Attach` (package, data directory, ART root,
//!    entry, boot image view);
//! 2. register the boot image view with lx ([`andruna_lx::image`]) so ART
//!    maps its image from the view instead of reading the files;
//! 3. send `Running` and start the runtime as [`crate::execute`] does;
//! 4. the guest's `exit_group` sends `Exit` with the status (an exit hook
//!    of the linker); `Stop` from sysd ends the process with status 0.
//!
//! The app's standard output and error are pipes that sysd reads; this
//! process does not touch them.
//!
//! What runs is [`Entry::Main`] (a class's `main`, as `dalvikvm`). The
//! framework entry ([`Entry::Framework`], `ActivityThread.main` through an
//! `app_process`-like start) is refused until the framework is part of the
//! runtime; adding it changes only [`options_for`] (another guest program
//! and its arguments) and the `plan` it leads to.

use std::fs::File;
use std::io::Write;
use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
use std::path::PathBuf;
use std::sync::Mutex;

use andruna_ipc::host::{self, Attach, Entry, FromHost, ToHost};

use crate::{ART_APEX, Action, ArtRoot, Layout, Mode, Options, execute, plan};

/// Environment variable with the channel's handle values.
pub const CHANNEL_ENV: &str = "ANDRUNA_HOST_CHANNEL";

/// Exit status when the channel or the assignment is unusable.
pub const EXIT_REFUSED: i32 = 2;

/// The write end to sysd, shared by the main thread, the stop listener and
/// the exit hook.
static TO_SYSD: Mutex<Option<File>> = Mutex::new(None);

fn send(m: &FromHost) {
    let mut g = TO_SYSD.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(w) = g.as_mut()
        && host::send(w, &m.encode()).is_err()
    {
        // sysd is gone; the job object ends this process soon.
        *g = None;
    }
}

fn on_exit(code: i32) {
    if andruna_lx::image::region().is_some() {
        let s = andruna_lx::image::stats();
        eprintln!(
            "andruna-app: boot image view: {} KiB adopted, {} KiB copied",
            s.adopted / 1024,
            s.copied / 1024
        );
    }
    send(&FromHost::Exit { code, crash: None });
    let _ = TO_SYSD.lock().map(|mut g| g.as_mut().map(|w| w.flush()));
}

/// Parses `<read>;<write>` handle values.
pub fn parse_channel(v: &str) -> Result<(usize, usize), String> {
    let (a, b) = v
        .split_once(';')
        .ok_or_else(|| format!("{CHANNEL_ENV} is not `<read>;<write>`: {v}"))?;
    let p = |s: &str| {
        s.trim()
            .parse::<usize>()
            .ok()
            .filter(|h| *h != 0)
            .ok_or_else(|| format!("{CHANNEL_ENV}: invalid handle `{s}`"))
    };
    Ok((p(a)?, p(b)?))
}

fn open_channel() -> Result<(File, File), String> {
    let v = std::env::var(CHANNEL_ENV)
        .map_err(|_| format!("{CHANNEL_ENV} is not set; --host is started by sysd"))?;
    let (r, w) = parse_channel(&v)?;
    // SAFETY: sysd passes these handles in the process's handle list for
    // this process alone; nothing else in the process owns or closes them,
    // and they are taken exactly once here.
    let (r, w) = unsafe {
        (
            OwnedHandle::from_raw_handle(r as RawHandle),
            OwnedHandle::from_raw_handle(w as RawHandle),
        )
    };
    Ok((File::from(r), File::from(w)))
}

/// The command-line options equivalent to an assignment.
pub fn options_for(a: &Attach) -> Result<Options, String> {
    let (classpath, class, args) = match &a.entry {
        Entry::Main {
            classpath,
            class,
            args,
        } => (classpath, class, args),
        Entry::Framework { .. } => {
            return Err(
                "the framework entry is not available: framework.jar is not part of this runtime"
                    .into(),
            );
        }
    };
    let mut art_options = a.runtime_options.clone();
    if a.boot_image.is_some() {
        // The view is at the image's own address; relocating would copy it.
        art_options.push("-Xnorelocate".into());
    }
    Ok(Options {
        art_root: Some(PathBuf::from(&a.art_root)),
        data: Some(PathBuf::from(&a.data_dir)),
        mode: Mode::parse(&a.mode)?,
        no_boot_image: false,
        art_options,
        action: Action::Run {
            classpath: classpath.iter().map(PathBuf::from).collect(),
            class: class.clone(),
            args: args.clone(),
        },
    })
}

/// Registers the boot image view sysd mapped with lx.
fn adopt_boot_image(b: &host::BootImage) -> Result<(), String> {
    let extents = b
        .extents
        .iter()
        .map(|x| {
            if x.path
                .split('/')
                .any(|c| c.is_empty() || c == "." || c == "..")
            {
                return Err(format!("invalid boot image path `{}`", x.path));
            }
            Ok(andruna_lx::image::Extent {
                path: format!("{ART_APEX}/{}", x.path).into_bytes(),
                offset: x.offset,
                addr: x.addr as usize,
                len: x.len as usize,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    andruna_lx::image::adopt(b.base as usize, b.size as usize, extents).map_err(|e| {
        format!(
            "the boot image view at {:#x} (+{:#x}) is not usable: {e:?}",
            b.base, b.size
        )
    })
}

/// Runs the host. Returns only on failure (the guest ends the process).
pub fn run() -> i32 {
    andruna_linker::profile::mark("andruna-app host main");
    let (mut from_sysd, to_sysd) = match open_channel() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("andruna-app: {e}");
            return EXIT_REFUSED;
        }
    };
    *TO_SYSD.lock().unwrap_or_else(|e| e.into_inner()) = Some(to_sysd);
    send(&FromHost::Hello {
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").into(),
    });
    let attach = match host::recv_to_host(&mut from_sysd) {
        Ok(Some(ToHost::Attach(a))) => a,
        Ok(Some(ToHost::Stop)) | Ok(None) => return 0,
        Err(e) => {
            send(&FromHost::Refused {
                message: format!("cannot read the assignment: {e}"),
            });
            return EXIT_REFUSED;
        }
    };
    match start(&attach, from_sysd) {
        Ok(never) => match never {},
        Err(message) => {
            eprintln!("andruna-app: {message}");
            send(&FromHost::Refused { message });
            EXIT_REFUSED
        }
    }
}

fn start(a: &Attach, mut from_sysd: File) -> Result<std::convert::Infallible, String> {
    let opts = options_for(a)?;
    if let Some(b) = &a.boot_image {
        adopt_boot_image(b)?;
    }
    let art = ArtRoot::locate(opts.art_root.as_deref(), None)?;
    let layout = Layout::new(art, opts.data.as_deref());
    let classpath = match &opts.action {
        Action::Run { classpath, .. } => classpath.clone(),
        _ => vec![],
    };
    let p = plan(&opts, &layout)?;
    layout.install(&classpath)?;
    andruna_linker::process::set_exit_hook(on_exit);
    std::thread::Builder::new()
        .name("sysd-channel".into())
        .spawn(move || {
            loop {
                match host::recv_to_host(&mut from_sysd) {
                    Ok(Some(ToHost::Stop)) => {
                        andruna_linker::process::exit_process(0);
                    }
                    Ok(Some(ToHost::Attach(_))) => continue,
                    // sysd closed the channel: it ends the process through
                    // the job when it needs to.
                    Ok(None) | Err(_) => break,
                }
            }
        })
        .map_err(|e| format!("cannot start the channel thread: {e}"))?;
    send(&FromHost::Running);
    andruna_linker::profile::mark("host attached");
    execute(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attach(entry: Entry, boot: bool) -> Attach {
        Attach {
            package: "com.example.app".into(),
            data_dir: r"C:\d".into(),
            art_root: r"C:\art".into(),
            entry,
            mode: "int".into(),
            runtime_options: vec!["-Xmx64m".into()],
            boot_image: boot.then(|| host::BootImage {
                base: 0x6000_0000,
                size: 0x1_0000,
                extents: vec![],
            }),
        }
    }

    #[test]
    fn channel_values() {
        assert_eq!(parse_channel("12;34"), Ok((12, 34)));
        assert!(parse_channel("12").is_err());
        assert!(parse_channel("0;3").is_err());
        assert!(parse_channel("a;3").is_err());
    }

    #[test]
    fn assignment_to_options() {
        let main = Entry::Main {
            classpath: vec![r"C:\app\hello.dex".into()],
            class: "Hello".into(),
            args: vec!["x".into()],
        };
        let o = options_for(&attach(main.clone(), true)).unwrap();
        assert_eq!(o.mode, Mode::Int);
        assert_eq!(o.art_options, vec!["-Xmx64m", "-Xnorelocate"]);
        assert_eq!(o.data, Some(PathBuf::from(r"C:\d")));
        assert_eq!(
            o.action,
            Action::Run {
                classpath: vec![PathBuf::from(r"C:\app\hello.dex")],
                class: "Hello".into(),
                args: vec!["x".into()],
            }
        );
        let o = options_for(&attach(main, false)).unwrap();
        assert_eq!(o.art_options, vec!["-Xmx64m"]);
        let e = options_for(&attach(Entry::Framework { activity: None }, false)).unwrap_err();
        assert!(e.contains("framework"), "{e}");
    }
}
