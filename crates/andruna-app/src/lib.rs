//! `andruna-app`: the per-app host process (docs/spec/03-components.md).
//!
//! This first version starts ART on andruna-linker with a class path and a
//! main class, as `dalvikvm` does on Android:
//!
//! ```text
//! andruna-app [options] --classpath <dex|jar>[;...] <MainClass> [args...]
//! andruna-app [options] --compile-boot        boot image with dex2oat
//! andruna-app [options] --compile <dex|jar>   app code with dex2oat (AOT)
//! andruna-app [options] --exec <elf> [args...] a native guest program
//! ```
//!
//! Guest code gets OpenGL ES and EGL from andruna-gles (ANGLE on Direct3D
//! 11): [`gles_guest_dir`] holds the guest's `libEGL.so` and friends and is
//! on the library search path, and the process registers andruna-gles's
//! host symbols before the linker starts. With `--compositor <handles>`
//! (inherited handles of a window channel, see andruna-compositor's
//! `producer` module) the guest's windows are the compositor's surfaces.
//!
//! The process mounts the guest file tree with andruna-lx (see [`Layout`]),
//! then andruna-linker loads ART's `dalvikvm` (or `dex2oat`) with the real
//! guest bionic and starts it on a guest main thread; the guest ends the
//! process with its exit status.
//!
//! `andruna-app --host` is the same process started by sysd for a package:
//! the assignment comes over the host channel and the boot image from the
//! view sysd mapped ([`host`]). The framework comes in a later phase.

use std::path::{Path, PathBuf};

pub mod host;
pub mod layout;

pub use layout::{ArtRoot, Layout};

/// The instruction set name ART uses for this build's architecture.
pub const ISA: &str = if cfg!(target_arch = "x86_64") {
    "x86_64"
} else {
    "arm64"
};

/// The boot class path jars, in order: the ART module's core libraries and
/// Bouncy Castle, then Conscrypt (on Android in the com.android.conscrypt
/// APEX; here in the ART root's javalib, ADR 0021).
pub const BOOT_JARS: [&str; 5] = [
    "core-oj.jar",
    "core-libart.jar",
    "core-icu4j.jar",
    "bouncycastle.jar",
    "conscrypt.jar",
];

/// Guest directory of the ART module.
pub const ART_APEX: &str = "/apex/com.android.art";
/// Guest directory of the boot class path jars and the boot image.
pub const JAVALIB: &str = "/apex/com.android.art/javalib";
/// Guest directory of ICU's data file.
pub const ICU_DATA: &str = "/apex/com.android.i18n/etc/icu";
/// Guest directory of the time zone data module.
pub const TZDATA_APEX: &str = "/apex/com.android.tzdata";
/// Guest path of the boot image (ART inserts the ISA directory).
pub const BOOT_IMAGE: &str = "/apex/com.android.art/javalib/boot.art";
/// ART_BASE_ADDRESS of the build (aosp/modules/libartbase.cmake, S2):
/// below it Windows leaves the address space free.
pub const BOOT_IMAGE_BASE: &str = "0x60000000";

/// How compiled code is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The interpreter only (`-Xint`).
    Int,
    /// The interpreter and the JIT (`-Xusejit:true`).
    Jit,
    /// AOT code from dex2oat (boot image and the app's odex), no JIT.
    Aot,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "int" => Ok(Mode::Int),
            "jit" => Ok(Mode::Jit),
            "aot" => Ok(Mode::Aot),
            _ => Err(format!("unknown mode '{s}' (int, jit or aot)")),
        }
    }
}

/// What the process does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Runs `main` of `class` with the class path.
    Run {
        classpath: Vec<PathBuf>,
        class: String,
        args: Vec<String>,
    },
    /// Compiles the boot class path into the boot image.
    CompileBoot,
    /// Compiles the class path entries against the boot image.
    Compile { classpath: Vec<PathBuf> },
    /// Runs a native guest program (Android ELF) in the app process's
    /// tree: the program's directory is `/data/app/cp0`.
    Exec { exe: PathBuf, args: Vec<String> },
}

