"""Generates the Java class of a .sysprop file, as the sysprop_java tool
(system/tools/sysprop, JavaGen.cpp) does for a sysprop_library: one class
named after the file's `module`, a typed static getter per property (and a
setter for writable ones), reading android.os.SystemProperties.

All scopes are generated (the platform's view, as framework.jar's static
libraries use them).

Usage: gen_sysprop.py <out_dir> <file.sysprop>...
"""

import os
import re
import sys

PARSERS = '''    private static Boolean tryParseBoolean(String str) {
        if (str == null) return null;
        switch (str.toLowerCase(Locale.US)) {
            case "1":
            case "true":
                return Boolean.TRUE;
            case "0":
            case "false":
                return Boolean.FALSE;
            default:
                return null;
        }
    }

    private static Integer tryParseInteger(String str) {
        try {
            return Integer.valueOf(str);
        } catch (NumberFormatException e) {
            return null;
        }
    }

    private static Integer tryParseUInt(String str) {
        try {
            return Integer.parseUnsignedInt(str);
        } catch (NumberFormatException e) {
            return null;
        }
    }

    private static Long tryParseLong(String str) {
        try {
            return Long.valueOf(str);
        } catch (NumberFormatException e) {
            return null;
        }
    }

    private static Long tryParseULong(String str) {
        try {
            return Long.parseUnsignedLong(str);
        } catch (NumberFormatException e) {
            return null;
        }
    }

    private static Double tryParseDouble(String str) {
        try {
            return Double.valueOf(str);
        } catch (NumberFormatException e) {
            return null;
        }
    }

    private static String tryParseString(String str) {
        return "".equals(str) ? null : str;
    }

    private static <T extends Enum<T>> T tryParseEnum(Function<String, T> enumParser, String str) {
        try {
            return enumParser.apply(str.toUpperCase(Locale.US));
        } catch (IllegalArgumentException e) {
            return null;
        }
    }

    private static <T> List<T> tryParseList(Function<String, T> elementParser, String str) {
        if ("".equals(str)) return new ArrayList<>();

        List<T> ret = new ArrayList<>();

        int p = 0;
        for (;;) {
            StringBuilder sb = new StringBuilder();
            while (p < str.length() && str.charAt(p) != ',') {
                if (str.charAt(p) == '\\\\') ++p;
                if (p == str.length()) break;
                sb.append(str.charAt(p++));
            }
            ret.add(elementParser.apply(sb.toString()));
            if (p == str.length()) break;
            ++p;
        }

        return ret;
    }

    private static <T extends Enum<T>> List<T> tryParseEnumList(Function<String, T> enumParser, String str) {
        if ("".equals(str)) return new ArrayList<>();

        List<T> ret = new ArrayList<>();

        for (String element : str.split(",")) {
            ret.add(tryParseEnum(enumParser, element));
        }

        return ret;
    }

    private static String escape(String str) {
        return str.replaceAll("([\\\\\\\\,])", "\\\\\\\\$1");
    }

    private static <T> String formatList(List<T> list) {
        StringJoiner joiner = new StringJoiner(",");

        for (T element : list) {
            joiner.add(element == null ? "" : escape(element.toString()));
        }

        return joiner.toString();
    }

    private static String formatUIntList(List<Integer> list) {
        StringJoiner joiner = new StringJoiner(",");

        for (Integer element : list) {
            joiner.add(element == null ? "" : escape(Integer.toUnsignedString(element)));
        }

        return joiner.toString();
    }

    private static String formatULongList(List<Long> list) {
        StringJoiner joiner = new StringJoiner(",");

        for (Long element : list) {
            joiner.add(element == null ? "" : escape(Long.toUnsignedString(element)));
        }

        return joiner.toString();
    }

    private static <T extends Enum<T>> String formatEnumList(List<T> list, Function<T, String> elementFormatter) {
        StringJoiner joiner = new StringJoiner(",");

        for (T element : list) {
            joiner.add(element == null ? "" : elementFormatter.apply(element));
        }

        return joiner.toString();
    }
'''

IMPORTS = '''import android.os.SystemProperties;
import android.util.Log;

import java.lang.StringBuilder;
import java.util.ArrayList;
import java.util.function.Function;
import java.util.List;
import java.util.Locale;
import java.util.Optional;
import java.util.StringJoiner;
import java.util.stream.Collectors;

'''

LISTS = {"BooleanList", "IntegerList", "LongList", "DoubleList", "StringList", "EnumList",
         "UIntList", "ULongList"}
ELEMENT = {"BooleanList": "Boolean", "IntegerList": "Integer", "LongList": "Long",
           "DoubleList": "Double", "StringList": "String", "UIntList": "UInt", "ULongList": "ULong"}
SCALAR_TYPE = {"Boolean": "Boolean", "Integer": "Integer", "UInt": "Integer", "Long": "Long",
               "ULong": "Long", "Double": "Double", "String": "String"}


