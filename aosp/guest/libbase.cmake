# Stage aosp: liblog.so, libbase.so and libbase's gtest suite
# (libbase_test, a PIE started through crtbegin_dynamic.o like any Android
# executable), linked against the guest bionic and libc++.

set(_empty "${CMAKE_CURRENT_BINARY_DIR}/empty.c")
file(WRITE "${_empty}" "")

# liblog.so
add_library(log_so SHARED "${_empty}")
target_link_libraries(log_so PRIVATE "$<LINK_LIBRARY:WHOLE_ARCHIVE,liblog>")
set_target_properties(log_so PROPERTIES OUTPUT_NAME liblog.so)
guest_shared(log_so liblog.so)

# libbase.so (fmtlib linked in, as on Android).
add_library(base_so SHARED "${_empty}")
target_link_libraries(base_so PRIVATE
  "$<LINK_LIBRARY:WHOLE_ARCHIVE,libbase>" "$<LINK_LIBRARY:WHOLE_ARCHIVE,fmtlib>" log_so)
set_target_properties(base_so PROPERTIES OUTPUT_NAME libbase.so)
guest_shared(base_so libbase.so)

# libbase_test: the sources of Android.bp's libbase_test for the android
# target.
set(_b "${AOSP}/system/libbase")
add_executable(libbase_test)
aosp_sources(libbase_test "${_b}"
  chrono_utils_test.cpp
  cmsg_test.cpp
  endian_test.cpp
  errors_test.cpp
  expected_test.cpp
  file_test.cpp
  function_ref_test.cpp
  hex_test.cpp
  logging_splitters_test.cpp
  logging_test.cpp
  macros_test.cpp
  mapped_file_test.cpp
  no_destructor_test.cpp
  parsebool_test.cpp
  parsedouble_test.cpp
  parseint_test.cpp
  parsenetaddress_test.cpp
  process_test.cpp
  properties_test.cpp
  result_test.cpp
  scopeguard_test.cpp
  stringify_test.cpp
  stringprintf_test.cpp
  strings_test.cpp
  test_main.cpp
  test_utils_test.cpp)
set_target_properties(libbase_test PROPERTIES CXX_STANDARD 23)
target_compile_definitions(libbase_test PRIVATE _FILE_OFFSET_BITS=64)
target_include_directories(libbase_test PRIVATE "${_b}" "${_b}/include"
  "${AOSP}/system/core/libutils/include" "${AOSP}/system/logging/liblog/include")
target_link_libraries(libbase_test PRIVATE gtest_guest fmtlib base_so log_so)
guest_executable(libbase_test)
