# Compilation environment for AOSP code: Android (bionic) source semantics,
# Windows object code. See aosp/README.md, "How AOSP code is compiled".
#
# Every AOSP target links the interface library `aosp_android`, which
# carries the flags below. Nothing here links a program; the Linux runtime
# (andruna-lx, bionic and a libc++ built for it) is not part of this build.

if(NOT ANDRUNA_TRIPLE)
  message(FATAL_ERROR "configure with -DCMAKE_TOOLCHAIN_FILE=aosp/cmake/toolchain-llvm-mingw.cmake")
endif()

set(BIONIC "${AOSP}/bionic")
if(ANDRUNA_ARCH STREQUAL "x64")
  set(BIONIC_ARCH x86_64)
  set(BIONIC_ASM_DIR asm-x86)
else()
  set(BIONIC_ARCH arm64)
  set(BIONIC_ASM_DIR asm-arm64)
endif()

# The clang resource directory (stddef.h, stdarg.h, intrinsics). It is added
# by hand because -nostdinc also drops it.
execute_process(
  COMMAND "${CMAKE_C_COMPILER}" --target=${ANDRUNA_TRIPLE} -print-resource-dir
  OUTPUT_VARIABLE CLANG_RESOURCE_DIR
  OUTPUT_STRIP_TRAILING_WHITESPACE
  COMMAND_ERROR_IS_FATAL ANY)
file(TO_CMAKE_PATH "${CLANG_RESOURCE_DIR}" CLANG_RESOURCE_DIR)

set(LIBCXX_INCLUDE "${ANDRUNA_LLVM_MINGW}/include/c++/v1")
if(NOT EXISTS "${LIBCXX_INCLUDE}/__config")
  message(FATAL_ERROR "libc++ headers not found in ${LIBCXX_INCLUDE}")
endif()

add_library(aosp_android INTERFACE)

# Predefined macros of the windows-gnu target that would select Windows code
# paths in AOSP sources (and in libc++), removed so that the sources see an
# Android target. The object code stays Windows code (COFF, Windows calling
# convention, LLP64).
set(_undef
  _WIN32 _WIN64 WIN32 WIN64 WINNT __WIN32 __WIN32__ __WIN64 __WIN64__
  __WINNT __WINNT__ __MINGW32__ __MINGW64__ __MSVCRT__)
list(TRANSFORM _undef PREPEND "-U")

target_compile_options(aosp_android INTERFACE
  ${_undef}
  "SHELL:-include andruna/data_model.h"
  # Bionic headers, not the mingw-w64 headers.
  -nostdinc
  $<$<COMPILE_LANGUAGE:CXX>:-nostdinc++>
  # Linux layout rules: 32-bit wchar_t, GCC bitfield layout.
  -fno-short-wchar
  -mno-ms-bitfields
  # Android platform defaults.
  -fno-exceptions
  # On by default for Android targets, off for windows-gnu.
  $<$<COMPILE_LANGUAGE:CXX>:-fsized-deallocation>
  -funwind-tables
  -ffunction-sections
  -fdata-sections
  -fno-strict-aliasing
  -fdebug-prefix-map=${AOSP}=aosp
  -gline-tables-only
  -gcodeview
  -O2
)

# Upstream builds with its own warning set and -Werror on an older
# toolchain, so warnings are off by default. ANDRUNA_AOSP_WARNINGS turns on
# the ones that point at data-model and ABI problems (see
# aosp/README.md, "Data model").
option(ANDRUNA_AOSP_WARNINGS "Report format, narrowing and pointer-size warnings" OFF)
if(ANDRUNA_AOSP_WARNINGS)
  target_compile_options(aosp_android INTERFACE
    -Wno-everything -Wformat -Wshorten-64-to-32 -Wpointer-to-int-cast
    -Wint-to-pointer-cast -Wvoid-pointer-to-int-cast)
else()
  target_compile_options(aosp_android INTERFACE -w)
endif()

target_compile_definitions(aosp_android INTERFACE
  ANDRUNA=1
  __linux__=1 __linux=1 linux=1 __unix__=1 __unix=1 unix=1
  __ANDROID__=1 ANDROID=1
  # A platform build: every bionic API is available (clang defines this
  # from an Android triple only).
  __ANDROID_MIN_SDK_VERSION__=10000
  # Pointer size and ABI selection in bionic and AOSP headers; see
  # "Data model" in aosp/README.md.
  __LP64__=1 _LP64=1
  _FILE_OFFSET_BITS=64
  _GNU_SOURCE=1
)

# Search order: Andruna's overlay, libc++, bionic (libc, kernel UAPI), then
# the compiler's own headers. These are compile options rather than include
# directories because CMake drops include directories it considers implicit
# for the compiler (libc++ and the resource directory), which -nostdinc
# removed.
target_compile_options(aosp_android INTERFACE
  "SHELL:-isystem ${CMAKE_CURRENT_LIST_DIR}/../overlay/include"
  "$<$<COMPILE_LANGUAGE:CXX>:SHELL:-isystem ${LIBCXX_INCLUDE}>")
set(_sysinc
  "${BIONIC}/libc/include"
  "${BIONIC}/libc/kernel/uapi"
  "${BIONIC}/libc/kernel/uapi/${BIONIC_ASM_DIR}"
  "${BIONIC}/libc/kernel/android/uapi"
  "${CLANG_RESOURCE_DIR}/include")
foreach(dir ${_sysinc})
  target_compile_options(aosp_android INTERFACE "SHELL:-isystem ${dir}")
endforeach()

# Runs art/tools/generate_operator_out.py over `headers` (paths relative to
# ${AOSP}) and adds the result to `target`.
function(art_operator_out target local_path)
  set(out "${CMAKE_CURRENT_BINARY_DIR}/gen/${target}_operator_out.cc")
  set(abs)
  foreach(h ${ARGN})
    list(APPEND abs "${AOSP}/${h}")
  endforeach()
  add_custom_command(
    OUTPUT "${out}"
    COMMAND "${Python3_EXECUTABLE}" "${AOSP}/art/tools/generate_operator_out.py"
            "${local_path}" ${ARGN} > "${out}"
    WORKING_DIRECTORY "${AOSP}"
    DEPENDS ${abs} "${AOSP}/art/tools/generate_operator_out.py"
    COMMENT "Generating operator<< for ${target}"
    VERBATIM)
  target_sources(${target} PRIVATE "${out}")
endfunction()

# Prefixes every source with `dir`.
function(aosp_sources target dir)
  set(list)
  foreach(s ${ARGN})
    list(APPEND list "${dir}/${s}")
  endforeach()
  target_sources(${target} PRIVATE ${list})
endfunction()
