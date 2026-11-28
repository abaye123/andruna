"""Generates a statsd logging class (FrameworkStatsLog and the like), the
Java output of stats-log-api-gen (frameworks/proto_logging/stats/
stats_log_api_gen, java_writer.cpp) for one module: the atom code and enum
value constants, the write() methods for pushed atoms, buildStatsEvent() for
pulled atoms, write_non_chained() and, with --worksource, the WorkSource
variants. Atoms are read from atoms.proto and the extensions of Atom in
atoms/**/*.proto with gen_javastream.py's parser.

Difference to the C++ generator: the per-field and per-atom annotations
(is_uid, state options, truncate_timestamp, restriction categories) are not
written into the StatsEvent. Constants, method signatures and the values the
methods log are the same. Histogram helpers are not generated.

Usage: gen_statslog.py <out.java> <aosp_root> --module <m> --javaPackage <p>
       --javaClass <C> [--worksource]
"""

import argparse
import glob
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import gen_javastream as js  # noqa: E402

STATS = "frameworks/proto_logging/stats"
PULLED_MIN = 10000
PULLED_MAX = 99999
VENDOR_MIN = 100000

INT_TYPES = {"int32", "uint32", "fixed32", "sint32", "sfixed32"}
LONG_TYPES = {"int64", "uint64", "fixed64", "sint64", "sfixed64"}
ANNOTATIONS = ["ANNOTATION_ID_IS_UID", "ANNOTATION_ID_TRUNCATE_TIMESTAMP",
               "ANNOTATION_ID_PRIMARY_FIELD", "ANNOTATION_ID_EXCLUSIVE_STATE",
               "ANNOTATION_ID_PRIMARY_FIELD_FIRST_UID", "ANNOTATION_ID_DEFAULT_STATE",
               "ANNOTATION_ID_TRIGGER_STATE_RESET", "ANNOTATION_ID_STATE_NESTED"]


def opt(field, *names):
    return [v.strip('"') for k, v in field.options if k in names]


class Atom:
    def __init__(self, name, code, message, fields, pulled):
        self.name, self.code, self.message = name, code, message
        self.fields, self.pulled = fields, pulled  # fields: [(name, jtype, enum values)]


def java_type(tree, f, scope):
    t = f.type_name
    rep = f.label == "repeated"
    if t in INT_TYPES:
        base = "int"
    elif t in LONG_TYPES:
        base = "long"
    elif t in ("float", "bool", "string", "bytes", "double"):
        base = {"float": "float", "bool": "boolean", "string": "String", "bytes": "byte[]",
                "double": "double"}[t]
        if t == "bytes":
            return "byte[]", None
    else:
        full = tree.resolve(t, scope)
        if full is None:
            raise SystemExit(f"cannot resolve {t} in {scope}")
        if tree.symbols[full] == "enum":
            vals = enum_values(tree, full)
            return ("int[]" if rep else "int"), vals
        if full.endswith("AttributionNode") and rep:
            return "chain", None
        if opt(f, "log_mode", "(log_mode)", "(android.os.statsd.log_mode)") and \
                opt(f, "log_mode", "(log_mode)", "(android.os.statsd.log_mode)")[0] == "MODE_BYTES":
            return "byte[]", None
        return None, None  # not loggable as a field
    return (base + "[]" if rep else base), None


def enum_values(tree, full):
    for pf in tree.files.values():
        def walk_enums(enums, msgs):
            for e in enums:
                if e.full == full:
                    return e
            for m in msgs:
                r = walk_enums(m.enums, m.messages)
                if r:
                    return r
            return None
        e = walk_enums(pf.enums, pf.messages)
        if e:
            # stats-log-api-gen keeps one name per value (std::map<int, string>).
            vals = {}
            for n, v in e.values:
                vals.setdefault(v, n)
            return sorted(vals.items())
    raise SystemExit(f"enum {full} not found")


def find_message(tree, full):
    for pf in tree.files.values():
        stack = list(pf.messages)
        while stack:
            m = stack.pop()
            if m.full == full:
                return m
            stack += m.messages
    raise SystemExit(f"message {full} not found")


def collect(tree, module):
    atom_fields = []
    am = find_message(tree, "android.os.statsd.Atom")
    for f in am.fields:
        atom_fields.append((f, "android.os.statsd", f.oneof == "pulled"))
    for pf in tree.files.values():
        for target, f in pf.extends:
            if target.split(".")[-1] == "Atom":
                atom_fields.append((f, pf.package, PULLED_MIN <= f.number <= PULLED_MAX))
    atoms = []
    for f, scope, pulled in atom_fields:
        mods = opt(f, "(module_name)", "(android.os.statsd.module_name)", "(module)",
                   "(android.os.statsd.module)")
        if module not in mods:
            continue
        full = tree.resolve(f.type_name, scope)
        msg = find_message(tree, full)
        fields = []
        for af in sorted(msg.fields, key=lambda x: x.number):
            jt, vals = java_type(tree, af, msg.full)
            if jt is None:
                raise SystemExit(f"{msg.full}.{af.name}: unsupported field type {af.type_name}")
            fields.append((af.name, jt, vals))
        atoms.append(Atom(f.name, f.number, msg.name, fields, pulled))
    atoms.sort(key=lambda a: a.code)
    return atoms


def const(s):
    return js.make_constant_name(s)


def sig_params(sig):
    out = []
    for i, t in enumerate(sig, 1):
        if t == "chain":
            out.append("int[] uid, java.lang.String[] tag")
        else:
            out.append(f"{t} arg{i}")
    return "".join(", " + p for p in out)


WRITERS = {"boolean": "writeBoolean", "int": "writeInt", "float": "writeFloat", "long": "writeLong",
           "String": "writeString"}
