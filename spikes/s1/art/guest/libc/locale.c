// ctype, wide characters and locales of the guest libc shim (spike S8).
//
// Like bionic, there is one locale: "C" with UTF-8 multibyte conversion
// (bionic's C.UTF-8). The *_l functions ignore their locale argument.

#define __BIONIC_CTYPE_INLINE /* emit the out-of-line definitions */
#include <ctype.h>

#include <errno.h>
#include <langinfo.h>
#include <limits.h>
#include <locale.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <uchar.h>
#include <wchar.h>
#include <wctype.h>
#include <inttypes.h>

// The out-of-line ctype functions come from <ctype.h> above.
// Wide classification: ASCII rules, everything above 0x7F printable and
// alphabetic (enough for the code built here; bionic uses ICU data).
int iswalpha(wint_t c) { return c < 128 ? isalpha((int)c) : c > 0xa0; }
int iswdigit(wint_t c) { return c >= '0' && c <= '9'; }
int iswalnum(wint_t c) { return iswalpha(c) || iswdigit(c); }
int iswblank(wint_t c) { return c == ' ' || c == '\t'; }
int iswcntrl(wint_t c) { return c < 32 || c == 127; }
int iswgraph(wint_t c) { return c < 128 ? isgraph((int)c) : c > 0xa0; }
int iswlower(wint_t c) { return c < 128 && islower((int)c); }
int iswprint(wint_t c) { return c < 128 ? isprint((int)c) : c >= 0xa0; }
int iswpunct(wint_t c) { return c < 128 && ispunct((int)c); }
int iswspace(wint_t c) { return c < 128 ? isspace((int)c) : c == 0x2028 || c == 0x2029; }
int iswupper(wint_t c) { return c < 128 && isupper((int)c); }
int iswxdigit(wint_t c) { return c < 128 && isxdigit((int)c); }
wint_t towlower(wint_t c) { return c < 128 ? (wint_t)tolower((int)c) : c; }
wint_t towupper(wint_t c) { return c < 128 ? (wint_t)toupper((int)c) : c; }
int iswalpha_l(wint_t c, locale_t l) { (void)l; return iswalpha(c); }
int iswdigit_l(wint_t c, locale_t l) { (void)l; return iswdigit(c); }
int iswalnum_l(wint_t c, locale_t l) { (void)l; return iswalnum(c); }
int iswblank_l(wint_t c, locale_t l) { (void)l; return iswblank(c); }
int iswcntrl_l(wint_t c, locale_t l) { (void)l; return iswcntrl(c); }
int iswgraph_l(wint_t c, locale_t l) { (void)l; return iswgraph(c); }
int iswlower_l(wint_t c, locale_t l) { (void)l; return iswlower(c); }
int iswprint_l(wint_t c, locale_t l) { (void)l; return iswprint(c); }
int iswpunct_l(wint_t c, locale_t l) { (void)l; return iswpunct(c); }
int iswspace_l(wint_t c, locale_t l) { (void)l; return iswspace(c); }
int iswupper_l(wint_t c, locale_t l) { (void)l; return iswupper(c); }
int iswxdigit_l(wint_t c, locale_t l) { (void)l; return iswxdigit(c); }
wint_t towlower_l(wint_t c, locale_t l) { (void)l; return towlower(c); }
wint_t towupper_l(wint_t c, locale_t l) { (void)l; return towupper(c); }

static const char* const class_names[] = {"alnum", "alpha", "blank", "cntrl", "digit", "graph",
                                          "lower", "print", "punct", "space", "upper", "xdigit"};
wctype_t wctype(const char* name) {
  for (unsigned i = 0; i < 12; i++)
    if (strcmp(name, class_names[i]) == 0) return i + 1;
  return 0;
}
wctype_t wctype_l(const char* name, locale_t l) { (void)l; return wctype(name); }
int iswctype(wint_t c, wctype_t t) {
  static int (*const fns[])(wint_t) = {iswalnum, iswalpha, iswblank, iswcntrl, iswdigit, iswgraph,
                                       iswlower, iswprint, iswpunct, iswspace, iswupper, iswxdigit};
  return t >= 1 && t <= 12 ? fns[t - 1](c) : 0;
}
int iswctype_l(wint_t c, wctype_t t, locale_t l) { (void)l; return iswctype(c, t); }
int wcwidth(wchar_t c) { return c == 0 ? 0 : (c < 32 || c == 127) ? -1 : 1; }

