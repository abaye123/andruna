// stdlib of the guest libc shim (spike S8): number parsing, sorting,
// environment, process exit and atexit, temporary files.

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "host.h"

char** environ;
const char* __progname = "guest";
char* program_invocation_name = "guest";
char* program_invocation_short_name = "guest";

const char* getprogname(void) { return __progname; }
void setprogname(const char* n) { __progname = n; }

// Called by the loader once, before any initializer of the other objects.
void __andruna_libc_init(int argc, char** argv, char** envp) {
  (void)argc;
  environ = envp;
  if (argv && argv[0]) {
    program_invocation_name = argv[0];
    const char* s = strrchr(argv[0], '/');
    program_invocation_short_name = (char*)(s ? s + 1 : argv[0]);
    __progname = program_invocation_short_name;
  }
}

// ---------------------------------------------------------------------------
// Integers.

static int digit(int c) {
  if (c >= '0' && c <= '9') return c - '0';
  if (c >= 'a' && c <= 'z') return c - 'a' + 10;
  if (c >= 'A' && c <= 'Z') return c - 'A' + 10;
  return 99;
}

static int space(int c) { return c == ' ' || (c >= '\t' && c <= '\r'); }

// Parses an unsigned magnitude; sets *neg and *overflow.
static uint64_t parse(const char* s, char** end, int base, int* neg, int* overflow) {
  const char* p = s;
  *neg = 0;
  *overflow = 0;
  while (space((unsigned char)*p)) p++;
  if (*p == '+' || *p == '-') *neg = *p++ == '-';
  if ((base == 0 || base == 16) && p[0] == '0' && (p[1] == 'x' || p[1] == 'X') &&
      digit((unsigned char)p[2]) < 16) {
    p += 2;
    base = 16;
  } else if ((base == 0 || base == 2) && p[0] == '0' && (p[1] == 'b' || p[1] == 'B') &&
             digit((unsigned char)p[2]) < 2) {
    p += 2;
    base = 2;
  } else if (base == 0) {
    base = *p == '0' ? 8 : 10;
  }
  if (base < 2 || base > 36) {
    errno = EINVAL;
    if (end) *end = (char*)s;
    return 0;
  }
  uint64_t v = 0;
  const char* start = p;
  for (;; p++) {
    int d = digit((unsigned char)*p);
    if (d >= base) break;
    if (v > (UINT64_MAX - (uint64_t)d) / (uint64_t)base) *overflow = 1;
    v = v * (uint64_t)base + (uint64_t)d;
  }
  if (end) *end = (char*)(p == start ? s : p);
  return v;
}

long long strtoll(const char* restrict s, char** restrict end, int base) {
  int neg, of;
  uint64_t v = parse(s, end, base, &neg, &of);
  if (of || v > (uint64_t)LLONG_MAX + neg) {
    errno = ERANGE;
    return neg ? LLONG_MIN : LLONG_MAX;
  }
  return neg ? (long long)(0 - v) : (long long)v;
}

unsigned long long strtoull(const char* restrict s, char** restrict end, int base) {
  int neg, of;
  uint64_t v = parse(s, end, base, &neg, &of);
  if (of) {
    errno = ERANGE;
    return ULLONG_MAX;
  }
  return neg ? 0 - v : v;
}

long strtol(const char* restrict s, char** restrict end, int base) { return strtoll(s, end, base); }
unsigned long strtoul(const char* restrict s, char** restrict end, int base) {
  return strtoull(s, end, base);
}
intmax_t strtoimax(const char* restrict s, char** restrict end, int base) {
  return strtoll(s, end, base);
}
uintmax_t strtoumax(const char* restrict s, char** restrict end, int base) {
  return strtoull(s, end, base);
}

int atoi(const char* s) { return (int)strtol(s, NULL, 10); }
long atol(const char* s) { return strtol(s, NULL, 10); }
long long atoll(const char* s) { return strtoll(s, NULL, 10); }

// strtod and strtof come from the host (Windows C runtime). long double
// has more range than double on Android; the spike accepts the loss.
long double strtold(const char* restrict s, char** restrict end) { return strtod(s, end); }
double atof(const char* s) { return strtod(s, NULL); }

int abs(int v) { return v < 0 ? -v : v; }
long labs(long v) { return v < 0 ? -v : v; }
long long llabs(long long v) { return v < 0 ? -v : v; }
intmax_t imaxabs(intmax_t v) { return v < 0 ? -v : v; }
div_t div(int a, int b) { return (div_t){a / b, a % b}; }
ldiv_t ldiv(long a, long b) { return (ldiv_t){a / b, a % b}; }
lldiv_t lldiv(long long a, long long b) { return (lldiv_t){a / b, a % b}; }

