"""Builds fixture.apk from src/ with the Android SDK's own tools.

aapt2 compiles and links the manifest and resources (Hebrew label in
values-iw, an adaptive icon with a vector foreground), javac and d8 build
classes.dex, zipalign aligns, and apksigner signs with schemes v1, v2 and v3
using a key generated for this run (the key is not kept). The result is
committed so the tests check andruna-pkg against real tool output without
the SDK; rerun this script only to change the fixture.

usage: python build.py   (needs ANDROID_HOME or %LOCALAPPDATA%\\Android\\Sdk,
                          build-tools 34+, platforms/android-34+, a JDK)
"""

import glob
import os
import shutil
import subprocess
import sys
import tempfile
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(HERE, "src")


def sdk():
    root = os.environ.get("ANDROID_HOME") or os.path.join(
        os.environ.get("LOCALAPPDATA", ""), "Android", "Sdk")
    tools = sorted(glob.glob(os.path.join(root, "build-tools", "*")))
    platforms = sorted(glob.glob(os.path.join(root, "platforms", "android-*", "android.jar")))
    if not tools or not platforms:
        sys.exit("Android SDK build-tools or platforms not found")
    return tools[-1], platforms[-1]


def run(*args):
    print(" ".join(str(a) for a in args))
    subprocess.run([str(a) for a in args], check=True, shell=str(args[0]).endswith(".bat"))


def main():
    tools, android_jar = sdk()
    tool = lambda name: os.path.join(tools, name)
    work = tempfile.mkdtemp(prefix="andruna-fixture-")
    try:
        compiled = os.path.join(work, "res.zip")
        run(tool("aapt2.exe"), "compile", "--dir", os.path.join(SRC, "res"), "-o", compiled)
        linked = os.path.join(work, "linked.apk")
        run(tool("aapt2.exe"), "link", "-o", linked, "-I", android_jar,
            "--manifest", os.path.join(SRC, "AndroidManifest.xml"),
            "--min-sdk-version", "26", "--target-sdk-version", "35",
            "--version-code", "3", "--version-name", "1.2.3", compiled)
        classes = os.path.join(work, "classes")
        java = glob.glob(os.path.join(SRC, "java", "**", "*.java"), recursive=True)
        run("javac", "--release", "11", "-classpath", android_jar, "-d", classes, *java)
        dex_out = os.path.join(work, "dex")
        os.makedirs(dex_out)
        class_files = glob.glob(os.path.join(classes, "**", "*.class"), recursive=True)
        run(tool("d8.bat"), "--release", "--min-api", "26", "--lib", android_jar,
            "--output", dex_out, *class_files)
        with zipfile.ZipFile(linked, "a", zipfile.ZIP_DEFLATED) as z:
            z.write(os.path.join(dex_out, "classes.dex"), "classes.dex")
        aligned = os.path.join(work, "aligned.apk")
        run(tool("zipalign.exe"), "-p", "-f", "4", linked, aligned)
        ks = os.path.join(work, "key.p12")
        run("keytool", "-genkeypair", "-keystore", ks, "-storetype", "PKCS12",
            "-storepass", "fixture", "-alias", "fixture", "-keyalg", "EC",
            "-groupname", "secp256r1", "-dname", "CN=Andruna fixture", "-validity", "36500")
        out = os.path.join(HERE, "fixture.apk")
        run(tool("apksigner.bat"), "sign", "--ks", ks, "--ks-pass", "pass:fixture",
            "--v1-signing-enabled", "true", "--v2-signing-enabled", "true",
            "--v3-signing-enabled", "true", "--v4-signing-enabled", "false",
            "--min-sdk-version", "26", "--out", out, aligned)
        print(f"wrote {out} ({os.path.getsize(out)} bytes)")
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
