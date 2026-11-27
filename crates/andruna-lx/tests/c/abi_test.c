/*
 * Calls the andruna-lx exports the way bionic callers do, from C, linked
 * against the crate's static library. Run by tests/c_abi.rs.
 *
 * The declarations below use explicit 64-bit types wherever bionic's LP64
 * ABI has `long` (ssize_t, off_t, time_t, long), because this file is
 * compiled for Windows, where `long` is 32-bit. No CRT headers that declare
 * POSIX names are included, so every call below binds to lx.
 *
 * Usage: abi_test <empty Windows directory to mount at />
 */

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <windows.h>

typedef int64_t lx_long;
typedef int64_t lx_ssize_t;
typedef int64_t lx_off_t;

struct lx_timespec {
    int64_t tv_sec;
    int64_t tv_nsec;
};

#if defined(__x86_64__)
struct lx_stat {
    uint64_t st_dev;
    uint64_t st_ino;
    uint64_t st_nlink;
    uint32_t st_mode;
    uint32_t st_uid;
    uint32_t st_gid;
    uint32_t __pad0;
    uint64_t st_rdev;
    int64_t st_size;
    int64_t st_blksize;
    int64_t st_blocks;
    struct lx_timespec st_atim;
    struct lx_timespec st_mtim;
    struct lx_timespec st_ctim;
    int64_t __pad3[3];
};
_Static_assert(sizeof(struct lx_stat) == 144, "x86_64 struct stat");
_Static_assert(offsetof(struct lx_stat, st_mode) == 24, "st_mode");
_Static_assert(offsetof(struct lx_stat, st_size) == 48, "st_size");
_Static_assert(offsetof(struct lx_stat, st_mtim) == 88, "st_mtim");

#define LX_O_DIRECTORY 0200000
#define NR_read 0
#define NR_getpid 39
#define NR_gettid 186
#define NR_futex 202
#define NR_newfstatat 262
#define NR_clock_gettime 228
#elif defined(__aarch64__)
struct lx_stat {
    uint64_t st_dev;
    uint64_t st_ino;
    uint32_t st_mode;
    uint32_t st_nlink;
    uint32_t st_uid;
    uint32_t st_gid;
    uint64_t st_rdev;
    uint64_t __pad1;
    int64_t st_size;
    int32_t st_blksize;
    int32_t __pad2;
    int64_t st_blocks;
    struct lx_timespec st_atim;
    struct lx_timespec st_mtim;
    struct lx_timespec st_ctim;
    uint32_t __unused4;
    uint32_t __unused5;
};
_Static_assert(sizeof(struct lx_stat) == 128, "arm64 struct stat");
_Static_assert(offsetof(struct lx_stat, st_mode) == 16, "st_mode");
_Static_assert(offsetof(struct lx_stat, st_size) == 48, "st_size");
_Static_assert(offsetof(struct lx_stat, st_mtim) == 88, "st_mtim");

#define LX_O_DIRECTORY 040000
#define NR_read 63
#define NR_getpid 172
#define NR_gettid 178
#define NR_futex 98
#define NR_newfstatat 79
#define NR_clock_gettime 113
#else
#error "unsupported architecture"
#endif

#define LX_O_RDONLY 0
#define LX_O_WRONLY 01
#define LX_O_RDWR 02
#define LX_O_CREAT 0100
#define LX_O_TRUNC 01000
#define LX_O_CLOEXEC 02000000
#define LX_AT_FDCWD (-100)
#define LX_F_GETFD 1
#define LX_F_SETFD 2
#define LX_F_GETFL 3
#define LX_FD_CLOEXEC 1
#define LX_SEEK_SET 0
#define LX_SEEK_CUR 1
#define LX_S_IFMT 0170000
#define LX_S_IFREG 0100000
#define LX_PROT_READ 1
#define LX_PROT_WRITE 2
#define LX_MAP_PRIVATE 0x02
#define LX_MAP_FIXED 0x10
#define LX_MAP_ANONYMOUS 0x20
#define LX_MAP_FAILED ((void *)-1)
#define LX_MADV_DONTNEED 4
#define LX_FUTEX_WAIT_PRIVATE 128
#define LX_FUTEX_WAKE_PRIVATE 129
#define LX_CLOCK_REALTIME 0
#define LX_CLOCK_MONOTONIC 1

