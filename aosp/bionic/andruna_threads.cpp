// Thread creation and exit for the Andruna guest bionic.
//
// Replaces bionic's per-architecture assembler files that execute system
// calls whose semantics a Windows process cannot give directly (see
// aosp/bionic/README.md):
//
// - __bionic_clone (arch-*/bionic/__bionic_clone.S): `clone` returns twice,
//   once on a new stack. The host creates a Windows thread instead, with the
//   guest thread pointer and stack bionic prepared, and starts it in
//   bionic's own __start_thread(fn, arg). Only thread clones
//   (CLONE_VM | CLONE_THREAD ...) are supported; the host fails others with
//   ENOSYS.
// - __bionic_clone3: fails with ENOSYS (bionic's pthread_create uses
//   __bionic_clone; clone3(2) callers get the kernel's "not supported").
// - _exit_with_stack_teardown (arch-*/bionic/_exit_with_stack_teardown.S):
//   unmaps the stack the thread is running on and exits. The host switches
//   to its own stack first.
// - vfork (arch-*/bionic/vfork.S): a process without fork cannot vfork; the
//   call goes to the host as the kernel's vfork/clone and fails there with
//   ENOSYS, an honest error.

#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

#include <linux/sched.h>

#include "private/bionic_defs.h"

extern "C" {

// Host functions (aosp/bionic/host_symbols.txt).
int __andruna_clone_thread(int flags, void* child_stack, pid_t* parent_tid, void* tls,
                           pid_t* child_tid, void (*entry)(int (*fn)(void*), void* arg),
                           int (*fn)(void*), void* arg);
__noreturn void __andruna_exit_with_stack_teardown(void* stack, size_t size);

// bionic (clone.cpp, __set_errno.cpp).
__LIBC_HIDDEN__ void __start_thread(int (*fn)(void*), void* arg);
__LIBC_HIDDEN__ long __set_errno_internal(int n);

__LIBC_HIDDEN__ pid_t __bionic_clone(uint32_t flags, void* child_stack, int* parent_tid, void* tls,
                                     int* child_tid, int (*fn)(void*), void* arg) {
  int rc = __andruna_clone_thread(static_cast<int>(flags), child_stack, parent_tid, tls, child_tid,
                                  __start_thread, fn, arg);
  if (rc < 0 && rc >= -4095) {
    return static_cast<pid_t>(__set_errno_internal(rc));
  }
  return rc;
}

__LIBC_HIDDEN__ pid_t __bionic_clone3(clone_args*, size_t, int (*)(void*), void*) {
  errno = ENOSYS;
  return -1;
}

__LIBC_HIDDEN__ __noreturn void _exit_with_stack_teardown(void* stack, size_t size) {
  __andruna_exit_with_stack_teardown(stack, size);
}

pid_t vfork() {
#if defined(__NR_vfork)
  return static_cast<pid_t>(syscall(__NR_vfork));
#else
  return static_cast<pid_t>(syscall(__NR_clone, CLONE_VM | CLONE_VFORK | SIGCHLD, 0, 0, 0, 0));
#endif
}

}  // extern "C"
