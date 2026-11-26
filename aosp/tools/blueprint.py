#!/usr/bin/env python3
"""Reads Android.bp files and flattens modules for one architecture.

The AOSP modules of aosp/modules/ mirror Android.bp modules. For small
modules the source lists are written out by hand; bionic's libraries have
hundreds of sources, globs, defaults and per-architecture lists, so the
bionic modules ask this tool for the flattened view instead and only spell
out Andruna's own changes (see aosp/modules/bionic.cmake).

Supported Blueprint: variables (`x = ...`, `x += ...`), modules
(`type { ... }`), strings, booleans, integers, lists, maps, `+`
concatenation, and `select(...)`, for which the `default` branch is taken
(release flags and product variables keep their default values).

Flattening merges `defaults` (recursively), then the architecture blocks:
`arch: { <arch>: ... }`, `multilib: { lib64: ... }` and the
`target: { android: ..., bionic: ..., linux_bionic: ... }` choices of a
device build, and optionally the `static:` or `shared:` block. Lists
concatenate, maps merge, scalars override, as Soong does.

Usage (writes CMake variables for each module):

    blueprint.py --arch x86_64 --aosp <dir> --out <file.cmake>
        --module <bp file>:<module>[:static|:shared] ...
"""

import argparse
import glob
import os
import re
import sys
from pathlib import Path


class ParseError(Exception):
    pass


TOKEN = re.compile(
    r"""
    (?P<ws>\s+|//[^\n]*|/\*.*?\*/) |
    (?P<str>"(?:\\.|[^"\\])*"|`[^`]*`) |
    (?P<num>-?\d+) |
    (?P<id>[A-Za-z_][A-Za-z0-9_.]*) |
    (?P<op>\+=|[{}\[\]():,=+])
    """,
    re.S | re.X,
)


def tokenize(text):
    pos = 0
    out = []
    while pos < len(text):
        m = TOKEN.match(text, pos)
        if not m:
            raise ParseError(f"bad character at {pos}: {text[pos:pos + 20]!r}")
        pos = m.end()
        kind = m.lastgroup
        if kind == "ws":
            continue
        v = m.group(kind)
        if kind == "str":
            if v[0] == "`":
                v = v[1:-1]
            else:
                v = bytes(v[1:-1], "utf-8").decode("unicode_escape")
            out.append(("str", v))
        elif kind == "num":
            out.append(("num", int(v)))
        else:
            out.append((kind, v))
    out.append(("eof", None))
    return out


class Select:
    """A select() whose default branch is used."""

    def __init__(self, default):
        self.default = default


class Parser:
    def __init__(self, text):
        self.t = tokenize(text)
        self.i = 0
        self.vars = {}
        self.modules = []

    def peek(self, k=0):
        return self.t[self.i + k]

    def take(self, kind=None, value=None):
        tok = self.t[self.i]
        if kind and tok[0] != kind or value is not None and tok[1] != value:
            raise ParseError(f"expected {kind} {value!r}, got {tok!r}")
        self.i += 1
        return tok

    def parse(self):
        while self.peek()[0] != "eof":
            name = self.take("id")[1]
            nxt = self.peek()
            if nxt == ("op", "="):
                self.take()
                self.vars[name] = self.expr()
            elif nxt == ("op", "+="):
                self.take()
                self.vars[name] = add(self.vars[name], self.expr())
            elif nxt == ("op", "{"):
                props = self.map_body()
                props["__type"] = name
                self.modules.append(props)
            else:
                raise ParseError(f"unexpected {nxt!r} after {name}")
        return self

    def map_body(self):
        self.take("op", "{")
        d = {}
        while self.peek() != ("op", "}"):
            k = self.take()[1]
            self.take("op", ":")
            d[k] = self.expr()
            if self.peek() == ("op", ","):
                self.take()
        self.take("op", "}")
        return d

    def expr(self):
        v = self.atom()
        while self.peek() == ("op", "+"):
            self.take()
            v = add(v, self.atom())
        return v

    def atom(self):
        kind, v = self.peek()
        if kind == "str" or kind == "num":
            self.take()
            return v
        if kind == "op" and v == "[":
            self.take()
            items = []
            while self.peek() != ("op", "]"):
                items.append(self.expr())
                if self.peek() == ("op", ","):
                    self.take()
            self.take("op", "]")
            return items
        if kind == "op" and v == "{":
            return self.map_body()
        if kind == "id":
            self.take()
            if v in ("true", "false"):
                return v == "true"
            if v == "select":
                return self.select()
            if v == "unset":
                return None
            if v not in self.vars:
                raise ParseError(f"unknown variable {v}")
            return self.vars[v]
        raise ParseError(f"unexpected {kind} {v!r}")

    def select(self):
        # select(<condition>, { <pattern>: <value>, ..., default: <value> })
        self.take("op", "(")
        depth = 0
        while not (depth == 0 and self.peek() == ("op", ",")):
            tok = self.take()
            if tok == ("op", "("):
                depth += 1
            elif tok == ("op", ")"):
                depth -= 1
        self.take("op", ",")
        self.take("op", "{")
        default = None
        while self.peek() != ("op", "}"):
            # A pattern: string, bool, `default`, `any`, or a tuple.
            if self.peek() == ("op", "("):
                depth = 0
                while True:
                    tok = self.take()
                    if tok == ("op", "("):
                        depth += 1
                    elif tok == ("op", ")"):
                        depth -= 1
                        if depth == 0:
                            break
                key = None
            else:
                key = self.take()[1]
            self.take("op", ":")
            val = self.expr()
            if key == "default":
                default = val
            if self.peek() == ("op", ","):
                self.take()
        self.take("op", "}")
        if self.peek() == ("op", ","):
            self.take()
        self.take("op", ")")
        return Select(default)


