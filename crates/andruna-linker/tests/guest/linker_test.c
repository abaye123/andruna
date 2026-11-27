// LINK: -lltdep -ldl
// Exercises the loader through real bionic: stdio, malloc, pthread, TLS in
// the executable and in libraries (static and dynamic), dlopen, dlsym,
// dladdr, dlclose, dl_iterate_phdr, IFUNC, constructors, exit status.
#include <dlfcn.h>
#include <link.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern __thread int ltdep_counter;
extern int ltdep_init_order;
int ltdep_bump(void);
int ltdep_pick(void);

static __thread int exe_tls = 3;
static int failures = 0;

#define CHECK(c)                                              \
  do {                                                        \
    if (!(c)) {                                               \
      printf("FAIL %s:%d %s\n", __FILE__, __LINE__, #c);      \
      failures++;                                             \
    }                                                         \
  } while (0)

static void* thread_main(void* arg) {
  CHECK(exe_tls == 3);
  exe_tls = 100;
  CHECK(ltdep_counter == 40);
  ltdep_bump();
  return (void*)(long)(exe_tls + ltdep_counter + (long)arg);
}

static int phdr_cb(struct dl_phdr_info* info, size_t size, void* data) {
  (void)size;
  int* n = (int*)data;
  (*n)++;
  if (info->dlpi_name && strstr(info->dlpi_name, "libltdep.so")) *n += 1000;
  return 0;
}

int main(int argc, char** argv) {
  (void)argc;
  (void)argv;
  printf("hello from bionic\n");
  CHECK(ltdep_init_order == 1);
  CHECK(exe_tls == 3);
  CHECK(ltdep_bump() == 41);
  CHECK(ltdep_pick() == 7);

  char* p = malloc(1 << 20);
  CHECK(p != NULL);
  memset(p, 0xab, 1 << 20);
  free(p);
  char buf[64];
  snprintf(buf, sizeof(buf), "%d %.2f %s", 42, 3.25, "x");
  CHECK(strcmp(buf, "42 3.25 x") == 0);

  pthread_t t;
  void* r = NULL;
  CHECK(pthread_create(&t, NULL, thread_main, (void*)1L) == 0);
  CHECK(pthread_join(t, &r) == 0);
  CHECK((long)r == 100 + 41 + 1);
  CHECK(exe_tls == 3);
  CHECK(ltdep_counter == 41);

  void* h = dlopen("libltplugin.so", RTLD_NOW);
  CHECK(h != NULL);
  if (h == NULL) printf("dlerror: %s\n", dlerror());
  if (h) {
    int* constructed = dlsym(h, "plugin_constructed");
    CHECK(constructed && *constructed == 1);
    long (*f)(void) = (long (*)(void))dlsym(h, "plugin_tls");
    CHECK(f && f() == 6 && f() == 7);
    Dl_info info;
    CHECK(dladdr((void*)f, &info) != 0);
    CHECK(info.dli_sname && strcmp(info.dli_sname, "plugin_tls") == 0);
    CHECK(info.dli_fname && strstr(info.dli_fname, "libltplugin.so"));
    CHECK(dlsym(h, "no_such_symbol") == NULL);
    const char* e = dlerror();
    CHECK(e && strstr(e, "no_such_symbol"));
    CHECK(dlclose(h) == 0);
  }
  CHECK(dlopen("libdoes_not_exist.so", RTLD_NOW) == NULL);
  CHECK(dlsym(RTLD_DEFAULT, "ltdep_bump") == (void*)ltdep_bump);

  int n = 0;
  dl_iterate_phdr(phdr_cb, &n);
  CHECK(n > 1000 + 3);

  printf("linker_test %s (%d failures)\n", failures ? "FAILED" : "passed", failures);
  return failures ? 1 : 0;
}
