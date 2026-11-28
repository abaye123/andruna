"""Generates the Java constant classes that protoc-gen-javastream
(frameworks/base/tools/streaming_proto) writes for .proto files: for each
message a class of `long` field ids for android.util.proto.ProtoOutputStream,
for each enum `int` constants. Same names, values, layout and file split
as the C++ generator (java_proto_stream_code_generator.cpp), which runs as a
protoc plugin; this script parses the .proto files itself, so the build
needs neither protoc nor the plugin.

Usage: gen_javastream.py [options] <out_dir> <file.proto>...

  -I <dir>                 import root (protoc's -I), in order
  --filter <a,b,...>       protoc-gen-javastream's include_filter: only these
                           top-level messages and enums (full proto names)
  --rename <from>=<to>     Java package prefix rename, as Soong's jarjar rule
                           for these classes (perfetto.protos -> 
                           android.internal.perfetto.protos)

A file's proto name is its path relative to the first import root that
contains it, as protoc names it. Only names and field types are needed from
imported files.
"""

import os
import re
import sys

TYPES = {
    "double": 1, "float": 2, "int64": 3, "uint64": 4, "int32": 5, "fixed64": 6,
    "fixed32": 7, "bool": 8, "string": 9, "group": 10, "message": 11, "bytes": 12,
    "uint32": 13, "enum": 14, "sfixed32": 15, "sfixed64": 16, "sint32": 17, "sint64": 18,
}
FIELD_TYPE_SHIFT = 32
FIELD_COUNT_SINGLE = 1 << 40
FIELD_COUNT_REPEATED = 2 << 40
FIELD_COUNT_PACKED = 5 << 40
INDENT = "    "

TOKEN = re.compile(r'''\s+|//[^\n]*|/\*.*?\*/|("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')|([A-Za-z_][\w.]*)|(-?(?:0[xX][0-9a-fA-F]+|\d+(?:\.\d*)?(?:[eE][+-]?\d+)?))|(\.[A-Za-z_][\w.]*)|([{}\[\]();=<>,:.\-+/])''', re.S)


def tokenize(text, path):
    out, pos = [], 0
    while pos < len(text):
        m = TOKEN.match(text, pos)
        if not m:
            raise SystemExit(f"{path}: cannot tokenize at {text[pos:pos + 30]!r}")
        pos = m.end()
        for g in m.groups():
            if g is not None:
                out.append(g)
                break
    return out


class Field:
    def __init__(self, name, number, label, type_name, packed):
        self.name, self.number, self.label = name, number, label
        self.type_name, self.packed = type_name, packed
        self.resolved = None  # (type number, full type name or None)


class Message:
    def __init__(self, name, full):
        self.name, self.full = name, full
        self.enums, self.messages, self.fields, self.extensions = [], [], [], []


class Enum:
    def __init__(self, name, full):
        self.name, self.full, self.values = name, full, []


class ProtoFile:
    def __init__(self, path, name):
        self.path, self.name = path, name
        self.package = ""
        self.options = {}
        self.imports = []
        self.enums, self.messages = [], []


