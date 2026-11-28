# Stage binder: libbinder, libbinder_ndk, servicemanager and the libraries
# they link, as Android ELF (docs/plan/phase2/binder.md):
#
#   liblog.so libbase.so libcutils.so libutils.so libvndksupport.so
#   libapexsupport.so (Andruna's, apexsupport.cpp) libbinder.so
#   libbinder_ndk.so servicemanager, and the libbinder test programs in
#   aosp/guest/binder/tests (one executable per .cpp file).
#
# The C++ interfaces libbinder and servicemanager use are generated from
# their .aidl files with the AIDL compiler (ANDRUNA_AIDL, the host tool
# from aosp/tools/build_host.py, ADR 0026), with the flags Soong uses for
# an `aidl` source of a cc module.
#
# Each library follows its Android.bp module (sources, cflags, static and
# shared libraries) for the android target. Departures:
#   - servicemanager's access checks: Access.cpp (SELinux) is replaced by
#     servicemanager_access.cpp (uid rules, ADR 0041); ServiceManager.cpp
#     and main.cpp are compiled with VENDORSERVICEMANAGER, the switch that
#     leaves out VINTF manifests and perfetto tracing (Andruna has neither).
#   - libapexsupport is Andruna's (no APEX provides HALs).

set(ANDRUNA_AIDL "" CACHE FILEPATH "The AIDL compiler (out/host/<arch>/bin/aidl.exe)")
if(NOT EXISTS "${ANDRUNA_AIDL}")
  message(FATAL_ERROR "AIDL compiler not found at '${ANDRUNA_AIDL}'; run aosp/tools/build_host.py")
endif()

set(_here "${CMAKE_CURRENT_LIST_DIR}")
set(_fn "${AOSP}/frameworks/native")
set(_b "${_fn}/libs/binder")
set(_fb "${AOSP}/frameworks/base")
set(_gen "${CMAKE_CURRENT_BINARY_DIR}/gen")
set(_empty "${CMAKE_CURRENT_BINARY_DIR}/empty.c")
file(WRITE "${_empty}" "")

# C++23: cutils/trace.h includes <stdatomic.h>, which the guest libc++
# accepts next to <atomic> only from C++23 on.
set(CMAKE_CXX_STANDARD 23)

# Compile options of the libraries and programs of this stage.
add_library(binder_defaults INTERFACE)
target_link_libraries(binder_defaults INTERFACE aosp_android)
# Standard headers AOSP's older libc++ included transitively.
target_compile_options(binder_defaults INTERFACE
  "$<$<COMPILE_LANGUAGE:CXX>:SHELL:-include andruna/libcxx_compat.h>"
  # Soong's default for cc modules without `rtti: true`.
  $<$<COMPILE_LANGUAGE:CXX>:-fno-rtti>)

# aidl_cpp(<target> <header dir> <source dir> INCLUDES <dir>... FILES <root> <file.aidl>...)
# Generates C++ for each .aidl file (relative to <root>, an import root)
# and adds the sources to <target>. aidl matches files against import
# roots textually, so paths are given with Windows separators on Windows.
function(aidl_cpp target hdir sdir)
  cmake_parse_arguments(A "" "" "INCLUDES;FILES" ${ARGN})
  set(inc)
  foreach(i ${A_INCLUDES})
    file(TO_NATIVE_PATH "${i}" n)
    list(APPEND inc "-I${n}")
  endforeach()
  file(TO_NATIVE_PATH "${hdir}" nh)
  file(TO_NATIVE_PATH "${sdir}" ns)
  list(GET A_FILES 0 root)
  list(REMOVE_AT A_FILES 0)
  foreach(f ${A_FILES})
    string(REGEX REPLACE "\\.aidl$" ".cpp" rel "${f}")
    set(in "${root}/${f}")
    file(TO_NATIVE_PATH "${in}" nin)
    add_custom_command(
      OUTPUT "${sdir}/${rel}"
      COMMAND "${ANDRUNA_AIDL}" --lang=cpp ${inc} -h "${nh}" -o "${ns}" "${nin}"
      DEPENDS "${in}" "${ANDRUNA_AIDL}"
      COMMENT "aidl ${f}"
      VERBATIM)
    target_sources(${target} PRIVATE "${sdir}/${rel}")
  endforeach()
  target_include_directories(${target} PUBLIC "${hdir}")
endfunction()

