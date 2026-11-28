# andruna-linker

The ELF loader and dynamic linker for Android ELF code (`<arch>-linux-android`)
in a Windows process ([ADR 0013](../../docs/decisions/0013-aosp-binary-format.md)).
It plays the role of bionic's `linker64`; guest code reaches the host
through `__andruna_syscall` and a few other functions with the Android
calling convention (see `src/lib.rs` and `aosp/bionic/host_symbols.txt`).

## What it does

- ELF validation (header, program headers, dynamic section bounds), `PT_LOAD`
  mapping with andruna-lx (file-backed when lx supports it, otherwise copied),
  protections and `PT_GNU_RELRO`.
- Relocations: `RELA`, `REL`, Android `APS2` packed tables, `RELR`, `JMPREL`,
  `IRELATIVE` and IFUNC symbols, TLS (`DTPMOD64`, `DTPOFF64`, `TPOFF64`,
  `TLSDESC`) on x86_64 and arm64. Text and copy relocations are rejected.
- Symbol lookup with GNU and System V hash tables and symbol versioning
  (bionic's rules), bionic's lookup order (`DT_SYMBOLIC`, global group, local
  group, then the host table).
- Dependencies breadth first, `LD_LIBRARY_PATH`, `DT_RUNPATH` with `$ORIGIN`,
  initializers dependencies first (`DT_PREINIT_ARRAY` of the executable
  first), finalizers on `dlclose`.
- ELF TLS on the guest thread pointer (S4: x64 FS base with lazy restore;
  arm64 `TPIDR_EL0`, which Windows preserves per thread, written once). With bionic, the layout and module table are bionic's,
  through the bridge `ld-android.so` (`aosp/bionic/ld_android_bridge.cpp`);
  without bionic a built-in static layout is used.
- `dlopen`, `dlsym`, `dlvsym`, `dlclose`, `dladdr`, `dl_iterate_phdr`,
  `android_dlopen_ext` (only `ANDROID_DLEXT_USE_NAMESPACE` with the
  default namespace), as the `__loader_*` functions bionic's `libdl.so`
  calls. The process has one namespace, exported as "default"; creating
  or linking namespaces fails with a `dlerror` message.
- Threads: `__andruna_clone_thread` (bionic's `__bionic_clone`) on Windows
  threads running on bionic's stacks; `CLONE_*SETTID`/`CLEARTID` semantics.
- System calls (`__andruna_syscall`) run on the thread's Windows stack and
  go to andruna-lx; the few lx does not answer yet are in `interim.rs`
  and `procfs.rs` (`/proc/self/stat`, `/sys` processor lists), and
  liblog's logd socket is a stand-in that writes the records to the
  guest's stderr (`logd.rs`, `ANDRUNA_LOG_PRIORITY`). Faults go to
  andruna-lx's signal delivery from the first vectored handler; on arm64
  that handler first emulates `mrs ctr_el0` (and `dc cvau`/`ic ivau`),
  which Windows does not allow at EL0 and the JIT's `__clear_cache` uses.
- A vDSO (`vdso.rs`, `AT_SYSINFO_EHDR`): bionic's `clock_gettime`,
  `clock_getres`, `gettimeofday` and `time` call host functions on
  andruna-lx's clocks without a system call, as on Linux.
  `ANDRUNA_NO_VDSO` leaves it out.
- `ANDRUNA_PROFILE` (`profile.rs`): phase marks, system call counts and
  times, and exception counts on stderr at exit; with
  `ANDRUNA_PROFILE_EVERY=<seconds>` also periodically, for processes that
  hang or abort.
- Debugging: Windows unwind tables translated from `.eh_frame` (x64), crash
  reports for faults in guest code, and LLDB commands in the file named by
  `ANDRUNA_LLDB_FILE` (`command source` it in llvm-mingw's `lldb.exe`).

## Running guest programs

```
python aosp/tools/build_guest.py --arch x64          # bionic, libc++, tests
cargo build -p andruna-linker --release
target\release\andruna-elf run -L out\guest\x64\sysroot\usr\lib ^
    -L out\guest\x64\linker-tests out\guest\x64\linker-tests\linker_test
target\release\andruna-elf link --arch arm64 -L out\guest\arm64\sysroot\usr\lib ^
    -L out\guest\arm64\linker-tests out\guest\arm64\linker-tests\linker_test
```

`ANDRUNA_LIB_PATH` (`;`-separated) adds library directories; `--root`
chooses the Windows directory mounted as the guest `/`.

## Tests

```
cargo test -p andruna-linker
```

Unit tests cover the formats, validation, mapping, TLS, the system call
entry, threads and the host functions. `tests/guest.rs` runs the objects of
`tests/guest/` (built against real bionic) in a child process and checks the
arm64 objects in link-only mode on x64; it skips when the guest build is
missing unless `ANDRUNA_REQUIRE_GUEST=1`.

### ARM64 runner

On a Windows ARM64 machine, with the arm64 guest build available (built
there, or `out/guest/arm64` copied from an x64 build machine):

```
cargo test -p andruna-linker --target aarch64-pc-windows-gnullvm
set ANDRUNA_REQUIRE_GUEST=1
cargo test -p andruna-linker --target aarch64-pc-windows-gnullvm --test guest
```

The second command fails if the arm64 test objects are missing, and runs
`linker_test` and `cxx_test` natively on real bionic. `TPIDR_EL0` holds the
thread pointer (preserved per thread, measured in S4's ARM64 runs); x18 must
stay the TEB, so bionic is built with the shadow call stack set-up disabled
(patch in `patches/bionic/`).