class Parser:
    def __init__(self, toks, path):
        self.t, self.i, self.path = toks, 0, path

    def peek(self, k=0):
        return self.t[self.i + k] if self.i + k < len(self.t) else None

    def take(self, want=None):
        tok = self.peek()
        if want is not None and tok != want:
            raise SystemExit(f"{self.path}: expected {want!r}, got {tok!r} (token {self.i})")
        self.i += 1
        return tok

    def skip_statement(self):
        """Skips to the end of a statement (`;`) or a balanced block."""
        depth = 0
        while True:
            tok = self.take()
            if tok in ("{", "["):
                depth += 1
            elif tok in ("}", "]"):
                depth -= 1
                if depth == 0 and tok == "}":
                    if self.peek() == ";":
                        self.take()
                    return
            elif tok == ";" and depth == 0:
                return

    def option_value(self):
        """Value of `option x = <value>;` (aggregates are skipped)."""
        if self.peek() == "{":
            self.skip_statement()
            return None
        v = self.take()
        if self.peek() == ";":
            self.take()
        return v

    def field_options(self):
        """[packed = true, (x).y = {...}, ...] -> packed flag."""
        packed = False
        self.take("[")
        depth = 1
        name = []
        while depth:
            tok = self.take()
            if tok in ("[", "{"):
                depth += 1
            elif tok in ("]", "}"):
                depth -= 1
            elif depth == 1 and tok == "=":
                key = "".join(name)
                val = self.peek()
                if key == "packed":
                    packed = val == "true"
                name = []
            elif depth == 1 and tok == ",":
                name = []
            elif depth == 1:
                name.append(tok)
        return packed

    def parse_file(self, pf):
        while self.peek() is not None:
            tok = self.peek()
            if tok == "syntax" or tok == "edition":
                self.skip_statement()
            elif tok == "package":
                self.take()
                pf.package = self.take()
                self.take(";")
            elif tok == "import":
                self.take()
                if self.peek() in ("public", "weak"):
                    self.take()
                pf.imports.append(self.take()[1:-1])
                self.take(";")
            elif tok == "option":
                self.take()
                name = self.take()
                if name == "(":
                    while self.take() != ")":
                        pass
                    while self.peek() != "=":
                        self.take()
                    name = None
                self.take("=")
                v = self.option_value()
                if name:
                    pf.options[name] = v
            elif tok == "message":
                pf.messages.append(self.parse_message(pf.package))
            elif tok == "enum":
                pf.enums.append(self.parse_enum(pf.package))
            elif tok in ("extend", "service"):
                self.take()
                self.take()
                self.skip_block()
            elif tok == ";":
                self.take()
            else:
                raise SystemExit(f"{self.path}: unexpected {tok!r} at top level")

    def skip_block(self):
        self.take("{")
        depth = 1
        while depth:
            tok = self.take()
            if tok == "{":
                depth += 1
            elif tok == "}":
                depth -= 1

    def parse_enum(self, scope):
        self.take("enum")
        name = self.take()
        e = Enum(name, f"{scope}.{name}" if scope else name)
        self.take("{")
        while self.peek() != "}":
            tok = self.peek()
            if tok in ("option", "reserved"):
                self.skip_statement()
            elif tok == ";":
                self.take()
            else:
                vname = self.take()
                self.take("=")
                num = self.take()
                if num == "-":
                    num = "-" + self.take()
                if self.peek() == "[":
                    self.field_options()
                self.take(";")
                e.values.append((vname, int(num, 0)))
        self.take("}")
        if self.peek() == ";":
            self.take()
        return e

    def parse_field(self, label, extension_target=None):
        type_name = self.take()
        if type_name == "map":
            self.take("<")
            ktype = self.take()
            self.take(",")
            vtype = self.take()
            self.take(">")
            name = self.take()
            self.take("=")
            number = int(self.take(), 0)
            if self.peek() == "[":
                self.field_options()
            self.take(";")
            # A map is a repeated message of the generated <Name>Entry type.
            entry = "".join(p[:1].upper() + p[1:] for p in name.split("_")) + "Entry"
            f = Field(name, number, "repeated", entry, False)
            f.is_map_entry = True
            f.entry_fields = [Field("key", 1, "optional", ktype, False),
                              Field("value", 2, "optional", vtype, False)]
            return f
        if type_name == "group":
            raise SystemExit(f"{self.path}: groups are not supported")
        name = self.take()
        self.take("=")
        number = int(self.take(), 0)
        packed = False
        if self.peek() == "[":
            packed = self.field_options()
        self.take(";")
        return Field(name, number, label, type_name, packed)

    def parse_message(self, scope):
        self.take("message")
        name = self.take()
        full = f"{scope}.{name}" if scope else name
        m = Message(name, full)
        self.take("{")
        while self.peek() != "}":
            tok = self.peek()
            if tok == "message":
                m.messages.append(self.parse_message(full))
            elif tok == "enum":
                m.enums.append(self.parse_enum(full))
            elif tok in ("option", "reserved", "extensions"):
                self.skip_statement()
            elif tok == ";":
                self.take()
            elif tok == "oneof":
                self.take()
                self.take()
                self.take("{")
                while self.peek() != "}":
                    if self.peek() == "option":
                        self.skip_statement()
                        continue
                    m.fields.append(self.parse_field("optional"))
                self.take("}")
            elif tok == "extend":
                self.take()
                self.take()
                self.take("{")
                while self.peek() != "}":
                    lab = "optional"
                    if self.peek() in ("optional", "repeated", "required"):
                        lab = self.take()
                    m.extensions.append(self.parse_field(lab))
                self.take("}")
            else:
                lab = "optional"
                if tok in ("optional", "repeated", "required"):
                    lab = self.take()
                f = self.parse_field(lab)
                m.fields.append(f)
                if getattr(f, "is_map_entry", False):
                    # The synthesized entry message is nested in this one.
                    entry = Message(f.type_name, f"{full}.{f.type_name}")
                    entry.fields = f.entry_fields
                    m.messages.append(entry)
        self.take("}")
        if self.peek() == ";":
            self.take()
        return m


