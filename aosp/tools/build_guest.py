#!/usr/bin/env python3
"""Builds the guest userland as Android ELF (ADR 0013).

Steps (all output in out/guest/<arch>/ of the repository):

  sysroot       bionic's headers laid out as an NDK-style sysroot
  builtins      compiler-rt builtins for arm64 (llvm-mingw ships x86_64's)
  bionic        libc.so, libm.so, libdl.so, ld-android.so (the bridge),
                libandruna_host.so and the crt objects, installed into
                sysroot/usr/lib (aosp/guest, stage bionic)
  libcxx        libc++.so with libc++abi, exceptions and RTTI, from LLVM's
                runtimes build against that sysroot
  aosp          liblog.so, libbase.so and the libbase gtest suite
                (libbase_test), stage aosp
  tests         bionic_tests: a subset of bionic's own gtest suite
  linker-tests  crates/andruna-linker/tests/guest/* (lib<name>.c -> a shared
                object, anything else a PIE; `// LINK: -lfoo` on the first
                line adds libraries)
  art           libart.so, the JNI libraries ART opens at start-up (ICU,
                libcore), dalvikvm and dex2oat, collected in art/out
                (aosp/guest/art.cmake; needs the art and libcore patches;
                not in the default step list)

Usage: python aosp/tools/build_guest.py [--arch x64|arm64] [step...]

Inputs: third_party/aosp (cargo xtask fetch + patch), llvm-project sources
at the tag of the llvm-mingw clang (ANDRUNA_LLVM_PROJECT, default
out/llvm-project; sparse checkout of libcxx libcxxabi libunwind runtimes
cmake llvm/cmake llvm/utils/llvm-lit compiler-rt/lib/builtins libc),
llvm-mingw (ANDRUNA_LLVM_MINGW, default C:/tools/llvm-mingw), CMake and
Ninja.
"""

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
AOSP = Path(os.environ.get("ANDRUNA_AOSP_DIR", ROOT / "third_party" / "aosp"))
LLVM_PROJECT = Path(os.environ.get("ANDRUNA_LLVM_PROJECT", ROOT / "out" / "llvm-project"))
LLVM_MINGW = Path(os.environ.get("ANDRUNA_LLVM_MINGW", "C:/tools/llvm-mingw"))
TOOLCHAIN = ROOT / "aosp" / "cmake" / "toolchain-android-elf.cmake"
GUEST = ROOT / "aosp" / "guest"

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
    line = " ".join(cmd)
    print("+", line if len(line) < 400 else line[:400] + " ...", flush=True)
    subprocess.run(cmd, check=True, **kw)


def copytree(src: Path, dst: Path):
    shutil.copytree(src, dst, dirs_exist_ok=True)


def step_sysroot(arch, out):
    """bionic's headers in the layout clang's Android driver expects."""
    a = ARCH[arch]
    inc = out / "sysroot" / "usr" / "include"
    libc = AOSP / "bionic" / "libc"
    if not (inc / "stdio.h").exists():
        copytree(libc / "include", inc)
        copytree(libc / "kernel" / "uapi", inc)
        copytree(libc / "kernel" / "android" / "uapi", inc)
        # asm/ is per architecture (usr/include/<triple>/asm in the NDK).
        copytree(libc / "kernel" / "uapi" / a["asm"] / "asm", inc / a["triple"] / "asm")
        for d in ("asm-x86", "asm-arm", "asm-arm64", "asm-riscv"):
            shutil.rmtree(inc / d, ignore_errors=True)
    (out / "sysroot" / "usr" / "lib").mkdir(parents=True, exist_ok=True)


def builtins(arch, out):
    """The compiler runtime archive linked into every guest object."""
    if arch == "x64":
        return next((LLVM_MINGW / "lib" / "clang").glob("*/lib/linux/libclang_rt.builtins-x86_64.a"))
    return out / "builtins" / "libclang_rt.builtins.a"


def cmake_common(arch, out):
    return [
        "-G", "Ninja",
        f"-DCMAKE_MAKE_PROGRAM={NINJA}",
        f"-DCMAKE_TOOLCHAIN_FILE={TOOLCHAIN}",
        f"-DANDRUNA_ARCH={arch}",
        f"-DANDRUNA_LLVM_MINGW={LLVM_MINGW}",
        f"-DANDRUNA_ELF_SYSROOT={out / 'sysroot'}",
        "-DCMAKE_BUILD_TYPE=Release",
        f"-DANDRUNA_ELF_BUILTINS={builtins(arch, out)}",
    ]


def keep_going():
    return ["--", "-k", "0"] if os.environ.get("KEEP_GOING") else []


