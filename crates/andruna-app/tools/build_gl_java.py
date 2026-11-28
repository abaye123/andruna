#!/usr/bin/env python3
"""Builds the Java GL test program of andruna-app (tests/gl.rs).

Output in out/app/<arch>/gl of the repository:

  gljava.dex    crates/andruna-app/tests/java/GlMain.java (javac, then d8)
  libgljni.so   crates/andruna-app/tests/java/gljni.c, Android ELF linked
                against the guest bionic sysroot and the guest GLES stubs

Usage: python crates/andruna-app/tools/build_gl_java.py [--arch x64|arm64]

Needs: a JDK (javac, java), the boot class path classes and d8 from
spikes/s1/java/build_boot.py (out/s1/java/classes/boot, out/s1/tools),
the guest sysroot (out/guest/<arch>/sysroot, aosp/tools/build_guest.py),
the guest GLES stubs (out/gles/<arch>/guest, andruna-gles's
tools/build_guest.py), llvm-mingw (ANDRUNA_LLVM_MINGW, default
C:/tools/llvm-mingw), and the headers of ANGLE (ANDRUNA_ANGLE_SRC) and of
libnativehelper (ANDRUNA_JNI_INCLUDE, default
third_party/aosp/libnativehelper/include_jni).
"""

import argparse
import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
CRATE = ROOT / "crates" / "andruna-app"
LLVM_MINGW = Path(os.environ.get("ANDRUNA_LLVM_MINGW", "C:/tools/llvm-mingw"))
ANGLE = Path(os.environ.get("ANDRUNA_ANGLE_SRC", ROOT / "third_party" / "aosp" / "external" / "angle"))
JNI = Path(os.environ.get("ANDRUNA_JNI_INCLUDE",
                          ROOT / "third_party" / "aosp" / "libnativehelper" / "include_jni"))
S1 = Path(os.environ.get("ANDRUNA_S1_OUT", ROOT / "out" / "s1"))
TRIPLE = {"x64": "x86_64-linux-android29", "arm64": "aarch64-linux-android29"}


def run(cmd):
    cmd = [str(c) for c in cmd]
    print("+", " ".join(cmd), flush=True)
    subprocess.run(cmd, check=True)


def builtins(arch):
    if arch == "x64":
        return next((LLVM_MINGW / "lib" / "clang").glob("*/lib/linux/libclang_rt.builtins-x86_64.a"), None)
    p = ROOT / "out" / "guest" / arch / "builtins" / "libclang_rt.builtins.a"
    return p if p.exists() else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arch", default="x64", choices=TRIPLE)
    a = ap.parse_args()
    out = ROOT / "out" / "app" / a.arch / "gl"
    out.mkdir(parents=True, exist_ok=True)

    # Java: compiled against the boot class path classes, dexed with d8.
    boot = S1 / "java" / "classes" / "boot"
    d8 = S1 / "tools" / "build-tools-37" / "android-37.0" / "lib" / "d8.jar"
    classes = out / "classes"
    if classes.exists():
        shutil.rmtree(classes)
    classes.mkdir()
    run(["javac", "-encoding", "UTF-8", "-source", "21", "-target", "21", "--system=none",
         "--module-path", boot, "-XDstringConcat=inline", "-nowarn", "-d", classes, CRATE / "tests" / "java" / "GlMain.java"])
    jar = out / "gljava-classes.jar"
    with zipfile.ZipFile(jar, "w") as z:
        for f in sorted(classes.glob("*.class")):
            z.write(f, f.name)
    tmp = out / "gljava.d8.jar"
    run(["java", "-cp", d8, "com.android.tools.r8.D8", "--release", "--min-api", "36",
         "--output", tmp, "--lib", S1 / "java" / "classes" / "core-oj-classes.jar", jar])
    with zipfile.ZipFile(tmp) as z:
        (out / "gljava.dex").write_bytes(z.read("classes.dex"))
    tmp.unlink()

    # The JNI library, linked against bionic and the GLES stubs.
    sysroot = Path(os.environ.get("ANDRUNA_GUEST_SYSROOT", ROOT / "out" / "guest" / a.arch / "sysroot"))
    lib = sysroot / "usr" / "lib"
    gles = ROOT / "out" / "gles" / a.arch / "guest"
    rt = builtins(a.arch)
    if not (lib / "crtbegin_so.o").exists() or rt is None:
        print(f"no guest sysroot at {sysroot}", file=sys.stderr)
        return 1
    exe = ".exe" if os.name == "nt" else ""
    run([LLVM_MINGW / "bin" / f"clang{exe}", f"--target={TRIPLE[a.arch]}", "-fuse-ld=lld", "-O2", "-g",
         "-fPIC", "-shared", f"--sysroot={sysroot}", "-nostdlib", "-Wall", "-Werror",
         "-I", ANGLE / "include", "-I", JNI, "-I", ROOT / "crates" / "andruna-gles" / "tests" / "guest",
         "-DGL_GLES_PROTOTYPES=1", "-Wl,-soname,libgljni.so", "-Wl,-z,max-page-size=16384",
         lib / "crtbegin_so.o", CRATE / "tests" / "java" / "gljni.c",
         f"-L{gles}", f"-L{lib}", "-lEGL", "-lGLESv2", "-landruna_window", "-lc", "-ldl",
         rt, lib / "crtend_so.o", "-o", out / "libgljni.so"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
