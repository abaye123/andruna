// Time conversion, error strings and small helpers of the guest libc shim
// (spike S8). Clocks and sleeping come from the host (andruna-lx).

#include <errno.h>
#include <libgen.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

time_t time(time_t* t) {
  struct timespec ts;
  clock_gettime(CLOCK_REALTIME, &ts);
  if (t) *t = ts.tv_sec;
  return ts.tv_sec;
}

int gettimeofday(struct timeval* tv, struct timezone* tz) {
  (void)tz;
  struct timespec ts;
  clock_gettime(CLOCK_REALTIME, &ts);
  if (tv) {
    tv->tv_sec = ts.tv_sec;
    tv->tv_usec = ts.tv_nsec / 1000;
  }
  return 0;
}

clock_t clock(void) {
  struct timespec ts;
  if (clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &ts)) clock_gettime(CLOCK_MONOTONIC, &ts);
  return (clock_t)(ts.tv_sec * CLOCKS_PER_SEC + ts.tv_nsec / (1000000000 / CLOCKS_PER_SEC));
}

unsigned sleep(unsigned s) {
  struct timespec ts = {s, 0};
  nanosleep(&ts, NULL);
  return 0;
}

int usleep(useconds_t us) {
  struct timespec ts = {us / 1000000, (long)(us % 1000000) * 1000};
  return nanosleep(&ts, NULL);
}

// Days from 1970-01-01 to y-m-d (proleptic Gregorian).
static long long days_from_civil(long long y, unsigned m, unsigned d) {
  y -= m <= 2;
  long long era = (y >= 0 ? y : y - 399) / 400;
  unsigned yoe = (unsigned)(y - era * 400);
  unsigned doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
  unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
  return era * 146097 + (long long)doe - 719468;
}

struct tm* gmtime_r(const time_t* restrict t, struct tm* restrict tm) {
  long long s = *t;
  long long days = s / 86400;
  long long rem = s % 86400;
  if (rem < 0) rem += 86400, days--;
  tm->tm_hour = (int)(rem / 3600);
  tm->tm_min = (int)(rem / 60 % 60);
  tm->tm_sec = (int)(rem % 60);
  tm->tm_wday = (int)((days % 7 + 11) % 7);
  long long z = days + 719468;
  long long era = (z >= 0 ? z : z - 146096) / 146097;
  unsigned doe = (unsigned)(z - era * 146097);
  unsigned yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
  long long y = (long long)yoe + era * 400;
  unsigned doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  unsigned mp = (5 * doy + 2) / 153;
  unsigned d = doy - (153 * mp + 2) / 5 + 1;
  unsigned m = mp < 10 ? mp + 3 : mp - 9;
  y += m <= 2;
  tm->tm_year = (int)(y - 1900);
  tm->tm_mon = (int)m - 1;
  tm->tm_mday = (int)d;
  tm->tm_yday = (int)(days - days_from_civil(y, 1, 1));
  tm->tm_isdst = 0;
  tm->tm_gmtoff = 0;
  tm->tm_zone = "UTC";
  return tm;
}

// The guest runs in UTC (no time zone database in the spike).
struct tm* localtime_r(const time_t* restrict t, struct tm* restrict tm) { return gmtime_r(t, tm); }
struct tm* gmtime(const time_t* t) {
  static struct tm tm;
  return gmtime_r(t, &tm);
}
struct tm* localtime(const time_t* t) {
  static struct tm tm;
  return gmtime_r(t, &tm);
}
time_t timegm(struct tm* tm) {
  long long y = tm->tm_year + 1900LL + tm->tm_mon / 12;
  int mon = tm->tm_mon % 12;
  if (mon < 0) mon += 12, y--;
  long long days = days_from_civil(y, (unsigned)mon + 1, 1) + tm->tm_mday - 1;
  time_t t = (time_t)(days * 86400 + tm->tm_hour * 3600LL + tm->tm_min * 60LL + tm->tm_sec);
  gmtime_r(&t, tm);
  return t;
}
time_t mktime(struct tm* tm) { return timegm(tm); }
void tzset(void) {}
char* tzname[2] = {"UTC", "UTC"};
long timezone;
int daylight;

