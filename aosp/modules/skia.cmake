# external/skia: libskia (Android.bp "libskia", the Android target), the
# static library Android links into libhwui (whole_static_libs), and the
# Skia modules the guest tests use:
#
#   libskia            the Android.bp source list for the architecture, with
#                      Ganesh GL, CPU raster, codecs, PDF and FreeType
#   libskia_shaper     modules/skshaper (HarfBuzz shaper) and
#                      modules/skunicode (ICU, built-in), plus the Android
#                      font manager (fonts.xml), for the guest tests; HWUI
#                      itself shapes with minikin
#
# Configuration: aosp/overlay/skia/include/config/SkUserConfig.h replaces
# Android's generated SkUserConfig.h; it turns off the features whose
# dependencies are not built (Vulkan, AVIF, RAW, Fontations, Perfetto), and
# the matching sources are left out below.
#
# GL: Skia's GL backend calls EGL and OpenGL ES through libEGL.so and
# libGLESv2.so (andruna-gles' guest libraries, crates/andruna-gles/guest);
# the Khronos headers are frameworks/native's, as on Android.

include("${CMAKE_CURRENT_LIST_DIR}/skia_deps.cmake")

set(_sk "${AOSP}/external/skia")
set(_fn "${AOSP}/frameworks/native")

bp_read(skia ARCH ${BP_ARCH} MODULES external/skia/Android.bp:libskia)

# Headers of the Android libraries libskia uses (skia_deps' shared libraries).
add_library(skia_android_headers INTERFACE)
target_include_directories(skia_android_headers INTERFACE
  "${_fn}/opengl/include"
  "${_fn}/libs/nativewindow/include"
  "${_fn}/libs/nativebase/include"
  "${_fn}/libs/arect/include"
  "${AOSP}/system/core/libcutils/include"
  "${AOSP}/system/core/libutils/include"
  "${AOSP}/system/core/libsystem/include"
  "${AOSP}/system/logging/liblog/include"
  "${AOSP}/external/expat/expat/lib"
  "${AOSP}/external/zlib")

set(_skia_excl
  # Vulkan (SK_VULKAN off: no libvulkan yet).
  src/gpu/vk/vulkanmemoryallocator/VulkanMemoryAllocatorWrapper.cpp
  src/gpu/android/AndroidVulkanMemoryAllocator.cpp
  # AVIF through CrabbyAvif (Rust) and RAW through the DNG SDK.
  src/codec/SkCrabbyAvifCodec.cpp
  src/codec/SkRawCodec.cpp
  # Fontations (Rust); FreeType is the font back end.
  src/ports/SkFontMgr_fontations_empty.cpp
  src/ports/SkFontScanner_fontations.cpp
  src/ports/SkTypeface_fontations.cpp
  # Perfetto track events (SK_ANDROID_FRAMEWORK_USE_PERFETTO off).
  src/android/SkAndroidFrameworkPerfettoStaticStorage.cpp)
foreach(s ${BP_libskia_SRCS})
  if(s MATCHES "/src/gpu/(ganesh/)?vk/")
    string(REGEX REPLACE "^.*/external/skia/" "" rel "${s}")
    list(APPEND _skia_excl "${rel}")
  endif()
endforeach()

# Skia's own include directories, with Andruna's configuration first.
set(_skia_includes "${CMAKE_CURRENT_LIST_DIR}/../overlay/skia")
foreach(d ${BP_libskia_INCLUDES})
  if(NOT d MATCHES "vulkanmemoryallocator|vma_android")
    list(APPEND _skia_includes "${d}")
  endif()
endforeach()

bp_object(libskia_obj libskia EXCLUDE ${_skia_excl})
# bp_object put Android.bp's directories first; the overlay must win.
target_include_directories(libskia_obj BEFORE PRIVATE "${CMAKE_CURRENT_LIST_DIR}/../overlay/skia")
target_link_libraries(libskia_obj PUBLIC aosp_android skia_android_headers)
target_link_libraries(libskia_obj PRIVATE libft2 libharfbuzz_subset libpng libjpeg
  libwebp_decode libwebp_encode libwuffs libexpat libz)
# C++23: <cutils/trace.h> (SkATrace) uses <stdatomic.h> from C++, which
# libc++ accepts from C++23 (as for libcutils' trace-dev.cpp). The newer
# libc++ no longer includes some headers transitively: libcxx_compat.h.
set_target_properties(libskia_obj PROPERTIES CXX_STANDARD 23)
target_compile_options(libskia_obj PRIVATE
  "$<$<COMPILE_LANGUAGE:CXX>:SHELL:-include andruna/libcxx_compat.h>")

add_library(libskia STATIC $<TARGET_OBJECTS:libskia_obj>)
target_include_directories(libskia PUBLIC ${_skia_includes})
target_compile_definitions(libskia PUBLIC SK_BUILD_FOR_ANDROID_FRAMEWORK)
target_link_libraries(libskia PUBLIC aosp_android skia_android_headers
  libft2 libharfbuzz_subset libharfbuzz_ng libpng libjpeg libwebp_decode libwebp_encode
  libwuffs libexpat libz)

# --- libskia_shaper (tests) ---------------------------------------------------
add_library(libskia_shaper STATIC
  "${_sk}/modules/skshaper/src/SkShaper.cpp"
  "${_sk}/modules/skshaper/src/SkShaper_factory.cpp"
  "${_sk}/modules/skshaper/src/SkShaper_primitive.cpp"
  "${_sk}/modules/skshaper/src/SkShaper_harfbuzz.cpp"
  "${_sk}/modules/skshaper/src/SkShaper_skunicode.cpp"
  "${_sk}/modules/skunicode/src/SkUnicode.cpp"
  "${_sk}/modules/skunicode/src/SkUnicode_hardcoded.cpp"
  "${_sk}/modules/skunicode/src/SkUnicode_icu.cpp"
  "${_sk}/modules/skunicode/src/SkUnicode_icu_bidi.cpp"
  "${_sk}/modules/skunicode/src/SkBidiFactory_icu_full.cpp"
  "${_sk}/modules/skunicode/src/SkUnicode_icu_builtin.cpp"
  "${_sk}/src/ports/SkFontMgr_android.cpp"
  "${_sk}/src/ports/SkFontMgr_android_parser.cpp"
  "${_sk}/src/ports/SkTypeface_proxy.cpp")
target_compile_definitions(libskia_shaper PUBLIC
  SK_SHAPER_PRIMITIVE_AVAILABLE SK_SHAPER_HARFBUZZ_AVAILABLE SK_SHAPER_UNICODE_AVAILABLE
  SK_UNICODE_AVAILABLE SK_UNICODE_ICU_IMPLEMENTATION)
target_compile_definitions(libskia_shaper PRIVATE
  SKSHAPER_IMPLEMENTATION=1 SKUNICODE_IMPLEMENTATION=1 U_USING_ICU_NAMESPACE=0)
target_include_directories(libskia_shaper PUBLIC
  "${_sk}/modules/skshaper/include" "${_sk}/modules/skunicode/include")
target_compile_options(libskia_shaper PRIVATE -fvisibility=hidden)
# ICU's data comes from the data file (libandroidicuinit maps it on Android);
# the linked-in data is the empty stub, as in Android's libicuuc.
target_link_libraries(libskia_shaper PUBLIC libskia libharfbuzz_ng libicuuc libicuuc_stubdata libexpat)
target_link_libraries(libskia_shaper PRIVATE libicuuc_headers icu4c_defaults)
