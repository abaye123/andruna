# external/zlib: libz (Chromium zlib with SIMD paths, as on Android).
set(_d "${AOSP}/external/zlib")

add_library(libz STATIC)
aosp_sources(libz "${_d}"
  adler32.c adler32_simd.c compress.c cpu_features.c crc32.c crc32_simd.c
  crc_folding.c deflate.c gzclose.c gzlib.c gzread.c gzwrite.c infback.c
  inffast.c inflate.c inftrees.c trees.c uncompr.c zutil.c)
target_compile_definitions(libz PRIVATE
  HAVE_HIDDEN ZLIB_CONST CHROMIUM_ZLIB_NO_CASTAGNOLI
  DEFLATE_CHUNK_WRITE_64LE INFLATE_CHUNK_READ_64LE)
if(ANDRUNA_ARCH STREQUAL "x64")
  target_compile_definitions(libz PRIVATE
    ADLER32_SIMD_SSSE3 CRC32_SIMD_SSE42_PCLMUL X86_NOT_WINDOWS)
else()
  target_compile_definitions(libz PRIVATE
    ADLER32_SIMD_NEON CRC32_ARMV8_CRC32 ARMV8_OS_LINUX)
endif()
target_compile_options(libz PRIVATE -O3)
set_target_properties(libz PROPERTIES OUTPUT_NAME z)
target_include_directories(libz PUBLIC "${_d}")
target_link_libraries(libz PUBLIC aosp_android)
