#!/usr/bin/env python3
"""Compares two PNG images pixel by pixel (standard library only).

Usage: pngdiff.py <actual.png> <reference.png> [--tolerance N] [--max-bad F]
                  [--diff <out.png>]

A pixel is bad when one of its channels (RGBA, 8 bits) differs by more than
`--tolerance` (default 8). The comparison fails when the images differ in
size or when more than `--max-bad` (default 0.002, a fraction of all pixels)
are bad. `--diff` writes an image with the bad pixels in red over a faded
copy of the reference.

Supports 8-bit RGB, RGBA, grey and grey-alpha images, non-interlaced, which
is what Skia's PNG encoder writes.
"""

import argparse
import struct
import sys
import zlib


def read_png(path):
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path}: not a PNG file")
    pos = 8
    idat = []
    width = height = depth = ctype = interlace = None
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, depth, ctype, _, _, interlace = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat.append(body)
        elif kind == b"IEND":
            break
    if depth != 8 or interlace != 0 or ctype not in (0, 2, 4, 6):
        raise ValueError(f"{path}: unsupported PNG (depth {depth}, colour type {ctype}, interlace {interlace})")
    channels = {0: 1, 2: 3, 4: 2, 6: 4}[ctype]
    raw = zlib.decompress(b"".join(idat))
    stride = width * channels
    out = bytearray(width * height * 4)
    prev = bytearray(stride)
    p = 0
    for y in range(height):
        f = raw[p]
        line = bytearray(raw[p + 1:p + 1 + stride])
        p += 1 + stride
        for i in range(stride):
            a = line[i - channels] if i >= channels else 0
            b = prev[i]
            c = prev[i - channels] if i >= channels else 0
            if f == 1:
                line[i] = (line[i] + a) & 255
            elif f == 2:
                line[i] = (line[i] + b) & 255
            elif f == 3:
                line[i] = (line[i] + ((a + b) >> 1)) & 255
            elif f == 4:
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pr = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 255
        prev = line
        for x in range(width):
            s = line[x * channels:(x + 1) * channels]
            if channels == 1:
                px = (s[0], s[0], s[0], 255)
            elif channels == 2:
                px = (s[0], s[0], s[0], s[1])
            elif channels == 3:
                px = (s[0], s[1], s[2], 255)
            else:
                px = tuple(s)
            o = (y * width + x) * 4
            out[o:o + 4] = bytes(px)
    return width, height, out


def write_png(path, width, height, rgba):
    rows = b"".join(b"\x00" + bytes(rgba[y * width * 4:(y + 1) * width * 4]) for y in range(height))

    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(rows, 9)))
        f.write(chunk(b"IEND", b""))


def compare(actual, reference, tolerance=8, max_bad=0.002, diff=None):
    """Returns (ok, message)."""
    aw, ah, a = read_png(actual)
    rw, rh, r = read_png(reference)
    if (aw, ah) != (rw, rh):
        return False, f"size {aw}x{ah}, reference {rw}x{rh}"
    bad = 0
    worst = 0
    out = bytearray(r) if diff else None
    for i in range(0, len(a), 4):
        d = max(abs(a[i + k] - r[i + k]) for k in range(4))
        worst = max(worst, d)
        if d > tolerance:
            bad += 1
            if out is not None:
                out[i:i + 4] = b"\xff\x00\x00\xff"
        elif out is not None:
            out[i:i + 3] = bytes(255 - (255 - v) // 4 for v in r[i:i + 3])
    total = aw * ah
    if diff:
        write_png(diff, aw, ah, out)
    msg = f"{bad} of {total} pixels differ by more than {tolerance} (largest difference {worst})"
    return bad <= max_bad * total, msg


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("actual")
    ap.add_argument("reference")
    ap.add_argument("--tolerance", type=int, default=8)
    ap.add_argument("--max-bad", type=float, default=0.002)
    ap.add_argument("--diff")
    a = ap.parse_args()
    ok, msg = compare(a.actual, a.reference, a.tolerance, a.max_bad, a.diff)
    print(("match: " if ok else "MISMATCH: ") + msg)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
