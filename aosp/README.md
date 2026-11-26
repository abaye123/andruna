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
apply the same way on every host.

To change the tag: update `tag` and every `commit` together (`git ls-remote
<remote>/<name> 'refs/tags/<tag>^{}'`), delete `third_party/aosp`, fetch, and
refresh the patches in [../patches](../patches). The current tag is proposed
in [ADR 0011](../docs/decisions/0011-initial-aosp-tag.md).

This directory will also hold the CMake targets that mirror the
`Android.bp` modules we build with llvm-mingw.

## Licenses

Only permissively licensed parts of these projects are built and shipped
([ADR 0006](../docs/decisions/0006-open-source-permissive-dependencies.md)).
Some fetched repositories also contain files under other terms (for example
command-line programs or kernel headers), and a few components are public
domain rather than one of the listed licenses (dlmalloc, the LZMA SDK). The
build must not use files under non-permissive terms, and every AOSP module
that ships is recorded in `THIRD_PARTY_NOTICES`.
