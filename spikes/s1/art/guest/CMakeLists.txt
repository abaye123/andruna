# Spike S1 round 3 guest build: ART and the libraries it needs as Android
# ELF shared objects, and dalvikvm as an Android PIE. Driven by build.py
# (step `aosp`). Derived from the S8 guest build.
#
# The AOSP modules are those of aosp/CMakeLists.txt with
# ANDRUNA_AOSP_RUNTIME=ON, compiled with aosp/cmake/android-elf.cmake.
# Library split as on Android where it matters at run time (one liblog,
# libbase, libnativehelper, libsigchain in front of libc, libart);
# ART's own static parts are folded into libart.so.

cmake_minimum_required(VERSION 3.28)
project(andruna_s1_guest LANGUAGES C CXX ASM)

get_filename_component(ANDRUNA_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../../.." ABSOLUTE)
set(ANDRUNA_AOSP_DIR "${ANDRUNA_ROOT}/third_party/aosp" CACHE PATH "AOSP sources")
get_filename_component(AOSP "${ANDRUNA_AOSP_DIR}" ABSOLUTE)

set(CMAKE_C_STANDARD 17)
set(CMAKE_C_EXTENSIONS ON)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_EXTENSIONS ON)
set(CMAKE_CXX_SCAN_FOR_MODULES OFF)

add_subdirectory("${ANDRUNA_ROOT}/aosp" aosp EXCLUDE_FROM_ALL)

set(SYSLIB "${CMAKE_SYSROOT}/usr/lib")
set(GUEST_LINK
  "${SYSLIB}/crtbegin_so.o"
  "-L${SYSLIB}" "-L${ANDRUNA_LIBCXX_DIR}/lib"
  -lc++ -lc -landruna_host
  "${ANDRUNA_ELF_BUILTINS}")

function(guest_so target soname)
  add_library(${target} SHARED "${CMAKE_CURRENT_SOURCE_DIR}/empty.c")
  set(wl)
  foreach(a ${ARGN})
    list(APPEND wl "$<LINK_LIBRARY:WHOLE_ARCHIVE,${a}>")
  endforeach()
  target_link_libraries(${target} PRIVATE ${wl})
  string(REGEX REPLACE "^lib(.*)\\.so$" "\\1" stem "${soname}")
  set_target_properties(${target} PROPERTIES OUTPUT_NAME "${stem}")
  target_link_options(${target} PRIVATE "-Wl,-soname,${soname}")
endfunction()

guest_so(log_so liblog.so liblog)
target_link_libraries(log_so PRIVATE ${GUEST_LINK})

guest_so(base_so libbase.so libbase fmtlib)
target_link_libraries(base_so PRIVATE log_so ${GUEST_LINK})

guest_so(nativehelper_so libnativehelper.so libnativehelper)
target_link_libraries(nativehelper_so PRIVATE log_so ${GUEST_LINK})

guest_so(sigchain_so libsigchain.so libsigchain_static art_async_safe)
target_link_libraries(sigchain_so PRIVATE base_so log_so ${GUEST_LINK})

# libart.so: the runtime with libartbase, libdexfile, libprofile and the
# smaller libraries folded in.
guest_so(art_so libart.so
  libart_runtime libart_compiler libartbase libdexfile libprofile libelffile libartpalette
  libnativebridge libnativeloader libcpu_features libprocinfo liblzma)
target_link_libraries(art_so PRIVATE
  libziparchive libtinyxml2 liblz4 libz libcutils libutils
  sigchain_so nativehelper_so base_so log_so ${GUEST_LINK})

# dalvikvm (art/dalvikvm), an Android PIE that finds libart.so through
# libnativehelper's JniInvocation.
add_executable(dalvikvm "${AOSP}/art/dalvikvm/dalvikvm.cc")
target_include_directories(dalvikvm PRIVATE "${AOSP}/art/libartbase" "${AOSP}/bionic/libc/platform")
target_include_directories(dalvikvm PRIVATE $<TARGET_PROPERTY:libbase,INTERFACE_INCLUDE_DIRECTORIES>)
target_link_libraries(dalvikvm PRIVATE aosp_android art_defaults libnativehelper_headers
  nativehelper_so base_so log_so ${GUEST_LINK})
# dl* and the namespace calls are bound by the loader at run time.
target_link_options(dalvikvm PRIVATE -Wl,--export-dynamic-symbol=main
  -Wl,--unresolved-symbols=ignore-all)

# dex2oat (art/dex2oat), an Android PIE linked against libart.so (which
# holds the runtime, the compiler, libprofile and libelffile) and
# libart-dex2oat. The archive is named by file, not as a target, so that
# its static dependencies already in libart.so are not linked again.
add_executable(dex2oat $<TARGET_OBJECTS:dex2oat_main>)
add_dependencies(dex2oat dex2oat_main libart_dex2oat)
target_link_libraries(dex2oat PRIVATE
  -Wl,--whole-archive "$<TARGET_FILE:libart_dex2oat>" -Wl,--no-whole-archive
  art_so sigchain_so nativehelper_so base_so log_so ${GUEST_LINK})
# Functions the guest libc shim lacks (getppid, fmod) are bound by the
# loader at run time, as for dalvikvm.
target_link_options(dex2oat PRIVATE -Wl,--export-dynamic-symbol=main
  -Wl,--unresolved-symbols=ignore-all)

include(natives.cmake)
