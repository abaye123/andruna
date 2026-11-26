#!/usr/bin/env python3
"""Lists the external symbols that the AOSP static libraries leave undefined.

Reads every static library in an `out/aosp/<arch>` build directory with
llvm-nm, removes the symbols that any of the libraries defines, and prints
what is left, grouped by who has to provide it:

  libc      C library and Linux interfaces (bionic names): andruna-lx/bionic
  libc++    C++ standard library and C++ ABI runtime (std::, __cxa_*, ...)
  compiler  compiler runtime and Windows CRT start-up helpers
  art-asm   ART's hand-written assembly (entry points, nterp), not built yet
  aosp      AOSP libraries that are not built yet

Usage: undefined_symbols.py <llvm-nm> <build dir> [--objects <dir>]... [--json]

--objects adds the object files below <dir> (for a library that does not
build completely, such as the libart attempt), reported under the
directory's name.
"""

import json
import os
import re
import subprocess
import sys

CXX_PREFIXES = ("_ZNSt3__1", "_ZNKSt3__1", "_ZSt", "_ZNSt", "_ZNKSt", "_ZTVNSt3__1",
                "_ZTINSt3__1", "_ZTSNSt3__1", "_ZTVN10__cxxabiv1", "_ZdlPv", "_ZdaPv",
                "_Znwm", "_Znam", "_Znwy", "_Znay", "_ZdlPvy", "_ZdaPvy", "_ZnwyRKSt",
                "_ZTISt", "_ZTVSt", "_ZThn", "_ZTv", "_ZTTNSt3__1")
CXX_NAMES = ("__cxa_", "__gxx_personality", "_Unwind_", "__dynamic_cast")
COMPILER = re.compile(r"^(__chkstk|___chkstk_ms|__security_|__guard_|_tls_|__emutls_|"
                      r"__(u?div|u?mod|mul|ashl|ashr|lshr|fix|float|extend|trunc)\w*[sdtx]f?i?\d?$|"
                      r"__clear_cache$|__popcount|__clz|__ctz|__bswap|__aarch64_|__arm64x|"
                      r"\.refptr|__imp_|__C_specific_handler|__CxxFrameHandler|_fltused|"
                      r"__mingw|__acrt|__stack_chk_guard$|__stack_chk_fail$)")


ART_ASM = re.compile(r"^(art_|art[A-Z]|ExecuteNterp|EndExecuteNterp|ExecuteSwitchImplAsm$|"
                     r"AsmGetRegs$|ExecuteMterp|MterpHandler|nterp\d*_op_|__memcmp16$)")


def classify(sym):
    if sym.startswith(CXX_PREFIXES) or sym.startswith(CXX_NAMES) or sym in ("_ZdlPv", "_Znwy"):
        return "libc++"
    if sym.startswith("_Z"):
        # A mangled name outside std is an AOSP C++ API.
        return "aosp"
    if COMPILER.match(sym):
        return "compiler"
    if ART_ASM.match(sym):
        return "art-asm"
    if re.search(r"[A-Z]", sym):
        # C names with capitals are AOSP APIs (libnativeloader, libnativebridge,
        # libsigchain); bionic's C names are lower case.
        return "aosp"
    return "libc"


def nm(llvm_nm, lib):
    out = subprocess.run([llvm_nm, "--format=posix", "--no-sort", lib],
                         capture_output=True, text=True, check=True).stdout
    defined, undefined = {}, set()
    for line in out.splitlines():
        parts = line.split()
        if len(parts) < 2 or line.endswith(":"):
            continue
        name, kind = parts[0], parts[1]
        if kind in ("U",):
            undefined.add(name)
        elif kind in ("w", "v"):
            undefined.add(name)  # weak undefined: still needs a provider or is optional
        elif kind not in ("N",):
            defined[name] = kind
    return defined, undefined


def main():
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    llvm_nm, build = sys.argv[1], sys.argv[2]
    inputs = []
    for f in sorted(os.listdir(build)):
        if f.endswith(".a"):
            inputs.append((f[3:-2] if f.startswith("lib") else f[:-2], [os.path.join(build, f)]))
    args = sys.argv[3:]
    for i, a in enumerate(args):
        if a == "--objects":
            d = args[i + 1]
            objs = [os.path.join(r, f) for r, _, fs in os.walk(d) for f in fs
                    if f.endswith((".obj", ".o"))]
            name = os.path.basename(os.path.normpath(d)).removesuffix(".dir")
            inputs.append((name + " (objects)", sorted(objs)))
    defined_all = set()
    users = {}
    for name, files in inputs:
        for path in files:
            d, u = nm(llvm_nm, path)
            defined_all.update(d)
            for s in u:
                users.setdefault(s, set()).add(name)
    result = {}
    for s in sorted(users):
        if s in defined_all:
            continue
        result.setdefault(classify(s), []).append((s, sorted(users[s])))
    if "--json" in sys.argv:
        json.dump({k: [{"symbol": s, "users": u} for s, u in v] for k, v in result.items()},
                  sys.stdout, indent=1)
        return
    for group in ("libc", "libc++", "compiler", "art-asm", "aosp"):
        items = result.get(group, [])
        print(f"## {group} ({len(items)})")
        for s, u in items:
            print(f"{s}\t{','.join(u)}")
        print()


if __name__ == "__main__":
    main()