# aconfig flag header (aosp/tools/aconfig_cc.py: every flag at its default).
function(binder_aconfig target aconfig_file)
  set(outdir "${_gen}/${target}")
  set(stamp "${outdir}/${target}.stamp")
  add_custom_command(
    OUTPUT "${stamp}"
    COMMAND "${Python3_EXECUTABLE}" "${ANDRUNA_ROOT}/aosp/tools/aconfig_cc.py"
            "${aconfig_file}" "${outdir}"
    COMMAND "${CMAKE_COMMAND}" -E touch "${stamp}"
    DEPENDS "${aconfig_file}" "${ANDRUNA_ROOT}/aosp/tools/aconfig_cc.py"
    VERBATIM)
  add_custom_target(${target}_gen DEPENDS "${stamp}")
  add_library(${target} INTERFACE)
  add_dependencies(${target} ${target}_gen)
  target_include_directories(${target} INTERFACE "${outdir}")
endfunction()

# --- liblog.so, libbase.so (as stage aosp) ----------------------------------
add_library(binder_log_so SHARED "${_empty}")
target_link_libraries(binder_log_so PRIVATE "$<LINK_LIBRARY:WHOLE_ARCHIVE,liblog>")
set_target_properties(binder_log_so PROPERTIES OUTPUT_NAME liblog.so)
guest_shared(binder_log_so liblog.so)

add_library(binder_base_so SHARED "${_empty}")
target_link_libraries(binder_base_so PRIVATE
  "$<LINK_LIBRARY:WHOLE_ARCHIVE,libbase>" "$<LINK_LIBRARY:WHOLE_ARCHIVE,fmtlib>" binder_log_so)
set_target_properties(binder_base_so PROPERTIES OUTPUT_NAME libbase.so)
guest_shared(binder_base_so libbase.so)

# --- libcutils.so (aosp/modules/libcutils.cmake: the subset built so far) ---
add_library(binder_cutils_so SHARED "${_empty}")
target_link_libraries(binder_cutils_so PRIVATE
  "$<LINK_LIBRARY:WHOLE_ARCHIVE,libcutils>" binder_base_so binder_log_so)
set_target_properties(binder_cutils_so PROPERTIES OUTPUT_NAME libcutils.so)
guest_shared(binder_cutils_so libcutils.so)

# --- libvndksupport.so (system/core/libvndksupport) --------------------------
set(_d "${AOSP}/system/core/libvndksupport")
add_library(binder_vndksupport_so SHARED "${_d}/linker.cpp")
target_include_directories(binder_vndksupport_so PRIVATE "${_d}/include/vndksupport")
target_include_directories(binder_vndksupport_so PUBLIC "${_d}/include")
target_link_libraries(binder_vndksupport_so PRIVATE binder_defaults liblog_headers_guest
  binder_log_so -ldl_android)
target_link_options(binder_vndksupport_so PRIVATE
  "-Wl,--version-script=${_d}/libvndksupport.map.txt")
set_target_properties(binder_vndksupport_so PROPERTIES OUTPUT_NAME libvndksupport.so)
guest_shared(binder_vndksupport_so libvndksupport.so)

# --- libapexsupport.so (Andruna's) -------------------------------------------
add_library(binder_apexsupport_so SHARED "${_here}/apexsupport.cpp")
target_include_directories(binder_apexsupport_so PUBLIC "${_here}/include")
target_link_libraries(binder_apexsupport_so PRIVATE binder_defaults liblog_headers_guest
  binder_log_so)
set_target_properties(binder_apexsupport_so PROPERTIES OUTPUT_NAME libapexsupport.so)
guest_shared(binder_apexsupport_so libapexsupport.so)

# --- libutils.so (system/core/libutils, libutils_impl_defaults) --------------
set(_d "${AOSP}/system/core/libutils")
add_library(binder_utils_so SHARED)
aosp_sources(binder_utils_so "${_d}"
  FileMap.cpp JenkinsHash.cpp LightRefBase.cpp NativeHandle.cpp Printer.cpp
  StopWatch.cpp SystemClock.cpp Threads.cpp Timers.cpp Tokenizer.cpp misc.cpp
  Trace.cpp Looper.cpp
  # libutils_binder (whole_static_libs)
  binder/Errors.cpp binder/RefBase.cpp binder/SharedBuffer.cpp binder/String16.cpp
  binder/String8.cpp binder/StrongPointer.cpp binder/Unicode.cpp binder/VectorImpl.cpp)
target_include_directories(binder_utils_so PUBLIC "${_d}/include" "${_d}/binder/include"
  "${AOSP}/system/core/libsystem/include")
target_compile_definitions(binder_utils_so PRIVATE
  ANDROID_UTILS_REF_BASE_DISABLE_IMPLICIT_CONSTRUCTION)
target_compile_options(binder_utils_so PRIVATE -fvisibility=protected)
target_link_libraries(binder_utils_so PUBLIC binder_defaults)
target_link_libraries(binder_utils_so PRIVATE binder_base_so binder_cutils_so binder_log_so
  binder_vndksupport_so)
