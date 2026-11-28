# Stage framework: the framework's native libraries built so far, as Android
# ELF shared objects, and andruna_app_process (docs/plan/phase2/
# framework-build.md). They link against the ART root's libraries
# (ANDRUNA_ART_ROOT, made by the art stage): liblog, libbase,
# libnativehelper and libc++ are the ones already in the process.
#
#   libutils.so             system/core/libutils (with libvndksupport)
#   libandroidfw.so         resources (AssetManager2, ApkAssets, ResTable)
#   libandroid_runtime.so   AndroidRuntime and the JNI natives listed in
#                           aosp/modules/framework_natives.cmake, with
#                           libcutils and server_configurable_flags folded in
#   andruna_app_process     AndroidRuntime::start() for one main class

set(ANDRUNA_ART_ROOT "" CACHE PATH "The ART root of the art stage (out/guest/<arch>/art-root)")
if(NOT EXISTS "${ANDRUNA_ART_ROOT}/lib64/libnativehelper.so")
  message(FATAL_ERROR "ANDRUNA_ART_ROOT (${ANDRUNA_ART_ROOT}) has no lib64/libnativehelper.so; build the art stage first")
endif()

foreach(n log base nativehelper)
  add_library(art_root_${n} SHARED IMPORTED)
  set_target_properties(art_root_${n} PROPERTIES IMPORTED_LOCATION "${ANDRUNA_ART_ROOT}/lib64/lib${n}.so")
endforeach()

set(_empty "${CMAKE_CURRENT_BINARY_DIR}/empty.c")
file(WRITE "${_empty}" "")

# A shared object of whole static libraries (after WHOLE), linked like
# Soong links them, against the shared objects before WHOLE.
function(fw_so target soname)
  add_library(${target} SHARED "${_empty}")
  set(shared)
  set(wl)
  set(whole OFF)
  foreach(a ${ARGN})
    if(a STREQUAL "WHOLE")
      set(whole ON)
    elseif(whole)
      list(APPEND wl "$<LINK_LIBRARY:WHOLE_ARCHIVE,${a}>")
    else()
      list(APPEND shared ${a})
    endif()
  endforeach()
  # The shared objects first, so that the static libbase, liblog and
  # libandroidfw the AOSP targets carry are not linked in again.
  target_link_libraries(${target} PRIVATE art_root_log art_root_base art_root_nativehelper
    ${shared} ${wl})
  set_target_properties(${target} PROPERTIES OUTPUT_NAME "${soname}")
  guest_shared(${target} ${soname})
  target_link_options(${target} PRIVATE -Wl,--no-undefined)
endfunction()

fw_so(utils_so libutils.so "${SYSLIB}/libdl_android.so" WHOLE libutils_framework)
target_link_libraries(utils_so PRIVATE libcutils)

fw_so(androidfw_so libandroidfw.so utils_so WHOLE libandroidfw libincfs_utils)
target_link_libraries(androidfw_so PRIVATE libziparchive libz libcutils)

fw_so(android_runtime_so libandroid_runtime.so utils_so androidfw_so "${SYSLIB}/libdl_android.so"
  WHOLE libandroid_runtime_jni libserver_configurable_flags)
target_link_libraries(android_runtime_so PRIVATE libcutils)

add_executable(andruna_app_process "${CMAKE_CURRENT_SOURCE_DIR}/andruna_app_process.cpp")
# Only the headers of the static libraries: their code is in the .so files.
foreach(lib libandroid_runtime_jni libutils_framework libcutils libbase liblog)
  target_include_directories(andruna_app_process PRIVATE
    $<TARGET_PROPERTY:${lib},INTERFACE_INCLUDE_DIRECTORIES>)
endforeach()
target_compile_definitions(andruna_app_process PRIVATE ANDROID_UTILS_REF_BASE_DISABLE_IMPLICIT_CONSTRUCTION)
target_link_libraries(andruna_app_process PRIVATE aosp_android android_runtime_so androidfw_so
  utils_so art_root_log art_root_base art_root_nativehelper)
guest_executable(andruna_app_process)

set(FW_OUT "${CMAKE_BINARY_DIR}/out")
add_custom_target(framework_install ALL
  COMMAND "${CMAKE_COMMAND}" -E make_directory "${FW_OUT}"
  COMMAND "${CMAKE_COMMAND}" -E copy_if_different
    $<TARGET_FILE:utils_so> $<TARGET_FILE:androidfw_so> $<TARGET_FILE:android_runtime_so>
    $<TARGET_FILE:andruna_app_process> "${FW_OUT}"
  DEPENDS utils_so androidfw_so android_runtime_so andruna_app_process
  VERBATIM)
