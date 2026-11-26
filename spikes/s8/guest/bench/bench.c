// Boundary benchmark of spike S8 (guest side, Android ELF).
//
// Each loop makes `n` calls of one kind; the host times the loop and
// divides. Calls to imported functions go through the GOT, as every call
// from AOSP code into libc does.

#include <stdint.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

// Provided by the host: an empty function with the System V convention.
int __andruna_bench_nop(void);

__attribute__((noinline)) int s8_guest_nop(void) {
  __asm__ volatile("");
  return 0;
}

static int (*volatile guest_fn)(void) = s8_guest_nop;

uint64_t s8_loop_guest(uint64_t n) {
  uint64_t s = 0;
  for (uint64_t i = 0; i < n; i++) s += (uint64_t)guest_fn();
  return s;
}

uint64_t s8_loop_host_nop(uint64_t n) {
  uint64_t s = 0;
  for (uint64_t i = 0; i < n; i++) s += (uint64_t)__andruna_bench_nop();
  return s;
}

uint64_t s8_loop_gettid(uint64_t n) {
  uint64_t s = 0;
  for (uint64_t i = 0; i < n; i++) s += (uint64_t)gettid();
  return s;
}

uint64_t s8_loop_clock(uint64_t n) {
  uint64_t s = 0;
  struct timespec ts;
  for (uint64_t i = 0; i < n; i++) {
    clock_gettime(CLOCK_MONOTONIC, &ts);
    s += (uint64_t)ts.tv_nsec;
  }
  return s;
}

uint64_t s8_loop_strlen(uint64_t n, const char* str) {
  uint64_t s = 0;
  for (uint64_t i = 0; i < n; i++) {
    __asm__ volatile("" : "+r"(str));
    s += strlen(str);
  }
  return s;
}

// Thread-local access in the guest: ELF TLS (initial-exec in this object
// is not allowed for a dlopen-able library, so this is general dynamic,
// through __tls_get_addr) and the stack-guard slot read through %fs.
static __thread uint64_t tls_counter;

uint64_t s8_loop_tls(uint64_t n) {
  for (uint64_t i = 0; i < n; i++) {
    __asm__ volatile("");
    tls_counter++;
  }
  return tls_counter;
}

uint64_t s8_read_stack_guard(void) {
#if defined(__x86_64__)
  uint64_t v;
  __asm__ volatile("mov %%fs:0x28, %0" : "=r"(v));
  return v;
#else
  uint64_t tp;
  __asm__ volatile("mrs %0, tpidr_el0" : "=r"(tp));
  return ((uint64_t*)tp)[5];
#endif
}

// A deliberate fault inside guest code, for the debugger checks.
__attribute__((noinline)) int s8_crash_leaf(volatile int* p) { return *p + 1; }
__attribute__((noinline)) int s8_crash_middle(volatile int* p) { return s8_crash_leaf(p) * 2; }
int s8_crash(void) { return s8_crash_middle((volatile int*)0x10); }

// Calls back into the host from a guest frame that saves callee-saved
// registers, for the Windows unwinding check (S8.md, "Debugging").
__attribute__((noinline)) uint64_t s8_call_host(uint64_t (*f)(uint64_t), uint64_t x) {
  uint64_t a = x * 3, b = x ^ 0x55, c = x + 7;
  __asm__ volatile("" : "+r"(a), "+r"(b), "+r"(c));
  uint64_t r = f(x);
  __asm__ volatile("" : "+r"(a), "+r"(b), "+r"(c));
  return r + a + b + c;
}

__attribute__((noinline)) uint64_t s8_call_host_outer(uint64_t (*f)(uint64_t), uint64_t x) {
  return s8_call_host(f, x + 1) * 2;
}