static const char* const wday_names[] = {"Sunday", "Monday", "Tuesday", "Wednesday",
                                         "Thursday", "Friday", "Saturday"};
static const char* const mon_names[] = {"January", "February", "March",     "April",
                                        "May",     "June",     "July",      "August",
                                        "September", "October", "November", "December"};

size_t strftime(char* restrict s, size_t max, const char* restrict f, const struct tm* restrict tm) {
  size_t n = 0;
  char tmp[64];
  for (; *f; f++) {
    const char* add = tmp;
    if (*f != '%') {
      tmp[0] = *f;
      tmp[1] = 0;
    } else {
      f++;
      while (*f == 'E' || *f == 'O' || *f == '-' || *f == '_' || *f == '0') f++;
      switch (*f) {
        case 'a': snprintf(tmp, sizeof tmp, "%.3s", wday_names[tm->tm_wday % 7]); break;
        case 'A': add = wday_names[tm->tm_wday % 7]; break;
        case 'b':
        case 'h': snprintf(tmp, sizeof tmp, "%.3s", mon_names[tm->tm_mon % 12]); break;
        case 'B': add = mon_names[tm->tm_mon % 12]; break;
        case 'c':
          strftime(tmp, sizeof tmp, "%a %b %e %H:%M:%S %Y", tm);
          break;
        case 'C': snprintf(tmp, sizeof tmp, "%02d", (tm->tm_year + 1900) / 100); break;
        case 'd': snprintf(tmp, sizeof tmp, "%02d", tm->tm_mday); break;
        case 'D': strftime(tmp, sizeof tmp, "%m/%d/%y", tm); break;
        case 'e': snprintf(tmp, sizeof tmp, "%2d", tm->tm_mday); break;
        case 'F': strftime(tmp, sizeof tmp, "%Y-%m-%d", tm); break;
        case 'H': snprintf(tmp, sizeof tmp, "%02d", tm->tm_hour); break;
        case 'I': snprintf(tmp, sizeof tmp, "%02d", tm->tm_hour % 12 ? tm->tm_hour % 12 : 12); break;
        case 'j': snprintf(tmp, sizeof tmp, "%03d", tm->tm_yday + 1); break;
        case 'm': snprintf(tmp, sizeof tmp, "%02d", tm->tm_mon + 1); break;
        case 'M': snprintf(tmp, sizeof tmp, "%02d", tm->tm_min); break;
        case 'n': add = "\n"; break;
        case 'p': add = tm->tm_hour < 12 ? "AM" : "PM"; break;
        case 'r': strftime(tmp, sizeof tmp, "%I:%M:%S %p", tm); break;
        case 'R': strftime(tmp, sizeof tmp, "%H:%M", tm); break;
        case 's': {
          struct tm c = *tm;
          snprintf(tmp, sizeof tmp, "%lld", (long long)timegm(&c));
          break;
        }
        case 'S': snprintf(tmp, sizeof tmp, "%02d", tm->tm_sec); break;
        case 't': add = "\t"; break;
        case 'T': strftime(tmp, sizeof tmp, "%H:%M:%S", tm); break;
        case 'u': snprintf(tmp, sizeof tmp, "%d", tm->tm_wday ? tm->tm_wday : 7); break;
        case 'w': snprintf(tmp, sizeof tmp, "%d", tm->tm_wday); break;
        case 'x': strftime(tmp, sizeof tmp, "%m/%d/%y", tm); break;
        case 'X': strftime(tmp, sizeof tmp, "%H:%M:%S", tm); break;
        case 'y': snprintf(tmp, sizeof tmp, "%02d", (tm->tm_year + 1900) % 100); break;
        case 'Y': snprintf(tmp, sizeof tmp, "%d", tm->tm_year + 1900); break;
        case 'z': add = "+0000"; break;
        case 'Z': add = "UTC"; break;
        case '%': add = "%"; break;
        default: snprintf(tmp, sizeof tmp, "%%%c", *f); break;
      }
    }
    size_t l = strlen(add);
    if (n + l >= max) return 0;
    memcpy(s + n, add, l);
    n += l;
  }
  if (n >= max) return 0;
  s[n] = 0;
  return n;
}

