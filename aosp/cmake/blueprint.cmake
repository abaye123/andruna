# Helpers for modules whose source lists come from Android.bp
# (aosp/tools/blueprint.py): the bionic libraries of the guest build.
#
#   bp_read(<name> ARCH <bp arch> MODULES <file:module[:variant]>... [READ <bp>...])
#       Runs blueprint.py at configure time and includes the result, which
#       defines BP_<module>[_<variant>]_{SRCS,CFLAGS,CONLYFLAGS,CPPFLAGS,
#       ASFLAGS,INCLUDES,HEADER_INCLUDES,WHOLE,STATIC,VERSION_SCRIPT}.
#
#   bp_object(<target> <var> [EXCLUDE <file>...] [SOURCES <file>...]
#             [OPTIONS <flag>...] [INCLUDES <dir>...] [GENERATED <name>=<file>...])
#       An OBJECT library from BP_<var>_*: the Android.bp sources minus
#       EXCLUDE (matched by path suffix), plus SOURCES, compiled with the
#       module's flags after ANDRUNA_BP_BASE_OPTIONS. Sources named
#       `:<genrule>` are replaced by the GENERATED mapping or dropped.

find_package(Python3 REQUIRED COMPONENTS Interpreter)

set(ANDRUNA_BLUEPRINT "${CMAKE_CURRENT_LIST_DIR}/../tools/blueprint.py")

function(bp_read name)
  cmake_parse_arguments(P "" "ARCH" "MODULES;READ" ${ARGN})
  set(out "${CMAKE_BINARY_DIR}/bp/${name}.cmake")
  set(args --arch ${P_ARCH} --aosp "${AOSP}" --out "${out}")
  foreach(r ${P_READ})
    list(APPEND args --read "${r}")
  endforeach()
  foreach(m ${P_MODULES})
    list(APPEND args --module "${m}")
  endforeach()
  execute_process(
    COMMAND "${Python3_EXECUTABLE}" "${ANDRUNA_BLUEPRINT}" ${args}
    RESULT_VARIABLE rc)
  if(NOT rc EQUAL 0)
    message(FATAL_ERROR "blueprint.py failed for ${name}")
  endif()
  include("${out}")
  # Make the BP_* variables visible to the caller.
  get_cmake_property(vars VARIABLES)
  foreach(v ${vars})
    if(v MATCHES "^BP_")
      set(${v} "${${v}}" PARENT_SCOPE)
    endif()
  endforeach()
  set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS
    "${ANDRUNA_BLUEPRINT}")
endfunction()

function(bp_object target var)
  cmake_parse_arguments(P "" "" "EXCLUDE;SOURCES;OPTIONS;INCLUDES;GENERATED" ${ARGN})
  set(srcs)
  foreach(s ${BP_${var}_SRCS})
    if(s MATCHES "^:(.*)$")
      set(gen "${CMAKE_MATCH_1}")
      foreach(g ${P_GENERATED})
        if(g MATCHES "^${gen}=(.*)$")
          list(APPEND srcs "${CMAKE_MATCH_1}")
        endif()
      endforeach()
      continue()
    endif()
    set(skip FALSE)
    foreach(e ${P_EXCLUDE})
      string(LENGTH "${e}" el)
      string(LENGTH "${s}" sl)
      if(sl GREATER_EQUAL el)
        math(EXPR start "${sl} - ${el}")
        string(SUBSTRING "${s}" ${start} ${el} tail)
        if(tail STREQUAL e)
          set(skip TRUE)
        endif()
      endif()
    endforeach()
    if(NOT skip)
      list(APPEND srcs "${s}")
    endif()
  endforeach()
  list(APPEND srcs ${P_SOURCES})
  if(NOT srcs)
    # An empty module (for example libc_aeabi outside arm): no target.
    add_library(${target} INTERFACE)
    return()
  endif()
  add_library(${target} OBJECT ${srcs})
  target_include_directories(${target} PRIVATE
    ${P_INCLUDES} ${BP_${var}_INCLUDES} ${BP_${var}_HEADER_INCLUDES})
  set(c ${BP_${var}_CFLAGS})
  target_compile_options(${target} PRIVATE
    ${ANDRUNA_BP_BASE_OPTIONS}
    ${c}
    "$<$<COMPILE_LANGUAGE:C>:${BP_${var}_CONLYFLAGS}>"
    "$<$<COMPILE_LANGUAGE:CXX>:${BP_${var}_CPPFLAGS}>"
    "$<$<COMPILE_LANGUAGE:ASM>:${BP_${var}_ASFLAGS}>"
    ${P_OPTIONS}
    ${ANDRUNA_BP_LATE_OPTIONS})
endfunction()

# The objects of a module and, recursively, of its whole_static_libs,
# among the modules for which `bp_object` created a target named
# bp_<module>.
function(bp_whole_objects out var)
  set(result)
  set(t "bp_${var}")
  string(REPLACE "-" "_" t "${t}")
  string(REPLACE "+" "x" t "${t}")
  if(TARGET ${t})
    get_target_property(type ${t} TYPE)
    if(type STREQUAL "OBJECT_LIBRARY")
      list(APPEND result "$<TARGET_OBJECTS:${t}>")
    endif()
  endif()
  foreach(w ${BP_${var}_WHOLE})
    string(REPLACE "-" "_" wv "${w}")
    string(REPLACE "+" "x" wv "${wv}")
    if(DEFINED BP_${wv}_SRCS)
      bp_whole_objects(sub ${wv})
      list(APPEND result ${sub})
    endif()
  endforeach()
  set(${out} ${result} PARENT_SCOPE)
endfunction()
