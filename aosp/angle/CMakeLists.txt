# ANGLE (AOSP external/angle) as Windows DLLs with the Direct3D 11 back end
# only: libGLESv2.dll (EGL and OpenGL ES 1 to 3.x entry points, libANGLE,
# the shader translator with HLSL output, the D3D11 renderer) and libEGL.dll
# (the EGL entry points, forwarding to libGLESv2.dll in its own directory).
# See docs/decisions/0029-angle-build-cmake-llvm-mingw.md.
#
# Configured by `cargo xtask angle-build` with
# aosp/cmake/toolchain-llvm-mingw.cmake (llvm-mingw clang, x64 or arm64).
# The source lists come from ANGLE's own .gni files (gni_sources.py); the
# defines follow BUILD.gn for a Windows (win32, not UWP) build with
# angle_enable_d3d11 and no other back end.

cmake_minimum_required(VERSION 3.28)
project(andruna_angle LANGUAGES C CXX)

get_filename_component(ANDRUNA_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../.." ABSOLUTE)
set(ANDRUNA_AOSP_DIR "${ANDRUNA_ROOT}/third_party/aosp" CACHE PATH "AOSP sources")
set(ANGLE "${ANDRUNA_AOSP_DIR}/external/angle")
set(ZLIB "${ANDRUNA_AOSP_DIR}/external/zlib")
foreach(d "${ANGLE}/src/libANGLE" "${ZLIB}/google")
  if(NOT EXISTS "${d}")
    message(FATAL_ERROR "${d} is missing: cargo xtask fetch --project external/angle --project external/zlib")
  endif()
endforeach()

find_package(Python3 REQUIRED COMPONENTS Interpreter)

set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_CXX_SCAN_FOR_MODULES OFF)
set(CMAKE_POSITION_INDEPENDENT_CODE OFF)

# --- Source lists from the .gni files --------------------------------------

set(_lists "${CMAKE_CURRENT_BINARY_DIR}/angle_sources.cmake")
execute_process(
  COMMAND "${Python3_EXECUTABLE}" -I "${CMAKE_CURRENT_SOURCE_DIR}/gni_sources.py" "${ANGLE}" "${_lists}"
  COMMAND_ERROR_IS_FATAL ANY)
include("${_lists}")
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS
  "${ANGLE}/src/libGLESv2.gni" "${ANGLE}/src/compiler.gni"
  "${ANGLE}/src/libANGLE/renderer/d3d/d3d_backend.gni"
  "${CMAKE_CURRENT_SOURCE_DIR}/gni_sources.py")

function(angle_paths out)
  set(r)
  foreach(f ${ARGN})
    list(APPEND r "${ANGLE}/${f}")
  endforeach()
  set(${out} ${r} PARENT_SCOPE)
endfunction()

# --- Generated headers -------------------------------------------------------

set(GEN "${CMAKE_CURRENT_BINARY_DIR}/gen/angle")
file(MAKE_DIRECTORY "${GEN}")
# angle_commit.h ships at the root of AOSP's copy (BUILD.gn generates it).
configure_file("${ANGLE}/angle_commit.h" "${GEN}/angle_commit.h" COPYONLY)

# ANGLEShaderProgramVersion.h: a hash over the code that defines the program
# binary format, as BUILD.gn's angle_program_version_id action computes it.
set(_serialize ${ANGLE_LIBANGLE_SOURCES} ${ANGLE_ANGLE_TRANSLATOR_SOURCES}
  ${ANGLE_D3D_SHARED_SOURCES} ${ANGLE_D3D11_BACKEND_SOURCES})
list(JOIN _serialize " " _serialize_txt)
file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/program_version.rsp" "${_serialize_txt}")
execute_process(
  COMMAND "${Python3_EXECUTABLE}" -I "${ANGLE}/src/program_serialize_data_version.py"
    "${GEN}/ANGLEShaderProgramVersion.h" "${CMAKE_CURRENT_BINARY_DIR}/program_version.rsp"
  WORKING_DIRECTORY "${ANGLE}"
  COMMAND_ERROR_IS_FATAL ANY)

