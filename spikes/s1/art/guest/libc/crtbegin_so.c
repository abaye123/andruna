// Linked first into every guest shared object, as bionic's crtbegin_so.o:
// the object's own `__dso_handle`, which C++ static destructors pass to
// __cxa_atexit.
__attribute__((visibility("hidden"))) void* __dso_handle = &__dso_handle;
