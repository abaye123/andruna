# external/lz4/lib: liblz4 (BSD-2-Clause; the GPL programs/ directory of the
# same repository is not built).
set(_d "${AOSP}/external/lz4/lib")

add_library(liblz4 STATIC)
aosp_sources(liblz4 "${_d}" lz4.c lz4hc.c lz4frame.c xxhash.c)
set_target_properties(liblz4 PROPERTIES OUTPUT_NAME lz4)
target_include_directories(liblz4 PUBLIC "${_d}")
target_link_libraries(liblz4 PUBLIC aosp_android)