#define LX_EBADF 9
#define LX_ENOENT 2
#define LX_EAGAIN 11
#define LX_EFAULT 14
#define LX_ENOTDIR 20
#define LX_EINVAL 22
#define LX_ENOSYS 38
#define LX_ETIMEDOUT 110

extern int *__errno(void);
extern int open(const char *path, int flags, ...);
extern int openat(int dirfd, const char *path, int flags, ...);
extern int close(int fd);
extern lx_ssize_t read(int fd, void *buf, size_t count);
extern lx_ssize_t write(int fd, const void *buf, size_t count);
extern lx_ssize_t pread64(int fd, void *buf, size_t count, lx_off_t offset);
extern lx_ssize_t pwrite64(int fd, const void *buf, size_t count, lx_off_t offset);
extern lx_off_t lseek64(int fd, lx_off_t offset, int whence);
extern int fstat(int fd, struct lx_stat *st);
extern int fstatat(int dirfd, const char *path, struct lx_stat *st, int flags);
extern int dup(int fd);
extern int dup2(int old, int new_fd);
extern int dup3(int old, int new_fd, int flags);
extern int fcntl(int fd, int cmd, ...);
extern void *mmap(void *addr, size_t len, int prot, int flags, int fd, lx_off_t off);
extern int munmap(void *addr, size_t len);
extern int mprotect(void *addr, size_t len, int prot);
extern int madvise(void *addr, size_t len, int advice);
extern lx_long syscall(lx_long number, ...);
extern int clock_gettime(int clk, struct lx_timespec *tp);
extern int nanosleep(const struct lx_timespec *req, struct lx_timespec *rem);
extern int clock_nanosleep(int clk, int flags, const struct lx_timespec *req,
                           struct lx_timespec *rem);
extern int sched_yield(void);
extern int getpid(void);
extern int gettid(void);
extern int andruna_lx_mount(const char *linux_dir, const char *windows_dir);

static int checks;
static int failures;

#define CHECK(cond)                                                    \
    do {                                                               \
        checks++;                                                      \
        if (!(cond)) {                                                 \
            failures++;                                                \
            printf("FAIL line %d: %s (errno %d)\n", __LINE__, #cond,   \
                   *__errno());                                        \
        }                                                              \
    } while (0)

#define CHECK_ERR(expr, err)                                           \
    do {                                                               \
        *__errno() = 0;                                                \
        CHECK((expr) == -1 && *__errno() == (err));                    \
    } while (0)

static int64_t ns(const struct lx_timespec *t) {
    return t->tv_sec * 1000000000LL + t->tv_nsec;
}

