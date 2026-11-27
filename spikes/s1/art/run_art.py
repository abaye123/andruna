#!/usr/bin/env python3
"""Runs dalvikvm (ART built as Android ELF) in the S1 round 3 runner.

Sets up the guest file tree (default %TEMP%/andruna-s1-root, or S8_ROOT):
the boot class path jars in /apex/com.android.art/javalib, the ICU data in
/apex/com.android.i18n/etc/icu, the test dex files in /data/local/tmp, and
runs

  andruna-spike-s1-art run dalvikvm <ART options> -cp <dex> <class> [args]

Usage: python spikes/s1/art/run_art.py [--mode int|jit|jit0] [--extra opt]...
                                       Hello|Bench [args...]

Inputs: out/s1/java (spikes/s1/java/build_boot.py), out/s1art/x64 (guest
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
JAVA = ROOT / "out" / "s1" / "java"
GUEST = ROOT / "out" / "s1art" / "x64"
RUNNER = Path(__file__).resolve().parent / "target" / "x86_64-pc-windows-gnullvm" / "release" / "andruna-spike-s1-art.exe"
AOSP = ROOT / "third_party" / "aosp"
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
    if dat.exists():
        shutil.copy2(dat, icu / dat.name)
    tmp = root / "data" / "local" / "tmp"
    tmp.mkdir(parents=True, exist_ok=True)
    for d in ("hello.dex", "bench.dex", "faults.dex"):
        if (JAVA / d).exists():
            shutil.copy2(JAVA / d, tmp / d)
    for d in ("data/dalvik-cache/x86_64", "system/framework", "system/bin"):
        (root / d).mkdir(parents=True, exist_ok=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", default="int", choices=["int", "jit", "jit0", "aot"])
    ap.add_argument("--extra", action="append", default=[])
    ap.add_argument("--timeout", type=float, default=300)
    ap.add_argument("cls")
    ap.add_argument("args", nargs="*")
    a = ap.parse_args()
    root = Path(os.environ.get("S8_ROOT", Path(tempfile.gettempdir()) / "andruna-s1-root"))
    setup_root(root)
    bcp = ":".join(f"{JAVALIB}/{j}" for j in BOOT_JARS)
    dex = "/data/local/tmp/" + a.cls.lower() + ".dex"
    opts = [f"-Xbootclasspath:{bcp}", f"-Xbootclasspath-locations:{bcp}",
            "-Ximage:/system/framework/none.art", "-Xnoimage-dex2oat"]
    if a.mode == "int":
        opts += ["-Xint"]
    elif a.mode == "jit":
        opts += ["-Xusejit:true"]
    elif a.mode == "jit0":
        # Compile every method at its first call.
        opts += ["-Xusejit:true", "-Xjitthreshold:0"]
    opts += a.extra
    env = dict(os.environ)
    env["S8_ROOT"] = str(root)
    env.setdefault("S8_LIB_PATH", ";".join(str(GUEST / d) for d in ("aosp", "natives-build", "natives-build/manual", "sysroot/usr/lib", "libcxx-install/lib")))
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
