# Compilation environment for AOSP code built as Android ELF (spike S8),
# the counterpart of android.cmake for toolchain-android-elf.cmake.
#
# The target is a real Android triple, so clang already defines
# __ANDROID__, __linux__, __LP64__ and the Android ABI (LP64, 32-bit
# wchar_t, System V on x64, AAPCS64 on arm64). Bionic's headers come from
# the sysroot unchanged: none of the data-model work of android.cmake is
# needed, and ANDRUNA is not defined, so the bionic patches stay inactive.
#
# Every AOSP target links the interface library `aosp_android`.

if(NOT ANDRUNA_AOSP_ELF)
  message(FATAL_ERROR "configure with -DCMAKE_TOOLCHAIN_FILE=aosp/cmake/toolchain-android-elf.cmake")
endif()

set(ANDRUNA_LIBCXX_DIR "" CACHE PATH
  "Install directory of a libc++ built for the guest (spikes/s8/guest/build.py)")
if(NOT EXISTS "${ANDRUNA_LIBCXX_DIR}/include/c++/v1/__config_site")
  message(FATAL_ERROR "libc++ for the guest not found in '${ANDRUNA_LIBCXX_DIR}'")
endif()

add_library(aosp_android INTERFACE)

target_compile_options(aosp_android INTERFACE
  # libc++ built for the guest, not llvm-mingw's Windows libc++.
  $<$<COMPILE_LANGUAGE:CXX>:-nostdinc++>
  "$<$<COMPILE_LANGUAGE:CXX>:SHELL:-isystem ${ANDRUNA_LIBCXX_DIR}/include/c++/v1>"
  # Android platform defaults.
  -fno-exceptions
  -funwind-tables
  -ffunction-sections
  -fdata-sections
  -fno-strict-aliasing
  -fPIC
  -fdebug-prefix-map=${AOSP}=aosp
  # DWARF, as Android: the debuggers that read it are discussed in S8.md.
  -g
  -O2
)

option(ANDRUNA_AOSP_WARNINGS "Report format, narrowing and pointer-size warnings" OFF)
if(ANDRUNA_AOSP_WARNINGS)
  target_compile_options(aosp_android INTERFACE
    -Wno-everything -Wformat -Wshorten-64-to-32)
else()
  target_compile_options(aosp_android INTERFACE -w)
endif()

target_compile_definitions(aosp_android INTERFACE
  ANDROID=1
  # A platform build: every bionic API is available.
  __ANDROID_MIN_SDK_VERSION__=10000
  _GNU_SOURCE=1
)

# Same helpers as android.cmake, so the module files work unchanged.
function(art_operator_out target local_path)
  set(out "${CMAKE_CURRENT_BINARY_DIR}/gen/${target}_operator_out.cc")
  set(abs)
  foreach(h ${ARGN})
    list(APPEND abs "${AOSP}/${h}")
  endforeach()
  add_custom_command(
    OUTPUT "${out}"
    COMMAND "${Python3_EXECUTABLE}" "${AOSP}/art/tools/generate_operator_out.py"
            "${local_path}" ${ARGN} > "${out}"
    WORKING_DIRECTORY "${AOSP}"
    DEPENDS ${abs} "${AOSP}/art/tools/generate_operator_out.py"
    COMMENT "Generating operator<< for ${target}"
    VERBATIM)
  target_sources(${target} PRIVATE "${out}")
endfunction()

function(aosp_sources target dir)
  set(list)
  foreach(s ${ARGN})
    list(APPEND list "${dir}/${s}")
  endforeach()
  target_sources(${target} PRIVATE ${list})
endfunction()
