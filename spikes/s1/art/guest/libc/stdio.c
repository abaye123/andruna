// stdio of the guest libc shim (spike S8): FILE on top of file descriptors,
// printf and scanf engines.
//
// Floating-point conversions are delegated to the host
// (`__andruna_fmt_fp`, which uses the Windows C runtime) one conversion at
// a time; everything else is formatted here. Streams are not locked: the
// spike runs its tests on one thread at a time. Production uses bionic's
// stdio.

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <wchar.h>

#include "host.h"

#define F_READ 1
#define F_WRITE 2
#define F_APPEND 4
#define F_STR 8      // writes go to a growing or fixed memory buffer
#define F_STRDYN 16  // the buffer grows (open_memstream, asprintf)

struct __sFILE {
  int fd;
  int flags;
  int err, eof;
  int ungot;  // -1 or a pushed-back byte
  int unbuffered;
  unsigned char rbuf[4096];
  size_t rpos, rlen;
  unsigned char wbuf[4096];
  size_t wlen;
  // Memory streams.
  char* sbuf;
  size_t scap, slen;
  char** sbufp;
  size_t* slenp;
  struct __sFILE* next;
};

static FILE std_files[3] = {
    {.fd = 0, .flags = F_READ, .ungot = -1},
    {.fd = 1, .flags = F_WRITE, .ungot = -1, .unbuffered = 1},
    {.fd = 2, .flags = F_WRITE, .ungot = -1, .unbuffered = 1},
};
FILE* stdin = &std_files[0];
FILE* stdout = &std_files[1];
FILE* stderr = &std_files[2];
static FILE* open_files;

// ---------------------------------------------------------------------------
// Low level.

static int raw_write(int fd, const void* p, size_t n) {
  const char* c = p;
  while (n) {
    ssize_t w = write(fd, c, n);
    if (w < 0) {
      if (errno == EINTR) continue;
      return -1;
    }
    c += w;
    n -= (size_t)w;
  }
  return 0;
}

int fflush(FILE* f) {
  if (!f) {
    for (int i = 1; i < 3; i++) fflush(&std_files[i]);
    for (FILE* p = open_files; p; p = p->next) fflush(p);
    return 0;
  }
  if (f->flags & F_STR) {
    if (f->sbufp) *f->sbufp = f->sbuf;
    if (f->slenp) *f->slenp = f->slen;
    return 0;
  }
  if (f->wlen) {
    int r = raw_write(f->fd, f->wbuf, f->wlen);
    f->wlen = 0;
    if (r) {
      f->err = 1;
      return EOF;
    }
  }
  return 0;
}

static int str_put(FILE* f, const char* p, size_t n) {
  if (f->slen + n + 1 > f->scap) {
    if (!(f->flags & F_STRDYN)) {
      size_t room = f->scap ? f->scap - 1 - f->slen : 0;
      memcpy(f->sbuf + f->slen, p, room);
      f->slen += room;
      f->sbuf[f->slen] = 0;
      return 0;
    }
    size_t c = f->scap ? f->scap : 64;
    while (c < f->slen + n + 1) c *= 2;
    char* nb = realloc(f->sbuf, c);
    if (!nb) return -1;
    f->sbuf = nb;
    f->scap = c;
  }
  memcpy(f->sbuf + f->slen, p, n);
  f->slen += n;
  f->sbuf[f->slen] = 0;
  return 0;
}

static int put(FILE* f, const void* p, size_t n) {
  if (f->flags & F_STR) return str_put(f, p, n);
  if (!(f->flags & F_WRITE)) {
    f->err = 1;
    errno = EBADF;
    return -1;
  }
  if (f->unbuffered) return raw_write(f->fd, p, n) ? (f->err = 1, -1) : 0;
  if (f->wlen + n > sizeof f->wbuf) {
    if (fflush(f)) return -1;
    if (n > sizeof f->wbuf) return raw_write(f->fd, p, n) ? (f->err = 1, -1) : 0;
  }
  memcpy(f->wbuf + f->wlen, p, n);
  f->wlen += n;
  return 0;
}

static int fill(FILE* f) {
  if (f->flags & F_STR) return f->eof = 1, -1;
  if (f == stdin) fflush(stdout);
  ssize_t r;
  do r = read(f->fd, f->rbuf, sizeof f->rbuf);
  while (r < 0 && errno == EINTR);
  if (r <= 0) {
    if (r < 0) f->err = 1;
    else f->eof = 1;
    return -1;
  }
  f->rpos = 0;
  f->rlen = (size_t)r;
  return 0;
}

