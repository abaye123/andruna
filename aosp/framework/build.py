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
sys.dont_write_bytecode = True
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
    return java, aidl, missing, generated, tree


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


def gen_aidl(gen, aidl_files, report, include_roots=(), per_file_args=None):
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
                  NATIVE / "libs" / "permission" / "aidl", NATIVE / "aidl" / "binder",
                  *include_roots):
        if extra.is_dir() and extra not in roots:
            roots.append(extra)
    inc = [f"-I{r}" for r in roots]
    base = [AIDL, "--lang=java", "--transaction_names", "-o", out] + inc

    ok = 0
    todo = [f for r in roots for f in by_root.get(r, [])]
    # One process per file: aidl stops at the first file that fails.
    for i, f in enumerate(todo):
        extra = (per_file_args or {}).get(f, [])
        r = subprocess.run([str(a) for a in base] + extra + [str(f)], capture_output=True, text=True)
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


PERFETTO = AOSP / "external" / "perfetto"
# Projects whose aconfig declarations become flag classes in framework.jar.
# libcore's and ART's flags are in the boot class path jars already.
ACONFIG_PROJECTS = [FW, NATIVE, AOSP / "system" / "server_configurable_flags"]
PROTOBUF_SRC = AOSP / "external" / "protobuf" / "src"


def filegroup_files(tree, bp, name, report):
    try:
        tree.read(bp)
        m = tree.module(name)
        return [Path(f) for f in tree.sources(tree.select_blocks(m, None)) if not f.startswith(":")]
    except (KeyError, FileNotFoundError, blueprint.ParseError) as e:
        report.setdefault("protos_not_available", []).append(f"{name}: {e}")
        return []


def gen_javastream(gen, tree, report):
    """framework-javastream-protos (ProtoLibraries.bp) and
    perfetto_trace_javastream_protos_jarjar (frameworks/base/Android.bp):
    protoc-gen-javastream's output, from gen_javastream.py."""
    out = gen / "javastream"
    if out.is_dir():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    srcs = sorted((FW / "core" / "proto").rglob("*.proto")) + sorted((FW / "libs" / "incident").rglob("*.proto"))
    srcs += filegroup_files(tree, FW / "proto" / "Android.bp", "ipconnectivity-proto-src", report)
    stats = AOSP / "frameworks" / "proto_logging" / "stats" / "Android.bp"
    srcs += filegroup_files(tree, stats, "libstats_atom_enum_protos", report)
    srcs += filegroup_files(tree, stats, "libstats_atom_message_protos", report)
    srcs += filegroup_files(tree, AOSP / "system" / "core" / "debuggerd" / "proto" / "Android.bp",
                            "libtombstone_proto-src", report)
    report.setdefault("protos_not_available", []).append(
        "aconfigd_protos: not defined in the fetched system/server_configurable_flags")
    argfile = gen / "javastream.args"
    argfile.write_text("\n".join(str(s) for s in srcs), encoding="utf-8")
    run([sys.executable, "-I", HERE / "gen_javastream.py", "-I", AOSP, "-I", PROTOBUF_SRC,
         out, f"@{argfile}"])
    # Perfetto: only the messages the framework uses (include_filter of the
    # genrule), renamed as framework-jarjar-rules.txt renames them.
    bp = PERFETTO / "Android.bp"
    psrcs = filegroup_files(tree, bp, "perfetto_javastream_filegroup_proto", report)
    text = bp.read_text(encoding="utf-8")
    i = text.index('name: "perfetto_trace_javastream_protos"')
    j = text.index("--javastream_out", i)
    flt = "".join(re.findall(r'"([^"]*)"', text[i:j]))
    flt = flt.split("include_filter:", 1)[1].split()[0]
    (gen / "perfetto.args").write_text("\n".join(str(s) for s in psrcs), encoding="utf-8")
    run([sys.executable, "-I", HERE / "gen_javastream.py", "-I", PERFETTO, "-I", PROTOBUF_SRC,
         "-I", AOSP, "--filter", flt, "--rename", "perfetto.protos=android.internal.perfetto.protos",
         out, f"@{gen / 'perfetto.args'}"])
    return out