size_t strftime_l(char* restrict s, size_t max, const char* restrict f, const struct tm* restrict tm,
                  locale_t l) {
  (void)l;
  return strftime(s, max, f, tm);
}

char* asctime_r(const struct tm* restrict tm, char* restrict buf) {
  strftime(buf, 26, "%a %b %e %H:%M:%S %Y\n", tm);
  return buf;
}
char* ctime_r(const time_t* t, char* buf) {
  struct tm tm;
  return asctime_r(gmtime_r(t, &tm), buf);
}

// Error strings, with bionic's wording for the common values.
static const char* const errors[] = {
    [0] = "Success",
    [EPERM] = "Operation not permitted",
    [ENOENT] = "No such file or directory",
    [ESRCH] = "No such process",
    [EINTR] = "Interrupted system call",
    [EIO] = "I/O error",
    [ENXIO] = "No such device or address",
    [E2BIG] = "Argument list too long",
    [ENOEXEC] = "Exec format error",
    [EBADF] = "Bad file descriptor",
    [ECHILD] = "No child processes",
    [EAGAIN] = "Try again",
    [ENOMEM] = "Out of memory",
    [EACCES] = "Permission denied",
    [EFAULT] = "Bad address",
    [EBUSY] = "Device or resource busy",
    [EEXIST] = "File exists",
    [EXDEV] = "Cross-device link",
    [ENODEV] = "No such device",
    [ENOTDIR] = "Not a directory",
    [EISDIR] = "Is a directory",
    [EINVAL] = "Invalid argument",
    [ENFILE] = "File table overflow",
    [EMFILE] = "Too many open files",
    [ENOTTY] = "Not a typewriter",
    [EFBIG] = "File too large",
    [ENOSPC] = "No space left on device",
    [ESPIPE] = "Illegal seek",
    [EROFS] = "Read-only file system",
    [EMLINK] = "Too many links",
    [EPIPE] = "Broken pipe",
    [EDOM] = "Math argument out of domain of func",
    [ERANGE] = "Math result not representable",
    [EDEADLK] = "Resource deadlock would occur",
    [ENAMETOOLONG] = "File name too long",
    [ENOSYS] = "Function not implemented",
    [ENOTEMPTY] = "Directory not empty",
    [ELOOP] = "Too many symbolic links encountered",
    [EOVERFLOW] = "Value too large for defined data type",
    [EILSEQ] = "Illegal byte sequence",
    [ENOTSOCK] = "Socket operation on non-socket",
    [EOPNOTSUPP] = "Operation not supported on transport endpoint",
    [EAFNOSUPPORT] = "Address family not supported by protocol",
    [ECONNREFUSED] = "Connection refused",
    [ETIMEDOUT] = "Connection timed out",
};

int strerror_r(int e, char* buf, size_t n) {
  const char* m = (e >= 0 && (size_t)e < sizeof errors / sizeof *errors) ? errors[e] : NULL;
  int r = m ? snprintf(buf, n, "%s", m) : snprintf(buf, n, "Unknown error %d", e);
  return (size_t)r >= n ? ERANGE : 0;
}

char* __gnu_strerror_r(int e, char* buf, size_t n) {
  strerror_r(e, buf, n);
  return buf;
}

char* strerror(int e) {
  static __thread char buf[64];
  const char* m = (e >= 0 && (size_t)e < sizeof errors / sizeof *errors) ? errors[e] : NULL;
  if (m) return (char*)m;
  snprintf(buf, sizeof buf, "Unknown error %d", e);
  return buf;
}


const char* strerrorname_np(int e) { (void)e; return NULL; }

char* strsignal(int s) {
  static __thread char buf[32];
  snprintf(buf, sizeof buf, "Signal %d", s);
  return buf;
}

