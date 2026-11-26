# system/core/libcutils: the part of libcutils that the runtime stack uses.
# ART itself includes no libcutils header; liblog and libbase use its
# headers, and the platform's libartpalette uses ashmem and trace. The
# device-management sources (android_reboot, klog, partition_utils,
# qtaguid, uevent, fs_config, sockets) are left out.
set(_d "${AOSP}/system/core/libcutils")

add_library(libcutils STATIC)
aosp_sources(libcutils "${_d}"
  config_utils.cpp
  iosched_policy.cpp
  load_file.cpp
  native_handle.cpp
  properties.cpp
  record_stream.cpp
  strlcpy.c
  fs.cpp
  hashmap.cpp
  multiuser.cpp
  str_parms.cpp
  ashmem-dev.cpp
  trace-dev.cpp)
set_target_properties(libcutils PROPERTIES OUTPUT_NAME cutils)
target_include_directories(libcutils PUBLIC
  "${_d}/include"
  "${AOSP}/system/core/libprocessgroup/include")
target_include_directories(libcutils PRIVATE
  "${AOSP}/bionic/libc/async_safe/include")
target_link_libraries(libcutils PUBLIC aosp_android libbase liblog)
# trace-dev.inc uses <stdatomic.h> from C++, which libc++ accepts from C++23.
set_source_files_properties("${_d}/trace-dev.cpp" PROPERTIES COMPILE_OPTIONS "-std=gnu++23")