def gen_statslog(gen, tree, generated, report):
    """The statslog-*-java-gen genrules of the source universe
    (FrameworkStatsLog, TelephonyStatsLog, ...): stats-log-api-gen's Java
    output, from gen_statslog.py with the genrule's arguments."""
    out = gen / "statslog"
    if out.is_dir():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    done = []
    for g in generated:
        name = g[1:]
        if not name.startswith("statslog-"):
            continue
        m = tree.module(name)
        cmd = m.get("cmd", "")
        args = cmd.split()
        def val(flag):
            return args[args.index(flag) + 1] if flag in args else None
        module, pkg, cls = val("--module"), val("--javaPackage"), val("--javaClass")
        if not (module and pkg and cls):
            report.setdefault("statslog_skipped", []).append(name)
            continue
        dest = out / Path(*pkg.split(".")) / f"{cls}.java"
        extra = ["--worksource"] if "--worksource" in args else []
        run([sys.executable, "-I", HERE / "gen_statslog.py", dest, AOSP, "--module", module,
             "--javaPackage", pkg, "--javaClass", cls] + extra)
        done.append(cls)
    report["statslog"] = done
    return out


def gen_aconfig(gen):
    out = gen / "aconfig"
    if out.is_dir():
        shutil.rmtree(out)
    decls = []
    for proj in ACONFIG_PROJECTS:
        decls += [d for d in sorted(proj.rglob("*.aconfig")) if "/tests/" not in d.as_posix()]
    run([sys.executable, "-I", HERE / "gen_aconfig.py", out] + decls, stdout=subprocess.DEVNULL)
    return out


FLAG_USE = re.compile(r"\b((?:[a-z_][\w]*\.)+)Flags\.(\w+)")
FLAG_IMPORT = re.compile(r"^\s*import\s+(static\s+)?((?:[a-z_][\w]*\.)+)Flags(?:\.(\w+))?\s*;", re.M)
SIMPLE_USE = re.compile(r"(?<![\w.])Flags\.(\w+)")


def snake(name):
    if name.startswith("FLAG_"):
        return name[5:].lower()
    return re.sub(r"(?<=[a-z0-9])([A-Z])", r"_\1", name).lower()


def gen_aconfig_from_uses(gen, java_files, report):
    """Flag classes of aconfig packages declared in projects that are not
    fetched (frameworks/av, packages/modules/*, ...), with the flags the
    framework's sources name, all disabled like every other flag here. The
    declarations are not available, so the flag set is what the sources use;
    the values are the same as a declaration would give at its default."""
    sys.path.insert(0, str(HERE))
    import gen_aconfig as ga
    out = gen / "aconfig-uses"
    if out.is_dir():
        shutil.rmtree(out)
    known = set()
    for f in (gen / "aconfig").rglob("Flags.java"):
        known.add(f.parent.relative_to(gen / "aconfig").as_posix().replace("/", "."))
    have_source = set()
    for f in java_files:
        if f.name == "Flags.java":
            have_source.add(package_of(f))
    uses = {}
    for f in java_files:
        text = f.read_text(encoding="utf-8", errors="replace")
        if "Flags" not in text:
            continue
        imported = None
        for m in FLAG_IMPORT.finditer(text):
            pkg = m.group(2)[:-1]
            uses.setdefault(pkg, set())
            if m.group(1) and m.group(3):
                uses[pkg].add(m.group(3))
            elif not m.group(1):
                imported = pkg
        for m in FLAG_USE.finditer(text):
            uses.setdefault(m.group(1)[:-1], set()).add(m.group(2))
        if imported is None and package_of(f) not in known | have_source:
            # Flags of the file's own package (no import needed).
            if SIMPLE_USE.search(text) and not (f.parent / "Flags.java").exists():
                imported = package_of(f)
        if imported:
            for m in SIMPLE_USE.finditer(text):
                uses.setdefault(imported, set()).add(m.group(1))
    made = {}
    boot = boot_module()
    for pkg, names in sorted(uses.items()):
        if pkg in known or pkg in have_source or not pkg:
            continue
        if (boot / Path(*pkg.split("."))).is_dir():
            continue  # flags of the boot class path (libcore, ART)
        by_method = {}
        for n in sorted(names, key=lambda x: not x.startswith("FLAG_")):
            if n in ("FEATURE_FLAGS", "class") or n[0].isupper() and not n.startswith("FLAG_"):
                continue
            s = snake(n)
            by_method.setdefault(ga.camel(s), s)
        flags = set(by_method.values())
        if not flags:
            continue
        ga.emit(str(out), pkg, sorted(flags))
        made[pkg] = len(flags)
    report["aconfig_from_uses"] = made
    log(f"aconfig from uses: {len(made)} packages of unfetched projects")
    return out


