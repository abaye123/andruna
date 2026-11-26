# libc.so, the libc static archive for the bridge, ld-android.so and the
# crt objects (bionic/libc/Android.bp). Included by bionic.cmake.

set(_arch_dir "arch-${BP_ARCH}")

# Assembler files replaced by aosp/bionic/ (they execute system calls).
set(_replaced
  "libc/${_arch_dir}/bionic/syscall.S"
  "libc/${_arch_dir}/bionic/vfork.S"
  "libc/${_arch_dir}/bionic/__bionic_clone.S"
  "libc/${_arch_dir}/bionic/_exit_with_stack_teardown.S")

bp_object(bp_libc_bionic libc_bionic
  EXCLUDE ${_replaced}
  SOURCES
    "${ANDRUNA_BIONIC_DIR}/${_arch_dir}/syscall.S"
    "${ANDRUNA_BIONIC_DIR}/andruna_threads.cpp")
add_dependencies(bp_libc_bionic bionic_generated_headers)

foreach(m
    libc_bootstrap libc_init_static libc_init_dynamic libc_tzcode libc_dns
    libc_freebsd libc_freebsd_ldexp libc_freebsd_large_stack libc_netbsd
    libc_openbsd libc_openbsd_large_stack libc_gdtoa libc_fortify libc_aeabi
    libc_common libc_dynamic_dispatch libc_common_shared libc_unwind_static
    libc_nomalloc libasync_safe libsystemproperties libpropertyinfoparser
    llvmlibc libscudo gwp_asan)
  bp_object(bp_${m} ${m})
endforeach()
bp_object(bp_libstdcxx libstdcxx)
bp_object(bp_libc_syscalls libc_syscalls GENERATED "syscalls-${BP_ARCH}=${_syscalls}")
bp_object(bp_libarm_optimized_routines_string libarm_optimized_routines_string)
if(BP_ARCH STREQUAL "x86_64")
  bp_object(bp_libc_bionic_portable_simd_x86_64_v2 libc_bionic_portable_simd_x86_64_v2)
  bp_object(bp_libc_bionic_portable_simd_x86_64_v3 libc_bionic_portable_simd_x86_64_v3)
else()
  bp_object(bp_libc_bionic_portable_simd_arm64_neon_no_mte libc_bionic_portable_simd_arm64_neon_no_mte)
  bp_object(bp_libc_bionic_portable_simd_arm64_neon_mte libc_bionic_portable_simd_arm64_neon_mte)
endif()

# libc.so's own sources (libc_sources_shared, including its crt objects).
bp_object(bp_libc_shared libc_shared)

# LLVM's libunwind, linked into libc.so and exported (Android's
# libunwind-exported), so C++ exceptions work in every guest object.
set(_uw "${ANDRUNA_LLVM_PROJECT}/libunwind")
file(GLOB _uw_srcs "${_uw}/src/*.c" "${_uw}/src/*.cpp" "${_uw}/src/*.S")
add_library(bionic_libunwind OBJECT ${_uw_srcs})
target_include_directories(bionic_libunwind PRIVATE "${_uw}/include")
target_compile_options(bionic_libunwind PRIVATE
  ${ANDRUNA_BP_BASE_OPTIONS}
  -D_LIBUNWIND_IS_NATIVE_ONLY
  -fstrict-aliasing -fno-stack-protector
  $<$<COMPILE_LANGUAGE:CXX>:-std=c++17>
  "SHELL:-isystem ${BIONIC}/libc/include"
  "SHELL:-isystem ${BIONIC}/libc/kernel/uapi"
  "SHELL:-isystem ${BIONIC}/libc/kernel/android/uapi"
  "SHELL:-isystem ${BIONIC}/libc/kernel/uapi/asm-${BIONIC_ASM_ARCH}"
  ${ANDRUNA_BP_LATE_OPTIONS})

# All objects of libc.so: its own sources and the whole_static_libs closure.
bp_whole_objects(_libc_objs libc_shared)
list(REMOVE_DUPLICATES _libc_objs)