static void test_files(void) {
    char buf[32];
    struct lx_stat st, st2;
    int fd = open("/c-test.txt", LX_O_CREAT | LX_O_RDWR | LX_O_TRUNC | LX_O_CLOEXEC, 0644);
    CHECK(fd >= 0);
    CHECK(fcntl(fd, LX_F_GETFD) == LX_FD_CLOEXEC);
    CHECK(fcntl(fd, LX_F_SETFD, 0) == 0);
    CHECK(fcntl(fd, LX_F_GETFD) == 0);
    CHECK((fcntl(fd, LX_F_GETFL) & 3) == LX_O_RDWR);

    CHECK(write(fd, "hello, lx", 9) == 9);
    CHECK(lseek64(fd, 0, LX_SEEK_CUR) == 9);
    memset(buf, 0, sizeof buf);
    CHECK(pread64(fd, buf, 5, 7) == 2 && memcmp(buf, "lx", 2) == 0);
    CHECK(pwrite64(fd, "H", 1, 0) == 1);
    CHECK(lseek64(fd, 0, LX_SEEK_CUR) == 9);

    CHECK(fstat(fd, &st) == 0);
    CHECK((st.st_mode & LX_S_IFMT) == LX_S_IFREG);
    CHECK(st.st_size == 9);
    CHECK(st.st_nlink == 1);
    CHECK(st.st_blksize == 4096);
    CHECK(st.st_mtim.tv_sec > 1700000000);

    /* Duplicates share the offset; FD_CLOEXEC is per descriptor. */
    CHECK(dup3(fd, 10, LX_O_CLOEXEC) == 10);
    CHECK(fcntl(10, LX_F_GETFD) == LX_FD_CLOEXEC);
    CHECK(lseek64(10, 0, LX_SEEK_SET) == 0);
    CHECK(lseek64(fd, 0, LX_SEEK_CUR) == 0);
    memset(buf, 0, sizeof buf);
    CHECK(read(fd, buf, 5) == 5 && memcmp(buf, "Hello", 5) == 0);
    int d = dup(fd);
    CHECK(d >= 0 && d != fd && d != 10);
    CHECK(dup2(d, d) == d);
    CHECK(close(d) == 0);
    CHECK_ERR(dup2(d, 11), LX_EBADF);

    CHECK(fstatat(LX_AT_FDCWD, "/c-test.txt", &st2, 0) == 0);
    CHECK(st2.st_ino == st.st_ino && st2.st_dev == st.st_dev);
    CHECK(syscall(NR_newfstatat, LX_AT_FDCWD, "/c-test.txt", &st2, 0) == 0);
    CHECK(st2.st_size == 9);
    memset(buf, 0, sizeof buf);
    CHECK(syscall(NR_read, fd, buf, 4) == 4 && memcmp(buf, ", lx", 4) == 0);

    CHECK_ERR(open("/missing", LX_O_RDONLY), LX_ENOENT);
    CHECK_ERR(openat(LX_AT_FDCWD, "/c-test.txt", LX_O_RDONLY | LX_O_DIRECTORY), LX_ENOTDIR);
    CHECK_ERR(open(NULL, LX_O_RDONLY), LX_EFAULT);
    CHECK_ERR(fstat(fd, NULL), LX_EFAULT);

    CHECK(close(10) == 0);
    CHECK(close(fd) == 0);
    CHECK_ERR(close(fd), LX_EBADF);
    CHECK_ERR(read(fd, buf, 1), LX_EBADF);
}

static void test_memory(void) {
    char *p = mmap(NULL, 3 * 4096, LX_PROT_READ | LX_PROT_WRITE,
                   LX_MAP_PRIVATE | LX_MAP_ANONYMOUS, -1, 0);
    CHECK(p != LX_MAP_FAILED);
    if (p == LX_MAP_FAILED)
        return;
    CHECK(((uintptr_t)p & 4095) == 0);
    CHECK(p[0] == 0 && p[8191] == 0);
    p[0] = 1;
    p[4096] = 2;
    p[8192] = 3;
    CHECK(munmap(p + 4096, 4096) == 0);
    CHECK(p[0] == 1 && p[8192] == 3);
    CHECK(mprotect(p, 4096, LX_PROT_READ) == 0);
    CHECK(madvise(p + 8192, 4096, LX_MADV_DONTNEED) == 0);
    CHECK(p[8192] == 0);
    /* Map the hole again at a fixed address. */
    char *q = mmap(p + 4096, 4096, LX_PROT_READ | LX_PROT_WRITE,
                   LX_MAP_PRIVATE | LX_MAP_ANONYMOUS | LX_MAP_FIXED, -1, 0);
    CHECK(q == p + 4096);
    q[0] = 9;
    CHECK(munmap(p, 3 * 4096) == 0);

    *__errno() = 0;
    CHECK(mmap(p + 1, 4096, LX_PROT_READ, LX_MAP_PRIVATE | LX_MAP_ANONYMOUS | LX_MAP_FIXED,
               -1, 0) == LX_MAP_FAILED && *__errno() == LX_EINVAL);
    *__errno() = 0;
    /* A file mapping of a descriptor that is not open. */
    CHECK(mmap(NULL, 4096, LX_PROT_READ, LX_MAP_PRIVATE, 77, 0) == LX_MAP_FAILED &&
          *__errno() == LX_EBADF);
}