char* basename(const char* path) {
  static __thread char buf[4096];
  size_t l = strlen(path);
  while (l > 1 && path[l - 1] == '/') l--;
  size_t b = l;
  while (b > 0 && path[b - 1] != '/') b--;
  if (l == 0) return strcpy(buf, ".");
  snprintf(buf, sizeof buf, "%.*s", (int)(l - b), path + b);
  return buf;
}

char* dirname(const char* path) {
  static __thread char buf[4096];
  size_t l = strlen(path);
  while (l > 1 && path[l - 1] == '/') l--;
  while (l > 0 && path[l - 1] != '/') l--;
  while (l > 1 && path[l - 1] == '/') l--;
  if (l == 0) return strcpy(buf, ".");
  snprintf(buf, sizeof buf, "%.*s", (int)l, path);
  return buf;
}

// Signal sets are plain bit masks; delivery is not implemented in lx yet.
int sigemptyset(sigset_t* s) {
  memset(s, 0, sizeof *s);
  return 0;
}
int sigfillset(sigset_t* s) {
  memset(s, 0xff, sizeof *s);
  return 0;
}
int sigaddset(sigset_t* s, int n) {
  if (n < 1 || n > 64) return errno = EINVAL, -1;
  ((unsigned long*)s)[0] |= 1UL << (n - 1);
  return 0;
}
int sigdelset(sigset_t* s, int n) {
  if (n < 1 || n > 64) return errno = EINVAL, -1;
  ((unsigned long*)s)[0] &= ~(1UL << (n - 1));
  return 0;
}
int sigismember(const sigset_t* s, int n) {
  if (n < 1 || n > 64) return errno = EINVAL, -1;
  return (((const unsigned long*)s)[0] >> (n - 1)) & 1;
}
int sigemptyset64(sigset64_t* s) {
  memset(s, 0, sizeof *s);
  return 0;
}
int sigaddset64(sigset64_t* s, int n) {
  if (n < 1 || n > 64) return errno = EINVAL, -1;
  ((unsigned long*)s)[0] |= 1UL << (n - 1);
  return 0;
}
int sigfillset64(sigset64_t* s) {
  memset(s, 0xff, sizeof *s);
  return 0;
}

#include <sys/socket.h>

struct cmsghdr* __cmsg_nxthdr(struct msghdr* msg, struct cmsghdr* cmsg) {
  struct cmsghdr* next = (struct cmsghdr*)((char*)cmsg + CMSG_ALIGN(cmsg->cmsg_len));
  size_t end = (size_t)((char*)msg->msg_control + msg->msg_controllen);
  if ((size_t)((char*)(next + 1)) > end || (size_t)((char*)next + CMSG_ALIGN(next->cmsg_len)) > end)
    return NULL;
  return next;
}

// Not implemented in the spike (no caller in the tests that run).
char* strptime(const char* restrict s, const char* restrict f, struct tm* restrict tm) {
  (void)s;
  (void)f;
  (void)tm;
  return NULL;
}

// A few of Android's fixed users (bionic's android_ids), enough for the
// code under test; real bionic also maps app ids.
#include <pwd.h>

static struct {
  const char* name;
  uid_t id;
} android_ids[] = {{"root", 0}, {"system", 1000}, {"shell", 2000}, {"nobody", 9999}};

static struct passwd* make_passwd(int i) {
  static struct passwd pw;
  pw.pw_name = (char*)android_ids[i].name;
  pw.pw_passwd = NULL;
  pw.pw_uid = android_ids[i].id;
  pw.pw_gid = android_ids[i].id;
  pw.pw_dir = "/";
  pw.pw_shell = "/bin/sh";
  return &pw;
}

struct passwd* getpwnam(const char* name) {
  for (int i = 0; i < 4; i++)
    if (strcmp(name, android_ids[i].name) == 0) return make_passwd(i);
  errno = ENOENT;
  return NULL;
}

struct passwd* getpwuid(uid_t uid) {
  for (int i = 0; i < 4; i++)
    if (android_ids[i].id == uid) return make_passwd(i);
  errno = ENOENT;
  return NULL;
}
