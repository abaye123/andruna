#!/usr/bin/env python3
"""Builds the libcore test jar that tests/libcore/run.py runs on ART.

Steps:
  1. Fetch the test libraries (JUnit 4, Hamcrest, JUnitParams) at pinned
     versions, checked by SHA-256, into out/libcore-tests/tools. They are
     test tools only (ADR 0006 amendment) and are never committed or shipped.
  2. Expand the selection in tests/libcore/tests.txt against the fetched
     libcore sources (third_party/aosp/libcore).
  3. javac the selected tests and the runner (tests/libcore/harness) against
     the boot class path classes (out/s1/java/classes/boot, from
     spikes/s1/java/build_boot.py), with libcore's test support sources on
     the source path. A selected file that does not compile is dropped and
     recorded with the first error; compilation is retried without it.
  4. d8 the classes and the test libraries into dex and write
     out/libcore-tests/libcore-tests.jar (dex files and the test resources),
     classes.txt (the test classes to run) and build-report.json.

Environment overrides: S1_AOSP (AOSP tree), S1_JAVA (boot class path build
output), LIBCORE_TESTS_OUT (output directory), LIBCORE_D8 (d8.jar).
"""

import fnmatch
import glob
import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys

# The spike scripts this imports live in the source tree; keep it clean.
sys.dont_write_bytecode = True
import urllib.request
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
AOSP = Path(os.environ.get("S1_AOSP", ROOT / "third_party" / "aosp"))
LIBCORE = AOSP / "libcore"
JAVA = Path(os.environ.get("S1_JAVA", ROOT / "out" / "s1" / "java"))
OUT = Path(os.environ.get("LIBCORE_TESTS_OUT", ROOT / "out" / "libcore-tests"))
TOOLS = OUT / "tools"
JAVA_VERSION = "21"
MIN_API = "36"

# Test libraries from Maven Central. Test tools only: never shipped.
LIBS = [
    # (file, url, sha256, license)
    ("junit-4.13.2.jar",
     "https://repo1.maven.org/maven2/junit/junit/4.13.2/junit-4.13.2.jar",
     "8e495b634469d64fb8acfa3495a065cbacc8a0fff55ce1e31007be4c16dc57d3", "EPL-1.0"),
    ("hamcrest-core-1.3.jar",
     "https://repo1.maven.org/maven2/org/hamcrest/hamcrest-core/1.3/hamcrest-core-1.3.jar",
     "66fdef91e9739348df7a096aa384a5685f4e875584cce89386a7a47251c4d8e9", "BSD-3-Clause"),
    ("JUnitParams-1.1.1.jar",
     "https://repo1.maven.org/maven2/pl/pragmatists/JUnitParams/1.1.1/JUnitParams-1.1.1.jar",
     "1be1aac16d424ce940d5407bef86656dc4ed5803c93e563cb1682ae07b591ecb", "Apache-2.0"),
]

# libcore's test support code, compiled when a selected test uses it.
SOURCE_PATH = [
    "luni/src/test/java",
    "support/src/test/java",
    "test-rules/src/main/java",
    "test-utils/src/main/java",
    "dalvik/src/test/java",
]

# Directories whose non-Java files are test resources (as core-tests'
# java_resource_dirs).
RESOURCE_DIRS = [
    "luni/src/test/java",
    "luni/src/test/resources",
    "support/src/test/java",
]

ERROR_RE = re.compile(r"^(.*?\.java):(\d+): error: (.*)$")


def log(msg):
    print(msg, flush=True)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def fetch_libs():
    TOOLS.mkdir(parents=True, exist_ok=True)
    jars = []
    for name, url, digest, _license in LIBS:
        path = TOOLS / name
        if not path.exists() or sha256(path) != digest:
            log(f"downloading {url}")
            tmp = path.with_suffix(".part")
            urllib.request.urlretrieve(url, tmp)
            got = sha256(tmp)
            if got != digest:
                tmp.unlink()
                raise SystemExit(f"{url}: sha256 {got}, expected {digest}")
            tmp.replace(path)
        jars.append(path)
    return jars


