"""Build the security providers of the boot class path as dex jars:
bouncycastle.jar (external/bouncycastle, module "bouncycastle" of the ART
module) and conscrypt.jar (external/conscrypt, module "conscrypt" of the
com.android.conscrypt APEX). Called by build_boot.py after the core
libraries; usable alone once they are built:

    python spikes/s1/java/build_security.py

libcore's security.properties names both providers, and
sun.security.jca.Providers asserts at class initialization that every
configured provider loads, so without them nothing that needs a
SecureRandom or a MessageDigest works.

The build mirrors Soong at android-17.0.0_r1:

- Sources are the checked-in repackaged trees (com.android.org.*), as the
  Android.bp filegroups name them.
- Conscrypt's generated sources are produced here:
  NativeConstants.java by conscrypt_generate_constants (a host program built
  from constants/src/gen/cpp/generate_constants.cc against BoringSSL's
  headers), StaticBlocklist.java from cert_verify_proc_blocklist.inc (the
  logic of generate_blocklist.awk), the flatbuffers classes of
  platform/src/main/ct_log_store.fbs by flatc (built from external/flatbuffers
  as Soong's host tool "flatc"), and the aconfig flag classes by
  gen_aconfig.py (every flag at its default).
- flatbuffers-java is compiled into conscrypt.jar under
  com.android.org.conscrypt.internal.flatbuffers, which is what
  flatbuffers-jarjar-rules.txt does to the class files; here the package is
  renamed in the sources instead.
- ConscryptStatsLog: Soong generates it with stats-log-api-gen (whose atom
  definitions are not in the fetched tree). Conscrypt's own unbundled
  implementation (common/src/main/java/org/conscrypt/metrics/
  ConscryptStatsLog.java, which writes through ReflexiveStatsEvent and does
  nothing when android.util.StatsEvent is absent) is used, repackaged the way
  Conscrypt's srcgen does it (org.conscrypt -> com.android.org.conscrypt).
- R8 shrinking of both libraries is not done (d8 only), so the jars keep
  classes Soong would remove.
"""

import os
import re
import shutil
import subprocess
import sys
import zipfile
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import build_boot as bb  # noqa: E402

AOSP = bb.AOSP
OUT = bb.OUT
TOOLS = bb.TOOLS
CONSCRYPT = os.path.join(AOSP, "external", "conscrypt")
BOUNCYCASTLE = os.path.join(AOSP, "external", "bouncycastle")
BORINGSSL = os.path.join(AOSP, "external", "boringssl")
FLATBUFFERS = os.path.join(AOSP, "external", "flatbuffers")
LLVM_MINGW = os.environ.get("ANDRUNA_LLVM_MINGW", "C:/tools/llvm-mingw")
EXE = ".exe" if os.name == "nt" else ""

BOOT_LIBS = ("core-oj", "core-libart", "core-icu4j")


def log(msg):
    print(msg, flush=True)


def clangxx():
    for c in (os.path.join(LLVM_MINGW, "bin", "clang++" + EXE), shutil.which("clang++")):
        if c and os.path.isfile(c):
            return c
    raise SystemExit("clang++ not found (set ANDRUNA_LLVM_MINGW)")


# --- host tools ------------------------------------------------------------------


def build_host_tool(name, sources, includes, flags=()):
    """Compiles a host program from C++ sources into out/s1/tools/<name>.

    Rebuilt only when missing; the sources are at the pinned tag.
    """
    exe = os.path.join(TOOLS, name + EXE)
    if os.path.isfile(exe):
        return exe
    obj_dir = os.path.join(TOOLS, name + "-obj")
    os.makedirs(obj_dir, exist_ok=True)
    cxx = clangxx()
    base = [cxx, "-std=c++17", "-O1", "-w"] + list(flags) + [f"-I{i}" for i in includes]

    def compile_one(src):
        stem = os.path.relpath(src, AOSP).replace(os.sep, "_").replace("/", "_")
        obj = os.path.join(obj_dir, stem + ".o")
        r = subprocess.run(base + ["-c", src, "-o", obj], capture_output=True, text=True)
        if r.returncode != 0:
            raise SystemExit(f"{src}: compile failed\n{r.stdout}{r.stderr}")
        return obj

    log(f"host tool {name}: {len(sources)} sources")
    with ThreadPoolExecutor(4) as ex:
        objs = list(ex.map(compile_one, sources))
    bb.run([cxx, "-static", "-o", exe] + objs)
    return exe