/// Parsed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub art_root: Option<PathBuf>,
    pub data: Option<PathBuf>,
    pub mode: Mode,
    /// Start without the boot image even when it exists (the boot class
    /// path then runs interpreted or JIT-compiled too).
    pub no_boot_image: bool,
    /// Extra runtime options for ART (`-X...`), in order.
    pub art_options: Vec<String>,
    /// The window channel's inherited handles (`--compositor`).
    pub compositor: Option<String>,
    pub action: Action,
}

pub const USAGE: &str = "\
usage: andruna-app [options] --classpath <dex|jar>[;...] <MainClass> [args...]
       andruna-app [options] --compile-boot
       andruna-app [options] --compile <dex|jar>[;...]
       andruna-app [options] --exec <elf> [args...]
       andruna-app --host   (started by sysd, assignment over the host channel)

options:
  --mode int|jit|aot   interpreter, JIT (default) or AOT code from dex2oat
  --no-boot-image      start without the boot image
  --art-root <dir>     the ART module (lib64, bin, javalib, icu); default:
                       ANDRUNA_ART_ROOT, else the art directory next to
                       this executable
  --data <dir>         Windows directory mounted as the guest /; default
                       %TEMP%\\andruna-app
  --compositor <handles>
                       the window channel's inherited handles
                       (section:capacity:to_app:to_compositor, hex); the
                       guest's windows are then the compositor's surfaces
  -X<option>, -D<name>=<value>, -verbose:<what>
                       passed to ART";

fn split_classpath(s: &str) -> Vec<PathBuf> {
    s.split(';')
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Parses the arguments after the program name.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut art_root = None;
    let mut data = None;
    let mut mode = Mode::Jit;
    let mut no_boot_image = false;
    let mut art_options = Vec::new();
    let mut compositor = None;
    let mut i = 0;
    let value = |i: usize, name: &str| -> Result<String, String> {
        args.get(i)
            .cloned()
            .ok_or_else(|| format!("{name} needs a value"))
    };
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--art-root" => {
                i += 1;
                art_root = Some(PathBuf::from(value(i, a)?));
            }
            "--data" => {
                i += 1;
                data = Some(PathBuf::from(value(i, a)?));
            }
            "--mode" => {
                i += 1;
                mode = Mode::parse(&value(i, a)?)?;
            }
            "--no-boot-image" => no_boot_image = true,
            "--compositor" => {
                i += 1;
                compositor = Some(value(i, a)?);
            }
            "--exec" => {
                i += 1;
                let exe = PathBuf::from(value(i, a)?);
                return Ok(Options {
                    art_root,
                    data,
                    mode,
                    no_boot_image,
                    art_options,
                    compositor,
                    action: Action::Exec {
                        exe,
                        args: args[i + 1..].to_vec(),
                    },
                });
            }
            "--compile-boot" => {
                return finish(
                    art_root,
                    data,
                    mode,
                    no_boot_image,
                    art_options,
                    compositor,
                    Action::CompileBoot,
                    &args[i + 1..],
                );
            }
            "--compile" => {
                i += 1;
                let classpath = split_classpath(&value(i, a)?);
                return finish(
                    art_root,
                    data,
                    mode,
                    no_boot_image,
                    art_options,
                    compositor,
                    Action::Compile { classpath },
                    &args[i + 1..],
                );
            }
            "--classpath" | "-cp" | "-classpath" => {
                i += 1;
                let classpath = split_classpath(&value(i, a)?);
                let class = args
                    .get(i + 1)
                    .cloned()
                    .ok_or("missing main class after the class path")?;
                if class.starts_with('-') {
                    return Err(format!("options go before the class path: {class}"));
                }
                return Ok(Options {
                    art_root,
                    data,
                    mode,
                    no_boot_image,
                    art_options,
                    compositor,
                    action: Action::Run {
                        classpath,
                        class,
                        args: args[i + 2..].to_vec(),
                    },
                });
            }
            x if x.starts_with("-X") || x.starts_with("-D") || x.starts_with("-verbose:") => {
                art_options.push(x.to_string())
            }
            x => return Err(format!("unknown argument '{x}'")),
        }
        i += 1;
    }
    Err("nothing to do: give --classpath, --exec, --compile or --compile-boot".into())
}