// ---------------------------------------------------------------------------
// Sorting.

static void swap(char* a, char* b, size_t n) {
  while (n--) {
    char t = *a;
    *a++ = *b;
    *b++ = t;
  }
}

static void sort_r(char* base, size_t n, size_t sz, int (*cmp)(const void*, const void*, void*),
                   void* arg) {
  // Insertion sort for small ranges, quicksort otherwise.
  while (n > 8) {
    char* pivot = base + (n / 2) * sz;
    swap(pivot, base + (n - 1) * sz, sz);
    pivot = base + (n - 1) * sz;
    size_t store = 0;
    for (size_t i = 0; i < n - 1; i++) {
      if (cmp(base + i * sz, pivot, arg) < 0) {
        swap(base + i * sz, base + store * sz, sz);
        store++;
      }
    }
    swap(base + store * sz, pivot, sz);
    if (store < n - store - 1) {
      sort_r(base, store, sz, cmp, arg);
      base += (store + 1) * sz;
      n -= store + 1;
    } else {
      sort_r(base + (store + 1) * sz, n - store - 1, sz, cmp, arg);
      n = store;
    }
  }
  for (size_t i = 1; i < n; i++)
    for (size_t j = i; j > 0 && cmp(base + (j - 1) * sz, base + j * sz, arg) > 0; j--)
      swap(base + (j - 1) * sz, base + j * sz, sz);
}

static int call_cmp(const void* a, const void* b, void* f) {
  return ((int (*)(const void*, const void*))f)(a, b);
}

void qsort(void* base, size_t n, size_t sz, int (*cmp)(const void*, const void*)) {
  sort_r(base, n, sz, call_cmp, (void*)cmp);
}

void qsort_r(void* base, size_t n, size_t sz, int (*cmp)(const void*, const void*, void*),
             void* arg) {
  sort_r(base, n, sz, cmp, arg);
}

void* bsearch(const void* key, const void* base, size_t n, size_t sz,
              int (*cmp)(const void*, const void*)) {
  const char* b = base;
  while (n) {
    const char* m = b + (n / 2) * sz;
    int c = cmp(key, m);
    if (!c) return (void*)m;
    if (c > 0) {
      b = m + sz;
      n -= n / 2 + 1;
    } else {
      n /= 2;
    }
  }
  return NULL;
}

// ---------------------------------------------------------------------------
// Environment.

static size_t env_count(void) {
  size_t n = 0;
  if (environ)
    while (environ[n]) n++;
  return n;
}

static char** env_find(const char* name, size_t len) {
  if (!environ) return NULL;
  for (char** e = environ; *e; e++)
    if (strncmp(*e, name, len) == 0 && (*e)[len] == '=') return e;
  return NULL;
}

char* getenv(const char* name) {
  char** e = env_find(name, strlen(name));
  return e ? *e + strlen(name) + 1 : NULL;
}
char* secure_getenv(const char* name) { return getenv(name); }

static int env_owned;

static int env_put(char* entry, size_t namelen, int overwrite) {
  char** e = env_find(entry, namelen);
  if (e) {
    if (overwrite) *e = entry;
    return 0;
  }
  size_t n = env_count();
  char** ne = malloc((n + 2) * sizeof(char*));
  if (!ne) return -1;
  if (n) memcpy(ne, environ, n * sizeof(char*));
  ne[n] = entry;
  ne[n + 1] = NULL;
  if (env_owned) free(environ);
  environ = ne;
  env_owned = 1;
  return 0;
}

int setenv(const char* name, const char* value, int overwrite) {
  size_t nl = strlen(name), vl = strlen(value);
  if (!nl || strchr(name, '=')) {
    errno = EINVAL;
    return -1;
  }
  if (!overwrite && env_find(name, nl)) return 0;
  char* s = malloc(nl + vl + 2);
  if (!s) return -1;
  memcpy(s, name, nl);
  s[nl] = '=';
  memcpy(s + nl + 1, value, vl + 1);
  return env_put(s, nl, 1);
}

int putenv(char* s) {
  char* eq = strchr(s, '=');
  if (!eq) return unsetenv(s);
  return env_put(s, (size_t)(eq - s), 1);
}

int unsetenv(const char* name) {
  size_t nl = strlen(name);
  char** e;
  while ((e = env_find(name, nl)))
    do e[0] = e[1];
    while (*e++);
  return 0;
}

int clearenv(void) {
  environ = NULL;
  return 0;
}

// ---------------------------------------------------------------------------
// Random numbers.