ARRAYS = {"byte[]": ("writeByteArray", "byte"), "boolean[]": ("writeBooleanArray", "boolean"),
          "int[]": ("writeIntArray", "int"), "float[]": ("writeFloatArray", "float"),
          "long[]": ("writeLongArray", "long"), "String[]": ("writeStringArray", "String")}


def body(sig):
    out = ["        final StatsEvent.Builder builder = StatsEvent.newBuilder();\n",
           "        builder.setAtomId(code);\n"]
    for i, t in enumerate(sig, 1):
        if t == "chain":
            out.append("        builder.writeAttributionChain(\n"
                       "                null == uid ? new int[0] : uid,\n"
                       "                null == tag ? new String[0] : tag);\n")
        elif t in WRITERS:
            out.append(f"        builder.{WRITERS[t]}(arg{i});\n")
        else:
            w, el = ARRAYS[t]
            out.append(f"        builder.{w}(null == arg{i} ? new {el}[0] : arg{i});\n")
    return "".join(out)


def generate(atoms, package, cls, worksource):
    o = ["// This file is autogenerated\n\n", f"package {package};\n\n\n",
         "import android.os.Build;\n", "import android.util.StatsEvent;\n",
         "import android.util.StatsLog;\n\n\n",
         "/**\n * Utility class for logging statistics events.\n * @hide\n */\n",
         f"public final class {cls} {{\n", "    // Constants for atom codes.\n"]
    for a in atoms:
        o.append(f"\n    /**\n     * {a.message} {a.name}<br>\n     */\n")
        o.append(f"    public static final int {const(a.name)} = {a.code};\n")
    o.append("\n    // Constants for enum values.\n\n")
    for a in atoms:
        for name, jt, vals in a.fields:
            if vals is None:
                continue
            o.append(f"    // Values for {a.message}.{name}\n")
            for v, n in vals:
                o.append(f"    public static final int {const(a.message)}__{const(name)}__{const(n)} = {v};\n")
            o.append("\n")
    o.append("    // Annotation constants.\n")
    for n in ANNOTATIONS:
        o.append(f'    @android.annotation.SuppressLint("InlinedApi")\n'
                 f"    public static final byte {n} = StatsLog.{n};\n\n")
    o.append("\n    // Write methods\n")
    pushed = sorted({tuple(t for _, t, _ in a.fields) for a in atoms if not a.pulled})
    pulled = sorted({tuple(t for _, t, _ in a.fields) for a in atoms if a.pulled})
    for sig in pushed:
        o.append(f"    public static void write(int code{sig_params(sig)}) {{\n")
        o.append(body(sig))
        o.append("\n        builder.usePooledBuffer();\n        StatsLog.write(builder.build());\n    }\n\n")
    # Non-chained variants of signatures that start with an attribution chain.
    nonchained = sorted({("int", "String") + s[1:] for s in pushed if s and s[0] == "chain"})
    for sig in nonchained:
        o.append(f"    public static void write_non_chained(int code{sig_params(sig)}) {{\n")
        args = "".join(", new int[] {arg1}" if i == 1 else ", new java.lang.String[] {arg2}" if i == 2
                       else f", arg{i}" for i in range(1, len(sig) + 1))
        o.append(f"        write(code{args});\n    }}\n\n")
    for sig in pulled:
        o.append(f"    public static StatsEvent buildStatsEvent(int code{sig_params(sig)}) {{\n")
        o.append(body(sig))
        o.append("\n        return builder.build();\n    }\n\n")
    if worksource:
        o.append("    // WorkSource methods.\n")
        for sig in pushed:
            if sig.count("chain") != 1:
                continue
            at = sig.index("chain") + 1
            params = "".join(", android.os.WorkSource ws" if i == at else f", {t} arg{i}"
                             for i, t in enumerate(sig, 1))
            nc = "".join(", ws.getUid(i), ws.getPackageName(i)" if i == at else f", arg{i}"
                         for i in range(1, len(sig) + 1))
            wc = "".join(", wc.getUids(), wc.getTags()" if i == at else f", arg{i}"
                         for i in range(1, len(sig) + 1))
            o.append(f"\n    public static void write(int code{params}) {{\n"
                     f"        for (int i = 0; i < ws.size(); ++i) {{\n"
                     f"            write_non_chained(code{nc});\n        }}\n"
                     f"        java.util.List<android.os.WorkSource.WorkChain> workChains = ws.getWorkChains();\n"
                     f"        if (workChains != null) {{\n"
                     f"            for (android.os.WorkSource.WorkChain wc : workChains) {{\n"
                     f"                write(code{wc});\n            }}\n        }}\n    }}\n")
    o.append("}\n")
    return "".join(o)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("root")
    ap.add_argument("--module", required=True)
    ap.add_argument("--javaPackage", required=True)
    ap.add_argument("--javaClass", required=True)
    ap.add_argument("--worksource", action="store_true")
    a = ap.parse_args()
    root = os.path.abspath(a.root)
    tree = js.Tree([root, os.path.join(root, "external", "protobuf", "src")])
    files = [f"{STATS}/atoms.proto"] + sorted(
        os.path.relpath(p, root).replace(os.sep, "/")
        for p in glob.glob(os.path.join(root, *STATS.split("/"), "atoms", "**", "*.proto"), recursive=True))
    for f in files:
        tree.load(f)
    atoms = collect(tree, a.module)
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    with open(a.out, "w", encoding="utf-8", newline="\n") as f:
        f.write(generate(atoms, a.javaPackage, a.javaClass, a.worksource))
    print(f"statslog: {a.javaClass}: {len(atoms)} atoms")


if __name__ == "__main__":
    main()