#[allow(clippy::too_many_arguments)]
fn finish(
    art_root: Option<PathBuf>,
    data: Option<PathBuf>,
    mode: Mode,
    no_boot_image: bool,
    art_options: Vec<String>,
    compositor: Option<String>,
    action: Action,
    rest: &[String],
) -> Result<Options, String> {
    if let Some(x) = rest.first() {
        return Err(format!("unexpected argument '{x}'"));
    }
    if let Action::Compile { classpath } = &action
        && classpath.is_empty()
    {
        return Err("--compile needs a dex or jar file".into());
    }
    Ok(Options {
        art_root,
        data,
        mode,
        no_boot_image,
        art_options,
        compositor,
        action,
    })
}

/// The class path of an action: what [`Layout::install`] mounts under
/// `/data/app` (for `--exec`, the program).
pub fn mounted_entries(action: &Action) -> Vec<PathBuf> {
    match action {
        Action::Run { classpath, .. } | Action::Compile { classpath } => classpath.clone(),
        Action::Exec { exe, .. } => vec![exe.clone()],
        Action::CompileBoot => vec![],
    }
}

/// Environment variable naming the directory of the guest GLES libraries
/// (`libEGL.so`, `libGLESv1_CM.so`, `libGLESv2.so`, `libGLESv3.so`,
/// `libandruna_window.so`, built by andruna-gles's `tools/build_guest.py`).
pub const GLES_GUEST_VAR: &str = "ANDRUNA_GLES_GUEST";

/// The guest GLES libraries: `ANDRUNA_GLES_GUEST`, else `gles` next to the
/// executable (the runtime layout; ANGLE's DLLs are in `angle` there, see
/// andruna-gles's `angle` module). `None` when neither has `libEGL.so`.
pub fn gles_guest_dir(exe_dir: Option<&Path>) -> Option<PathBuf> {
    std::env::var_os(GLES_GUEST_VAR)
        .map(PathBuf::from)
        .or_else(|| exe_dir.map(|d| d.join("gles")))
        .filter(|d| d.join("libEGL.so").is_file())
}

/// A guest program run: what andruna-linker starts.
#[derive(Clone, Debug)]
pub struct Plan {
    /// The executable (Windows path).
    pub exe: PathBuf,
    /// Its `argv` (guest strings; `argv[0]` is the guest path).
    pub argv: Vec<String>,
    pub env: Vec<String>,
    /// Library directories (Windows paths) for the linker.
    pub search: Vec<PathBuf>,
}

