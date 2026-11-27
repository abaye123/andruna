#!/usr/bin/env python3
"""Runs the libcore test jar (tests/libcore/build.py) on ART and reports.

Every test class runs in its own ART process with the runner
andruna.libcoretest.Main. A test that crashes the VM, or produces no result
line for --test-timeout seconds, is recorded as crash or timeout and the
class is restarted without it. Results are matched against
tests/libcore/expectations.txt (known failures, each with a category and a
reason).

Output (out/libcore-tests/<runner>-<mode>/):
  results.json   every test: id, status, ms, reason, expectation
  summary.txt    totals, pass rate, unexpected results
  logs/<class>.log   the VM output of each class

Exit status: 0 when every failure is expected, 1 otherwise (2 on a harness
error). Unexpected passes are reported but do not fail the run.

The ART launcher is pluggable (RUNNERS below): "s1" is the spike S1 runner
(spikes/s1/art/run_art.py), "app" is andruna-app. DEFAULT_RUNNER selects one;
--runner or LIBCORE_RUNNER override it.
"""

import argparse
import concurrent.futures
import fnmatch
import importlib.util
import json
import os
import queue
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
OUT = Path(os.environ.get("LIBCORE_TESTS_OUT", ROOT / "out" / "libcore-tests"))
JAR_NAME = "libcore-tests.jar"
MAIN = "andruna.libcoretest.Main"
TAG = "##lt "

DEFAULT_RUNNER = "s1"

# Options every runner passes to the VM. The temporary directory is a
# writable directory inside the guest tree (tests create files there).
GUEST_TMP = "/data/local/tmp/libcore-tmp"
VM_PROPS = [f"-Djava.io.tmpdir={GUEST_TMP}", "-Duser.home=/data/local/tmp"]


# --- runners --------------------------------------------------------------------


class S1Runner:
    """dalvikvm through the spike S1 runner (andruna-spike-s1-art.exe)."""

    def __init__(self, mode, root, jar):
        spec = importlib.util.spec_from_file_location(
            "run_art", ROOT / "spikes" / "s1" / "art" / "run_art.py")
        ra = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(ra)
        self.ra = ra
        self.mode = mode
        self.root = root
        ra.setup_root(root)
        tmp = root / "data" / "local" / "tmp"
        shutil.copy2(jar, tmp / JAR_NAME)
        self.cp = "/data/local/tmp/" + JAR_NAME
        self.bcp = ":".join(f"{ra.JAVALIB}/{j}" for j in ra.BOOT_JARS)

    def guest_root(self):
        return self.root

    def command(self, args):
        ra = self.ra
        opts = [f"-Xbootclasspath:{self.bcp}", f"-Xbootclasspath-locations:{self.bcp}",
                "-Ximage:/system/framework/none.art", "-Xnoimage-dex2oat", "-Xmx512m"]
        opts += {"int": ["-Xint"], "jit": ["-Xusejit:true"],
                 "jit0": ["-Xusejit:true", "-Xjitthreshold:0"]}[self.mode]
        opts += VM_PROPS
        cmd = [str(ra.RUNNER), "run", str(ra.GUEST / "aosp" / "dalvikvm")] + opts + \
              ["-cp", self.cp, MAIN] + args
        env = dict(os.environ)
        env["S8_ROOT"] = str(self.root)
        env.setdefault("S8_LIB_PATH", ";".join(str(ra.GUEST / d) for d in (
            "aosp", "natives-build", "natives-build/manual", "sysroot/usr/lib",
            "libcxx-install/lib")))
        return cmd, env


class AppRunner:
    """andruna-app (crates/andruna-app): ART on andruna-linker. The executable
    is ANDRUNA_APP (default target/release/andruna-app.exe); the ART root comes
    from ANDRUNA_ART_ROOT as andruna-app documents."""

    def __init__(self, mode, root, jar):
        self.exe = Path(os.environ.get("ANDRUNA_APP", ROOT / "target" / "release" / "andruna-app.exe"))
        self.mode = mode
        self.root = root
        self.jar = jar
        root.mkdir(parents=True, exist_ok=True)
        if mode not in ("int", "jit"):
            raise SystemExit(f"andruna-app has no mode {mode}")

    def guest_root(self):
        return self.root

    def command(self, args):
        cmd = [str(self.exe), "--mode", self.mode, "--data", str(self.root),
               "--classpath", str(self.jar)] + VM_PROPS + [MAIN] + args
        return cmd, dict(os.environ)


