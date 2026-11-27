// A dependency of linker_test: TLS, a constructor and an IFUNC.

__thread int ltdep_counter = 40;
int ltdep_init_order = 0;

__attribute__((constructor)) static void ltdep_ctor(void) { ltdep_init_order = 1; }

int ltdep_bump(void) { return ++ltdep_counter; }

static int impl_a(void) { return 7; }
static void* resolve_pick(void) { return (void*)impl_a; }
int ltdep_pick(void) __attribute__((ifunc("resolve_pick")));
