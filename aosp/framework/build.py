#!/usr/bin/env python3
"""Builds the Android framework's Java side for Andruna: framework-res.apk
(aapt2) and framework.jar (aidl, javac, d8), from frameworks/base at the
pinned tag. See docs/plan/phase2/framework-build.md.

What Soong does for `framework-minus-apex` (frameworks/base/Android.bp) and
`framework-res` (core/res/Android.bp), and what this script does instead:

- Sources. The universe is Soong's filegroup `framework-non-updatable-sources`
  resolved with aosp/tools/blueprint.py over frameworks/base and
  frameworks/native. Filegroups of projects that are not fetched (hardware
  interfaces, packages/modules/*, frameworks/av, system/*) are left out and
  listed in the build report.
- framework.jar is the transitive closure of the classes named in
  roots.txt: javac compiles the roots and, through -sourcepath and
  -implicit:class, every class they reference, recursively. A class that
  nothing in the closure references is not in the jar. The jar grows by
  adding roots (system_server needs far more of the framework than an app
  process at this step).
- Generated sources: AIDL (aidl built by aosp/tools/build_host.py), aconfig
  flag classes (gen_aconfig.py: every flag at its declared default, which is
  disabled), event log tags (gen_logtags.py), android.R and
  com.android.internal.R and android.Manifest from framework-res (aapt2),
  MessageQueue.java (Soong's messagequeue-gen, default variant).
- Compile-only stubs (stubs/) stand in for classes of projects that are not
  fetched. They are used by javac like Soong's `libs` and are never packaged.

Usage: python aosp/framework/build.py [--arch x64|arm64] [--skip-res]

Inputs: third_party/aosp (cargo xtask fetch), the boot class path classes
of spikes/s1/java/build_boot.py (out/s1/java/classes/boot and the core
jars), aidl (out/host/<host arch>/bin/aidl.exe) and the Android SDK
build-tools that build_boot.py downloads (d8, aapt2; ADR 0023).

Outputs (out/framework/): framework.jar, framework-res.apk, report.json.
"""

import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import time
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "aosp" / "tools"))
import blueprint  # noqa: E402

AOSP = Path(os.environ.get("ANDRUNA_AOSP_DIR", ROOT / "third_party" / "aosp"))
FW = AOSP / "frameworks" / "base"
NATIVE = AOSP / "frameworks" / "native"
MODULES_UTILS = AOSP / "frameworks" / "libs" / "modules-utils"
OUT = ROOT / "out" / "framework"
S1 = ROOT / "out" / "s1"
BOOT_CLASSES = S1 / "java" / "classes" / "boot"
BOOT_JARS = [S1 / "java" / f"{j}.jar" for j in ("core-oj", "core-libart", "core-icu4j")]
BUILD_TOOLS = S1 / "tools" / "build-tools-37" / "android-37.0"
D8_JAR = BUILD_TOOLS / "lib" / "d8.jar"
AAPT2 = Path(os.environ.get("ANDRUNA_AAPT2", BUILD_TOOLS / "aapt2.exe"))

JAVA_VERSION = "21"
MIN_API = "36"


def host_arch():
    m = platform.machine().lower()
    return "arm64" if m in ("arm64", "aarch64") else "x64"


AIDL = Path(os.environ.get("ANDRUNA_AIDL", ROOT / "out" / "host" / host_arch() / "bin" / "aidl.exe"))


def log(msg):
    print(msg, flush=True)


def run(cmd, **kw):
    cmd = [str(c) for c in cmd]
    line = " ".join(cmd)
    log("+ " + (line if len(line) < 300 else line[:300] + " ..."))
    return subprocess.run(cmd, check=True, **kw)


# --- sources ----------------------------------------------------------------


def universe():
    """Soong's framework-non-updatable-sources over the fetched projects:
    (java files, aidl files, filegroups not available)."""
    tree = blueprint.Tree(AOSP, "x86_64")
    for proj in (FW, NATIVE, MODULES_UTILS):
        for bp in sorted(proj.rglob("Android.bp")):
            rel = bp.relative_to(proj).as_posix()
            if "/tests/" in "/" + rel or rel.startswith("services/"):
                continue
            try:
                tree.read(bp)
            except blueprint.ParseError as e:
                log(f"warning: {bp}: {e}")
    m = tree.module("framework-non-updatable-sources")
    files, missing, generated = [], [], []
    for s in m.get("device_common_srcs", []):
        try:
            for f in tree.expand(m["__dir"], s):
                (generated if f.startswith(":") else files).append(f)
        except KeyError:
            missing.append(s)
    files = sorted(set(files))
    java = [Path(f) for f in files if f.endswith(".java")]
    aidl = [Path(f) for f in files if f.endswith(".aidl")]
    return java, aidl, missing, generated


