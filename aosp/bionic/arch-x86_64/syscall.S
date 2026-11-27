// long syscall(long number, ...) for the Andruna guest bionic (x86_64).
//
// Replaces bionic's arch-x86_64/bionic/syscall.S, which executes the
// `syscall` instruction. Here the number moves to %rax and the arguments one
// register down, as there, but the call goes to the host's
// __andruna_syscall, which takes the fourth argument in %rcx like any C
// function (not in %r10 as the kernel does). See aosp/bionic/README.md.

#include <private/bionic_asm.h>

ENTRY(syscall)
    mov     %edi, %eax
    mov     %rsi, %rdi
    mov     %rdx, %rsi
    mov     %rcx, %rdx
    mov     %r8,  %rcx
    mov     %r9,  %r8
    mov     8(%rsp), %r9
    subq    $8, %rsp
    .cfi_adjust_cfa_offset 8
    call    __andruna_syscall@PLT
    addq    $8, %rsp
    .cfi_adjust_cfa_offset -8
    DO_SYSCALL_RETURN
END(syscall)
