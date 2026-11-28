#!/usr/bin/env python3
"""Builds the framework test programs (tests/framework/src) into dex files
for andruna-app --framework: javac against the boot class path and the
framework's classes (out/framework, aosp/framework/build.py), then d8.

Usage: python tests/framework/build.py
Output: out/framework-tests/<name>.dex
"""

import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
FW = ROOT / "out" / "framework"
OUT = ROOT / "out" / "framework-tests"
D8_JAR = ROOT / "out" / "s1" / "tools" / "build-tools-37" / "android-37.0" / "lib" / "d8.jar"
BOOT_JARS = [ROOT / "out" / "s1" / "java" / f"{j}.jar" for j in ("core-oj", "core-libart", "core-icu4j")]


def run(cmd):
    print("+", " ".join(str(c) for c in cmd), flush=True)
    subprocess.run([str(c) for c in cmd], check=True)


def main():
    classes_jar = FW / "framework-classes.jar"
    for p in [classes_jar, FW / "boot-module", D8_JAR] + BOOT_JARS:
        if not p.exists():
            sys.exit(f"missing {p}; run aosp/framework/build.py (and spikes/s1/java/build_boot.py)")
    OUT.mkdir(parents=True, exist_ok=True)
    cp = [classes_jar] + sorted(FW.glob("framework-*-classes.jar"))
    for src in sorted((HERE / "src").glob("*.java")):
        name = src.stem
        classes = OUT / "classes" / name
        if classes.is_dir():
            shutil.rmtree(classes)
        classes.mkdir(parents=True)
        run(["javac", "-encoding", "UTF-8", "-source", "21", "-target", "21", "--system=none",
             "--module-path", FW / "boot-module", "--add-modules", "java.base",
             "-classpath", os.pathsep.join(str(c) for c in cp),
             "-nowarn", "-d", classes, src])
        cj = OUT / f"{name}-classes.jar"
        with zipfile.ZipFile(cj, "w") as z:
            for f in sorted(classes.rglob("*.class")):
                z.write(f, f.relative_to(classes).as_posix())
        tmp = OUT / f"{name}.d8.jar"
        cmd = ["java", "-cp", D8_JAR, "com.android.tools.r8.D8", "--release", "--min-api", "36",
               "--output", tmp]
        for j in BOOT_JARS:
            cmd += ["--lib", j]
        for c in cp:
            cmd += ["--lib", c]
        run(cmd + [cj])
        with zipfile.ZipFile(tmp) as z:
            (OUT / f"{name.lower()}.dex").write_bytes(z.read("classes.dex"))
        tmp.unlink()
        print(f"wrote {OUT / (name.lower() + '.dex')}")


if __name__ == "__main__":
    main()
