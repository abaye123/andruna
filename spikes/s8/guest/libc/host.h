// Functions of the guest libc shim that the host (the spike's loader, in
// Rust, System V convention) provides and that bionic's headers do not
// declare. Everything bionic declares and the shim does not define (file
// descriptors, memory, time, threads, allocation) also comes from the host,
// mostly forwarded to andruna-lx.

#pragma once

#include <stddef.h>

// Formats one floating-point conversion `spec` (for example "%-12.4e") of
// `v` into `buf` with the host C runtime. Returns the length, as snprintf.
int __andruna_fmt_fp(char* buf, size_t cap, const char* spec, double v);

// Runs the C++ thread_local destructors registered on this thread.
void __andruna_libc_thread_exit(void);
