# S1: undefined external symbols of the AOSP build (the lx export list)

**Status:** round 2, 2026-11-26. Measured from the static libraries that
`cargo xtask aosp-build` produces for both architectures, and from the
object files of the libart attempt (`--runtime`, 226 to 229 of 238 runtime
sources compile). This replaces the unverified appendix in [S1.md](S1.md).

Regenerate with:

```
cargo xtask aosp-build --arch x64            # and arm64
python aosp/tools/undefined_symbols.py C:/tools/llvm-mingw/bin/llvm-nm.exe out/aosp/x64
cargo xtask aosp-build --arch x64 --runtime --keep-going
python aosp/tools/undefined_symbols.py C:/tools/llvm-mingw/bin/llvm-nm.exe out/aosp/x64 \
    --objects out/aosp/x64/CMakeFiles/libart_runtime.dir
```

A symbol counts as external when no library of the build defines it.
Groups: **libc** (bionic names: andruna-lx and bionic provide them),
**libc++** (C++ library and C++ ABI runtime), **compiler** (compiler
runtime), **art-asm** (ART assembly not built yet), **aosp** (AOSP libraries
not built yet: libnativeloader, libnativebridge, libsigchain,
libunwindstack, libprofile, libelffile, cpu_features, and the runtime files
that did not compile).

## Counts

| Build | libc | libc++ | compiler | art-asm | aosp |
| --- | ---: | ---: | ---: | ---: | ---: |
| Layers 1 and 2, x64 | 186 | 76 | 1 | 0 | 0 |
| Layers 1 and 2, arm64 | 186 | 76 | 2 | 0 | 0 |
| With libart objects, x64 | 268 | 139 | 2 | 259 | 271 |
| With libart objects, arm64 | 255 | 139 | 3 | 168 | 275 |

In all, **270 libc symbols**: 187 for layers 1 and 2
(liblog, libbase, libcutils and libutils parts, libz, liblz4,
libziparchive, libartpalette, libtinyxml2, libartbase, libdexfile) and
83 more for the runtime objects.

Differences between the architectures (libc group, with the runtime
objects): only on x64: `acos`, `asin`, `atan`, `atan2`, `cbrt`, `ceil`, `ceilf`, `cosh`, `expm1`, `floor`, `hypot`, `log10`, `nextafter`, `sinh`, `tanh`; only on arm64: `android_mallopt`, `getauxval`.

## libc symbols by area

"Layer" is where the symbol is first needed: 1-2 for the libraries that
build completely, rt for the libart objects only. "S1" marks symbols the
unverified round 1 list already had.

### Memory (8)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `getpagesize` | 1-2 | x64/arm64 | yes | ziparchive |
| `madvise` | 1-2 | x64/arm64 | yes | artbase, ziparchive |
| `mincore` | 1-2 | x64/arm64 | yes | artbase |
| `mmap` | 1-2 | x64/arm64 | yes | artbase, base, log |
| `mprotect` | 1-2 | x64/arm64 | yes | artbase |
| `mremap` | 1-2 | x64/arm64 | yes | artbase |
| `msync` | 1-2 | x64/arm64 | yes | artbase |
| `munmap` | 1-2 | x64/arm64 | yes | artbase, base, log |

### Allocator (4)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `calloc` | 1-2 | x64/arm64 | yes | artbase, cutils, log, lz4, ziparchive |
| `free` | 1-2 | x64/arm64 | yes | artbase, base, cutils, dexfile, fmt, log, lz4, z, ziparchive |
| `malloc` | 1-2 | x64/arm64 | yes | artbase, cutils, fmt, log, lz4, z |
| `realloc` | 1-2 | x64/arm64 | yes | log |