def d8_jar():
    env = os.environ.get("LIBCORE_D8")
    if env:
        return Path(env)
    # The pinned build-tools of the boot class path build.
    spec = importlib.util.spec_from_file_location(
        "build_boot", ROOT / "spikes" / "s1" / "java" / "build_boot.py")
    bb = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bb)
    bb.ensure_build_tools()
    return Path(bb.D8_JAR)


def read_selection():
    """Returns (selected test files, {file: reason} removed by the list,
    support files compiled but not run)."""
    include, exclude, support = [], [], []
    for raw in (HERE / "tests.txt").read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("-"):
            pat, _, reason = line[1:].partition("|")
            exclude.append((pat.strip(), reason.strip()))
        elif line.startswith("+"):
            pat = line[1:].partition("|")[0].strip()
            matches = sorted(glob.glob(str(LIBCORE / pat)))
            if not matches:
                log(f"warning: {pat} matches nothing")
            support += [Path(m).relative_to(LIBCORE).as_posix() for m in matches]
        else:
            include.append(line)
    files = []
    for pat in include:
        matches = sorted(glob.glob(str(LIBCORE / pat)))
        if not matches:
            log(f"warning: {pat} matches nothing")
        files += [Path(m).relative_to(LIBCORE).as_posix() for m in matches]
    files = sorted(set(files))
    removed = {}
    for f in list(files):
        for pat, reason in exclude:
            if fnmatch.fnmatch(f, pat):
                removed[f] = reason
                files.remove(f)
                break
    return files, removed, sorted(set(support) - set(files))


def boot_packages(boot):
    pkgs = set()
    for d, _dirs, names in os.walk(boot):
        if any(n.endswith(".class") for n in names):
            rel = os.path.relpath(d, boot)
            if rel != ".":
                pkgs.add(rel.replace(os.sep, "."))
    return sorted(pkgs)


def class_name(rel):
    """The top-level class of a test source file."""
    text = (LIBCORE / rel).read_text(encoding="utf-8", errors="replace")
    m = re.search(r"^\s*package\s+([\w.]+)\s*;", text, re.M)
    stem = Path(rel).stem
    return f"{m.group(1)}.{stem}" if m else stem


def javac(files, libs, classes, support=()):
    """Compiles files; returns {file (relative to libcore or absolute): first error}."""
    if classes.exists():
        shutil.rmtree(classes)
    classes.mkdir(parents=True)
    boot = JAVA / "classes" / "boot"
    harness = sorted(str(p) for p in (HERE / "harness" / "src").rglob("*.java"))
    args = ["-encoding", "UTF-8", "-source", JAVA_VERSION, "-target", JAVA_VERSION,
            "--system=none", "--module-path", str(boot),
            "-XDstringConcat=inline", "-g", "-nowarn", "-Xlint:none", "-proc:none",
            "-Xmaxerrs", "100000", "-implicit:class",
            "-cp", os.pathsep.join(str(j) for j in libs),
            "-sourcepath", os.pathsep.join(str(LIBCORE / s) for s in SOURCE_PATH),
            "-d", str(classes)]
    for p in boot_packages(boot):
        args += ["--add-exports", f"java.base/{p}=ALL-UNNAMED"]
    args += harness + [str(LIBCORE / f) for f in list(files) + list(support)]
    argfile = OUT / "javac.args"
    with open(argfile, "w", encoding="utf-8", newline="\n") as f:
        for a in args:
            f.write('"' + a.replace("\\", "/") + '"\n')
    r = subprocess.run(["javac", "@" + str(argfile)], capture_output=True, text=True,
                       encoding="utf-8", errors="replace")
    errors = {}
    for line in (r.stdout + r.stderr).splitlines():
        m = ERROR_RE.match(line.strip())
        if m:
            p = Path(m.group(1)).resolve()
            try:
                key = p.relative_to(LIBCORE.resolve()).as_posix()
            except ValueError:
                key = str(p)
            errors.setdefault(key, f"line {m.group(2)}: {m.group(3)}")
    if r.returncode != 0 and not errors:
        raise SystemExit("javac failed without parsable errors:\n" + r.stdout + r.stderr)
    return errors


