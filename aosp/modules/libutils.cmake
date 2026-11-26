# system/core/libutils: the clock functions ART uses (utils/SystemClock.h in
# runtime/hprof). The rest of libutils (RefBase, Looper, threads) is for the
# framework and is not built yet.
set(_d "${AOSP}/system/core/libutils")

add_library(libutils STATIC)
aosp_sources(libutils "${_d}"
  SystemClock.cpp
  Timers.cpp)
set_target_properties(libutils PROPERTIES OUTPUT_NAME utils)
target_include_directories(libutils PUBLIC
  "${_d}/include"
  "${AOSP}/system/core/libsystem/include")
target_link_libraries(libutils PUBLIC aosp_android libcutils liblog)
