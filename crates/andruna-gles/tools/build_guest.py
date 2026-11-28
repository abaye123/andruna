#!/usr/bin/env python3
"""Builds the guest side of andruna-gles as Android ELF (ADR 0013).

Output in out/gles/<arch>/guest of the repository:

  libEGL.so libGLESv1_CM.so libGLESv2.so libGLESv3.so libandruna_window.so
      the stub libraries (crates/andruna-gles/guest/lib*.c), freestanding
  gles_test
      crates/andruna-gles/tests/guest/gles_test.c, freestanding (no libc):
      the loader calls its main and the exit code names the failed check
  gles_window
      crates/andruna-gles/tests/guest/gles_window.c, freestanding: renders
      into the window a compositor hands to an app process (andruna-app)
  gles_demo
      crates/andruna-gles/tests/guest/gles_demo.c, linked against the guest
      bionic and libc++ sysroot (out/guest/<arch>/sysroot, from
      aosp/tools/build_guest.py, or ANDRUNA_GUEST_SYSROOT); skipped when
      the sysroot is missing

Usage: python crates/andruna-gles/tools/build_guest.py [--arch x64|arm64]

Needs llvm-mingw (ANDRUNA_LLVM_MINGW, default C:/tools/llvm-mingw) and the
ANGLE sources for the Khronos headers (third_party/aosp/external/angle).
"""

import argparse
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
CRATE = ROOT / "crates" / "andruna-gles"
LLVM_MINGW = Path(os.environ.get("ANDRUNA_LLVM_MINGW", "C:/tools/llvm-mingw"))
ANGLE = Path(os.environ.get("ANDRUNA_ANGLE_SRC", ROOT / "third_party" / "aosp" / "external" / "angle"))
TRIPLE = {"x64": "x86_64-linux-android29", "arm64": "aarch64-linux-android29"}
STUBS = ["EGL", "GLESv1_CM", "GLESv2", "GLESv3", "andruna_window"]


def run(cmd):
    cmd = [str(c) for c in cmd]
    print("+", " ".join(cmd), flush=True)
    subprocess.run(cmd, check=True)


def clang():
    exe = ".exe" if os.name == "nt" else ""
    return LLVM_MINGW / "bin" / f"clang{exe}"


def builtins(arch):
    if arch == "x64":
        return next((LLVM_MINGW / "lib" / "clang").glob("*/lib/linux/libclang_rt.builtins-x86_64.a"), None)
    p = ROOT / "out" / "guest" / arch / "builtins" / "libclang_rt.builtins.a"
    return p if p.exists() else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arch", default="x64", choices=TRIPLE)
    a = ap.parse_args()
    out = ROOT / "out" / "gles" / a.arch / "guest"
    out.mkdir(parents=True, exist_ok=True)
    base = [clang(), f"--target={TRIPLE[a.arch]}", "-fuse-ld=lld", "-O2", "-g", "-fPIC",
            "-ffreestanding", "-fno-stack-protector", "-Wall", "-Werror"]
    for lib in STUBS:
        run(base + ["-shared", "-nostdlib", f"-Wl,-soname,lib{lib}.so", "-Wl,-z,max-page-size=16384",
                    CRATE / "guest" / f"lib{lib}.c", "-o", out / f"lib{lib}.so"])
    tests = CRATE / "tests" / "guest"
    inc = ["-I", ANGLE / "include", "-DGL_GLES_PROTOTYPES=1"]
    run(base + inc + ["-fPIE", "-pie", "-nostdlib", "-Wl,--export-dynamic-symbol=main", "-Wl,--entry=main",
                      "-Wl,--dynamic-linker=/system/bin/linker64", "-Wl,--allow-shlib-undefined",
                      tests / "gles_test.c", "-o", out / "gles_test", f"-L{out}",
                      "-lEGL", "-lGLESv2", "-landruna_window"])
    # GL in an app process (andruna-app's tests/gl.rs), freestanding too.
    run(base + inc + ["-fPIE", "-pie", "-nostdlib", "-Wl,--export-dynamic-symbol=main", "-Wl,--entry=main",
                      "-Wl,--dynamic-linker=/system/bin/linker64", "-Wl,--allow-shlib-undefined",
                      tests / "gles_window.c", "-o", out / "gles_window", f"-L{out}",
                      "-lEGL", "-lGLESv2", "-landruna_window"])
    sysroot = Path(os.environ.get("ANDRUNA_GUEST_SYSROOT", ROOT / "out" / "guest" / a.arch / "sysroot"))
    lib = sysroot / "usr" / "lib"
    rt = builtins(a.arch)
    if not (lib / "crtbegin_dynamic.o").exists() or rt is None:
        print(f"gles_demo skipped: no guest sysroot at {sysroot}")
        return 0
    demo = [clang(), f"--target={TRIPLE[a.arch]}", "-fuse-ld=lld", "-O2", "-g", "-fPIE", "-pie",
            f"--sysroot={sysroot}", "-nostdlib", "-Wall", "-Werror"] + inc
    run(demo + [lib / "crtbegin_dynamic.o", tests / "gles_demo.c",
                "-Wl,--dynamic-linker=/system/bin/linker64", "-Wl,--allow-shlib-undefined", f"-L{out}", f"-L{lib}",
                "-lEGL", "-lGLESv2", "-landruna_window", "-lm", "-lc", "-ldl", "-landruna_host",
                rt, lib / "crtend_android.o", "-o", out / "gles_demo"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
