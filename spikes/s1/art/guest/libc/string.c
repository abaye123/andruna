// String and memory functions of the guest libc shim (spike S8).
//
// Plain C, compiled with -ffreestanding so the compiler does not turn these
// loops back into calls to themselves. Production would use bionic's own
// optimized implementations.

#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <strings.h>
#include <wchar.h>

void* malloc(size_t);

void* memcpy(void* restrict d, const void* restrict s, size_t n) {
  unsigned char* dp = d;
  const unsigned char* sp = s;
  // Word copies when both are aligned; enough for a spike.
  if ((((uintptr_t)dp | (uintptr_t)sp) & 7) == 0) {
    for (; n >= 8; n -= 8, dp += 8, sp += 8) *(uint64_t*)dp = *(const uint64_t*)sp;
  }
  while (n--) *dp++ = *sp++;
  return d;
}

void* memmove(void* d, const void* s, size_t n) {
  unsigned char* dp = d;
  const unsigned char* sp = s;
  if (dp == sp || n == 0) return d;
  if (dp < sp || dp >= sp + n) return memcpy(d, s, n);
  dp += n;
  sp += n;
  while (n--) *--dp = *--sp;
  return d;
}

void* memset(void* d, int c, size_t n) {
  unsigned char* dp = d;
  if (((uintptr_t)dp & 7) == 0 && n >= 8) {
    uint64_t v = (unsigned char)c * 0x0101010101010101ull;
    for (; n >= 8; n -= 8, dp += 8) *(uint64_t*)dp = v;
  }
  while (n--) *dp++ = (unsigned char)c;
  return d;
}

void* memset_explicit(void* d, int c, size_t n) { return memset(d, c, n); }
void explicit_bzero(void* d, size_t n) { memset(d, 0, n); }
int memcmp(const void* a, const void* b, size_t n) {
  const unsigned char *x = a, *y = b;
  for (; n; n--, x++, y++)
    if (*x != *y) return *x - *y;
  return 0;
}

int bcmp(const void* a, const void* b, size_t n) { return memcmp(a, b, n); }

void* memchr(const void* s, int c, size_t n) {
  const unsigned char* p = s;
  for (; n; n--, p++)
    if (*p == (unsigned char)c) return (void*)p;
  return NULL;
}

void* memrchr(const void* s, int c, size_t n) {
  const unsigned char* p = (const unsigned char*)s + n;
  while (n--)
    if (*--p == (unsigned char)c) return (void*)p;
  return NULL;
}

void* memmem(const void* h, size_t hl, const void* n, size_t nl) {
  const unsigned char* hp = h;
  if (nl == 0) return (void*)h;
  for (size_t i = 0; i + nl <= hl; i++)
    if (hp[i] == *(const unsigned char*)n && memcmp(hp + i, n, nl) == 0) return (void*)(hp + i);
  return NULL;
}

void* mempcpy(void* d, const void* s, size_t n) { return (char*)memcpy(d, s, n) + n; }

size_t strlen(const char* s) {
  const char* p = s;
  while (*p) p++;
  return (size_t)(p - s);
}

size_t strnlen(const char* s, size_t n) {
  size_t i = 0;
  while (i < n && s[i]) i++;
  return i;
}

char* strcpy(char* restrict d, const char* restrict s) {
  char* r = d;
  while ((*d++ = *s++)) {
  }
  return r;
}

char* stpcpy(char* restrict d, const char* restrict s) {
  while ((*d = *s++)) d++;
  return d;
}

char* strncpy(char* restrict d, const char* restrict s, size_t n) {
  size_t i = 0;
  for (; i < n && s[i]; i++) d[i] = s[i];
  for (; i < n; i++) d[i] = 0;
  return d;
}

char* stpncpy(char* restrict d, const char* restrict s, size_t n) {
  size_t i = 0;
  for (; i < n && s[i]; i++) d[i] = s[i];
  char* e = d + i;
  for (; i < n; i++) d[i] = 0;
  return e;
}

size_t strlcpy(char* d, const char* s, size_t n) {
  size_t l = strlen(s);
  if (n) {
    size_t c = l < n - 1 ? l : n - 1;
    memcpy(d, s, c);
    d[c] = 0;
  }
  return l;
}

