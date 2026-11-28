# 0023 - Building framework.jar without Soong

**Status:** proposed, 2026-11-28

## Context
framework.jar (Soong's `framework-minus-apex`) is compiled from
frameworks/base's `framework-non-updatable-sources` plus about 60 static
libraries and generated sources from some 40 other projects: AIDL of HALs
and native services, aconfig flag classes, proto constants
(protoc-gen-javastream), nano protos, statsd logging classes
(stats-log-api-gen), sysprop classes, R.java from framework-res, and the
APIs of the mainline modules (DeviceConfig, Connectivity, Bluetooth, ...),
which are separate jars on Android. Andruna has no Soong and fetches only
some of those projects. Most generators are C++ protoc plugins or host tools.

## Decision (proposed)
- `aosp/framework/build.py` builds framework.jar, framework-location.jar,
  framework-telecom.jar and framework-res.apk. The source set is Soong's,
  read from the Android.bp files with aosp/tools/blueprint.py.
- Directories of large projects come from `aosp/framework/sources.toml`
  (gitiles archives at the commit of the pinned tag, into out/); whole
  projects stay in aosp.lock.
- Generators are rewritten in Python where the original is a protoc plugin
  or host program: javastream (same output as protoc-gen-javastream),
  statslog (stats-log-api-gen without per-field annotations), sysprop,
  logtags, nano protos, aconfig. aidl is built from source (ADR 0026).
- Every aconfig flag is disabled (its declared default), also for flag
  packages declared in projects that are not fetched, whose flag set is
  taken from the framework's uses.
- APIs of modules that are their own jars on Android are compile-only
  stubs generated from their real sources (`stubgen.py`), never packaged.
- What still cannot be compiled (a dependency that is not built: a module's
  internals, HIDL interfaces, AIDL of projects not fetched) is left out by
  `repair.py` at member granularity: the method throws
  `UnsupportedOperationException("Andruna: not built: <reason>")`, a field
  initializer, member or type that cannot compile is removed. Every change
  is listed in out/framework/report.json. The list shrinks as dependencies
  are built; it is reviewed like a patch set.
- aapt2 and d8 are the Android SDK build-tools 37 binaries build_boot.py
  already downloads (build-time only, not shipped) until aapt2 is built from
  frameworks/base/tools/aapt2 like aidl.

## Consequences
- framework.jar builds on Windows from the pinned sources in about ten
  minutes, with a precise, generated inventory of what is not built.
- Calls into a not-built part fail loudly instead of pretending: an
  honest "unavailable" (compatibility policy, rule 2).
- The repair step is new build machinery between AOSP and the jar; it must
  shrink to nothing for the parts applications use, and it needs review:
  a member it removes changes the framework's API.
- The SDK build-tools license is not OSI: replacing aapt2 and d8 with
  binaries built from source is a follow-up.