### Threads and scheduling (40)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `getpriority` | rt | x64/arm64 | yes | runtime |
| `gettid` | 1-2 | x64/arm64 | yes | artbase, base, log |
| `prctl` | 1-2 | x64/arm64 | yes | artbase |
| `pthread_attr_destroy` | rt | x64/arm64 | yes | runtime |
| `pthread_attr_getguardsize` | rt | x64/arm64 | yes | runtime |
| `pthread_attr_getschedparam` | rt | x64/arm64 | yes | runtime |
| `pthread_attr_getstack` | rt | x64/arm64 | yes | runtime |
| `pthread_attr_init` | rt | x64/arm64 | yes | runtime |
| `pthread_attr_setdetachstate` | rt | x64/arm64 | yes | runtime |
| `pthread_attr_setstacksize` | rt | x64/arm64 | yes | runtime |
| `pthread_cond_clockwait` | rt | x64/arm64 | yes | runtime |
| `pthread_create` | rt | x64/arm64 | yes | runtime |
| `pthread_getattr_np` | rt | x64/arm64 | yes | runtime |
| `pthread_getcpuclockid` | rt | x64/arm64 | yes | runtime |
| `pthread_getname_np` | rt | x64/arm64 | yes | runtime |
| `pthread_getspecific` | rt | x64/arm64 | yes | runtime |
| `pthread_gettid_np` | rt | x64/arm64 |  | runtime |
| `pthread_join` | rt | x64/arm64 | yes | runtime |
| `pthread_key_create` | rt | x64/arm64 | yes | runtime |
| `pthread_key_delete` | rt | x64/arm64 | yes | runtime |
| `pthread_kill` | rt | x64/arm64 | yes | runtime |
| `pthread_mutex_destroy` | 1-2 | x64/arm64 | yes | cutils |
| `pthread_mutex_init` | 1-2 | x64/arm64 | yes | cutils |
| `pthread_mutex_lock` | 1-2 | x64/arm64 | yes | cutils |
| `pthread_mutex_trylock` | 1-2 | x64/arm64 | yes | log |
| `pthread_mutex_unlock` | 1-2 | x64/arm64 | yes | cutils, log |
| `pthread_once` | 1-2 | x64/arm64 | yes | cutils, z |
| `pthread_rwlock_destroy` | 1-2 | x64/arm64 | yes | log |
| `pthread_rwlock_init` | 1-2 | x64/arm64 | yes | log |
| `pthread_rwlock_rdlock` | 1-2 | x64/arm64 | yes | log |
| `pthread_rwlock_unlock` | 1-2 | x64/arm64 | yes | log |
| `pthread_rwlock_wrlock` | 1-2 | x64/arm64 | yes | log |
| `pthread_self` | 1-2 | x64/arm64 | yes | artbase |
| `pthread_setname_np` | 1-2 | x64/arm64 | yes | artbase |
| `pthread_setspecific` | rt | x64/arm64 | yes | runtime |
| `sched_getparam` | rt | x64/arm64 |  | runtime |
| `sched_getscheduler` | rt | x64/arm64 | yes | runtime |
| `sched_yield` | rt | x64/arm64 | yes | runtime |
| `setpriority` | rt | x64/arm64 | yes | runtime |
| `unshare` | rt | x64/arm64 |  | runtime |

### Raw system calls (1)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `syscall` | 1-2 | x64/arm64 | yes | artbase, cutils |

### Signals (13)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `__libc_current_sigrtmin` | rt | x64/arm64 |  | runtime |
| `abort` | 1-2 | x64/arm64 | yes | base, fmt, log |
| `android_set_abort_message` | 1-2 | x64/arm64 | yes | base, log |
| `kill` | rt | x64/arm64 | yes | runtime |
| `sigaction` | rt | x64/arm64 | yes | runtime |
| `sigaddset64` | rt | x64/arm64 | yes | runtime |
| `sigaltstack` | rt | x64/arm64 | yes | runtime |
| `sigdelset` | rt | x64/arm64 | yes | runtime |
| `sigemptyset` | rt | x64/arm64 | yes | runtime |
| `sigemptyset64` | rt | x64/arm64 | yes | runtime |
| `sigfillset` | rt | x64/arm64 | yes | runtime |
| `sigwaitinfo64` | rt | x64/arm64 |  | runtime |
| `strsignal` | rt | x64/arm64 | yes | runtime |

