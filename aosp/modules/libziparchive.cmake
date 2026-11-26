# system/libziparchive: libziparchive.
set(_d "${AOSP}/system/libziparchive")

add_library(libziparchive STATIC)
aosp_sources(libziparchive "${_d}"
  zip_archive.cc
  zip_archive_stream_entry.cc
  zip_cd_entry_map.cc
  zip_error.cpp
  zip_writer.cc)
target_compile_definitions(libziparchive PRIVATE ZLIB_CONST)
set_target_properties(libziparchive PROPERTIES OUTPUT_NAME ziparchive)
target_include_directories(libziparchive PUBLIC
  "${_d}/include"
  "${_d}/incfs_support/include"
  "${AOSP}/external/googletest/googletest/include")
target_link_libraries(libziparchive PUBLIC aosp_android libbase liblog libz)