int fgetc(FILE* f) {
  if (f->ungot >= 0) {
    int c = f->ungot;
    f->ungot = -1;
    return c;
  }
  if (f->flags & F_STR) {
    if (f->rpos < f->slen) return (unsigned char)f->sbuf[f->rpos++];
    f->eof = 1;
    return EOF;
  }
  if (f->rpos >= f->rlen && fill(f)) return EOF;
  return f->rbuf[f->rpos++];
}
int getc(FILE* f) { return fgetc(f); }
int getchar(void) { return fgetc(stdin); }
int getc_unlocked(FILE* f) { return fgetc(f); }
int getchar_unlocked(void) { return fgetc(stdin); }

int ungetc(int c, FILE* f) {
  if (c == EOF) return EOF;
  f->ungot = (unsigned char)c;
  f->eof = 0;
  return c;
}

size_t fread(void* restrict p, size_t sz, size_t n, FILE* restrict f) {
  size_t total = sz * n, got = 0;
  unsigned char* d = p;
  if (!total) return 0;
  if (f->ungot >= 0) {
    d[got++] = (unsigned char)f->ungot;
    f->ungot = -1;
  }
  while (got < total) {
    if (f->flags & F_STR) {
      size_t a = f->slen - f->rpos;
      if (!a) {
        f->eof = 1;
        break;
      }
      if (a > total - got) a = total - got;
      memcpy(d + got, f->sbuf + f->rpos, a);
      f->rpos += a;
      got += a;
      continue;
    }
    if (f->rpos >= f->rlen && fill(f)) break;
    size_t a = f->rlen - f->rpos;
    if (a > total - got) a = total - got;
    memcpy(d + got, f->rbuf + f->rpos, a);
    f->rpos += a;
    got += a;
  }
  return got / sz;
}

size_t fwrite(const void* restrict p, size_t sz, size_t n, FILE* restrict f) {
  if (!sz || !n) return 0;
  return put(f, p, sz * n) ? 0 : n;
}
size_t fwrite_unlocked(const void* p, size_t sz, size_t n, FILE* f) { return fwrite(p, sz, n, f); }

int fputc(int c, FILE* f) {
  unsigned char b = (unsigned char)c;
  return put(f, &b, 1) ? EOF : b;
}
int putc(int c, FILE* f) { return fputc(c, f); }
int putchar(int c) { return fputc(c, stdout); }
int putc_unlocked(int c, FILE* f) { return fputc(c, f); }
int putchar_unlocked(int c) { return fputc(c, stdout); }
int fputs(const char* restrict s, FILE* restrict f) { return put(f, s, strlen(s)) ? EOF : 0; }
int puts(const char* s) { return fputs(s, stdout) || fputc('\n', stdout) == EOF ? EOF : 0; }

char* fgets(char* restrict s, int n, FILE* restrict f) {
  int i = 0;
  if (n <= 0) return NULL;
  while (i < n - 1) {
    int c = fgetc(f);
    if (c == EOF) break;
    s[i++] = (char)c;
    if (c == '\n') break;
  }
  if (!i) return NULL;
  s[i] = 0;
  return s;
}

ssize_t getdelim(char** restrict lp, size_t* restrict np, int delim, FILE* restrict f) {
  size_t i = 0;
  if (!*lp || !*np) {
    *np = 128;
    *lp = malloc(*np);
    if (!*lp) return -1;
  }
  for (;;) {
    int c = fgetc(f);
    if (c == EOF) break;
    if (i + 2 > *np) {
      char* nb = realloc(*lp, *np * 2);
      if (!nb) return -1;
      *lp = nb;
      *np *= 2;
    }
    (*lp)[i++] = (char)c;
    if (c == delim) break;
  }
  if (!i) return -1;
  (*lp)[i] = 0;
  return (ssize_t)i;
}
ssize_t getline(char** restrict lp, size_t* restrict np, FILE* restrict f) {
  return getdelim(lp, np, '\n', f);
}

int feof(FILE* f) { return f->eof; }
int ferror(FILE* f) { return f->err; }
void clearerr(FILE* f) { f->eof = f->err = 0; }
int fileno(FILE* f) { return f->fd; }
int feof_unlocked(FILE* f) { return f->eof; }
int ferror_unlocked(FILE* f) { return f->err; }
int fileno_unlocked(FILE* f) { return f->fd; }
void flockfile(FILE* f) { (void)f; }
void funlockfile(FILE* f) { (void)f; }
int ftrylockfile(FILE* f) { (void)f; return 0; }

