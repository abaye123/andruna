"""Leaves out the parts of the framework's sources that cannot be compiled
yet because what they use is not built (a module's jar, a HIDL interface, a
generator Andruna does not have yet), so that everything else is built.

From javac's errors, for each error the innermost member of the source file
that contains it is changed, in a copy of the file (out/framework/repaired):

  error in a method or constructor body   the body becomes
                                          `throw new UnsupportedOperationException(
                                          "Andruna: not built: <reason>")`
                                          (a constructor keeps its this()/super()
                                          call when the error is not in it)
  error in a field initializer            the initializer goes (and `final`)
  error in a signature or a field type    the member goes
  error in an initializer block           the block goes
  error in an import                      the import goes
  "might not have been initialized"       the field's `final` goes

Every change is recorded with javac's message (report.json, "not_built"). The
build repeats javac until the sources compile. This is the build's equivalent
of a compiler's "proceed on error": nothing reports success for an
operation it did not perform; a call into a part that is left out throws.
"""

import re
import sys
import os

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from stubgen import tokenize, TYPE_KEYWORDS  # noqa: E402

ERROR = re.compile(r"^(.*\.java):(\d+): error: (.*)$")


def parse_errors(log):
    """[(path, line, column, message, detail)] from javac's output."""
    lines = log.splitlines()
    out = []
    for i, ln in enumerate(lines):
        m = ERROR.match(ln)
        if not m:
            continue
        col = 0
        detail = []
        for j in range(i + 1, min(i + 8, len(lines))):
            if ERROR.match(lines[j]):
                break
            if lines[j].strip() == "^" or (lines[j].rstrip().endswith("^") and set(lines[j].strip()) == {"^"}):
                col = lines[j].index("^")
            s = re.match(r"\s+(symbol|location):\s+(.*)", lines[j])
            if s:
                detail.append(s.group(2).strip())
        msg = m.group(3)
        if detail:
            msg += " (" + ", ".join(detail) + ")"
        out.append((m.group(1), int(m.group(2)), col, msg))
    return out


class Member:
    def __init__(self, kind, start, end, name, header_end=None, body=None, eq=None, ctor_call=None):
        self.kind, self.start, self.end, self.name = kind, start, end, name
        self.header_end, self.body, self.eq, self.ctor_call = header_end, body, eq, ctor_call
        self.children = []