size_t strlcat(char* d, const char* s, size_t n) {
  size_t dl = strnlen(d, n);
  if (dl == n) return n + strlen(s);
  return dl + strlcpy(d + dl, s, n - dl);
}

char* strcat(char* restrict d, const char* restrict s) {
  strcpy(d + strlen(d), s);
  return d;
}

char* strncat(char* restrict d, const char* restrict s, size_t n) {
  char* p = d + strlen(d);
  while (n-- && *s) *p++ = *s++;
  *p = 0;
  return d;
}

int strcmp(const char* a, const char* b) {
  while (*a && *a == *b) a++, b++;
  return (unsigned char)*a - (unsigned char)*b;
}

int strncmp(const char* a, const char* b, size_t n) {
  for (; n; n--, a++, b++) {
    if (*a != *b) return (unsigned char)*a - (unsigned char)*b;
    if (!*a) return 0;
  }
  return 0;
}

int strcoll(const char* a, const char* b) { return strcmp(a, b); }

size_t strxfrm(char* restrict d, const char* restrict s, size_t n) {
  size_t l = strlen(s);
  if (l < n) memcpy(d, s, l + 1);
  return l;
}

static int lower(int c) { return c >= 'A' && c <= 'Z' ? c + 32 : c; }

int strcasecmp(const char* a, const char* b) {
  while (*a && lower((unsigned char)*a) == lower((unsigned char)*b)) a++, b++;
  return lower((unsigned char)*a) - lower((unsigned char)*b);
}

int strncasecmp(const char* a, const char* b, size_t n) {
  for (; n; n--, a++, b++) {
    int x = lower((unsigned char)*a), y = lower((unsigned char)*b);
    if (x != y) return x - y;
    if (!x) return 0;
  }
  return 0;
}

char* strchr(const char* s, int c) {
  for (;; s++) {
    if (*s == (char)c) return (char*)s;
    if (!*s) return NULL;
  }
}

char* strchrnul(const char* s, int c) {
  while (*s && *s != (char)c) s++;
  return (char*)s;
}

char* strrchr(const char* s, int c) {
  const char* r = NULL;
  for (;; s++) {
    if (*s == (char)c) r = s;
    if (!*s) return (char*)r;
  }
}

size_t strspn(const char* s, const char* a) {
  size_t i = 0;
  while (s[i] && strchr(a, s[i])) i++;
  return i;
}

size_t strcspn(const char* s, const char* r) {
  size_t i = 0;
  while (s[i] && !strchr(r, s[i])) i++;
  return i;
}

char* strpbrk(const char* s, const char* a) {
  s += strcspn(s, a);
  return *s ? (char*)s : NULL;
}

char* strstr(const char* h, const char* n) {
  size_t nl = strlen(n);
  if (!nl) return (char*)h;
  for (; *h; h++)
    if (*h == *n && strncmp(h, n, nl) == 0) return (char*)h;
  return NULL;
}

char* strcasestr(const char* h, const char* n) {
  size_t nl = strlen(n);
  if (!nl) return (char*)h;
  for (; *h; h++)
    if (strncasecmp(h, n, nl) == 0) return (char*)h;
  return NULL;
}

char* strdup(const char* s) {
  size_t l = strlen(s) + 1;
  char* d = malloc(l);
  return d ? memcpy(d, s, l) : NULL;
}

char* strndup(const char* s, size_t n) {
  size_t l = strnlen(s, n);
  char* d = malloc(l + 1);
  if (!d) return NULL;
  memcpy(d, s, l);
  d[l] = 0;
  return d;
}

char* strtok_r(char* restrict s, const char* restrict sep, char** restrict save) {
  if (!s) s = *save;
  s += strspn(s, sep);
  if (!*s) {
    *save = s;
    return NULL;
  }
  char* e = s + strcspn(s, sep);
  if (*e) *e++ = 0;
  *save = e;
  return s;
}

char* strtok(char* restrict s, const char* restrict sep) {
  static char* save;
  return strtok_r(s, sep, &save);
}