int setvbuf(FILE* restrict f, char* restrict b, int mode, size_t sz) {
  (void)b;
  (void)sz;
  fflush(f);
  f->unbuffered = mode == _IONBF || mode == _IOLBF;
  return 0;
}
void setbuf(FILE* restrict f, char* restrict b) { setvbuf(f, b, b ? _IOFBF : _IONBF, BUFSIZ); }
int setlinebuf(FILE* f) { return setvbuf(f, NULL, _IOLBF, 0); }

static int mode_flags(const char* m, int* oflags) {
  int f = 0, o = 0;
  switch (*m) {
    case 'r': f = F_READ; o = O_RDONLY; break;
    case 'w': f = F_WRITE; o = O_WRONLY | O_CREAT | O_TRUNC; break;
    case 'a': f = F_WRITE | F_APPEND; o = O_WRONLY | O_CREAT | O_APPEND; break;
    default: errno = EINVAL; return -1;
  }
  for (m++; *m; m++) {
    if (*m == '+') {
      f |= F_READ | F_WRITE;
      o = (o & ~(O_RDONLY | O_WRONLY)) | O_RDWR;
    } else if (*m == 'e') {
      o |= O_CLOEXEC;
    } else if (*m == 'x') {
      o |= O_EXCL;
    }
  }
  *oflags = o;
  return f;
}

static FILE* new_file(int fd, int flags) {
  FILE* f = calloc(1, sizeof *f);
  if (!f) return NULL;
  f->fd = fd;
  f->flags = flags;
  f->ungot = -1;
  f->next = open_files;
  open_files = f;
  return f;
}

FILE* fdopen(int fd, const char* mode) {
  int o;
  int fl = mode_flags(mode, &o);
  return fl < 0 ? NULL : new_file(fd, fl);
}

FILE* fopen(const char* restrict path, const char* restrict mode) {
  int o;
  int fl = mode_flags(mode, &o);
  if (fl < 0) return NULL;
  int fd = open(path, o, 0666);
  if (fd < 0) return NULL;
  FILE* f = new_file(fd, fl);
  if (!f) close(fd);
  return f;
}
FILE* fopen64(const char* restrict path, const char* restrict mode) { return fopen(path, mode); }

static void unlink_file(FILE* f) {
  for (FILE** pp = &open_files; *pp; pp = &(*pp)->next) {
    if (*pp == f) {
      *pp = f->next;
      return;
    }
  }
}

int fclose(FILE* f) {
  int r = fflush(f);
  if (f->flags & F_STR) {
    if (!(f->flags & F_STRDYN) || !f->sbufp) free(f->flags & F_STRDYN ? f->sbuf : NULL);
  } else if (close(f->fd) < 0) {
    r = EOF;
  }
  unlink_file(f);
  if (f >= std_files && f < std_files + 3) return r;
  free(f);
  return r;
}

FILE* freopen(const char* restrict path, const char* restrict mode, FILE* restrict f) {
  (void)path;
  (void)mode;
  (void)f;
  errno = ENOSYS;
  return NULL;
}

FILE* open_memstream(char** bp, size_t* sp) {
  FILE* f = new_file(-1, F_WRITE | F_STR | F_STRDYN);
  if (!f) return NULL;
  f->sbufp = bp;
  f->slenp = sp;
  str_put(f, "", 0);
  fflush(f);
  return f;
}

FILE* fmemopen(void* restrict buf, size_t size, const char* restrict mode) {
  int o;
  int fl = mode_flags(mode, &o);
  if (fl < 0) return NULL;
  FILE* f = new_file(-1, fl | F_STR);
  if (!f) return NULL;
  f->sbuf = buf;
  f->scap = size;
  f->slen = (fl & F_WRITE) && !(fl & F_READ) ? 0 : strnlen(buf, size);
  return f;
}