def gen_logtags(gen):
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
    for d in [d for proj in ACONFIG_PROJECTS for d in sorted(proj.rglob("*.aconfig"))]:
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




# --- sources of other projects (sources.toml) -------------------------------------

GITILES = "https://android.googlesource.com/platform"
DEPS = OUT / "deps"


def load_sources():
    import tomllib
    with open(HERE / "sources.toml", "rb") as f:
        return tomllib.load(f)["source"]


def fetch_sources(entries):
    """Each entry's directories, from gitiles archives at the pinned commit,
    into out/framework/deps/<project>/<dir>. A directory is fetched again when
    its commit changes."""
    import io
    import tarfile
    import urllib.request
    for e in entries:
        if e.get("local"):
            continue  # a project of aosp.lock, fetched by cargo xtask fetch
        dirs = (e.get("java", []) + e.get("aidl", []) + e.get("aidl_include", [])
                + e.get("aconfig", []) + e.get("implicit", []) + e.get("sysprop", [])
                + e.get("nano", [])
                + [v["dir"] for v in e.get("aidl_versioned", [])])
        for d in sorted(set(dirs)):
            dest = DEPS / e["project"] / d
            stamp = dest / ".commit"
            if stamp.is_file() and stamp.read_text().strip() == e["commit"]:
                continue
            url = f"{GITILES}/{e['project']}/+archive/{e['commit']}/{d}.tar.gz"
            log(f"fetching {url}")
            data = urllib.request.urlopen(url, timeout=300).read()
            if dest.is_dir():
                shutil.rmtree(dest)
            dest.mkdir(parents=True)
            with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tf:
                for m in tf.getmembers():
                    target = (dest / m.name).resolve()
                    if not str(target).startswith(str(dest.resolve())):
                        raise SystemExit(f"{url}: unsafe path {m.name}")
                    if m.issym() or m.islnk() or m.isdev():
                        continue  # links are not needed by this build
                    if m.isdir():
                        target.mkdir(parents=True, exist_ok=True)
                    elif m.isfile():
                        target.parent.mkdir(parents=True, exist_ok=True)
                        target.write_bytes(tf.extractfile(m).read())
            stamp.write_text(e["commit"], encoding="utf-8")


def renamed_root(e, root):
    """A copy of a Java source root with a package prefix renamed, the
    source-level equivalent of the jarjar rule Soong applies to the library
    (every occurrence of the package name, and the directory layout)."""
    frm, to = e["rename"]["from"], e["rename"]["to"]
    out = OUT / "renamed" / e["project"] / root.relative_to(dep_dir(e, ""))
    stamp = out / ".done"
    key = f"{frm}={to}"
    if stamp.is_file() and stamp.read_text() == key:
        return out
    if out.is_dir():
        shutil.rmtree(out)
    pat = re.compile(r"\b" + re.escape(frm) + r"\b")
    frm_dir, to_dir = frm.replace(".", "/"), to.replace(".", "/")
    for f in root.rglob("*.java"):
        r = f.relative_to(root).as_posix()
        if r.startswith(frm_dir + "/"):
            r = to_dir + r[len(frm_dir):]
        dest = out / r
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(pat.sub(to, f.read_text(encoding="utf-8")), encoding="utf-8", newline="\n")
    stamp.write_text(key, encoding="utf-8")
    return out


def gen_nano(gen, sources):
    """Soong's `proto: { type: "nano" }` static libraries of the framework
    (framework-protos, devicepolicyprotosnano, libaconfig_java_proto_nano),
    with the nano runtime renamed as their jarjar rules rename it."""
    out = gen / "nano"
    if out.is_dir():
        shutil.rmtree(out)
    files, roots = [], [AOSP, PROTOBUF_SRC]
    for e in sources:
        for d in e.get("nano", []):
            p = dep_dir(e, d)
            files += sorted(p.rglob("*.proto")) if p.is_dir() else [p]
            if not e.get("local"):
                roots.append(dep_dir(e, ""))
    args = [sys.executable, "-I", HERE / "gen_nano.py"]
    for r in roots:
        args += ["-I", r]
    run(args + ["--runtime", "com.android.framework.protobuf.nano", out] + files)
    return out


