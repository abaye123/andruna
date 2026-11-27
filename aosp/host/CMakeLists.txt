# Host tools for the framework build, compiled as native Windows programs
# with llvm-mingw (docs/plan/phase2/framework-build.md, ADR 0023).
#
# Configured by aosp/tools/build_host.py:
#
#   cmake -G Ninja -S aosp/host -B out/host/<arch> \
#     -DCMAKE_TOOLCHAIN_FILE=aosp/cmake/toolchain-llvm-mingw.cmake \
#     -DANDRUNA_ARCH=<host arch> -DANDRUNA_AOSP_DIR=third_party/aosp \
#     -DANDRUNA_BISON=... -DANDRUNA_FLEX=...
#
# Unlike aosp/CMakeLists.txt (AOSP code with Android semantics), these are
# the Windows variants Soong itself builds for these modules
# (`target: { windows: { enabled: true } }`): libbase, liblog and aidl
# compile for the mingw target with their own Windows code paths.
#
# Tools:
#   aidl.exe   system/tools/aidl (cc_binary_host "aidl")

cmake_minimum_required(VERSION 3.28)
project(andruna_host LANGUAGES C CXX)

set(ANDRUNA_AOSP_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../third_party/aosp"
    CACHE PATH "Directory with the fetched AOSP projects")
get_filename_component(AOSP "${ANDRUNA_AOSP_DIR}" ABSOLUTE)
set(ANDRUNA_BISON "" CACHE FILEPATH "bison (build tool, GPL-3.0 with the Bison exception)")
set(ANDRUNA_FLEX "" CACHE FILEPATH "flex (build tool, BSD-2-Clause)")
# The platform SDK level the tools report (ADR 0019: 37 at android-17.0.0_r1).
set(ANDRUNA_PLATFORM_SDK_VERSION 37)

set(CMAKE_C_STANDARD 17)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_EXTENSIONS ON)
set(CMAKE_CXX_SCAN_FOR_MODULES OFF)
# Self-contained executables: no libc++ or libunwind DLL next to them.
add_link_options(-static)
# The toolchain file restricts try_compile to static libraries (for the
# guest build); host programs link normally.
set(CMAKE_TRY_COMPILE_TARGET_TYPE EXECUTABLE)

add_library(host_defaults INTERFACE)
target_compile_options(host_defaults INTERFACE -w -O2 -g0)
target_compile_definitions(host_defaults INTERFACE
  # libbase_cflags_defaults, windows target.
  _POSIX_THREAD_SAFE_FUNCTIONS
  # Soong's host defaults for mingw.
  _FILE_OFFSET_BITS=64 _GNU_SOURCE WIN32_LEAN_AND_MEAN NOMINMAX)

function(host_sources target dir)
  set(list)
  foreach(s ${ARGN})
    list(APPEND list "${dir}/${s}")
  endforeach()
  target_sources(${target} PRIVATE ${list})
endfunction()

# --- fmtlib ----------------------------------------------------------------
add_library(host_fmtlib STATIC
  "${AOSP}/external/fmtlib/src/format.cc")
target_include_directories(host_fmtlib PUBLIC "${AOSP}/external/fmtlib/include")
target_link_libraries(host_fmtlib PRIVATE host_defaults)

# --- liblog (liblog_sources; the Windows variant has no target sources) ------
set(_d "${AOSP}/system/logging/liblog")
add_library(host_liblog STATIC)
host_sources(host_liblog "${_d}"
  log_event_list.cpp log_event_write.cpp logger_name.cpp logger_read.cpp
  logger_write.cpp logprint.cpp properties.cpp)
target_include_directories(host_liblog PUBLIC "${_d}/include")
target_include_directories(host_liblog PRIVATE
  "${AOSP}/system/libbase/include" "${AOSP}/system/core/libcutils/include"
  "${AOSP}/external/fmtlib/include")
target_compile_definitions(host_liblog PRIVATE
  LIBLOG_LOG_TAG=1006 SNET_EVENT_LOG_TAG=1397638484 ANDROID_DEBUGGABLE=0)
target_link_libraries(host_liblog PRIVATE host_defaults)

# --- libbase (libbase_defaults, windows target) ------------------------------
set(_d "${AOSP}/system/libbase")
add_library(host_libbase STATIC)
host_sources(host_libbase "${_d}"
  chrono_utils.cpp file.cpp hex.cpp logging.cpp mapped_file.cpp parsebool.cpp
  parsenetaddress.cpp posix_strerror_r.cpp process.cpp properties.cpp
  result.cpp stringprintf.cpp strings.cpp threads.cpp test_utils.cpp
  errors_windows.cpp utf8.cpp)
target_include_directories(host_libbase PUBLIC "${_d}/include")
target_link_libraries(host_libbase PUBLIC host_fmtlib host_liblog)
target_link_libraries(host_libbase PRIVATE host_defaults)
set_target_properties(host_libbase PROPERTIES CXX_STANDARD 23)

# --- aidl ------------------------------------------------------------------
include(aidl.cmake)
