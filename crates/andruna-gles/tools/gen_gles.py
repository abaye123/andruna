#!/usr/bin/env python3
"""Generates the OpenGL ES and EGL entry point tables of andruna-gles.

Inputs (ANGLE at the pinned AOSP tag, third_party/aosp/external/angle):
  src/libEGL/libEGL_autogen.def, src/libGLESv2/libGLESv2_autogen.def
      the functions ANGLE exports, grouped by version and extension
  include/EGL/*.h, include/GLES/*.h, include/GLES2/*.h, include/GLES3/*.h
      the Khronos prototypes (the signature of each function)

Outputs (checked in, regenerate after an ANGLE update):
  crates/andruna-gles/src/entries.rs
      one `entries!` line per function: slot index, library, name,
      parameter types and result type. The macro in src/thunk.rs makes the
      System V to Microsoft x64 thunks from it.
  crates/andruna-gles/guest/lib<name>.c
      the guest stub libraries (libEGL.so, libGLESv1_CM.so, libGLESv2.so,
      libGLESv3.so, libandruna_window.so): each exported function jumps
      through a slot that the library's constructor fills by name with
      `__andruna_gles_resolve`, a host function.

Usage: python crates/andruna-gles/tools/gen_gles.py [--angle <dir>]
"""

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
CRATE = ROOT / "crates" / "andruna-gles"

# C type -> Rust type of a thunk parameter or result. Every pointer,
# handle and function pointer is `usize`: both conventions pass them as
# integers of the same size.
SCALARS = {
    "GLenum": "u32", "GLbitfield": "u32", "GLuint": "u32", "GLint": "i32",
    "GLsizei": "i32", "GLfixed": "i32", "GLclampx": "i32", "GLboolean": "u8",
    "GLubyte": "u8", "GLbyte": "i8", "GLshort": "i16", "GLushort": "u16",
    "GLfloat": "f32", "GLclampf": "f32", "GLdouble": "f64", "GLint64": "i64", "GLint64EXT": "i64",
    "GLuint64": "u64", "GLuint64EXT": "u64", "GLintptr": "isize",
    "GLsizeiptr": "isize", "GLintptrEXT": "isize", "GLsizeiptrEXT": "isize",
    "GLhalf": "u16", "GLhalfNV": "u16", "GLchar": "i8",
    "EGLint": "i32", "EGLBoolean": "u32", "EGLenum": "u32",
    "EGLAttrib": "isize", "EGLAttribKHR": "isize", "EGLTime": "u64",
    "EGLTimeKHR": "u64", "EGLTimeNV": "u64", "EGLuint64KHR": "u64",
    "EGLuint64NV": "u64", "EGLnsecsANDROID": "i64", "EGLsizeiANDROID": "isize",
    "EGLFrameTokenANGLE": "u64",
    "int": "i32", "unsigned int": "u32", "void": None,
}
HANDLES = {
    "GLsync", "GLeglImageOES", "GLeglClientBufferEXT", "GLDEBUGPROC",
    "GLDEBUGPROCKHR", "GLVULKANPROCNV", "GLSETBLOBPROCANGLE", "GLGETBLOBPROCANGLE",
    "EGLDisplay", "EGLConfig", "EGLContext", "EGLSurface", "EGLClientBuffer",
    "EGLImage", "EGLImageKHR", "EGLSync", "EGLSyncKHR", "EGLSyncNV",
    "EGLStreamKHR", "EGLDeviceEXT", "EGLLabelKHR", "EGLObjectKHR",
    "EGLNativeDisplayType", "EGLNativeWindowType", "EGLNativePixmapType",
    "EGLDEBUGPROCKHR", "EGLSetBlobFuncANDROID", "EGLGetBlobFuncANDROID",
    "EGLOutputLayerEXT", "EGLOutputPortEXT", "EGLNativeFileDescriptorKHR",
    "EGLClientPixmapHI", "__eglMustCastToProperFunctionPointerType",
}

PROTO = re.compile(
    r"^(?:GL_APICALL|EGLAPI|GL_API)\s+(.+?)\s*(?:GL_APIENTRY|EGLAPIENTRY)\s+(\w+)\s*\((.*)\)\s*;")


