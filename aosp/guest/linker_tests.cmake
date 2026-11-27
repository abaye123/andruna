# Stage linker-tests: the guest objects of andruna-linker's integration
# tests (crates/andruna-linker/tests/guest). Each lib<name>.c / .cpp
# becomes lib<name>.so, every other file a PIE executable. A first line
# `// LINK: -lfoo -lbar` adds libraries (other test libraries are found in
# this build directory). C++ files get libc++ with exceptions and RTTI.

set(ANDRUNA_LINKER_TESTS_DIR "" CACHE PATH "Directory of the linker test sources")
file(GLOB _srcs "${ANDRUNA_LINKER_TESTS_DIR}/*.c" "${ANDRUNA_LINKER_TESTS_DIR}/*.cpp")
if(NOT _srcs)
  message(STATUS "no linker test sources in '${ANDRUNA_LINKER_TESTS_DIR}'")
endif()

set(_libs_first)
set(_others)
foreach(s ${_srcs})
  get_filename_component(n "${s}" NAME_WE)
  if(n MATCHES "^lib")
    list(APPEND _libs_first "${s}")
  else()
    list(APPEND _others "${s}")
  endif()
endforeach()

foreach(s ${_libs_first} ${_others})
  get_filename_component(n "${s}" NAME_WE)
  file(STRINGS "${s}" _first LIMIT_COUNT 1)
  set(_extra)
  if(_first MATCHES "^// LINK:(.*)$")
    separate_arguments(_extra NATIVE_COMMAND "${CMAKE_MATCH_1}")
  endif()
  if(n MATCHES "^lib")
    add_library(${n} SHARED "${s}")
    set_target_properties(${n} PROPERTIES OUTPUT_NAME ${n}.so)
    guest_shared(${n} ${n}.so)
  else()
    add_executable(${n} "${s}")
    guest_executable(${n})
  endif()
  target_compile_options(${n} PRIVATE -O2 -g -fPIC -funwind-tables -fstack-protector-strong -ftrivial-auto-var-init=zero
    $<$<COMPILE_LANGUAGE:CXX>:-nostdinc++>
    "$<$<COMPILE_LANGUAGE:CXX>:SHELL:-isystem ${ANDRUNA_LIBCXX_DIR}/include/c++/v1>")
  target_link_options(${n} PRIVATE "-L${CMAKE_CURRENT_BINARY_DIR}")
  # Test libraries named in LINK are built first.
  foreach(e ${_extra})
    if(e MATCHES "^-l(.*)$")
      set(_dep "lib${CMAKE_MATCH_1}")
      if(TARGET ${_dep})
        add_dependencies(${n} ${_dep})
      endif()
    endif()
  endforeach()
  # lld resolves shared libraries in any order.
  target_link_libraries(${n} PRIVATE ${_extra})
endforeach()
