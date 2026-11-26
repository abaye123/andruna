# 0001 - AOSP's runtime and framework, bridged to Windows by Rust code

**Status:** accepted, 2026-11-26

## Context
Android apps can run on a PC in three ways: a virtual machine with a full
Android system, a reimplementation of the Android framework on another
runtime, or Android's own runtime and framework built for the host OS. A
virtual machine is heavy and poorly integrated with the desktop. A
reimplementation diverges from Android's behavior in many small ways that
apps depend on.

## Decision
Build AOSP's ART, libcore, framework.jar, HWUI and Skia for Windows, and
replace only the layers that talk to the Linux kernel and phone hardware.
The replacement layers (Linux interfaces, ELF linker, Binder, system
services, compositor, launcher, tooling) are new code written in Rust.

## Consequences
- Apps see Android's real framework behavior.
- The hard work is the Linux interface layer, calling conventions and
  Binder; spikes S1 to S4 come first.
- AOSP tag upgrades are a recurring task.