// UTF-8 conversion. mbstate_t keeps the bytes of an incomplete sequence.

size_t __ctype_get_mb_cur_max(void) { return 4; }

typedef struct {
  unsigned char bytes[4];
  unsigned char count;
} u8state;

size_t mbrtowc(wchar_t* restrict pwc, const char* restrict s, size_t n, mbstate_t* restrict ps) {
  static mbstate_t internal;
  u8state* st = (u8state*)(ps ? ps : &internal);
  if (!s) {
    st->count = 0;
    return 0;
  }
  if (n == 0) return (size_t)-2;
  unsigned char buf[4];
  size_t have = st->count;
  memcpy(buf, st->bytes, have);
  size_t used = 0;
  while (have < 4 && used < n) {
    buf[have++] = (unsigned char)s[used++];
    unsigned char b = buf[0];
    size_t need = b < 0x80 ? 1 : (b & 0xE0) == 0xC0 ? 2 : (b & 0xF0) == 0xE0 ? 3 : (b & 0xF8) == 0xF0 ? 4 : 0;
    if (!need || (b >= 0x80 && b < 0xC2)) {
      st->count = 0;
      errno = EILSEQ;
      return (size_t)-1;
    }
    for (size_t i = 1; i < have; i++)
      if ((buf[i] & 0xC0) != 0x80) {
        st->count = 0;
        errno = EILSEQ;
        return (size_t)-1;
      }
    if (have == need) {
      uint32_t c = need == 1 ? b : need == 2 ? b & 0x1F : need == 3 ? b & 0x0F : b & 0x07;
      for (size_t i = 1; i < need; i++) c = (c << 6) | (buf[i] & 0x3F);
      if ((need == 3 && c < 0x800) || (need == 4 && (c < 0x10000 || c > 0x10FFFF)) ||
          (c >= 0xD800 && c <= 0xDFFF)) {
        st->count = 0;
        errno = EILSEQ;
        return (size_t)-1;
      }
      st->count = 0;
      if (pwc) *pwc = (wchar_t)c;
      return c ? used : 0;
    }
  }
  memcpy(st->bytes, buf, have);
  st->count = (unsigned char)have;
  return (size_t)-2;
}

size_t mbrlen(const char* restrict s, size_t n, mbstate_t* restrict ps) {
  static mbstate_t internal;
  return mbrtowc(NULL, s, n, ps ? ps : &internal);
}

int mbtowc(wchar_t* restrict pwc, const char* restrict s, size_t n) {
  static mbstate_t st;
  if (!s) return 0;
  size_t r = mbrtowc(pwc, s, n, &st);
  if (r == (size_t)-2) {
    memset(&st, 0, sizeof st);
    errno = EILSEQ;
    return -1;
  }
  return (int)r;
}

int mblen(const char* s, size_t n) { return mbtowc(NULL, s, n); }
int mbsinit(const mbstate_t* ps) { return !ps || ((const u8state*)ps)->count == 0; }

size_t wcrtomb(char* restrict s, wchar_t wc, mbstate_t* restrict ps) {
  (void)ps;
  char tmp[4];
  if (!s) s = tmp, wc = 0;
  uint32_t c = (uint32_t)wc;
  if (c < 0x80) {
    s[0] = (char)c;
    return 1;
  }
  if (c < 0x800) {
    s[0] = (char)(0xC0 | (c >> 6));
    s[1] = (char)(0x80 | (c & 0x3F));
    return 2;
  }
  if ((c >= 0xD800 && c <= 0xDFFF) || c > 0x10FFFF) {
    errno = EILSEQ;
    return (size_t)-1;
  }
  if (c < 0x10000) {
    s[0] = (char)(0xE0 | (c >> 12));
    s[1] = (char)(0x80 | ((c >> 6) & 0x3F));
    s[2] = (char)(0x80 | (c & 0x3F));
    return 3;
  }
  s[0] = (char)(0xF0 | (c >> 18));
  s[1] = (char)(0x80 | ((c >> 12) & 0x3F));
  s[2] = (char)(0x80 | ((c >> 6) & 0x3F));
  s[3] = (char)(0x80 | (c & 0x3F));
  return 4;
}