PACKAGE = re.compile(r"^\s*package\s+([\w.]+)\s*;", re.M)


def package_of(path):
    text = path.read_text(encoding="utf-8", errors="replace")
    m = PACKAGE.search(text)
    return m.group(1) if m else ""


def source_root(path):
    """The directory a source file's package path starts at."""
    pkg = package_of(path)
    d = path.parent
    for part in reversed(pkg.split(".") if pkg else []):
        if d.name != part:
            return None
        d = d.parent
    return d


# --- generated sources ---------------------------------------------------------


def gen_messagequeue(gen):
    """Soong's messagequeue-gen: the default (CombinedMessageQueue) variant."""
    d = gen / "messagequeue" / "android" / "os"
    d.mkdir(parents=True, exist_ok=True)
    shutil.copy2(FW / "core" / "java" / "android" / "os" / "CombinedMessageQueue" / "MessageQueue.java",
                 d / "MessageQueue.java")
    return gen / "messagequeue"


def gen_aidl(gen, aidl_files, report):
    """Java for every AIDL file of the universe. Files whose imports are not
    available (types of projects that are not fetched) are reported."""
    out = gen / "aidl"
    if out.is_dir():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    roots = []
    by_root = {}
    for f in aidl_files:
        r = source_root(f)
        if r is None:
            report["aidl_skipped"].append({"file": rel(f), "reason": "package does not match the path"})
            continue
        if r not in roots:
            roots.append(r)
        by_root.setdefault(r, []).append(f)
    # Extra import roots Soong passes (framework-minus-apex-defaults aidl
    # include_dirs that exist in the fetched projects).
    for extra in (FW / "media" / "aidl", FW / "telecomm" / "framework" / "aidl-export",
                  NATIVE / "libs" / "permission" / "aidl", NATIVE / "aidl" / "binder"):
        if extra.is_dir() and extra not in roots:
            roots.append(extra)
    inc = [f"-I{r}" for r in roots]
    base = [AIDL, "--lang=java", "--transaction_names", "-o", out] + inc

    def run_batch(files):
        # aidl matches files to import roots textually: Windows separators.
        args = base + [str(f) for f in files]
        argfile = gen / "aidl.args"
        argfile.write_text("\n".join(f'"{a}"' for a in map(str, args[1:])), encoding="utf-8")
        return subprocess.run([str(AIDL), f"@{argfile}"], capture_output=True, text=True)

    ok = 0
    todo = [f for r in roots for f in by_root.get(r, [])]
    # One process per file: aidl stops at the first file that fails.
    for i, f in enumerate(todo):
        r = subprocess.run([str(a) for a in base] + [str(f)], capture_output=True, text=True)
        if r.returncode == 0:
            ok += 1
        else:
            msg = (r.stderr or r.stdout).strip().splitlines()
            reason = msg[0].replace("ERROR: ", "").replace(str(f), "").lstrip(": ") if msg else "failed"
            report["aidl_skipped"].append({"file": rel(f), "reason": reason[:300]})
    log(f"aidl: {ok} of {len(todo)} files, {len(todo) - ok} skipped")
    fix_aidl_output(out)
    return out


def fix_aidl_output(out):
    """aidl writes an empty file for each unstructured parcelable
    declaration (the class is hand-written Java); those are removed. Source
    positions in annotations use the path as given, which on Windows has
    backslashes: they become forward slashes so the Java string is valid."""
    for f in out.rglob("*.java"):
        if f.stat().st_size == 0:
            f.unlink()
            continue
        text = f.read_text(encoding="utf-8")
        prefix = str(AOSP) + "\\"
        new = re.sub(r'overrideSourcePosition="([^"]*)"',
                     lambda m: 'overrideSourcePosition="%s"'
                     % m.group(1).replace(prefix, "").replace("\\", "/"),
                     text)
        if new != text:
            f.write_text(new, encoding="utf-8", newline="\n")


