# art/libartpalette: libartpalette.
#
# On Android this is apex/palette.cc, which loads the platform's
# libartpalette-system.so with dlopen. Andruna has no such library yet, so
# this builds the host variant (system/palette_fake.cc), which implements
# the palette calls in place. Which palette Andruna ships is decided when the
# runtime links (phase 0 exit).
set(_d "${AOSP}/art/libartpalette")

add_library(libartpalette STATIC)
aosp_sources(libartpalette "${_d}" system/palette_fake.cc)
set_target_properties(libartpalette PROPERTIES OUTPUT_NAME artpalette)
target_include_directories(libartpalette PUBLIC
  "${_d}/include"
  "${AOSP}/libnativehelper/include_jni")
target_link_libraries(libartpalette PUBLIC aosp_android libbase liblog)