### Time (12)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `clock_gettime` | 1-2 | x64/arm64 | yes | artbase, base, log, utils |
| `gettimeofday` | rt | x64/arm64 | yes | runtime |
| `localtime_r` | 1-2 | x64/arm64 | yes | artbase, base, log, ziparchive |
| `mktime` | 1-2 | x64/arm64 | yes | log |
| `nanosleep` | 1-2 | x64/arm64 | yes | artbase |
| `sleep` | 1-2 | x64/arm64 | yes | artbase |
| `strftime` | 1-2 | x64/arm64 | yes | artbase, base, log |
| `strptime` | 1-2 | x64/arm64 |  | log |
| `time` | rt | x64/arm64 | yes | runtime |
| `tzname` | 1-2 | x64/arm64 |  | log |
| `tzset` | 1-2 | x64/arm64 | yes | log |
| `usleep` | rt | x64/arm64 | yes | runtime |

### Files and directories (48)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `access` | 1-2 | x64/arm64 | yes | base, cutils |
| `basename` | 1-2 | x64/arm64 |  | base |
| `chmod` | 1-2 | x64/arm64 | yes | cutils |
| `chown` | 1-2 | x64/arm64 | yes | cutils |
| `close` | 1-2 | x64/arm64 | yes | artbase, base, cutils, log, z |
| `closedir` | 1-2 | x64/arm64 | yes | artbase |
| `dup` | 1-2 | x64/arm64 | yes | base |
| `dup2` | 1-2 | x64/arm64 | yes | base |
| `fallocate` | 1-2 | x64/arm64 | yes | ziparchive |
| `fchmod` | 1-2 | x64/arm64 | yes | base |
| `fchown` | 1-2 | x64/arm64 | yes | base |
| `fcntl` | 1-2 | x64/arm64 | yes | artbase, base, cutils, z |
| `fdatasync` | 1-2 | x64/arm64 | yes | artbase |
| `flock` | 1-2 | x64/arm64 | yes | artbase |
| `fstat` | 1-2 | x64/arm64 | yes | artbase, base, cutils, dexfile, ziparchive |
| `fstatat` | 1-2 | x64/arm64 | yes | cutils |
| `fstatfs` | 1-2 | x64/arm64 | yes | ziparchive |
| `fsync` | 1-2 | x64/arm64 | yes | artbase |
| `ftruncate` | 1-2 | x64/arm64 | yes | base, cutils, ziparchive |
| `ftruncate64` | 1-2 | x64/arm64 | yes | artbase |
| `ioctl` | 1-2 | x64/arm64 | yes | cutils, ziparchive |
| `lseek` | 1-2 | x64/arm64 | yes | artbase, base, cutils, log, z |
| `lseek64` | 1-2 | x64/arm64 | yes | ziparchive |
| `lstat` | 1-2 | x64/arm64 | yes | base, cutils |
| `mkdir` | 1-2 | x64/arm64 | yes | artbase, cutils |
| `mkdirat` | 1-2 | x64/arm64 | yes | cutils |
| `mkdtemp` | 1-2 | x64/arm64 | yes | base |
| `mkstemp` | 1-2 | x64/arm64 | yes | base, cutils |
| `nftw` | 1-2 | x64/arm64 |  | base |
| `open` | 1-2 | x64/arm64 | yes | artbase, base, cutils, log, z, ziparchive |
| `openat` | 1-2 | x64/arm64 | yes | cutils |
| `opendir` | 1-2 | x64/arm64 | yes | artbase |
| `pipe2` | rt | x64/arm64 | yes | runtime |
| `poll` | 1-2 | x64/arm64 | yes | log |
| `pread` | 1-2 | x64/arm64 |  | artbase, base |
| `pread64` | 1-2 | x64/arm64 | yes | artbase |
| `pwrite` | 1-2 | x64/arm64 |  | artbase, base |
| `pwrite64` | 1-2 | x64/arm64 | yes | artbase |
| `read` | 1-2 | x64/arm64 | yes | artbase, base, cutils, log, z |
| `readdir` | 1-2 | x64/arm64 | yes | artbase, base |
| `readlink` | 1-2 | x64/arm64 | yes | base |
| `realpath` | 1-2 | x64/arm64 | yes | base, dexfile |
| `rename` | 1-2 | x64/arm64 | yes | artbase, cutils |
| `rmdir` | 1-2 | x64/arm64 | yes | base |
| `stat` | 1-2 | x64/arm64 | yes | artbase, cutils |
| `unlink` | 1-2 | x64/arm64 | yes | artbase, base, cutils |
| `write` | 1-2 | x64/arm64 | yes | artbase, base, cutils, log, z |
| `writev` | 1-2 | x64/arm64 | yes | log |