def gen_sysprop(gen, sources):
    out = gen / "sysprop"
    if out.is_dir():
        shutil.rmtree(out)
    files = [f for e in sources for d in e.get("sysprop", []) for f in sorted(dep_dir(e, d).glob("*.sysprop"))]
    run([sys.executable, "-I", HERE / "gen_sysprop.py", out] + files)
    return out


def dep_dir(e, d):
    if e.get("local"):
        return AOSP / e["project"] / d
    return DEPS / e["project"] / d


# --- javac / d8 -------------------------------------------------------------------


def write_argfile(path, args):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        for a in args:
            f.write('"' + str(a).replace("\\", "/") + '"\n')


def source_key(path):
    """'pkg/dir/File.java' of a source file, from its package declaration."""
    pkg = package_of(Path(path))
    return (pkg.replace(".", "/") + "/" if pkg else "") + Path(path).name


def javac(explicit, sourcepath, classes):
    if classes.is_dir():
        shutil.rmtree(classes)
    classes.mkdir(parents=True)
    args = [
        "-encoding", "UTF-8",
        "-source", JAVA_VERSION, "-target", JAVA_VERSION,
        "--system=none",
        "--module-path", boot_module(),
        "--add-modules", "java.base",
    ] + boot_exports() + [
        "-XDstringConcat=inline",
        "-g",
        "-nowarn", "-Xlint:none", "-Xmaxerrs", "100000",
        "-proc:none",
        "-implicit:class",
        "-sourcepath", os.pathsep.join(str(d) for d in sourcepath),
        "-d", classes,
    ] + explicit
    write_argfile(OUT / "javac.args", args)
    t = time.time()
    r = subprocess.run(["javac", "-J-Xmx4g", f"@{OUT / 'javac.args'}"], capture_output=True, text=True)
    (OUT / "javac.log").write_text(r.stdout + r.stderr, encoding="utf-8")
    errors = [ln for ln in (r.stdout + r.stderr).splitlines() if ": error: " in ln]
    log(f"javac: exit {r.returncode}, {len(errors)} errors, {time.time() - t:.0f} s")
    return r.returncode == 0, r.stdout + r.stderr, time.time() - t


REPAIRED = OUT / "repaired"
REPAIRED_SP = OUT / "repaired-sourcepath"


def repair_round(log_text, explicit, sourcepath, report):
    """Applies repair.py to every file with errors; returns the new explicit
    list. Repaired copies of explicit files go to out/framework/repaired, of
    source path files to a root that comes first on the source path."""
    import repair
    errs, spelled = {}, {}
    for path, line, col, msg in repair.parse_errors(log_text):
        k = os.path.normcase(os.path.abspath(path))
        spelled.setdefault(k, os.path.abspath(path))
        errs.setdefault(k, []).append((line, col, msg))
    explicit_idx = {os.path.normcase(str(Path(f).resolve())): i for i, f in enumerate(explicit)}
    changed = 0
    for path, el in sorted(errs.items()):
        src = Path(spelled[path])
        text = src.read_text(encoding="utf-8", errors="replace")
        new, done, unhandled = repair.repair(text, el)
        key = source_key(src)
        for desc, reason in done:
            report["not_built"].append({"class": key[:-5].replace("/", "."), "member": desc,
                                        "reason": reason})
        for desc, reason in unhandled:
            report["not_built_unhandled"].append({"class": key, "member": desc, "reason": reason})
        if new is None:
            # The file's top-level type cannot be built: it is left out.
            changed += 1
            if path in explicit_idx:
                explicit[explicit_idx[path]] = None
            else:
                dest = REPAIRED_SP / key
                dest.parent.mkdir(parents=True, exist_ok=True)
                # An empty compilation unit hides the source path copy.
                pkg = key.rsplit("/", 1)[0].replace("/", ".") if "/" in key else ""
                dest.write_text(f"package {pkg};\n" if pkg else "", encoding="utf-8", newline="\n")
            continue
        if new == text:
            continue
        changed += 1
        if path in explicit_idx:
            dest = REPAIRED / key
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(new, encoding="utf-8", newline="\n")
            explicit[explicit_idx[path]] = dest
        else:
            dest = REPAIRED_SP / key
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(new, encoding="utf-8", newline="\n")
    explicit[:] = [f for f in explicit if f is not None]
    log(f"repair: {changed} files changed")
    return changed


sys.path.insert(0, str(ROOT / "spikes" / "s1" / "java"))
from build_boot import class_source_file  # noqa: E402


