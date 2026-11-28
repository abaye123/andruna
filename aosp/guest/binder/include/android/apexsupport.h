/*
 * The one function of Android's libapexsupport that libbinder calls
 * (openDeclaredPassthroughHal, for a HAL declared as updatable through an
 * APEX). Implemented by aosp/guest/binder/apexsupport.cpp.
 */

#pragma once

#include <sys/cdefs.h>

__BEGIN_DECLS

void* AApexSupport_loadLibrary(const char* name, const char* apexName, int flag);

__END_DECLS