add_library(c_so SHARED ${_libc_objs} $<TARGET_OBJECTS:bionic_libunwind>
  $<TARGET_OBJECTS:bp_libdl_android_shared>)
set_target_properties(c_so PROPERTIES OUTPUT_NAME c LINKER_LANGUAGE C
  LINK_DEPENDS "${_libc_map}")
add_dependencies(c_so bionic_version_scripts)
target_link_options(c_so PRIVATE
  -Wl,-soname,libc.so
  "-Wl,--version-script,${_libc_map}"
  # Android does not pack libc.so's relocations (Android.bp).
  -Wl,--pack-dyn-relocs=relr
  -Wl,--no-undefined
  -Wl,--exclude-libs,libclang_rt.builtins.a)
target_link_libraries(c_so PRIVATE ld_android_so dl_so andruna_host "${ANDRUNA_ELF_BUILTINS}")

# The static parts of libc the bridge links, as Android's linker links
# libc_nomalloc (plus the dynamic dispatch of the string functions).
bp_whole_objects(_nomalloc_objs libc_nomalloc)
list(APPEND _nomalloc_objs $<TARGET_OBJECTS:bp_libc_dynamic_dispatch>)
list(REMOVE_DUPLICATES _nomalloc_objs)
add_library(bionic_libc_nomalloc STATIC ${_nomalloc_objs})

# ld-android.so: Andruna's bridge between andruna-linker and libc.
add_library(ld_android_so SHARED "${ANDRUNA_BIONIC_DIR}/ld_android_bridge.cpp")
target_include_directories(ld_android_so PRIVATE
  ${BP_libc_bionic_INCLUDES} ${BP_libc_bionic_HEADER_INCLUDES} "${BIONIC}/libc")
target_compile_options(ld_android_so PRIVATE
  ${ANDRUNA_BP_BASE_OPTIONS} ${BP_libc_bionic_CFLAGS}
  -fno-stack-protector -ffreestanding
  ${ANDRUNA_BP_LATE_OPTIONS})
set(_bridge_map "${ANDRUNA_BIONIC_DIR}/ld_android_bridge.map.txt")
set_target_properties(ld_android_so PROPERTIES OUTPUT_NAME ld-android PREFIX ""
  LINK_DEPENDS "${_bridge_map}")
target_link_options(ld_android_so PRIVATE
  -Wl,-soname,ld-android.so
  -Wl,--exclude-libs,ALL
  "-Wl,--version-script,${_bridge_map}"
  -Wl,--gc-sections
  -Wl,--no-undefined)
target_link_libraries(ld_android_so PRIVATE bionic_libc_nomalloc andruna_host "${ANDRUNA_ELF_BUILTINS}")

# crt objects: Android's cc_object modules, each merged with crtbrand.
bp_object(bp_crtbrand crtbrand)
foreach(o crtbegin_so crtend_so crtbegin_dynamic crtend_android)
  bp_object(bp_${o} ${o})
endforeach()
set(BIONIC_CRT_OBJECTS)
foreach(o crtbegin_so crtbegin_dynamic crtend_so crtend_android)
  set(_out "${CMAKE_BINARY_DIR}/crt/${o}.o")
  set(_in $<TARGET_OBJECTS:bp_${o}>)
  if(o MATCHES "^crtbegin")
    list(APPEND _in $<TARGET_OBJECTS:bp_crtbrand>)
  endif()
  add_custom_command(
    OUTPUT "${_out}"
    COMMAND "${CMAKE_COMMAND}" -E make_directory "${CMAKE_BINARY_DIR}/crt"
    COMMAND "${ANDRUNA_LLD}" -r -o "${_out}" ${_in}
    DEPENDS bp_${o} bp_crtbrand
    COMMAND_EXPAND_LISTS
    VERBATIM)
  list(APPEND BIONIC_CRT_OBJECTS "${_out}")
endforeach()
add_custom_target(bionic_crt ALL DEPENDS ${BIONIC_CRT_OBJECTS})
