#!/usr/bin/env python3
"""Generates bionic's system call stubs for the Andruna guest.

Bionic generates one assembler stub per line of libc/SYSCALLS.TXT with
libc/tools/gensyscalls.py. In Andruna the guest libc runs in a Windows
process, so a stub cannot execute `syscall` (x86_64) or `svc #0` (arm64):
it calls the host function `__andruna_syscall` instead, which andruna-linker
binds to the Linux layer (andruna-lx).

Convention of `__andruna_syscall` (see aosp/bionic/README.md):

- arguments in the C argument registers (x86_64: rdi rsi rdx rcx r8 r9;
  arm64: x0-x5), the system call number in rax (x86_64) or x8 (arm64);
- result in rax / x0, the raw kernel value (-4095..-1 is -errno);
- registers preserved as by a normal System V / AAPCS64 call.

This script imports bionic's own gensyscalls.py from the source tree and
replaces only the per-architecture call templates, so the parser, the
aliases and the hidden symbols stay exactly bionic's.

Usage: gensyscalls_andruna.py <arch> <bionic>/libc/SYSCALLS.TXT > out.S
"""

import importlib.util
import sys
from pathlib import Path

X86_64_CALL = """\
    subq    $8, %%rsp
    .cfi_adjust_cfa_offset 8
    movl    $%(__NR_name)s, %%eax
    call    __andruna_syscall@PLT
    addq    $8, %%rsp
    .cfi_adjust_cfa_offset -8
    DO_SYSCALL_RETURN
END(%(func)s)
"""

ARM64_CALL = """\
    stp     x29, x30, [sp, #-16]!
    .cfi_def_cfa_offset 16
    .cfi_rel_offset x29, 0
    .cfi_rel_offset x30, 8
    mov     x29, sp
    mov     x8, %(__NR_name)s
    bl      __andruna_syscall
    ldp     x29, x30, [sp], #16
    .cfi_def_cfa_offset 0
    .cfi_restore x29
    .cfi_restore x30
    DO_SYSCALL_RETURN
END(%(func)s)
"""


def load_gensyscalls(syscalls_txt: Path):
    tool = syscalls_txt.parent / "tools" / "gensyscalls.py"
    spec = importlib.util.spec_from_file_location("bionic_gensyscalls", tool)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in ("x86_64", "arm64"):
        sys.exit("usage: gensyscalls_andruna.py x86_64|arm64 SYSCALLS.TXT")
    arch, txt = sys.argv[1], Path(sys.argv[2])
    g = load_gensyscalls(txt)
    g.x86_64_call = X86_64_CALL
    # bionic's arm64 template carries the ENTRY header itself (the x86_64
    # generator adds it separately).
    g.arm64_call = g.syscall_stub_header + ARM64_CALL
    original = g.x86_64_genstub

    def x86_64_genstub(syscall):
        # bionic moves rcx to r10 for the kernel; the host takes the fourth
        # argument in rcx like any C function, so the move is dropped.
        return original(syscall).replace("    movq    %rcx, %r10\n", "")

    g.x86_64_genstub = x86_64_genstub
    g.main(arch, str(txt))


if __name__ == "__main__":
    main()
