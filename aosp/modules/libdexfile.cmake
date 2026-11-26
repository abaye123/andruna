# art/libdexfile: libdexfile (Android target sources, static variant).
set(_d "${AOSP}/art/libdexfile")

add_library(libdexfile STATIC)
aosp_sources(libdexfile "${_d}"
  dex/art_dex_file_loader.cc
  dex/descriptors_names.cc
  dex/dex_file.cc
  dex/dex_file_exception_helpers.cc
  dex/dex_file_loader.cc
  dex/dex_file_tracking_registrar.cc
  dex/dex_file_verifier.cc
  dex/dex_instruction.cc
  dex/modifiers.cc
  dex/primitive.cc
  dex/signature.cc
  dex/standard_dex_file.cc
  dex/type_lookup_table.cc
  dex/utf.cc
  # Android target
  external/dex_file_ext.cc)
art_operator_out(libdexfile art/libdexfile
  art/libdexfile/dex/dex_file.h
  art/libdexfile/dex/dex_instruction.h
  art/libdexfile/dex/dex_instruction_utils.h
  art/libdexfile/dex/invoke_type.h)
target_compile_definitions(libdexfile PRIVATE STATIC_LIB)
target_include_directories(libdexfile PUBLIC
  "${_d}"
  "${_d}/external/include"
  "${AOSP}/libnativehelper/include_jni")
target_link_libraries(libdexfile PUBLIC
  aosp_android art_defaults libartbase libartpalette libziparchive libz
  libbase liblog)
set_target_properties(libdexfile PROPERTIES OUTPUT_NAME dexfile)
