# The framework's native side, the part built so far (Android ELF only;
# docs/plan/phase2/framework-build.md). Static libraries, linked into the
# shared objects of aosp/guest/framework.cmake:
#
#   libutils_framework      system/core/libutils (libutils_impl and
#                           libutils_binder: RefBase, String8/16, Looper,
#                           Threads, FileMap, ...) and libvndksupport
#   libserver_configurable_flags
#   libincfs_utils          system/incremental_delivery incfs/util (map_ptr)
#   libandroidfw            frameworks/base/libs/androidfw, without the PNG
#                           sources (aapt2's crunching) and BackupData /
#                           BackupHelpers (backup natives, not built)
#   libandroid_runtime_jni  frameworks/base/core/jni: AndroidRuntime and the
#                           JNI sources listed below
#
# Needs art_elf.cmake (libnativehelper_headers) and libartbase.cmake
# (aconfig_cc_library).

set(_fw "${AOSP}/frameworks/base")
set(_cu "${AOSP}/system/core/libutils")

# --- libutils (full) --------------------------------------------------------
add_library(libutils_framework STATIC)
aosp_sources(libutils_framework "${_cu}"
  FileMap.cpp JenkinsHash.cpp LightRefBase.cpp NativeHandle.cpp Printer.cpp
  StopWatch.cpp SystemClock.cpp Threads.cpp Timers.cpp Tokenizer.cpp misc.cpp
  Trace.cpp Looper.cpp
  binder/Errors.cpp binder/RefBase.cpp binder/SharedBuffer.cpp binder/String16.cpp
  binder/String8.cpp binder/StrongPointer.cpp binder/Unicode.cpp binder/VectorImpl.cpp)
target_sources(libutils_framework PRIVATE "${AOSP}/system/core/libvndksupport/linker.cpp")
target_include_directories(libutils_framework PUBLIC
  "${_cu}/include" "${_cu}/binder/include"
  "${AOSP}/system/core/libsystem/include"
  "${AOSP}/system/core/libvndksupport/include")
target_compile_definitions(libutils_framework PUBLIC ANDROID_UTILS_REF_BASE_DISABLE_IMPLICIT_CONSTRUCTION)
target_compile_options(libutils_framework PRIVATE -fvisibility=protected)
target_link_libraries(libutils_framework PUBLIC aosp_android libcutils liblog libbase)

# --- server_configurable_flags ------------------------------------------------
add_library(libserver_configurable_flags STATIC
  "${AOSP}/system/server_configurable_flags/libflags/server_configurable_flags.cc")
target_include_directories(libserver_configurable_flags PUBLIC
  "${AOSP}/system/server_configurable_flags/libflags/include")
target_link_libraries(libserver_configurable_flags PUBLIC aosp_android libcutils libbase liblog)

# --- incfs utils -------------------------------------------------------------
set(_incfs "${AOSP}/system/incremental_delivery/incfs")
add_library(libincfs_utils STATIC "${_incfs}/util/map_ptr.cpp")
target_include_directories(libincfs_utils PUBLIC "${_incfs}/util/include")
target_include_directories(libincfs_utils PRIVATE "${_incfs}/include" "${_incfs}/kernel-headers")
target_link_libraries(libincfs_utils PUBLIC aosp_android libutils_framework libbase)

# --- aconfig flag headers (all flags disabled, aosp/tools/aconfig_cc.py) ------
aconfig_cc_library(android_content_res_flags_lib "${_fw}/core/java/android/content/res/flags.aconfig")
aconfig_cc_library(android_os_flags_lib "${_fw}/core/java/android/os/flags.aconfig")

# --- libandroidfw --------------------------------------------------------------
set(_afw "${_fw}/libs/androidfw")
add_library(libandroidfw STATIC)
aosp_sources(libandroidfw "${_afw}"
  ApkAssets.cpp ApkParsing.cpp Asset.cpp AssetDir.cpp AssetManager.cpp AssetManager2.cpp
  AssetsProvider.cpp AttributeResolution.cpp BigBuffer.cpp BigBufferStream.cpp
  ChunkIterator.cpp ConfigDescription.cpp CursorWindow.cpp FileStream.cpp Idmap.cpp
  LoadedArsc.cpp Locale.cpp LocaleData.cpp LocaleDataLookup.cpp misc.cpp NinePatch.cpp
  ObbFile.cpp PosixUtils.cpp ResourceTimer.cpp ResourceTypes.cpp ResourceUtils.cpp
  StreamingZipInflater.cpp StringPool.cpp TypeWrappers.cpp Util.cpp ZipFileRO.cpp
  ZipUtils.cpp
  # libandroidfw_pathutils
  PathUtils.cpp)