class Tree:
    def __init__(self, roots):
        self.roots = roots
        self.files = {}
        self.symbols = {}  # full name -> "message" | "enum"

    def locate(self, name):
        for base in self.roots:
            p = os.path.join(base, *name.split("/"))
            if os.path.isfile(p):
                return p
        return None

    def proto_name(self, path):
        path = os.path.abspath(path)
        for base in self.roots:
            if path.startswith(base + os.sep):
                return os.path.relpath(path, base).replace(os.sep, "/")
        raise SystemExit(f"{path} is under no import root")

    def load(self, name):
        if name in self.files:
            return self.files[name]
        path = self.locate(name)
        if path is None:
            raise SystemExit(f"import {name} not found under {self.roots}")
        with open(path, encoding="utf-8") as f:
            text = f.read()
        pf = ProtoFile(path, name)
        self.files[name] = pf
        Parser(tokenize(text, path), path).parse_file(pf)
        for e in pf.enums:
            self.symbols[e.full] = "enum"

        def walk(m):
            self.symbols[m.full] = "message"
            for e in m.enums:
                self.symbols[e.full] = "enum"
            for n in m.messages:
                walk(n)
        for m in pf.messages:
            walk(m)
        for imp in pf.imports:
            self.load(imp)
        return pf

    def resolve(self, type_name, scope):
        """protoc's scoping: innermost scope outward; returns full name."""
        if type_name.startswith("."):
            full = type_name[1:]
            return full if full in self.symbols else None
        first = type_name.split(".")[0]
        parts = scope.split(".") if scope else []
        while True:
            prefix = ".".join(parts)
            cand_first = f"{prefix}.{first}" if prefix else first
            if cand_first in self.symbols or any(k.startswith(cand_first + ".") for k in self.symbols):
                full = f"{prefix}.{type_name}" if prefix else type_name
                return full if full in self.symbols else None
            if not parts:
                return None
            parts.pop()


def make_constant_name(s):
    out, underscore_next = [], False
    for c in s:
        if "A" <= c <= "Z":
            if underscore_next:
                out.append("_")
                underscore_next = False
        elif "a" <= c <= "z":
            c = c.upper()
            underscore_next = True
        elif c == "_":
            underscore_next = False
        out.append(c)
    return "".join(out)


def to_camel_case(s):
    out, cap = [], True
    for c in s:
        if c == "_":
            cap = True
            continue
        if cap and "a" <= c <= "z":
            c = c.upper()
            cap = False
        elif "A" <= c <= "Z":
            cap = False
        elif "0" <= c <= "9":
            cap = True
        else:
            cap = False
        out.append(c)
    return "".join(out)


