# The guest libc shim of spike S8: libc.so (ELF) and crtbegin_so.o.
#
# Configure with aosp/cmake/toolchain-android-elf.cmake (see
# spikes/s8/guest/build.py). The symbols libc.so leaves undefined (files,
# memory, time, threads, allocation, math) are provided by the host loader
# and forwarded to andruna-lx where lx has them.

cmake_minimum_required(VERSION 3.28)
project(andruna_s8_libc LANGUAGES C)

# Link-time stub of the host library: one empty function per name of
# host_symbols.txt. At run time the loader binds these names to the host.
set(_host_list "${CMAKE_CURRENT_SOURCE_DIR}/host_symbols.txt")
set(_host_c "${CMAKE_CURRENT_BINARY_DIR}/host_stub.c")
file(STRINGS "${_host_list}" _names REGEX "^[A-Za-z_]")
set(_src "// Generated from host_symbols.txt; do not edit.\n")
foreach(n ${_names})
  string(APPEND _src "void ${n}(void) {}\n")
endforeach()
file(WRITE "${_host_c}" "${_src}")
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${_host_list}")
add_library(andruna_host SHARED "${_host_c}")
target_compile_options(andruna_host PRIVATE -w -fno-builtin)
target_link_options(andruna_host PRIVATE -Wl,-soname,libandruna_host.so)

add_library(c SHARED string.c stdio.c stdlib.c locale.c misc.c)
target_link_libraries(c PRIVATE andruna_host)
target_compile_options(c PRIVATE
  -ffreestanding -fno-builtin -fno-stack-protector -fno-exceptions
  -funwind-tables -g -O2 -w)
set_target_properties(c PROPERTIES OUTPUT_NAME c NO_SONAME OFF)
target_link_options(c PRIVATE -Wl,-soname,libc.so -Wl,-z,defs)

add_library(crtbegin_so OBJECT crtbegin_so.c)
target_compile_options(crtbegin_so PRIVATE -O2)
# A stable output name for build.py.
add_custom_command(TARGET c POST_BUILD
  COMMAND ${CMAKE_COMMAND} -E copy $<TARGET_OBJECTS:crtbegin_so>
          ${CMAKE_CURRENT_BINARY_DIR}/crtbegin_so.o
  COMMAND_EXPAND_LISTS)
add_dependencies(c crtbegin_so)