### stdio (27)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `asprintf` | 1-2 | x64/arm64 |  | cutils |
| `fclose` | 1-2 | x64/arm64 | yes | cutils, tinyxml2 |
| `ferror` | 1-2 | x64/arm64 |  | tinyxml2 |
| `fflush` | 1-2 | x64/arm64 | yes | base, ziparchive |
| `fgetc` | 1-2 | x64/arm64 |  | tinyxml2 |
| `fgets` | 1-2 | x64/arm64 | yes | cutils |
| `fileno` | 1-2 | x64/arm64 | yes | ziparchive |
| `fopen` | 1-2 | x64/arm64 | yes | cutils, log, tinyxml2 |
| `fprintf` | 1-2 | x64/arm64 | yes | base, fmt, log |
| `fputc` | 1-2 | x64/arm64 |  | fmt, tinyxml2 |
| `fputs` | 1-2 | x64/arm64 | yes | base |
| `fread` | 1-2 | x64/arm64 | yes | tinyxml2 |
| `fseeko` | 1-2 | x64/arm64 |  | ziparchive |
| `fseeko64` | 1-2 | x64/arm64 |  | tinyxml2 |
| `ftello64` | 1-2 | x64/arm64 |  | tinyxml2 |
| `fwrite` | 1-2 | x64/arm64 | yes | fmt, log, tinyxml2, ziparchive |
| `getline` | 1-2 | x64/arm64 |  | log |
| `pclose` | 1-2 | x64/arm64 |  | log |
| `perror` | 1-2 | x64/arm64 |  | log |
| `popen` | 1-2 | x64/arm64 |  | log |
| `puts` | 1-2 | x64/arm64 |  | tinyxml2 |
| `snprintf` | 1-2 | x64/arm64 |  | artbase, base, cutils, log, tinyxml2, z |
| `sscanf` | 1-2 | x64/arm64 |  | artbase, base, cutils, tinyxml2 |
| `stderr` | 1-2 | x64/arm64 | yes | base, fmt, log |
| `stdout` | 1-2 | x64/arm64 | yes | base, fmt, tinyxml2 |
| `vfprintf` | 1-2 | x64/arm64 | yes | tinyxml2 |
| `vsnprintf` | 1-2 | x64/arm64 |  | base, log, tinyxml2, z |

### Process and environment (23)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `_exit` | rt | x64/arm64 | yes | runtime |
| `atexit` | 1-2 | x64/arm64 | yes | artbase, artpalette, cutils, dexfile |
| `execv` | rt | x64/arm64 | yes | runtime |
| `execve` | rt | x64/arm64 | yes | runtime |
| `exit` | 1-2 | x64/arm64 | yes | log |
| `fork` | rt | x64/arm64 | yes | runtime |
| `getegid` | 1-2 | x64/arm64 | yes | log |
| `getenv` | 1-2 | x64/arm64 | yes | artbase, base, log |
| `geteuid` | 1-2 | x64/arm64 | yes | log |
| `getgid` | 1-2 | x64/arm64 | yes | log |
| `getgroups` | 1-2 | x64/arm64 | yes | log |
| `getpid` | 1-2 | x64/arm64 | yes | artbase, base, cutils, log |
| `getprogname` | 1-2 | x64/arm64 | yes | base, log |
| `getpwnam` | 1-2 | x64/arm64 |  | artbase |
| `getpwuid` | 1-2 | x64/arm64 |  | log |
| `getuid` | 1-2 | x64/arm64 | yes | artbase, log |
| `pidfd_open` | rt | x64/arm64 |  | runtime |
| `setenv` | 1-2 | x64/arm64 | yes | log |
| `setpgid` | rt | x64/arm64 |  | runtime |
| `sysconf` | 1-2 | x64/arm64 | yes | artbase, base |
| `uname` | 1-2 | x64/arm64 | yes | artbase |
| `unsetenv` | 1-2 | x64/arm64 | yes | log |
| `waitid` | rt | x64/arm64 |  | runtime |

