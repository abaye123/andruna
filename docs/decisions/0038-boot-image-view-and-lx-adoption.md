# 0038 - Boot image view file and adoption by lx

**Status:** proposed, 2026-11-27

## Context
ADR 0035 has sysd map the boot image into each suspended app process (S2).
ART maps the boot image itself: a reservation at the image base
(`0x60000000`), each `.art` image at its `image_begin`, each oat file's
loadable segments at its `oat_file_begin`. Those addresses are 16 KiB
aligned, but a view must start on a 64 KiB boundary at a 64 KiB file
offset, so one view per file is not possible. lx fills file mappings by
reading the file into private memory, so every app holds its own 28 MiB
copy of the image. By default ART also relocates the image to a random
address, which would make every page private anyway.

## Decision
- sysd lays out the boot image files once in a **view file**
  (`cache\boot-image\<isa>-<hash>.img`): the reservation as ART fills it,
  file pages at their addresses, zeros elsewhere. The name carries a hash
  of the files' names, sizes and times, so a changed image gets a new file.
- sysd holds one file-backed section of it and maps one **read-only view**
  (`PAGE_EXECUTE_READ`, no commit charge) at the image base into every app
  process while it is suspended. The process gets no section handle.
- andruna-app registers the view with lx (`andruna_lx::image`). ART's
  mappings that land in it **adopt** the view's pages when the pages hold
  exactly the requested file pages (checked against the extents and the
  first and last page of the file); writable mappings become
  copy-on-write, so only pages ART writes become private. Anything else in
  the region (other files, anonymous memory, pages written before) is
  filled or zeroed as lx does elsewhere.
- With a view, andruna-app passes `-Xnorelocate`: every app uses the image
  at its own address, as all apps forked from one zygote share one address
  on a device.

## Consequences
- Measured (Hello World and an idle app, x64, 15 interleaved runs):
  28.3 MiB of the image adopted and none copied; an idle app has 25.7 MiB
  less private working set and 21 MiB less private commit; start time is
  unchanged within noise; mapping the view takes 0.06-0.2 ms
  ([app lifecycle](../plan/phase2/app-lifecycle.md)).
- The boot image address is the same in every app process (no per-process
  image randomization). Other ASLR is unchanged.
- No ART patch: the adoption is in lx, so the path ART takes is unchanged.
- The vdex files and the image bitmaps are still read per process (mapped
  at addresses ART chooses outside the reservation).
- If ART's layout changes so that a request does not match the view (for
  example 64 KiB segment alignment on arm64), lx falls back to copying,
  which is correct but not shared; the host reports adopted and copied
  sizes at exit.