def resolve(v):
    while isinstance(v, Select):
        v = v.default
    if isinstance(v, list):
        out = []
        for x in v:
            x = resolve(x)
            if x is not None:
                out.append(x)
        return out
    if isinstance(v, dict):
        return {k: resolve(x) for k, x in v.items()}
    return v


def add(a, b):
    a, b = resolve(a), resolve(b)
    if a is None:
        return b
    if b is None:
        return a
    if isinstance(a, list):
        return a + b
    if isinstance(a, dict):
        return merge(a, b)
    return a + b


def merge(a, b):
    """Soong's property merge: b appended to (or overriding) a."""
    out = dict(a)
    for k, v in b.items():
        if k in out and isinstance(out[k], list) and isinstance(v, list):
            out[k] = out[k] + v
        elif k in out and isinstance(out[k], dict) and isinstance(v, dict):
            out[k] = merge(out[k], v)
        else:
            out[k] = v
    return out


ARCH_BLOCKS = {"x86_64": ["x86_64"], "arm64": ["arm64"]}
TARGETS = ["android", "bionic", "linux_bionic", "linux", "android_" + "x86_64"]


class Tree:
    """All Android.bp files read so far, with modules by name."""

    def __init__(self, aosp, arch):
        self.aosp = Path(aosp)
        self.arch = arch
        self.files = {}
        self.by_name = {}

    def read(self, bp):
        bp = Path(bp)
        if not bp.is_absolute():
            bp = self.aosp / bp
        bp = bp.resolve()
        if bp in self.files:
            return
        p = Parser(bp.read_text(encoding="utf-8")).parse()
        self.files[bp] = p
        for m in p.modules:
            m = resolve(m)
            m["__dir"] = bp.parent
            if "name" in m:
                self.by_name.setdefault(m["name"], m)

    def module(self, name):
        if name not in self.by_name:
            raise KeyError(f"module {name} not found in the Android.bp files read")
        return self.by_name[name]

    def flatten(self, name, variant=None):
        m = self.module(name)
        props = {}
        for d in m.get("defaults", []):
            if d in self.by_name:
                props = merge(props, self.flatten(d, variant))
        own = {k: v for k, v in m.items() if k != "defaults"}
        props = merge(props, own)
        return self.select_blocks(props, variant)

    def select_blocks(self, props, variant):
        out = {k: v for k, v in props.items()
               if k not in ("arch", "multilib", "target", "static", "shared")}
        out["__dir"] = props["__dir"]
        blocks = []
        arch = props.get("arch", {})
        for a in ARCH_BLOCKS[self.arch]:
            if a in arch:
                blocks.append(arch[a])
        ml = props.get("multilib", {})
        if "lib64" in ml:
            blocks.append(ml["lib64"])
        tg = props.get("target", {})
        for t in ("android", "bionic", f"android_{self.arch}", f"bionic_{self.arch}"):
            if t in tg:
                blocks.append(tg[t])
        if variant and variant in props:
            blocks.append(props[variant])
        for b in blocks:
            b = dict(b)
            b["__dir"] = props["__dir"]
            # Nested arch blocks inside target/static blocks.
            out = merge(out, self.select_blocks(b, None))
            out["__dir"] = props["__dir"]
        return out

    def sources(self, props):
        """Expanded source paths (absolute) of flattened properties."""
        d = props["__dir"]
        excl = set()
        for e in props.get("exclude_srcs", []):
            excl.update(self.expand(d, e))
        out = []
        for s in props.get("srcs", []):
            for f in self.expand(d, s):
                if f not in excl and f not in out:
                    out.append(f)
        return out

    def expand(self, d, pattern):
        if pattern.startswith(":"):
            fg = self.module(pattern[1:])
            if fg["__type"] not in ("filegroup",):
                # A generated source (genrule); the caller provides it.
                return [pattern]
            p = self.select_blocks(fg, None)
            return self.sources(p)
        if "*" in pattern:
            hits = glob.glob(str(d / pattern), recursive=True)
            return sorted(str(Path(h).resolve()) for h in hits if Path(h).is_file())
        return [str((d / pattern).resolve())]

    def include_dirs(self, props):
        d = props["__dir"]
        dirs = [str((d / x).resolve()) for x in props.get("local_include_dirs", [])]
        # Soong adds the module's directory (include_build_directory).
        if props.get("include_build_directory", True):
            dirs.append(str(d.resolve()))
        dirs += [str((d / x).resolve()) for x in props.get("export_include_dirs", [])]
        dirs += [str((self.aosp / x).resolve()) for x in props.get("include_dirs", [])]
        return dirs

    def header_dirs(self, names, seen=None):
        """Exported include directories of header libraries (recursive)."""
        seen = seen or set()
        out = []
        for n in names:
            if n in seen or n not in self.by_name:
                continue
            seen.add(n)
            p = self.flatten(n)
            d = p["__dir"]
            for x in p.get("export_include_dirs", []) + p.get("export_system_include_dirs", []):
                out.append(str((d / x).resolve()))
            out += self.header_dirs(
                p.get("header_libs", []) + p.get("export_header_lib_headers", []), seen)
        return out


