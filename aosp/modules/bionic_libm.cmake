# libm.so (bionic/libm/Android.bp: libm, with the math routines of
# arm-optimized-routines). Included by bionic.cmake.

bp_object(bp_libm_shared libm_shared)
# Generic C routines (exp, log, pow, sin/cos float) used on every
# architecture, as Android's libm does.
bp_object(bp_libarm_optimized_routines_math libarm_optimized_routines_math)

bp_whole_objects(_libm_objs libm_shared)
list(REMOVE_DUPLICATES _libm_objs)
add_library(m_so SHARED ${_libm_objs})
set_target_properties(m_so PROPERTIES OUTPUT_NAME m LINKER_LANGUAGE C
  LINK_DEPENDS "${_libm_map}")
add_dependencies(m_so bionic_version_scripts)
target_link_options(m_so PRIVATE
  -Wl,-soname,libm.so
  "-Wl,--version-script,${_libm_map}"
  -Wl,--no-undefined)
target_link_libraries(m_so PRIVATE c_so "${ANDRUNA_ELF_BUILTINS}")
