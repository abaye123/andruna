# andruna-app

The per-app host process ([docs/spec/03-components.md](../../docs/spec/03-components.md#andruna-app-app-and-host-process)).
This first version starts ART on andruna-linker with the real guest bionic
and runs a Java program, as `dalvikvm` does on Android:

```
andruna-app [options] --classpath <dex|jar>[;...] <MainClass> [args...]
andruna-app [options] --compile-boot          boot image (dex2oat)
andruna-app [options] --compile <dex|jar>     AOT code for one entry (dex2oat)
andruna-app [options] --exec <elf> [args...]  a native guest program

  --mode int|jit|aot   interpreter, JIT (default), or AOT code without JIT
  --no-boot-image      start without the boot image
  --art-root <dir>     the ART root (default ANDRUNA_ART_ROOT, else art\
                       next to the executable)
  --data <dir>         Windows directory mounted as the guest / (default
                       %TEMP%\andruna-app)
  --compositor <handles>       the window channel's inherited handles
                       (section:capacity:to_app:to_compositor, hex)
  -X..., -D..., -verbose:...   passed to ART
```

## OpenGL ES and windows

The process registers andruna-gles's host symbols before the linker
starts, and puts the guest GLES libraries (`libEGL.so`, `libGLESv1_CM.so`,
`libGLESv2.so`, `libGLESv3.so`, `libandruna_window.so`) on the library
search path: `ANDRUNA_GLES_GUEST`, else `gles\` next to the executable.
ANGLE's DLLs come from `ANDRUNA_ANGLE_DIR`, else `angle\` next to the
executable ([ADR 0048](../../docs/decisions/0048-gles-runtime-layout-and-native-programs.md)).

With `--compositor`, the process opens its window channel
(andruna-compositor's `producer` module, [ADR 0047](../../docs/decisions/0047-surface-handshake-on-the-window-channel.md))
and the guest's `andruna_window_create` returns the surfaces the
compositor hands over; an EGL window surface on it presents straight to
the task window. `--exec` runs a native program (its directory is
`/data/app/cp0`; the ART root is optional for it).

The process mounts the guest tree with andruna-lx, then andruna-linker
loads ART's `dalvikvm` (or `dex2oat`) from the ART root and starts it on a
guest main thread; the guest ends the process with its exit status.

| Guest path | Windows directory |
| --- | --- |
| `/` | `--data` |
| `/apex/com.android.art` | the ART root: `lib64` (bionic, libc++, ART), `bin` (`dalvikvm`, `dex2oat`), `javalib` (boot class path; boot image in `javalib/<isa>`) |
| `/apex/com.android.i18n/etc/icu` | `<ART root>/icu` (ICU's data) |
| `/data/app/cp<N>` | the directory of class path entry N (its odex goes to `oat/<isa>/` there) |

## Building the ART root

```
cargo xtask fetch
cargo xtask patch
python spikes/s1/java/build_boot.py                  # boot jars, test dex files
python aosp/tools/build_guest.py --arch x64          # bionic, libc++, ...
python aosp/tools/build_guest.py --arch x64 art      # ART -> out\guest\x64\art-root
cargo build -p andruna-app --release
target\release\andruna-app --art-root out\guest\x64\art-root ^
    --classpath out\s1\java\hello.dex Hello
```

## Tests

`tests/gl.rs` is a compositor host: it opens a task window, starts
`andruna-app --compositor ... --exec gles_window` (and the Java program
`GlMain` with its JNI library, `tools/build_gl_java.py`), hands the app a
surface, checks screenshots for upright content, resizes the window and
checks the producer's new buffers, and closes the window. It needs
ANGLE's DLLs and the guest GLES build (skipped without them unless
`ANDRUNA_REQUIRE_GLES=1`); the Java part also needs the ART root.


`cargo test -p andruna-app`: unit tests for the command line and the
layout; `tests/run.rs` runs Hello World in the interpreter, with the JIT
(also compiling every method at its first call), the fault test (implicit
null checks and stack overflow through the signal path), dex2oat for the
boot image and the app and the AOT run, and the JNI stress test (native
calls on several threads during GCs and stack walks, so that compiled JNI
stubs take their slow paths, with the boot image and with JIT-compiled
stubs), each in a child process. They skip
when the ART root or the test dex files are missing, unless
`ANDRUNA_REQUIRE_GUEST=1`.

## Host mode

`andruna-app --host` is what sysd starts for a package (ADRs 0035 to
0038, [app lifecycle](../../docs/plan/phase2/app-lifecycle.md)): created
suspended in the package's AppContainer with the boot image view mapped
at the image base, it reads its assignment from the host channel
(`ANDRUNA_HOST_CHANNEL`, `andruna_ipc::host`), registers the view with lx
(`andruna_lx::image`, so ART's image mappings share the view's pages) and
runs ART with `-Xnorelocate`. The guest's exit status goes back to sysd
through the linker's exit hook; at exit the host prints how much of the
image it adopted and copied.

## Not yet

- The framework and the main Looper: `--host` refuses the framework entry
  until framework.jar is part of the runtime; adding it changes
  `host::options_for` and the plan (an `app_process`-like guest entry
  instead of `dalvikvm`).
- arm64: runs (interpreter, JIT, AOT) in CI, `.github/workflows/art-arm64.yml`.
