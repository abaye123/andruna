# external/boringssl: libcrypto_static and libssl (Android.bp), the
# cryptography of Conscrypt (libjavacrypto) and of libcore's NativeBN
# (Android ELF build only).
#
# Modules:
#   libcrypto_static  libcrypto_sources + libcrypto_bcm_sources, without
#                     BORINGSSL_FIPS (no self tests, no module hash)
#   libssl            libssl_sources
#
# Both are static, -fPIC; the guest build links libcrypto_static into
# libjavacore.so and libopenjdk.so (as Android does, "for bignums only"),
# and links libcrypto.so and libssl.so (the shared objects libjavacrypto.so
# needs) from the same archives. Android's shared libcrypto is the FIPS
# build (bcm_object with its integrity hash injected after the link); the
# Andruna build uses the non-FIPS sources for it too (ADR 0021).
#
# The source lists are BoringSSL's generated android-sources.cmake. Its
# assembly files are built for every architecture: each one is guarded by
# the architecture and object format it targets, as in Soong's build.

set(_bssl "${AOSP}/external/boringssl")
set(BORINGSSL_ROOT "${_bssl}/")
include("${_bssl}/android-sources.cmake")

# boringssl_flags_common + boringssl_flags + boringssl_defaults.
add_library(boringssl_defaults INTERFACE)
target_compile_definitions(boringssl_defaults INTERFACE
  BORINGSSL_SHARED_LIBRARY OPENSSL_SMALL BORINGSSL_ANDROID_SYSTEM
  BORINGSSL_IMPLEMENTATION)
target_compile_options(boringssl_defaults INTERFACE -fvisibility=hidden
  -Wno-unused-parameter)
target_include_directories(boringssl_defaults INTERFACE "${_bssl}/src/include")
target_link_libraries(boringssl_defaults INTERFACE aosp_android)

# The exported headers (export_include_dirs).
add_library(boringssl_headers INTERFACE)
target_include_directories(boringssl_headers INTERFACE "${_bssl}/src/include")

# --- libcrypto_static -------------------------------------------------------
add_library(libcrypto_static STATIC ${crypto_sources} ${crypto_sources_asm})
target_link_libraries(libcrypto_static PRIVATE boringssl_defaults)
target_link_libraries(libcrypto_static PUBLIC boringssl_headers)

# --- libssl -----------------------------------------------------------------
add_library(libssl STATIC ${ssl_sources})
target_link_libraries(libssl PRIVATE boringssl_defaults)
target_link_libraries(libssl PUBLIC boringssl_headers)
