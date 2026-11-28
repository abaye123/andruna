# The libraries Skia links on Android (skia_deps in external/skia/Android.bp),
# as static libraries for the guest build (ANDRUNA_AOSP_GRAPHICS):
#
#   libft2                      external/freetype       FreeType (FTL)
#   libharfbuzz_ng              external/harfbuzz_ng    HarfBuzz (MIT), ICU
#                                                       Unicode functions
#   libharfbuzz_subset          external/harfbuzz_ng    font subsetting for PDF
#   libpng                      external/libpng         (libpng license)
#   libjpeg                     external/libjpeg-turbo  (IJG, BSD-3-Clause)
#   libwebp-decode/-encode      external/webp           (BSD-3-Clause)
#   libwuffs_mirror_release_c   external/wuffs-mirror-release-c (Apache-2.0)
#
# Sources and flags come from each project's Android.bp (blueprint.py);
# Andruna's changes are listed where the targets are made.
#
# Not built (Skia features turned off in aosp/overlay/skia): libdng_sdk and
# libpiex (RAW), libcrabbyavif_ffi (AVIF, Rust), libskrifa (Fontations,
# Rust), libperfetto_client_experimental, libvulkan.

include("${CMAKE_CURRENT_LIST_DIR}/../cmake/blueprint.cmake")
if(ANDRUNA_ARCH STREQUAL "x64")
  set(BP_ARCH x86_64)
else()
  set(BP_ARCH arm64)
endif()

bp_read(skia_deps ARCH ${BP_ARCH} MODULES
  external/freetype/Android.bp:libft2
  external/harfbuzz_ng/Android.bp:libharfbuzz_ng
  external/harfbuzz_ng/Android.bp:libharfbuzz_subset
  external/libpng/Android.bp:libpng
  external/libjpeg-turbo/Android.bp:libjpeg
  external/webp/Android.bp:libwebp-decode
  external/webp/Android.bp:libwebp-decode-avx2
  external/webp/Android.bp:libwebp-decode-sse41
  external/webp/Android.bp:libwebp-encode
  external/webp/Android.bp:libwebp-encode-avx2
  external/webp/Android.bp:libwebp-encode-sse41
  external/wuffs-mirror-release-c/Android.bp:libwuffs_mirror_release_c)

# Module flags come after aosp_android's; -w and no -Werror as elsewhere.
set(ANDRUNA_BP_BASE_OPTIONS)
set(ANDRUNA_BP_LATE_OPTIONS -w -Wno-error)

# A static library from one Blueprint module (bp_object + an archive).
function(skia_dep target var)
  bp_object(${target}_obj ${var} ${ARGN})
  target_link_libraries(${target}_obj PUBLIC aosp_android)
  add_library(${target} STATIC $<TARGET_OBJECTS:${target}_obj>)
  target_link_libraries(${target} PUBLIC aosp_android)
endfunction()

set(_ext "${AOSP}/external")

# libicuuc for HarfBuzz's ICU Unicode functions (HAVE_ICU_BUILTIN): the same
# module ART's stage builds.
if(NOT TARGET libicuuc)
  include("${CMAKE_CURRENT_LIST_DIR}/icu.cmake")
endif()
# libexpat for Skia's XML support (SK_XML) and the Android font manager.
if(NOT TARGET libexpat)
  include("${CMAKE_CURRENT_LIST_DIR}/libexpat.cmake")
endif()

# --- libft2 -----------------------------------------------------------------
skia_dep(libft2 libft2)
target_include_directories(libft2_obj PRIVATE "${_ext}/libpng" "${_ext}/zlib")
target_include_directories(libft2 PUBLIC "${_ext}/freetype/include")
target_link_libraries(libft2 PUBLIC libpng libz)

# --- libpng -----------------------------------------------------------------
# libpng's Android.bp keeps the Intel SSE filters for the device build
# (PNG_INTEL_SSE_OPT=1 for android_x86_64), which the target blocks give.
skia_dep(libpng libpng)
target_include_directories(libpng_obj PRIVATE "${_ext}/zlib")
target_include_directories(libpng PUBLIC "${_ext}/libpng")
target_link_libraries(libpng PUBLIC libz)

# --- libjpeg ----------------------------------------------------------------
# On x86_64 Android assembles the SIMD routines with NASM, which the guest
# toolchain does not have: x64 uses jsimd_none.c (portable C) instead, as
# Android's riscv64 build does. arm64 keeps the NEON intrinsics.
if(BP_ARCH STREQUAL "x86_64")
  set(_jpeg_excl)
  foreach(s ${BP_libjpeg_SRCS})
    if(s MATCHES "/simd/")
      get_filename_component(n "${s}" NAME)
      list(APPEND _jpeg_excl "simd/x86_64/${n}")
    endif()
  endforeach()
  skia_dep(libjpeg libjpeg EXCLUDE ${_jpeg_excl}
    SOURCES "${_ext}/libjpeg-turbo/jsimd_none.c")
  target_compile_options(libjpeg_obj PRIVATE -UWITH_SIMD)
else()
  skia_dep(libjpeg libjpeg)
endif()
target_include_directories(libjpeg PUBLIC "${_ext}/libjpeg-turbo")

# --- libwebp ----------------------------------------------------------------
skia_dep(libwebp_decode libwebp_decode)
skia_dep(libwebp_encode libwebp_encode)
foreach(t libwebp_decode libwebp_encode)
  target_include_directories(${t}_obj PRIVATE "${_ext}/webp/src")
  target_include_directories(${t} PUBLIC "${_ext}/webp/src")
endforeach()
if(BP_ARCH STREQUAL "x86_64")
  foreach(v decode_avx2 decode_sse41 encode_avx2 encode_sse41)
    bp_object(libwebp_${v}_obj libwebp_${v})
    target_link_libraries(libwebp_${v}_obj PUBLIC aosp_android)
    target_include_directories(libwebp_${v}_obj PRIVATE "${_ext}/webp/src")
  endforeach()
  target_sources(libwebp_decode PRIVATE
    $<TARGET_OBJECTS:libwebp_decode_avx2_obj> $<TARGET_OBJECTS:libwebp_decode_sse41_obj>)
  target_sources(libwebp_encode PRIVATE
    $<TARGET_OBJECTS:libwebp_encode_avx2_obj> $<TARGET_OBJECTS:libwebp_encode_sse41_obj>)
endif()

# --- libwuffs_mirror_release_c ----------------------------------------------
skia_dep(libwuffs libwuffs_mirror_release_c)
target_include_directories(libwuffs PUBLIC "${_ext}/wuffs-mirror-release-c/release/c")

# --- libharfbuzz_ng, libharfbuzz_subset ---------------------------------------
skia_dep(libharfbuzz_ng libharfbuzz_ng)
target_link_libraries(libharfbuzz_ng_obj PRIVATE libicuuc_headers icu4c_defaults)
target_include_directories(libharfbuzz_ng PUBLIC "${_ext}/harfbuzz_ng/src")
target_link_libraries(libharfbuzz_ng PUBLIC libicuuc)
skia_dep(libharfbuzz_subset libharfbuzz_subset)
target_link_libraries(libharfbuzz_subset_obj PRIVATE libicuuc_headers icu4c_defaults)
target_link_libraries(libharfbuzz_subset PUBLIC libharfbuzz_ng)