target_include_directories(binder_utils_so PUBLIC
  "${AOSP}/system/core/libcutils/include" "${AOSP}/system/logging/liblog/include"
  "${AOSP}/system/libbase/include")
set_target_properties(binder_utils_so PROPERTIES OUTPUT_NAME libutils.so)
guest_shared(binder_utils_so libutils.so)

# liblog's headers without its static library (for code linking liblog.so).
add_library(liblog_headers_guest INTERFACE)
target_include_directories(liblog_headers_guest INTERFACE "${AOSP}/system/logging/liblog/include")

# --- generated sources of libbinder --------------------------------------------
binder_aconfig(binder_flags "${_b}/flags.aconfig")

# --- libbinder.so -------------------------------------------------------------
add_library(binder_so SHARED)
aosp_sources(binder_so "${_b}"
  # libbinder_common_defaults
  Binder.cpp BpBinder.cpp Debug.cpp FdTrigger.cpp IInterface.cpp IResultReceiver.cpp
  JavaBBinderBase.cpp Parcel.cpp ParcelFileDescriptor.cpp RecordedTransaction.cpp
  RpcSession.cpp RpcServer.cpp RpcState.cpp RpcTransportRaw.cpp Stability.cpp
  Status.cpp TextOutput.cpp Utils.cpp file.cpp
  # libbinder_android_defaults
  OS_android.cpp OS_unix_base.cpp PersistableBundle.cpp
  # libbinder_kernel_defaults
  BufferedTextOutput.cpp BackendUnifiedServiceManager.cpp IPCThreadState.cpp
  IServiceManager.cpp IServiceManagerFFI.cpp ProcessState.cpp Static.cpp
  IPermissionController.cpp PermissionCache.cpp PermissionController.cpp
  observer/BinderObserver.cpp observer/BinderObserverConfig.cpp
  observer/BinderStatsPusher.cpp
  # libbinder
  IMemory.cpp IShellCallback.cpp LazyServiceRegistrar.cpp MemoryBase.cpp
  MemoryDealer.cpp MemoryHeapBase.cpp ParcelableHolder.cpp)
aidl_cpp(binder_so "${_gen}/binder/h" "${_gen}/binder/src"
  INCLUDES "${_b}/aidl" "${_fn}/aidl/binder"
  FILES "${_b}/aidl"
    android/os/ConnectionInfo.aidl android/os/IClientCallback.aidl
    android/os/IServiceCallback.aidl android/os/IServiceManager.aidl
    android/os/Service.aidl android/os/ServiceWithMetadata.aidl
    android/os/ServiceDebugInfo.aidl android/os/IAccessor.aidl)
# :pcc_sandbox_manager_native_aidl and :android-os-binderstatsconsumer-aidl
# (frameworks/base/core/java).
aidl_cpp(binder_so "${_gen}/binder/h" "${_gen}/binder/src"
  INCLUDES "${_fb}/core/java" "${_fn}/aidl/binder"
  FILES "${_fb}/core/java"
    android/app/privatecompute/IPccSandboxManagerNative.aidl
    android/os/binder/IBinderStatsConsumerService.aidl
    android/os/binder/BinderCallsStats.aidl
    android/os/binder/BinderSpamStats.aidl
    android/os/binder/SingleSecondBinderStats.aidl)
target_include_directories(binder_so PUBLIC
  "${_b}/include" "${_b}/ndk/include_cpp" "${AOSP}/libnativehelper/include_jni")
target_include_directories(binder_so PRIVATE "${_b}/include_private" "${_b}")
target_compile_definitions(binder_so PRIVATE
  BINDER_WITH_KERNEL_IPC
  NO_LIBBINDER_BINDER_OBSERVER
  ANDROID_BASE_UNIQUE_FD_DISABLE_IMPLICIT_CONVERSION
  ANDROID_UTILS_REF_BASE_DISABLE_IMPLICIT_CONSTRUCTION
  BUILDING_LIBBINDER)
target_compile_options(binder_so PRIVATE -fvisibility=hidden)
target_link_libraries(binder_so PUBLIC binder_defaults binder_utils_so)
target_link_libraries(binder_so PRIVATE binder_flags libbase binder_cutils_so binder_log_so
  binder_vndksupport_so binder_apexsupport_so)
target_link_options(binder_so PRIVATE "-Wl,--version-script=${_b}/libbinder.map")
set_target_properties(binder_so PROPERTIES OUTPUT_NAME libbinder.so)
guest_shared(binder_so libbinder.so)