# --- Common flags --------------------------------------------------------------

add_library(angle_flags INTERFACE)
target_include_directories(angle_flags INTERFACE
  "${ANGLE}/include" "${ANGLE}/src" "${ANGLE}/src/common/base"
  "${ANGLE}/src/common/third_party/xxhash" "${GEN}"
  "${ZLIB}" "${ZLIB}/google")
target_compile_definitions(angle_flags INTERFACE
  # Windows build environment (as Chromium's build config sets it).
  UNICODE _UNICODE NOMINMAX WIN32_LEAN_AND_MEAN
  _WIN32_WINNT=0x0A00 WINVER=0x0A00 NTDDI_VERSION=NTDDI_WIN10_RS4
  _CRT_SECURE_NO_DEPRECATE _SCL_SECURE_NO_DEPRECATE
  # internal_config for is_win, angle_enable_share_context_lock and
  # angle_enable_context_mutex (the Windows defaults).
  ANGLE_IS_WIN
  ANGLE_ENABLE_SHARE_CONTEXT_LOCK=1
  ANGLE_ENABLE_CONTEXT_MUTEX=1
  ANGLE_OUTSIDE_WEBKIT
  # angle_backend_config and the D3D11 back end.
  ANGLE_ENABLE_D3D11
  ANGLE_ENABLE_HLSL
  # Frame capture is not built (angle_frame_capture_disabled).
  ANGLE_CAPTURE_ENABLED=0
  # libANGLE_config outside Android.
  ANGLE_PLATFORM_EXPORT=
  # zlib (compression_utils_portable) as part of this build.
  ZLIB_CONST CHROMIUM_ZLIB_NO_CASTAGNOLI
  ANDRUNA)
target_compile_options(angle_flags INTERFACE
  -fno-exceptions -fno-rtti
  -fno-strict-aliasing
  -Wno-everything)

# --- zlib and compression_utils_portable --------------------------------------

add_library(angle_zlib STATIC
  "${ZLIB}/adler32.c" "${ZLIB}/compress.c" "${ZLIB}/cpu_features.c" "${ZLIB}/crc32.c"
  "${ZLIB}/deflate.c" "${ZLIB}/infback.c" "${ZLIB}/inffast.c" "${ZLIB}/inflate.c"
  "${ZLIB}/inftrees.c" "${ZLIB}/trees.c" "${ZLIB}/uncompr.c" "${ZLIB}/zutil.c"
  "${ZLIB}/google/compression_utils_portable.cc")
target_link_libraries(angle_zlib PRIVATE angle_flags)

# --- libGLESv2.dll ---------------------------------------------------------------

set(_d3d_sources ${ANGLE_D3D_SHARED_SOURCES} ${ANGLE_D3D11_BACKEND_SOURCES}
  # BUILD.gn adds these under `!angle_is_winuwp` and the D3D format tables.
  src/libANGLE/renderer/d3d/d3d11/win32/NativeWindow11Win32.cpp
  src/libANGLE/renderer/dxgi_format_map_autogen.cpp
  src/libANGLE/renderer/dxgi_support_table_autogen.cpp
  src/libANGLE/renderer/d3d_format.cpp)
list(REMOVE_DUPLICATES _d3d_sources)