def parse(path):
    text = open(path, encoding="utf-8").read()
    text = "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))
    module = re.search(r'^\s*module:\s*"([^"]+)"', text, re.M).group(1)
    props = []
    for m in re.finditer(r"\bprop\s*\{(.*?)\}", text, re.S):
        p = {}
        for k, v in re.findall(r'(\w+)\s*:\s*("(?:\\.|[^"\\])*"|\w+)', m.group(1)):
            p[k] = v[1:-1] if v.startswith('"') else v
        props.append(p)
    return module, props


def ident(name):
    return ("_" if name[0].isdigit() else "") + re.sub(r"[-.]", "_", name)


def enum_type(p):
    return ident(p["api_name"]) + "_values"


def java_type(p):
    t = p["type"]
    if t in SCALAR_TYPE:
        return SCALAR_TYPE[t]
    if t == "Enum":
        return enum_type(p)
    if t == "EnumList":
        return f"List<{enum_type(p)}>"
    return f"List<{SCALAR_TYPE[ELEMENT[t]]}>"


def parsing(p):
    t = p["type"]
    if t in SCALAR_TYPE:
        return f"Optional.ofNullable(tryParse{t}(value))"
    if t == "Enum":
        return f"Optional.ofNullable(tryParseEnum({enum_type(p)}::valueOf, value))"
    if t == "EnumList":
        return f"tryParseEnumList({enum_type(p)}::valueOf, value)"
    return f"tryParseList(v -> tryParse{ELEMENT[t]}(v), value)"


def formatting(p):
    t = p["type"]
    if p.get("integer_as_bool") == "true":
        if t == "Boolean":
            return '(value ? "1" : "0")'
        return 'value.stream().map(x -> x == null ? "" : (x ? "1" : "0")).collect(Collectors.joining(","))'
    return {"Enum": "value.getPropValue()",
            "EnumList": f"formatEnumList(value, {enum_type(p)}::getPropValue)",
            "UInt": "Integer.toUnsignedString(value)", "ULong": "Long.toUnsignedString(value)",
            "UIntList": "formatUIntList(value)", "ULongList": "formatULongList(value)"}.get(
        t, "formatList(value)" if t in LISTS else "value.toString()")


def generate(module, props):
    pkg, cls = module.rsplit(".", 1)
    o = ["// Generated by aosp/framework/gen_sysprop.py (the sysprop_java output).\n\n",
         f"package {pkg};\n\n", IMPORTS, f"public final class {cls} {{\n",
         f"    private {cls} () {{}}\n\n", PARSERS]
    for p in props:
        o.append("\n")
        pid, typ = ident(p["api_name"]), java_type(p)
        if p["type"] in ("Enum", "EnumList"):
            vals = p.get("enum_values", "").split("|")
            o.append(f"    public static enum {enum_type(p)} {{\n")
            o.append(",\n".join(f'        {v.upper()}("{v}")' for v in vals) + ";\n")
            o.append(f"        private final String propValue;\n"
                     f"        private {enum_type(p)}(String propValue) {{\n"
                     f"            this.propValue = propValue;\n        }}\n"
                     f"        public String getPropValue() {{\n            return propValue;\n        }}\n    }}\n\n")
        if p.get("deprecated") == "true":
            o.append("    @Deprecated\n")
        default = p.get("default_value", "")
        is_list = p["type"] in LISTS
        if is_list or default:
            o.append(f"    public static {typ} {pid}() {{\n")
        else:
            o.append(f"    public static Optional<{typ}> {pid}() {{\n")
        o.append(f'        String value = SystemProperties.get("{p["prop_name"]}");\n')
        if p.get("legacy_prop_name"):
            o.append(f'        if ("".equals(value)) {{\n'
                     f'            Log.v("{cls}", "prop {p["prop_name"]} doesn\'t exist; fallback to legacy prop {p["legacy_prop_name"]}");\n'
                     f'            value = SystemProperties.get("{p["legacy_prop_name"]}");\n        }}\n')
        if default:
            o.append(f'        if ("".equals(value)) {{\n            value = "{default}";\n        }}\n')
        if is_list or not default:
            o.append(f"        return {parsing(p)};\n    }}\n")
        else:
            o.append(f"        return {parsing(p)}.orElse(null);\n    }}\n")
        if p.get("access", "Readonly") != "Readonly":
            o.append("\n")
            if p.get("deprecated") == "true":
                o.append("    @Deprecated\n")
            o.append(f"    public static void {pid}({typ} value) {{\n"
                     f'        SystemProperties.set("{p["prop_name"]}", value == null ? "" : {formatting(p)});\n    }}\n')
    o.append("}\n")
    return pkg, cls, "".join(o)


def main(argv):
    if len(argv) < 3:
        raise SystemExit(__doc__)
    for path in argv[2:]:
        module, props = parse(path)
        pkg, cls, text = generate(module, props)
        d = os.path.join(argv[1], *pkg.split("."))
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, cls + ".java"), "w", encoding="utf-8", newline="\n") as f:
            f.write(text)
    print(f"sysprop: {len(argv) - 2} files")


if __name__ == "__main__":
    main(sys.argv)