/// The guest boot class path.
pub fn boot_classpath() -> String {
    BOOT_JARS
        .iter()
        .map(|j| format!("{JAVALIB}/{j}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn guest_env(layout: &Layout) -> Vec<String> {
    vec![
        "ANDROID_ROOT=/system".into(),
        "ANDROID_DATA=/data".into(),
        format!("ANDROID_ART_ROOT={ART_APEX}"),
        "ANDROID_I18N_ROOT=/apex/com.android.i18n".into(),
        format!("ANDROID_TZDATA_ROOT={TZDATA_APEX}"),
        format!("BOOTCLASSPATH={}", boot_classpath()),
        "HOME=/data/local/tmp".into(),
        "TMPDIR=/data/local/tmp".into(),
        format!("PATH={ART_APEX}/bin:/system/bin"),
        format!("ANDRUNA_DATA={}", layout.data.display()),
    ]
}

/// The guest program for `opts` on `layout`.
pub fn plan(opts: &Options, layout: &Layout) -> Result<Plan, String> {
    let bcp = boot_classpath();
    let art = &layout.art;
    let mut search = vec![art.lib()];
    // Guest libraries link against libEGL.so and libGLESv*.so from here.
    search.extend(gles_guest_dir(exe_dir().as_deref()));
    let env = guest_env(layout);
    let runtime = |xs: &[&str]| -> Vec<String> {
        xs.iter()
            .flat_map(|x| ["--runtime-arg".to_string(), x.to_string()])
            .collect()
    };
    match &opts.action {
        Action::Run {
            classpath,
            class,
            args,
        } => {
            let mut argv = vec![format!("{ART_APEX}/bin/dalvikvm")];
            argv.push(format!("-Xbootclasspath:{bcp}"));
            argv.push(format!("-Xbootclasspath-locations:{bcp}"));
            let image = !opts.no_boot_image && art.boot_image().is_file();
            if opts.mode == Mode::Aot && !image {
                return Err(format!(
                    "--mode aot needs the boot image ({}); run andruna-app --compile-boot",
                    art.boot_image().display()
                ));
            }
            if image {
                argv.push(format!("-Ximage:{BOOT_IMAGE}"));
            } else {
                // A boot image path that does not exist: ART starts
                // without one.
                argv.push(format!("-Ximage:{JAVALIB}/none.art"));
            }
            argv.push("-Xnoimage-dex2oat".into());
            argv.push(
                match opts.mode {
                    Mode::Int => "-Xint",
                    Mode::Jit => "-Xusejit:true",
                    Mode::Aot => "-Xusejit:false",
                }
                .into(),
            );
            argv.extend(opts.art_options.iter().cloned());
            argv.push("-cp".into());
            argv.push(layout.guest_classpath(classpath)?.join(":"));
            argv.push(class.clone());
            argv.extend(args.iter().cloned());
            Ok(Plan {
                exe: art.bin().join("dalvikvm"),
                argv,
                env,
                search,
            })
        }
        Action::Exec { exe, args } => {
            let mut argv = layout.guest_classpath(std::slice::from_ref(exe))?;
            argv.extend(args.iter().cloned());
            if let Some(d) = std::path::absolute(exe)
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
            {
                search.push(d);
            }
            Ok(Plan {
                exe: exe.clone(),
                argv,
                env,
                search,
            })
        }
        Action::CompileBoot => {
            let out = format!("{JAVALIB}/{ISA}");
            let mut argv = vec![format!("{ART_APEX}/bin/dex2oat")];
            argv.extend(runtime(&["-Xms64m", "-Xmx512m"]));
            for j in BOOT_JARS {
                argv.push(format!("--dex-file={JAVALIB}/{j}"));
                argv.push(format!("--dex-location={JAVALIB}/{j}"));
            }
            argv.extend([
                format!("--image={out}/boot.art"),
                format!("--oat-file={out}/boot.oat"),
                format!("--oat-location={out}/boot.oat"),
                format!("--base={BOOT_IMAGE_BASE}"),
                format!("--instruction-set={ISA}"),
                "--compiler-filter=speed".into(),
                "--image-format=uncompressed".into(),
                "--android-root=/system".into(),
                "--strip".into(),
                "--avoid-storing-invocation".into(),
                "--abort-on-hard-verifier-error".into(),
                "--no-abort-on-soft-verifier-error".into(),
                "--compilation-reason=andruna".into(),
            ]);
            argv.extend(runtime(&[
                &format!("-Xbootclasspath:{bcp}"),
                &format!("-Xbootclasspath-locations:{bcp}"),
            ]));
            Ok(Plan {
                exe: art.bin().join("dex2oat"),
                argv,
                env,
                search,
            })
        }
        Action::Compile { classpath } => {
            if !art.boot_image().is_file() {
                return Err(format!(
                    "--compile needs the boot image ({}); run andruna-app --compile-boot",
                    art.boot_image().display()
                ));
            }
            if classpath.len() != 1 {
                return Err("--compile takes one dex or jar file".into());
            }
            let dex = layout.guest_classpath(classpath)?.remove(0);
            let (dir, file) = dex.rsplit_once('/').unwrap_or(("", &dex));
            let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
            // Where ART looks for the odex of a dex file outside the
            // dalvik-cache: <dir>/oat/<isa>/<name>.odex.
            layout.ensure_oat_dir(&classpath[0])?;
            let mut argv = vec![format!("{ART_APEX}/bin/dex2oat")];
            argv.extend(runtime(&["-Xms64m", "-Xmx256m"]));
            argv.extend([
                format!("--dex-file={dex}"),
                format!("--dex-location={dex}"),
                format!("--oat-file={dir}/oat/{ISA}/{stem}.odex"),
                format!("--boot-image={BOOT_IMAGE}"),
                "--class-loader-context=PCL[]".into(),
                format!("--instruction-set={ISA}"),
                "--compiler-filter=speed".into(),
                "--android-root=/system".into(),
                "--avoid-storing-invocation".into(),
                "--compilation-reason=andruna".into(),
            ]);
            argv.extend(runtime(&[
                &format!("-Xbootclasspath:{bcp}"),
                &format!("-Xbootclasspath-locations:{bcp}"),
            ]));
            Ok(Plan {
                exe: art.bin().join("dex2oat"),
                argv,
                env,
                search,
            })
        }
    }
}

/// Starts the plan on andruna-linker. Does not return: the guest ends the
/// process with its exit status.
pub fn execute(plan: Plan) -> Result<std::convert::Infallible, String> {
    use andruna_linker::process::{Program, run};
    if !plan.exe.is_file() {
        return Err(format!("{} not found", plan.exe.display()));
    }
    // `__andruna_gles_resolve` for the guest GLES stubs, before the linker
    // binds any guest library.
    andruna_gles::register();
    let _ = andruna_lx::fs::set_cwd("/data/local/tmp");
    let config = andruna_linker::Config {
        search: plan.search,
        ..Default::default()
    };
    run(Program {
        config,
        exe: plan.exe.display().to_string(),
        args: plan.argv,
        env: plan.env,
    })
    .map_err(|e| e.to_string())
}

/// How long the guest's `andruna_window_create` waits for the compositor
/// to hand over a surface.
const SURFACE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

static COMPOSITOR: std::sync::OnceLock<andruna_compositor::producer::Connection> =
    std::sync::OnceLock::new();

/// Opens the window channel given with `--compositor` (inherited handles)
/// and makes the compositor's surfaces the guest's windows.
pub fn connect_compositor(handles: &str) -> Result<(), String> {
    use andruna_compositor::channel::ChannelHandles;
    use andruna_compositor::producer::Connection;
    let h = ChannelHandles::parse(handles)
        .ok_or_else(|| format!("--compositor: bad handles '{handles}'"))?;
    // SAFETY: sysd (or the test host) started this process with these
    // handles inherited; nothing else in the process owns them.
    let c = unsafe { Connection::open(h) }.map_err(|e| format!("--compositor: {e}"))?;
    if COMPOSITOR.set(c).is_err() {
        return Err("--compositor given twice".into());
    }
    andruna_gles::harness::set_window_provider(Some(Box::new(|_, _, _| {
        let c = COMPOSITOR.get().ok_or("no compositor connection")?;
        let s = c
            .next_surface(SURFACE_WAIT)
            .ok_or("the compositor handed over no surface")?;
        Ok(s as std::sync::Arc<dyn andruna_gles::window::WindowTarget>)
    })));
    Ok(())
}

/// The directory of the running executable.
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_a_run() {
        let o = parse(&s(&[
            "--mode",
            "int",
            "-Xmx64m",
            "--classpath",
            r"C:\a\hello.dex;C:\b\lib.jar",
            "Hello",
            "x",
            "--y",
        ]))
        .unwrap();
        assert_eq!(o.mode, Mode::Int);
        assert_eq!(o.art_options, s(&["-Xmx64m"]));
        assert_eq!(
            o.action,
            Action::Run {
                classpath: vec![r"C:\a\hello.dex".into(), r"C:\b\lib.jar".into()],
                class: "Hello".into(),
                args: s(&["x", "--y"]),
            }
        );
    }

    #[test]
    fn rejects_bad_command_lines() {
        assert!(parse(&[]).is_err());
        assert!(parse(&s(&["--mode", "fast", "--compile-boot"])).is_err());
        assert!(parse(&s(&["--classpath", "a.dex"])).is_err());
        assert!(parse(&s(&["--classpath", "a.dex", "-Xint"])).is_err());
        assert!(parse(&s(&["--compile-boot", "extra"])).is_err());
        assert!(parse(&s(&["--compile", ";"])).is_err());
        assert!(parse(&s(&["--bogus"])).is_err());
    }

    #[test]
    fn parses_compile_actions() {
        let o = parse(&s(&["--art-root", "r", "--compile-boot"])).unwrap();
        assert_eq!(o.action, Action::CompileBoot);
        assert_eq!(o.art_root, Some("r".into()));
        let o = parse(&s(&["--compile", "a.dex"])).unwrap();
        assert_eq!(
            o.action,
            Action::Compile {
                classpath: vec!["a.dex".into()]
            }
        );
    }

    #[test]
    fn parses_exec_and_the_compositor_channel() {
        let o = parse(&s(&[
            "--compositor",
            "1a4:10000:1b0:1b4",
            "--exec",
            r"C:\g\gles_window",
            "-x",
            "--y",
        ]))
        .unwrap();
        assert_eq!(o.compositor.as_deref(), Some("1a4:10000:1b0:1b4"));
        assert_eq!(
            o.action,
            Action::Exec {
                exe: r"C:\g\gles_window".into(),
                args: s(&["-x", "--y"]),
            }
        );
        assert_eq!(
            mounted_entries(&o.action),
            vec![PathBuf::from(r"C:\g\gles_window")]
        );
        assert!(parse(&s(&["--exec"])).is_err());
        assert!(parse(&s(&["--compositor"])).is_err());
        // Bad handles are reported when connecting, not here.
        assert!(connect_compositor("nope").is_err());
    }

    #[test]
    fn exec_runs_the_program_from_cp0_with_the_gles_libraries() {
        let d = std::env::temp_dir().join(format!("andruna-app-exec-{}", std::process::id()));
        std::fs::create_dir_all(d.join("gles")).unwrap();
        std::fs::write(d.join("gles").join("libEGL.so"), b"elf").unwrap();
        std::fs::write(d.join("prog"), b"elf").unwrap();
        // SAFETY: tests of this crate do not read this variable concurrently.
        unsafe { std::env::set_var(GLES_GUEST_VAR, d.join("gles")) };
        let o = parse(&s(&["--exec", d.join("prog").to_str().unwrap(), "a"])).unwrap();
        let l = Layout::new(ArtRoot { dir: d.join("art") }, Some(&d.join("data")));
        let p = plan(&o, &l).unwrap();
        assert_eq!(p.argv, s(&["/data/app/cp0/prog", "a"]));
        assert!(p.search.contains(&d.join("gles")), "{:?}", p.search);
        assert_eq!(gles_guest_dir(None), Some(d.join("gles")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn boot_classpath_is_the_art_module_jars() {
        assert_eq!(
            boot_classpath(),
            "/apex/com.android.art/javalib/core-oj.jar:\
             /apex/com.android.art/javalib/core-libart.jar:\
             /apex/com.android.art/javalib/core-icu4j.jar:\
             /apex/com.android.art/javalib/bouncycastle.jar:\
             /apex/com.android.art/javalib/conscrypt.jar"
        );
    }
}
