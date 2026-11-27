// Clocks through bionic: clock_gettime, clock_getres, gettimeofday and
// time (the loader's vDSO when it is there) against the clock_gettime
// system call, and the cost of each path. Prints "vdso: yes|no" (from
// AT_SYSINFO_EHDR), one "ns/call" line per path and "clock_test passed".
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/auxv.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static int failures = 0;

#define CHECK(c)                                              \
  do {                                                        \
    if (!(c)) {                                               \
      printf("FAIL %s:%d %s\n", __FILE__, __LINE__, #c);      \
      failures++;                                             \
    }                                                         \
  } while (0)

static int64_t ns(const struct timespec* t) {
  return (int64_t)t->tv_sec * 1000000000LL + t->tv_nsec;
}

static int64_t mono(void) {
  struct timespec t;
  syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &t);
  return ns(&t);
}

#define N 200000

int main(void) {
  printf("vdso: %s\n", getauxval(AT_SYSINFO_EHDR) ? "yes" : "no");

  // Values agree with the system call, clock by clock.
  const clockid_t clocks[] = {CLOCK_REALTIME, CLOCK_MONOTONIC, CLOCK_MONOTONIC_RAW,
                              CLOCK_REALTIME_COARSE, CLOCK_MONOTONIC_COARSE, CLOCK_BOOTTIME,
                              CLOCK_PROCESS_CPUTIME_ID, CLOCK_THREAD_CPUTIME_ID};
  for (unsigned i = 0; i < sizeof(clocks) / sizeof(clocks[0]); i++) {
    struct timespec a, b, c, ra, rb;
    CHECK(syscall(SYS_clock_gettime, clocks[i], &a) == 0);
    CHECK(clock_gettime(clocks[i], &b) == 0);
    CHECK(syscall(SYS_clock_gettime, clocks[i], &c) == 0);
    CHECK(b.tv_nsec >= 0 && b.tv_nsec < 1000000000);
    CHECK(ns(&a) <= ns(&b) && ns(&b) <= ns(&c));
    CHECK(syscall(SYS_clock_getres, clocks[i], &ra) == 0);
    CHECK(clock_getres(clocks[i], &rb) == 0);
    CHECK(ra.tv_sec == rb.tv_sec && ra.tv_nsec == rb.tv_nsec);
  }
  // Errors are the system call's.
  struct timespec t;
  errno = 0;
  CHECK(clock_gettime(12345, &t) == -1 && errno == EINVAL);
  errno = 0;
  CHECK(clock_getres(12345, &t) == -1 && errno == EINVAL);
  CHECK(clock_getres(CLOCK_MONOTONIC, NULL) == 0);

  // Wall clock: gettimeofday and time follow CLOCK_REALTIME.
  struct timeval tv;
  struct timezone tz = {7, 1};
  struct timespec r0, r1;
  clock_gettime(CLOCK_REALTIME, &r0);
  CHECK(gettimeofday(&tv, &tz) == 0);
  time_t now = time(NULL);
  clock_gettime(CLOCK_REALTIME, &r1);
  int64_t us = (int64_t)tv.tv_sec * 1000000 + tv.tv_usec;
  CHECK(us >= ns(&r0) / 1000 && us <= ns(&r1) / 1000);
  CHECK(tv.tv_usec >= 0 && tv.tv_usec < 1000000);
  CHECK(tz.tz_minuteswest == 0 && tz.tz_dsttime == 0);
  CHECK(now >= r0.tv_sec && now <= r1.tv_sec);
  time_t stored = 0;
  CHECK(time(&stored) == stored && stored >= now);

  // Cost per call.
  int64_t t0 = mono();
  for (int i = 0; i < N; i++) clock_gettime(CLOCK_MONOTONIC, &t);
  int64_t t1 = mono();
  for (int i = 0; i < N; i++) syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &t);
  int64_t t2 = mono();
  for (int i = 0; i < N; i++) gettimeofday(&tv, NULL);
  int64_t t3 = mono();
  for (int i = 0; i < N; i++) syscall(SYS_gettid);
  int64_t t4 = mono();
  for (int i = 0; i < N; i++) getpid();
  int64_t t5 = mono();
  printf("clock_gettime(CLOCK_MONOTONIC): %.1f ns/call\n", (double)(t1 - t0) / N);
  printf("syscall(SYS_clock_gettime): %.1f ns/call\n", (double)(t2 - t1) / N);
  printf("gettimeofday: %.1f ns/call\n", (double)(t3 - t2) / N);
  printf("syscall(SYS_gettid): %.1f ns/call\n", (double)(t4 - t3) / N);
  printf("getpid (bionic caches it): %.1f ns/call\n", (double)(t5 - t4) / N);

  if (failures == 0) printf("clock_test passed\n");
  return failures == 0 ? 0 : 1;
}
