#!/usr/bin/env python3
"""Runs a gtest suite (an Android ELF executable) with andruna-elf.

A test that ends the process is recorded and excluded, and the suite is run
again, until it completes. Death tests are excluded from the start: they
fork, and Windows processes cannot fork.

Usage:
  python crates/andruna-linker/tools/run_gtest.py <test exe> [-L dir ...]
      [--elf target/release/andruna-elf.exe] [--out results.json]

Prints a summary; writes the result JSON and the last run's log next to
--out (default: <test exe>-results.json).
"""

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


def run(elf, libs, exe, args, timeout):
    cmd = [str(elf), "run"]
    for d in libs:
        cmd += ["-L", str(d)]
    cmd += [str(exe), *args]
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, errors="replace", timeout=timeout)
        return p.returncode, p.stdout + p.stderr
    except subprocess.TimeoutExpired as e:
        out = (e.stdout or b"").decode(errors="replace") + (e.stderr or b"").decode(errors="replace")
        return "timeout", out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("exe")
    ap.add_argument("-L", dest="libs", action="append", default=[])
    ap.add_argument("--elf", default=str(ROOT / "target/release/andruna-elf.exe"))
    ap.add_argument("--out")
    ap.add_argument("--timeout", type=int, default=600)
    a = ap.parse_args()

    rc, out = run(a.elf, a.libs, a.exe, ["--gtest_list_tests"], a.timeout)
    tests, suite = [], None
    for line in out.splitlines():
        if not line.strip() or line.startswith("[") or line.startswith("libc:"):
            continue
        if not line.startswith(" "):
            suite = line.split()[0]
        else:
            tests.append(suite + line.split()[0])
    if not tests:
        print(out[-3000:])
        sys.exit("no tests listed")
    excluded = sorted(t for t in tests if "DeathTest" in t.split(".")[0] or "Death" in t.split(".")[-1])
    crashed = []
    rounds = 0
    while True:
        rounds += 1
        filt = "-" + ":".join(excluded + crashed) if excluded or crashed else "*"
        rc, out = run(a.elf, a.libs, a.exe, [f"--gtest_filter={filt}"], a.timeout)
        started = re.findall(r"^\[ RUN      \] (\S+)", out, re.M)
        finished = set(re.findall(r"^\[  (?:     OK|FAILED |SKIPPED) \] (\S+) \(", out, re.M))
        unfinished = [t for t in started if t not in finished]
        if rc in (0, 1) and not unfinished:
            break
        if not unfinished:
            print(out[-3000:])
            sys.exit(f"suite stopped (rc={rc}) outside a test")
        crashed.append(unfinished[-1])
        print(f"round {rounds}: {unfinished[-1]} ended the process (rc={rc})", flush=True)
        if rounds > 80:
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
        "excluded_death_tests": excluded,
        "rounds": rounds,
    }
    dest = Path(a.out) if a.out else Path(str(a.exe) + "-results.json")
    dest.write_text(json.dumps(res, indent=2))
    dest.with_suffix(".log").write_text(out)
    print(json.dumps({k: (v if not isinstance(v, list) else len(v)) for k, v in res.items()}))
    print("failed:", " ".join(failed))
    print("crashed:", " ".join(crashed))


if __name__ == "__main__":
    main()
