# art/dex2oat: libart-dex2oat (compiler driver, oat and image writers) and
# the dex2oat binary's sources. Sources from art/dex2oat/Android.bp
# ("libart-dex2oat-defaults", "dex2oat-defaults"), relative patchers for the
# build architecture only. Android ELF build (spike S1 round 4); the
# executable is linked by the guest build against libart.so, as on Android.
#
# BoringSSL is not built: its only use here, the SHA-1 build ID of oat
# files, comes from aosp/overlay/dex2oat (LZMA SDK SHA-1).

set(_d "${AOSP}/art/dex2oat")
add_library(libart_dex2oat STATIC)
aosp_sources(libart_dex2oat "${_d}"
  aot_class_linker.cc
  dex/quick_compiler_callbacks.cc
  dex/verification_results.cc
  driver/compiled_method.cc
  driver/compiled_method_storage.cc
  driver/compiler_driver.cc
  interpreter/interpreter_switch_impl1.cc
  linker/code_info_table_deduper.cc
  linker/elf_writer_quick.cc
  linker/image_writer.cc
  linker/multi_oat_relative_patcher.cc
  linker/oat_writer.cc
  linker/relative_patcher.cc
  sdk_checker.cc
  transaction.cc
  utils/swap_space.cc)
if(ANDRUNA_ARCH STREQUAL "x64")
  aosp_sources(libart_dex2oat "${_d}"
    linker/x86/relative_patcher_x86.cc
    linker/x86/relative_patcher_x86_base.cc
    linker/x86_64/relative_patcher_x86_64.cc)
else()
  # The arm64 patcher derives from ArmBaseRelativePatcher, which Soong
  # compiles under the arm codegen (always present next to arm64 there).
  aosp_sources(libart_dex2oat "${_d}"
    linker/arm/relative_patcher_arm_base.cc
    linker/arm64/relative_patcher_arm64.cc)
endif()
art_operator_out(libart_dex2oat art/dex2oat art/dex2oat/linker/image_writer.h)
target_include_directories(libart_dex2oat PUBLIC "${_d}" "${AOSP}/art/cmdline")
target_include_directories(libart_dex2oat PRIVATE
  "${CMAKE_CURRENT_SOURCE_DIR}/overlay/dex2oat/include")
target_sources(libart_dex2oat PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/overlay/dex2oat/src/sha1.c")
# Only sha1.c sees the LZMA SDK's headers (its Compiler.h would shadow
# ART's compiler.h on a case-insensitive file system).
set_source_files_properties("${CMAKE_CURRENT_SOURCE_DIR}/overlay/dex2oat/src/sha1.c"
  PROPERTIES INCLUDE_DIRECTORIES "${AOSP}/external/lzma/C")
target_link_libraries(libart_dex2oat PUBLIC libart_compiler libelffile libprofile)

# The binary's own sources, as an object library the guest build links.
add_library(dex2oat_main OBJECT "${_d}/dex2oat_options.cc" "${_d}/dex2oat.cc")
target_link_libraries(dex2oat_main PUBLIC libart_dex2oat)