int fseeko(FILE* f, off_t off, int whence) {
  if (f->flags & F_STR) {
    off_t base = whence == SEEK_SET ? 0 : whence == SEEK_CUR ? (off_t)f->rpos : (off_t)f->slen;
    f->rpos = (size_t)(base + off);
    f->eof = 0;
    return 0;
  }
  if (fflush(f)) return -1;
  if (whence == SEEK_CUR) off -= (off_t)(f->rlen - f->rpos) + (f->ungot >= 0);
  f->rpos = f->rlen = 0;
  f->ungot = -1;
  if (lseek(f->fd, off, whence) < 0) return -1;
  f->eof = 0;
  return 0;
}
int fseek(FILE* f, long off, int whence) { return fseeko(f, off, whence); }
int fseeko64(FILE* f, off64_t off, int whence) { return fseeko(f, off, whence); }

off_t ftello(FILE* f) {
  if (f->flags & F_STR) return (off_t)((f->flags & F_READ) ? f->rpos : f->slen);
  off_t p = lseek(f->fd, 0, SEEK_CUR);
  if (p < 0) return -1;
  return p - (off_t)(f->rlen - f->rpos) + (off_t)f->wlen - (f->ungot >= 0);
}
long ftell(FILE* f) { return ftello(f); }
off64_t ftello64(FILE* f) { return ftello(f); }
void rewind(FILE* f) {
  fseeko(f, 0, SEEK_SET);
  f->err = 0;
}
int fgetpos(FILE* f, fpos_t* p) {
  off_t o = ftello(f);
  if (o < 0) return -1;
  *p = o;
  return 0;
}
int fsetpos(FILE* f, const fpos_t* p) { return fseeko(f, *p, SEEK_SET); }

int remove(const char* path) {
  if (unlink(path) == 0) return 0;
  if (errno == EISDIR || errno == EPERM || errno == EACCES) return rmdir(path);
  return -1;
}

FILE* tmpfile(void) {
  char name[] = "/tmp/tmpfile.XXXXXX";
  int fd = mkstemp(name);
  if (fd < 0) return NULL;
  unlink(name);
  return fdopen(fd, "w+");
}
FILE* tmpfile64(void) { return tmpfile(); }

void perror(const char* s) {
  int e = errno;
  if (s && *s) {
    fputs(s, stderr);
    fputs(": ", stderr);
  }
  fputs(strerror(e), stderr);
  fputc('\n', stderr);
}

// ---------------------------------------------------------------------------
// printf.

typedef struct {
  FILE* f;
  int n;  // characters produced so far
} out_t;

static void emit(out_t* o, const char* p, size_t n) {
  if (n) put(o->f, p, n);
  o->n += (int)n;
}

static void pad(out_t* o, char c, int n) {
  char b[64];
  memset(b, c, sizeof b);
  while (n > 0) {
    int k = n > 64 ? 64 : n;
    emit(o, b, (size_t)k);
    n -= k;
  }
}

enum { L_NONE, L_HH, L_H, L_L, L_LL, L_J, L_Z, L_T, L_LD };

static size_t utf8_encode(char* d, uint32_t c) {
  if (c < 0x80) return d[0] = (char)c, 1;
  if (c < 0x800) return d[0] = (char)(0xC0 | (c >> 6)), d[1] = (char)(0x80 | (c & 0x3F)), 2;
  if (c < 0x10000) {
    d[0] = (char)(0xE0 | (c >> 12));
    d[1] = (char)(0x80 | ((c >> 6) & 0x3F));
    d[2] = (char)(0x80 | (c & 0x3F));
    return 3;
  }
  d[0] = (char)(0xF0 | (c >> 18));
  d[1] = (char)(0x80 | ((c >> 12) & 0x3F));
  d[2] = (char)(0x80 | ((c >> 6) & 0x3F));
  d[3] = (char)(0x80 | (c & 0x3F));
  return 4;
}

static int put_uint(char* d, unsigned v) {
  char t[12];
  int n = 0;
  do t[n++] = (char)('0' + v % 10);
  while (v /= 10);
  for (int i = 0; i < n; i++) d[i] = t[n - 1 - i];
  return n;
}

