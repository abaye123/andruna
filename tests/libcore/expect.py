#!/usr/bin/env python3
"""Suggests expectations.txt lines from one or more run.py results.

  python tests/libcore/expect.py out/libcore-tests/*/results.json

For every test with an unexpected failure in any of the given runs, prints
one line with the runners and modes it failed in (none when it failed in
all of them) and a category guessed from the failure reason (run.py's
categorize, refined by OVERRIDES below). Review the lines before adding them
to expectations.txt: the category is a guess, and a test that fails in only
one run may be flaky.
"""

import fnmatch
import importlib.util
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

# (test glob, text the failure reason must contain, category, reason):
# causes found by reading the logs, for failures whose message does not show
# the cause.
OVERRIDES = [
    ("libcore.java.nio.file.*", "UnixPath.normalizeAndCheck", "bootclasspath",
     "FilesSetup cannot create its temporary directory (no SecureRandom provider); "
     "the NullPointerException comes from its tearDown"),
    ("libcore.java.io.ConsoleTest#*", "", "lx-syscall",
     "tcgetattr on a non-terminal must fail with ENOTTY"),
    ("libcore.java.lang.OldSystemTest#test_load", "", "environment",
     "ClassLoader.findLibrary(\"log\") finds no liblog.so on java.library.path"),
    ("libcore.java.util.TimeZoneTest#*", "", "tzdata",
     "no time zone data (/apex/com.android.tzdata is not provided): every zone is GMT"),
    ("libcore.java.util.PropertiesTest#*", "xalan", "bootclasspath",
     "no XML transformer (Xalan is not on the boot class path)"),
]


def main():
    spec = importlib.util.spec_from_file_location("run", HERE / "run.py")
    run = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(run)
    runs = []
    fails = {}
    for p in sys.argv[1:]:
        r = json.loads(Path(p).read_text(encoding="utf-8"))
        key = (r["summary"]["runner"], r["summary"]["mode"])
        runs.append(key)
        for t in r["tests"]:
            if t.get("outcome") == "unexpected-failure":
                fails.setdefault(t["id"], {})[key] = t
    all_runners = sorted({k[0] for k in runs})
    all_modes = sorted({k[1] for k in runs})
    for tid in sorted(fails):
        where = fails[tid]
        first = next(iter(where.values()))
        cat = run.categorize(first["reason"])
        reason = f"{first['status']}: {first['reason'][:180]}"
        for pat, needle, ocat, oreason in OVERRIDES:
            if (fnmatch.fnmatchcase(tid, pat) and needle in first["reason"]
                    and first["status"] == "fail"):
                cat = ocat
                reason = oreason
                break
        for only in only_sets(set(where), runs, all_runners, all_modes):
            print(f"{tid} | {cat} | {reason}" + (f" | {only}" if only else ""))


def only_sets(failed, runs, all_runners, all_modes):
    """The <only> fields that cover exactly the failed (runner, mode) runs."""
    if failed == set(runs):
        return [""]
    out = []
    left = set(failed)
    for r in all_runners:
        if all((r, m) in left for m in all_modes if (r, m) in runs):
            out.append(r)
            left -= {(r, m) for m in all_modes}
    for m in all_modes:
        if left and all((r, m) in left for r in all_runners if (r, m) in runs):
            out.append(m)
            left -= {(r, m) for r in all_runners}
    out += [f"{r},{m}" for r, m in sorted(left)]
    return out


if __name__ == "__main__":
    main()