def stage(arch, out, name, extra=()):
    b = out / name
    run(["cmake", "-S", GUEST, "-B", b] + cmake_common(arch, out) + [
        f"-DANDRUNA_GUEST_STAGE={name}",
        f"-DANDRUNA_AOSP_DIR={AOSP}",
        f"-DANDRUNA_LLVM_PROJECT={LLVM_PROJECT}",
        f"-DANDRUNA_LIBCXX_DIR={out / 'libcxx-install'}",
        f"-DCMAKE_INSTALL_PREFIX={out / 'sysroot' / 'usr'}",
    ] + list(extra))
    run(["cmake", "--build", b] + keep_going())
    return b


def step_bionic(arch, out):
    b = stage(arch, out, "bionic")
    run(["cmake", "--install", b])


def step_libcxx(arch, out):
    b = out / "libcxx"
    lib = out / "sysroot" / "usr" / "lib"
    flags = "-g -O2 -funwind-tables -fstack-protector-strong"
    libs = f"{lib / 'crtbegin_so.o'} -L{lib} -lc -lm -ldl -landruna_host {builtins(arch, out)} {lib / 'crtend_so.o'}"
    run(["cmake", "-S", LLVM_PROJECT / "runtimes", "-B", b] + cmake_common(arch, out) + [
        "-DLLVM_ENABLE_RUNTIMES=libcxx;libcxxabi",
        f"-DCMAKE_INSTALL_PREFIX={out / 'libcxx-install'}",
        f"-DCMAKE_C_FLAGS={flags}",
        f"-DCMAKE_CXX_FLAGS={flags}",
        f"-DCMAKE_CXX_STANDARD_LIBRARIES={libs}",
        f"-DCMAKE_C_STANDARD_LIBRARIES={libs}",
        "-DLIBCXX_ENABLE_SHARED=ON",
        "-DLIBCXX_ENABLE_STATIC=OFF",
        "-DLIBCXX_ENABLE_EXCEPTIONS=ON",
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
        "-DLIBCXXABI_ENABLE_EXCEPTIONS=ON",
        # _Unwind_* come from libc.so (LLVM libunwind linked in and
        # exported, as Android's libunwind-exported).
        "-DLIBCXXABI_USE_LLVM_UNWINDER=OFF",
        "-DLIBCXXABI_HAS_PTHREAD_LIB=OFF",
        "-DLIBCXXABI_INCLUDE_TESTS=OFF",
        "-DLIBCXXABI_USE_COMPILER_RT=OFF",
    ])
    run(["cmake", "--build", b])
    run(["cmake", "--install", b])
    # The real file under every name objects use: its soname libc++.so.1
    # (DT_NEEDED of everything linked with -lc++) and libc++.so (the link
    # name), as plain copies since symbolic links are not portable here.
    real = next(p for p in (out / "libcxx-install" / "lib").glob("libc++.so*")
                if p.is_file() and not p.is_symlink())
    for name in ("libc++.so", "libc++.so.1"):
        shutil.copy2(real, lib / name)


def step_aosp(arch, out):
    stage(arch, out, "aosp")


def step_tests(arch, out):
    stage(arch, out, "tests")


def step_linker_tests(arch, out):
    stage(arch, out, "linker-tests",
          [f"-DANDRUNA_LINKER_TESTS_DIR={ROOT / 'crates' / 'andruna-linker' / 'tests' / 'guest'}"])


BOOT_JARS = ("core-oj.jar", "core-libart.jar", "core-icu4j.jar")


def step_art(arch, out):
    """ART, then the ART root andruna-app runs (out/guest/<arch>/art-root):
    lib64 (bionic, libc++, ART's libraries), bin (dalvikvm, dex2oat),
    javalib (the boot class path jars from ANDRUNA_BOOT_JARS, default
    out/s1/java, built by spikes/s1/java/build_boot.py) and icu (ICU's
    data file)."""
    b = stage(arch, out, "art")
    if arch == "x64":
        check_no_gs_access(b / "out")
    root = out / "art-root"
    for d in ("lib64", "bin", "javalib", "icu"):
        (root / d).mkdir(parents=True, exist_ok=True)
    sysroot_lib = out / "sysroot" / "usr" / "lib"
    for so in sysroot_lib.glob("*.so*"):
        if so.name != "libandruna_host.so" and so.is_file():
            shutil.copy2(so, root / "lib64" / so.name)
    for f in (b / "out").iterdir():
        dst = root / ("bin" if f.name in ("dalvikvm", "dex2oat") else "lib64") / f.name
        shutil.copy2(f, dst)
    jars = Path(os.environ.get("ANDRUNA_BOOT_JARS", ROOT / "out" / "s1" / "java"))
    for j in BOOT_JARS:
        if (jars / j).exists():
            shutil.copy2(jars / j, root / "javalib" / j)
        else:
            print(f"warning: {jars / j} missing (spikes/s1/java/build_boot.py)")
    dat = next((AOSP / "external" / "icu" / "icu4c" / "source" / "stubdata").glob("icudt*l.dat"), None)
    if dat:
        shutil.copy2(dat, root / "icu" / dat.name)
    print(f"ART root: {root}")


