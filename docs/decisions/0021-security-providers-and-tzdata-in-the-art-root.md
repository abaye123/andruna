# 0021 - Security providers and time zone data in the ART root

**Status:** proposed, 2026-11-27

## Context
On a device the boot class path holds, besides the ART module's core
libraries, Bouncy Castle (ART module) and Conscrypt (the
`com.android.conscrypt` APEX, with `libjavacrypto.so`, `libcrypto.so` and
`libssl.so` from BoringSSL). libcore's `security.properties` names both as
providers, and `sun.security.jca.Providers` fails its class initialization
when one does not load: no `SecureRandom`, `MessageDigest`,
`UUID.randomUUID`, temporary files. Time zones come from the
`com.android.tzdata` APEX (`etc/tz/versioned/<format>/`), which ICU4C and
libcore read through `ANDROID_TZDATA_ROOT`; without it every zone is GMT.
These are application-visible: the set of providers and algorithms, the
default provider order, and the time zone rules an app sees.

Phase 1 has one directory for runtime files, the ART root that
`build_guest.py art` stages and andruna-app mounts at
`/apex/com.android.art`, with one library search path (its `lib64`).

## Decision
- The boot class path is Android's for these modules, in Android's order:
  `core-oj`, `core-libart`, `core-icu4j`, `bouncycastle`, `conscrypt`, all
  built from the pinned AOSP tag (Bouncy Castle and Conscrypt from their
  checked-in repackaged sources). The provider list is libcore's
  `security.properties`, unchanged.
- For phase 1 they live in the ART root: the jars in `javalib`
  (guest `/apex/com.android.art/javalib`), Conscrypt's three shared
  objects in `lib64`. Separate `/apex/com.android.conscrypt` and
  `/apex/com.android.i18n` trees come with the package manager's APEX
  layout (phase 2); only the guest paths of the jars change then.
- BoringSSL is built without `BORINGSSL_FIPS`: `libcrypto.so` is built from
  the sources of Android's `libcrypto_static` (no power-on self tests, no
  module integrity hash). Android's shared `libcrypto` is the FIPS build;
  Andruna makes no FIPS claim.
- The time zone module's files are staged from `system/timezone`'s
  `output_data` at the pinned tag into `<ART root>/tzdata` and mounted at
  `/apex/com.android.tzdata`, in the format 10 layout Android installs.
  Time zone updates follow AOSP tag updates until a module update path
  exists.
- Conscrypt's statistics atoms go nowhere: its unbundled `ConscryptStatsLog`
  (reflection to `android.util.StatsEvent`, absent here) is used instead of
  the generated one, as there is no statsd.

## Consequences
- Cryptography, secure random numbers, digests and TLS primitives work in
  every process, with the same providers and algorithm names as Android.
- The boot image covers five jars; its size and compile time grow
  (numbers in docs/plan/phase1/libcore-tests.md).
- Moving Conscrypt to its own APEX directory later changes the guest paths
  in `BOOTCLASSPATH`, which apps can read; acceptable before phase 2.
- A FIPS build, if ever wanted, needs the BCM object with the integrity hash
  (`bssl_inject_hash`), a separate decision.
