#!/usr/bin/env python3
"""Builds the guest side of spike S8: AOSP code as Android ELF.

Steps (all output in out/s8/<arch>/ of the repository):

  sysroot  bionic's headers laid out as an NDK-style sysroot
  libc     the guest libc shim (libc.so, crtbegin_so.o) from guest/libc
  libcxx   libc++ and libc++abi from LLVM's runtimes build (libc++.so)
  aosp     liblog, fmtlib, libbase (static, -fPIC) with aosp/CMakeLists.txt
           and the ELF toolchain, then guest/CMakeLists.txt links them into
           liblog.so, libbase.so, and the libbase gtest binary
           libbase_test.so

Usage: python spikes/s8/guest/build.py [--arch x64|arm64] [step...]

Inputs: third_party/aosp (cargo xtask fetch), out/s8/llvm-project (a sparse
checkout of llvm-project at the tag of the llvm-mingw clang, see S8.md),
llvm-mingw (ANDRUNA_LLVM_MINGW, default C:/tools/llvm-mingw), CMake and
Ninja on PATH.
"""

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
GUEST = Path(__file__).resolve().parent
AOSP = ROOT / "third_party" / "aosp"
LLVM_PROJECT = ROOT / "out" / "s8" / "llvm-project"
LLVM_MINGW = Path(os.environ.get("ANDRUNA_LLVM_MINGW", "C:/tools/llvm-mingw"))
TOOLCHAIN = ROOT / "aosp" / "cmake" / "toolchain-android-elf.cmake"

ARCH = {
    "x64": {"bionic": "x86_64", "asm": "asm-x86", "triple": "x86_64-linux-android"},
    "arm64": {"bionic": "arm64", "asm": "asm-arm64", "triple": "aarch64-linux-android"},
}


def find_tool(name, env, candidates):
    if os.environ.get(env):
        return os.environ[env]
    found = shutil.which(name)
    if found:
        return found
    for c in candidates:
        if Path(c).exists():
            return str(c)
    sys.exit(f"{name} not found (set {env})")


LOCAL = os.environ.get("LOCALAPPDATA", "")
CMAKE = find_tool("cmake", "ANDRUNA_CMAKE", ["C:/Program Files/CMake/bin/cmake.exe"])
NINJA = find_tool("ninja", "ANDRUNA_NINJA", [f"{LOCAL}/Microsoft/WinGet/Links/ninja.exe"])


def run(cmd, **kw):
    cmd = [str(c) for c in cmd]
    if cmd[0] == "cmake":
        cmd[0] = CMAKE
    print("+", " ".join(cmd), flush=True)
    subprocess.run(cmd, check=True, **kw)


def copytree(src: Path, dst: Path):
    shutil.copytree(src, dst, dirs_exist_ok=True)


def step_sysroot(arch, out):
    """bionic's headers in the layout clang's Android driver expects."""
    a = ARCH[arch]
    inc = out / "sysroot" / "usr" / "include"
    libc = AOSP / "bionic" / "libc"
    if (inc / "stdio.h").exists():
        return
    copytree(libc / "include", inc)
    copytree(libc / "kernel" / "uapi", inc)
    copytree(libc / "kernel" / "android" / "uapi", inc)
    # asm/ is per architecture (usr/include/<triple>/asm in the NDK).
    asm = inc / a["triple"] / "asm"
    copytree(libc / "kernel" / "uapi" / a["asm"] / "asm", asm)
    for d in ("asm-x86", "asm-arm", "asm-arm64", "asm-riscv"):
        shutil.rmtree(inc / d, ignore_errors=True)
    (out / "sysroot" / "usr" / "lib").mkdir(parents=True, exist_ok=True)


def builtins(arch):
    """The compiler runtime archive linked into every guest object."""
    if arch == "x64":
        return next((LLVM_MINGW / "lib" / "clang").glob("*/lib/linux/libclang_rt.builtins-x86_64.a"))
    return ROOT / "out" / "s8" / arch / "builtins" / "libclang_rt.builtins.a"


def cmake_common(arch, out):
    return [
        "-G", "Ninja",
        f"-DCMAKE_MAKE_PROGRAM={NINJA}",
        f"-DCMAKE_TOOLCHAIN_FILE={TOOLCHAIN}",
        f"-DANDRUNA_ARCH={arch}",
        f"-DANDRUNA_LLVM_MINGW={LLVM_MINGW}",
        f"-DANDRUNA_ELF_SYSROOT={out / 'sysroot'}",
        "-DCMAKE_BUILD_TYPE=Release",
        f"-DANDRUNA_ELF_BUILTINS={builtins(arch)}",
    ]


