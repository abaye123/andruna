# Spike S8 guest build: AOSP's liblog and libbase as Android ELF shared
# objects, the libbase gtest suite as an Android PIE executable, and the
# boundary benchmark library. Driven by build.py (step `aosp`).
#
# The AOSP modules are the ones of aosp/CMakeLists.txt (same module files
# as the Windows-target build), compiled with aosp/cmake/android-elf.cmake.

cmake_minimum_required(VERSION 3.28)
project(andruna_s8_guest LANGUAGES C CXX ASM)

get_filename_component(ANDRUNA_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../.." ABSOLUTE)
set(ANDRUNA_AOSP_DIR "${ANDRUNA_ROOT}/third_party/aosp" CACHE PATH "AOSP sources")
get_filename_component(AOSP "${ANDRUNA_AOSP_DIR}" ABSOLUTE)

set(CMAKE_C_STANDARD 17)
set(CMAKE_C_EXTENSIONS ON)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_EXTENSIONS ON)
set(CMAKE_CXX_SCAN_FOR_MODULES OFF)

# The AOSP static libraries (built -fPIC); only what is linked below is
# built, unless ANDRUNA_S8_ALL_LAYERS asks for layer 2 as well.
option(ANDRUNA_S8_ALL_LAYERS "Also build layer 2 (libartbase, libdexfile, ...) as ELF" OFF)
if(ANDRUNA_S8_ALL_LAYERS)
  add_subdirectory("${ANDRUNA_ROOT}/aosp" aosp)
else()
  add_subdirectory("${ANDRUNA_ROOT}/aosp" aosp EXCLUDE_FROM_ALL)
endif()

set(SYSLIB "${CMAKE_SYSROOT}/usr/lib")
set(GUEST_LINK
  "${SYSLIB}/crtbegin_so.o"
  "-L${SYSLIB}" "-L${ANDRUNA_LIBCXX_DIR}/lib"
  -lc++ -lc -landruna_host
  "${ANDRUNA_ELF_BUILTINS}")

# liblog.so
add_library(log_so SHARED "${CMAKE_CURRENT_SOURCE_DIR}/empty.c")
target_link_libraries(log_so PRIVATE
  "$<LINK_LIBRARY:WHOLE_ARCHIVE,liblog>" ${GUEST_LINK})
set_target_properties(log_so PROPERTIES OUTPUT_NAME log)
target_link_options(log_so PRIVATE -Wl,-soname,liblog.so)

# libbase.so (fmtlib linked in, as on Android).
add_library(base_so SHARED "${CMAKE_CURRENT_SOURCE_DIR}/empty.c")
target_link_libraries(base_so PRIVATE
  "$<LINK_LIBRARY:WHOLE_ARCHIVE,libbase>" "$<LINK_LIBRARY:WHOLE_ARCHIVE,fmtlib>"
  log_so ${GUEST_LINK})
set_target_properties(base_so PROPERTIES OUTPUT_NAME base)
target_link_options(base_so PRIVATE -Wl,-soname,libbase.so)

# googletest and googlemock (static, as Android's libgtest/libgmock).
set(GT "${AOSP}/external/googletest")
add_library(gtest_s8 STATIC
  "${GT}/googletest/src/gtest-all.cc"
  "${GT}/googlemock/src/gmock-all.cc")
target_include_directories(gtest_s8 PUBLIC
  "${GT}/googletest/include" "${GT}/googlemock/include")
target_include_directories(gtest_s8 PRIVATE "${GT}/googletest" "${GT}/googlemock")
target_link_libraries(gtest_s8 PUBLIC aosp_android)
# gtest's own simple regular expressions instead of POSIX <regex.h>, which
# the guest libc shim does not have.
target_compile_definitions(gtest_s8 PUBLIC GTEST_HAS_POSIX_RE=0)

# libbase_test: the sources of Android.bp's libbase_test for the android
# target.
set(_b "${AOSP}/system/libbase")
add_executable(libbase_test)
aosp_sources(libbase_test "${_b}"
  cmsg_test.cpp
  endian_test.cpp
  errors_test.cpp
  expected_test.cpp
  file_test.cpp
  function_ref_test.cpp
  hex_test.cpp
  logging_splitters_test.cpp
  logging_test.cpp
  macros_test.cpp
  mapped_file_test.cpp
  no_destructor_test.cpp
  parsedouble_test.cpp
  parsebool_test.cpp
  parseint_test.cpp
  parsenetaddress_test.cpp
  process_test.cpp
  properties_test.cpp
  result_test.cpp
  scopeguard_test.cpp
  stringify_test.cpp
  stringprintf_test.cpp
  strings_test.cpp
  test_main.cpp
  test_utils_test.cpp
  chrono_utils_test.cpp)
set_target_properties(libbase_test PROPERTIES CXX_STANDARD 23)
target_compile_definitions(libbase_test PRIVATE _FILE_OFFSET_BITS=64)
target_include_directories(libbase_test PRIVATE "${_b}" "${_b}/include"
  "${AOSP}/system/core/libutils/include" "${AOSP}/system/logging/liblog/include")
target_link_libraries(libbase_test PRIVATE
  gtest_s8 fmtlib base_so log_so ${GUEST_LINK})
# The loader calls main through the dynamic symbol table.
target_link_options(libbase_test PRIVATE -Wl,--export-dynamic-symbol=main)

# The boundary benchmark (S8.md, "Boundary cost").
add_library(s8bench SHARED "${CMAKE_CURRENT_SOURCE_DIR}/bench/bench.c")
target_compile_options(s8bench PRIVATE -O2 -g)
target_link_libraries(s8bench PRIVATE ${GUEST_LINK})
target_link_options(s8bench PRIVATE -Wl,-soname,libs8bench.so)
