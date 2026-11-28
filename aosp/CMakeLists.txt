# CMake build of the AOSP modules Andruna uses, compiled with llvm-mingw for
# Windows x64 or ARM64 with Android (bionic) source semantics.
#
# Configure and build with `cargo xtask aosp-build --arch x64|arm64`, which
# runs, in effect:
#
#   cmake -G Ninja -S aosp -B out/aosp/<arch> \
#     -DCMAKE_TOOLCHAIN_FILE=aosp/cmake/toolchain-llvm-mingw.cmake \
#     -DANDRUNA_ARCH=<arch> -DANDRUNA_AOSP_DIR=third_party/aosp
#   cmake --build out/aosp/<arch>
#
# Each file in modules/ mirrors one Android.bp module (same name, same
# sources as its Android target). Only static libraries are produced.

cmake_minimum_required(VERSION 3.28)
project(andruna_aosp LANGUAGES C CXX ASM)

set(ANDRUNA_AOSP_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../third_party/aosp"
    CACHE PATH "Directory with the fetched AOSP projects")
get_filename_component(AOSP "${ANDRUNA_AOSP_DIR}" ABSOLUTE)
if(NOT EXISTS "${AOSP}/bionic/libc/include/stdio.h")
  message(FATAL_ERROR "AOSP sources not found in ${AOSP}; run `cargo xtask fetch`")
endif()

option(ANDRUNA_AOSP_RUNTIME
  "Also compile libart (the runtime). Not expected to build yet; see docs/plan/spike-results/S1.md"
  OFF)

find_package(Python3 REQUIRED COMPONENTS Interpreter)

set(CMAKE_C_STANDARD 17)
set(CMAKE_C_EXTENSIONS ON)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_EXTENSIONS ON)
# No C++20 modules in AOSP; skip the dependency scan.
set(CMAKE_CXX_SCAN_FOR_MODULES OFF)

if(ANDRUNA_AOSP_ELF)
  # Android ELF objects (spike S8), set by cmake/toolchain-android-elf.cmake.
  include(cmake/android-elf.cmake)
else()
  include(cmake/android.cmake)
endif()

# Layer 1: logging, base utilities.
include(modules/fmtlib.cmake)
include(modules/liblog.cmake)
include(modules/libbase.cmake)
include(modules/libcutils.cmake)
include(modules/libutils.cmake)

# Layer 2: compression, zip, ART base libraries.
include(modules/libz.cmake)
include(modules/liblz4.cmake)
include(modules/libziparchive.cmake)
include(modules/libartpalette.cmake)
include(modules/libartbase.cmake)
include(modules/libdexfile.cmake)

if(ANDRUNA_AOSP_RUNTIME)
  include(modules/libart.cmake)
  if(ANDRUNA_AOSP_ELF)
    # Generated headers, assembly, nterp and the libraries libart links
    # (spike S1 round 3, Android ELF build only).
    include(modules/art_elf.cmake)
    include(modules/icu.cmake)
    include(modules/boringssl.cmake)
    include(modules/libcore_natives.cmake)
    include(modules/conscrypt.cmake)
    if(ANDRUNA_AOSP_FRAMEWORK)
      # The framework's natives built so far (guest stage framework).
      include(modules/framework_natives.cmake)
    endif()
  endif()
endif()
