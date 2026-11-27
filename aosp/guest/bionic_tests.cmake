# Stage tests: bionic_tests, a subset of bionic's own gtest suite
# (bionic/tests, Android.bp's libBionicStandardTests) for the areas the
# guest libc must get right first: stdio, strings, stdlib, locale, wide
# characters, malloc, pthread, time, ctype, math, C++ runtime support.
#
# The test files are compiled as Android does (bionic_tests_defaults
# flags), against libbase for the helpers in bionic/tests/utils.h, and run
# with gtest's own main instead of bionic's isolating runner, which forks
# per test (not possible on Windows).
#
# Left out for now: test files that need fork/exec, signals, sockets,
# /proc, ptrace or the test libraries of bionic/tests/libs (dlfcn, dlext,
# elftls, ifunc, ...); they are listed in aosp/bionic/README.md.

include("${CMAKE_CURRENT_SOURCE_DIR}/libbase.cmake")

set(_t "${AOSP}/bionic/tests")
set(ANDRUNA_BIONIC_TEST_FILES
  __cxa_atexit_test.cpp
  __cxa_demangle_test.cpp
  __cxa_thread_atexit_test.cpp
  alloca_test.cpp
  byteswap_test.cpp
  ctype_test.cpp
  endian_test.cpp
  inttypes_test.cpp
  libgen_test.cpp
  locale_test.cpp
  malloc_test.cpp
  math_test.cpp
  pthread_test.cpp
  stdio_test.cpp
  stdlib_test.cpp
  string_test.cpp
  strings_test.cpp
  time_test.cpp
  uchar_test.cpp
  wchar_test.cpp
  wctype_test.cpp
  CACHE STRING "bionic test files built into bionic_tests")

add_executable(bionic_tests)
foreach(f ${ANDRUNA_BIONIC_TEST_FILES})
  target_sources(bionic_tests PRIVATE "${_t}/${f}")
endforeach()
# Helpers the test files call (buffer checks, Errno printing, GWP-ASan and
# unwinding helpers).
target_sources(bionic_tests PRIVATE "${_t}/gtest_globals.cpp"
  "${_t}/buffer_tests.cpp" "${_t}/utils.cpp" "${_t}/gwp_asan_test.cpp"
  "${_t}/stack_unwinding_test.cpp"
  "${AOSP}/external/googletest/googletest/src/gtest_main.cc")
set_target_properties(bionic_tests PROPERTIES CXX_STANDARD 20)
target_include_directories(bionic_tests PRIVATE
  "${_t}"
  "${AOSP}/bionic/libc"
  "${AOSP}/bionic/libc/platform"
  "${AOSP}/bionic/libc/async_safe/include"
  "${AOSP}/system/core/libcutils/include"
  "${AOSP}/system/libbase/include"
  "${AOSP}/system/logging/liblog/include"
  "${AOSP}/external/gwp_asan"
  "${AOSP}/external/tinyxml2")
target_compile_options(bionic_tests PRIVATE
  -fstack-protector-all -fno-builtin -fexceptions -ftrivial-auto-var-init=zero
  -Wno-deprecated-declarations -Wno-invalid-offsetof)
# libtinyxml2 (aosp/modules/libartbase.cmake): malloc_test parses
# malloc_info's XML.
target_link_libraries(bionic_tests PRIVATE gtest_guest base_so log_so libtinyxml2 -ldl_android)
guest_executable(bionic_tests)