### Dynamic loading (8)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `android_dlopen_ext` | rt | x64/arm64 | yes | runtime |
| `android_get_exported_namespace` | rt | x64/arm64 | yes | runtime |
| `dl_iterate_phdr` | rt | x64/arm64 | yes | runtime |
| `dladdr` | rt | x64/arm64 | yes | runtime |
| `dlclose` | rt | x64/arm64 | yes | runtime |
| `dlerror` | rt | x64/arm64 | yes | runtime |
| `dlopen` | rt | x64/arm64 | yes | runtime |
| `dlsym` | rt | x64/arm64 | yes | runtime |

### Sockets (8)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `__cmsg_nxthdr` | 1-2 | x64/arm64 |  | base |
| `connect` | 1-2 | x64/arm64 | yes | log |
| `getsockopt` | 1-2 | x64/arm64 | yes | artbase |
| `recv` | 1-2 | x64/arm64 | yes | log |
| `recvmsg` | 1-2 | x64/arm64 | yes | base |
| `sendmsg` | 1-2 | x64/arm64 | yes | base |
| `setsockopt` | 1-2 | x64/arm64 | yes | log |
| `socket` | 1-2 | x64/arm64 | yes | log |

### Android specifics (14)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `__system_property_area_serial` | 1-2 | x64/arm64 | yes | base, log |
| `__system_property_find` | 1-2 | x64/arm64 | yes | base, cutils, log |
| `__system_property_foreach` | 1-2 | x64/arm64 | yes | cutils |
| `__system_property_get` | 1-2 | x64/arm64 | yes | cutils, log |
| `__system_property_read` | 1-2 | x64/arm64 | yes | log |
| `__system_property_read_callback` | 1-2 | x64/arm64 | yes | base, cutils |
| `__system_property_serial` | 1-2 | x64/arm64 | yes | base, cutils, log |
| `__system_property_set` | 1-2 | x64/arm64 | yes | base, cutils |
| `__system_property_wait` | 1-2 | x64/arm64 | yes | base |
| `android_fdsan_get_owner_tag` | 1-2 | x64/arm64 |  | cutils |
| `android_get_application_target_sdk_version` | 1-2 | x64/arm64 |  | cutils |
| `android_get_device_api_level` | 1-2 | x64/arm64 |  | artbase |
| `android_mallopt` | rt | arm64 |  | runtime |
| `android_set_application_target_sdk_version` | rt | x64/arm64 |  | runtime |

### Errors, strings, numbers (40)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `__assert2` | 1-2 | x64/arm64 |  | cutils, log |
| `__errno` | 1-2 | x64/arm64 | yes | artbase, artpalette, base, cutils, dexfile, fmt, log, utils, z, ziparchive |
| `arc4random` | rt | x64/arm64 |  | runtime |
| `arc4random_buf` | 1-2 | x64/arm64 |  | artbase |
| `atoi` | 1-2 | x64/arm64 |  | artbase, base, dexfile |
| `fnmatch` | 1-2 | x64/arm64 |  | cutils |
| `mbrtowc` | 1-2 | x64/arm64 |  | log |
| `memchr` | 1-2 | x64/arm64 |  | artbase, base, dexfile, log, z |
| `memcmp` | 1-2 | x64/arm64 |  | artbase, base, dexfile, fmt, log, ziparchive |
| `memcpy` | 1-2 | x64/arm64 |  | artbase, base, cutils, dexfile, fmt, log, lz4, tinyxml2, z, ziparchive |
| `memmove` | 1-2 | x64/arm64 |  | artbase, base, cutils, dexfile, fmt, log, lz4, z, ziparchive |
| `memset` | 1-2 | x64/arm64 |  | artbase, artpalette, base, dexfile, fmt, log, lz4, z, ziparchive |
| `rand` | rt | x64/arm64 |  | runtime |
| `srand` | rt | x64/arm64 |  | runtime |
| `strcasecmp` | 1-2 | x64/arm64 |  | log |
| `strchr` | 1-2 | x64/arm64 |  | base, cutils, log, tinyxml2 |
| `strcmp` | 1-2 | x64/arm64 |  | artbase, cutils, dexfile, log |
| `strcpy` | 1-2 | x64/arm64 |  | log |
| `strcspn` | 1-2 | x64/arm64 |  | log |
| `strdup` | 1-2 | x64/arm64 |  | cutils, log |
| `strerror` | 1-2 | x64/arm64 | yes | artbase, base, cutils, dexfile, log, utils, z, ziparchive |
| `strerror_r` | 1-2 | x64/arm64 | yes | base |
| `strlcpy` | 1-2 | x64/arm64 |  | base, cutils, log |
| `strlen` | 1-2 | x64/arm64 |  | artbase, base, cutils, dexfile, fmt, log, tinyxml2, z |
| `strncasecmp` | 1-2 | x64/arm64 |  | base |
| `strncmp` | 1-2 | x64/arm64 |  | artbase, log, tinyxml2 |
| `strncpy` | 1-2 | x64/arm64 |  | artbase |
| `strndup` | 1-2 | x64/arm64 |  | cutils |
| `strnlen` | 1-2 | x64/arm64 |  | log |
| `strpbrk` | 1-2 | x64/arm64 |  | log |
| `strrchr` | 1-2 | x64/arm64 |  | artbase, base, log |
| `strstr` | 1-2 | x64/arm64 |  | log |
| `strtod` | rt | x64/arm64 | yes | runtime |
| `strtof` | 1-2 | x64/arm64 | yes | cutils |
| `strtoimax` | 1-2 | x64/arm64 |  | cutils |
| `strtok_r` | 1-2 | x64/arm64 |  | cutils |
| `strtol` | 1-2 | x64/arm64 | yes | cutils |
| `strtoll` | 1-2 | x64/arm64 | yes | artbase, base, log |
| `strtoul` | 1-2 | x64/arm64 | yes | artbase, log |
| `strtoull` | 1-2 | x64/arm64 | yes | artbase, base, cutils, dexfile, log |

