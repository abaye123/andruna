# external/expat: libexpat (Android.bp "libexpat"), the XML parser behind
# libjavacore's org.apache.harmony.xml.ExpatParser natives.
set(_d "${AOSP}/external/expat")

add_library(libexpat STATIC)
aosp_sources(libexpat "${_d}/expat/lib" xmlparse.c xmlrole.c xmltok.c)
target_compile_definitions(libexpat PRIVATE HAVE_EXPAT_CONFIG_H)
target_compile_options(libexpat PRIVATE -fno-strict-aliasing -UWIN32_LEAN_AND_MEAN
  "SHELL:-include stdio.h")
target_include_directories(libexpat PUBLIC "${_d}" "${_d}/expat/lib")
target_link_libraries(libexpat PUBLIC aosp_android)
