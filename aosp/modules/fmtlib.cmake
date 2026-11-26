# external/fmtlib: fmtlib (linked into libbase, as on Android).
set(_d "${AOSP}/external/fmtlib")

add_library(fmtlib STATIC)
aosp_sources(fmtlib "${_d}" src/format.cc)
target_compile_options(fmtlib PRIVATE -UNDEBUG)
set_target_properties(fmtlib PROPERTIES OUTPUT_NAME fmt)
target_include_directories(fmtlib PUBLIC "${_d}/include")
target_link_libraries(fmtlib PUBLIC aosp_android)
