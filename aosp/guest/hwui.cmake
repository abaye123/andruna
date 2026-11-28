# Stage hwui: Skia and HWUI as guest libraries (docs/plan/phase3/hwui.md).
#
# Outputs (in this build directory, collected in hwui/out by build_guest.py):
#   skia_scene   tests/skia/skia_scene.cpp: a Skia scene (paths, codecs,
#                HarfBuzz text with bidi) on the CPU raster back end and,
#                when the GLES libraries are given, on Ganesh GL
#
# GLES: ANDRUNA_GLES_DIR is the directory of andruna-gles' guest libraries
# (libEGL.so, libGLESv2.so: crates/andruna-gles/tools/build_guest.py, which
# writes out/gles/<arch>/guest). They are linked by soname, so the program
# finds them on the guest library path at run time.

set(ANDRUNA_GLES_DIR "" CACHE PATH "andruna-gles guest libraries (libEGL.so, libGLESv2.so)")
set(_gles FALSE)
if(EXISTS "${ANDRUNA_GLES_DIR}/libEGL.so" AND EXISTS "${ANDRUNA_GLES_DIR}/libGLESv2.so")
  set(_gles TRUE)
else()
  message(STATUS "hwui: no GLES libraries in '${ANDRUNA_GLES_DIR}'; skia_scene is raster only")
endif()

# Skia's sources use exceptions in one place (SkRawCodec, not built) and
# otherwise follow Android's defaults.
add_executable(skia_scene "${ANDRUNA_ROOT}/tests/skia/skia_scene.cpp")
target_link_libraries(skia_scene PRIVATE libskia_shaper libskia)
target_link_options(skia_scene PRIVATE -Wl,--gc-sections)
if(_gles)
  target_compile_definitions(skia_scene PRIVATE SKIA_SCENE_GL)
  target_link_libraries(skia_scene PRIVATE
    "${ANDRUNA_GLES_DIR}/libEGL.so" "${ANDRUNA_GLES_DIR}/libGLESv2.so")
  # The GLES libraries call the host's __andruna_gles_resolve, which the
  # loader provides at run time (andruna-gles' own demo links the same way).
  target_link_options(skia_scene PRIVATE -Wl,--allow-shlib-undefined)
endif()
target_link_libraries(skia_scene PRIVATE liblog)
guest_executable(skia_scene)

# libhwui_core is built with the stage so that CI checks the HWUI sources
# that compile today (aosp/modules/hwui.cmake); nothing links it yet.
add_custom_target(hwui_core ALL DEPENDS libhwui_core)
