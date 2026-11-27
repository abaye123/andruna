# external/conscrypt: libjavacrypto (Android.bp, with libjavacrypto-defaults),
# the JNI library of the Conscrypt security provider (Android ELF build only).
# Needs boringssl.cmake.
#
# Static, -fPIC; the guest build links it into libjavacrypto.so against
# libcrypto.so, libssl.so and liblog.so, as the com.android.conscrypt APEX
# ships it. JNI_JARJAR_PREFIX makes the natives register for the
# repackaged classes (com/android/org/conscrypt/...).

set(_cs "${AOSP}/external/conscrypt")

file(GLOB_RECURSE _srcs "${_cs}/common/src/jni/main/cpp/*.cc")
add_library(libjavacrypto STATIC ${_srcs})
target_compile_definitions(libjavacrypto PRIVATE "JNI_JARJAR_PREFIX=com/android/")
target_compile_options(libjavacrypto PRIVATE -fvisibility=hidden)
target_include_directories(libjavacrypto PRIVATE "${_cs}/common/src/jni/main/include")
target_link_libraries(libjavacrypto PRIVATE aosp_android libnativehelper_headers
  boringssl_headers liblog)