set(_glesv2 ${ANGLE_LIBANGLE_COMMON_SOURCES}
  # libangle_common_sources additions for is_win (not UWP).
  src/common/backtrace_utils_noop.cpp
  src/common/system_utils_win.cpp
  src/common/system_utils_win32.cpp
  ${ANGLE_LIBANGLE_COMMON_SHADER_STATE_SOURCES}
  ${ANGLE_LIBANGLE_IMAGE_UTIL_SOURCES}
  src/image_util/AstcDecompressorNoOp.cpp
  ${ANGLE_LIBANGLE_GPU_INFO_UTIL_SOURCES}
  ${ANGLE_LIBANGLE_GPU_INFO_UTIL_WIN_SOURCES}
  ${ANGLE_ANGLE_PREPROCESSOR_SOURCES}
  ${ANGLE_ANGLE_TRANSLATOR_SOURCES}
  ${ANGLE_ANGLE_TRANSLATOR_ESSL_SYMBOL_TABLE_SOURCES}
  ${ANGLE_ANGLE_TRANSLATOR_HLSL_SOURCES}
  src/common/angle_version_info.cpp
  ${ANGLE_LIBANGLE_SOURCES}
  ${_d3d_sources}
  # angle_frame_capture_mock
  src/libANGLE/capture/FrameCapture_mock.cpp
  src/libANGLE/capture/serialize_mock.cpp
  ${ANGLE_LIBGLESV2_ENTRY_POINT_SOURCES}
  ${ANGLE_LIBGLESV2_SOURCES})
list(REMOVE_DUPLICATES _glesv2)
angle_paths(_glesv2 ${_glesv2})

add_library(angle_xxhash OBJECT ${ANGLE}/src/common/third_party/xxhash/xxhash.c)
target_link_libraries(angle_xxhash PRIVATE angle_flags)

add_library(libGLESv2 SHARED ${_glesv2} "${ANGLE}/src/libGLESv2/libGLESv2_autogen.def")
target_link_libraries(libGLESv2 PRIVATE angle_flags angle_xxhash angle_zlib
  dxguid dxgi setupapi gdi32 user32 synchronization)
target_compile_definitions(libGLESv2 PRIVATE
  LIBANGLE_IMPLEMENTATION LIBGLESV2_IMPLEMENTATION
  "GL_APICALL=" "GL_API="
  # gl_prototypes (the entry point sources define the GL names).
  GL_GLEXT_PROTOTYPES EGL_EGLEXT_PROTOTYPES
  "ANGLE_PRELOADED_D3DCOMPILER_MODULE_NAMES={ \"d3dcompiler_47.dll\", \"d3dcompiler_46.dll\", \"d3dcompiler_43.dll\" }")
set_target_properties(libGLESv2 PROPERTIES PREFIX "" OUTPUT_NAME libGLESv2)

# --- libEGL.dll --------------------------------------------------------------------

set(_egl ${ANGLE_LIBEGL_SOURCES} src/libEGL/egl_loader_autogen.cpp
  ${ANGLE_LIBANGLE_COMMON_SOURCES}
  src/common/backtrace_utils_noop.cpp
  src/common/system_utils_win.cpp
  src/common/system_utils_win32.cpp)
list(REMOVE_DUPLICATES _egl)
angle_paths(_egl ${_egl})

add_library(libEGL SHARED ${_egl} "${ANGLE}/src/libEGL/libEGL_autogen.def")
target_link_libraries(libEGL PRIVATE angle_flags angle_xxhash synchronization)
target_compile_definitions(libEGL PRIVATE
  LIBEGL_IMPLEMENTATION ANGLE_USE_EGL_LOADER "EGLAPI="
  "ANGLE_DISPATCH_LIBRARY=\"libGLESv2\""
  "ANGLE_EGL_LIBRARY_NAME=\"libEGL\"" "ANGLE_GLESV2_LIBRARY_NAME=\"libGLESv2\"")
target_compile_definitions(libEGL PRIVATE GL_GLEXT_PROTOTYPES EGL_EGLEXT_PROTOTYPES)
set_target_properties(libEGL PROPERTIES PREFIX "" OUTPUT_NAME libEGL)

# Both DLLs depend only on system DLLs: libc++, libunwind and the mingw-w64
# runtime are linked statically.
foreach(t libGLESv2 libEGL)
  target_link_options(${t} PRIVATE -static -Wl,--no-insert-timestamp)
endforeach()

install(TARGETS libGLESv2 libEGL RUNTIME DESTINATION bin)