target_include_directories(libandroidfw PUBLIC "${_afw}/include")
target_link_libraries(libandroidfw PUBLIC aosp_android libutils_framework libincfs_utils
  libziparchive libz libcutils libbase liblog android_content_res_flags_lib)

# --- libandroid_runtime: the JNI sources built so far --------------------------
set(_jni "${_fw}/core/jni")
set(ANDRUNA_FRAMEWORK_JNI
  # source                                registration function
  "android_os_SystemClock.cpp"            register_android_os_SystemClock
  "android_util_CharsetUtils.cpp"         register_android_util_CharsetUtils
  "android_util_EventLog.cpp"             register_android_util_EventLog
  "android_util_Log.cpp"                  register_android_util_Log
  "android_util_AssetManager.cpp"         register_android_content_AssetManager
  "android_util_StringBlock.cpp"          register_android_content_StringBlock
  "android_util_XmlBlock.cpp"             register_android_content_XmlBlock
  "android_content_res_ApkAssets.cpp"     register_android_content_res_ApkAssets
  "android_content_res_ResourceTimer.cpp" register_android_content_res_ResourceTimer
  "android_os_SystemProperties.cpp"       register_android_os_SystemProperties
  "android_os_MessageQueue.cpp"           register_android_os_MessageQueue
  "android_os_Trace.cpp"                  register_android_os_Trace
  "android_content_res_Configuration.cpp" register_android_content_res_Configuration
)
set(_srcs)
set(_inc "${CMAKE_CURRENT_BINARY_DIR}/gen/andruna_reg_jni")
file(MAKE_DIRECTORY "${_inc}")
set(_reg "// Generated by aosp/modules/framework_natives.cmake.\n")
string(APPEND _reg "        REG_JNI(register_com_android_internal_os_RuntimeInit),\n")
string(APPEND _reg "        REG_JNI(register_com_android_internal_os_ZygoteInit_nativeZygoteInit),\n")
list(LENGTH ANDRUNA_FRAMEWORK_JNI _n)
math(EXPR _last "${_n} - 1")
foreach(i RANGE 0 ${_last} 2)
  math(EXPR j "${i} + 1")
  list(GET ANDRUNA_FRAMEWORK_JNI ${i} _s)
  list(GET ANDRUNA_FRAMEWORK_JNI ${j} _r)
  list(APPEND _srcs "${_jni}/${_s}")
  string(APPEND _reg "        REG_JNI(${_r}),\n")
endforeach()
file(WRITE "${_inc}/andruna_reg_jni.inc.tmp" "${_reg}")
configure_file("${_inc}/andruna_reg_jni.inc.tmp" "${_inc}/andruna_reg_jni.inc" COPYONLY)

add_library(libandroid_runtime_jni STATIC
  "${_jni}/AndroidRuntime.cpp" "${_jni}/core_jni_helpers.cpp" ${_srcs}
  # Andruna: tracing_perfetto's API on cutils' atrace (the perfetto SDK is
  # not built).
  "${CMAKE_CURRENT_LIST_DIR}/../guest/tracing_atrace.cpp")
target_include_directories(libandroid_runtime_jni PUBLIC "${_jni}" "${_jni}/include")
target_include_directories(libandroid_runtime_jni PRIVATE
  "${_inc}"
  "${AOSP}/frameworks/native/libs/tracing_perfetto/include"
  "${AOSP}/frameworks/native/include"
  "${AOSP}/bionic/libc/platform"
  "${AOSP}/libnativehelper/header_only_include"
  "${AOSP}/libnativehelper/include_platform"
  "${AOSP}/libnativehelper/include_platform_header_only")
target_compile_definitions(libandroid_runtime_jni PRIVATE
  ANDRUNA=1 U_USING_ICU_NAMESPACE=0 GL_GLEXT_PROTOTYPES EGL_EGLEXT_PROTOTYPES)
target_compile_options(libandroid_runtime_jni PRIVATE $<$<COMPILE_LANGUAGE:CXX>:-fno-rtti>)
target_link_libraries(libandroid_runtime_jni PUBLIC aosp_android libnativehelper_headers
  libandroidfw libutils_framework libserver_configurable_flags libcutils libbase liblog
  android_os_flags_lib android_content_res_flags_lib)
