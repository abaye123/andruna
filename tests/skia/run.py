#!/usr/bin/env python3
"""Runs the Skia scene test (tests/skia/skia_scene.cpp) and compares its
image with the reference.

  python tests/skia/run.py [--arch x64|arm64] [--backend raster|gl] [--update]
                           [--windows-fonts]

Inputs:
  out/guest/<arch>/hwui/out/skia_scene     aosp/tools/build_guest.py hwui
  out/guest/<arch>/sysroot, libcxx-install  the guest bionic and libc++
  out/gles/<arch>/guest                     andruna-gles' guest libraries
  out/fonts/test                            tests/skia/fonts.py
  target/release/andruna-gles-run.exe       (or ANDRUNA_RUNNER); andruna-elf
                                            works for the raster back end

The guest root is out/skia-test/<arch>/root: fonts.xml goes to
/system/etc/fonts.xml and the fonts to /system/fonts, the places Skia's
Android font manager reads, as on a device. The image is written to
out/skia-test/<arch>/scene_<backend>.png; a mismatch also writes
scene_<backend>_diff.png. `--update` replaces the reference instead.

`--windows-fonts` uses the interim font configuration of ADR 0046 instead:
fonts.xml from aosp/fonts/windows_fonts.py and the Windows fonts it names
(copied into the local guest root only). Its image depends on the Windows
build, so it is not compared; the program's own checks still run.

The GL back end runs on ANGLE (ANDRUNA_ANGLE_DIR); set
ANDRUNA_GLES_DEVICE=warp for the software rasterizer (CI).
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.dont_write_bytecode = True
sys.path.insert(0, str(HERE))
import pngdiff  # noqa: E402

# GL output differs from raster in anti-aliasing and text rasterization, so
# each back end has its own reference; both use the same tolerance.
TOLERANCE = 8
MAX_BAD = 0.002


def runner():
    if os.environ.get("ANDRUNA_RUNNER"):
        p = Path(os.environ["ANDRUNA_RUNNER"])
        return p, (["run"] if p.stem == "andruna-elf" else [])
    for name, args in (("andruna-gles-run.exe", []), ("andruna-elf.exe", ["run"])):
        for profile in ("release", "debug"):
            for triple in ("", "x86_64-pc-windows-gnullvm", "aarch64-pc-windows-gnullvm"):
                p = ROOT / "target" / triple / profile / name
                if p.exists():
                    return p, args
    sys.exit("no runner: build andruna-gles (cargo build -p andruna-gles --release) or set ANDRUNA_RUNNER")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arch", default="x64", choices=("x64", "arm64"))
    ap.add_argument("--backend", default="raster", choices=("raster", "gl"))
    ap.add_argument("--update", action="store_true")
    ap.add_argument("--windows-fonts", action="store_true")
    a = ap.parse_args()

    guest = ROOT / "out" / "guest" / a.arch
    exe = guest / "hwui" / "out" / "skia_scene"
    if not exe.exists():
        sys.exit(f"{exe} is missing; run python aosp/tools/build_guest.py --arch {a.arch} hwui")
    fonts = Path(os.environ.get("ANDRUNA_TEST_FONTS", ROOT / "out" / "fonts" / "test"))
    if not (fonts / "Roboto-Regular.ttf").exists():
        sys.exit(f"fonts missing in {fonts}; run python tests/skia/fonts.py")

    work = ROOT / "out" / "skia-test" / a.arch
    root = work / "root"
    for d in ("system/etc", "system/fonts", "data/local/tmp"):
        (root / d).mkdir(parents=True, exist_ok=True)
    sysfonts = root / "system" / "fonts"
    for f in sysfonts.iterdir():
        f.unlink()
    shutil.copy2(fonts / "Roboto-Regular.ttf", sysfonts / "Roboto-Regular.ttf")
    hebrew_family = []
    if a.windows_fonts:
        sys.path.insert(0, str(ROOT / "aosp" / "fonts"))
        import windows_fonts
        windir = Path(os.environ.get("WINDIR", "C:/Windows")) / "Fonts"
        text, _ = windows_fonts.generate(windir, fonts)
        (root / "system" / "etc" / "fonts.xml").write_text(text, encoding="utf-8")
        for name in set(re.findall(r">([^<>\s]+\.tt[fc])", text)):
            if (windir / name).exists():
                shutil.copy2(windir / name, sysfonts / name)
        hebrew_family = ["SegoeUI"]
    else:
        shutil.copy2(HERE / "fonts.xml", root / "system" / "etc" / "fonts.xml")
        for f in fonts.glob("*.ttf"):
            shutil.copy2(f, sysfonts / f.name)

    libs = [guest / "sysroot" / "usr" / "lib", guest / "libcxx-install" / "lib",
            ROOT / "out" / "gles" / a.arch / "guest"]
    run, pre = runner()
    cmd = [str(run)] + pre
    for d in libs:
        cmd += ["-L", str(d)]
    cmd += ["--root", str(root), str(exe), "/data/local/tmp/scene.png"]
    cmd.append(a.backend)
    cmd += hebrew_family
    # ICU's data file where Android's i18n module has it; libandroidicuinit
    # maps it from $ANDROID_I18N_ROOT/etc/icu (line breaking needs it).
    icu = root / "apex" / "com.android.i18n" / "etc" / "icu"
    icu.mkdir(parents=True, exist_ok=True)
    aosp = Path(os.environ.get("ANDRUNA_AOSP_DIR", ROOT / "third_party" / "aosp"))
    dat = next((aosp / "external" / "icu" / "icu4c" / "source" / "stubdata").glob("icudt*l.dat"), None)
    if dat is None:
        sys.exit("ICU data file missing (third_party/aosp/external/icu)")
    if not (icu / dat.name).exists() or (icu / dat.name).stat().st_size != dat.stat().st_size:
        shutil.copy2(dat, icu / dat.name)
    env = dict(os.environ)
    env["ANDRUNA_GUEST_ENV"] = ";".join(filter(None, [
        "ANDROID_I18N_ROOT=/apex/com.android.i18n",
        "ANDROID_TZDATA_ROOT=/apex/com.android.tzdata",
        env.get("ANDRUNA_GUEST_ENV", "")]))
    angle = ROOT / "out" / "angle" / a.arch / "bin"
    if "ANDRUNA_ANGLE_DIR" not in env and angle.is_dir():
        env["ANDRUNA_ANGLE_DIR"] = str(angle)
    elif "ANDRUNA_ANGLE_DIR" in env:
        # The DLLs are loaded by absolute path.
        env["ANDRUNA_ANGLE_DIR"] = str(Path(env["ANDRUNA_ANGLE_DIR"]).resolve())
    print("+", " ".join(cmd), flush=True)
    r = subprocess.run(cmd, env=env)
    produced = root / "data" / "local" / "tmp" / "scene.png"
    out = work / f"scene_{a.backend}.png"
    if produced.exists():
        shutil.copy2(produced, out)
        produced.unlink()
    if r.returncode != 0:
        print(f"skia_scene failed with exit code {r.returncode}")
        return 1
    if a.windows_fonts:
        print(f"image (not compared, Windows fonts): {out}")
        return 0
    reference = HERE / "reference" / f"scene_{a.backend}.png"
    if a.update:
        reference.parent.mkdir(exist_ok=True)
        shutil.copy2(out, reference)
        print(f"reference updated: {reference}")
        return 0
    if not reference.exists():
        print(f"no reference {reference}; run with --update after checking {out}")
        return 1
    diff = work / f"scene_{a.backend}_diff.png"
    ok, msg = pngdiff.compare(out, reference, TOLERANCE, MAX_BAD, diff)
    print(("match: " if ok else "MISMATCH: ") + msg)
    if ok and diff.exists():
        diff.unlink()
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