def split_classes(classes, origin):
    """{jar name: [(entry, bytes)]} by the source file each class came from;
    classes of compile-only sources (origin None) are dropped."""
    jars, unplaced = {}, []
    for p in sorted(classes.rglob("*.class")):
        data = p.read_bytes()
        name, src = class_source_file(data)
        pkg = name.rsplit("/", 1)[0] + "/" if "/" in name else ""
        key = pkg + (src or name.split("/")[-1].split("$")[0] + ".java")
        if key not in origin:
            unplaced.append(key)
            continue
        if origin[key] is None:
            continue
        jars.setdefault(origin[key], []).append((name + ".class", data))
    if unplaced:
        raise SystemExit(f"classes from unknown sources: {sorted(set(unplaced))[:20]}")
    return jars


def write_jar(path, entries):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in sorted(entries):
            z.writestr(zipfile.ZipInfo(name, date_time=(2008, 1, 1, 0, 0, 0)), data)


def d8(classes_jar, out_jar, classpath):
    cmd = ["java", "-Xmx3g", "-cp", D8_JAR, "com.android.tools.r8.D8",
           "--release", "--android-platform-build", "--min-api", MIN_API,
           "--output", out_jar]
    for j in BOOT_JARS:
        cmd += ["--lib", j]
    for c in classpath:
        cmd += ["--classpath", c]
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

    sources = load_sources()
    fetch_sources(sources)
    ACONFIG_PROJECTS.extend(dep_dir(e, d) for e in sources for d in e.get("aconfig", []))

    java, aidl, missing, generated, tree = universe()
    report["filegroups_not_fetched"] = missing
    report["generated_filegroups"] = generated
    log(f"universe: {len(java)} java, {len(aidl)} aidl; {len(missing)} filegroups of projects "
        f"not fetched")

    # AIDL of other projects: compiled into the jar the entry names, or
    # compile-only; aidl_include directories are import roots only.
    extra_aidl, aidl_jar, aidl_args = [], {}, {}
    for e in sources:
        for d in e.get("aidl", []):
            for f in sorted(dep_dir(e, d).rglob("*.aidl")):
                extra_aidl.append(f)
                aidl_jar[f] = e.get("jar", "stub")
        for v in e.get("aidl_versioned", []):
            root = dep_dir(e, v["dir"])
            h = root / ".hash"
            hashes = h.read_text(encoding="utf-8").split() if h.is_file() else []
            extra = ["--structured", f"--version={v['version']}"]
            if e.get("stability"):
                extra.append(f"--stability={e['stability']}")
            if hashes:
                extra.append(f"--hash={hashes[-1]}")
            for f in sorted(root.rglob("*.aidl")):
                extra_aidl.append(f)
                aidl_jar[f] = e.get("jar", "stub")
                aidl_args[f] = extra
    include_roots = [dep_dir(e, d) for e in sources for d in e.get("aidl_include", [])]

    if args.skip_aidl and (gen / "aidl").is_dir():
        aidl_out = gen / "aidl"
        fix_aidl_output(aidl_out)
        prev = OUT / "report.json"
        if prev.is_file():
            report["aidl_skipped"] = json.loads(prev.read_text())["aidl_skipped"]
    else:
        aidl_out = gen_aidl(gen, aidl + extra_aidl, report, include_roots, aidl_args)
    if args.skip_res and (gen / "R").is_dir():
        r_java, apk = gen / "R", OUT / "framework-res.apk"
    else:
        r_java, apk = build_res(gen)
    gen_dirs = [gen_messagequeue(gen), aidl_out, gen_aconfig(gen), gen_logtags(gen), r_java,
                gen_javastream(gen, tree, report), gen_statslog(gen, tree, generated, report),
                gen_sysprop(gen, sources), gen_nano(gen, sources)]
    gen_java = [f for d in gen_dirs if d.name != "aconfig" for f in d.rglob("*.java")]

    # Java of other projects.
    dep_java = {}
    stub_roots = [HERE / "stubs"]
    stubsrc = OUT / "stubsrc"
    implicit_roots = []
    for e in sources:
        for d in e.get("implicit", []):
            root = dep_dir(e, d)
            if e.get("rename"):
                root = renamed_root(e, root)
            implicit_roots.append((root, e["jar"]))
        for d in e.get("java", []):
            root = dep_dir(e, d)
            if e.get("rename"):
                root = renamed_root(e, root)
            if e.get("jar", "stub") == "stub":
                out = stubsrc / e["project"] / d
                if not (out / ".done").is_file() or (out / ".done").read_text() != e["commit"]:
                    if out.is_dir():
                        shutil.rmtree(out)
                    run([sys.executable, "-I", HERE / "stubgen.py", out, root])
                    (out / ".done").write_text(e["commit"], encoding="utf-8")
                stub_roots.append(out)
            else:
                for f in sorted(root.rglob("*.java")):
                    dep_java[f] = e["jar"]
    stub_java = [f for r in stub_roots for f in r.rglob('*.java')]
    gen_dirs.append(gen_aconfig_from_uses(gen, java + gen_java + list(dep_java) + stub_java, report))

    # Which jar each source belongs to, by 'package/File.java'. Compile-only
    # sources map to None; framework sources win over a stub of the same class.
    origin = {}
    for root in stub_roots:
        for f in root.rglob("*.java"):
            origin[source_key(f)] = None
    for f in [MODULES_UTILS / "java"]:
        for s in f.rglob("*.java"):
            origin[source_key(s)] = "framework"
    for root, jar in implicit_roots:
        for s in root.rglob("*.java"):
            origin[source_key(s)] = jar
    for f, jar in dep_java.items():
        origin[source_key(f)] = jar
    aidl_gen_jar = {}
    for f, jar in aidl_jar.items():
        aidl_gen_jar[source_key(f).replace(".aidl", ".java")] = jar
    explicit = []
    for d in gen_dirs:
        for f in d.rglob("*.java"):
            k = source_key(f)
            origin[k] = aidl_gen_jar.get(k, "framework") if d == aidl_out else "framework"
            if origin[k] is not None:
                explicit.append(f)
    for f in java:
        origin[source_key(f)] = "framework"
        explicit.append(f)
    explicit += [f for f, jar in dep_java.items()]
    # The framework's own static libraries (modules-utils) and the
    # compile-only stubs come in through the source path when referenced.
    sourcepath = [MODULES_UTILS / "java", aidl_out] + [r for r, _ in implicit_roots] + stub_roots
    (OUT / "origin.json").write_text(json.dumps(origin, indent=0, sort_keys=True), encoding="utf-8")

    classes = OUT / "classes"
    for d in (REPAIRED, REPAIRED_SP):
        if d.is_dir():
            shutil.rmtree(d)
    REPAIRED_SP.mkdir(parents=True)
    sourcepath = [REPAIRED_SP] + sourcepath
    report["not_built"], report["not_built_unhandled"] = [], []
    secs = 0
    for rnd in range(12):
        ok, javac_log, s = javac(explicit, sourcepath, classes)
        secs += s
        if ok:
            break
        if not repair_round(javac_log, explicit, sourcepath, report):
            errors = [ln for ln in javac_log.splitlines() if ": error: " in ln]
            log("\n".join(errors[:40]))
            raise SystemExit(f"javac failed with {len(errors)} errors that repair.py does not "
                             f"handle; full log in {OUT / 'javac.log'}")
    else:
        raise SystemExit("javac still fails after 12 repair rounds")
    report["javac_rounds"] = rnd + 1
    jars = split_classes(classes, origin)
    log(f"javac: {sum(len(v) for v in jars.values())} classes in {secs:.0f} s")
    built = {}
    cjars = {}
    for name, entries in jars.items():
        cj = OUT / f"{name}-classes.jar"
        write_jar(cj, entries)
        cjars[name] = cj
    stub_jar = OUT / "compile-only-classes.jar"
    write_jar(stub_jar, [(p.relative_to(classes).as_posix(), p.read_bytes())
                         for p in classes.rglob("*.class")])
    for name, cj in cjars.items():
        jar = OUT / f"{name}.jar"
        d8(cj, jar, [stub_jar])
        built[name] = {"classes": len(jars[name]), "bytes": jar.stat().st_size}
    report.update({
        "jars": built,
        "universe_java": len(java),
        "universe_aidl": len(aidl),
        "framework_res_apk_bytes": apk.stat().st_size,
        "javac_seconds": round(secs),
        "seconds": round(time.time() - t0),
    })
    (OUT / "report.json").write_text(json.dumps(report, indent=1), encoding="utf-8")
    for name, b in built.items():
        log(f"wrote {OUT / (name + '.jar')}: {b['classes']} classes, {b['bytes']} bytes")
    log(f"wrote {apk} ({apk.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