# x86_64 nterp still uses %gs (patches/art/0003 keeps it disabled).
NTERP_PREFIXES = ("nterp_", "Nterp", "ExecuteNterp", "artNterp", "EndExecuteNterp")


def check_no_gs_access(lib_dir):
    """On Windows x64 GS is the TEB, so an %gs: access in ART's x86_64 code
    reads the TEB instead of the Thread (ADR 0015). Fails the build when one
    is left outside nterp."""
    objdump = LLVM_MINGW / "bin" / "llvm-objdump.exe"
    found = []
    for lib in sorted(lib_dir.glob("*.so")):
        text = subprocess.run([objdump, "-d", "--no-show-raw-insn", lib],
                              capture_output=True, text=True, check=True).stdout
        symbol = ""
        for line in text.splitlines():
            if line.endswith(">:"):
                symbol = line[line.find("<") + 1:-2]
            elif "%gs:" in line and not symbol.startswith(NTERP_PREFIXES):
                found.append(f"{lib.name}: {symbol}: {line.strip()}")
    if found:
        sys.exit("\n".join(["%gs: access in x86_64 code (GS is the TEB on Windows):"] + found))
    print("no %gs: access outside nterp")


def step_builtins(arch, out):
    """compiler-rt builtins for arm64 Android (llvm-mingw ships them for
    x86_64 Linux only): every generic and aarch64 source that compiles, and
    the outline atomics (`__aarch64_*`) Android code calls by default."""
    if arch == "x64":
        return
    import concurrent.futures as cf
    src = LLVM_PROJECT / "compiler-rt" / "lib" / "builtins"
    obj = out / "builtins" / "obj"
    obj.mkdir(parents=True, exist_ok=True)
    clang = LLVM_MINGW / "bin" / "clang.exe"
    base = [str(clang), f"--target={ARCH[arch]['triple']}10000", f"--sysroot={out / 'sysroot'}",
            "-O2", "-fPIC", "-fno-builtin", "-w", f"-I{src}", "-c"]
    jobs = []
    # cpu_model/aarch64.c defines __aarch64_have_lse_atomics (from
    # getauxval(AT_HWCAP), which the loader provides).
    extra = [src / "cpu_model" / "aarch64.c"]
    for f in sorted(src.glob("*.c")) + sorted((src / "aarch64").glob("*.[cS]")) + extra:
        if f.name == "lse.S":
            continue
        jobs.append((base + [str(f), "-o", str(obj / (f.parent.name + "_" + f.stem + ".o"))], f.name))
    lse = src / "aarch64" / "lse.S"
    for op, sizes in (("cas", (1, 2, 4, 8, 16)), ("swp", (1, 2, 4, 8)), ("ldadd", (1, 2, 4, 8)),
                      ("ldclr", (1, 2, 4, 8)), ("ldeor", (1, 2, 4, 8)), ("ldset", (1, 2, 4, 8))):
        for size in sizes:
            for model in (1, 2, 3, 4):
                o = obj / f"lse_{op}{size}_{model}.o"
                jobs.append((base + [f"-DL_{op}", f"-DSIZE={size}", f"-DMODEL={model}", str(lse), "-o", str(o)], o.name))

    def build(j):
        return j[1], subprocess.run(j[0], capture_output=True).returncode == 0

    with cf.ThreadPoolExecutor(8) as ex:
        results = list(ex.map(build, jobs))
    failed = [n for n, ok in results if not ok]
    print(f"builtins: {len(results) - len(failed)} objects, {len(failed)} sources skipped: {' '.join(failed)}")
    lib = builtins(arch, out)
    if lib.exists():
        lib.unlink()
    objs = [str(p) for p in obj.glob("*.o")]
    run([LLVM_MINGW / "bin" / "llvm-ar.exe", "rcs", lib] + objs)


STEPS = {"sysroot": step_sysroot, "builtins": step_builtins, "bionic": step_bionic,
         "libcxx": step_libcxx, "aosp": step_aosp, "tests": step_tests,
         "linker-tests": step_linker_tests, "art": step_art}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arch", default="x64", choices=ARCH)
    ap.add_argument("steps", nargs="*", default=[s for s in STEPS if s != "art"])
    args = ap.parse_args()
    out = ROOT / "out" / "guest" / args.arch
    out.mkdir(parents=True, exist_ok=True)
    for s in args.steps:
        STEPS[s](args.arch, out)


if __name__ == "__main__":
    sys.exit(main())