# --- libbinder_ndk.so (libs/binder/ndk) -------------------------------------
# libandroid_runtime_lazy (static): reaches libandroid_runtime's binder JNI
# with dlopen when a process has it.
set(_d "${_fn}/libs/android_runtime_lazy")
add_library(binder_android_runtime_lazy STATIC "${_d}/android_runtime_lazy.cpp")
target_include_directories(binder_android_runtime_lazy PUBLIC "${_d}/include")
target_link_libraries(binder_android_runtime_lazy PRIVATE binder_defaults binder_so
  binder_utils_so liblog_headers_guest)

set(_d "${_b}/ndk")
add_library(binder_ndk_so SHARED)
aosp_sources(binder_ndk_so "${_d}"
  ibinder.cpp libbinder.cpp parcel.cpp stability.cpp status.cpp
  ibinder_jni.cpp parcel_jni.cpp persistable_bundle.cpp process.cpp
  service_manager.cpp binder_rpc.cpp)
target_include_directories(binder_ndk_so PUBLIC
  "${_d}/include_cpp" "${_d}/include_ndk" "${_d}/include_platform")
target_compile_definitions(binder_ndk_so PRIVATE BINDER_WITH_KERNEL_IPC)
target_link_libraries(binder_ndk_so PUBLIC binder_defaults)
target_link_libraries(binder_ndk_so PRIVATE binder_android_runtime_lazy libbase binder_so
  binder_utils_so binder_log_so binder_flags)
target_link_options(binder_ndk_so PRIVATE "-Wl,--version-script=${_d}/libbinder_ndk.map.txt")
set_target_properties(binder_ndk_so PROPERTIES OUTPUT_NAME libbinder_ndk.so)
guest_shared(binder_ndk_so libbinder_ndk.so)

# --- servicemanager -------------------------------------------------------------
set(_d "${_fn}/cmds/servicemanager")
add_executable(servicemanager
  "${_d}/main.cpp" "${_d}/ServiceManager.cpp" "${_here}/servicemanager_access.cpp")
target_include_directories(servicemanager PRIVATE "${_d}")
target_compile_definitions(servicemanager PRIVATE
  VENDORSERVICEMANAGER=1 ANDROID_UTILS_REF_BASE_DISABLE_IMPLICIT_CONSTRUCTION)
target_link_libraries(servicemanager PRIVATE binder_defaults binder_so binder_utils_so
  binder_base_so binder_cutils_so binder_log_so)
guest_executable(servicemanager)

# --- test programs (aosp/guest/binder/tests) -------------------------------------
file(GLOB _tests "${_here}/tests/*.cpp")
foreach(s ${_tests})
  get_filename_component(n "${s}" NAME_WE)
  add_executable(${n} "${s}")
  target_link_libraries(${n} PRIVATE binder_defaults binder_so binder_utils_so binder_base_so
    binder_cutils_so binder_log_so)
  # Tests use exceptions-free C++ like platform code, with assertions on.
  target_compile_options(${n} PRIVATE -UNDEBUG)
  guest_executable(${n})
endforeach()

# --- Java Binder JNI (compile check) -------------------------------------------
# The Binder part of libandroid_runtime (frameworks/base/core/jni:
# android.os.Binder, BinderProxy, Parcel, ServiceManager natives), compiled
# against this libbinder to keep the two in step. Not shipped: the
# framework build links these files into libandroid_runtime.so.
# android_util_Binder.cpp includes binderthreadstate/CallerUtils.h, which
# calls hwbinder's IPCThreadState::selfOrNull (system/libhwbinder headers;
# libandroid_runtime needs libhwbinder or a replacement to link,
# docs/plan/phase2/binder.md).
set(_d "${_fb}/core/jni")
binder_aconfig(binder_jni_flags_core "${_d}/flags.aconfig")
binder_aconfig(binder_jni_flags_os "${_fb}/core/java/android/os/flags.aconfig")
add_library(binder_jni_check STATIC)
aosp_sources(binder_jni_check "${_d}"
  android_util_Binder.cpp android_os_Parcel.cpp android_os_ServiceManager.cpp android_os_ServiceManagerNative.cpp)
target_include_directories(binder_jni_check PRIVATE "${_d}" "${_d}/include"
  "${AOSP}/libnativehelper/include" "${AOSP}/libnativehelper/include_platform"
  "${AOSP}/libnativehelper/header_only_include"
  "${AOSP}/libnativehelper/include_platform_header_only"
  "${_fn}/libs/binderthreadstate/include" "${AOSP}/system/libhwbinder/include"
  "${_b}/include_private")
target_compile_definitions(binder_jni_check PRIVATE
  ANDROID_UTILS_REF_BASE_DISABLE_IMPLICIT_CONSTRUCTION)
target_link_libraries(binder_jni_check PRIVATE binder_defaults binder_so binder_utils_so
  binder_jni_flags_core binder_jni_flags_os liblog_headers_guest libbase)
