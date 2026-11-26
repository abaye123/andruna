// Placeholder for the bridge ld-android.so (replaced by the andruna-linker
// work): defines the loader's shared globals so the build target links.

#include "private/bionic_globals.h"

static libc_shared_globals g_shared_globals;

extern "C" libc_shared_globals* __loader_shared_globals() {
  return &g_shared_globals;
}

extern "C" void __andruna_bridge_placeholder() {}
