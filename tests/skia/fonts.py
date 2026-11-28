#!/usr/bin/env python3
"""Downloads the font files the Skia scene test renders with.

The files come from AOSP at the tag in aosp/aosp.lock, one file each
(not the whole font projects, which are large), and are checked against
the SHA-256 recorded below. They are test data only: nothing in a build
output ships them (see docs/decisions/0046-guest-fonts.md).

  Roboto-Regular.ttf          external/roboto-fonts, Apache-2.0
  NotoSansHebrew-Regular.ttf  external/noto-fonts, OFL-1.1 (test data only)

Usage: fonts.py [<dir>]   (default: out/fonts/test)
"""

import base64
import hashlib
import re
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

FONTS = [
    ("platform/external/roboto-fonts", "font/3.005/Roboto-Regular.ttf",
     "9ca9debb09459bf4e3e7f826f5cd0f35f253902b85684921fce2ba3f28dd0f50"),
    ("platform/external/noto-fonts", "notosanshebrew/font/1.040/NotoSansHebrew-Regular.ttf",
     "ef877e0e136f7942900b7ec8c866c180562d45def425b8bfe2dc0a992e3c2fd7"),
]


def tag():
    text = (ROOT / "aosp" / "aosp.lock").read_text(encoding="utf-8")
    return re.search(r'^tag = "([^"]+)"', text, re.M).group(1)


def main():
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "out" / "fonts" / "test"
    out.mkdir(parents=True, exist_ok=True)
    t = tag()
    for project, path, sha in FONTS:
        dst = out / Path(path).name
        if dst.exists() and hashlib.sha256(dst.read_bytes()).hexdigest() == sha:
            continue
        url = f"https://android.googlesource.com/{project}/+/refs/tags/{t}/{path}?format=TEXT"
        print(f"downloading {url}", flush=True)
        with urllib.request.urlopen(url, timeout=120) as r:
            data = base64.b64decode(r.read())
        got = hashlib.sha256(data).hexdigest()
        if got != sha:
            sys.exit(f"{path}: SHA-256 {got}, expected {sha}")
        dst.write_bytes(data)
    print(f"fonts in {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