def flatc():
    srcs = sorted(
        os.path.join(FLATBUFFERS, "src", f)
        for f in os.listdir(os.path.join(FLATBUFFERS, "src"))
        if f.endswith(".cpp") and f != "flathash.cpp")
    grpc = os.path.join(FLATBUFFERS, "grpc", "src", "compiler")
    srcs += sorted(os.path.join(grpc, f) for f in os.listdir(grpc) if f.endswith(".cc"))
    srcs.append(os.path.join(FLATBUFFERS, "include", "codegen", "python.cc"))
    return build_host_tool("flatc", srcs,
                           [os.path.join(FLATBUFFERS, "grpc"), os.path.join(FLATBUFFERS, "include")],
                           ["-fsigned-char"])


def generate_constants():
    return build_host_tool(
        "conscrypt_generate_constants",
        [os.path.join(CONSCRYPT, "constants", "src", "gen", "cpp", "generate_constants.cc")],
        [os.path.join(BORINGSSL, "src", "include")])


# --- generated sources -------------------------------------------------------------


def write_text(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


def gen_native_constants(gen):
    out = subprocess.run([generate_constants(), "com.android.org.conscrypt"],
                         check=True, capture_output=True, text=True).stdout
    write_text(os.path.join(gen, "com", "android", "org", "conscrypt", "NativeConstants.java"),
               out.replace("\r\n", "\n"))


def gen_static_blocklist(gen):
    """StaticBlocklist.java: the SPKI hashes of kSPKIBlockList that are not
    also in kKnownInterceptionList (generate_blocklist.awk)."""
    inc = os.path.join(CONSCRYPT, "constants", "src", "gen", "java", "cert_verify_proc_blocklist.inc")
    with open(inc, encoding="utf-8") as f:
        text = f.read()

    def hashes(name):
        m = re.search(name + r"\[\]\[crypto::kSHA256Length\] = \{(.*?)\n\s*\};", text, re.S)
        if not m:
            raise SystemExit(f"{inc}: {name} not found")
        body = re.sub(r"//[^\n]*", "", m.group(1))
        out = []
        for block in re.findall(r"\{([^{}]*)\}", body):
            out.append(tuple(re.findall(r"0x([0-9a-fA-F]{2})", block)))
        return out

    blocked = hashes("kSPKIBlockList")
    known = set(hashes("kKnownInterceptionList"))
    lines = [
        "/* Generated by spikes/s1/java/build_security.py from",
        " * constants/src/gen/java/cert_verify_proc_blocklist.inc. Do not modify. */",
        "",
        "package com.android.org.conscrypt;",
        "",
        "final class StaticBlocklist {",
        "    static final byte[][] PUBLIC_KEYS = {",
    ]
    for h in blocked:
        if h in known:
            continue
        lines.append("        {")
        for i in range(0, len(h), 8):
            lines.append("            " + " ".join(f"(byte) 0x{b}," for b in h[i:i + 8]))
        lines.append("        },")
    lines += ["    };", "}", ""]
    write_text(os.path.join(gen, "com", "android", "org", "conscrypt", "StaticBlocklist.java"),
               "\n".join(lines))


def gen_stats_log(gen):
    src = os.path.join(CONSCRYPT, "common", "src", "main", "java", "org", "conscrypt", "metrics",
                       "ConscryptStatsLog.java")
    with open(src, encoding="utf-8") as f:
        text = f.read()
    text = re.sub(r"\borg\.conscrypt\b", "com.android.org.conscrypt", text)
    write_text(os.path.join(gen, "com", "android", "org", "conscrypt", "metrics",
                            "ConscryptStatsLog.java"), text)


FB_FROM = "com.google.flatbuffers"
FB_TO = "com.android.org.conscrypt.internal.flatbuffers"


def gen_flatbuffers(gen):
    fbs = os.path.join(CONSCRYPT, "platform", "src", "main", "ct_log_store.fbs")
    tmp = os.path.join(OUT, "gen", "flatc")
    if os.path.isdir(tmp):
        shutil.rmtree(tmp)
    os.makedirs(tmp)
    bb.run([flatc(), "-o", tmp, "--java", fbs])
    # flatbuffers-java and the generated classes, with the package renamed
    # (flatbuffers-jarjar-rules.txt).
    java = os.path.join(FLATBUFFERS, "java", "src", "main", "java")
    pairs = [(p, os.path.relpath(p, java)) for p in bb.glob_java(java)]
    pairs += [(p, os.path.relpath(p, tmp)) for p in bb.glob_java(tmp)]
    for src, rel in pairs:
        with open(src, encoding="utf-8") as f:
            text = f.read()
        text = text.replace(FB_FROM, FB_TO)
        rel = rel.replace(os.sep, "/")
        if rel.startswith(FB_FROM.replace(".", "/") + "/"):
            rel = FB_TO.replace(".", "/") + rel[len(FB_FROM):]
        write_text(os.path.join(gen, *rel.split("/")), text)


# --- compile ---------------------------------------------------------------------


def boot_packages(boot):
    pkgs = set()
    for d, _dirs, names in os.walk(boot):
        if any(n.endswith(".class") for n in names):
            rel = os.path.relpath(d, boot)
            if rel != ".":
                pkgs.add(rel.replace(os.sep, "."))
    return sorted(pkgs)


def compile_library(name, sources, classpath=()):
    """javac against the boot class path (java.base), then d8; returns the
    classes jar."""
    boot = os.path.join(OUT, "classes", "boot")
    classes = os.path.join(OUT, "classes", name)
    if os.path.isdir(classes):
        shutil.rmtree(classes)
    os.makedirs(classes)
    # Compile-only annotations (stubs/, as in build_boot.py): resolved from
    # the source path and not written to the output.
    args = ["-encoding", "UTF-8", "-source", bb.JAVA_VERSION, "-target", bb.JAVA_VERSION,
            "--system=none", "--module-path", boot,
            "-XDstringConcat=inline", "-g", "-nowarn", "-Xlint:none", "-proc:none",
            "-Xmaxerrs", "500", "-implicit:none",
            "-sourcepath", os.path.join(HERE, "stubs"),
            "-d", classes]
    if classpath:
        args += ["-cp", os.pathsep.join(classpath)]
    for p in boot_packages(boot):
        args += ["--add-exports", f"java.base/{p}=ALL-UNNAMED"]
    args += sources
    bb.javac(args, os.path.join(OUT, f"javac-{name}.args"))
    entries = []
    for d, _, files in os.walk(classes):
        for f in files:
            if f.endswith(".class"):
                p = os.path.join(d, f)
                with open(p, "rb") as fh:
                    entries.append((os.path.relpath(p, classes).replace(os.sep, "/"), fh.read()))
    cj = os.path.join(OUT, "classes", name + "-classes.jar")
    bb.write_jar(cj, entries)
    log(f"{name}: {len(entries)} classes")
    return cj


def dex(name, classes_jar, classpath=()):
    boot_jars = [os.path.join(OUT, "classes", lib + "-classes.jar") for lib in BOOT_LIBS]
    out_jar = os.path.join(OUT, name + ".jar")
    tmp = out_jar + ".d8.jar"
    bb.d8([classes_jar], tmp, classpath=boot_jars + list(classpath), boot=True)
    with zipfile.ZipFile(tmp) as z:
        entries = [(n, z.read(n)) for n in z.namelist()]
    bb.write_jar(out_jar, entries)
    os.remove(tmp)
    log(f"wrote {out_jar} ({os.path.getsize(out_jar)} bytes)")
    return out_jar


def build_bouncycastle():
    srcs = bb.glob_java(os.path.join(BOUNCYCASTLE, "repackaged", "bcprov", "src", "main", "java"))
    log(f"sources: bouncycastle {len(srcs)}")
    cj = compile_library("bouncycastle", srcs)
    return dex("bouncycastle", cj)


def build_conscrypt():
    gen = os.path.join(OUT, "gen", "conscrypt")
    if os.path.isdir(gen):
        shutil.rmtree(gen)
    os.makedirs(gen)
    gen_native_constants(gen)
    gen_static_blocklist(gen)
    gen_stats_log(gen)
    gen_flatbuffers(gen)
    bb.run([sys.executable, "-I", os.path.join(HERE, "gen_aconfig.py"), gen,
            os.path.join(CONSCRYPT, "conscrypt.aconfig"),
            os.path.join(CONSCRYPT, "networksecurity.aconfig")])
    srcs = []
    for d in ("repackaged/common/src/main/java", "repackaged/platform/src/main/java",
              "publicapi/src/main/java"):
        srcs += bb.glob_java(os.path.join(CONSCRYPT, *d.split("/")))
    srcs += bb.glob_java(gen)
    log(f"sources: conscrypt {len(srcs)}")
    cj = compile_library("conscrypt", srcs)
    return dex("conscrypt", cj)


def main():
    bb.ensure_build_tools()
    os.makedirs(TOOLS, exist_ok=True)
    build_bouncycastle()
    build_conscrypt()


if __name__ == "__main__":
    main()
