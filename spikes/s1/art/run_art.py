#!/usr/bin/env python3
"""Runs dalvikvm (ART built as Android ELF) in the S1 round 3 runner.

Sets up the guest file tree (default %TEMP%/andruna-s1-root, or S8_ROOT):
the boot class path jars in /apex/com.android.art/javalib, the ICU data in
/apex/com.android.i18n/etc/icu, the test dex files in /data/local/tmp, and
runs

  andruna-spike-s1-art run dalvikvm <ART options> -cp <dex> <class> [args]

Usage: python spikes/s1/art/run_art.py [--mode int|jit|jit0] [--extra opt]...
                                       Hello|Bench [args...]

Inputs: out/s1/java (spikes/s1/java/build_boot.py), out/s1art/<arch> (guest
build), the runner built with `cargo build --release --target
x86_64-pc-windows-gnullvm` in spikes/s1/art.
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
JAVA = Path(os.environ.get("S1_JAVA", ROOT / "out" / "s1" / "java"))
import platform

# The guest architecture is the host's: x64 or arm64 (S1_ARCH overrides).
ARCH = os.environ.get("S1_ARCH", "arm64" if platform.machine().lower() in ("arm64", "aarch64") else "x64")
ISA = {"x64": "x86_64", "arm64": "arm64"}[ARCH]
GUEST = Path(os.environ.get("S1_GUEST", ROOT / "out" / "s1art" / ARCH))
TARGET = {"x64": "x86_64-pc-windows-gnullvm", "arm64": "aarch64-pc-windows-gnullvm"}[ARCH]
RUNNER = Path(os.environ.get("S1_RUNNER", Path(__file__).resolve().parent / "target" / TARGET / "release" / "andruna-spike-s1-art.exe"))
AOSP = Path(os.environ.get("S1_AOSP", ROOT / "third_party" / "aosp"))
BOOT_JARS = ["core-oj.jar", "core-libart.jar", "core-icu4j.jar"]
JAVALIB = "/apex/com.android.art/javalib"


def setup_root(root: Path):
    jl = root / JAVALIB.lstrip("/")
    jl.mkdir(parents=True, exist_ok=True)
    for j in BOOT_JARS:
        shutil.copy2(JAVA / j, jl / j)
    icu = root / "apex" / "com.android.i18n" / "etc" / "icu"
    icu.mkdir(parents=True, exist_ok=True)
    dat = AOSP / "external" / "icu" / "icu4c" / "source" / "stubdata" / "icudt78l.dat"
    if not dat.exists():
        # CI: the data file travels with the boot jars.
        dat = JAVA / "icudt78l.dat"
    if dat.exists():
        shutil.copy2(dat, icu / dat.name)
    tmp = root / "data" / "local" / "tmp"
    tmp.mkdir(parents=True, exist_ok=True)
    for d in ("hello.dex", "bench.dex", "faults.dex"):
        if (JAVA / d).exists():
            shutil.copy2(JAVA / d, tmp / d)
    for d in (f"data/dalvik-cache/{ISA}", "system/framework", "system/bin"):
        (root / d).mkdir(parents=True, exist_ok=True)


BOOT_IMAGE = "/system/framework/boot.art"
# ART_BASE_ADDRESS (aosp/modules/libartbase.cmake, S2): below it Windows
# leaves the address space free.
BOOT_BASE = "0x60000000"


def dex2oat_boot_args(bcp):
    """dex2oat options that compile the boot class path into a boot image
    (one image, oat and vdex per jar, as on a device) in
    /system/framework/<isa>."""
    out = f"/system/framework/{ISA}"
    args = ["--runtime-arg", "-Xms64m", "--runtime-arg", "-Xmx512m"]
    for j in BOOT_JARS:
        args += [f"--dex-file={JAVALIB}/{j}", f"--dex-location={JAVALIB}/{j}"]
    args += [f"--image={out}/boot.art", f"--oat-file={out}/boot.oat",
             f"--oat-location={out}/boot.oat", f"--base={BOOT_BASE}",
             f"--instruction-set={ISA}", "--compiler-filter=speed",
             "--image-format=uncompressed", "--android-root=/system",
             "--runtime-arg", f"-Xbootclasspath:{bcp}",
             "--runtime-arg", f"-Xbootclasspath-locations:{bcp}",
             "--strip",
             "--avoid-storing-invocation", "--abort-on-hard-verifier-error",
             "--no-abort-on-soft-verifier-error", "--compilation-reason=s1",
             "-j4"]
    return args


def dex2oat_app_args(bcp, dex):
    """dex2oat options that compile one test dex file against the boot
    image into /data/local/tmp/oat/<isa>/<name>.odex (where ART looks)."""
    name = Path(dex).stem
    return ["--runtime-arg", "-Xms64m", "--runtime-arg", "-Xmx256m",
            f"--dex-file={dex}", f"--dex-location={dex}",
            f"--oat-file=/data/local/tmp/oat/{ISA}/{name}.odex",
            f"--boot-image={BOOT_IMAGE}", "--class-loader-context=PCL[]",
            f"--instruction-set={ISA}", "--compiler-filter=speed",
            "--android-root=/system",
            "--runtime-arg", f"-Xbootclasspath:{bcp}",
            "--runtime-arg", f"-Xbootclasspath-locations:{bcp}",
            "--avoid-storing-invocation", "--compilation-reason=s1"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", default="int", choices=["int", "jit", "jit0", "aot"])
    ap.add_argument("--extra", action="append", default=[])
    ap.add_argument("--timeout", type=float, default=300)
    ap.add_argument("cls", help="test class (Hello, Bench, Faults), or dex2oat-boot, "
                    "or dex2oat:<Class> to compile that class's dex file")
    ap.add_argument("args", nargs="*")
    a = ap.parse_args()
    root = Path(os.environ.get("S8_ROOT", Path(tempfile.gettempdir()) / "andruna-s1-root"))
    setup_root(root)
    (root / "system" / "framework" / ISA).mkdir(parents=True, exist_ok=True)
    (root / "data" / "local" / "tmp" / "oat" / ISA).mkdir(parents=True, exist_ok=True)
    bcp = ":".join(f"{JAVALIB}/{j}" for j in BOOT_JARS)
    env = dict(os.environ)
    env["S8_ROOT"] = str(root)
    env.setdefault("S8_LIB_PATH", ";".join(str(GUEST / d) for d in ("aosp", "natives-build", "natives-build/manual", "sysroot/usr/lib", "libcxx-install/lib")))
    if a.cls == "dex2oat-boot" or a.cls.startswith("dex2oat:"):
        if a.cls == "dex2oat-boot":
            dargs = dex2oat_boot_args(bcp)
        else:
            dargs = dex2oat_app_args(bcp, "/data/local/tmp/" + a.cls.split(":", 1)[1].lower() + ".dex")
        cmd = [str(RUNNER), "run", str(GUEST / "aosp" / "dex2oat")] + dargs + a.extra
    else:
        dex = "/data/local/tmp/" + a.cls.lower() + ".dex"
        opts = [f"-Xbootclasspath:{bcp}", f"-Xbootclasspath-locations:{bcp}"]
        if a.mode == "aot":
            # The boot image and the app's odex from dex2oat; no JIT, so
            # what runs is AOT code (and the interpreter for anything
            # dex2oat did not compile).
            opts += [f"-Ximage:{BOOT_IMAGE}", "-Xnoimage-dex2oat", "-Xusejit:false"]
        else:
            opts += ["-Ximage:/system/framework/none.art", "-Xnoimage-dex2oat"]
        if a.mode == "int":
            opts += ["-Xint"]
        elif a.mode == "jit":
            opts += ["-Xusejit:true"]
        elif a.mode == "jit0":
            # Compile every method at its first call.
            opts += ["-Xusejit:true", "-Xjitthreshold:0"]
        opts += a.extra
        cmd = [str(RUNNER), "run", str(GUEST / "aosp" / "dalvikvm")] + opts + ["-cp", dex, a.cls] + a.args
    print("+", " ".join(cmd), flush=True)
    import time
    t = time.perf_counter()
    try:
        rc = subprocess.run(cmd, env=env, timeout=a.timeout).returncode
    except subprocess.TimeoutExpired:
        print(f"[run_art] timed out after {a.timeout} s", flush=True)
        rc = 124
    print(f"[run_art] exit {rc}, wall time {time.perf_counter() - t:.2f} s", flush=True)
    return rc


if __name__ == "__main__":
    sys.exit(main())