RUNNERS = {"s1": S1Runner, "app": AppRunner}


# --- expectations ---------------------------------------------------------------


class Expectation:
    def __init__(self, pattern, category, reason, modes, line):
        self.pattern = pattern
        self.category = category
        self.reason = reason
        self.modes = modes
        self.line = line
        self.used = False

    def matches(self, test_id, mode):
        if self.modes and mode not in self.modes:
            return False
        cls = test_id.split("#", 1)[0]
        return fnmatch.fnmatchcase(test_id, self.pattern) or self.pattern == cls


CATEGORIES = {
    "lx-syscall": "a Linux system call or file the Linux layer does not provide yet",
    "missing-native": "a JNI method or native library the guest build lacks",
    "icu-data": "ICU data or locale data missing or different",
    "environment": "the test assumes a device environment (users, paths, properties, network)",
    "art": "an ART runtime problem (crash, hang, wrong result) to investigate",
    "slow": "too slow for the per-test timeout in this mode",
    "harness": "a limitation of this test harness",
    "triage": "not yet analyzed",
}


def load_expectations(path):
    exps = []
    if not path.exists():
        return exps
    for n, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = [p.strip() for p in line.split("|")]
        if len(parts) < 3:
            raise SystemExit(f"{path}:{n}: expected '<test> | <category> | <reason> [| modes]'")
        if parts[1] not in CATEGORIES:
            raise SystemExit(f"{path}:{n}: unknown category {parts[1]}")
        modes = set(m.strip() for m in parts[3].split(",")) if len(parts) > 3 and parts[3] else set()
        exps.append(Expectation(parts[0], parts[1], parts[2], modes, n))
    return exps


def categorize(reason):
    """A first guess at the category of a failure, for suggested expectations."""
    r = reason or ""
    if "UnsatisfiedLinkError" in r or "No implementation found" in r:
        return "missing-native"
    if re.search(r"ENOSYS|Function not implemented|EOPNOTSUPP|Operation not supported", r):
        return "lx-syscall"
    if re.search(r"ErrnoException|ENOENT|EACCES|EPERM|EINVAL", r):
        return "lx-syscall"
    if re.search(r"icu|ICU|MissingResourceException|Locale", r):
        return "icu-data"
    return "triage"


# --- running one class ----------------------------------------------------------


def run_class(runner, cls, logdir, test_timeout, max_restarts):
    """Runs one class, restarting after crashes. Returns a list of result dicts."""
    results = {}
    order = []
    excluded = []
    exc_host = runner.guest_root() / "data" / "local" / "tmp" / "libcore-exclude.txt"
    exc_guest = "/data/local/tmp/libcore-exclude.txt"
    (runner.guest_root() / GUEST_TMP.lstrip("/")).mkdir(parents=True, exist_ok=True)
    log_path = logdir / f"{cls}.log"
    with open(log_path, "w", encoding="utf-8", errors="replace") as logf:
        for attempt in range(max_restarts + 1):
            exc_host.write_text("\n".join(excluded) + "\n", encoding="utf-8")
            cmd, env = runner.command(["--exclude-file", exc_guest, cls])
            logf.write(f"### attempt {attempt}: {' '.join(cmd)}\n")
            logf.flush()
            pending, done, exit_code, timed_out, tail = run_process(cmd, env, logf, test_timeout,
                                                                    results, order)
            if done:
                break
            # The process ended (or hung) before reporting DONE.
            what = "timeout" if timed_out else "crash"
            detail = (f"no result for {test_timeout:.0f} s" if timed_out
                      else f"VM exited with {exit_code} ({exit_code & 0xffffffff:#x})")
            if tail:
                detail += ": " + " / ".join(tail)[-400:]
            if pending is None:
                cid = f"{cls}#<class>"
                if cid not in results:
                    order.append(cid)
                results[cid] = {"id": cid, "status": what, "ms": 0, "reason": detail}
                break
            if pending not in results:
                order.append(pending)
            results[pending] = {"id": pending, "status": what, "ms": 0, "reason": detail}
            excluded = [t for t in order]
        else:
            logf.write("### too many restarts\n")
    return [results[t] for t in order]


