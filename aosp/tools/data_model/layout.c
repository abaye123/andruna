/* Sizes and offsets read by compare.py from the assembly of this file. */
#include <stddef.h>
#include <stdint.h>
#include <inttypes.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/resource.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <sys/mman.h>
#include <signal.h>
#include <setjmp.h>
#include <time.h>
#include <pthread.h>
#include <dirent.h>
#include <sys/epoll.h>
#include <poll.h>
#include <ucontext.h>

#define E(name, expr) const long long probe_##name = (long long)(expr);
E(long, sizeof(long))
E(size_t, sizeof(size_t))
E(ssize_t, sizeof(ssize_t))
E(off_t, sizeof(off_t))
E(time_t, sizeof(time_t))
E(int64_t, sizeof(int64_t))
E(intptr_t, sizeof(intptr_t))
E(timespec, sizeof(struct timespec))
E(timespec_nsec_size, sizeof(((struct timespec*)0)->tv_nsec))
E(timeval, sizeof(struct timeval))
E(stat, sizeof(struct stat))
E(stat_st_size, offsetof(struct stat, st_size))
E(stat_st_blksize, offsetof(struct stat, st_blksize))
E(stat_st_blocks, offsetof(struct stat, st_blocks))
E(stat_st_atim, offsetof(struct stat, st_atim))
E(stat_st_mtim, offsetof(struct stat, st_mtim))
E(statvfs, sizeof(struct statvfs))
E(rlimit, sizeof(struct rlimit))
E(fd_set, sizeof(fd_set))
E(sigset_t, sizeof(sigset_t))
E(sigaction, sizeof(struct sigaction))
E(sigaction_mask, offsetof(struct sigaction, sa_mask))
E(siginfo_t, sizeof(siginfo_t))
E(stack_t, sizeof(stack_t))
E(ucontext_t, sizeof(ucontext_t))
E(jmp_buf, sizeof(jmp_buf))
E(pthread_t, sizeof(pthread_t))
E(pthread_attr_t, sizeof(pthread_attr_t))
E(pthread_mutex_t, sizeof(pthread_mutex_t))
E(pthread_cond_t, sizeof(pthread_cond_t))
E(pthread_rwlock_t, sizeof(pthread_rwlock_t))
E(dirent, sizeof(struct dirent))
E(msghdr, sizeof(struct msghdr))
E(cmsghdr, sizeof(struct cmsghdr))
E(iovec, sizeof(struct iovec))
E(epoll_event, sizeof(struct epoll_event))
E(pollfd, sizeof(struct pollfd))
E(wchar_t, sizeof(wchar_t))
E(long_double, sizeof(long double))
E(uc_mcontext, offsetof(ucontext_t, uc_mcontext))
E(uc_sigmask, offsetof(ucontext_t, uc_sigmask))
E(sigaction_flags, offsetof(struct sigaction, sa_flags))
E(rlim_t, sizeof(rlim_t))
E(fd_mask, sizeof(fd_mask))
E(blkcnt_t, sizeof(blkcnt_t))
E(statvfs_f_blocks, offsetof(struct statvfs, f_blocks))
E(greg_t, sizeof(greg_t))