def rust_type(ctype, where):
    t = ctype.replace("const", " ").replace("struct", " ")
    if "*" in t or "[" in t:
        return "usize"
    t = " ".join(t.split())
    if t in HANDLES:
        return "usize"
    if t in SCALARS:
        return SCALARS[t]
    sys.exit(f"unknown C type `{ctype}` in {where}")


def parse_params(text, where):
    text = text.strip()
    if text in ("", "void"):
        return []
    out = []
    for i, p in enumerate(text.split(",")):
        p = p.strip()
        arr = "[" in p
        p = re.sub(r"\[.*?\]", "", p)
        bare = " ".join(p.replace("const", " ").split())
        if bare in SCALARS or bare in HANDLES or bare.endswith("*"):
            out.append(rust_type(p + ("*" if arr else ""), where))
            continue
        m = re.match(r"^(.*?)(\w+)$", p)
        if not m or not m.group(1).strip():
            sys.exit(f"cannot parse parameter `{p}` in {where}")
        ctype = m.group(1) + ("*" if arr else "")
        out.append(rust_type(ctype, where))
    return out


def prototypes(angle):
    """name -> (params, result, header)."""
    protos = {}
    for d in ("EGL", "GLES", "GLES2", "GLES3"):
        for h in sorted((angle / "include" / d).glob("*.h")):
            for line in h.read_text(encoding="utf-8", errors="replace").splitlines():
                m = PROTO.match(line.strip())
                if not m:
                    continue
                ret, name, params = m.groups()
                where = f"{h.name}: {name}"
                r = rust_type(ret, where)
                sig = (tuple(parse_params(params, where)), r)
                key = f"{d}/{h.name}"
                if name in protos:
                    if protos[name][:2] != sig:
                        sys.exit(f"conflicting prototypes of {name}")
                    protos[name][2].add(key)
                else:
                    protos[name] = (sig[0], sig[1], {key})
    return protos


def def_sections(path):
    """[(section, [names])] of a .def file."""
    sections = []
    for line in path.read_text().splitlines():
        s = line.strip()
        if s.startswith("; ") and s[2:].startswith(("OpenGL", "GL_", "EGL")):
            sections.append((s[2:].strip(), []))
            continue
        if s and not s.startswith(";") and sections:
            name = s.split()[0]
            sections[-1][1].append(name)
    return sections


# Functions of the window harness (src/harness.rs), with their signatures.
WINDOW = [
    ("andruna_window_create", ("i32", "i32", "usize"), "usize"),
    ("andruna_window_destroy", ("usize",), None),
    ("andruna_window_is_open", ("usize",), "i32"),
    ("andruna_window_frame_stats", ("usize", "usize"), "i32"),
    ("andruna_window_resize", ("usize", "i32", "i32"), "i32"),
]


def generate(angle):
    protos = prototypes(angle)
    egl_def = def_sections(angle / "src" / "libEGL" / "libEGL_autogen.def")
    gles_def = def_sections(angle / "src" / "libGLESv2" / "libGLESv2_autogen.def")

    entries = []  # (lib, name, params, result)
    seen = set()
    libs = {"EGL": [], "GLESv1_CM": [], "GLESv2": [], "andruna_window": []}
    skipped = []

    def add(lib, name):
        if name not in protos:
            skipped.append(name)
            return False
        if name not in seen:
            seen.add(name)
            p, r, _ = protos[name]
            entries.append((lib, name, p, r))
        return True

    for title, names in egl_def:
        for n in names:
            if add("Egl", n):
                libs["EGL"].append(n)
    for title, names in gles_def:
        if title.startswith("EGL"):
            continue
        for n in names:
            if not n.startswith("gl"):
                continue
            if not add("Gles", n):
                continue
            headers = protos[n][2]
            if title == "OpenGL ES 1.0":
                libs["GLESv1_CM"].append(n)
            elif title.startswith("OpenGL ES"):
                libs["GLESv2"].append(n)
            else:
                if "GLES/glext.h" in headers or "GLES/gl.h" in headers:
                    libs["GLESv1_CM"].append(n)
                if any(h.startswith(("GLES2/", "GLES3/")) for h in headers):
                    libs["GLESv2"].append(n)
    # The harness functions are host functions (no ANGLE entry, no thunk).
    for n, _p, _r in WINDOW:
        libs["andruna_window"].append(n)
    libs["GLESv3"] = list(libs["GLESv2"])
    return entries, libs, sorted(set(skipped))


