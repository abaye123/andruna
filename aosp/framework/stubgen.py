"""Compile-only API stubs from real Java sources: method and constructor
bodies become `throw new RuntimeException("Stub!")`, initializer blocks and
non-constant field initializers are removed, everything else (types,
signatures, annotations, constants) is kept.

The framework is compiled against the APIs of modules that are separate
jars on Android (Connectivity, Bluetooth, DeviceConfig, ...), as Soong does
with its `libs` stubs. Compiling those modules' sources in full would pull
in each module's own dependencies; their signatures are all the framework's
compiler needs. The stubs are never packaged (aosp/framework/build.py drops
every class that comes from them).

Usage: stubgen.py <out_dir> <source_root>...   (each root a package root)
"""

import os
import re
import sys

TOKEN = re.compile(r'''
    (?P<ws>\s+)
  | (?P<lc>//[^\n]*)
  | (?P<bc>/\*.*?\*/)
  | (?P<tb>"""(?:\\.|[^\\])*?""")
  | (?P<str>"(?:\\.|[^"\\\n])*")
  | (?P<chr>'(?:\\.|[^'\\\n])+')
  | (?P<id>[A-Za-z_$][\w$]*)
  | (?P<num>\d[\w.]*)
  | (?P<op>.)
''', re.S | re.X)

TYPE_KEYWORDS = {"class", "interface", "enum"}
PRIMITIVES = {"boolean", "byte", "char", "short", "int", "long", "float", "double"}
DEFAULTS = {"boolean": "false", "char": "'\\0'", "float": "0f", "double": "0d", "long": "0L"}
STUB_BODY = '{ throw new RuntimeException("Stub!"); }'


def tokenize(text):
    toks = []
    for m in TOKEN.finditer(text):
        kind = m.lastgroup
        if kind in ("ws", "lc", "bc"):
            continue
        toks.append((kind, m.group(), m.start(), m.end()))
    return toks