char* strsep(char** sp, const char* delim) {
  char* s = *sp;
  if (!s) return NULL;
  char* e = s + strcspn(s, delim);
  if (*e) {
    *e = 0;
    *sp = e + 1;
  } else {
    *sp = NULL;
  }
  return s;
}

char* __gnu_basename(const char* path) {
  const char* s = strrchr(path, '/');
  return (char*)(s ? s + 1 : path);
}

// Wide strings (wchar_t is 32 bits on Android).

size_t wcslen(const wchar_t* s) {
  const wchar_t* p = s;
  while (*p) p++;
  return (size_t)(p - s);
}

size_t wcsnlen(const wchar_t* s, size_t n) {
  size_t i = 0;
  while (i < n && s[i]) i++;
  return i;
}

wchar_t* wmemcpy(wchar_t* restrict d, const wchar_t* restrict s, size_t n) {
  return memcpy(d, s, n * sizeof(wchar_t));
}

wchar_t* wmemmove(wchar_t* d, const wchar_t* s, size_t n) {
  return memmove(d, s, n * sizeof(wchar_t));
}

wchar_t* wmemset(wchar_t* d, wchar_t c, size_t n) {
  for (size_t i = 0; i < n; i++) d[i] = c;
  return d;
}

int wmemcmp(const wchar_t* a, const wchar_t* b, size_t n) {
  for (; n; n--, a++, b++)
    if (*a != *b) return *a < *b ? -1 : 1;
  return 0;
}

wchar_t* wmemchr(const wchar_t* s, wchar_t c, size_t n) {
  for (; n; n--, s++)
    if (*s == c) return (wchar_t*)s;
  return NULL;
}

int wcscmp(const wchar_t* a, const wchar_t* b) {
  while (*a && *a == *b) a++, b++;
  return *a < *b ? -1 : *a > *b;
}

int wcsncmp(const wchar_t* a, const wchar_t* b, size_t n) {
  for (; n; n--, a++, b++) {
    if (*a != *b) return *a < *b ? -1 : 1;
    if (!*a) return 0;
  }
  return 0;
}

int wcscoll(const wchar_t* a, const wchar_t* b) { return wcscmp(a, b); }

size_t wcsxfrm(wchar_t* restrict d, const wchar_t* restrict s, size_t n) {
  size_t l = wcslen(s);
  if (l < n) wmemcpy(d, s, l + 1);
  return l;
}

wchar_t* wcschr(const wchar_t* s, wchar_t c) {
  for (;; s++) {
    if (*s == c) return (wchar_t*)s;
    if (!*s) return NULL;
  }
}

wchar_t* wcsrchr(const wchar_t* s, wchar_t c) {
  const wchar_t* r = NULL;
  for (;; s++) {
    if (*s == c) r = s;
    if (!*s) return (wchar_t*)r;
  }
}

wchar_t* wcscpy(wchar_t* restrict d, const wchar_t* restrict s) {
  wchar_t* r = d;
  while ((*d++ = *s++)) {
  }
  return r;
}

wchar_t* wcsncpy(wchar_t* restrict d, const wchar_t* restrict s, size_t n) {
  size_t i = 0;
  for (; i < n && s[i]; i++) d[i] = s[i];
  for (; i < n; i++) d[i] = 0;
  return d;
}

wchar_t* wcscat(wchar_t* restrict d, const wchar_t* restrict s) {
  wcscpy(d + wcslen(d), s);
  return d;
}

size_t wcsspn(const wchar_t* s, const wchar_t* a) {
  size_t i = 0;
  while (s[i] && wcschr(a, s[i])) i++;
  return i;
}

size_t wcscspn(const wchar_t* s, const wchar_t* r) {
  size_t i = 0;
  while (s[i] && !wcschr(r, s[i])) i++;
  return i;
}

wchar_t* wcspbrk(const wchar_t* s, const wchar_t* a) {
  s += wcscspn(s, a);
  return *s ? (wchar_t*)s : NULL;
}

wchar_t* wcsstr(const wchar_t* h, const wchar_t* n) {
  size_t nl = wcslen(n);
  if (!nl) return (wchar_t*)h;
  for (; *h; h++)
    if (wcsncmp(h, n, nl) == 0) return (wchar_t*)h;
  return NULL;
}