### Math (23)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `acos` | rt | x64 |  | runtime |
| `asin` | rt | x64 |  | runtime |
| `atan` | rt | x64 |  | runtime |
| `atan2` | rt | x64 |  | runtime |
| `cbrt` | rt | x64 |  | runtime |
| `ceil` | rt | x64 |  | runtime |
| `ceilf` | 1-2 | x64 |  | artbase, log |
| `cos` | rt | x64/arm64 |  | runtime |
| `cosh` | rt | x64 |  | runtime |
| `exp` | rt | x64/arm64 |  | runtime |
| `expm1` | rt | x64 |  | runtime |
| `floor` | rt | x64 |  | runtime |
| `fmod` | rt | x64/arm64 |  | runtime |
| `fmodf` | rt | x64/arm64 |  | runtime |
| `hypot` | rt | x64 |  | runtime |
| `log` | rt | x64/arm64 |  | runtime |
| `log10` | rt | x64 |  | runtime |
| `nextafter` | rt | x64 |  | runtime |
| `pow` | rt | x64/arm64 |  | runtime |
| `sin` | rt | x64/arm64 |  | runtime |
| `sinh` | rt | x64 |  | runtime |
| `tan` | rt | x64/arm64 |  | runtime |
| `tanh` | rt | x64 |  | runtime |

### Other (1)

| Symbol | Layer | Arch | S1 | Used by |
| --- | --- | --- | --- | --- |
| `getauxval` | 1-2 | arm64 | yes | z |

## Compared with the round 1 list

**Needed but not in the round 1 list (91):** `__assert2`, `__cmsg_nxthdr`, `__libc_current_sigrtmin`, `acos`, `android_fdsan_get_owner_tag`, `android_get_application_target_sdk_version`, `android_get_device_api_level`, `android_mallopt`, `android_set_application_target_sdk_version`, `arc4random`, `arc4random_buf`, `asin`, `asprintf`, `atan`, `atan2`, `atoi`, `basename`, `cbrt`, `ceil`, `ceilf`, `cos`, `cosh`, `exp`, `expm1`, `ferror`, `fgetc`, `floor`, `fmod`, `fmodf`, `fnmatch`, `fputc`, `fseeko`, `fseeko64`, `ftello64`, `getline`, `getpwnam`, `getpwuid`, `hypot`, `log`, `log10`, `mbrtowc`, `memchr`, `memcmp`, `memcpy`, `memmove`, `memset`, `nextafter`, `nftw`, `pclose`, `perror`, `pidfd_open`, `popen`, `pow`, `pread`, `pthread_gettid_np`, `puts`, `pwrite`, `rand`, `sched_getparam`, `setpgid`, `sigwaitinfo64`, `sin`, `sinh`, `snprintf`, `srand`, `sscanf`, `strcasecmp`, `strchr`, `strcmp`, `strcpy`, `strcspn`, `strdup`, `strlcpy`, `strlen`, `strncasecmp`, `strncmp`, `strncpy`, `strndup`, `strnlen`, `strpbrk`, `strptime`, `strrchr`, `strstr`, `strtoimax`, `strtok_r`, `tan`, `tanh`, `tzname`, `unshare`, `vsnprintf`, `waitid`.

