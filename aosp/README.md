# aosp

AOSP is not vendored. Its sources are fetched at build time at one pinned
release tag, listed in [`aosp.lock`](aosp.lock):

```
cargo xtask fetch                              # all projects in the lock
cargo xtask fetch --project system/libbase     # one project
cargo xtask patch                              # apply patches/<path>/*.patch
```

`fetch` makes a shallow clone (`--depth 1`) of each project at the tag into
`third_party/aosp/<path>` (ignored by git) and checks that the checkout is the
commit recorded in the lock. A project already present at that commit is
skipped. Clones use `core.autocrlf=false` so files stay byte-exact and patches
apply the same way on every host, and `core.symlinks=true` because several
projects share headers through symbolic links (for example
`system/core/libutils/include`). On Windows, creating symbolic links needs
Developer Mode or an elevated shell; without it git writes each link as a
text file holding the target path, and the build fails on those headers.

`platform/libcore` cannot be checked out on Windows at this tag: one test
resource (`luni/src/test/resources/org/apache/harmony/tests/java/lang/test?.properties`)
has a name Windows cannot store, and git refuses the checkout. libcore is not
needed for the modules built so far; how to fetch it is open (S1).

To change the tag: update `tag` and every `commit` together (`git ls-remote
<remote>/<name> 'refs/tags/<tag>^{}'`), delete `third_party/aosp`, fetch, and
refresh the patches in [../patches](../patches). The current tag is recorded
in [ADR 0011](../docs/decisions/0011-initial-aosp-tag.md).

## Building

```
cargo xtask aosp-build --arch x64      # or arm64; default: the host
cargo xtask aosp-build --arch arm64 --keep-going --target libdexfile
```

`aosp-build` applies the patches, configures this directory with CMake
(Ninja generator, [`cmake/toolchain-llvm-mingw.cmake`](cmake/toolchain-llvm-mingw.cmake))
into `out/aosp/<arch>` and builds it. CMake and Ninja are taken from PATH,
from `ANDRUNA_CMAKE` / `ANDRUNA_NINJA`, or from their default Windows install
locations. Only static libraries are produced: nothing links a program yet,
because the Linux runtime they need (andruna-lx, bionic and a libc++ built
for it) does not exist yet.

Each file in [`modules/`](modules) mirrors one `Android.bp` module, with the
sources of its Android target:

| Module | Project | Notes |
| --- | --- | --- |
| `liblog` | system/logging | All Android sources, including the logd and pmsg transports |
| `libbase`, `fmtlib` | system/libbase, external/fmtlib | |
| `libcutils` | system/core | Subset: the common sources, ashmem and trace; ART includes no libcutils header |
| `libutils` | system/core | Subset: `SystemClock.cpp`, `Timers.cpp` (used by `runtime/hprof`) |
| `libz`, `liblz4` | external/zlib, external/lz4 | lz4: `lib/` only (BSD-2-Clause) |
| `libziparchive` | system/libziparchive | |
| `libtinyxml2` | external/tinyxml2 | For libartbase's metrics |
| `libartpalette` | art | The host variant (`palette_fake.cc`); Android loads `libartpalette-system.so` instead |
| `libartbase`, `libdexfile` | art | ART defaults from `art/build` (`art_defaults`, `art.go` device flags) |
| `libart_runtime` | art | Only with `--runtime` (`-DANDRUNA_AOSP_RUNTIME=ON`); does not build yet |

`tools/aconfig_cc.py` writes the aconfig flag headers that ART includes
(`com_android_art_flags.h`, `com_android_art_rw_flags.h`). Android generates
them with the aconfig tool; here every flag is a compile-time constant at
its default value (disabled), until Andruna has a flag configuration.

`tools/undefined_symbols.py <llvm-nm> out/aosp/<arch>` prints what the built
libraries leave undefined, grouped by provider (the andruna-lx export list:
[S1-lx-symbols.md](../docs/plan/spike-results/S1-lx-symbols.md)).

## How AOSP code is compiled

AOSP code is compiled **as Android code for a Windows target**: the clang
target is `x86_64-w64-windows-gnu` or `aarch64-w64-windows-gnu` (COFF
objects, Windows calling convention, the same ABI as Rust's gnullvm
targets), but the sources see an Android system. This follows the
architecture: AOSP C/C++ code "is compiled against Linux headers (bionic's)
and links to andruna-lx exports" ([02-architecture](../docs/spec/02-architecture.md);
andruna-lx in [03-components](../docs/spec/03-components.md)). The
alternative, the Windows code paths that libbase, liblog, libziparchive,
libartbase and libdexfile have for the SDK host tools, is not used: it runs
on the UCRT (its errno numbering, its file descriptors, no fork or Linux mmap
semantics), and libart has no Windows code path, so one process would mix
two C runtimes.

[`cmake/android.cmake`](cmake/android.cmake) does this with:

- `-nostdinc -nostdinc++` and this search order: the overlay
  ([`overlay/include`](overlay/include)), libc++ headers, bionic's
  `libc/include`, the kernel UAPI headers (`libc/kernel/uapi`, the
  architecture's `asm-*`, `libc/kernel/android/uapi`), clang's own headers.
- The Windows macros of the target removed (`-U_WIN32 -U__MINGW32__ ...`)
  and Android's defined: `__ANDROID__`, `__linux__`, `__unix__`, `ANDRUNA`,
  `__ANDROID_MIN_SDK_VERSION__=10000` (a platform build: every bionic API is
  declared).
- Linux layout rules where clang allows them: `-fno-short-wchar` (32-bit
  `wchar_t`) and `-mno-ms-bitfields` (GCC bitfield layout).
- Android defaults: `-fno-exceptions`, `-funwind-tables`, CodeView line
  tables (`-gline-tables-only -gcodeview`). Upstream warning sets with
  `-Werror` are not used (`-w`): the toolchain is newer than AOSP's.

The libc++ headers are llvm-mingw's. With `_WIN32` undefined they select
their Linux configuration (pthread threads, bionic locale), so the
**compiled** libc++ in llvm-mingw (Windows threads, UCRT locale) does not
match them. A libc++ and libc++abi built with the same flags against bionic
are needed before anything links. That libc++ is newer than AOSP's and no
longer includes some headers transitively;
[`andruna/libcxx_compat.h`](overlay/include/andruna/libcxx_compat.h) adds the
ones ART relies on.

### Data model

Windows targets are LLP64: `long` is 32 bits. Android's 64-bit ABI is LP64.
clang has no option to change the width of `long` on a Windows target (its
`x86_64-pc-cygwin` target is LP64 with the Windows calling convention, but
there is no ARM64 equivalent). The build therefore defines `__LP64__`, so
that bionic and AOSP choose their 64-bit layouts, and fixes the places where
bionic uses `long` for a 64-bit type:

- [`andruna/data_model.h`](overlay/include/andruna/data_model.h) (forced
  with `-include`) defines the kernel's `__kernel_long_t` and
  `__kernel_ulong_t` as `long long`, as the kernel headers allow for x32.
  `size_t`, `ssize_t`, `off_t`, `time_t`, `clock_t`, `suseconds_t`, `ino_t`
  and the other derived types are 64 bits.
- `patches/bionic/0001`: `int64_t` and `intptr_t` without `long`.
- `patches/bionic/0002`: the thread pointer passed to `arch_prctl`.

**Not fixed yet** (no compile error, but a layout or a value that differs
from Linux): every struct field, typedef and function parameter that bionic
or the UAPI headers spell `long` directly. Examples: `struct timespec`
(`tv_nsec`), `struct stat` on x86_64 (`st_nlink`, `st_blksize`,
`st_blocks`), `struct statvfs`, `rlim_t`, `fd_mask` (and so `fd_set`),
`sigset_t`, `struct sigaction` (`sa_flags`), `pthread_t` and the pthread
attribute types, `jmp_buf`, `ucontext_t`, `blkcnt_t`, `blksize_t`,
`fsblkcnt_t`, and functions that return or take `long` (`strtol`,
`sysconf`, `ftell`, `syscall`, `random`, `telldir`). There are about 190
such lines in `libc/include` alone. Two tools measure what is left:

- `python aosp/tools/data_model/compare.py` compiles bionic's headers both
  ways (this build, and a real Android LP64 build) and prints every size
  and offset that differs.
- Configuring with `-DANDRUNA_AOSP_WARNINGS=ON` replaces `-w` with
  `-Wformat`, `-Wshorten-64-to-32` and the pointer-cast warnings, which
  show AOSP code that stores 64-bit values in `long`.

The measurements, the options and a recommendation are in
[S1.md](../docs/plan/spike-results/S1.md#data-model); this needs a decision
before andruna-lx's export ABI grows.

## Licenses

Only permissively licensed parts of these projects are built and shipped
([ADR 0006](../docs/decisions/0006-open-source-permissive-dependencies.md)).
Some fetched repositories also contain files under other terms (for example
command-line programs or kernel headers), and a few components are public
domain rather than one of the listed licenses (dlmalloc, the LZMA SDK). The
build must not use files under non-permissive terms, and every AOSP module
that ships is recorded in `THIRD_PARTY_NOTICES`.

Built so far: Apache-2.0 (art, the bionic headers, system/*, the
libnativehelper headers, modules-utils; libbase also has BSD-licensed
files), MIT (fmtlib), Zlib (zlib, tinyxml2) and BSD-2-Clause (lz4 `lib/`).
The bionic kernel UAPI headers are generated from Linux headers and, as
their header comment says, contain only constants, structures and macros.
The libart attempt also reads dlmalloc's header (public domain). Not used:
lz4's GPL `programs/`, and libcap (BSD or GPL): libartbase's Android target
links it, but only the `art_exec` tool uses it.
