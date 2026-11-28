"""Generates protobuf "nano" Java classes (the javanano runtime's API, as
Soong's `proto: { type: "nano" }` libraries produce with protoc's javanano
plugin, default options): one outer class per file, messages as public static
final classes with public fields, C-style enums (constants in the enclosing
scope), and clear(), writeTo(), computeSerializedSize(), mergeFrom(),
parseFrom() and emptyArray().

Supported: proto2 and proto3 scalar, string, bytes, enum and message fields,
repeated fields (packed ones are read in both encodings), oneof members as
plain fields. Not supported (rejected): maps, groups, extensions.

Usage: gen_nano.py [-I dir]... [--runtime <java package>] <out_dir> <file.proto>...
"""

import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import gen_javastream as js  # noqa: E402

KEYWORDS = {"abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class",
            "const", "continue", "default", "do", "double", "else", "enum", "extends", "final",
            "finally", "float", "for", "goto", "if", "implements", "import", "instanceof", "int",
            "interface", "long", "native", "new", "package", "private", "protected", "public",
            "return", "short", "static", "strictfp", "super", "switch", "synchronized", "this",
            "throw", "throws", "transient", "try", "void", "volatile", "while", "true", "false",
            "null"}

# proto type -> (java type, nano suffix of the Coded*BufferNano methods, wire type)
SCALARS = {
    "double": ("double", "Double", 1), "float": ("float", "Float", 5),
    "int64": ("long", "Int64", 0), "uint64": ("long", "UInt64", 0),
    "int32": ("int", "Int32", 0), "fixed64": ("long", "Fixed64", 1),
    "fixed32": ("int", "Fixed32", 5), "bool": ("boolean", "Bool", 0),
    "string": ("String", "String", 2), "bytes": ("byte[]", "Bytes", 2),
    "uint32": ("int", "UInt32", 0), "sfixed32": ("int", "SFixed32", 5),
    "sfixed64": ("long", "SFixed64", 1), "sint32": ("int", "SInt32", 0),
    "sint64": ("long", "SInt64", 0),
}
EMPTY = {"int": "INT", "long": "LONG", "float": "FLOAT", "double": "DOUBLE", "boolean": "BOOLEAN",
         "String": "STRING", "byte[]": "BYTES"}
DEFAULTS = {"int": "0", "long": "0L", "float": "0F", "double": "0D", "boolean": "false",
            "String": '""', "byte[]": "com.RT.WireFormatNano.EMPTY_BYTES"}


def lower_camel(name):
    out, up = [], False
    for i, c in enumerate(name):
        if c == "_":
            up = True
            continue
        out.append(c.upper() if up and out else c)
        up = False
    s = "".join(out)
    s = s[:1].lower() + s[1:]
    return s + "_" if s in KEYWORDS else s


