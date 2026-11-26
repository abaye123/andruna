# The JNI libraries ART loads at startup (Runtime::InitNativeMethods) and
# the libraries they need, as Android ELF shared objects with the sonames
# and the split of the ART and i18n modules:
#
#   libicuuc.so libicui18n.so libicu.so   ICU4C, and the NDK ICU API shim
#   libicu_jni.so                         ICU's JNI natives (android.icu)
#   libexpat.so                           XML parser
#   libandroidio.so                       AsynchronousCloseMonitor
#   libjavacore.so                        libcore's luni natives
#   libopenjdkjvm.so                      the JVM_* interface over libart.so
#   libopenjdk.so                         libcore's ojluni natives
#
# Included at the end of CMakeLists.txt (uses guest_so and GUEST_LINK).
# The static parts come from aosp/modules/icu.cmake and
# aosp/modules/libcore_natives.cmake.

# ICU. libandroidicuinit and the stub data are linked into libicuuc.so.
guest_so(icuuc_so libicuuc.so libicuuc)
target_link_libraries(icuuc_so PRIVATE libandroidicuinit libicuuc_stubdata
  base_so log_so ${GUEST_LINK})

guest_so(icui18n_so libicui18n.so libicui18n)
target_link_libraries(icui18n_so PRIVATE icuuc_so ${GUEST_LINK})

guest_so(icu_so libicu.so libicu_static)
target_link_libraries(icu_so PRIVATE icui18n_so icuuc_so ${GUEST_LINK})
target_link_options(icu_so PRIVATE "-Wl,--version-script,${AOSP}/external/icu/libicu/libicu.map.txt")

guest_so(icu_jni_so libicu_jni.so libicu_jni)
target_link_libraries(icu_jni_so PRIVATE libcutils
  icui18n_so icuuc_so nativehelper_so base_so log_so ${GUEST_LINK})

guest_so(expat_so libexpat.so libexpat)
target_link_libraries(expat_so PRIVATE ${GUEST_LINK})

# libcore.
guest_so(androidio_so libandroidio.so libandroidio)
target_link_libraries(androidio_so PRIVATE log_so ${GUEST_LINK})
target_link_options(androidio_so PRIVATE "-Wl,--version-script,${AOSP}/libcore/libandroidio.map.txt")

guest_so(javacore_so libjavacore.so libjavacore)
target_link_libraries(javacore_so PRIVATE libziparchive libz
  androidio_so icu_so expat_so nativehelper_so base_so log_so ${GUEST_LINK})
target_link_options(javacore_so PRIVATE "-Wl,--version-script,${AOSP}/libcore/libjavacore.map")

guest_so(openjdkjvm_so libopenjdkjvm.so libopenjdkjvm)
target_link_libraries(openjdkjvm_so PRIVATE art_so base_so log_so ${GUEST_LINK})

# android_get_LD_LIBRARY_PATH (libdl_android on Android) is left undefined:
# the loader binds it at run time.
guest_so(openjdk_so libopenjdk.so libopenjdk)
target_link_libraries(openjdk_so PRIVATE libfdlibm libz
  openjdkjvm_so androidio_so icu_so nativehelper_so base_so log_so ${GUEST_LINK})