int wctomb(char* s, wchar_t wc) {
  if (!s) return 0;
  return (int)wcrtomb(s, wc, NULL);
}

wint_t btowc(int c) { return c == EOF || (unsigned)c >= 128 ? WEOF : (wint_t)c; }
int wctob(wint_t c) { return c < 128 ? (int)c : EOF; }

size_t mbsnrtowcs(wchar_t* restrict d, const char** restrict src, size_t nms, size_t len,
                  mbstate_t* restrict ps) {
  static mbstate_t internal;
  if (!ps) ps = &internal;
  const char* s = *src;
  size_t n = 0;
  while ((!d || n < len) && nms) {
    wchar_t wc;
    size_t r = mbrtowc(&wc, s, nms, ps);
    if (r == (size_t)-1) {
      *src = s;
      return r;
    }
    if (r == (size_t)-2) {
      s += nms;
      break;
    }
    if (r == 0) {
      if (d) d[n] = 0, *src = NULL;
      return n;
    }
    if (d) d[n] = wc;
    n++;
    s += r;
    nms -= r;
  }
  if (d) *src = s;
  return n;
}

size_t mbsrtowcs(wchar_t* restrict d, const char** restrict src, size_t len,
                 mbstate_t* restrict ps) {
  return mbsnrtowcs(d, src, (size_t)-1, len, ps);
}

size_t mbstowcs(wchar_t* restrict d, const char* restrict s, size_t n) {
  mbstate_t st = {0};
  return mbsrtowcs(d, &s, n, &st);
}

size_t wcsnrtombs(char* restrict d, const wchar_t** restrict src, size_t nwc, size_t len,
                  mbstate_t* restrict ps) {
  (void)ps;
  const wchar_t* s = *src;
  size_t n = 0;
  for (; nwc; nwc--, s++) {
    char b[4];
    size_t r = wcrtomb(b, *s, NULL);
    if (r == (size_t)-1) {
      if (d) *src = s;
      return r;
    }
    if (d) {
      if (n + r > len) break;
      memcpy(d + n, b, r);
    }
    if (*s == 0) {
      if (d) *src = NULL;
      return n;
    }
    n += r;
  }
  if (d) *src = s;
  return n;
}

size_t wcsrtombs(char* restrict d, const wchar_t** restrict src, size_t len,
                 mbstate_t* restrict ps) {
  return wcsnrtombs(d, src, (size_t)-1, len, ps);
}

size_t wcstombs(char* restrict d, const wchar_t* restrict s, size_t n) {
  return wcsrtombs(d, &s, n, NULL);
}

size_t c32rtomb(char* restrict s, char32_t c, mbstate_t* restrict ps) {
  return wcrtomb(s, (wchar_t)c, ps);
}
size_t mbrtoc32(char32_t* restrict pc, const char* restrict s, size_t n, mbstate_t* restrict ps) {
  wchar_t w;
  size_t r = mbrtowc(&w, s, n, ps);
  if (pc && r < (size_t)-2) *pc = (char32_t)w;
  return r;
}

// Wide number parsing through the narrow parsers.
static char* narrow(const wchar_t* s, size_t* consumed_map_len) {
  size_t n = wcslen(s);
  char* d = malloc(n + 1);
  if (!d) return NULL;
  for (size_t i = 0; i <= n; i++) d[i] = s[i] < 128 ? (char)s[i] : '?';
  *consumed_map_len = n;
  return d;
}

#define WIDE_PARSE(name, type, call)                                     \
  type name(const wchar_t* restrict s, wchar_t** restrict end, int base) { \
    size_t n;                                                            \
    char* t = narrow(s, &n);                                             \
    if (!t) return 0;                                                    \
    char* e;                                                             \
    type v = call(t, &e, base);                                          \
    if (end) *end = (wchar_t*)s + (e - t);                               \
    free(t);                                                             \
    return v;                                                            \
  }
