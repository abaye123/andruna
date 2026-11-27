# libcore's native libraries (libcore/NativeCode.bp) and libopenjdkjvm
# (art/openjdkjvm/Android.bp), the JNI libraries ART loads at startup
# (spike S1 round 3, Android ELF build only). Needs icu.cmake and the ART
# targets (libart.cmake, art_elf.cmake).
#
# Modules:
#   libandroidio   :libandroidio_srcs (luni/src/main/native/Android.bp)
#   libjavacore    :luni_native_srcs  (luni/src/main/native/Android.bp)
#   libopenjdk     :libopenjdk_native_srcs (ojluni/src/main/native/Android.bp)
#   libopenjdkjvm  art/openjdkjvm/OpenjdkJvm.cc
#
# All are static, -fPIC; the guest build links each into the shared object
# of the same name.
#
# libjavacore and libopenjdk link BoringSSL's libcrypto_static
# (boringssl.cmake), as on Android: NativeBN's big numbers and the BoringSSL
# version System.c reports.

include(${CMAKE_CURRENT_LIST_DIR}/libexpat.cmake)
include(${CMAKE_CURRENT_LIST_DIR}/libfdlibm.cmake)

set(_lc "${AOSP}/libcore")
set(_luni "${_lc}/luni/src/main/native")
set(_oj "${_lc}/ojluni/src/main/native")

# core_native_default_flags + core_native_default_libs. -fno-rtti is Soong's
# default for C++. libnativehelper_compat_libc++ (JNIHelp.c and
# ExpandableString.c) is part of the guest libnativehelper.so here.
add_library(core_native_defaults INTERFACE)
target_compile_options(core_native_defaults INTERFACE
  $<$<COMPILE_LANGUAGE:CXX>:-DU_USING_ICU_NAMESPACE=0>
  $<$<COMPILE_LANGUAGE:CXX>:-fno-rtti>)
# patches/libcore is gated on ANDRUNA.
target_compile_definitions(core_native_defaults INTERFACE ANDRUNA=1)
target_link_libraries(core_native_defaults INTERFACE
  aosp_android libnativehelper_headers libbase liblog)

# --- libandroidio -----------------------------------------------------------
add_library(libandroidio STATIC "${_luni}/AsynchronousCloseMonitor.cpp")
target_compile_options(libandroidio PRIVATE
  $<$<COMPILE_LANGUAGE:CXX>:-DU_USING_ICU_NAMESPACE=0> $<$<COMPILE_LANGUAGE:CXX>:-fno-rtti>)
target_link_libraries(libandroidio PRIVATE aosp_android liblog)

# --- libjavacore ------------------------------------------------------------
add_library(libjavacore STATIC)
aosp_sources(libjavacore "${_luni}"
  ExecStrings.cpp
  IcuUtilities.cpp
  JniConstants.cpp
  JniException.cpp
  NetworkUtilities.cpp
  Register.cpp
  android_system_OsConstantsHolder.cpp
  cbigint.cpp
  java_lang_invoke_MethodHandle.cpp
  java_lang_invoke_VarHandle.cpp
  libcore_math_NativeBN.cpp
  libcore_icu_ICU.cpp
  libcore_io_AsynchronousCloseMonitor.cpp
  libcore_io_Linux.cpp
  libcore_io_Memory.cpp
  libcore_util_NativeAllocationRegistry.cpp
  org_apache_harmony_xml_ExpatParser.cpp)
target_include_directories(libjavacore PRIVATE "${_luni}")
target_link_libraries(libjavacore PRIVATE core_native_defaults
  libicu_ndk_headers libexpat libz libziparchive libcrypto_static)

# --- libopenjdk (libopenjdk_native_defaults) --------------------------------
add_library(libopenjdk STATIC)
aosp_sources(libopenjdk "${_oj}"
  Inflater.c
  Deflater.c
  CRC32.c
  Adler32.c
  jni_util.c
  jni_util_md.c
  io_util.c
  canonicalize_md.c
  FileDescriptor_md.c
  DatagramChannelImpl.c
  DatagramDispatcher.c
  Console_md.c
  IOUtil.c
  PollArrayWrapper.c
  SocketChannelImpl.c
  FileChannelImpl.c
  FileDispatcherImpl.c
  FileInputStream.c
  FileSystemPreferences.c
  EPoll.c
  EPollPort.c
  UnixAsynchronousServerSocketChannelImpl.c
  UnixAsynchronousSocketChannelImpl.c
  io_util_md.c
  NativeThread.c
  FileKey.c
  UnixFileSystem_md.c
  ObjectStreamClass.c
  ObjectOutputStream.c
  ObjectInputStream.c
  LinuxNativeDispatcher.c
  LinuxWatchService.c
  UnixCopyFile.c
  UnixNativeDispatcher.c
  InetAddress.c
  net_util.c
  net_util_md.c
  Net.c
  MappedByteBuffer.c
  Inet6Address.c
  Inet4Address.c
  linux_close.cpp
  ServerSocketChannelImpl.c
  SocketInputStream.c
  SocketOutputStream.c
  Float.c
  Double.c
  StrictMath.c
  Math.c
  ProcessEnvironment_md.c
  System.c
  Runtime.c
  UNIXProcess_md.c
  Character.cpp
  socket_tagger_util.cpp
  OnLoad.cpp
  JniConstants.cpp
  UnixDomainSockets.c
  jdk_internal_misc_VM.cpp)
target_include_directories(libopenjdk PRIVATE
  "${_luni}"
  # StrictMath.c includes "../../external/fdlibm/fdlibm.h", relative to a
  # directory two levels below the AOSP root.
  "${AOSP}/external/fdlibm")
# Soong defines _GNU_SOURCE for libopenjdk only on glibc and musl hosts; on
# Android jni_util_md.c needs the POSIX strerror_r.
target_compile_options(libopenjdk PRIVATE -U_GNU_SOURCE
  -Wno-unused-parameter -Wno-unused-variable -Wno-parentheses-equality
  -Wno-constant-logical-operand -Wno-sometimes-uninitialized)
target_link_libraries(libopenjdk PRIVATE core_native_defaults
  libicu_ndk_headers libz libfdlibm libcrypto_static)

# --- libopenjdkjvm ----------------------------------------------------------
add_library(libopenjdkjvm STATIC "${AOSP}/art/openjdkjvm/OpenjdkJvm.cc")
# libart_runtime's usage requirements (headers, ART defines), not its archive:
# the guest build links libopenjdkjvm.so against libart.so.
target_link_libraries(libopenjdkjvm PRIVATE $<COMPILE_ONLY:libart_runtime>
  libnativehelper_headers)
