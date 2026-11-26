# xtask

Build orchestration in Rust, run as `cargo xtask <command>` (the alias is in
`.cargo/config.toml`). `cargo xtask help` lists the commands:

| Command | What it does |
| --- | --- |
| `toolchain [--dir <dir>] [--force]` | Installs the pinned llvm-mingw release, verified by SHA-256 |
| `fetch [--project <path>]...` | Shallow-clones the AOSP projects in `aosp/aosp.lock` at the pinned tag and verifies each commit |
| `patch [--project <path>]...` | Applies `patches/<path>/*.patch` in order; already applied patches are skipped |
| `build [--arch x64\|arm64] [--release]` | Builds the Rust workspace for the gnullvm target (more parts are added as they appear) |

`fetch` and `patch` also take `--lock <file>`, `--aosp-dir <dir>` and (for
`patch`) `--patches-dir <dir>`, mainly for testing.

Dependencies are kept small and permissively licensed: `serde`, `toml` and
`sha2`. Downloads use `curl` and zip extraction uses the `tar.exe` that ships
with Windows.

## llvm-mingw location

The pinned release (version and SHA-256 per host archive) is in
`src/toolchain.rs`. The x64 host build is used on x64 machines and the ARM64
host build on ARM64 machines; both contain the compilers for both targets.

The install directory is, in order: `--dir`, the `ANDRUNA_LLVM_MINGW`
environment variable, `C:\tools\llvm-mingw`. A file `.andruna-toolchain`
inside it records the installed release, so `toolchain` is a no-op when the
pinned release is already there, and refuses to overwrite a directory it did
not install unless `--force` is given.

## How Rust finds the linker

Cargo configuration cannot expand environment variables in `linker`, so the
llvm-mingw path is not written into `.cargo/config.toml`.

- `cargo xtask build` sets `CARGO_TARGET_<TRIPLE>_LINKER` to
  `%ANDRUNA_LLVM_MINGW%\bin\<arch>-w64-mingw32-clang.exe` and puts that `bin`
  directory first on PATH for the cargo it runs.
- Plain cargo (`cargo test --target x86_64-pc-windows-gnullvm`, clippy) uses
  the target's default linker, `<arch>-w64-mingw32-clang`, from PATH. Add
  `%ANDRUNA_LLVM_MINGW%\bin` to PATH once. In GitHub Actions,
  `cargo xtask toolchain` does this through `GITHUB_PATH` and also sets
  `ANDRUNA_LLVM_MINGW` through `GITHUB_ENV`.

xtask itself is built for the host's default Rust target (usually
`*-pc-windows-msvc`), so it runs before llvm-mingw exists.
