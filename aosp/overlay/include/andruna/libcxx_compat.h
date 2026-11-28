/*
 * Included first in ART's, Skia's and HWUI's C++ translation units
 * (-include; art_defaults, aosp/modules/skia.cmake, hwui.cmake).
 *
 * AOSP is built with the libc++ revision in its own prebuilt clang, whose
 * headers still include some standard headers transitively. The newer
 * libc++ in llvm-mingw removed those transitive includes, so a few AOSP
 * files that rely on them do not compile. Instead of patching each file,
 * the headers they rely on are included here. Each entry names the first
 * file that needed it.
 */
#ifndef ANDRUNA_LIBCXX_COMPAT_H
#define ANDRUNA_LIBCXX_COMPAT_H

#ifdef __cplusplus
#include <vector> /* art/libartbase/base/unix_file/fd_file.h */
#include <cstdlib> /* external/skia/client_utils/android/FrontBufferedStream.cpp */
#endif

#endif
