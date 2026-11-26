# CMake toolchain file: llvm-mingw clang producing Android ELF code
# (`<arch>-linux-android`), for spike S8 (docs/plan/spikes.md).
#
# This is the alternative to toolchain-llvm-mingw.cmake: the objects are ELF,
# LP64, System V on x64 and AAPCS64 on arm64, exactly as on Android, and they
# are loaded by an ELF loader instead of the Windows loader.
#
# Inputs (cache variables):
#   ANDRUNA_LLVM_MINGW  llvm-mingw install directory (default: the
#                       ANDRUNA_LLVM_MINGW environment variable, else
#                       C:/tools/llvm-mingw)
#   ANDRUNA_ARCH        x64 or arm64
#   ANDRUNA_ELF_SYSROOT A sysroot with bionic's headers in usr/include and
#                       the guest libc in usr/lib (made by
#                       spikes/s8/guest/build.py)
#   ANDRUNA_ELF_API     Android API level of the triple (default 10000)
#
# Nothing is linked implicitly (-nostdlib): every shared object names its
# libraries, so the same file works before the guest libc exists.

set(CMAKE_SYSTEM_NAME Linux)
set(ANDRUNA_AOSP_ELF ON)

if(NOT ANDRUNA_LLVM_MINGW)
  if(DEFINED ENV{ANDRUNA_LLVM_MINGW})
    set(ANDRUNA_LLVM_MINGW "$ENV{ANDRUNA_LLVM_MINGW}")
  else()
    set(ANDRUNA_LLVM_MINGW "C:/tools/llvm-mingw")
  endif()
endif()
file(TO_CMAKE_PATH "${ANDRUNA_LLVM_MINGW}" ANDRUNA_LLVM_MINGW)
set(ANDRUNA_LLVM_MINGW "${ANDRUNA_LLVM_MINGW}" CACHE PATH "llvm-mingw directory")

if(NOT ANDRUNA_ARCH)
  set(ANDRUNA_ARCH x64)
endif()
set(ANDRUNA_ARCH "${ANDRUNA_ARCH}" CACHE STRING "Target architecture: x64 or arm64")
if(NOT ANDRUNA_ELF_API)
  # Platform code is built for the "current" API level, as Soong does.
  set(ANDRUNA_ELF_API 10000)
endif()

if(ANDRUNA_ARCH STREQUAL "x64")
  set(CMAKE_SYSTEM_PROCESSOR x86_64)
  set(ANDRUNA_TRIPLE x86_64-linux-android${ANDRUNA_ELF_API})
  set(_maxpage 4096)
elseif(ANDRUNA_ARCH STREQUAL "arm64")
  set(CMAKE_SYSTEM_PROCESSOR aarch64)
  set(ANDRUNA_TRIPLE aarch64-linux-android${ANDRUNA_ELF_API})
  # Android arm64 builds support 16 KiB pages.
  set(_maxpage 16384)
else()
  message(FATAL_ERROR "ANDRUNA_ARCH must be x64 or arm64, not '${ANDRUNA_ARCH}'")
endif()

if(NOT ANDRUNA_ELF_SYSROOT)
  message(FATAL_ERROR "set ANDRUNA_ELF_SYSROOT (see spikes/s8/guest/build.py)")
endif()
file(TO_CMAKE_PATH "${ANDRUNA_ELF_SYSROOT}" ANDRUNA_ELF_SYSROOT)
set(ANDRUNA_ELF_SYSROOT "${ANDRUNA_ELF_SYSROOT}" CACHE PATH "Guest sysroot")
set(CMAKE_SYSROOT "${ANDRUNA_ELF_SYSROOT}")

list(APPEND CMAKE_TRY_COMPILE_PLATFORM_VARIABLES
  ANDRUNA_LLVM_MINGW ANDRUNA_ARCH ANDRUNA_ELF_SYSROOT ANDRUNA_ELF_API)

set(_bin "${ANDRUNA_LLVM_MINGW}/bin")
if(CMAKE_HOST_WIN32)
  set(_exe ".exe")
else()
  set(_exe "")
endif()

set(CMAKE_C_COMPILER "${_bin}/clang${_exe}")
set(CMAKE_CXX_COMPILER "${_bin}/clang++${_exe}")
set(CMAKE_ASM_COMPILER "${_bin}/clang${_exe}")
set(CMAKE_C_COMPILER_TARGET ${ANDRUNA_TRIPLE})
set(CMAKE_CXX_COMPILER_TARGET ${ANDRUNA_TRIPLE})
set(CMAKE_ASM_COMPILER_TARGET ${ANDRUNA_TRIPLE})
set(CMAKE_AR "${_bin}/llvm-ar${_exe}" CACHE FILEPATH "")
set(CMAKE_RANLIB "${_bin}/llvm-ranlib${_exe}" CACHE FILEPATH "")
set(CMAKE_NM "${_bin}/llvm-nm${_exe}" CACHE FILEPATH "")
set(CMAKE_OBJCOPY "${_bin}/llvm-objcopy${_exe}" CACHE FILEPATH "")
set(CMAKE_READELF "${_bin}/llvm-readelf${_exe}" CACHE FILEPATH "")
set(CMAKE_STRIP "${_bin}/llvm-strip${_exe}" CACHE FILEPATH "")

# The compiler runtime is linked statically into every shared object, as on
# Android. llvm-mingw ships it for x86_64 Linux; for arm64 build.py
# compiles it from compiler-rt and passes ANDRUNA_ELF_BUILTINS.
if(NOT ANDRUNA_ELF_BUILTINS AND ANDRUNA_ARCH STREQUAL "x64")
  file(GLOB _rt "${ANDRUNA_LLVM_MINGW}/lib/clang/*/lib/linux/libclang_rt.builtins-x86_64.a")
  list(GET _rt 0 ANDRUNA_ELF_BUILTINS)
endif()
set(ANDRUNA_ELF_BUILTINS "${ANDRUNA_ELF_BUILTINS}" CACHE FILEPATH "compiler-rt builtins archive")
set(CMAKE_C_STANDARD_LIBRARIES_INIT "${ANDRUNA_ELF_BUILTINS}")
set(CMAKE_CXX_STANDARD_LIBRARIES_INIT "${ANDRUNA_ELF_BUILTINS}")
list(APPEND CMAKE_TRY_COMPILE_PLATFORM_VARIABLES ANDRUNA_ELF_BUILTINS)

set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# Android's link defaults for platform libraries.
set(_link "-fuse-ld=lld -nostdlib -Wl,--pack-dyn-relocs=android+relr -Wl,--hash-style=gnu -Wl,--build-id=sha1 -Wl,-z,max-page-size=${_maxpage} -Wl,-z,now -Wl,-z,relro")
set(CMAKE_SHARED_LINKER_FLAGS_INIT "${_link}")
set(CMAKE_MODULE_LINKER_FLAGS_INIT "${_link}")
set(CMAKE_EXE_LINKER_FLAGS_INIT "${_link} -pie")

set(CMAKE_FIND_ROOT_PATH "${ANDRUNA_ELF_SYSROOT}")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
