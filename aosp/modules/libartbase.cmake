# art/libartbase: libartbase (Android target sources), with the ART-wide
# defaults (art/build/Android.bp "art_defaults", art/build/art.go) and the
# small libraries it pulls in.

# --- art_defaults ---------------------------------------------------------
add_library(art_defaults INTERFACE)
target_compile_options(art_defaults INTERFACE
  "$<$<COMPILE_LANGUAGE:CXX>:SHELL:-include andruna/libcxx_compat.h>"
  $<$<COMPILE_LANGUAGE:CXX>:-fno-rtti>
  -fstrict-aliasing
  -O3)
target_compile_definitions(art_defaults INTERFACE
  _LIBCPP_ENABLE_THREAD_SAFETY_ANNOTATIONS
  # art.go, device build defaults
  ART_TARGET ART_TARGET_ANDROID
  ART_DEFAULT_GC_TYPE_IS_CMC
  ART_USE_GENERATIONAL_GC=1
  ART_USE_TLAB=1
  ART_STACK_OVERFLOW_GAP_arm=8192
  ART_STACK_OVERFLOW_GAP_arm64=8192
  ART_STACK_OVERFLOW_GAP_riscv64=8192
  ART_STACK_OVERFLOW_GAP_x86=8192
  ART_STACK_OVERFLOW_GAP_x86_64=8192
  ART_PAGE_SIZE_AGNOSTIC=1
  USE_D8_DESUGAR=1
  ART_FRAME_SIZE_LIMIT=1744
  ART_BASE_ADDRESS=0x70000000
  ART_BASE_ADDRESS_MIN_DELTA=-0x1000000
  ART_BASE_ADDRESS_MAX_DELTA=0x1000000)

# --- aconfig flag libraries -------------------------------------------------
# Android generates these with the aconfig tool. aosp/tools/aconfig_cc.py
# writes the same C and C++ API from the .aconfig declarations, with every
# flag at its declared default (disabled) and read-only.
function(aconfig_cc_library target aconfig_file)
  get_filename_component(stem "${aconfig_file}" NAME_WE)
  set(outdir "${CMAKE_CURRENT_BINARY_DIR}/gen/${target}")
  set(stamp "${outdir}/${target}.stamp")
  add_custom_command(
    OUTPUT "${stamp}"
    COMMAND "${Python3_EXECUTABLE}" "${CMAKE_CURRENT_SOURCE_DIR}/tools/aconfig_cc.py"
            "${aconfig_file}" "${outdir}"
    COMMAND "${CMAKE_COMMAND}" -E touch "${stamp}"
    DEPENDS "${aconfig_file}" "${CMAKE_CURRENT_SOURCE_DIR}/tools/aconfig_cc.py"
    COMMENT "Generating aconfig flags for ${target}"
    VERBATIM)
  add_custom_target(${target}_gen DEPENDS "${stamp}")
  add_library(${target} INTERFACE)
  add_dependencies(${target} ${target}_gen)
  target_include_directories(${target} INTERFACE "${outdir}")
endfunction()

aconfig_cc_library(art_aconfig_flags_lib "${AOSP}/art/build/flags/art-flags.aconfig")
aconfig_cc_library(art_aconfig_rw_flags_lib "${AOSP}/art/build/flags/art-rw-flags.aconfig")
add_library(art_aconfig_flags INTERFACE)
target_link_libraries(art_aconfig_flags INTERFACE
  art_aconfig_flags_lib art_aconfig_rw_flags_lib)

# --- frameworks/libs/modules-utils: libmodules-utils-build (headers only) ---
add_library(libmodules_utils_build INTERFACE)
target_include_directories(libmodules_utils_build INTERFACE
  "${AOSP}/frameworks/libs/modules-utils/build/include")

# --- external/tinyxml2: libtinyxml2 (zlib license) --------------------------
add_library(libtinyxml2 STATIC)
aosp_sources(libtinyxml2 "${AOSP}/external/tinyxml2" tinyxml2.cpp)
target_compile_definitions(libtinyxml2 PRIVATE DEBUG ANDROID_NDK)
set_target_properties(libtinyxml2 PROPERTIES OUTPUT_NAME tinyxml2)
target_include_directories(libtinyxml2 PUBLIC "${AOSP}/external/tinyxml2")
target_link_libraries(libtinyxml2 PUBLIC aosp_android liblog)

# --- libartbase ---------------------------------------------------------------
set(_d "${AOSP}/art/libartbase")

add_library(libartbase STATIC)
aosp_sources(libartbase "${_d}"
  arch/instruction_set.cc
  base/allocator.cc
  base/arena_allocator.cc
  base/arena_bit_vector.cc
  base/bit_vector.cc
  base/compiler_filter.cc
  base/file_magic.cc
  base/file_utils.cc
  base/flags.cc
  base/hex_dump.cc
  base/logging.cc
  base/calloc_arena_pool.cc
  base/membarrier.cc
  base/memfd.cc
  base/memory_region.cc
  base/mem_map.cc
  base/metrics/metrics_common.cc
  base/os_linux.cc
  base/pointer_size.cc
  base/runtime_debug.cc
  base/scoped_arena_allocator.cc
  base/scoped_flock.cc
  base/socket_peer_is_trusted.cc
  base/time_utils.cc
  base/unix_file/fd_file.cc
  base/unix_file/random_access_file_utils.cc
  base/utils.cc
  base/zip_archive.cc
  # Android target
  base/globals_unix.cc
  base/mem_map_unix.cc)
art_operator_out(libartbase art/libartbase
  art/libartbase/arch/instruction_set.h
  art/libartbase/base/allocator.h
  art/libartbase/base/unix_file/fd_file.h)
target_compile_definitions(libartbase PRIVATE ART_STATIC_LIBARTBASE)
target_include_directories(libartbase PUBLIC "${_d}")
target_link_libraries(libartbase PUBLIC
  aosp_android art_defaults art_aconfig_flags libmodules_utils_build
  libbase liblog libartpalette libziparchive libz libtinyxml2)
set_target_properties(libartbase PROPERTIES OUTPUT_NAME artbase)
