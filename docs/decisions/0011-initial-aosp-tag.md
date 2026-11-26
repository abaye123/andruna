# 0011 - Initial AOSP release tag

**Status:** proposed, 2026-11-26

## Context
AOSP is fetched at one pinned release tag (open question 12). Every project
in `aosp/aosp.lock` must exist at that tag, and the tag should be recent so
the framework matches what current apps target. On 2026-11-26 the newest
`android-<version>_r<n>` tag on `platform/art` is `android-17.0.0_r1`
(the previous line ends at `android-16.0.0_r4`). `git ls-remote` confirms
the tag on all 20 projects in the initial lock (ART, libcore,
libnativehelper, bionic, system/core, system/libbase, system/logging and
their external dependencies).

## Decision
Use `android-17.0.0_r1` as the initial tag. Each project's commit is
recorded in `aosp/aosp.lock`, and `cargo xtask fetch` refuses a checkout
that does not match.

## Consequences
- Andruna targets Android 17 behavior and API level from phase 1.
- An `_r1` tag is the first release of its line; later `_rN` tags of the
  same version carry fixes. Moving to one is a lock update (tag, all
  commits) plus a refresh of every patch, done in one change.
- If spike S1 finds that this tag needs build tooling the project cannot
  provide, the fallback is the newest `android-16.0.0_rN` tag, verified the
  same way.