WIDE_PARSE(wcstol, long, strtol)
WIDE_PARSE(wcstoll, long long, strtoll)
WIDE_PARSE(wcstoul, unsigned long, strtoul)
WIDE_PARSE(wcstoull, unsigned long long, strtoull)
WIDE_PARSE(wcstoimax, intmax_t, strtoimax)
WIDE_PARSE(wcstoumax, uintmax_t, strtoumax)

#define WIDE_FPARSE(name, type, call)                                \
  type name(const wchar_t* restrict s, wchar_t** restrict end) {     \
    size_t n;                                                        \
    char* t = narrow(s, &n);                                         \
    if (!t) return 0;                                                \
    char* e;                                                         \
    type v = call(t, &e);                                            \
    if (end) *end = (wchar_t*)s + (e - t);                           \
    free(t);                                                         \
    return v;                                                        \
  }
WIDE_FPARSE(wcstod, double, strtod)
WIDE_FPARSE(wcstof, float, strtof)
WIDE_FPARSE(wcstold, long double, strtold)

// Locales.

static struct lconv c_lconv = {
    .decimal_point = ".",
    .thousands_sep = "",
    .grouping = "",
    .int_curr_symbol = "",
    .currency_symbol = "",
    .mon_decimal_point = "",
    .mon_thousands_sep = "",
    .mon_grouping = "",
    .positive_sign = "",
    .negative_sign = "",
    .int_frac_digits = CHAR_MAX,
    .frac_digits = CHAR_MAX,
    .p_cs_precedes = CHAR_MAX,
    .p_sep_by_space = CHAR_MAX,
    .n_cs_precedes = CHAR_MAX,
    .n_sep_by_space = CHAR_MAX,
    .p_sign_posn = CHAR_MAX,
    .n_sign_posn = CHAR_MAX,
    .int_p_cs_precedes = CHAR_MAX,
    .int_p_sep_by_space = CHAR_MAX,
    .int_n_cs_precedes = CHAR_MAX,
    .int_n_sep_by_space = CHAR_MAX,
    .int_p_sign_posn = CHAR_MAX,
    .int_n_sign_posn = CHAR_MAX,
};

struct lconv* localeconv(void) { return &c_lconv; }

static int known_locale(const char* name) {
  return !name || !*name || strcmp(name, "C") == 0 || strcmp(name, "POSIX") == 0 ||
         strcmp(name, "C.UTF-8") == 0 || strcmp(name, "en_US.UTF-8") == 0 ||
         strcmp(name, "en_US") == 0;
}

char* setlocale(int cat, const char* name) {
  (void)cat;
  if (!known_locale(name)) return NULL;
  return "C.UTF-8";
}

struct __locale_t {
  int dummy;
};
static struct __locale_t the_locale;

locale_t newlocale(int mask, const char* name, locale_t base) {
  (void)mask;
  (void)base;
  if (!known_locale(name)) {
    errno = ENOENT;
    return NULL;
  }
  return &the_locale;
}
locale_t duplocale(locale_t l) { return l; }
void freelocale(locale_t l) { (void)l; }
locale_t uselocale(locale_t l) {
  (void)l;
  return LC_GLOBAL_LOCALE;
}

int strcoll_l(const char* a, const char* b, locale_t l) { (void)l; return strcmp(a, b); }
size_t strxfrm_l(char* d, const char* s, size_t n, locale_t l) {
  (void)l;
  return strxfrm(d, s, n);
}
int wcscoll_l(const wchar_t* a, const wchar_t* b, locale_t l) { (void)l; return wcscmp(a, b); }
size_t wcsxfrm_l(wchar_t* d, const wchar_t* s, size_t n, locale_t l) {
  (void)l;
  return wcsxfrm(d, s, n);
}

char* nl_langinfo(nl_item item) {
  switch (item) {
    case CODESET: return "UTF-8";
    case RADIXCHAR: return ".";
    case THOUSEP: return "";
    case D_T_FMT: return "%a %b %e %H:%M:%S %Y";
    case D_FMT: return "%m/%d/%y";
    case T_FMT: return "%H:%M:%S";
    case T_FMT_AMPM: return "%I:%M:%S %p";
    case AM_STR: return "AM";
    case PM_STR: return "PM";
    default: return "";
  }
}
char* nl_langinfo_l(nl_item item, locale_t l) {
  (void)l;
  return nl_langinfo(item);
}