class Stubber:
    def __init__(self, text):
        self.text = text
        self.t = tokenize(text)
        self.edits = []  # (start, end, replacement)

    def v(self, i):
        return self.t[i][1] if i < len(self.t) else None

    def match(self, i):
        """Index of the token closing the bracket at i."""
        open_, close = self.v(i), {"{": "}", "(": ")", "[": "]"}[self.v(i)]
        depth = 0
        while True:
            x = self.v(i)
            if x == open_:
                depth += 1
            elif x == close:
                depth -= 1
                if depth == 0:
                    return i
            i += 1

    def run(self):
        i = 0
        n = len(self.t)
        while i < n:
            if self.is_type_decl_at(i):
                j = self.find_body_open(i)
                kind = self.type_kind(i, j)
                end = self.match(j)
                self.body(j + 1, end, kind, self.type_name(i, j))
                i = end + 1
            else:
                i += 1
        out = []
        pos = 0
        for s, e, r in sorted(self.edits):
            if s < pos:
                continue
            out.append(self.text[pos:s])
            out.append(r)
            pos = e
        out.append(self.text[pos:])
        return "".join(out)

    def is_type_decl_at(self, i):
        x = self.v(i)
        if x in TYPE_KEYWORDS:
            return not (i > 0 and self.v(i - 1) == ".")  # Foo.class
        if x == "@" and self.v(i + 1) == "interface":
            return True
        if x == "record" and self.t[i + 1][0] == "id" and self.v(i + 2) in ("(", "<"):
            return True
        return False

    def find_body_open(self, i):
        while self.v(i) != "{":
            if self.v(i) == "(":
                i = self.match(i)
            i += 1
        return i

    def type_kind(self, i, j):
        x = self.v(i)
        if x == "@":
            return "annotation"
        return x

    def type_name(self, i, j):
        k = i + 1
        if self.v(i) == "@":
            k = i + 2
        return self.v(k)

    def body(self, start, end, kind, name):
        i = start
        if kind == "enum":
            i = self.enum_constants(start, end, name)
        while i < end:
            i = self.member(i, end, kind, name)

    def enum_constants(self, i, end, name):
        while i < end:
            x = self.v(i)
            if x == ";":
                return i + 1
            if x == "(":
                i = self.match(i) + 1
                continue
            if x == "{":
                close = self.match(i)
                self.body(i + 1, close, "class", name)
                i = close + 1
                continue
            i += 1
        return i

    def member(self, i, end, kind, name):
        """Processes one member starting at i; returns the index after it."""
        start = i
        depth = 0
        saw_eq = None
        saw_paren = None
        while i < end:
            x = self.v(i)
            if x == "@" and self.v(i + 1) != "interface":
                # Annotation: name and optional arguments.
                i += 2
                while self.v(i) == ".":
                    i += 2
                if self.v(i) == "(":
                    i = self.match(i) + 1
                continue
            if self.is_type_decl_at(i) and saw_eq is None and saw_paren is None:
                j = self.find_body_open(i)
                close = self.match(j)
                if self.is_private(start, i):
                    self.drop(start, close)
                else:
                    self.body(j + 1, close, self.type_kind(i, j), self.type_name(i, j))
                return close + 1
            if x == "(":
                if saw_eq is None and saw_paren is None:
                    saw_paren = i
                i = self.match(i) + 1
                continue
            if x == "[":
                i = self.match(i) + 1
                continue
            if x == "=" and saw_eq is None and saw_paren is None:
                saw_eq = i
            if x == "{":
                if saw_eq is not None or self.v(i - 1) == "default":
                    i = self.match(i) + 1  # array initializer / anonymous class
                    continue
                close = self.match(i)
                if saw_paren is not None:
                    self.method_body(start, saw_paren, i, close, name)
                else:
                    # Initializer block.
                    self.edits.append((self.t[start][2], self.t[close][3], ""))
                return close + 1
            if x == ";":
                mods = self.modifiers(start, i)
                if saw_paren is None and "private" in mods and not {"static", "final"} <= mods:
                    self.drop(start, i)  # a private field (constants stay: others use them)
                elif saw_eq is not None:
                    self.field_init(start, saw_eq, i, kind)
                elif saw_paren is None:
                    self.blank_field(start, i, kind)
                return i + 1
            if x == "}":
                return i + 1
            i += 1
        return i

    def is_private(self, start, stop):
        return "private" in self.modifiers(start, stop)

    def drop(self, start, end_tok):
        self.edits.append((self.t[start][2], self.t[end_tok][3], ""))

    def method_body(self, start, paren, open_, close, name):
        ctor = self.v(paren - 1) == name
        if not ctor and self.is_private(start, paren):
            self.drop(start, close)
            return
        keep = ""
        if ctor and self.v(open_ + 1) in ("this", "super") and self.v(open_ + 2) == "(":
            e = self.match(open_ + 2)
            keep = self.text[self.t[open_ + 1][2]:self.t[e][3]] + "; "
        body = "{ " + keep + 'throw new RuntimeException("Stub!"); }'
        self.edits.append((self.t[open_][2], self.t[close][3], body))

    def modifiers(self, start, stop):
        return {self.v(k) for k in range(start, stop)}

    def field_type(self, start, eq):
        # The declarator name is the token before '=' (after any []).
        k = eq - 1
        while self.v(k) == "]":
            k -= 2
        return self.v(k - 1)

    def field_init(self, start, eq, semi, kind):
        mods = self.modifiers(start, eq)
        static_final = ("static" in mods and "final" in mods) or kind in ("interface", "annotation")
        typ = self.field_type(start, eq)
        if static_final and (typ in PRIMITIVES or typ == "String"):
            return  # a constant: kept as written
        # Not a constant: the initializer goes.
        if kind in ("interface", "annotation"):
            default = DEFAULTS.get(typ, "0" if typ in PRIMITIVES else "null")
            self.edits.append((self.t[eq][2], self.t[semi][2], "= " + default))
            return
        self.edits.append((self.t[eq][2], self.t[semi][2], ""))
        self.drop_final(start, eq)

    def blank_field(self, start, semi, kind):
        if kind in ("interface", "annotation"):
            return
        self.drop_final(start, semi)

    def drop_final(self, start, stop):
        for k in range(start, stop):
            if self.v(k) == "final":
                self.edits.append((self.t[k][2], self.t[k][3], ""))


def stub_file(src, dst):
    with open(src, encoding="utf-8", errors="replace") as f:
        text = f.read()
    try:
        out = Stubber(text).run()
    except (IndexError, KeyError, TypeError) as e:
        raise SystemExit(f"{src}: cannot stub ({e})")
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    with open(dst, "w", encoding="utf-8", newline="\n") as f:
        f.write(out)


def main(argv):
    if len(argv) < 3:
        raise SystemExit(__doc__)
    out = argv[1]
    n = 0
    for root in argv[2:]:
        for d, _, files in os.walk(root):
            for name in files:
                if name.endswith(".java"):
                    src = os.path.join(d, name)
                    stub_file(src, os.path.join(out, os.path.relpath(src, root)))
                    n += 1
    print(f"stubgen: {n} files")


if __name__ == "__main__":
    main(sys.argv)