class Walker:
    """Members of a Java source file as character spans (types, methods,
    constructors, fields, initializer blocks, imports)."""

    def __init__(self, text):
        self.text = text
        self.t = tokenize(text)
        self.members = []

    def v(self, i):
        return self.t[i][1] if i < len(self.t) else None

    def match(self, i):
        open_ = self.v(i)
        close = {"{": "}", "(": ")", "[": "]"}[open_]
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

    def is_type_decl_at(self, i):
        x = self.v(i)
        if x in TYPE_KEYWORDS:
            return not (i > 0 and self.v(i - 1) == ".")
        if x == "@" and self.v(i + 1) == "interface":
            return True
        if x == "record" and i + 2 < len(self.t) and self.t[i + 1][0] == "id" and self.v(i + 2) in ("(", "<"):
            return True
        return False

    def find_body_open(self, i):
        while self.v(i) != "{":
            if self.v(i) == "(":
                i = self.match(i)
            i += 1
        return i

    def walk(self):
        i, n = 0, len(self.t)
        start = 0
        while i < n:
            x = self.v(i)
            if x == "import":
                j = i
                while self.v(j) != ";":
                    j += 1
                self.members.append(Member("import", self.t[i][2], self.t[j][3], "import"))
                i = j + 1
                start = i
                continue
            if self.is_type_decl_at(i):
                j = self.find_body_open(i)
                close = self.match(j)
                name = self.v(i + 2) if x == "@" else self.v(i + 1)
                m = Member("type", self.t[start][2], self.t[close][3], name, header_end=self.t[j][2])
                self.body(j + 1, close, m, x)
                self.members.append(m)
                i = close + 1
                start = i
                continue
            if x == ";":
                start = i + 1
            i += 1
        return self.members

    def body(self, i, end, parent, kind):
        if kind == "enum":
            while i < end:
                x = self.v(i)
                if x == ";":
                    i += 1
                    break
                if x == "(":
                    i = self.match(i) + 1
                    continue
                if x == "{":
                    close = self.match(i)
                    c = Member("type", self.t[i][2], self.t[close][3], "<enum constant>", header_end=self.t[i][2])
                    self.body(i + 1, close, c, "class")
                    parent.children.append(c)
                    i = close + 1
                    continue
                if x == "}":
                    break
                i += 1
        while i < end:
            i = self.member(i, end, parent)

    def member(self, i, end, parent):
        start = i
        saw_eq = saw_paren = None
        while i < end:
            x = self.v(i)
            if x == "@" and self.v(i + 1) != "interface":
                i += 2
                while self.v(i) == ".":
                    i += 2
                if self.v(i) == "(":
                    i = self.match(i) + 1
                continue
            if self.is_type_decl_at(i) and saw_eq is None and saw_paren is None:
                j = self.find_body_open(i)
                close = self.match(j)
                name = self.v(i + 2) if x == "@" else self.v(i + 1)
                m = Member("type", self.t[start][2], self.t[close][3], name, header_end=self.t[j][2])
                self.body(j + 1, close, m, x)
                parent.children.append(m)
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
                    i = self.match(i) + 1
                    continue
                close = self.match(i)
                if saw_paren is not None:
                    name = self.v(saw_paren - 1)
                    ctor = name == parent.name
                    call = None
                    if ctor and self.v(i + 1) in ("this", "super") and self.v(i + 2) == "(":
                        e = self.match(i + 2)
                        call = (self.t[i + 1][2], self.t[e + 1][3] if self.v(e + 1) == ";" else self.t[e][3])
                    parent.children.append(Member("ctor" if ctor else "method", self.t[start][2],
                                                  self.t[close][3], name, header_end=self.t[i][2],
                                                  body=(self.t[i][2], self.t[close][3]), ctor_call=call))
                else:
                    parent.children.append(Member("init", self.t[start][2], self.t[close][3], "<init>"))
                return close + 1
            if x == ";":
                if saw_paren is not None:
                    parent.children.append(Member("method", self.t[start][2], self.t[i][3],
                                                  self.v(saw_paren - 1), header_end=self.t[i][3]))
                elif i > start:
                    name = self.v(saw_eq - 1) if saw_eq is not None else self.v(i - 1)
                    parent.children.append(Member("field", self.t[start][2], self.t[i][3], name,
                                                  eq=(self.t[saw_eq][2], self.t[i][2]) if saw_eq is not None else None))
                return i + 1
            if x == "}":
                return i + 1
            i += 1
        return i


def innermost(members, pos):
    best = None
    stack = list(members)
    while stack:
        m = stack.pop()
        if m.start <= pos < m.end:
            if best is None or (m.end - m.start) < (best.end - best.start):
                best = m
            stack += m.children
    return best


def all_members(members):
    stack = list(members)
    while stack:
        m = stack.pop()
        yield m
        stack += m.children


def annotation_at(text, pos):
    """(start, end) of the annotation whose arguments contain pos."""
    toks = tokenize(text)
    idx = next((i for i, tk in enumerate(toks) if tk[2] <= pos < tk[3]), None)
    if idx is None:
        return None
    depth = 0
    i = idx
    while i > 0:
        v = toks[i][1]
        if v in (")", "}"):
            depth += 1
        elif v in ("(", "{"):
            if depth == 0 and v == "(":
                # name before '(' and '@' before the (qualified) name
                j = i - 1
                while j > 1 and toks[j - 1][1] == "." :
                    j -= 2
                if j >= 1 and toks[j - 1][1] == "@":
                    k, d = i, 0
                    while True:
                        if toks[k][1] == "(":
                            d += 1
                        elif toks[k][1] == ")":
                            d -= 1
                            if d == 0:
                                return toks[j - 1][2], toks[k][3]
                        k += 1
            if depth:
                depth -= 1
        elif v in (";",) and depth == 0:
            return None
        i -= 1
    return None


def java_string(s):
    return s.replace("\\", "\\\\").replace('"', '\\"')


