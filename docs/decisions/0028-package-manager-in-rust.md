# 0028 - Package manager backend in Rust: dependencies and ABI policy

**Status:** proposed, 2026-11-27

## Context
The components spec lets andruna-pkg verify signatures "in Rust or using the
framework's verifier in sysd". The framework (PackageParser2, apksig in
Java) is not running in sysd yet, and the install must work before it is
(phase 2 exit criterion: a test APK installed, parsed, verified,
compiled). Parsing untrusted APKs is attack surface; the security spec
asks for Rust there. Two app-visible defaults need deciding: which native
ABIs a PC accepts, and which config splits are installed.

## Decision
- **Rust, own parsers.** Zip, binary XML, resource table, JAR manifest and
  DER parsing are written in andruna-pkg (small, bounds-checked, fuzz-style
  tests). Signature schemes v1, v2, v3 and v3.1 follow apksig's rules and are
  cross-checked against apksigner in tests.
- **Dependencies** (all permissive, checked by cargo deny): RustCrypto for
  hashes and signatures (`sha1`, `sha2`, `rsa`, `p256`, `p384`, `p521`;
  verification only in shipped code), `miniz_oxide` and `crc32fast` for zip
  data, `tiny-skia` (BSD-3-Clause) and `image-webp` for icon rendering,
  `serde`/`serde_json` for the package database. `rand` only in test
  builds.
- **ABI policy.** Andruna runs 64-bit code only. x64 PCs prefer `x86_64`
  and accept `arm64-v8a` (translated, phase 5); ARM64 PCs accept
  `arm64-v8a`. Packages whose native code is only for other ABIs
  (`armeabi-v7a`, `x86`, ...) are refused with a clear reason (P-14).
- **Splits.** From a bundle, the selected ABI's split, every language
  split and the density split nearest xxhdpi (480 dpi) are installed;
  feature splits are kept.

## Consequences
- Installs work without the framework; when PackageManagerService runs in
  sysd it reads the same database rather than parsing again.
- Two advisories are about timing side channels on secret values: `rsa`
  private-key operations (RUSTSEC-2023-0071) and `crypto-bigint` 0.5
  arithmetic (RUSTSEC-2026-0387, fixed in the 0.7 generation that the next
  RustCrypto releases use). Verification handles public data only and
  shipped code never holds a private key, so both are ignored in
  `deny.toml` with this reason. Moving to the next RustCrypto generation
  (`rsa` 0.10, `p256` 0.14) removes them; that upgrade changes trait
  versions across the crates and is left for a separate change.
- 32-bit-only apps (rare on current Android) cannot be installed.
- Not implemented yet: v4 signatures (`.idsig`), DSA keys, source stamps,
  resource tables of splits for labels.
