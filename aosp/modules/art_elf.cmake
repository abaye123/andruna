# ART pieces that only the Android ELF build (ADR 0013) compiles: generated
# headers, the x86_64 assembly entry points and nterp, and the small
# libraries libart links on Android. Included after libart.cmake when both
# ANDRUNA_AOSP_ELF and ANDRUNA_AOSP_RUNTIME are set (spike S1 round 3).

set(_art "${AOSP}/art")
set(_gen "${CMAKE_CURRENT_BINARY_DIR}/gen/art")
file(MAKE_DIRECTORY "${_gen}")

# ART's Andruna patches (patches/art) are gated on ANDRUNA. It is defined
# for ART targets only: bionic's headers in the guest sysroot are used
# unchanged.
target_compile_definitions(art_defaults INTERFACE ANDRUNA=1)

# --- Headers Soong generates and Andruna writes by hand ----------------------
# aosp/overlay/art-gen: cache-info and apex-info readers (the XSD-generated
# parsers on Android), and the heapprofd client API declarations.
add_library(art_gen_headers INTERFACE)
target_include_directories(art_gen_headers INTERFACE
  "${CMAKE_CURRENT_SOURCE_DIR}/overlay/art-gen/include" "${_gen}")

# --- cpp-define-generator: asm_defines.h -------------------------------------
add_library(art_asm_defines_s OBJECT "${_art}/tools/cpp-define-generator/asm_defines.cc")
target_compile_options(art_asm_defines_s PRIVATE -S -Wno-unused-command-line-argument)
target_include_directories(art_asm_defines_s PRIVATE
  "${_art}/runtime" "${_art}/libartbase" "${_art}/libdexfile"
  "${AOSP}/libnativehelper/include_jni" "${AOSP}/external/dlmalloc"
  "${AOSP}/bionic/libc/platform" "${_art}/compiler/export")
target_link_libraries(art_asm_defines_s PRIVATE
  aosp_android art_defaults art_aconfig_flags libartbase libdexfile libbase liblog
  art_gen_headers)
add_custom_command(
  OUTPUT "${_gen}/asm_defines.h"
  COMMAND "${Python3_EXECUTABLE}" "${_art}/tools/cpp-define-generator/make_header.py"
          "$<TARGET_OBJECTS:art_asm_defines_s>" > "${_gen}/asm_defines.h"
  DEPENDS art_asm_defines_s "${_art}/tools/cpp-define-generator/make_header.py"
  COMMENT "Generating asm_defines.h"
  VERBATIM)
add_custom_target(art_asm_defines DEPENDS "${_gen}/asm_defines.h")

# --- nterp (generated from interpreter/mterp/x86_64ng) ------------------------
if(ANDRUNA_ARCH STREQUAL "x64")
  set(_mterp_dir "${_art}/runtime/interpreter/mterp/x86_64ng")
  set(_mterp_out "${_gen}/mterp_x86_64ng.S")
else()
  set(_mterp_dir "${_art}/runtime/interpreter/mterp/arm64ng")
  set(_mterp_out "${_gen}/mterp_arm64ng.S")
endif()
get_filename_component(_mterp_name "${_mterp_out}" NAME)
file(GLOB _mterp_in "${_mterp_dir}/*.S")
list(SORT _mterp_in)
add_custom_command(
  OUTPUT "${_mterp_out}"
  # The output name is relative: gen_mterp.py embeds it in Python source,
  # where a Windows path's backslashes would be escapes.
  COMMAND "${Python3_EXECUTABLE}" "${_art}/runtime/interpreter/mterp/gen_mterp.py"
          "${_mterp_name}" ${_mterp_in}
  WORKING_DIRECTORY "${_gen}"
  DEPENDS ${_mterp_in} "${_art}/runtime/interpreter/mterp/gen_mterp.py"
  COMMENT "Generating nterp"
  VERBATIM)

# --- libart_runtime: the parts the Windows-target round did not build --------
add_dependencies(libart_runtime art_asm_defines)
target_link_libraries(libart_runtime PUBLIC art_gen_headers)
target_include_directories(libart_runtime PUBLIC
  "${_art}/compiler/export" "${AOSP}/external/cpu_features/include" "${_art}/sigchainlib")
if(ANDRUNA_ARCH STREQUAL "x64")
  aosp_sources(libart_runtime "${_art}/runtime"
    arch/x86_64/memcmp16_x86_64.S
    arch/x86_64/native_entrypoints_x86_64.S
    arch/x86_64/jni_entrypoints_x86_64.S
    arch/x86_64/quick_entrypoints_x86_64.S)
