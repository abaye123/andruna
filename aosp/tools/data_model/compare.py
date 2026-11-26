#!/usr/bin/env python3
"""Compares type sizes and field offsets of bionic's headers as AOSP code
sees them in the ANDRUNA build (Windows target, LLP64, aosp/cmake/android.cmake)
with a real Android build of the same headers (LP64), on x64 and arm64.

Prints a Markdown table; rows that differ are in bold. Needs the fetched and
patched bionic checkout and llvm-mingw (ANDRUNA_LLVM_MINGW or
C:/tools/llvm-mingw). Usage: compare.py
"""
import os
import re
import subprocess
import sys

root = os.path.abspath(os.path.join(os.path.dirname(__file__), '..', '..', '..'))
B = root + '/third_party/aosp/bionic/libc'
clang = os.environ.get('ANDRUNA_LLVM_MINGW', 'C:/tools/llvm-mingw').replace(chr(92), '/') + '/bin/clang'
res = subprocess.run([clang, '-print-resource-dir'], capture_output=True, text=True).stdout.strip()
undef = '_WIN32 _WIN64 WIN32 WIN64 WINNT __WIN32 __WIN32__ __WIN64 __WIN64__ __WINNT __WINNT__ __MINGW32__ __MINGW64__ __MSVCRT__'.split()

def inc(asm):
    return ['-isystem', B + '/include', '-isystem', B + '/kernel/uapi', '-isystem', B + '/kernel/uapi/' + asm,
            '-isystem', B + '/kernel/android/uapi', '-isystem', res + '/include']

def andruna(triple, asm):
    return ([clang, '--target=' + triple] + ['-U' + u for u in undef] +
            ['-include', 'andruna/data_model.h', '-nostdinc', '-fno-short-wchar', '-mno-ms-bitfields',
             '-isystem', root + '/aosp/overlay/include'] + inc(asm) +
            ['-DANDRUNA=1', '-D__linux__=1', '-D__ANDROID__=1', '-D__LP64__=1', '-D_LP64=1',
             '-D__ANDROID_MIN_SDK_VERSION__=10000', '-D_GNU_SOURCE=1'])

def android(triple, asm):
    return [clang, '--target=' + triple, '-nostdinc'] + inc(asm) + ['-D_GNU_SOURCE=1']

def values(cmd):
    out = subprocess.run(cmd + ['-S', '-O1', '-o', '-', os.path.join(os.path.dirname(os.path.abspath(__file__)), 'layout.c')],
                         capture_output=True, text=True)
    if out.returncode:
        sys.exit(out.stderr)
    v, cur = {}, None
    for line in out.stdout.splitlines():
        m = re.match(r'^probe_(\w+):', line)
        if m:
            cur = m.group(1)
            continue
        m = re.match(r'^\s*\.(quad|xword)\s+(-?\d+)', line)
        if m and cur:
            v[cur] = int(m.group(2)); cur = None
    return v

cols = [('x64 Andruna', values(andruna('x86_64-w64-windows-gnu', 'asm-x86'))),
        ('x86_64 Android', values(android('x86_64-linux-android35', 'asm-x86'))),
        ('arm64 Andruna', values(andruna('aarch64-w64-windows-gnu', 'asm-arm64'))),
        ('arm64 Android', values(android('aarch64-linux-android35', 'asm-arm64')))]
print('| Probe | ' + ' | '.join(c for c, _ in cols) + ' |')
print('| --- |' + ' ---: |' * len(cols))
for k in cols[1][1]:
    row = [str(c[k]) if k in c else '?' for _, c in cols]
    diff = row[0] != row[1] or row[2] != row[3]
    print('| ' + ('**' + k + '**' if diff else k) + ' | ' + ' | '.join(row) + ' |')
