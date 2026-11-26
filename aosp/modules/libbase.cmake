# system/libbase: libbase (Android target sources; fmtlib is linked in).
set(_d "${AOSP}/system/libbase")

add_library(libbase STATIC)
aosp_sources(libbase "${_d}"
  chrono_utils.cpp
  cmsg.cpp
  file.cpp
  hex.cpp
  logging.cpp
  mapped_file.cpp
  parsebool.cpp
  parsenetaddress.cpp
  posix_strerror_r.cpp
  process.cpp
  properties.cpp
  result.cpp
  stringprintf.cpp
  strings.cpp
  threads.cpp
  test_utils.cpp
  # linux target
  errors_unix.cpp)
set_target_properties(libbase PROPERTIES CXX_STANDARD 23 OUTPUT_NAME base)
target_include_directories(libbase PUBLIC "${_d}/include")
target_link_libraries(libbase PUBLIC aosp_android fmtlib liblog)