else()
  aosp_sources(libart_runtime "${_art}/runtime"
    arch/arm64/memcmp16_arm64.S
    arch/arm64/native_entrypoints_arm64.S
    arch/arm64/jni_entrypoints_arm64.S
    arch/arm64/quick_entrypoints_arm64.S)
endif()
target_sources(libart_runtime PRIVATE "${_mterp_out}")
# Android's statsd backend needs the generated stats atoms (statslog_art.h)
# and libstatssocket. Andruna has no statsd: the runtime uses the same
# "no backend" functions as ART's host build.
get_target_property(_srcs libart_runtime SOURCES)
list(FILTER _srcs EXCLUDE REGEX "metrics/statsd\\.cc$")
set_target_properties(libart_runtime PROPERTIES SOURCES "${_srcs}")
target_sources(libart_runtime PRIVATE
  "${CMAKE_CURRENT_SOURCE_DIR}/overlay/art-gen/src/statsd_none.cc"
  "${CMAKE_CURRENT_SOURCE_DIR}/overlay/art-gen/src/heapprofd_client_none.c"
  # odrefresh's statsd upload; the host variant reports nothing.
  "${_art}/odrefresh/odr_statslog_host.cc")
target_include_directories(libart_runtime PRIVATE "${_art}/odrefresh/include"
  "${_art}/tools/cpp-define-generator")
# libcore's aconfig flags (scoped_thread_priority_change.cc).
aconfig_cc_library(libcore_aconfig_flags_lib "${AOSP}/libcore/libcore.aconfig")
target_link_libraries(libart_runtime PUBLIC libcore_aconfig_flags_lib)

# --- external/cpu_features: libcpu_features ----------------------------------
set(_d "${AOSP}/external/cpu_features")
add_library(libcpu_features STATIC)
aosp_sources(libcpu_features "${_d}"
  src/filesystem.c src/stack_line_reader.c src/string_view.c)
if(ANDRUNA_ARCH STREQUAL "x64")
  aosp_sources(libcpu_features "${_d}" src/impl_x86_linux_or_android.c)
else()
  aosp_sources(libcpu_features "${_d}" src/impl_aarch64_linux_or_android.c src/hwcaps.c)
  target_compile_definitions(libcpu_features PRIVATE HAVE_DLFCN_H HAVE_STRONG_GETAUXVAL)
endif()
target_compile_definitions(libcpu_features PRIVATE STACK_LINE_READER_BUFFER_SIZE=1024)
target_include_directories(libcpu_features PUBLIC "${_d}/include")
target_include_directories(libcpu_features PRIVATE "${_d}/include/internal")
target_link_libraries(libcpu_features PRIVATE aosp_android)

# --- external/lzma: liblzma (public domain LZMA SDK, ADR 0006 amendment) ------
set(_d "${AOSP}/external/lzma/C")
add_library(liblzma STATIC)
aosp_sources(liblzma "${_d}"
  7zAlloc.c 7zArcIn.c 7zBuf2.c 7zBuf.c 7zCrc.c 7zCrcOpt.c 7zDec.c 7zFile.c
  7zStream.c Aes.c AesOpt.c Alloc.c Bcj2.c Bra86.c Bra.c BraIA64.c CpuArch.c
  Delta.c LzFind.c Lzma2Dec.c Lzma2Enc.c Lzma86Dec.c Lzma86Enc.c LzmaDec.c
  LzmaEnc.c LzmaLib.c Ppmd7.c Ppmd7Dec.c Ppmd7Enc.c Sha1.c Sha1Opt.c Sha256.c Sha256Opt.c
  Sort.c Xz.c XzCrc64.c XzCrc64Opt.c XzDec.c XzEnc.c XzIn.c)
target_compile_definitions(liblzma PRIVATE Z7_ST)
target_include_directories(liblzma PUBLIC "${_d}")
target_link_libraries(liblzma PRIVATE aosp_android)

# --- system/libprocinfo -------------------------------------------------------
add_library(libprocinfo STATIC "${AOSP}/system/libprocinfo/process.cpp")
target_include_directories(libprocinfo PUBLIC "${AOSP}/system/libprocinfo/include")
target_link_libraries(libprocinfo PUBLIC aosp_android libbase)