static int fmt_core(FILE* f, const char* fmt, va_list* app) {
  out_t o = {f, 0};
  int saved_errno = errno;
  for (const char* p = fmt; *p;) {
    if (*p != '%') {
      const char* q = p;
      while (*q && *q != '%') q++;
      emit(&o, p, (size_t)(q - p));
      p = q;
      continue;
    }
    const char* spec_start = p++;
    int left = 0, plus = 0, space = 0, alt = 0, zero = 0;
    for (;; p++) {
      if (*p == '-') left = 1;
      else if (*p == '+') plus = 1;
      else if (*p == ' ') space = 1;
      else if (*p == '#') alt = 1;
      else if (*p == '0') zero = 1;
      else if (*p == '\'') {
      } else break;
    }
    int width = 0, prec = -1;
    if (*p == '*') {
      width = va_arg(*app, int);
      if (width < 0) left = 1, width = -width;
      p++;
    } else {
      while (*p >= '0' && *p <= '9') width = width * 10 + (*p++ - '0');
    }
    if (*p == '.') {
      p++;
      prec = 0;
      if (*p == '*') {
        prec = va_arg(*app, int);
        if (prec < 0) prec = -1;
        p++;
      } else {
        while (*p >= '0' && *p <= '9') prec = prec * 10 + (*p++ - '0');
      }
    }
    int len = L_NONE;
    switch (*p) {
      case 'h': len = p[1] == 'h' ? (p++, L_HH) : L_H; p++; break;
      case 'l': len = p[1] == 'l' ? (p++, L_LL) : L_L; p++; break;
      case 'q': len = L_LL; p++; break;
      case 'j': len = L_J; p++; break;
      case 'z': len = L_Z; p++; break;
      case 't': len = L_T; p++; break;
      case 'L': len = L_LD; p++; break;
    }
    char conv = *p ? *p++ : 0;
    char tmp[600];
    const char* body = tmp;
    size_t blen = 0;
    const char* prefix = "";
    int numeric = 0;
    switch (conv) {
      case '%':
        emit(&o, "%", 1);
        continue;
      case 'd':
      case 'i':
      case 'u':
      case 'o':
      case 'x':
      case 'X':
      case 'p': {
        uint64_t u;
        int neg = 0;
        if (conv == 'p') {
          u = (uintptr_t)va_arg(*app, void*);
          alt = 1;
        } else if (conv == 'd' || conv == 'i') {
          int64_t v;
          switch (len) {
            case L_HH: v = (signed char)va_arg(*app, int); break;
            case L_H: v = (short)va_arg(*app, int); break;
            case L_NONE: v = va_arg(*app, int); break;
            default: v = va_arg(*app, int64_t); break;
          }
          neg = v < 0;
          u = neg ? -(uint64_t)v : (uint64_t)v;
        } else {
          switch (len) {
            case L_HH: u = (unsigned char)va_arg(*app, unsigned); break;
            case L_H: u = (unsigned short)va_arg(*app, unsigned); break;
            case L_NONE: u = va_arg(*app, unsigned); break;
            default: u = va_arg(*app, uint64_t); break;
          }
        }
        unsigned base = (conv == 'o') ? 8 : (conv == 'x' || conv == 'X' || conv == 'p') ? 16 : 10;
        const char* digits = conv == 'X' ? "0123456789ABCDEF" : "0123456789abcdef";
        char* e = tmp + sizeof tmp;
        char* s = e;
        while (u) {
          *--s = digits[u % base];
          u /= base;
        }
        int nd = (int)(e - s);
        if (prec < 0 && nd == 0) *--s = '0', nd = 1;
        while (nd < prec) *--s = '0', nd++;
        if (conv == 'o' && alt && *s != '0') *--s = '0';
        body = s;
        blen = (size_t)(e - s);
        if (neg) prefix = "-";
        else if ((conv == 'd' || conv == 'i') && plus) prefix = "+";
        else if ((conv == 'd' || conv == 'i') && space) prefix = " ";
        if (alt && base == 16 && (conv == 'p' || !(blen == 1 && *body == '0')))
          prefix = conv == 'X' ? "0X" : "0x";
        numeric = prec < 0;
        break;
      }
      case 'c':
      case 'C': {
        if (len == L_L || conv == 'C') {
          blen = utf8_encode(tmp, (uint32_t)va_arg(*app, wint_t));
        } else {
          tmp[0] = (char)va_arg(*app, int);
          blen = 1;
        }
        break;
      }
      case 'm':
      case 's':
      case 'S': {
        if (conv == 'm') {
          body = strerror(saved_errno);
          blen = strlen(body);
        } else if (len == L_L || conv == 'S') {
          const wchar_t* ws = va_arg(*app, const wchar_t*);
          if (!ws) ws = L"(null)";
          char* d = tmp;
          for (; *ws; ws++) {
            char u8[4];
            size_t k = utf8_encode(u8, (uint32_t)*ws);
            if (prec >= 0 && (d - tmp) + (long)k > prec) break;
            if ((size_t)(d - tmp) + k >= sizeof tmp) break;
            memcpy(d, u8, k);
            d += k;
          }
          blen = (size_t)(d - tmp);
        } else {
          body = va_arg(*app, const char*);
          if (!body) body = "(null)";
          blen = prec >= 0 ? strnlen(body, (size_t)prec) : strlen(body);
        }
        break;
      }
      case 'n': {
        void* np = va_arg(*app, void*);
        switch (len) {
          case L_HH: *(signed char*)np = (signed char)o.n; break;
          case L_H: *(short*)np = (short)o.n; break;
          case L_NONE: *(int*)np = o.n; break;
          default: *(int64_t*)np = o.n; break;
        }
        continue;
      }
      case 'f':
      case 'F':
      case 'e':
      case 'E':
      case 'g':
      case 'G':
      case 'a':
      case 'A': {
        double v = len == L_LD ? (double)va_arg(*app, long double) : va_arg(*app, double);
        int upper = conv >= 'A' && conv <= 'Z';
        if (__builtin_isinf(v) || __builtin_isnan(v)) {
          int neg = __builtin_signbit(v) != 0;
          const char* w = __builtin_isnan(v) ? (upper ? "NAN" : "nan") : (upper ? "INF" : "inf");
          prefix = neg ? "-" : plus ? "+" : space ? " " : "";
          body = w;
          blen = 3;
          break;
        }
        // Rebuild the conversion without the length modifier and with the
        // width and precision resolved, then let the host format it.
        char spec[48];
        char* s = spec;
        *s++ = '%';
        if (left) *s++ = '-';
        if (plus) *s++ = '+';
        if (space) *s++ = ' ';
        if (alt) *s++ = '#';
        if (zero) *s++ = '0';
        if (width > 0) s += put_uint(s, (unsigned)width);
        if (prec >= 0) *s++ = '.', s += put_uint(s, (unsigned)(prec > 500 ? 500 : prec));
        *s++ = conv;
        *s = 0;
        int r = __andruna_fmt_fp(tmp, sizeof tmp, spec, v);
        if (r < 0) r = 0;
        if ((size_t)r >= sizeof tmp) r = sizeof tmp - 1;
        emit(&o, tmp, (size_t)r);
        continue;
      }
      default:
        // Unknown conversion: print it as is.
        emit(&o, spec_start, (size_t)(p - spec_start));
        continue;
    }
    int plen = (int)strlen(prefix);
    int total = plen + (int)blen;
    if (!left && !(zero && numeric)) pad(&o, ' ', width - total);
    emit(&o, prefix, (size_t)plen);
    if (!left && zero && numeric) pad(&o, '0', width - total);
    emit(&o, body, blen);
    if (left) pad(&o, ' ', width - total);
  }
  return o.n;
}

