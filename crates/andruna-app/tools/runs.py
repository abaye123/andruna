#!/usr/bin/env python3
"""Runs the S1 programs on andruna-app and prints the numbers.

Hello World, the benchmark and the fault test in the interpreter, with the
JIT (default thresholds and -Xjitthreshold:0) and as AOT code (dex2oat
writes the boot image when the ART root has none, then the odex files of
the three programs), each run timed from the outside (wall time of the
process). Prints one line per run and a Markdown summary at the end.

Usage:
  python crates/andruna-app/tools/runs.py [--app EXE] [--art-root DIR]
      [--dex DIR] [--data DIR] [--runs N] [--only hello|startup]

Defaults: target/release/andruna-app.exe, out/guest/<arch>/art-root,
out/s1/java, %TEMP%/andruna-app-runs, 3 runs. --only startup runs Hello
World only (start-up times), N times per mode.

Exit status: 1 if a run failed (non-zero exit or missing output).
"""

import argparse
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
ARM = platform.machine().lower() in ("arm64", "aarch64")
ARCH = "arm64" if ARM else "x64"
ISA = "arm64" if ARM else "x86_64"

failures = []
rows = []


def run(app, art, data, args, expect, name, timeout=600):
    cmd = [str(app), "--art-root", str(art), "--data", str(data)] + args
    t0 = time.perf_counter()
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, errors="replace", timeout=timeout)
        code, out, err = p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired as e:
        code, out, err = "timeout", e.stdout or "", e.stderr or ""
        out = out if isinstance(out, str) else out.decode(errors="replace")
        err = err if isinstance(err, str) else err.decode(errors="replace")
    wall = time.perf_counter() - t0
    ok = code == 0 and all(x in out for x in expect)
    print(f"### {name}: exit {code}, {wall * 1000:.0f} ms wall", flush=True)
    print(out.rstrip())
    tail = [l for l in err.splitlines() if not l.startswith(("W/", "I/"))]
    if not ok or os.environ.get("RUNS_VERBOSE"):
        print("--- stderr (last 60 lines)")
        print("\n".join(tail[-60:]))
    if not ok:
        failures.append(name)
    return ok, wall, out


def bench_total(out):
    m = re.search(r"^total: (\d+) ms", out, re.M)
    return int(m.group(1)) if m else None


def span(v, fmt="{:.0f}"):
    v = [x for x in v if x is not None]
    if not v:
        return "-"
    lo, hi = min(v), max(v)
    return fmt.format(lo) if lo == hi else f"{fmt.format(lo)} to {fmt.format(hi)}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--app", default=ROOT / "target" / "release" / "andruna-app.exe")
    ap.add_argument("--art-root", default=ROOT / "out" / "guest" / ARCH / "art-root")
    ap.add_argument("--dex", default=ROOT / "out" / "s1" / "java")
    ap.add_argument("--data", default=Path(tempfile.gettempdir()) / "andruna-app-runs")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--only", choices=["startup"])
    a = ap.parse_args()
    app, art, dex, data = Path(a.app), Path(a.art_root), Path(a.dex), Path(a.data)
    data.mkdir(parents=True, exist_ok=True)
    print(f"machine: {platform.machine()}, {os.cpu_count()} logical processors")
    # The programs in their own directory; dex2oat writes oat/<isa>/ there.
    appdir = data / "app"
    appdir.mkdir(exist_ok=True)
    prog = {}
    for n in ("hello", "bench", "faults", "clock"):
        if (dex / f"{n}.dex").is_file():
            shutil.copy2(dex / f"{n}.dex", appdir / f"{n}.dex")
            prog[n] = str(appdir / f"{n}.dex")

    modes = [
        ("int", ["--mode", "int", "--no-boot-image"]),
        ("jit", ["--mode", "jit", "--no-boot-image"]),
        ("jit0", ["--mode", "jit", "--no-boot-image", "-Xjitthreshold:0"]),
        ("aot", ["--mode", "aot"]),
    ]
    boot = art / "javalib" / ISA / "boot.art"
    if not boot.is_file():
        ok, w, _ = run(app, art, data, ["--compile-boot"], [], "dex2oat boot image", 1800)
        rows.append(("dex2oat, boot image", f"{w:.2f} s wall" if ok else "failed"))
    for n in ("hello", "bench", "faults"):
        if a.only and n != "hello":
            continue
        ok, w, _ = run(app, art, data, ["--compile", prog[n]], [], f"dex2oat {n}")
        rows.append((f"dex2oat, {n}.dex", f"{w:.2f} s wall" if ok else "failed"))

    hello = {}
    bench = {}
    bench_wall = {}
    for m, opts in modes:
        hello[m] = []
        for i in range(a.runs):
            ok, w, _ = run(app, art, data, opts + ["--classpath", prog["hello"], "Hello"],
                           ["Hello, Andruna"], f"hello {m} {i + 1}")
            if ok:
                hello[m].append(w * 1000)
        if a.only:
            continue
        bench[m], bench_wall[m] = [], []
        for i in range(a.runs):
            ok, w, out = run(app, art, data, opts + ["--classpath", prog["bench"], "Bench"],
                             ["total:"], f"bench {m} {i + 1}")
            if ok:
                bench[m].append(bench_total(out))
                bench_wall[m].append(w * 1000)
    if not a.only:
        for m in ("jit0", "aot"):
            opts = dict(modes)[m]
            ok, _, out = run(app, art, data, opts + ["--classpath", prog["faults"], "Faults"],
                             ["2000 NullPointerException caught", "StackOverflowError caught"],
                             f"faults {m}")
            d = re.search(r"caught at depth (\d+)", out)
            rows.append((f"Faults ({m})", ("passed" if ok else "FAILED")
                         + (f", StackOverflowError at depth {d.group(1)}" if d else "")))
        if "clock" in prog:
            ok, _, out = run(app, art, data, ["--mode", "int", "--no-boot-image", "--classpath",
                                              prog["clock"], "Clock"], ["nanoTime advances: true"],
                             "clock")
            m = re.search(r"currentTimeMillis: (\d+)", out)
            if m and abs(int(m.group(1)) / 1000 - time.time()) > 60:
                failures.append("clock: currentTimeMillis is not the system time")
                ok = False
            rows.append(("Clock (wall clock is the system time)", "passed" if ok else "FAILED"))

    print("\n## Summary\n")
    print(f"{platform.machine()}, {os.cpu_count()} logical processors, {a.runs} runs each\n")
    print("| Mode | Hello World, wall ms | Hello median | Bench total, ms | Bench, wall ms |")
    print("| --- | ---: | ---: | ---: | ---: |")
    for m, _ in modes:
        med = f"{statistics.median(hello[m]):.0f}" if hello[m] else "-"
        print(f"| {m} | {span(hello[m])} | {med} | {span(bench.get(m, []))} | "
              f"{span(bench_wall.get(m, []))} |")
    print()
    for k, v in rows:
        print(f"- {k}: {v}")
    if failures:
        print(f"\nFAILED: {', '.join(failures)}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
