# Stage art: ART and the libraries it opens at start-up, as Android ELF
# shared objects linked against the guest bionic and libc++, plus the
# dalvikvm and dex2oat executables. andruna-app runs these with
# andruna-linker (docs/plan/phase1/art-on-linker.md).
#
# The static parts come from aosp/CMakeLists.txt with
# ANDRUNA_AOSP_RUNTIME=ON (libart.cmake, art_elf.cmake, icu.cmake,
# libcore_natives.cmake, dex2oat.cmake). The library split is Android's
# where it matters at run time (one liblog, libbase, libnativehelper and
# libsigchain in front of libc and libart); ART's own static libraries are
# folded into libart.so.

set(_empty "${CMAKE_CURRENT_BINARY_DIR}/empty.c")
file(WRITE "${_empty}" "")

# A shared object made of whole static libraries.
function(art_so target soname)
  add_library(${target} SHARED "${_empty}")
  set(wl)
  foreach(a ${ARGN})
    list(APPEND wl "$<LINK_LIBRARY:WHOLE_ARCHIVE,${a}>")
  endforeach()
  target_link_libraries(${target} PRIVATE ${wl})
  set_target_properties(${target} PROPERTIES OUTPUT_NAME "${soname}")
  guest_shared(${target} ${soname})
  # As Soong links shared objects: every reference resolved at link time.
  target_link_options(${target} PRIVATE -Wl,--no-undefined)
endfunction()

art_so(log_so liblog.so liblog)

art_so(base_so libbase.so libbase fmtlib)
target_link_libraries(base_so PRIVATE log_so)

art_so(nativehelper_so libnativehelper.so libnativehelper)
target_link_libraries(nativehelper_so PRIVATE log_so)

art_so(sigchain_so libsigchain.so libsigchain_static art_async_safe)
target_link_libraries(sigchain_so PRIVATE base_so log_so)

# libart.so: the runtime with the optimizing compiler, libartbase,
# libdexfile, libprofile and the smaller libraries folded in.
art_so(art_so libart.so
  libart_runtime libart_compiler libartbase libdexfile libprofile libelffile libartpalette
  libnativebridge libnativeloader libcpu_features libprocinfo liblzma)
target_link_libraries(art_so PRIVATE
  libziparchive libtinyxml2 liblz4 libz libcutils libutils
  sigchain_so nativehelper_so base_so log_so "${SYSLIB}/libdl_android.so")

# dalvikvm (art/dalvikvm): finds libart.so through libnativehelper's
# JniInvocation, as on Android.
add_executable(dalvikvm "${AOSP}/art/dalvikvm/dalvikvm.cc")
target_include_directories(dalvikvm PRIVATE "${AOSP}/art/libartbase" "${AOSP}/bionic/libc/platform")
target_include_directories(dalvikvm PRIVATE $<TARGET_PROPERTY:libbase,INTERFACE_INCLUDE_DIRECTORIES>)
target_link_libraries(dalvikvm PRIVATE aosp_android art_defaults libnativehelper_headers
  nativehelper_so base_so log_so)
guest_executable(dalvikvm)

# dex2oat (art/dex2oat), linked against libart.so (which holds the runtime,
# the compiler, libprofile and libelffile) and libart-dex2oat. The archive
# is named by file, not as a target, so that its static dependencies
# already in libart.so are not linked again.
add_executable(dex2oat $<TARGET_OBJECTS:dex2oat_main>)
add_dependencies(dex2oat dex2oat_main libart_dex2oat)
target_link_libraries(dex2oat PRIVATE
  -Wl,--whole-archive "$<TARGET_FILE:libart_dex2oat>" -Wl,--no-whole-archive
  art_so sigchain_so nativehelper_so base_so log_so)
guest_executable(dex2oat)

# --- The JNI libraries ART loads at start-up (Runtime::InitNativeMethods) ----
#
#   libicuuc.so libicui18n.so libicu.so   ICU4C, and the NDK ICU API shim
#   libicu_jni.so                         ICU's JNI natives (android.icu)
#   libexpat.so                           XML parser
#   libandroidio.so                       AsynchronousCloseMonitor
#   libjavacore.so                        libcore's luni natives
#   libopenjdkjvm.so                      the JVM_* interface over libart.so
#   libopenjdk.so                         libcore's ojluni natives

art_so(icuuc_so libicuuc.so libicuuc)
target_link_libraries(icuuc_so PRIVATE libandroidicuinit libicuuc_stubdata base_so log_so)

art_so(icui18n_so libicui18n.so libicui18n)
target_link_libraries(icui18n_so PRIVATE icuuc_so)

art_so(icu_so libicu.so libicu_static)
target_link_libraries(icu_so PRIVATE icui18n_so icuuc_so)
target_link_options(icu_so PRIVATE "-Wl,--version-script,${AOSP}/external/icu/libicu/libicu.map.txt")

art_so(icu_jni_so libicu_jni.so libicu_jni)
target_link_libraries(icu_jni_so PRIVATE libcutils icui18n_so icuuc_so nativehelper_so base_so log_so)

art_so(expat_so libexpat.so libexpat)

art_so(androidio_so libandroidio.so libandroidio)
target_link_libraries(androidio_so PRIVATE log_so)
target_link_options(androidio_so PRIVATE "-Wl,--version-script,${AOSP}/libcore/libandroidio.map.txt")

art_so(javacore_so libjavacore.so libjavacore)
target_link_libraries(javacore_so PRIVATE libziparchive libz
  androidio_so icu_so expat_so nativehelper_so base_so log_so)
target_link_options(javacore_so PRIVATE "-Wl,--version-script,${AOSP}/libcore/libjavacore.map")

art_so(openjdkjvm_so libopenjdkjvm.so libopenjdkjvm)
target_link_libraries(openjdkjvm_so PRIVATE art_so base_so log_so)

art_so(openjdk_so libopenjdk.so libopenjdk)
target_link_libraries(openjdk_so PRIVATE libfdlibm libz
  openjdkjvm_so androidio_so icu_so nativehelper_so base_so log_so "${SYSLIB}/libdl_android.so")

# Everything the stage produces, in one directory (what andruna-app mounts
# as the ART module's library directory).
set(ART_OUT "${CMAKE_BINARY_DIR}/out")
add_custom_target(art_install ALL
  COMMAND "${CMAKE_COMMAND}" -E make_directory "${ART_OUT}"
  COMMAND "${CMAKE_COMMAND}" -E copy_if_different
    $<TARGET_FILE:log_so> $<TARGET_FILE:base_so> $<TARGET_FILE:nativehelper_so>
    $<TARGET_FILE:sigchain_so> $<TARGET_FILE:art_so> $<TARGET_FILE:dalvikvm>
    $<TARGET_FILE:dex2oat> $<TARGET_FILE:icuuc_so> $<TARGET_FILE:icui18n_so>
    $<TARGET_FILE:icu_so> $<TARGET_FILE:icu_jni_so> $<TARGET_FILE:expat_so>
    $<TARGET_FILE:androidio_so> $<TARGET_FILE:javacore_so>
    $<TARGET_FILE:openjdkjvm_so> $<TARGET_FILE:openjdk_so>
    "${ART_OUT}"
  DEPENDS log_so base_so nativehelper_so sigchain_so art_so dalvikvm dex2oat
    icuuc_so icui18n_so icu_so icu_jni_so expat_so androidio_so javacore_so
    openjdkjvm_so openjdk_so
  VERBATIM)
