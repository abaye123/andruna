/*
 * libapexsupport for Andruna: AApexSupport_loadLibrary.
 *
 * On Android it opens a library from the linker namespace of an APEX.
 * libbinder calls it only for a passthrough HAL that a VINTF manifest
 * declares as updatable through an APEX. Andruna has no VINTF manifests
 * (servicemanager declares no HAL, ADR 0041), so no such HAL exists and
 * the library cannot be found: the call fails with null, as Android's
 * does for a library the APEX does not have.
 */

#include <android/apexsupport.h>

#include <errno.h>
#include <log/log.h>

extern "C" void* AApexSupport_loadLibrary(const char* name, const char* apexName, int /*flag*/) {
    ALOGW("AApexSupport_loadLibrary(%s, %s): no APEX provides HALs on Andruna",
          name ? name : "(null)", apexName ? apexName : "(null)");
    errno = ENOENT;
    return nullptr;
}
