# 0019 - System properties: a read-only area with Andruna defaults

**Status:** accepted, 2026-11-27

## Context
bionic reads system properties from a shared memory area it maps from
`/dev/__properties__`, written by the property service of `init`. ART,
libbase and the framework read properties from the first instruction of a
process (SDK level, ABI list, debuggability, logging tags). Andruna has no
property service yet (phase 2, sysd), but phase 1 runs ART and its tests,
which need the area to exist. The values are visible to every application,
so the defaults are a maintainer decision (compatibility policy).

## Decision
- andruna-lx serves `/dev/__properties__` itself: a regular, root-owned,
  read-only file in bionic's single-area format (`prop_area`), built once
  per process from Andruna's defaults plus the properties the host process
  defines before guest code starts (`andruna_lx::properties::define`).
- Defaults describe the build and the machine, nothing else:
  `ro.build.version.sdk=37`, `ro.build.version.release=17`,
  `ro.build.version.codename=REL` (the AOSP tag of ADR 0011),
  `ro.build.type=user`, `ro.build.tags=release-keys`, `ro.debuggable=0`,
  `ro.secure=1`, the CPU ABI of the build (`ro.product.cpu.abi`,
  `abilist`, `abilist64`; `abilist32` empty, since no 32-bit code runs),
  and Andruna's own identity `ro.product.brand`, `ro.product.name`,
  `ro.product.device`, `ro.hardware` = `andruna`.
- Manufacturer, model and fingerprint are not set here; they follow the
  device identity decision listed in the compatibility policy (proposed).
- Setting a property fails as when the property service is unreachable,
  until sysd provides the service.

## Consequences
- bionic, libbase and ART find properties in every process from phase 1
  on, with values that do not pretend to be a phone.
- Properties are per process and fixed after start; `WaitForProperty`
  waits time out. Cross-process properties come with sysd, which then
  owns the area and these defaults.
- Changing a default changes what applications see and needs a new ADR.