int vfprintf(FILE* restrict f, const char* restrict fmt, va_list ap) {
  va_list c;
  va_copy(c, ap);
  int r = fmt_core(f, fmt, &c);
  va_end(c);
  return f->err ? -1 : r;
}

int fprintf(FILE* restrict f, const char* restrict fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  int r = vfprintf(f, fmt, ap);
  va_end(ap);
  return r;
}

int vprintf(const char* restrict fmt, va_list ap) { return vfprintf(stdout, fmt, ap); }

int printf(const char* restrict fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  int r = vfprintf(stdout, fmt, ap);
  va_end(ap);
  return r;
}

int vsnprintf(char* restrict buf, size_t n, const char* restrict fmt, va_list ap) {
  char dummy[1];
  FILE f = {.fd = -1, .flags = F_WRITE | F_STR, .ungot = -1};
  f.sbuf = n ? buf : dummy;
  f.scap = n ? n : 1;
  f.sbuf[0] = 0;
  // A fixed buffer truncates, but the count is the full length.
  va_list c;
  va_copy(c, ap);
  int r = fmt_core(&f, fmt, &c);
  va_end(c);
  return r;
}

int snprintf(char* restrict buf, size_t n, const char* restrict fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  int r = vsnprintf(buf, n, fmt, ap);
  va_end(ap);
  return r;
}

int vsprintf(char* restrict buf, const char* restrict fmt, va_list ap) {
  return vsnprintf(buf, INT_MAX, fmt, ap);
}

int sprintf(char* restrict buf, const char* restrict fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  int r = vsnprintf(buf, INT_MAX, fmt, ap);
  va_end(ap);
  return r;
}

