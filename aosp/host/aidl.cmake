# system/tools/aidl: libaidl-common and the aidl executable (Android.bp,
# cc_library_static "libaidl-common" and cc_binary_host "aidl").
set(_d "${AOSP}/system/tools/aidl")
set(_gen "${CMAKE_CURRENT_BINARY_DIR}/gen/aidl")
file(MAKE_DIRECTORY "${_gen}")

# Soong's yacc rule: bison -d --defines=<h> -o <cpp>, with location.hh and
# position.hh next to the output (gen_location_hh, gen_position_hh).
add_custom_command(
  OUTPUT "${_gen}/aidl_language_y.cpp" "${_gen}/aidl_language_y.h"
         "${_gen}/location.hh" "${_gen}/position.hh"
  COMMAND "${ANDRUNA_BISON}" -d "--defines=${_gen}/aidl_language_y.h"
          -o "${_gen}/aidl_language_y.cpp" "${_d}/aidl_language_y.yy"
  DEPENDS "${_d}/aidl_language_y.yy"
  COMMENT "bison aidl_language_y.yy"
  VERBATIM)
add_custom_command(
  OUTPUT "${_gen}/aidl_language_l.cpp"
  COMMAND "${ANDRUNA_FLEX}" "-o${_gen}/aidl_language_l.cpp" "${_d}/aidl_language_l.ll"
  DEPENDS "${_d}/aidl_language_l.ll" "${_gen}/aidl_language_y.h"
  COMMENT "flex aidl_language_l.ll"
  VERBATIM)

add_library(host_libaidl_common STATIC
  "${_gen}/aidl_language_y.cpp" "${_gen}/aidl_language_l.cpp")
host_sources(host_libaidl_common "${_d}"
  aidl_checkapi.cpp aidl_const_expressions.cpp aidl_dumpapi.cpp
  aidl_language.cpp aidl_to_common.cpp aidl_to_cpp_common.cpp aidl_to_cpp.cpp
  aidl_to_java.cpp aidl_to_ndk.cpp aidl_to_rust.cpp aidl_typenames.cpp aidl.cpp
  ast_java.cpp check_valid.cpp code_writer.cpp comments.cpp diagnostics.cpp
  generate_aidl_mappings.cpp generate_cpp.cpp generate_cpp_analyzer.cpp
  generate_java_binder.cpp generate_java.cpp generate_ndk.cpp generate_rust.cpp
  import_resolver.cpp io_delegate.cpp location.cpp logging.cpp options.cpp
  parser.cpp permission.cpp preprocess.cpp)
target_include_directories(host_libaidl_common PUBLIC
  "${_d}" "${_gen}" "${_d}/include"
  "${AOSP}/external/googletest/googletest/include")
target_compile_definitions(host_libaidl_common PUBLIC
  "PLATFORM_SDK_VERSION=${ANDRUNA_PLATFORM_SDK_VERSION}")
target_link_libraries(host_libaidl_common PUBLIC host_libbase)
target_link_libraries(host_libaidl_common PRIVATE host_defaults)

# libgtest is in aidl_defaults' static_libs: a few gtest assertions are
# used outside the unit tests.
set(_gt "${AOSP}/external/googletest/googletest")
add_library(host_gtest STATIC "${_gt}/src/gtest-all.cc")
target_include_directories(host_gtest PUBLIC "${_gt}/include")
target_include_directories(host_gtest PRIVATE "${_gt}")
target_link_libraries(host_gtest PRIVATE host_defaults)

add_executable(aidl "${_d}/main.cpp")
target_link_libraries(aidl PRIVATE host_libaidl_common host_libbase host_liblog host_gtest
  host_defaults -lpthread)