def run_process(cmd, env, logf, test_timeout, results, order):
    """Runs the VM once, recording result lines. Returns (pending test id or
    None, DONE seen, exit code, timed out, last non-protocol output lines)."""
    proc = subprocess.Popen(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            stdin=subprocess.DEVNULL)
    lines = queue.Queue()

    def reader():
        for raw in proc.stdout:
            lines.put(raw.decode("utf-8", errors="replace").rstrip("\r\n"))
        lines.put(None)

    threading.Thread(target=reader, daemon=True).start()
    pending = None
    done = False
    tail = []
    timed_out = False
    last = time.monotonic()
    while True:
        try:
            line = lines.get(timeout=1.0)
        except queue.Empty:
            if time.monotonic() - last > test_timeout:
                timed_out = True
                kill(proc)
                break
            continue
        if line is None:
            break
        logf.write(line + "\n")
        if not line.startswith(TAG):
            if line.strip():
                tail = (tail + [line.strip()])[-3:]
            continue
        last = time.monotonic()
        parts = line[len(TAG):].split(" ", 3)
        kind = parts[0]
        if kind == "START":
            pending = parts[1]
            tail = []
        elif kind in ("PASS", "FAIL", "SKIP"):
            tid = parts[1]
            ms = int(parts[2]) if len(parts) > 2 and parts[2].isdigit() else 0
            reason = parts[3] if len(parts) > 3 else ""
            if tid not in results:
                order.append(tid)
            results[tid] = {"id": tid, "status": kind.lower(), "ms": ms, "reason": reason}
            if tid == pending:
                pending = None
        elif kind == "CLASS":
            sub = line[len(TAG):].split(" ", 3)
            if len(sub) > 2 and sub[2] in ("LOADERROR", "RUNNERERROR"):
                tid = sub[1] + "#<class>"
                if tid not in results:
                    order.append(tid)
                results[tid] = {"id": tid, "status": "fail", "ms": 0,
                                "reason": sub[2] + ": " + (sub[3] if len(sub) > 3 else "")}
        elif kind == "DONE":
            done = True
    try:
        code = proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        kill(proc)
        code = proc.wait()
    logf.write(f"### exit {code}\n")
    if timed_out:
        logf.write("### killed: test timeout\n")
    return pending, done, code, timed_out, tail


def kill(proc):
    try:
        proc.kill()
    except OSError:
        pass