def repair(text, errors):
    """(new text, [(member description, reason)]) for a file's errors
    [(line, column, message)]."""
    members = Walker(text).walk()
    line_start = [0]
    for i, c in enumerate(text):
        if c == "\n":
            line_start.append(i + 1)
    edits = {}  # member start -> (start, end, replacement, description, reason)
    finals = set()
    done_unhandled = []
    drop_file = []
    for line, col, msg in errors:
        m_init = re.search(r"variable (\w+) (?:might not have been|not) initialized", msg)
        if m_init:
            p0 = line_start[line - 1] + col if line - 1 < len(line_start) else 0
            fm = innermost(members, p0)
            if fm is not None and fm.kind == "field":
                # The declaration itself: its `final` goes.
                for k in re.finditer(r"\bfinal\b", text[fm.start:fm.end]):
                    s = fm.start + k.start()
                    edits[s] = (s, s + 5, "", f"final modifier of {m_init.group(1)}",
                                "its initialization is not built")
            else:
                finals.add(m_init.group(1))
            continue
        pos = line_start[line - 1] + col if line - 1 < len(line_start) else len(text) - 1
        if True:
            # Any error inside an annotation's arguments: the annotation goes.
            span = annotation_at(text, pos)
            if span:
                edits[span[0]] = (span[0], span[1], "", "annotation " + text[span[0]:span[0] + 60].split("(")[0],
                                  msg[:200])
                continue
        m = innermost(members, pos)
        if m is None:
            continue
        if m.kind == "type" and pos >= m.header_end:
            # In a type's body outside every member: an enum constant's
            # arguments. The type goes.
            if any(m is top for top in members):
                drop_file.append((f"type {m.name}", reason))
            else:
                edits[m.start] = (m.start, m.end, "", f"type {m.name}", reason)
            continue
        reason = msg[:200]
        if m.kind == "import":
            edits[m.start] = (m.start, m.end, "", "import", reason)
        elif m.kind in ("method", "ctor") and m.body and m.body[0] < pos < m.body[1]:
            keep = ""
            if m.kind == "ctor" and m.ctor_call and not (m.ctor_call[0] <= pos < m.ctor_call[1]):
                keep = text[m.ctor_call[0]:m.ctor_call[1]]
                if not keep.rstrip().endswith(";"):
                    keep += ";"
                keep += " "
            elif m.kind == "ctor" and m.ctor_call:
                edits[m.start] = (m.start, m.end, "", f"constructor {m.name}", reason)
                continue
            body = ("{ " + keep + 'throw new UnsupportedOperationException("Andruna: not built: '
                    + java_string(reason) + '"); }')
            if m.start not in edits:
                edits[m.start] = (m.body[0], m.body[1], body, f"{m.kind} {m.name}", reason)
        elif m.kind == "field" and m.eq and m.eq[0] <= pos:
            edits[m.start] = (m.eq[0], m.eq[1], "", f"initializer of {m.name}", reason)
            finals.add(m.name)
        elif m.kind == "type" and pos < m.header_end:
            # In a type's declaration (a supertype that is not built): the
            # type goes; a top-level type is left out of the build.
            if any(m is top for top in members):
                drop_file.append((f"type {m.name}", reason))
            else:
                edits[m.start] = (m.start, m.end, "", f"type {m.name}", reason)
        elif m.kind == "type":
            done_unhandled.append((f"type {m.name}", reason))
        else:
            edits[m.start] = (m.start, m.end, "", f"{m.kind} {m.name}", reason)
    # A dropped member wins over a body change inside it.
    spans = sorted(edits.values(), key=lambda e: (e[0], -(e[1] - e[0])))
    out, pos, done = [], 0, []
    for s, e, r, desc, reason in spans:
        if s < pos:
            continue
        out.append(text[pos:s])
        out.append(r)
        pos = e
        done.append((desc, reason))
    out.append(text[pos:])
    new = "".join(out)
    if finals:
        for name in finals:
            new2 = re.sub(r"\bfinal\b(?=[^;{}=]*\b" + re.escape(name) + r"\s*[;=])", "", new)
            if new2 != new:
                done.append((f"final modifier of {name}", "its initialization is not built"))
                new = new2
    if drop_file:
        return None, drop_file, done_unhandled
    return new, done, done_unhandled
