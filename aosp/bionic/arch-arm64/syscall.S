// long syscall(long number, ...) for the Andruna guest bionic (arm64).
//
// Replaces bionic's arch-arm64/bionic/syscall.S, which executes `svc #0`.
// The number moves to x8 and the arguments one register down, as there, and
// the host's __andruna_syscall is called instead of the kernel. See
// aosp/bionic/README.md.

#include <private/bionic_asm.h>

ENTRY(syscall)
    stp     x29, x30, [sp, #-16]!
    .cfi_def_cfa_offset 16
    .cfi_rel_offset x29, 0
    .cfi_rel_offset x30, 8
    mov     x29, sp

    mov     x8, x0
    mov     x0, x1
    mov     x1, x2
    mov     x2, x3
    mov     x3, x4
    mov     x4, x5
    mov     x5, x6
    bl      __andruna_syscall

    ldp     x29, x30, [sp], #16
    .cfi_def_cfa_offset 0
    .cfi_restore x29
    .cfi_restore x30
    DO_SYSCALL_RETURN
END(syscall)

NOTE_GNU_PROPERTY()
