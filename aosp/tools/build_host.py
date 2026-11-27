#!/usr/bin/env python3
"""Builds the host tools of the framework build as native Windows programs
with llvm-mingw (aosp/host, ADR 0023): aidl.

Usage: python aosp/tools/build_host.py [--arch x64|arm64] [target...]

Output: out/host/<arch>/bin/<tool>.exe. The architecture defaults to the
machine's. Build-only tools are fetched into out/tools (never shipped,
ADR 0006 amendment on build tools):

  win_flex_bison 2.5.25   bison 3.8.2 (GPL-3.0-or-later with the Bison
                          exception for generated parsers) and flex 2.6.4
                          (BSD-2-Clause), Windows builds of both
"""

import argparse
import hashlib
import os
import platform
import shutil
import subprocess
import sys
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
AOSP = Path(os.environ.get("ANDRUNA_AOSP_DIR", ROOT / "third_party" / "aosp"))
LLVM_MINGW = Path(os.environ.get("ANDRUNA_LLVM_MINGW", "C:/tools/llvm-mingw"))
TOOLS = ROOT / "out" / "tools"

FLEX_BISON_URL = ("https://github.com/lexxmark/winflexbison/releases/download/"
                  "v2.5.25/win_flex_bison-2.5.25.zip")
FLEX_BISON_SHA256 = "8d324b62be33604b2c45ad1dd34ab93d722534448f55a16ca7292de32b6ac135"
FLEX_BISON_DIR = TOOLS / "win_flex_bison-2.5.25"

TOOLS_BUILT = ("aidl",)


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
    print("+", " ".join(cmd), flush=True)
    subprocess.run(cmd, check=True, **kw)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def fetch_zip(url, digest, dest):
    """Downloads and checks a zip, extracts it into its own empty directory."""
    if dest.is_dir():
        return
    TOOLS.mkdir(parents=True, exist_ok=True)
    z = TOOLS / url.rsplit("/", 1)[1]
    if not z.is_file() or sha256(z) != digest:
        print(f"downloading {url}", flush=True)
        urllib.request.urlretrieve(url, z)
    got = sha256(z)
    if got != digest:
        sys.exit(f"{z}: sha256 {got}, expected {digest}")
    tmp = dest.with_name(dest.name + ".partial")
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir(parents=True)
    with zipfile.ZipFile(z) as zf:
        for name in zf.namelist():
            p = (tmp / name).resolve()
            if not str(p).startswith(str(tmp.resolve()) + os.sep) and p != tmp.resolve():
                sys.exit(f"{z}: unsafe path {name}")
        zf.extractall(tmp)
    tmp.rename(dest)


def host_arch():
    m = platform.machine().lower()
    return "arm64" if m in ("arm64", "aarch64") else "x64"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arch", default=host_arch(), choices=("x64", "arm64"))
    ap.add_argument("targets", nargs="*", default=list(TOOLS_BUILT))
    args = ap.parse_args()

    fetch_zip(FLEX_BISON_URL, FLEX_BISON_SHA256, FLEX_BISON_DIR)
    out = ROOT / "out" / "host" / args.arch
    b = out / "build"
    run([CMAKE, "-S", ROOT / "aosp" / "host", "-B", b, "-G", "Ninja",
         f"-DCMAKE_MAKE_PROGRAM={NINJA}",
         f"-DCMAKE_TOOLCHAIN_FILE={ROOT / 'aosp' / 'cmake' / 'toolchain-llvm-mingw.cmake'}",
         f"-DANDRUNA_ARCH={args.arch}",
         f"-DANDRUNA_LLVM_MINGW={LLVM_MINGW}",
         f"-DANDRUNA_AOSP_DIR={AOSP}",
         f"-DANDRUNA_BISON={FLEX_BISON_DIR / 'win_bison.exe'}",
         f"-DANDRUNA_FLEX={FLEX_BISON_DIR / 'win_flex.exe'}",
         "-DCMAKE_BUILD_TYPE=Release"])
    jobs = os.environ.get("CMAKE_BUILD_PARALLEL_LEVEL", "4")
    run([CMAKE, "--build", b, "-j", jobs, "--target"] + args.targets)
    (out / "bin").mkdir(parents=True, exist_ok=True)
    for t in args.targets:
        shutil.copy2(b / f"{t}.exe", out / "bin" / f"{t}.exe")
        print(f"{out / 'bin' / (t + '.exe')}")


if __name__ == "__main__":
    sys.exit(main())