static uint64_t rng_state = 0x853c49e6748fea9bull;
static uint32_t next_rand(uint64_t* s) {
  *s = *s * 6364136223846793005ull + 1442695040888963407ull;
  return (uint32_t)(*s >> 33);
}
int rand(void) { return (int)(next_rand(&rng_state) & RAND_MAX); }
void srand(unsigned s) { rng_state = s; }
long random(void) { return rand(); }
void srandom(unsigned s) { srand(s); }
int rand_r(unsigned* s) {
  uint64_t st = *s;
  int r = (int)(next_rand(&st) & RAND_MAX);
  *s = (unsigned)st;
  return r;
}
long lrand48(void) { return (long)(next_rand(&rng_state) & 0x7fffffff); }
long mrand48(void) { return (long)(int32_t)next_rand(&rng_state); }
double drand48(void) { return next_rand(&rng_state) / 4294967296.0; }
void srand48(long s) { rng_state = (uint64_t)s; }

// ---------------------------------------------------------------------------
// Temporary files.

static int fill_template(char* t, int suffix) {
  size_t l = strlen(t);
  if (l < 6 + (size_t)suffix || memcmp(t + l - suffix - 6, "XXXXXX", 6) != 0) {
    errno = EINVAL;
    return -1;
  }
  static const char chars[] = "abcdefghijklmnopqrstuvwxyz0123456789";
  uint32_t r[6];
  arc4random_buf(r, sizeof r);
  for (int i = 0; i < 6; i++) t[l - suffix - 6 + i] = chars[r[i] % 36];
  return 0;
}

int mkostemps(char* t, int suffix, int flags) {
  for (int tries = 0; tries < 100; tries++) {
    if (fill_template(t, suffix)) return -1;
    int fd = open(t, O_RDWR | O_CREAT | O_EXCL | flags, 0600);
    if (fd >= 0 || errno != EEXIST) return fd;
  }
  errno = EEXIST;
  return -1;
}
int mkstemp(char* t) { return mkostemps(t, 0, 0); }
int mkstemp64(char* t) { return mkostemps(t, 0, 0); }
int mkstemps(char* t, int suffix) { return mkostemps(t, suffix, 0); }
int mkostemp(char* t, int flags) { return mkostemps(t, 0, flags); }

char* mkdtemp(char* t) {
  for (int tries = 0; tries < 100; tries++) {
    if (fill_template(t, 0)) return NULL;
    if (mkdir(t, 0700) == 0) return t;
    if (errno != EEXIST) return NULL;
  }
  return NULL;
}

char* mktemp(char* t) {
  if (fill_template(t, 0)) t[0] = 0;
  return t;
}

// ---------------------------------------------------------------------------
// atexit and exit.

struct exit_fn {
  void (*fn)(void*);
  void* arg;
  void* dso;
};
static struct exit_fn* exit_fns;
static size_t exit_count, exit_cap;

int __cxa_atexit(void (*fn)(void*), void* arg, void* dso) {
  if (exit_count == exit_cap) {
    size_t c = exit_cap ? exit_cap * 2 : 64;
    struct exit_fn* n = realloc(exit_fns, c * sizeof *n);
    if (!n) return -1;
    exit_fns = n;
    exit_cap = c;
  }
  exit_fns[exit_count++] = (struct exit_fn){fn, arg, dso};
  return 0;
}

static void call_noarg(void* f) { ((void (*)(void))f)(); }
int atexit(void (*fn)(void)) { return __cxa_atexit(call_noarg, (void*)fn, NULL); }

void __cxa_finalize(void* dso) {
  for (size_t i = exit_count; i-- > 0;) {
    struct exit_fn e = exit_fns[i];
    if (!e.fn || (dso && e.dso != dso)) continue;
    exit_fns[i].fn = NULL;
    e.fn(e.arg);
  }
}

void exit(int status) {
  __andruna_libc_thread_exit();
  __cxa_finalize(NULL);
  fflush(NULL);
  _exit(status);
}

void quick_exit(int status) { _exit(status); }
int at_quick_exit(void (*fn)(void)) {
  (void)fn;
  return 0;
}
void _Exit(int status) { _exit(status); }

void __stack_chk_fail(void) {
  static const char m[] = "stack corruption detected (-fstack-protector)\n";
  write(2, m, sizeof m - 1);
  abort();
}

void __assert2(const char* file, int line, const char* func, const char* expr) {
  fprintf(stderr, "%s:%d: %s: assertion \"%s\" failed\n", file, line, func, expr);
  abort();
}
void __assert(const char* file, int line, const char* expr) {
  fprintf(stderr, "%s:%d: assertion \"%s\" failed\n", file, line, expr);
  abort();
}
