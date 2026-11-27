# The guest userland as Android ELF (ADR 0013): bionic, then AOSP modules
# and tests linked against it. Configured once per stage by
# aosp/tools/build_guest.py with aosp/cmake/toolchain-android-elf.cmake:
#
#   ANDRUNA_GUEST_STAGE=bionic        libc.so, libm.so, libdl.so,
#                                     ld-android.so (the bridge),
#                                     libandruna_host.so, crt objects
#   ANDRUNA_GUEST_STAGE=aosp          liblog.so, libbase.so, libbase_test
#   ANDRUNA_GUEST_STAGE=tests         bionic_tests (a subset of bionic's
#                                     own gtest suite)
#   ANDRUNA_GUEST_STAGE=linker-tests  crates/andruna-linker/tests/guest/*
#
# libc++ is built between the bionic and aosp stages with LLVM's runtimes
# build (build_guest.py, step libcxx).

cmake_minimum_required(VERSION 3.28)
project(andruna_guest LANGUAGES C CXX ASM)

get_filename_component(ANDRUNA_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../.." ABSOLUTE)
set(ANDRUNA_AOSP_DIR "${ANDRUNA_ROOT}/third_party/aosp" CACHE PATH "AOSP sources")
get_filename_component(AOSP "${ANDRUNA_AOSP_DIR}" ABSOLUTE)
set(ANDRUNA_LLVM_PROJECT "" CACHE PATH "llvm-project sources at the compiler's tag")
set(ANDRUNA_GUEST_STAGE "bionic" CACHE STRING "bionic, aosp, tests or linker-tests")
set(ANDRUNA_LLD "${ANDRUNA_LLVM_MINGW}/bin/ld.lld${CMAKE_EXECUTABLE_SUFFIX}")
if(CMAKE_HOST_WIN32)
  set(ANDRUNA_LLD "${ANDRUNA_LLVM_MINGW}/bin/ld.lld.exe")
endif()

if(ANDRUNA_ARCH STREQUAL "x64")
  set(BIONIC_ASM_ARCH x86)
else()
  set(BIONIC_ASM_ARCH arm64)
endif()

find_package(Python3 REQUIRED COMPONENTS Interpreter)

set(CMAKE_C_STANDARD 17)
set(CMAKE_C_EXTENSIONS ON)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_EXTENSIONS ON)
set(CMAKE_CXX_SCAN_FOR_MODULES OFF)

if(ANDRUNA_GUEST_STAGE STREQUAL "bionic")
  include("${ANDRUNA_ROOT}/aosp/modules/bionic.cmake")
  return()
endif()

# ---------------------------------------------------------------------------
# Linking against the guest sysroot, as the NDK driver would.

set(SYSLIB "${CMAKE_SYSROOT}/usr/lib")
set(ANDRUNA_LIBCXX_DIR "" CACHE PATH "libc++ install directory")
set(_libs "-L${SYSLIB}" "-L${ANDRUNA_LIBCXX_DIR}/lib" -lc++ -lm -ldl -lc -landruna_host
  "${ANDRUNA_ELF_BUILTINS}")

# A shared object: crtbegin_so.o first, crtend_so.o last.
function(guest_shared target soname)
  target_link_options(${target} BEFORE PRIVATE "${SYSLIB}/crtbegin_so.o" "-Wl,-soname,${soname}")
  target_link_libraries(${target} PRIVATE ${_libs} "${SYSLIB}/crtend_so.o")
  set_target_properties(${target} PROPERTIES PREFIX "" SUFFIX "")
endfunction()

# A PIE executable: crtbegin_dynamic.o first, crtend_android.o last.
function(guest_executable target)
  target_link_options(${target} BEFORE PRIVATE "${SYSLIB}/crtbegin_dynamic.o"
    -Wl,--dynamic-linker=/system/bin/linker64)
  target_link_libraries(${target} PRIVATE ${_libs} "${SYSLIB}/crtend_android.o")
endfunction()

if(ANDRUNA_GUEST_STAGE STREQUAL "linker-tests")
  include("${CMAKE_CURRENT_SOURCE_DIR}/linker_tests.cmake")
  return()
endif()

# The AOSP static libraries (built -fPIC); only what is linked is built.
add_subdirectory("${ANDRUNA_ROOT}/aosp" aosp EXCLUDE_FROM_ALL)

# googletest and googlemock (static, as Android's libgtest/libgmock).
set(GT "${AOSP}/external/googletest")
add_library(gtest_guest STATIC
  "${GT}/googletest/src/gtest-all.cc"
  "${GT}/googlemock/src/gmock-all.cc")
target_include_directories(gtest_guest PUBLIC
  "${GT}/googletest/include" "${GT}/googlemock/include")
target_include_directories(gtest_guest PRIVATE "${GT}/googletest" "${GT}/googlemock")
target_link_libraries(gtest_guest PUBLIC aosp_android)

if(ANDRUNA_GUEST_STAGE STREQUAL "aosp")
  include("${CMAKE_CURRENT_SOURCE_DIR}/libbase.cmake")
elseif(ANDRUNA_GUEST_STAGE STREQUAL "tests")
  include("${CMAKE_CURRENT_SOURCE_DIR}/bionic_tests.cmake")
else()
  message(FATAL_ERROR "unknown ANDRUNA_GUEST_STAGE '${ANDRUNA_GUEST_STAGE}'")
endif()
