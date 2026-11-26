# CMake toolchain file: llvm-mingw clang for Windows x64 or ARM64.
#
# Inputs (cache variables, normally passed by `cargo xtask aosp-build`):
#   ANDRUNA_LLVM_MINGW  llvm-mingw install directory (default: the
#                       ANDRUNA_LLVM_MINGW environment variable, else
#                       C:/tools/llvm-mingw)
#   ANDRUNA_ARCH        x64 or arm64
#
# The compilers are the plain clang drivers with an explicit --target, so
# the same llvm-mingw installation builds both architectures on any host.

set(CMAKE_SYSTEM_NAME Windows)

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

if(ANDRUNA_ARCH STREQUAL "x64")
  set(CMAKE_SYSTEM_PROCESSOR AMD64)
  set(ANDRUNA_TRIPLE x86_64-w64-windows-gnu)
elseif(ANDRUNA_ARCH STREQUAL "arm64")
  set(CMAKE_SYSTEM_PROCESSOR ARM64)
  set(ANDRUNA_TRIPLE aarch64-w64-windows-gnu)
else()
  message(FATAL_ERROR "ANDRUNA_ARCH must be x64 or arm64, not '${ANDRUNA_ARCH}'")
endif()

# Make the inputs visible to try_compile projects.
list(APPEND CMAKE_TRY_COMPILE_PLATFORM_VARIABLES ANDRUNA_LLVM_MINGW ANDRUNA_ARCH)

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
set(CMAKE_RC_COMPILER "${_bin}/llvm-rc${_exe}" CACHE FILEPATH "")

# Only static libraries are produced in this phase; the compiler checks must
# not try to link a program against the (not yet existing) Linux runtime.
set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)

set(CMAKE_FIND_ROOT_PATH "${ANDRUNA_LLVM_MINGW}/${ANDRUNA_TRIPLE}")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