def gen_aconfig(gen):
    out = gen / "aconfig"
    if out.is_dir():
        shutil.rmtree(out)
    decls = sorted(FW.rglob("*.aconfig"))
    decls = [d for d in decls if "/tests/" not in d.as_posix()]
    run([sys.executable, "-I", HERE / "gen_aconfig.py", out] + decls, stdout=subprocess.DEVNULL)
    return out


def gen_logtags(gen, java_roots):
    out = gen / "logtags"
    if out.is_dir():
        shutil.rmtree(out)
    tags = sorted((FW / "core" / "java").rglob("*.logtags"))
    run([sys.executable, "-I", HERE / "gen_logtags.py", out] + tags)
    return out


def rel(p):
    try:
        return Path(p).relative_to(AOSP).as_posix()
    except ValueError:
        return str(p)


# --- framework-res --------------------------------------------------------------


def aconfig_flag_values():
    """name=false for every flag declared in frameworks/base (aapt2
    --feature-flags): the same defaults as the Java flag classes."""
    import importlib.util
    spec = importlib.util.spec_from_file_location("gen_aconfig", HERE / "gen_aconfig.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    vals = []
    for d in sorted(FW.rglob("*.aconfig")):
        if "/tests/" in d.as_posix():
            continue
        package, flags = mod.parse(d)
        for f in flags or []:
            vals.append(f"{package}.{f['name']}=false")
    # Flags declared in projects that are not fetched (frameworks/av,
    # packages/modules/*, ...) but named by framework-res: also disabled.
    named = set()
    res = FW / "core" / "res"
    for p in [res / "AndroidManifest.xml"] + sorted((res / "res").rglob("*.xml")):
        for m in re.finditer(r'featureFlag=\s*"!?([\w.]+)"', p.read_text(encoding="utf-8", errors="replace")):
            named.add(m.group(1))
    declared = {v.split("=")[0] for v in vals}
    vals += [f"{n}=false" for n in named - declared]
    return sorted(set(vals))


def build_res(gen):
    """framework-res.apk and its R.java / Manifest.java (core/res/Android.bp)."""
    res = FW / "core" / "res"
    work = OUT / "res"
    work.mkdir(parents=True, exist_ok=True)
    # remote-color-resources-arsc: a small resource table packaged as a raw
    # resource of framework-res.
    rc = work / "remote-color"
    rc.mkdir(exist_ok=True)
    rcres = res / "remote_color_resources_res" / "values"
    run([AAPT2, "compile", rcres / "public.xml", "-o", rc])
    run([AAPT2, "compile", rcres / "colors.xml", "-o", rc])
    run([AAPT2, "link", "-o", rc / "remote-color-resources.apk", "--manifest",
         res / "RemoteThemeColorsAndroidManifest.xml",
         rc / "values_public.arsc.flat", rc / "values_colors.arsc.flat"])
    extra = work / "extra-res"
    (extra / "raw").mkdir(parents=True, exist_ok=True)
    (extra / "values").mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(rc / "remote-color-resources.apk") as z:
        (extra / "raw" / "remote_views_color_resources.arsc").write_bytes(z.read("resources.arsc"))
    shutil.copy2(res / "remote_color_resources_res" / "symbols.xml", extra / "values" / "symbols.xml")

    # Resources can be gated on aconfig flags (android:featureFlag); aapt2
    # needs the value of every flag they name.
    flags = work / "feature-flags.txt"
    flags.write_text(",".join(aconfig_flag_values()), encoding="utf-8")
    compiled = work / "compiled.zip"
    if compiled.exists():
        compiled.unlink()
    run([AAPT2, "compile", "--dir", res / "res", "-o", compiled, "--feature-flags", f"@{flags}"])
    extra_zip = work / "extra.zip"
    if extra_zip.exists():
        extra_zip.unlink()
    run([AAPT2, "compile", "--dir", extra, "-o", extra_zip])

    java = gen / "R"
    if java.is_dir():
        shutil.rmtree(java)
    java.mkdir(parents=True)
    apk = OUT / "framework-res.apk"
    run([AAPT2, "link", "-o", apk,
         "--manifest", res / "AndroidManifest.xml",
         "--java", java,
         "--private-symbols", "com.android.internal",
         "--no-auto-version", "--auto-add-overlay", "--enable-sparse-encoding",
         # Soong's defaults for platform apps.
         "--min-sdk-version", "37", "--target-sdk-version", "37",
         "--version-code", "37", "--version-name", "17",
         "-A", res / "assets",
         "--feature-flags", f"@{flags}",
         "-x",  # the framework's package id (0x01)
         compiled, extra_zip])
    return java, apk


# --- javac / d8 -------------------------------------------------------------------


def write_argfile(path, args):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        for a in args:
            f.write('"' + str(a).replace("\\", "/") + '"\n')


def roots_from_file(java_dirs):
    roots = []
    for line in (HERE / "roots.txt").read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        relpath = line.replace(".", "/") + ".java"
        hits = [d / relpath for d in java_dirs if (d / relpath).is_file()]
        if not hits:
            raise SystemExit(f"roots.txt: no source for {line}")
        roots.append(hits[0])
    return roots


def boot_module():
    """java.base as javac sees it: the classes of core-oj, core-libart and
    core-icu4j and libcore's module-info, without the compile-only
    annotation stubs build_boot.py compiled into the same directory (the
    framework has the real android.annotation package, from modules-utils)."""
    d = OUT / "boot-module"
    stamp = d / ".complete"
    if stamp.is_file():
        return d
    if d.is_dir():
        shutil.rmtree(d)
    d.mkdir(parents=True)
    for lib in ("core-oj", "core-libart", "core-icu4j"):
        with zipfile.ZipFile(S1 / "java" / "classes" / f"{lib}-classes.jar") as z:
            z.extractall(d)
    shutil.copy2(BOOT_CLASSES / "module-info.class", d / "module-info.class")
    stamp.write_text("", encoding="utf-8")
    return d


def boot_exports():
    """--add-exports for every package of the boot class path: the framework
    is part of the platform and uses java.base's internal packages (libcore,
    dalvik, android.system, ...), as Soong's core_platform system modules
    allow."""
    pkgs = set()
    mod = boot_module()
    for p in mod.rglob("*.class"):
        pkg = p.parent.relative_to(mod).as_posix().replace("/", ".")
        if pkg and pkg != ".":
            pkgs.add(pkg)
    out = []
    for pkg in sorted(pkgs):
        out += ["--add-exports", f"java.base/{pkg}=ALL-UNNAMED"]
    return out


def javac(java_dirs, roots, classes):
    if classes.is_dir():
        shutil.rmtree(classes)
    classes.mkdir(parents=True)
    stubs = sorted((HERE / "stubs").rglob("*.java"))
    args = [
        "-encoding", "UTF-8",
        "-source", JAVA_VERSION, "-target", JAVA_VERSION,
        "--system=none",
        "--module-path", boot_module(),
        "--add-modules", "java.base",
    ] + boot_exports() + [
        "-XDstringConcat=inline",
        "-g",
        "-nowarn", "-Xlint:none", "-Xmaxerrs", "2000",
        "-proc:none",
        "-implicit:class",
        "-sourcepath", os.pathsep.join(str(d) for d in java_dirs),
        "-d", classes,
    ] + roots
    # Stubs compile into their own directory and are only on the class path.
    stub_classes = OUT / "classes-stubs"
    if stub_classes.is_dir():
        shutil.rmtree(stub_classes)
    stub_classes.mkdir(parents=True)
    if stubs:
        sargs = ["-encoding", "UTF-8", "-source", JAVA_VERSION, "-target", JAVA_VERSION,
                 "--system=none", "--module-path", boot_module(), "--add-modules", "java.base",
                 ] + boot_exports() + [
                 "-nowarn", "-proc:none", "-implicit:class",
                 "-sourcepath", os.pathsep.join(str(d) for d in [HERE / "stubs"] + java_dirs),
                 "-d", stub_classes] + stubs
        write_argfile(OUT / "javac-stubs.args", sargs)
        run(["javac", "-J-Xmx2g", f"@{OUT / 'javac-stubs.args'}"])
    args[args.index("-sourcepath")] = "-classpath"
    args.insert(args.index("-classpath") + 1, stub_classes)
    args.insert(args.index("-classpath") + 2, "-sourcepath")
    write_argfile(OUT / "javac.args", args)
    t = time.time()
    r = subprocess.run(["javac", "-J-Xmx3g", f"@{OUT / 'javac.args'}"], capture_output=True, text=True)
    (OUT / "javac.log").write_text(r.stdout + r.stderr, encoding="utf-8")
    if r.returncode != 0:
        tail = (r.stdout + r.stderr).splitlines()[-40:]
        log("\n".join(tail))
        raise SystemExit(f"javac failed; full log in {OUT / 'javac.log'}")
    return time.time() - t


def stub_class_names():
    names = set()
    for s in (HERE / "stubs").rglob("*.java"):
        names.add(s.relative_to(HERE / "stubs").with_suffix("").as_posix())
    return names


def package_jar(classes, out_jar):
    stubs = stub_class_names()
    entries = []
    for p in sorted(classes.rglob("*.class")):
        name = p.relative_to(classes).as_posix()
        outer = name[:-len(".class")].split("$")[0]
        if outer in stubs:
            continue
        entries.append((name, p.read_bytes()))
    with zipfile.ZipFile(out_jar, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in entries:
            z.writestr(zipfile.ZipInfo(name, date_time=(2008, 1, 1, 0, 0, 0)), data)
    return len(entries)


def d8(classes_jar, out_jar):
    cmd = ["java", "-Xmx3g", "-cp", D8_JAR, "com.android.tools.r8.D8",
           "--release", "--android-platform-build", "--min-api", MIN_API,
           "--output", out_jar]
    for j in BOOT_JARS:
        cmd += ["--lib", j]
    cmd += ["--classpath", OUT / "classes-stubs"]
    cmd.append(classes_jar)
    run(cmd)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--skip-res", action="store_true", help="reuse framework-res and R.java")
    ap.add_argument("--skip-aidl", action="store_true", help="reuse the generated AIDL sources")
    args = ap.parse_args()
    for p in [AIDL, AAPT2, D8_JAR, BOOT_CLASSES] + BOOT_JARS:
        if not Path(p).exists():
            raise SystemExit(f"missing input {p} (see the usage notes in this script)")
    OUT.mkdir(parents=True, exist_ok=True)
    gen = OUT / "gen"
    gen.mkdir(exist_ok=True)
    report = {"aidl_skipped": []}
    t0 = time.time()

    java, aidl, missing, generated = universe()
    report["filegroups_not_fetched"] = missing
    report["generated_filegroups"] = generated
    log(f"universe: {len(java)} java, {len(aidl)} aidl; {len(missing)} filegroups of projects "
        f"not fetched")
    java_dirs = [MODULES_UTILS / "java"]
    for f in java:
        r = source_root(f)
        if r is not None and r not in java_dirs:
            java_dirs.append(r)

    if args.skip_aidl and (gen / "aidl").is_dir():
        aidl_out = gen / "aidl"
        fix_aidl_output(aidl_out)
        prev = OUT / "report.json"
        if prev.is_file():
            report["aidl_skipped"] = json.loads(prev.read_text())["aidl_skipped"]
    else:
        aidl_out = gen_aidl(gen, aidl, report)
    if args.skip_res and (gen / "R").is_dir():
        r_java, apk = gen / "R", OUT / "framework-res.apk"
    else:
        r_java, apk = build_res(gen)
    gen_dirs = [gen_messagequeue(gen), aidl_out, gen_aconfig(gen), gen_logtags(gen, java_dirs), r_java]

    # Generated sources first: messagequeue-gen replaces nothing on the
    # source path (the variants live in subdirectories).
    all_dirs = gen_dirs + java_dirs
    roots = roots_from_file(all_dirs)
    classes = OUT / "classes"
    secs = javac(all_dirs, roots, classes)
    cj = OUT / "framework-classes.jar"
    n = package_jar(classes, cj)
    log(f"javac: {n} classes in {secs:.0f} s")
    jar = OUT / "framework.jar"
    d8(cj, jar)
    report.update({
        "classes": n,
        "universe_java": len(java),
        "universe_aidl": len(aidl),
        "framework_jar_bytes": jar.stat().st_size,
        "framework_res_apk_bytes": apk.stat().st_size,
        "seconds": round(time.time() - t0),
    })
    (OUT / "report.json").write_text(json.dumps(report, indent=1), encoding="utf-8")
    log(f"wrote {jar} ({jar.stat().st_size} bytes), {apk} ({apk.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
