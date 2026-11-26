#!/usr/bin/env python3
"""Runs libbase's gtest suite (an Android ELF executable) under the S8 loader.

A test that crashes the process is recorded and excluded, and the suite is
run again, until it completes. Death tests are excluded from the start:
they need fork/clone, which the spike does not provide.

Usage: python spikes/s8/run_tests.py [--exe target/.../andruna-spike-s8.exe]
Prints a summary and writes out/s8/<arch>/libbase_test-results.json.
"""

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def run(exe, env, args):
    p = subprocess.run([str(exe), "run", "libbase_test", *args], env=env,
                       capture_output=True, text=True, errors="replace", timeout=900)
    return p.returncode, p.stdout + p.stderr


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", default=str(HERE / "target/x86_64-pc-windows-gnullvm/release/andruna-spike-s8.exe"))
    ap.add_argument("--arch", default="x64")
    a = ap.parse_args()
    env = dict(os.environ)
    mingw = os.environ.get("ANDRUNA_LLVM_MINGW", "C:/tools/llvm-mingw")
    triple = "aarch64-w64-mingw32" if a.arch == "arm64" else "x86_64-w64-mingw32"
    env["PATH"] = f"{mingw}/{triple}/bin;{mingw}/bin;" + env["PATH"]
    env["S8_QUIET"] = "1"

    rc, out = run(a.exe, env, ["--gtest_list_tests"])
    tests, suite = [], None
    for line in out.splitlines():
        if not line.strip() or line.startswith("["):
            continue
        if not line.startswith(" "):
            suite = line.split()[0]
        else:
            tests.append(suite + line.split()[0])
    excluded = sorted(t for t in tests if "Death" in t or "DeathTest" in t.split(".")[0])
    crashed = []
    rounds = 0
    while True:
        rounds += 1
        filt = "-" + ":".join(excluded + crashed) if excluded or crashed else "*"
        rc, out = run(a.exe, env, [f"--gtest_filter={filt}"])
        started = re.findall(r"^\[ RUN      \] (\S+)", out, re.M)
        finished = set(re.findall(r"^\[  (?:     OK|FAILED |SKIPPED) \] (\S+) \(", out, re.M))
        unfinished = [t for t in started if t not in finished]
        if rc in (0, 1) and not unfinished:
            break
        if not unfinished:
            print(out[-3000:])
            sys.exit(f"suite stopped (rc={rc}) outside a test")
        crashed.append(unfinished[-1])
        print(f"round {rounds}: {unfinished[-1]} ended the process (rc={rc:#x})", flush=True)
        if rounds > 60:
            sys.exit("too many crashes")
    passed = re.findall(r"^\[       OK \] (\S+) \(", out, re.M)
    failed = re.findall(r"^\[  FAILED  \] (\S+) \(", out, re.M)
    skipped = re.findall(r"^\[  SKIPPED \] (\S+) \(", out, re.M)
    res = {
        "listed": len(tests),
        "passed": len(passed),
        "failed": failed,
        "skipped": skipped,
        "crashed": crashed,
        "excluded_death_tests": len(excluded),
        "rounds": rounds,
    }
    dest = ROOT / "out" / "s8" / a.arch / "libbase_test-results.json"
    dest.write_text(json.dumps(res, indent=2))
    (dest.with_suffix(".log")).write_text(out)
    print(json.dumps({k: (v if not isinstance(v, list) else len(v)) for k, v in res.items()}))
    print("failed:", " ".join(failed))
    print("crashed:", " ".join(crashed))


if __name__ == "__main__":
    main()