# --- main -----------------------------------------------------------------------


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--mode", default="int", choices=["int", "jit", "jit0"])
    ap.add_argument("--runner", default=os.environ.get("LIBCORE_RUNNER", DEFAULT_RUNNER),
                    choices=sorted(RUNNERS))
    ap.add_argument("--filter", action="append", default=[],
                    help="glob on class names (repeatable); default all of classes.txt")
    ap.add_argument("--jobs", type=int, default=max(1, min(4, (os.cpu_count() or 2) // 2)))
    ap.add_argument("--test-timeout", type=float, default=None,
                    help="seconds without a result line before a test counts as hung "
                    "(default 120, 60 with the JIT)")
    ap.add_argument("--max-restarts", type=int, default=40)
    ap.add_argument("--expectations", type=Path, default=HERE / "expectations.txt")
    ap.add_argument("--out", type=Path, default=None)
    a = ap.parse_args()
    timeout = a.test_timeout or (120.0 if a.mode == "int" else 60.0)

    jar = OUT / JAR_NAME
    classes_txt = OUT / "classes.txt"
    if not jar.exists() or not classes_txt.exists():
        print(f"{jar} not found: run tests/libcore/build.py", file=sys.stderr)
        return 2
    classes = [c for c in classes_txt.read_text(encoding="utf-8").split() if c]
    if a.filter:
        classes = [c for c in classes if any(fnmatch.fnmatchcase(c, f) for f in a.filter)]
    exps = load_expectations(a.expectations)
    out = a.out or OUT / f"{a.runner}-{a.mode}"
    if out.exists():
        shutil.rmtree(out)
    (out / "logs").mkdir(parents=True)
    base = Path(os.environ.get("LIBCORE_TESTS_ROOT",
                               Path(tempfile.gettempdir()) / "andruna-libcore-tests"))

    # One guest tree per worker, so tests writing files do not collide.
    workers = queue.Queue()
    for i in range(a.jobs):
        root = base / f"{a.runner}-{a.mode}-{i}"
        if root.exists():
            shutil.rmtree(root, ignore_errors=True)
        workers.put(RUNNERS[a.runner](a.mode, root, jar))

    print(f"libcore tests: {len(classes)} classes, runner {a.runner}, mode {a.mode}, "
          f"{a.jobs} jobs, test timeout {timeout:.0f} s", flush=True)
    t0 = time.perf_counter()
    all_results = {}

    def job(cls):
        r = workers.get()
        try:
            return cls, run_class(r, cls, out / "logs", timeout, a.max_restarts)
        finally:
            workers.put(r)

    with concurrent.futures.ThreadPoolExecutor(a.jobs) as ex:
        for n, fut in enumerate(concurrent.futures.as_completed([ex.submit(job, c) for c in classes]), 1):
            cls, res = fut.result()
            all_results[cls] = res
            counts = {}
            for t in res:
                counts[t["status"]] = counts.get(t["status"], 0) + 1
            print(f"[{n}/{len(classes)}] {cls}: " +
                  ", ".join(f"{k} {v}" for k, v in sorted(counts.items())), flush=True)
    wall = time.perf_counter() - t0

    tests = []
    for cls in classes:
        for t in all_results.get(cls, []):
            exp = next((e for e in exps if e.matches(t["id"], a.mode)), None)
            if exp:
                exp.used = True
                t["expected"] = {"category": exp.category, "reason": exp.reason, "line": exp.line}
            failed = t["status"] in ("fail", "crash", "timeout")
            if failed and exp:
                t["outcome"] = "expected-failure"
            elif failed:
                t["outcome"] = "unexpected-failure"
            elif t["status"] == "pass" and exp:
                t["outcome"] = "unexpected-pass"
            else:
                t["outcome"] = t["status"]
            tests.append(t)

    status = {}
    for t in tests:
        status[t["status"]] = status.get(t["status"], 0) + 1
    ran = sum(v for k, v in status.items() if k != "skip")
    passed = status.get("pass", 0)
    unexpected = [t for t in tests if t["outcome"] == "unexpected-failure"]
    upass = [t for t in tests if t["outcome"] == "unexpected-pass"]
    by_cat = {}
    for t in tests:
        if t["outcome"] == "expected-failure":
            c = t["expected"]["category"]
            by_cat[c] = by_cat.get(c, 0) + 1

    summary = {
        "runner": a.runner, "mode": a.mode, "classes": len(classes), "tests": len(tests),
        "status": status, "pass_rate": round(100.0 * passed / ran, 2) if ran else 0.0,
        "expected_failures_by_category": by_cat,
        "unexpected_failures": len(unexpected), "unexpected_passes": len(upass),
        "wall_seconds": round(wall, 1),
    }
    (out / "results.json").write_text(json.dumps({"summary": summary, "tests": tests}, indent=1) + "\n",
                                      encoding="utf-8")
    lines = [f"libcore tests, runner {a.runner}, mode {a.mode}",
             f"classes {len(classes)}, tests {len(tests)}, wall {wall:.0f} s",
             "status: " + ", ".join(f"{k} {v}" for k, v in sorted(status.items())),
             f"pass rate (pass / run, skips excluded): {summary['pass_rate']}%",
             "expected failures by category: " +
             (", ".join(f"{k} {v}" for k, v in sorted(by_cat.items())) or "none"),
             f"unexpected failures: {len(unexpected)}", f"unexpected passes: {len(upass)}"]
    for t in unexpected:
        lines.append(f"  FAIL {t['id']} [{t['status']}] {t['reason'][:300]}")
    for t in upass:
        lines.append(f"  PASS {t['id']} (expected: {t['expected']['category']}, line {t['expected']['line']})")
    text = "\n".join(lines) + "\n"
    (out / "summary.txt").write_text(text, encoding="utf-8")
    # Suggested expectation lines for the unexpected failures.
    sugg = [f"{t['id']} | {categorize(t['reason'])} | {t['status']}: {t['reason'][:200]}"
            for t in unexpected]
    (out / "suggested-expectations.txt").write_text("\n".join(sugg) + ("\n" if sugg else ""),
                                                    encoding="utf-8")
    print(text, end="")
    print(f"results: {out / 'results.json'}")
    return 1 if unexpected else 0


if __name__ == "__main__":
    sys.exit(main())
