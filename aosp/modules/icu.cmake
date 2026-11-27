# external/icu: ICU4C as the i18n module builds it, and the ICU JNI
# library ART loads at startup (spike S1 round 3, Android ELF build only).
#
# Modules (Android.bp in the directory named):
#   libicuuc_stubdata  icu4c/source            (empty icudt78_dat)
#   libandroidicuinit  libandroidicuinit       (maps the ICU data file)
#   libicuuc           icu4c/source/common     (*.cpp)
#   libicui18n         icu4c/source/i18n       (*.cpp)
#   libicu_static      libicu/src              (the NDK C API shim of libicu)
#   libicu_jni         android_icu4j/libcore_bridge/src/native (*.cpp)
#
# All are static, -fPIC; the guest build links them into libicuuc.so,
# libicui18n.so, libicu.so and libicu_jni.so as on Android.
#
# ICU data: libandroidicuinit maps $ANDROID_I18N_ROOT/etc/icu/icudt78l.dat
# (icu4c/source/stubdata/icudt78l.dat in the sources) and reads time zone
# overrides from $ANDROID_TZDATA_ROOT/etc/tz/versioned/10/icu if present.
# Both variables must be set (on Android: /apex/com.android.i18n and
# /apex/com.android.tzdata); without them it aborts.

set(_icu "${AOSP}/external/icu")
set(_icu4c "${_icu}/icu4c/source")

# icu_common_cflags + icu4c_defaults (external/icu/Android.bp), Android target.
add_library(icu4c_defaults INTERFACE)
target_compile_options(icu4c_defaults INTERFACE -Wno-unnecessary-virtual-specifier)
target_compile_definitions(icu4c_defaults INTERFACE
  UCONFIG_USE_ML_PHRASE_BREAKING=1
  # Required to compile against libicuuc and libicui18n.
  ANDROID_LINK_SHARED_ICU4C)
target_link_libraries(icu4c_defaults INTERFACE aosp_android)

# libicuuc_headers (with icu4c_extra_headers) and libicui18n_headers.
add_library(libicuuc_headers INTERFACE)
target_include_directories(libicuuc_headers INTERFACE
  "${_icu4c}/common" "${_icu}/android_icu4c/include")
add_library(libicui18n_headers INTERFACE)
target_include_directories(libicui18n_headers INTERFACE "${_icu4c}/i18n")

# --- libicuuc_stubdata ------------------------------------------------------
add_library(libicuuc_stubdata STATIC "${_icu4c}/stubdata/stubdata.cpp")
target_compile_definitions(libicuuc_stubdata PRIVATE ANDROID_LINK_SHARED_ICU4C)
target_link_libraries(libicuuc_stubdata PRIVATE aosp_android libicuuc_headers)

# --- libandroidicuinit ------------------------------------------------------
file(GLOB _srcs "${_icu}/libandroidicuinit/*.cpp")
add_library(libandroidicuinit STATIC ${_srcs})
target_compile_definitions(libandroidicuinit PRIVATE ANDROID_LINK_SHARED_ICU4C)
target_include_directories(libandroidicuinit PUBLIC "${_icu}/libandroidicuinit/include")
target_link_libraries(libandroidicuinit PRIVATE aosp_android libicuuc_headers libbase liblog)

# --- libicuuc (libicuuc_defaults) -------------------------------------------
file(GLOB _srcs "${_icu4c}/common/*.cpp")
add_library(libicuuc STATIC ${_srcs})
target_compile_definitions(libicuuc PRIVATE _REENTRANT U_COMMON_IMPLEMENTATION PIC)
target_compile_options(libicuuc PRIVATE -O3 -fvisibility=hidden)
target_link_libraries(libicuuc PUBLIC icu4c_defaults libicuuc_headers)
target_link_libraries(libicuuc PRIVATE libandroidicuinit)

# --- libicui18n (libicui18n_defaults) ---------------------------------------
file(GLOB _srcs "${_icu4c}/i18n/*.cpp")
add_library(libicui18n STATIC ${_srcs})
target_compile_definitions(libicui18n PRIVATE _REENTRANT U_I18N_IMPLEMENTATION PIC)
target_compile_options(libicui18n PRIVATE -O3 -fvisibility=hidden)
target_link_libraries(libicui18n PUBLIC icu4c_defaults libicuuc_headers libicui18n_headers)

# --- libicu_static (the libicu NDK shim) ------------------------------------
add_library(libicu_static STATIC "${_icu}/libicu/src/shim.cpp")
target_compile_definitions(libicu_static PRIVATE
  U_SHOW_CPLUSPLUS_API=0 U_SHOW_CPLUSPLUS_HEADER_API=0)
target_link_libraries(libicu_static PRIVATE icu4c_defaults libicuuc_headers libicui18n_headers)

# The headers libicu exports to its users (the NDK ICU API).
add_library(libicu_ndk_headers INTERFACE)
target_include_directories(libicu_ndk_headers INTERFACE "${_icu}/libicu/ndk_headers")

# --- libicu_jni -------------------------------------------------------------
file(GLOB _srcs "${_icu}/android_icu4j/libcore_bridge/src/native/*.cpp")
add_library(libicu_jni STATIC ${_srcs})
target_compile_definitions(libicu_jni PRIVATE ANDROID_LINK_SHARED_ICU4C)
# icu_common_cflags; cppflags; Soong's default -fno-rtti (rtti not set).
target_compile_options(libicu_jni PRIVATE -Wno-unnecessary-virtual-specifier
  $<$<COMPILE_LANGUAGE:CXX>:-DU_USING_ICU_NAMESPACE=0>
  $<$<COMPILE_LANGUAGE:CXX>:-fno-rtti>)
target_link_libraries(libicu_jni PRIVATE aosp_android libnativehelper_headers
  libbase liblog libcutils libicuuc_headers libicui18n_headers)
# <cutils/trace.h> uses <stdatomic.h> from C++, which libc++ accepts from C++23
# (as for libcutils' trace-dev.cpp).
set_source_files_properties(
  "${_icu}/android_icu4j/libcore_bridge/src/native/com_android_i18n_util_ATrace.cpp"
  PROPERTIES COMPILE_OPTIONS "-std=gnu++23")
