/*
 * Included first in every AOSP translation unit (-include).
 *
 * AOSP code is compiled for a Windows target, whose data model is LLP64:
 * `long` is 32 bits, pointers and `long long` are 64 bits. Bionic and the
 * kernel UAPI headers are written for LP64 and use `long` for 64-bit ABI
 * types. The kernel types are fixed here, before any header defines them;
 * bionic's own typedefs are fixed by patches/bionic (ANDRUNA blocks).
 * See "Data model" in aosp/README.md for what is and is not covered.
 */
#ifndef ANDRUNA_DATA_MODEL_H
#define ANDRUNA_DATA_MODEL_H

#if !defined(ANDRUNA)
#error "andruna/data_model.h is only for the ANDRUNA build of AOSP code"
#endif

#if __SIZEOF_POINTER__ != 8 || __SIZEOF_LONG_LONG__ != 8
#error "64-bit targets only"
#endif

/* asm-generic/posix_types.h keeps these when they are already defined, as
 * it does for x32. Every derived kernel type (size_t, ssize_t, off_t,
 * time_t, clock_t, suseconds_t, ino_t, ...) becomes 64 bits. */
typedef long long __kernel_long_t;
typedef unsigned long long __kernel_ulong_t;
#define __kernel_long_t __kernel_long_t
#define __kernel_ulong_t __kernel_ulong_t

#endif