Most are pure computation (string, memory, math functions) that round 1
left out on purpose. The ones that matter for lx are `__libc_current_sigrtmin`,
`pidfd_open`, `unshare`, `setpgid`, `waitid`, `sigwaitinfo64`,
`pthread_gettid_np`, `sched_getparam`, `android_mallopt` (arm64),
`android_get_application_target_sdk_version`,
`android_set_application_target_sdk_version`, `android_get_device_api_level`,
`android_fdsan_get_owner_tag`, `getpwnam`/`getpwuid`, `fnmatch`, `nftw`,
`getline`, `popen`/`pclose`, `strptime`, `arc4random`/`arc4random_buf`, and
the data symbol `tzname` (`stdout` and `stderr` are data symbols too).

**In the round 1 list but not referenced yet:** most of it. The modules
built so far do not use epoll, eventfd, timerfd, the networking calls
beyond `socket`/`connect`/`recv`/`recvmsg`/`sendmsg`/`getsockopt`/`setsockopt`,
`memfd_create` (libartbase calls it through `syscall`), `futex` (through
`syscall`, so it does not show as a symbol), `sigprocmask`,
`sigaltstack` (runtime only), `dl_iterate_phdr` (runtime only), the
`android_*` namespace calls beyond `android_get_exported_namespace`
(they are in libnativeloader, not built yet), `getrlimit`, `getauxval` on
x64, or `/proc` readers (files are opened with plain `open`). libcore
natives, libnativeloader, libsigchain and libunwindstack will add to the
list; they are not built yet.

## libc++ and compiler runtime

139 libc++/libc++abi symbols (std::string,
streams, locale, `std::mutex`, `std::condition_variable`, `operator new`
and `delete`, `__cxa_guard_*`, `__cxa_pure_virtual`, RTTI vtables). They
are compiled against libc++ headers in their Linux configuration, so they
must come from a libc++ built for bionic with the same flags, not from
llvm-mingw's Windows libc++ (see aosp/README.md).

Compiler runtime: `___chkstk_ms` (Windows stack probes, from the Windows
target), `__udivti3` (x64 runtime), `__clear_cache` (arm64), and
`_tls_index` (Windows TLS for `thread_local`, libziparchive). Those come
from compiler-rt and the mingw start-up objects. Note what the Windows
target implies: `thread_local` uses the PE TLS directory, not ELF TLS, and
stack probes call `___chkstk_ms`.

## art-asm and aosp groups (runtime)

art-asm: the `art_quick_*` and `art_jni_*` entry points, nterp
(`ExecuteNterpImpl`, `artNterpAsmInstruction*`), `ExecuteSwitchImplAsm`,
`__memcmp16`. They come from `runtime/arch/<arch>/*.S` and the generated
interpreter, which need `asm_defines.h` (cpp-define-generator) and, on
x64, the thread register decision (S1 round 1, risk 1).

aosp: libnativeloader (`OpenNativeLibrary`, `InitializeNativeLoader`, ...),
libnativebridge (`LoadNativeBridge`, `NativeBridge*`, ...), libsigchain
(`AddSpecialSignalHandlerFn`, `EnsureFrontOfChain`, ...), libunwindstack,
libprofile (`ProfileCompilationInfo`), libelffile, cpu_features, the
compiler (`jit_create.h`), and the symbols of the runtime files that did
not compile (`Runtime::*` from runtime.cc, `Heap`, `MarkCompact`).