def compile_selection(files, libs, classes, support):
    """javac, dropping failing files until the rest compiles. Returns
    (compiled files, {dropped file: reason})."""
    files = list(files)
    dropped = {}
    for round_ in range(1, 30):
        log(f"javac round {round_}: {len(files)} test files")
        errors = javac(files, libs, classes, support)
        if not errors:
            return files, dropped
        progress = False
        for path, err in errors.items():
            if path in support:
                support.remove(path)
                dropped[path] = err
                progress = True
                continue
            if path in files:
                files.remove(path)
                dropped[path] = err
                progress = True
                continue
            if not path.endswith(".java") or "harness" in Path(path).parts:
                raise SystemExit(f"the runner does not compile: {path}: {err}")
            # A support file on the source path failed: drop the selected
            # tests that name its class.
            simple = Path(path).stem
            word = re.compile(r"\b" + re.escape(simple) + r"\b")
            for f in list(files):
                if word.search((LIBCORE / f).read_text(encoding="utf-8", errors="replace")):
                    files.remove(f)
                    dropped[f] = f"needs {path}: {err}"
                    progress = True
        if not progress:
            raise SystemExit("javac errors that no selected file explains:\n" +
                             "\n".join(f"{k}: {v}" for k, v in errors.items()))
    raise SystemExit("javac: too many rounds")


def write_jar(classes, libs, d8, out_jar):
    boot_jars = [JAVA / "classes" / f"{j}-classes.jar" for j in ("core-oj", "core-libart", "core-icu4j")]
    inputs = OUT / "classes.jar"
    with zipfile.ZipFile(inputs, "w", zipfile.ZIP_DEFLATED) as z:
        for p in sorted(classes.rglob("*.class")):
            z.write(p, p.relative_to(classes).as_posix())
    dexzip = OUT / "dex.zip"
    if dexzip.exists():
        dexzip.unlink()
    cmd = ["java", "-Xmx4g", "-cp", str(d8), "com.android.tools.r8.D8", "--release",
           "--min-api", MIN_API, "--output", str(dexzip)]
    for b in boot_jars:
        cmd += ["--lib", str(b)]
    cmd += [str(inputs)] + [str(j) for j in libs]
    log("+ d8 ...")
    subprocess.run(cmd, check=True)
    n_res = 0
    with zipfile.ZipFile(out_jar, "w", zipfile.ZIP_DEFLATED) as z:
        with zipfile.ZipFile(dexzip) as dz:
            for name in dz.namelist():
                if name.endswith(".dex"):
                    z.writestr(name, dz.read(name))
        seen = set()
        for d in RESOURCE_DIRS:
            base = LIBCORE / d
            for p in sorted(base.rglob("*")):
                if p.is_file() and p.suffix not in (".java", ".bp", ".smali", ".mk"):
                    arc = p.relative_to(base).as_posix()
                    if arc not in seen:
                        seen.add(arc)
                        z.write(p, arc)
                        n_res += 1
    log(f"wrote {out_jar} ({out_jar.stat().st_size} bytes, {n_res} resources)")


def main():
    if not LIBCORE.is_dir():
        raise SystemExit(f"{LIBCORE} not found: run `cargo xtask fetch` (or set S1_AOSP)")
    if not (JAVA / "classes" / "boot").is_dir():
        raise SystemExit(f"{JAVA}/classes/boot not found: run spikes/s1/java/build_boot.py "
                         "(or set S1_JAVA)")
    OUT.mkdir(parents=True, exist_ok=True)
    libs = fetch_libs()
    d8 = d8_jar()
    selected, removed, support = read_selection()
    log(f"selected {len(selected)} test files ({len(removed)} removed by tests.txt)")
    compiled, dropped = compile_selection(selected, libs, OUT / "classes", support)
    log(f"compiled {len(compiled)}, dropped {len(dropped)}")
    write_jar(OUT / "classes", libs, d8, OUT / "libcore-tests.jar")
    names = sorted(class_name(f) for f in compiled)
    (OUT / "classes.txt").write_text("\n".join(names) + "\n", encoding="utf-8")
    report = {
        "selected": len(selected),
        "compiled": len(compiled),
        "removed_by_list": removed,
        "dropped_by_javac": dropped,
        "libraries": [{"file": n, "url": u, "sha256": s, "license": l} for n, u, s, l in LIBS],
    }
    (OUT / "build-report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    log(f"wrote {OUT / 'classes.txt'} ({len(names)} classes), {OUT / 'build-report.json'}")


if __name__ == "__main__":
    main()