HEADER_RS = """\
// Generated by crates/andruna-gles/tools/gen_gles.py from ANGLE's export
// lists and the Khronos headers at the pinned AOSP tag. Do not edit.
//
// slot library name(parameters) -> result
"""


def write_rust(entries):
    lines = [HEADER_RS, "entries! {"]
    for i, (lib, name, params, ret) in enumerate(entries):
        r = f" -> {ret}" if ret else ""
        args = ", ".join(f"a{k}: {t}" for k, t in enumerate(params))
        lines.append(f"    {i} {lib} {name}({args}){r};")
    lines.append("}")
    (CRATE / "src" / "entries.rs").write_text("\n".join(lines) + "\n", newline="\n")


STUB_C = """\
// Generated by crates/andruna-gles/tools/gen_gles.py. Do not edit.
//
// {soname}: the guest side of {what}. Each exported function jumps
// through a slot; the constructor fills the slots by name with the host's
// `__andruna_gles_resolve` (andruna-gles), so the functions run the host
// implementation with no extra frame. Built freestanding (no libc).

extern void *__andruna_gles_resolve(const char *name);

static const char *const andruna_names[{n}] = {{
{names}
}};

__attribute__((visibility("hidden"))) void *andruna_slots[{n}];

__attribute__((constructor(101))) static void andruna_bind(void) {{
  for (unsigned i = 0; i < {n}; i++) andruna_slots[i] = __andruna_gles_resolve(andruna_names[i]);
}}

#if defined(__x86_64__)
#define STUB(i, name)                                                   \\
  __asm__(".pushsection .text\\n.globl " #name "\\n.type " #name ",@function\\n" \\
          ".p2align 4\\n" #name ":\\n"                                     \\
          "  jmp *andruna_slots+8*" #i "(%rip)\\n"                        \\
          ".size " #name ", .-" #name "\\n.popsection\\n");
#elif defined(__aarch64__)
#define STUB(i, name)                                                   \\
  __asm__(".pushsection .text\\n.globl " #name "\\n.type " #name ",%function\\n" \\
          ".p2align 2\\n" #name ":\\n"                                     \\
          "  adrp x16, andruna_slots+8*" #i "\\n"                          \\
          "  ldr x16, [x16, :lo12:andruna_slots+8*" #i "]\\n"              \\
          "  br x16\\n"                                                   \\
          ".size " #name ", .-" #name "\\n.popsection\\n");
#else
#error "x86_64 and aarch64 only"
#endif

{stubs}
"""

WHAT = {
    "EGL": "EGL (ANGLE on Direct3D 11)",
    "GLESv1_CM": "OpenGL ES 1.x (ANGLE on Direct3D 11)",
    "GLESv2": "OpenGL ES 2.0 to 3.2 (ANGLE on Direct3D 11)",
    "GLESv3": "OpenGL ES 3.x (the same functions as libGLESv2.so)",
    "andruna_window": "the test window harness (andruna-gles src/harness.rs)",
}


def write_stubs(libs):
    d = CRATE / "guest"
    d.mkdir(exist_ok=True)
    for lib, names in libs.items():
        names = list(dict.fromkeys(names))
        text = STUB_C.format(
            soname=f"lib{lib}.so", what=WHAT[lib], n=len(names),
            names="\n".join(f'    "{n}",' for n in names),
            stubs="\n".join(f"STUB({i}, {n})" for i, n in enumerate(names)))
        (d / f"lib{lib}.c").write_text(text, newline="\n")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--angle", default=ROOT / "third_party" / "aosp" / "external" / "angle", type=Path)
    a = ap.parse_args()
    entries, libs, skipped = generate(a.angle)
    write_rust(entries)
    write_stubs(libs)
    print(f"{len(entries)} entry points; " + ", ".join(f"lib{k}.so {len(set(v))}" for k, v in libs.items()))
    if skipped:
        print(f"{len(skipped)} exported functions without a Khronos prototype (not exposed): {' '.join(skipped)}")


if __name__ == "__main__":
    main()