class Gen:
    def __init__(self, tree, allow=None, rename=None):
        self.tree = tree
        self.allow = allow or set()
        self.rename = rename

    def field_type(self, f, scope):
        if f.type_name in TYPES and f.type_name not in ("message", "enum", "group"):
            return TYPES[f.type_name], f.type_name
        full = self.tree.resolve(f.type_name, scope)
        if full is None:
            raise SystemExit(f"cannot resolve type {f.type_name} in {scope}")
        kind = self.tree.symbols[full]
        return TYPES[kind], "." + full

    def write_enum(self, out, e, indent):
        out.append(f"{indent}// enum {e.name}\n")
        for name, num in e.values:
            out.append(f"{indent}public static final int {make_constant_name(name)} = {num};\n")
        out.append("\n")

    def write_field(self, out, f, scope, indent):
        tnum, tname = self.field_type(f, scope)
        opt = "optional " if f.label == "optional" else ""
        rep = "repeated " if f.label == "repeated" else ""
        packed = " [packed=true]" if f.packed else ""
        out.append(f"{indent}// {opt}{rep}{tname} {f.name} = {f.number}{packed};\n")
        fid = (f.number & 0xFFFFFFFF) | (tnum << FIELD_TYPE_SHIFT)
        if f.packed:
            fid |= FIELD_COUNT_PACKED
        elif f.label == "repeated":
            fid |= FIELD_COUNT_REPEATED
        else:
            fid |= FIELD_COUNT_SINGLE
        out.append(f"{indent}public static final long {make_constant_name(f.name)} = 0x{fid:016x}L;\n\n")

    def write_message(self, out, m, indent):
        inner = indent + INDENT
        out.append(f"{indent}// message {m.name}\n")
        out.append(f"{indent}public final class {m.name} {{\n\n")
        for e in m.enums:
            self.write_enum(out, e, inner)
        for n in m.messages:
            self.write_message(out, n, inner)
        for f in m.fields:
            self.write_field(out, f, m.full, inner)
        for f in m.extensions:
            self.write_field(out, f, m.full, inner)
        out.append(f"{indent}}}\n\n")

    def write_file(self, out_dir, pf, class_name, generate_outer, enums, messages):
        pkg = pf.options.get("java_package") or pf.package
        if pkg and pkg.startswith('"'):
            pkg = pkg[1:-1]
        if self.rename and (pkg == self.rename[0] or pkg.startswith(self.rename[0] + ".")):
            pkg = self.rename[1] + pkg[len(self.rename[0]):]
        text = ["// Generated by protoc-gen-javastream. DO NOT MODIFY.\n",
                f"// source: {pf.name}\n\n"]
        if pkg:
            text.append(f"package {pkg};\n\n")
        text.append("/** @hide */\n")
        if generate_outer:
            text.append(f"public final class {class_name} {{\n\n")
        indent = INDENT if generate_outer else ""
        for e in enums:
            self.write_enum(text, e, indent)
        for m in messages:
            self.write_message(text, m, indent)
        if generate_outer:
            text.append("}\n")
        d = os.path.join(out_dir, *pkg.split(".")) if pkg else out_dir
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, class_name + ".java"), "w", encoding="utf-8", newline="\n") as f:
            f.write("".join(text))

    def outer_class_name(self, pf, messages):
        name = pf.options.get("java_outer_classname")
        if name:
            return name.strip('"')
        base = os.path.basename(pf.name).split(".")[0]
        name = to_camel_case(base)
        while any(m.name == name for m in messages):
            name += "OuterClass"
        return name

    def allowed(self, x):
        return not self.allow or x.full in self.allow

    def generate(self, out_dir, pf):
        enums = [e for e in pf.enums if self.allowed(e)]
        messages = [m for m in pf.messages if self.allowed(m)]
        if pf.options.get("java_multiple_files") == "true":
            if pf.enums and (not self.allow or enums):
                self.write_file(out_dir, pf, self.outer_class_name(pf, []), True, enums, [])
            for m in messages:
                self.write_file(out_dir, pf, m.name, False, [], [m])
        elif not self.allow or enums or messages:
            self.write_file(out_dir, pf, self.outer_class_name(pf, messages), True,
                            enums, messages)


def main(argv):
    roots, allow, rename, rest = [], set(), None, []
    args = iter(argv[1:])
    for a in args:
        if a == "-I":
            roots.append(os.path.abspath(next(args)))
        elif a == "--filter":
            allow.update(x for x in next(args).split(",") if x)
        elif a == "--rename":
            rename = tuple(next(args).split("=", 1))
        elif a.startswith("@"):
            with open(a[1:], encoding="utf-8") as f:
                rest += [ln.strip() for ln in f if ln.strip()]
        else:
            rest.append(a)
    if len(rest) < 2 or not roots:
        raise SystemExit(__doc__)
    out_dir = rest[0]
    tree = Tree(roots)
    names = []
    for p in rest[1:]:
        name = tree.proto_name(p)
        tree.load(name)
        names.append(name)
    gen = Gen(tree, allow, rename)
    for name in names:
        gen.generate(out_dir, tree.files[name])
    print(f"javastream: {len(names)} files")


if __name__ == "__main__":
    main(sys.argv)
