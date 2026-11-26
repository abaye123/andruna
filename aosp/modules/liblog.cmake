# system/logging/liblog: liblog (Android target sources).
set(_d "${AOSP}/system/logging/liblog")

add_library(liblog STATIC)
aosp_sources(liblog "${_d}"
  log_event_list.cpp
  log_event_write.cpp
  logger_name.cpp
  logger_read.cpp
  logger_write.cpp
  logprint.cpp
  properties.cpp
  # Android target only.
  event_tag_map.cpp
  log_time.cpp
  pmsg_reader.cpp
  pmsg_writer.cpp
  logd_reader.cpp
  logd_writer.cpp)
target_compile_definitions(liblog PRIVATE
  LIBLOG_LOG_TAG=1006
  SNET_EVENT_LOG_TAG=1397638484
  ANDROID_DEBUGGABLE=0)
target_include_directories(liblog PUBLIC "${_d}/include")
target_include_directories(liblog PRIVATE
  "${AOSP}/system/libbase/include"
  "${AOSP}/system/core/libcutils/include"
  "${AOSP}/system/core/libutils/include"
  "${AOSP}/system/core/libsystem/include"
  "${AOSP}/external/fmtlib/include")
target_link_libraries(liblog PUBLIC aosp_android)
set_target_properties(liblog PROPERTIES OUTPUT_NAME log)
