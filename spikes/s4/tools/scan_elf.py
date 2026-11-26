"""Survey of thread-register use in ELF files (spike S4).

Runs llvm-objdump on each input (.so, executables, or .a archives) and
counts, per file:

  arm64:  instructions that write x18/w18 (destination register, load pair
          destinations, atomics, base-register writeback), split into the
          shadow call stack patterns and everything else; instructions that
          only read x18; TPIDR_EL0 reads and writes.
  x86_64: instructions with an %fs segment override, split into absolute
          forms (%fs:disp32, patchable in place) and register forms
          (%fs:(%reg) or %fs:disp(%reg)), with the length histogram of the
          register forms (a 5-byte jump needs at least 5 bytes).

usage: python -I scan_elf.py LLVM_OBJDUMP FILE...
"""
import collections
import re
import subprocess
import sys

OBJDUMP = sys.argv[1]
FILES = sys.argv[2:]

LINE = re.compile(r"^\s*[0-9a-f]+:\s+((?:[0-9a-f]{2} )+)\s*\t?(\S+)\s*(.*)$")
LINE_ARM = re.compile(r"^\s*[0-9a-f]+:\s+([0-9a-f]{8})\s+(\S+)\s*(.*)$")

NO_DEST_PREFIX = ("st", "cmp", "cmn", "tst", "b", "cb", "tb", "ret", "prfm", "msr",
                  "hint", "dmb", "dsb", "isb", "nop", "brk", "svc", "hlt", "udf", "dc", "ic",
                  "at", "tlbi", "bti", "pac", "aut", "xpac", "ccmp", "ccmn", "fcmp", "fccmp", "sys", "clrex",
                  "yield", "wfe", "wfi", "sev", "esb", "csdb", "paciasp", "autiasp")
STATUS_FIRST = ("stxr", "stlxr", "stxp", "stlxp", "stxrb", "stlxrb", "stxrh", "stlxrh")
ATOMIC_SECOND = ("ldadd", "ldclr", "ldeor", "ldset", "ldsmax", "ldsmin", "ldumax", "ldumin", "swp")
PAIR_LOADS = ("ldp", "ldnp", "ldpsw", "ldxp", "ldaxp", "ldiapp")
R18 = re.compile(r"\b[xw]18\b")


def operands(text):
    text = text.split("//")[0].split("<")[0]
    out, depth, cur = [], 0, ""
    for ch in text:
        if ch in "[{":
            depth += 1
        elif ch in "]}":
            depth -= 1
        if ch == "," and depth == 0:
            out.append(cur.strip())
            cur = ""
        else:
            cur += ch
    if cur.strip():
        out.append(cur.strip())
    return out


def arm_writes_x18(mn, ops):
    """Returns 'scs', 'dest', 'writeback' or None."""
    wb = any(o.startswith("[x18") and (o.endswith("!") or (o == "[x18]" and i + 1 < len(ops))) for i, o in enumerate(ops))
    if wb:
        if mn == "str" and ops[0] == "x30":
            return "scs"
        if mn == "ldr" and ops[0] == "x30":
            return "scs"
        return "writeback"
    if not ops:
        return None
    is_r18 = lambda o: re.fullmatch(r"[xw]18", o) is not None
    if mn.startswith(STATUS_FIRST):
        return "dest" if is_r18(ops[0]) else None
    if mn.startswith(ATOMIC_SECOND):
        return "dest" if len(ops) > 1 and is_r18(ops[1]) else None
    if mn.startswith("cas"):
        return "dest" if is_r18(ops[0]) or (mn.startswith("casp") and len(ops) > 1 and is_r18(ops[1])) else None
    if mn.startswith(PAIR_LOADS):
        return "dest" if is_r18(ops[0]) or (len(ops) > 1 and is_r18(ops[1])) else None
    if mn.startswith(NO_DEST_PREFIX) and not mn.startswith(("bfi", "bfxil", "bfm", "bic", "bsl", "bit", "bif")):
        return None
    return "dest" if is_r18(ops[0]) else None


def scan(path):
    res = subprocess.run([OBJDUMP, "-d", "--no-print-imm-hex", path], capture_output=True, text=True, errors="replace")
    arch = None
    head = res.stdout[:4000]
    if "aarch64" in head:
        arch = "arm64"
    elif "x86-64" in head:
        arch = "x86_64"
    c = collections.Counter()
    samples = collections.defaultdict(list)
    lens = collections.Counter()
    for line in res.stdout.splitlines():
        if arch == "arm64":
            m = LINE_ARM.match(line)
            if not m:
                continue
            c["insns"] += 1
            mn, rest = m.group(2), m.group(3)
            low = rest.lower()
            if "tpidr_el0" in low:
                c["mrs_tpidr" if mn == "mrs" else "msr_tpidr"] += 1
            if not R18.search(rest):
                continue
            kind = arm_writes_x18(mn, operands(rest))
            if kind:
                c["x18_write_" + kind] += 1
                if len(samples[kind]) < 4:
                    samples[kind].append(f"{mn} {rest.split('//')[0].strip()}")
            else:
                c["x18_read_only"] += 1
                if len(samples["read"]) < 4:
                    samples["read"].append(f"{mn} {rest.split('//')[0].strip()}")
        elif arch == "x86_64":
            m = LINE.match(line)
            if not m:
                continue
            c["insns"] += 1
            raw, mn, rest = m.group(1), m.group(2), m.group(3)
            if "%fs:" not in rest:
                continue
            n = len(raw.split())
            seg = rest.split("%fs:")[1]
            if re.match(r"-?0x[0-9a-f]+\(|-?\d+\(|\(", seg):
                c["fs_reg"] += 1
                lens[n] += 1
                if len(samples["fs_reg"]) < 4:
                    samples["fs_reg"].append(f"{mn} {rest.strip()} [{n} bytes]")
            else:
                d = re.match(r"(-?(?:0x[0-9a-f]+|\d+))", seg)
                v = int(d.group(1), 0) if d else None
                c["fs_abs_tp" if v == 0 else ("fs_abs_slot" if v is not None and 0 < v < 0x50 else "fs_abs_other")] += 1
                if v == 0x28:
                    c["fs_abs_guard"] += 1
                if v not in (0, 0x28) and len(samples["fs_abs"]) < 4:
                    samples["fs_abs"].append(f"{mn} {rest.strip()}")
    return arch, c, samples, lens


for f in FILES:
    arch, c, samples, lens = scan(f)
    name = f.replace("\\", "/").split("/")
    print(f"== {'/'.join(name[-2:])} [{arch}] instructions: {c['insns']}")
    keys = sorted(k for k in c if k != "insns")
    print("   " + ", ".join(f"{k}={c[k]}" for k in keys))
    if lens:
        print("   fs register-form lengths: " + ", ".join(f"{k}B:{v}" for k, v in sorted(lens.items())))
    for k, v in samples.items():
        for s in v:
            print(f"   sample {k}: {s}")
