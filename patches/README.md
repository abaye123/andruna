# patches

Small patches to AOSP projects, applied by `cargo xtask patch` after
`cargo xtask fetch`.

- Layout: `patches/<project path>/NNNN-short-description.patch`, where
  `<project path>` is the `path` from [../aosp/aosp.lock](../aosp/aosp.lock)
  (for example `patches/system/libbase/0001-...patch`).
- Patches apply in file-name order with `git apply`. A patch that is already
  applied is skipped, so the command is idempotent.
- Create patches with `git format-patch` or `git diff` from inside the
  project checkout in `third_party/aosp/<path>`.
- Every change is gated on `defined(ANDRUNA)` and is upstreamable in spirit
  (see AGENTS.md).