int vasprintf(char** restrict out, const char* restrict fmt, va_list ap) {
  FILE f = {.fd = -1, .flags = F_WRITE | F_STR | F_STRDYN, .ungot = -1};
  va_list c;
  va_copy(c, ap);
  int r = fmt_core(&f, fmt, &c);
  va_end(c);
  if (!f.sbuf) f.sbuf = strdup("");
  *out = f.sbuf;
  return f.sbuf ? r : -1;
}

int asprintf(char** restrict out, const char* restrict fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  int r = vasprintf(out, fmt, ap);
  va_end(ap);
  return r;
}

int vdprintf(int fd, const char* restrict fmt, va_list ap) {
  char* s;
  int r = vasprintf(&s, fmt, ap);
  if (r < 0) return r;
  raw_write(fd, s, (size_t)r);
  free(s);
  return r;
}

int dprintf(int fd, const char* restrict fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  int r = vdprintf(fd, fmt, ap);
  va_end(ap);
  return r;
}

// Wide printf: the format is converted to UTF-8, formatted, and decoded.
static char* wide_to_utf8(const wchar_t* w) {
  size_t n = 1;
  for (const wchar_t* p = w; *p; p++) n += 4;
  char* s = malloc(n);
  if (!s) return NULL;
  char* d = s;
  for (; *w; w++) d += utf8_encode(d, (uint32_t)*w);
  *d = 0;
  return s;
}

int vswprintf(wchar_t* restrict buf, size_t n, const wchar_t* restrict fmt, va_list ap) {
  char* f8 = wide_to_utf8(fmt);
  if (!f8) return -1;
  char* s;
  int r = vasprintf(&s, f8, ap);
  free(f8);
  if (r < 0) return -1;
  size_t i = 0;
  mbstate_t st = {0};
  const char* sp = s;
  int ok = 1;
  while (*sp) {
    if (i + 1 >= n) {
      ok = 0;
      break;
    }
    wchar_t wc;
    size_t k = mbrtowc(&wc, sp, 4, &st);
    if (k == (size_t)-1 || k == (size_t)-2 || k == 0) k = 1, wc = (unsigned char)*sp;
    buf[i++] = wc;
    sp += k;
  }
  if (n) buf[i] = 0;
  free(s);
  return ok ? (int)i : -1;
}

int swprintf(wchar_t* restrict buf, size_t n, const wchar_t* restrict fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  int r = vswprintf(buf, n, fmt, ap);
  va_end(ap);
  return r;
}

// ---------------------------------------------------------------------------
// scanf (string input only, which is what the built code uses).

static int is_space(int c) { return c == ' ' || (c >= '\t' && c <= '\r'); }

