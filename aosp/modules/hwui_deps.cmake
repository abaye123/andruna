# Libraries libhwui links that no other Andruna module builds yet, as static
# libraries for the guest build (ANDRUNA_AOSP_GRAPHICS):
#
#   libutils_full      system/core/libutils: libutils_impl_defaults with
#                      libutils_binder (RefBase, String8/16, Vector, Looper,
#                      Threads, Trace), the Android target's sources.
#                      aosp/modules/libutils.cmake builds only the clock
#                      functions ART needs; when a fuller libutils module
#                      lands (libbinder needs the same), this one goes.
#   libvndksupport     system/core/libvndksupport (libutils' misc.cpp)
#   libminikin         frameworks/minikin/libs/minikin, without the Rust
#                      parts (patches/frameworks/minikin/0001)
#   *_aconfig          aconfig flag headers (aosp/tools/aconfig_cc.py; every
#                      flag at its default value, disabled)

set(_core "${AOSP}/system/core")

# --- aconfig flag libraries ---------------------------------------------------
# A package declared in several .aconfig files gets one header: the files are
# concatenated at configure time.
function(aconfig_cc_package target)
  set(merged "${CMAKE_CURRENT_BINARY_DIR}/gen/${target}.aconfig")
  set(text "")
  foreach(f ${ARGN})
    file(READ "${f}" t)
    if(text)
      # Only the first file's package line is kept.
      string(REGEX REPLACE "(^|\n)[ \t]*package[ \t]*:[^\n]*" "\\1" t "${t}")
      string(REGEX REPLACE "(^|\n)[ \t]*container[ \t]*:[^\n]*" "\\1" t "${t}")
    endif()
    string(APPEND text "${t}\n")
    set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${f}")
  endforeach()
  file(WRITE "${merged}.tmp" "${text}")
  file(COPY_FILE "${merged}.tmp" "${merged}" ONLY_IF_DIFFERENT)
  aconfig_cc_library(${target} "${merged}")
endfunction()

set(_fb "${AOSP}/frameworks/base")
aconfig_cc_package(text_flags_aconfig
  "${_fb}/core/java/android/text/flags/flags.aconfig"
  "${_fb}/core/java/android/text/flags/24Q3.aconfig")
aconfig_cc_package(hwui_flags_aconfig "${_fb}/libs/hwui/aconfig/hwui_flags.aconfig")
aconfig_cc_package(libgui_flags_aconfig "${AOSP}/frameworks/native/libs/gui/libgui_flags.aconfig")
aconfig_cc_package(surfaceflinger_flags_aconfig
  "${AOSP}/frameworks/native/services/surfaceflinger/surfaceflinger_flags.aconfig"
  "${AOSP}/frameworks/native/services/surfaceflinger/surfaceflinger_flags_new.aconfig")

# --- libvndksupport -----------------------------------------------------------
add_library(libvndksupport STATIC "${_core}/libvndksupport/linker.cpp")
target_include_directories(libvndksupport PUBLIC "${_core}/libvndksupport/include")
target_link_libraries(libvndksupport PRIVATE aosp_android liblog)

# --- libutils_full ------------------------------------------------------------
set(_u "${_core}/libutils")
add_library(libutils_full STATIC)
aosp_sources(libutils_full "${_u}"
  FileMap.cpp JenkinsHash.cpp LightRefBase.cpp NativeHandle.cpp Printer.cpp
  StopWatch.cpp SystemClock.cpp Threads.cpp Timers.cpp Tokenizer.cpp misc.cpp
  # android
  Trace.cpp
  # linux
  Looper.cpp)
aosp_sources(libutils_full "${_u}/binder"
  Errors.cpp RefBase.cpp SharedBuffer.cpp String16.cpp String8.cpp
  StrongPointer.cpp Unicode.cpp VectorImpl.cpp)
target_compile_definitions(libutils_full PRIVATE
  ANDROID_UTILS_REF_BASE_DISABLE_IMPLICIT_CONSTRUCTION)
target_compile_options(libutils_full PRIVATE -fvisibility=protected)
set_target_properties(libutils_full PROPERTIES CXX_STANDARD 23)
target_include_directories(libutils_full PUBLIC
  "${_u}/include" "${_u}/binder/include" "${_core}/libsystem/include")
target_link_libraries(libutils_full PUBLIC aosp_android libcutils liblog libbase)
target_link_libraries(libutils_full PRIVATE libvndksupport)

# --- libminikin ---------------------------------------------------------------
set(_m "${AOSP}/frameworks/minikin")
add_library(libminikin STATIC)
aosp_sources(libminikin "${_m}/libs/minikin"
  BidiUtils.cpp CmapCoverage.cpp Emoji.cpp Font.cpp FontCollection.cpp
  FontFakery.cpp FontFamily.cpp FontFeatureUtils.cpp FontFileParser.cpp
  FontUtils.cpp GraphemeBreak.cpp GreedyLineBreaker.cpp Hyphenator.cpp
  HyphenatorMap.cpp Layout.cpp LayoutCore.cpp LayoutUtils.cpp LineBreaker.cpp
  LineBreakerUtil.cpp Locale.cpp LocaleListCache.cpp MeasuredText.cpp
  Measurement.cpp MinikinFontFactory.cpp MinikinInternal.cpp
  OptimalLineBreaker.cpp ScriptUtils.cpp SparseBitSet.cpp SystemFonts.cpp
  U16StringPiece.cpp WordBreaker.cpp)
# ANDRUNA gates the patch that drops the Rust hyphenator.
target_compile_definitions(libminikin PRIVATE ANDRUNA "LOG_TAG=\"Minikin\"")
set_target_properties(libminikin PROPERTIES CXX_STANDARD 23)
target_compile_options(libminikin PRIVATE
  "$<$<COMPILE_LANGUAGE:CXX>:SHELL:-include andruna/libcxx_compat.h>")
target_include_directories(libminikin PUBLIC "${_m}/include")
target_include_directories(libminikin PRIVATE "${_m}/libs/minikin"
  "${AOSP}/external/googletest/googletest/include")
target_link_libraries(libminikin PUBLIC aosp_android libharfbuzz_ng libicuuc
  libicuuc_headers libicui18n_headers icu4c_defaults text_flags_aconfig
  libutils_full libcutils liblog libbase)
