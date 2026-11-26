# libdl.so (bionic/libdl/Android.bp: libdl with libdl_static) and the
# objects of libdl_android, which libc.so links statically. Included by
# bionic.cmake.
#
# libdl's functions call the loader's __loader_* functions (weak
# references), which andruna-linker provides as host functions.

bp_object(bp_libdl_static libdl_static)
bp_object(bp_libdl_android_shared libdl_android_shared)

bp_whole_objects(_libdl_objs libdl_shared)
list(REMOVE_DUPLICATES _libdl_objs)
add_library(dl_so SHARED ${_libdl_objs})
set_target_properties(dl_so PROPERTIES OUTPUT_NAME dl LINKER_LANGUAGE C
  LINK_DEPENDS "${_libdl_map}")
add_dependencies(dl_so bionic_version_scripts)
target_link_options(dl_so PRIVATE
  -Wl,-soname,libdl.so
  "-Wl,--version-script,${_libdl_map}"
  -Wl,--no-undefined)
target_link_libraries(dl_so PRIVATE ld_android_so andruna_host "${ANDRUNA_ELF_BUILTINS}")