int vsscanf(const char* restrict str, const char* restrict fmt, va_list ap) {
  const char* s = str;
  int assigned = 0;
  va_list c;
  va_copy(c, ap);
  for (const char* p = fmt; *p; p++) {
    if (is_space((unsigned char)*p)) {
      while (is_space((unsigned char)*s)) s++;
      continue;
    }
    if (*p != '%' || p[1] == '%') {
      if (*p == '%') p++;
      if (*s != *p) break;
      s++;
      continue;
    }
    p++;
    int suppress = 0, width = 0, len = L_NONE;
    if (*p == '*') suppress = 1, p++;
    while (*p >= '0' && *p <= '9') width = width * 10 + (*p++ - '0');
    switch (*p) {
      case 'h': len = p[1] == 'h' ? (p++, L_HH) : L_H; p++; break;
      case 'l': len = p[1] == 'l' ? (p++, L_LL) : L_L; p++; break;
      case 'j': len = L_J; p++; break;
      case 'z': len = L_Z; p++; break;
      case 't': len = L_T; p++; break;
      case 'L': len = L_LD; p++; break;
    }
    char conv = *p;
    if (conv == 'n') {
      if (!suppress) *va_arg(c, int*) = (int)(s - str);
      continue;
    }
    if (conv != 'c' && conv != '[')
      while (is_space((unsigned char)*s)) s++;
    if (!*s) {
      va_end(c);
      return assigned ? assigned : EOF;
    }
    char field[512];
    size_t lim = width ? (size_t)width : sizeof field - 1;
    if (lim > sizeof field - 1) lim = sizeof field - 1;
    if (conv == 'd' || conv == 'i' || conv == 'u' || conv == 'x' || conv == 'X' || conv == 'o' ||
        conv == 'p') {
      size_t k = 0;
      while (k < lim && s[k] && !is_space((unsigned char)s[k])) {
        field[k] = s[k];
        k++;
      }
      field[k] = 0;
      char* end;
      int base = conv == 'd' || conv == 'u' ? 10 : conv == 'i' ? 0 : conv == 'o' ? 8 : 16;
      uint64_t v = (conv == 'd' || conv == 'i') ? (uint64_t)strtoll(field, &end, base)
                                                : strtoull(field, &end, base);
      if (end == field) break;
      s += end - field;
      if (!suppress) {
        if (conv == 'p') *va_arg(c, void**) = (void*)(uintptr_t)v;
        else switch (len) {
            case L_HH: *va_arg(c, char*) = (char)v; break;
            case L_H: *va_arg(c, short*) = (short)v; break;
            case L_NONE: *va_arg(c, int*) = (int)v; break;
            default: *va_arg(c, int64_t*) = (int64_t)v; break;
          }
        assigned++;
      }
    } else if (conv == 'f' || conv == 'e' || conv == 'g' || conv == 'a' || conv == 'E' ||
               conv == 'G') {
      size_t k = 0;
      while (k < lim && s[k] && !is_space((unsigned char)s[k])) {
        field[k] = s[k];
        k++;
      }
      field[k] = 0;
      char* end;
      double v = strtod(field, &end);
      if (end == field) break;
      s += end - field;
      if (!suppress) {
        if (len == L_L) *va_arg(c, double*) = v;
        else if (len == L_LD) *va_arg(c, long double*) = v;
        else *va_arg(c, float*) = (float)v;
        assigned++;
      }
    } else if (conv == 's') {
      char* d = suppress ? NULL : va_arg(c, char*);
      size_t k = 0;
      size_t max = width ? (size_t)width : (size_t)-1;
      while (k < max && *s && !is_space((unsigned char)*s)) {
        if (d) d[k] = *s;
        k++, s++;
      }
      if (d) d[k] = 0, assigned++;
    } else if (conv == 'c') {
      char* d = suppress ? NULL : va_arg(c, char*);
      size_t n = width ? (size_t)width : 1;
      for (size_t k = 0; k < n && *s; k++, s++)
        if (d) d[k] = *s;
      if (d) assigned++;
    } else if (conv == '[') {
      p++;
      int neg = 0;
      if (*p == '^') neg = 1, p++;
      const char* set = p;
      if (*p == ']') p++;
      while (*p && *p != ']') p++;
      size_t setlen = (size_t)(p - set);
      char* d = suppress ? NULL : va_arg(c, char*);
      size_t k = 0;
      size_t max = width ? (size_t)width : (size_t)-1;
      while (k < max && *s) {
        int in = memchr(set, *s, setlen) != NULL;
        if (in == neg) break;
        if (d) d[k] = *s;
        k++, s++;
      }
      if (!k) break;
      if (d) d[k] = 0, assigned++;
    } else {
      break;
    }
  }
  va_end(c);
  return assigned;
}

int sscanf(const char* restrict str, const char* restrict fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  int r = vsscanf(str, fmt, ap);
  va_end(ap);
  return r;
}

void __andruna_stdio_flush_all(void) { fflush(NULL); }

// Wide-character stream functions (UTF-8 streams).
wint_t fputwc(wchar_t wc, FILE* f) {
  char b[4];
  size_t n = utf8_encode(b, (uint32_t)wc);
  return put(f, b, n) ? WEOF : (wint_t)wc;
}
wint_t putwc(wchar_t wc, FILE* f) { return fputwc(wc, f); }

wint_t fgetwc(FILE* f) {
  char b[4];
  mbstate_t st = {0};
  for (int i = 0; i < 4; i++) {
    int c = fgetc(f);
    if (c == EOF) return WEOF;
    b[i] = (char)c;
    wchar_t wc;
    size_t r = mbrtowc(&wc, b + i, 1, &st);
    if (r == (size_t)-1) return WEOF;
    if (r != (size_t)-2) return (wint_t)wc;
  }
  return WEOF;
}
wint_t getwc(FILE* f) { return fgetwc(f); }

wint_t ungetwc(wint_t wc, FILE* f) {
  if (wc == WEOF || wc >= 0x80) return WEOF;  // one byte of push-back
  return ungetc((int)wc, f) == EOF ? WEOF : wc;
}
