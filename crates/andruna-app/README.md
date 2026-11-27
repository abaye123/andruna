# andruna-app

The per-app host process ([docs/spec/03-components.md](../../docs/spec/03-components.md#andruna-app-app-and-host-process)).
This first version starts ART on andruna-linker with the real guest bionic
and runs a Java program, as `dalvikvm` does on Android:

```
andruna-app [options] --classpath <dex|jar>[;...] <MainClass> [args...]
andruna-app [options] --compile-boot          boot image (dex2oat)
andruna-app [options] --compile <dex|jar>     AOT code for one entry (dex2oat)

  --mode int|jit|aot   interpreter, JIT (default), or AOT code without JIT
  --no-boot-image      start without the boot image
  --art-root <dir>     the ART root (default ANDRUNA_ART_ROOT, else art\
                       next to the executable)
  --data <dir>         Windows directory mounted as the guest / (default
                       %TEMP%\andruna-app)
  -X..., -D..., -verbose:...   passed to ART
```

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

`cargo test -p andruna-app`: unit tests for the command line and the
layout; `tests/run.rs` runs Hello World in the interpreter, with the JIT
(also compiling every method at its first call), the fault test (implicit
null checks and stack overflow through the signal path), dex2oat for the
boot image and the app and the AOT run, each in a child process. They skip
when the ART root or the test dex files are missing, unless
`ANDRUNA_REQUIRE_GUEST=1`.

## Not yet

- The sysd assignment protocol, the shared boot image section, the
  framework and the main Looper (phase 2). The intended wiring: sysd
  starts `andruna-app` per package with a pipe handle and the package's
  data directory; `andruna-app` mounts the tree as here, starts the
  runtime through an `app_process`-like guest entry instead of `dalvikvm`,
  and receives the package assignment over the pipe; the launcher keeps
  talking to sysd only.
- arm64: runs (interpreter, JIT, AOT) in CI, `.github/workflows/art-arm64.yml`.
