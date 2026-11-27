"""Builds out/s1/java/warm.dex from Warm.java.

Needs what spikes/s1/java/build_boot.py leaves behind: the boot classes
(out/s1/java/classes/boot) and d8 (out/s1/tools/build-tools-37). Other
locations can be given with --boot-classes and --d8.
"""

import argparse
import os
import subprocess
import tempfile
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", "..", ".."))
OUT = os.path.join(ROOT, "out", "s1", "java")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--boot-classes", default=os.path.join(OUT, "classes", "boot"))
    p.add_argument("--boot-jars", default=OUT,
                   help="directory with core-oj.jar, core-libart.jar (d8 --lib)")
    p.add_argument("--d8", default=os.path.join(
        ROOT, "out", "s1", "tools", "build-tools-37", "android-37.0", "lib", "d8.jar"))
    a = p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run([
            "javac", "-encoding", "UTF-8", "-source", "21", "-target", "21",
            "--system=none", "--module-path", a.boot_classes,
            "-nowarn", "-d", tmp, os.path.join(HERE, "Warm.java"),
        ], check=True)
        jar = os.path.join(tmp, "warm-classes.jar")
        with zipfile.ZipFile(jar, "w") as z:
            z.write(os.path.join(tmp, "Warm.class"), "Warm.class")
        out_jar = os.path.join(tmp, "warm.d8.jar")
        libs = []
        for j in ("core-oj.jar", "core-libart.jar"):
            libs += ["--lib", os.path.join(a.boot_jars, j)]
        subprocess.run(["java", "-cp", a.d8, "com.android.tools.r8.D8", "--release",
                        "--min-api", "36", "--output", out_jar] + libs + [jar], check=True)
        with zipfile.ZipFile(out_jar) as z:
            data = z.read("classes.dex")
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, "warm.dex"), "wb") as f:
        f.write(data)
    print("wrote", os.path.join(OUT, "warm.dex"))


if __name__ == "__main__":
    main()