# --- art/libelffile, art/libprofile ------------------------------------------
add_library(libelffile STATIC)
aosp_sources(libelffile "${_art}/libelffile"
  elf/xz_utils.cc stream/buffered_output_stream.cc stream/file_output_stream.cc
  stream/output_stream.cc stream/vector_output_stream.cc)
target_include_directories(libelffile PUBLIC "${_art}/libelffile")
target_link_libraries(libelffile PUBLIC aosp_android art_defaults libartbase liblzma libbase)

add_library(libprofile STATIC)
aosp_sources(libprofile "${_art}/libprofile"
  profile/profile_boot_info.cc profile/profile_compilation_info.cc)
target_include_directories(libprofile PUBLIC "${_art}/libprofile")
target_link_libraries(libprofile PUBLIC aosp_android art_defaults libartbase libdexfile
  libartpalette libziparchive libz libbase)

# --- art/sigchainlib: libsigchain ---------------------------------------------
# libasync_safe (bionic/libc/async_safe) is compiled here as an ART-private
# copy; the guest bionic build owns the shared one.
add_library(art_async_safe STATIC "${AOSP}/bionic/libc/async_safe/async_safe_log.cpp")
target_include_directories(art_async_safe PUBLIC "${AOSP}/bionic/libc/async_safe/include")
target_include_directories(art_async_safe PRIVATE "${AOSP}/bionic/libc/platform" "${AOSP}/bionic/libc")
target_link_libraries(art_async_safe PRIVATE aosp_android liblog)

add_library(libsigchain_static STATIC "${_art}/sigchainlib/sigchain.cc")
target_include_directories(libsigchain_static PUBLIC "${_art}/sigchainlib")
target_include_directories(libsigchain_static PRIVATE
  "${AOSP}/bionic/libc/platform" "${AOSP}/system/unwinding/libunwindstack/include")
target_link_libraries(libsigchain_static PUBLIC aosp_android art_defaults art_async_safe libbase)

# --- art/libnativebridge, art/libnativeloader --------------------------------
add_library(libnativebridge STATIC "${_art}/libnativebridge/native_bridge.cc")
target_include_directories(libnativebridge PUBLIC "${_art}/libnativebridge/include")
target_link_libraries(libnativebridge PUBLIC aosp_android libbase liblog
  libnativehelper_headers)
# art_defaults stays private: its ART_TARGET_ANDROID would select
# libnativeloader's namespace code below.
target_link_libraries(libnativebridge PRIVATE art_defaults)
target_include_directories(libnativebridge PRIVATE "${AOSP}/bionic/libc/platform" "${AOSP}/bionic/libdl/include_private")

# The host variant of libnativeloader (plain dlopen of the library path, no
# linker namespaces): Andruna's linker has no namespaces yet.
add_library(libnativeloader STATIC "${_art}/libnativeloader/native_loader.cpp")
target_include_directories(libnativeloader PUBLIC "${_art}/libnativeloader/include")
target_link_libraries(libnativeloader PUBLIC aosp_android libbase liblog
  libnativebridge libnativehelper_headers libmodules_utils_build)
target_compile_options(libnativeloader PRIVATE
  "$<$<COMPILE_LANGUAGE:CXX>:SHELL:-include andruna/libcxx_compat.h>" -fno-rtti)
target_include_directories(libnativeloader PRIVATE "${_art}/libartbase" "${AOSP}/bionic/libc/platform")

# --- libnativehelper ----------------------------------------------------------
set(_d "${AOSP}/libnativehelper")
add_library(libnativehelper_headers INTERFACE)
target_include_directories(libnativehelper_headers INTERFACE
  "${_d}/include" "${_d}/include_jni" "${_d}/header_only_include"
  "${_d}/include_platform" "${_d}/include_platform_header_only")
add_library(libnativehelper STATIC)
aosp_sources(libnativehelper "${_d}"
  DlHelp.c ExpandableString.c JNIHelp.c JniInvocation.c
  JNIPlatformHelp.c JniConstants.c file_descriptor_jni.c)
target_link_libraries(libnativehelper PUBLIC aosp_android libnativehelper_headers liblog)

target_link_libraries(libart_runtime PUBLIC
  libcpu_features libelffile libprofile libnativebridge
  libnativeloader libnativehelper_headers libprocinfo liblzma)

# The optimizing compiler, linked into libart.so as on Android.
include(${CMAKE_CURRENT_LIST_DIR}/libart_compiler.cmake)

# dex2oat's compiler driver, linker and image writer (spike S1 round 4).
include(${CMAKE_CURRENT_LIST_DIR}/dex2oat.cmake)