static volatile uint32_t futex_word;
static volatile LONG waiter_done;

static DWORD WINAPI waiter(LPVOID arg) {
    (void)arg;
    while (futex_word == 0)
        syscall(NR_futex, &futex_word, LX_FUTEX_WAIT_PRIVATE, 0, NULL, NULL, 0);
    InterlockedExchange(&waiter_done, 1);
    return 0;
}

static void test_futex_and_ids(void) {
    uint32_t word = 5;
    struct lx_timespec t = {0, 10 * 1000 * 1000};
    CHECK(syscall(NR_getpid) == getpid());
    CHECK(syscall(NR_gettid) == gettid());
    CHECK(syscall(NR_futex, &word, LX_FUTEX_WAKE_PRIVATE, 1, NULL, NULL, 0) == 0);
    CHECK_ERR(syscall(NR_futex, &word, LX_FUTEX_WAIT_PRIVATE, 4, NULL, NULL, 0), LX_EAGAIN);
    CHECK_ERR(syscall(NR_futex, &word, LX_FUTEX_WAIT_PRIVATE, 5, &t, NULL, 0), LX_ETIMEDOUT);
    CHECK_ERR(syscall(99999), LX_ENOSYS);

    HANDLE h = CreateThread(NULL, 0, waiter, NULL, 0, NULL);
    CHECK(h != NULL);
    Sleep(50);
    CHECK(waiter_done == 0);
    futex_word = 1;
    CHECK(syscall(NR_futex, &futex_word, LX_FUTEX_WAKE_PRIVATE, 1, NULL, NULL, 0) <= 1);
    CHECK(WaitForSingleObject(h, 10000) == WAIT_OBJECT_0);
    CHECK(waiter_done == 1);
    CloseHandle(h);
    CHECK(sched_yield() == 0);
}

static void test_time(void) {
    struct lx_timespec a, b, rt;
    struct lx_timespec d = {0, 5 * 1000 * 1000};
    CHECK(clock_gettime(LX_CLOCK_MONOTONIC, &a) == 0);
    CHECK(nanosleep(&d, NULL) == 0);
    CHECK(clock_gettime(LX_CLOCK_MONOTONIC, &b) == 0);
    CHECK(ns(&b) - ns(&a) >= 5 * 1000 * 1000);
    CHECK(syscall(NR_clock_gettime, LX_CLOCK_REALTIME, &rt) == 0);
    CHECK(rt.tv_sec > 1700000000 && rt.tv_nsec >= 0 && rt.tv_nsec < 1000000000);
    CHECK_ERR(clock_gettime(12345, &a), LX_EINVAL);
    /* clock_nanosleep returns the error instead of setting errno. */
    *__errno() = 0;
    CHECK(clock_nanosleep(LX_CLOCK_MONOTONIC, 0, NULL, NULL) == LX_EFAULT && *__errno() == 0);
    CHECK(clock_nanosleep(LX_CLOCK_MONOTONIC, 0, &d, NULL) == 0);
}

int main(int argc, char **argv) {
    if (argc != 2) {
        printf("usage: abi_test <root dir>\n");
        return 100;
    }
    CHECK(andruna_lx_mount("/", argv[1]) == 0);
    *__errno() = 0;
    CHECK(close(-1) == -1 && *__errno() == LX_EBADF);
    CHECK(__errno() == __errno());

    test_files();
    test_memory();
    test_futex_and_ids();
    test_time();

    printf("lx C ABI: %d checks, %d failures\n", checks, failures);
    return failures == 0 ? 0 : 1;
}