def cmake_list(values):
    return " ".join('"' + str(v).replace("\\", "/").replace('"', '\\"') + '"' for v in values)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arch", required=True, choices=sorted(ARCH_BLOCKS))
    ap.add_argument("--aosp", required=True)
    ap.add_argument("--read", action="append", default=[],
                    help="extra Android.bp files to read (header libraries, filegroups)")
    ap.add_argument("--module", action="append", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    tree = Tree(a.aosp, a.arch)
    for bp in a.read:
        tree.read(bp)
    lines = ["# Generated by aosp/tools/blueprint.py. Do not edit.\n"]
    for spec in a.module:
        parts = spec.split(":")
        bp, name = parts[0], parts[1]
        variant = parts[2] if len(parts) > 2 else None
        tree.read(bp)
        p = tree.flatten(name, variant)
        var = (name + ("_" + variant if variant else "")).replace("-", "_").replace("+", "x")
        flags = p.get("cflags", [])
        lines.append(f"set(BP_{var}_SRCS {cmake_list(tree.sources(p))})")
        lines.append(f"set(BP_{var}_CFLAGS {cmake_list(flags)})")
        lines.append(f"set(BP_{var}_CONLYFLAGS {cmake_list(p.get('conlyflags', []))})")
        lines.append(f"set(BP_{var}_CPPFLAGS {cmake_list(p.get('cppflags', []))})")
        lines.append(f"set(BP_{var}_ASFLAGS {cmake_list(p.get('asflags', []))})")
        lines.append(f"set(BP_{var}_INCLUDES {cmake_list(tree.include_dirs(p))})")
        lines.append(f"set(BP_{var}_HEADER_INCLUDES {cmake_list(tree.header_dirs(p.get('header_libs', [])))})")
        lines.append(f"set(BP_{var}_WHOLE {cmake_list(p.get('whole_static_libs', []))})")
        lines.append(f"set(BP_{var}_STATIC {cmake_list(p.get('static_libs', []))})")
        vs = p.get("version_script")
        lines.append(f"set(BP_{var}_VERSION_SCRIPT {cmake_list([vs] if vs else [])})")
        lines.append("")
    Path(a.out).parent.mkdir(parents=True, exist_ok=True)
    text = "\n".join(lines) + "\n"
    if not Path(a.out).exists() or Path(a.out).read_text() != text:
        Path(a.out).write_text(text)


if __name__ == "__main__":
    sys.exit(main())