class Gen:
    def __init__(self, tree, runtime):
        self.tree, self.rt = tree, runtime
        self.defaults = {}

    def java_name(self, full):
        """Java name of a message or enum full proto name."""
        for pf in self.tree.files.values():
            if pf.package and not (full == pf.package or full.startswith(pf.package + ".")):
                continue
            rest = full[len(pf.package) + 1:] if pf.package else full
            top = rest.split(".")[0]
            if any(m.name == top for m in pf.messages) or any(e.name == top for e in pf.enums):
                return pf, rest
        raise SystemExit(f"{full}: not found")

    def outer(self, pf):
        name = pf.options.get("java_outer_classname")
        if name:
            return name.strip('"')
        return js.to_camel_case(os.path.basename(pf.name).split(".")[0])

    def pkg(self, pf):
        # javanano puts the classes in <java_package>.nano.
        p = pf.options.get("java_package")
        return (p.strip('"') if p else pf.package) + ".nano"

    def multi(self, pf):
        """One file per top-level message with java_multiple_files."""
        return pf.options.get("java_multiple_files") == "true"

    def jref(self, full):
        pf, rest = self.java_name(full)
        top = rest.split(".")[0]
        if self.multi(pf) and any(m.name == top for m in pf.messages):
            return f"{self.pkg(pf)}.{rest}"
        return f"{self.pkg(pf)}.{self.outer(pf)}.{rest}"

    def field_info(self, f, scope):
        """(kind, java type, method suffix, wire type, type ref)."""
        if f.type_name in SCALARS:
            jt, suf, wt = SCALARS[f.type_name]
            return "scalar", jt, suf, wt, None
        if f.type_name == "group" or getattr(f, "is_map_entry", False):
            raise SystemExit(f"{scope}.{f.name}: maps and groups are not supported")
        full = self.tree.resolve(f.type_name, scope)
        if full is None:
            raise SystemExit(f"cannot resolve {f.type_name} in {scope}")
        if self.tree.symbols[full] == "enum":
            return "enum", "int", "Int32", 0, None
        return "message", self.jref(full), "Message", 2, self.jref(full)

    def default(self, jt):
        return DEFAULTS[jt].replace("com.RT", self.rt)

    def declared_default(self, f, scope, kind, jt):
        vals = [v for k, v in f.options if k == "default"]
        if not vals:
            return None
        v = vals[0]
        if kind == "enum":
            full = self.tree.resolve(f.type_name, scope)
            for pf in self.tree.files.values():
                stack = [(pf.enums, pf.messages)]
                while stack:
                    enums, msgs = stack.pop()
                    for e in enums:
                        if e.full == full:
                            return str(dict(e.values)[v])
                    for mm in msgs:
                        stack.append((mm.enums, mm.messages))
            raise SystemExit(f"{scope}.{f.name}: default {v} not found")
        if jt == "String":
            return v if v.startswith('"') else f'"{v}"'
        if jt == "long":
            return v + "L"
        if jt == "float":
            return v + "F"
        if jt == "double":
            return v + "D"
        return v

    def enum_consts(self, out, e, ind):
        out.append(f"{ind}// enum {e.name}\n")
        for n, v in e.values:
            out.append(f"{ind}public static final int {n} = {v};\n")
        out.append("\n")

    def message(self, out, m, ind, top):
        st = "static " if True else ""
        rt = self.rt
        out.append(f"{ind}public {st}final class {m.name} extends\n{ind}    {rt}.MessageNano {{\n\n")
        i2 = ind + "    "
        for e in m.enums:
            self.enum_consts(out, e, i2)
        out.append(f"{i2}private static volatile {m.name}[] _emptyArray;\n"
                   f"{i2}public static {m.name}[] emptyArray() {{\n"
                   f"{i2}  if (_emptyArray == null) {{\n"
                   f"{i2}    synchronized ({rt}.InternalNano.LAZY_INIT_LOCK) {{\n"
                   f"{i2}      if (_emptyArray == null) {{\n"
                   f"{i2}        _emptyArray = new {m.name}[0];\n"
                   f"{i2}      }}\n{i2}    }}\n{i2}  }}\n{i2}  return _emptyArray;\n{i2}}}\n\n")
        for n in m.messages:
            self.message(out, n, i2, False)
        infos = []
        for f in m.fields:
            kind, jt, suf, wt, ref = self.field_info(f, m.full)
            name = lower_camel(f.name)
            rep = f.label == "repeated"
            infos.append((f, kind, jt, suf, wt, name, rep))
            self.defaults[(m.full, f.name)] = self.declared_default(f, m.full, kind, jt)
            out.append(f"{i2}// {f.label} {f.type_name} {f.name} = {f.number};\n")
            if rep:
                out.append(f"{i2}public {jt}[] {name};\n\n")
            elif kind == "message":
                out.append(f"{i2}public {jt} {name};\n\n")
            else:
                out.append(f"{i2}public {jt} {name};\n\n")
        out.append(f"{i2}public {m.name}() {{\n{i2}  clear();\n{i2}}}\n\n")
        out.append(f"{i2}public {m.name} clear() {{\n")
        for f, kind, jt, suf, wt, name, rep in infos:
            if rep:
                if kind == "message":
                    out.append(f"{i2}  {name} = {jt}.emptyArray();\n")
                else:
                    out.append(f"{i2}  {name} = {rt}.WireFormatNano.EMPTY_{EMPTY[jt]}_ARRAY;\n")
            elif kind == "message":
                out.append(f"{i2}  {name} = null;\n")
            else:
                d = self.defaults.get((m.full, f.name)) or self.default(jt)
                out.append(f"{i2}  {name} = {d};\n")
        out.append(f"{i2}  cachedSize = -1;\n{i2}  return this;\n{i2}}}\n\n")
        # writeTo
        out.append(f"{i2}@Override\n{i2}public void writeTo({rt}.CodedOutputByteBufferNano output)\n"
                   f"{i2}    throws java.io.IOException {{\n")
        for f, kind, jt, suf, wt, name, rep in infos:
            n = f.number
            if rep:
                out.append(f"{i2}  if (this.{name} != null && this.{name}.length > 0) {{\n"
                           f"{i2}    for (int i = 0; i < this.{name}.length; i++) {{\n")
                if kind == "message" or jt == "String" or jt == "byte[]":
                    out.append(f"{i2}      {jt.replace('[]', '') if False else ''}"
                               f"if (this.{name}[i] != null) {{\n"
                               f"{i2}        output.write{suf}({n}, this.{name}[i]);\n{i2}      }}\n")
                else:
                    out.append(f"{i2}      output.write{suf}({n}, this.{name}[i]);\n")
                out.append(f"{i2}    }}\n{i2}  }}\n")
            else:
                out.append(f"{i2}  if ({self.nondefault(kind, jt, name, self.defaults.get((m.full, f.name)))}) {{\n"
                           f"{i2}    output.write{suf}({n}, this.{name});\n{i2}  }}\n")
        out.append(f"{i2}  super.writeTo(output);\n{i2}}}\n\n")
        # computeSerializedSize
        out.append(f"{i2}@Override\n{i2}protected int computeSerializedSize() {{\n"
                   f"{i2}  int size = super.computeSerializedSize();\n")
        for f, kind, jt, suf, wt, name, rep in infos:
            n = f.number
            if rep:
                out.append(f"{i2}  if (this.{name} != null && this.{name}.length > 0) {{\n"
                           f"{i2}    for (int i = 0; i < this.{name}.length; i++) {{\n")
                if kind == "message" or jt in ("String", "byte[]"):
                    out.append(f"{i2}      if (this.{name}[i] != null) {{\n"
                               f"{i2}        size += {rt}.CodedOutputByteBufferNano\n"
                               f"{i2}            .compute{suf}Size({n}, this.{name}[i]);\n{i2}      }}\n")
                else:
                    out.append(f"{i2}      size += {rt}.CodedOutputByteBufferNano\n"
                               f"{i2}          .compute{suf}Size({n}, this.{name}[i]);\n")
                out.append(f"{i2}    }}\n{i2}  }}\n")
            else:
                out.append(f"{i2}  if ({self.nondefault(kind, jt, name, self.defaults.get((m.full, f.name)))}) {{\n"
                           f"{i2}    size += {rt}.CodedOutputByteBufferNano\n"
                           f"{i2}        .compute{suf}Size({n}, this.{name});\n{i2}  }}\n")
        out.append(f"{i2}  return size;\n{i2}}}\n\n")
        # mergeFrom
        out.append(f"{i2}@Override\n{i2}public {m.name} mergeFrom(\n"
                   f"{i2}        {rt}.CodedInputByteBufferNano input)\n"
                   f"{i2}    throws java.io.IOException {{\n"
                   f"{i2}  while (true) {{\n{i2}    int tag = input.readTag();\n"
                   f"{i2}    switch (tag) {{\n{i2}      case 0:\n{i2}        return this;\n"
                   f"{i2}      default: {{\n"
                   f"{i2}        if (!{rt}.WireFormatNano.parseUnknownField(input, tag)) {{\n"
                   f"{i2}          return this;\n{i2}        }}\n{i2}        break;\n{i2}      }}\n")
        for f, kind, jt, suf, wt, name, rep in infos:
            tag = (f.number << 3) | wt
            read = f"input.readMessage(x)" if kind == "message" else f"input.read{suf}()"
            if not rep:
                out.append(f"{i2}      case {tag}: {{\n")
                if kind == "message":
                    out.append(f"{i2}        if (this.{name} == null) {{\n"
                               f"{i2}          this.{name} = new {jt}();\n{i2}        }}\n"
                               f"{i2}        input.readMessage(this.{name});\n")
                else:
                    out.append(f"{i2}        this.{name} = input.read{suf}();\n")
                out.append(f"{i2}        break;\n{i2}      }}\n")
                continue
            elem = jt
            new_elem = (lambda: f"new {elem}()") if kind == "message" else None
            out.append(f"{i2}      case {tag}: {{\n"
                       f"{i2}        int arrayLength = {rt}.WireFormatNano\n"
                       f"{i2}            .getRepeatedFieldArrayLength(input, {tag});\n"
                       f"{i2}        int i = this.{name} == null ? 0 : this.{name}.length;\n"
                       f"{i2}        {elem}[] newArray = new {self.array_new(elem, 'i + arrayLength')};\n"
                       f"{i2}        if (i != 0) {{\n"
                       f"{i2}          java.lang.System.arraycopy(this.{name}, 0, newArray, 0, i);\n{i2}        }}\n"
                       f"{i2}        for (; i < newArray.length - 1; i++) {{\n")
            if kind == "message":
                out.append(f"{i2}          newArray[i] = new {elem}();\n"
                           f"{i2}          input.readMessage(newArray[i]);\n")
            else:
                out.append(f"{i2}          newArray[i] = input.read{suf}();\n")
            out.append(f"{i2}          input.readTag();\n{i2}        }}\n")
            if kind == "message":
                out.append(f"{i2}        newArray[i] = new {elem}();\n"
                           f"{i2}        input.readMessage(newArray[i]);\n")
            else:
                out.append(f"{i2}        newArray[i] = input.read{suf}();\n")
            out.append(f"{i2}        this.{name} = newArray;\n{i2}        break;\n{i2}      }}\n")
            if wt != 2:
                ptag = (f.number << 3) | 2
                out.append(f"{i2}      case {ptag}: {{\n"
                           f"{i2}        int length = input.readRawVarint32();\n"
                           f"{i2}        int limit = input.pushLimit(length);\n"
                           f"{i2}        int arrayLength = 0;\n{i2}        int startPos = input.getPosition();\n"
                           f"{i2}        while (input.getBytesUntilLimit() > 0) {{\n"
                           f"{i2}          input.read{suf}();\n{i2}          arrayLength++;\n{i2}        }}\n"
                           f"{i2}        input.rewindToPosition(startPos);\n"
                           f"{i2}        int i = this.{name} == null ? 0 : this.{name}.length;\n"
                           f"{i2}        {elem}[] newArray = new {self.array_new(elem, 'i + arrayLength')};\n"
                           f"{i2}        if (i != 0) {{\n"
                           f"{i2}          java.lang.System.arraycopy(this.{name}, 0, newArray, 0, i);\n{i2}        }}\n"
                           f"{i2}        for (; i < newArray.length; i++) {{\n"
                           f"{i2}          newArray[i] = input.read{suf}();\n{i2}        }}\n"
                           f"{i2}        this.{name} = newArray;\n"
                           f"{i2}        input.popLimit(limit);\n{i2}        break;\n{i2}      }}\n")
        out.append(f"{i2}    }}\n{i2}  }}\n{i2}}}\n\n")
        out.append(f"{i2}public static {m.name} parseFrom(byte[] data)\n"
                   f"{i2}    throws {rt}.InvalidProtocolBufferNanoException {{\n"
                   f"{i2}  return {rt}.MessageNano.mergeFrom(new {m.name}(), data);\n{i2}}}\n\n"
                   f"{i2}public static {m.name} parseFrom(\n"
                   f"{i2}        {rt}.CodedInputByteBufferNano input)\n"
                   f"{i2}    throws java.io.IOException {{\n"
                   f"{i2}  return new {m.name}().mergeFrom(input);\n{i2}}}\n")
        out.append(f"{ind}}}\n\n")

    def array_new(self, elem, n):
        if elem.endswith("[]"):
            return f"{elem[:-2]}[{n}][]"
        return f"{elem}[{n}]"

    def nondefault(self, kind, jt, name, declared=None):
        if declared is not None and jt not in ("float", "double", "String"):
            return f"this.{name} != {declared}"
        if declared is not None and jt == "String":
            return f"!this.{name}.equals({declared})"
        if kind == "message":
            return f"this.{name} != null"
        if jt == "String":
            return f'!this.{name}.equals("")'
        if jt == "byte[]":
            return f"!java.util.Arrays.equals(this.{name}, {self.rt}.WireFormatNano.EMPTY_BYTES)"
        if jt == "float":
            return f"java.lang.Float.floatToIntBits(this.{name}) != java.lang.Float.floatToIntBits(0F)"
        if jt == "double":
            return f"java.lang.Double.doubleToLongBits(this.{name}) != java.lang.Double.doubleToLongBits(0D)"
        if jt == "boolean":
            return f"this.{name} != false"
        return f"this.{name} != 0"

    def write(self, out_dir, pkg, name, body):
        d = os.path.join(out_dir, *pkg.split("."))
        os.makedirs(d, exist_ok=True)
        head = f"// Generated by aosp/framework/gen_nano.py. Do not edit.\n\npackage {pkg};\n\n"
        with open(os.path.join(d, name + ".java"), "w", encoding="utf-8", newline="\n") as f:
            f.write(head + body)

    def generate(self, out_dir, pf):
        pkg, outer = self.pkg(pf), self.outer(pf)
        if self.multi(pf):
            if pf.enums:
                out = [f"@SuppressWarnings(\"hiding\")\npublic interface {outer} {{\n\n"]
                for e in pf.enums:
                    self.enum_consts(out, e, "  ")
                out.append("}\n")
                self.write(out_dir, pkg, outer, "".join(out))
            for m in pf.messages:
                out = []
                self.message(out, m, "", True)
                body = "".join(out).replace("public static final class", "public final class", 1)
                self.write(out_dir, pkg, m.name, "@SuppressWarnings(\"hiding\")\n" + body)
            return
        out = [f"@SuppressWarnings(\"hiding\")\npublic interface {outer} {{\n\n"]
        for e in pf.enums:
            self.enum_consts(out, e, "  ")
        for m in pf.messages:
            self.message(out, m, "  ", True)
        out.append("}\n")
        self.write(out_dir, pkg, outer, "".join(out))


def main(argv):
    roots, rt, rest = [], "com.google.protobuf.nano", []
    args = iter(argv[1:])
    for a in args:
        if a == "-I":
            roots.append(os.path.abspath(next(args)))
        elif a == "--runtime":
            rt = next(args)
        else:
            rest.append(a)
    if len(rest) < 2 or not roots:
        raise SystemExit(__doc__)
    tree = js.Tree(roots)
    names = []
    for p in rest[1:]:
        n = tree.proto_name(p)
        tree.load(n)
        names.append(n)
    g = Gen(tree, rt)
    for n in names:
        g.generate(rest[0], tree.files[n])
    print(f"nano: {len(names)} files")


if __name__ == "__main__":
    main(sys.argv)