def step_libc(arch, out):
    b = out / "libc"
    run(["cmake", "-S", GUEST / "libc", "-B", b] + cmake_common(arch, out))
    run(["cmake", "--build", b])
    lib = out / "sysroot" / "usr" / "lib"
    for f in ("libc.so", "libandruna_host.so", "crtbegin_so.o"):
        src = next(b.rglob(f))
        shutil.copy2(src, lib / f)


def step_libcxx(arch, out):
    b = out / "libcxx"
    lib = out / "sysroot" / "usr" / "lib"
    flags = "-g -funwind-tables"
    libs = f"{lib / 'crtbegin_so.o'} -L{lib} -lc -landruna_host {builtins(arch)}"
    run(["cmake", "-S", LLVM_PROJECT / "runtimes", "-B", b] + cmake_common(arch, out) + [
        "-DLLVM_ENABLE_RUNTIMES=libcxx;libcxxabi",
        f"-DCMAKE_INSTALL_PREFIX={out / 'libcxx-install'}",
        f"-DCMAKE_C_FLAGS={flags}",
        f"-DCMAKE_CXX_FLAGS={flags}",
        f"-DCMAKE_CXX_STANDARD_LIBRARIES={libs}",
        f"-DCMAKE_C_STANDARD_LIBRARIES={libs}",
        "-DLIBCXX_ENABLE_SHARED=ON",
        "-DLIBCXX_ENABLE_STATIC=OFF",
        "-DLIBCXX_ENABLE_EXCEPTIONS=OFF",
        "-DLIBCXX_ENABLE_RTTI=ON",
        "-DLIBCXX_CXX_ABI=libcxxabi",
        "-DLIBCXX_ENABLE_STATIC_ABI_LIBRARY=ON",
        "-DLIBCXX_HAS_PTHREAD_LIB=OFF",
        "-DLIBCXX_HAS_RT_LIB=OFF",
        "-DLIBCXX_HAS_ATOMIC_LIB=OFF",
        "-DLIBCXX_HAS_GCC_LIB=OFF",
        "-DLIBCXX_HAS_GCC_S_LIB=OFF",
        "-DLIBCXX_HAS_C_LIB=OFF",
        "-DLIBCXXABI_HAS_GCC_S_LIB=OFF",
        "-DLIBCXXABI_HAS_GCC_LIB=OFF",
        "-DLIBCXXABI_HAS_C_LIB=OFF",
        "-DLIBCXXABI_HAS_DL_LIB=OFF",
        "-DLIBCXX_USE_COMPILER_RT=OFF",
        "-DLIBCXX_INCLUDE_TESTS=OFF",
        "-DLIBCXX_INCLUDE_BENCHMARKS=OFF",
        "-DLIBCXX_ENABLE_FILESYSTEM=ON",
        "-DLIBCXX_ENABLE_TIME_ZONE_DATABASE=OFF",
        "-DLIBCXXABI_ENABLE_SHARED=OFF",
        "-DLIBCXXABI_ENABLE_STATIC=ON",
        "-DLIBCXXABI_ENABLE_EXCEPTIONS=OFF",
        "-DLIBCXXABI_USE_LLVM_UNWINDER=OFF",
        "-DLIBCXXABI_HAS_PTHREAD_LIB=OFF",
        "-DLIBCXXABI_INCLUDE_TESTS=OFF",
        "-DLIBCXXABI_USE_COMPILER_RT=OFF",
    ])
    run(["cmake", "--build", b])
    run(["cmake", "--install", b])
    for so in (out / "libcxx-install" / "lib").glob("libc++.so*"):
        if so.is_file() and not so.is_symlink():
            shutil.copy2(so, lib / so.name)


def step_aosp(arch, out):
    b = out / "aosp"
    run(["cmake", "-S", GUEST, "-B", b] + cmake_common(arch, out) + [
        f"-DANDRUNA_AOSP_DIR={AOSP}",
        f"-DANDRUNA_LIBCXX_DIR={out / 'libcxx-install'}",
    ])
    run(["cmake", "--build", b] + (["--", "-k", "0"] if os.environ.get("KEEP_GOING") else []))


STEPS = {"sysroot": step_sysroot, "libc": step_libc, "libcxx": step_libcxx, "aosp": step_aosp}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arch", default="x64", choices=ARCH)
    ap.add_argument("steps", nargs="*", default=list(STEPS))
    args = ap.parse_args()
    out = ROOT / "out" / "s8" / args.arch
    out.mkdir(parents=True, exist_ok=True)
    for s in args.steps:
        STEPS[s](args.arch, out)


if __name__ == "__main__":
    sys.exit(main())
