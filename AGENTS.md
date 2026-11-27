# Instructions for AI agents and contributors

Andruna is developed by one maintainer who directs and reviews, and by AI
agents who write most of the code. These rules exist so that many agents can
work in parallel without breaking each other or the product.

## Read first

1. [docs/spec/00-overview.md](docs/spec/00-overview.md) - what we build and why.
2. [docs/spec/06-compatibility-policy.md](docs/spec/06-compatibility-policy.md) -
   the policy every change is reviewed against.
3. The spec file and ADRs for the component you work on.
4. [docs/plan/roadmap.md](docs/plan/roadmap.md) - which phase is active.

## Hard rules

- **Language.** Everything in the repository is in English: code, comments,
  identifiers, commit messages, docs. The only exception is user-facing text
  (UI strings, translations), which lives in resource files per language.
- **Punctuation.** Never use the em dash (U+2014) or en dash (U+2013). Use the
  hyphen-minus (`-`).
- **Commits.** Commit messages describe the change only. No `Co-Authored-By`
  lines for AI, no "Generated with" footers, no mention of AI.
- **Licenses.** Do not copy code from GPL, LGPL or AGPL projects, or from any
  project whose license is unclear. Third-party code must be permissively
  licensed (Apache-2.0, MIT, BSD, 0BSD, zlib, ISC, or public domain with an
  explicit dedication) and recorded in
  `THIRD_PARTY_NOTICES` with its source. Andruna's own code is
  AGPL-3.0-only ([LICENSING.md](LICENSING.md), ADR 0018); this does not
  relax the rule above for third-party code.
- **No references to other projects.** Specs, plans, docs, comments and
  commit messages in this repository do not mention or link to other
  repositories or similar projects. Dependencies we actually use (AOSP,
  ANGLE, dynarmic, microG, WebView2 and the like) are named where needed.
  Research about other projects is kept outside the repository.
- **AOSP stays upstream.** AOSP sources are fetched at build time at the
  pinned tag. Changes to AOSP are small patches in `patches/<project>/`, each
  gated on `defined(ANDRUNA)` and each upstreamable in spirit.
- **Stay in your module.** Each task names the crate or directory you own.
  Edits outside it are minimal, additive and listed in your final report.
- **Code signing.** Every shipped `.exe`, `.dll` and installer is signed (see
  [build docs](docs/spec/08-build-test-ci.md#code-signing)).

## How to work

- Work toward the exit criteria of the active roadmap phase. Do not start work
  from a later phase without the maintainer's decision.
- Every behavior change comes with tests that exercise the production path.
  Mocks and synthetic devices live only in tests, never in shipped behavior.
- Measure before claiming a performance gain. Put numbers in the PR.
- A decision that changes architecture, adds a dependency, or adds an
  application-visible default gets an ADR in `docs/decisions/` (next free
  number, one page, status line, context, decision, consequences).
- Report honestly at the end of a task: what was verified and how, what was
  not verified, what remains, every edit outside your module.

## Rust conventions

- Workspace crates are named `andruna-<name>`.
- `unsafe` lives in small, documented wrapper modules. Every `unsafe` block
  has a `// SAFETY:` comment.
- Every function exported to C or to Android code is `extern "C"` (or
  `extern "sysv64"` where the caller uses the System V ABI), is wrapped in
  `catch_unwind`, and reports errors the way the emulated interface does
  (for example `errno`), never by panicking across the boundary.
- Every `#[repr(C)]` type that crosses the boundary has compile-time size and
  field-offset assertions against the Linux/bionic layout.
- Windows APIs are called through the `windows` crate.
- `cargo fmt`, `cargo clippy -D warnings` and the tests pass before a PR.

## Reviewing

Review scope before correctness: does the application get the operation it
asked for, with an honest result? A violation of the
[compatibility policy](docs/spec/06-compatibility-policy.md) blocks merging.
Then check correctness, resource use, security and tests.
