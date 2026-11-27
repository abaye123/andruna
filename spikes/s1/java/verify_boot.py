"""Check the S1 boot classpath built by build_boot.py.

Prints the number of classes in each dex jar and checks that every class
descriptor ART names in art/runtime/class_root.h, well_known_classes.cc and
class_linker.cc (the classes it needs to start without a boot image) is
defined in one of the boot jars. Exits with status 1 if any is missing.

Usage: verify_boot.py
"""

import os
import re
import struct
import sys
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", "..", ".."))
OUT = os.path.join(ROOT, "out", "s1", "java")
RUNTIME = os.path.join(ROOT, "third_party", "aosp", "art", "runtime")
BOOT = ["core-oj.jar", "core-libart.jar", "core-icu4j.jar"]
SOURCES = ["class_root.h", "well_known_classes.cc", "class_linker.cc"]


def uleb128(data, pos):
    result, shift = 0, 0
    while True:
        b = data[pos]
        pos += 1
        result |= (b & 0x7F) << shift
        if b < 0x80:
            return result, pos
        shift += 7


def dex_classes(data):
    """Return the descriptors of the classes defined in one dex file."""
    if data[:4] != b"dex\n":
        raise ValueError("not a dex file")
    string_ids_off = struct.unpack_from("<I", data, 0x3C)[0]
    type_ids_off = struct.unpack_from("<I", data, 0x44)[0]
    class_defs_size, class_defs_off = struct.unpack_from("<II", data, 0x60)

    def string(idx):
        off = struct.unpack_from("<I", data, string_ids_off + 4 * idx)[0]
        _, pos = uleb128(data, off)
        end = data.index(b"\0", pos)
        return data[pos:end].decode("utf-8", "replace")

    out = []
    for i in range(class_defs_size):
        type_idx = struct.unpack_from("<I", data, class_defs_off + 32 * i)[0]
        desc_idx = struct.unpack_from("<I", data, type_ids_off + 4 * type_idx)[0]
        out.append(string(desc_idx))
    return out


def jar_classes(path):
    out = []
    with zipfile.ZipFile(path) as z:
        for n in z.namelist():
            if re.fullmatch(r"classes\d*\.dex", n):
                out.extend(dex_classes(z.read(n)))
    return out


def required():
    want = {}
    for f in SOURCES:
        with open(os.path.join(RUNTIME, f), encoding="utf-8") as fh:
            for d in re.findall(r'"(L[A-Za-z0-9_/$]+;)"', fh.read()):
                want.setdefault(d, f)
    return want


def main():
    defined = {}
    for jar in BOOT:
        path = os.path.join(OUT, jar)
        classes = jar_classes(path)
        dups = len(classes) - len(set(classes))
        print(f"{jar}: {len(classes)} classes, {os.path.getsize(path)} bytes"
              + (f", {dups} duplicates" if dups else ""))
        for c in classes:
            if c in defined:
                print(f"  {c} defined in both {defined[c]} and {jar}")
            defined.setdefault(c, jar)
    for name in ("hello.dex", "bench.dex"):
        path = os.path.join(OUT, name)
        with open(path, "rb") as fh:
            classes = dex_classes(fh.read())
        print(f"{name}: {len(classes)} classes ({', '.join(classes)}), {os.path.getsize(path)} bytes")
    want = required()
    missing = sorted(d for d in want if d not in defined)
    print(f"ART well-known classes: {len(want)} named in {', '.join(SOURCES)}, "
          f"{len(want) - len(missing)} present, {len(missing)} missing")
    for d in missing:
        print(f"  missing {d} (named in {want[d]})")
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
