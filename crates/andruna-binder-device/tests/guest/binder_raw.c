// A guest program that uses /dev/binder through bionic's system calls
// (open, mmap, ioctl), with the binder protocol written by hand, as
// libbinder's kernel IPC code does.
//
//   binder_raw manager            become the context manager and serve:
//                                 code 1 echoes, code 2 reads a passed
//                                 file descriptor, code 3 returns the
//                                 caller's pid and uid, code 99 exits
//   binder_raw client <iters>     call the manager: identity, round trips
//                                 (prints ROUNDTRIP lines), fd passing,
//                                 death notification

#include <errno.h>
#include <fcntl.h>
#include <linux/android/binder.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#define CHECK(c)                                                          \
  do {                                                                    \
    if (!(c)) {                                                           \
      fprintf(stderr, "FAIL %s:%d: %s (errno %d)\n", __FILE__, __LINE__, #c, \
              errno);                                                     \
      exit(1);                                                            \
    }                                                                     \
  } while (0)

static int fd;

static void open_binder(void) {
  fd = open("/dev/binder", O_RDWR | O_CLOEXEC);
  CHECK(fd >= 0);
  struct binder_version v;
  CHECK(ioctl(fd, BINDER_VERSION, &v) == 0);
  CHECK(v.protocol_version == BINDER_CURRENT_PROTOCOL_VERSION);
  void* m = mmap(NULL, 1024 * 1024 - 8192, PROT_READ, MAP_PRIVATE | MAP_NORESERVE, fd, 0);
  CHECK(m != MAP_FAILED);
}

static uint8_t rbuf[256];

// One BINDER_WRITE_READ; returns the bytes read.
static size_t wr(const void* w, size_t wlen, int read) {
  struct binder_write_read bwr = {
      .write_size = wlen,
      .write_buffer = (uintptr_t)w,
      .read_size = read ? sizeof(rbuf) : 0,
      .read_buffer = (uintptr_t)rbuf,
  };
  CHECK(ioctl(fd, BINDER_WRITE_READ, &bwr) == 0);
  CHECK(bwr.write_consumed == wlen);
  return bwr.read_consumed;
}

struct out {
  uint8_t b[512];
  size_t n;
};

static void put(struct out* o, const void* p, size_t n) {
  memcpy(o->b + o->n, p, n);
  o->n += n;
}

static void put32(struct out* o, uint32_t v) { put(o, &v, 4); }

static void put_tx(struct out* o, uint32_t cmd, uint32_t handle, uint32_t code, uint32_t flags,
                   const void* data, size_t len, const binder_size_t* offs, size_t noffs) {
  struct binder_transaction_data tr;
  memset(&tr, 0, sizeof(tr));
  tr.target.handle = handle;
  tr.code = code;
  tr.flags = flags;
  tr.data_size = len;
  tr.offsets_size = noffs * sizeof(binder_size_t);
  tr.data.ptr.buffer = (uintptr_t)data;
  tr.data.ptr.offsets = (uintptr_t)offs;
  put32(o, cmd);
  put(o, &tr, sizeof(tr));
}

static void free_buffer(struct out* o, binder_uintptr_t b) {
  put32(o, BC_FREE_BUFFER);
  put(o, &b, sizeof(b));
}

static void manager(void) {
  open_binder();
  struct flat_binder_object obj;
  memset(&obj, 0, sizeof(obj));
  obj.flags = FLAT_BINDER_FLAG_ACCEPTS_FDS;
  CHECK(ioctl(fd, BINDER_SET_CONTEXT_MGR_EXT, &obj) == 0);
  struct out o = {.n = 0};
  put32(&o, BC_ENTER_LOOPER);
  static uint8_t reply[8192];
  for (;;) {
    size_t n = wr(o.b, o.n, 1);
    o.n = 0;
    for (size_t pos = 0; pos < n;) {
      uint32_t cmd;
      memcpy(&cmd, rbuf + pos, 4);
      pos += 4 + _IOC_SIZE(cmd);
      if (cmd != BR_TRANSACTION && cmd != BR_TRANSACTION_SEC_CTX) continue;
      struct binder_transaction_data tr;
      memcpy(&tr, rbuf + pos - _IOC_SIZE(cmd), sizeof(tr));
      const uint8_t* data = (const uint8_t*)(uintptr_t)tr.data.ptr.buffer;
      size_t len = tr.data_size;
      if (tr.code == 99) exit(0);
      if (tr.code == 2) {
        const binder_size_t* offs = (const binder_size_t*)(uintptr_t)tr.data.ptr.offsets;
        const struct binder_fd_object* f = (const struct binder_fd_object*)(data + offs[0]);
        CHECK(f->hdr.type == BINDER_TYPE_FD);
        lseek(f->fd, 0, SEEK_SET);
        ssize_t r = read(f->fd, reply, sizeof(reply));
        CHECK(r > 0);
        close(f->fd);
        len = (size_t)r;
      } else if (tr.code == 3) {
        memcpy(reply, &tr.sender_pid, 4);
        memcpy(reply + 4, &tr.sender_euid, 4);
        len = 8;
      } else {
        if (len > sizeof(reply)) len = sizeof(reply);
        memcpy(reply, data, len);
      }
      free_buffer(&o, tr.data.ptr.buffer);
      if (!(tr.flags & TF_ONE_WAY)) put_tx(&o, BC_REPLY, 0, 0, 0, reply, len, NULL, 0);
    }
  }
}

// Calls handle 0; copies the reply into `out` and returns its size.
static size_t call(uint32_t code, uint32_t flags, const void* data, size_t len,
                   const binder_size_t* offs, size_t noffs, void* out) {
  struct out o = {.n = 0};
  put_tx(&o, BC_TRANSACTION, 0, code, flags, data, len, offs, noffs);
  int first = 1;
  for (;;) {
    size_t n = wr(o.b, first ? o.n : 0, 1);
    first = 0;
    for (size_t pos = 0; pos < n;) {
      uint32_t cmd;
      memcpy(&cmd, rbuf + pos, 4);
      pos += 4 + _IOC_SIZE(cmd);
      CHECK(cmd != BR_DEAD_REPLY && cmd != BR_FAILED_REPLY);
      if (cmd != BR_REPLY) continue;
      struct binder_transaction_data tr;
      memcpy(&tr, rbuf + pos - _IOC_SIZE(cmd), sizeof(tr));
      memcpy(out, (const void*)(uintptr_t)tr.data.ptr.buffer, tr.data_size);
      struct out f = {.n = 0};
      free_buffer(&f, tr.data.ptr.buffer);
      wr(f.b, f.n, 0);
      return tr.data_size;
    }
  }
}

static double now_us(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec * 1e6 + t.tv_nsec / 1e3;
}

static int cmp(const void* a, const void* b) {
  double x = *(const double*)a, y = *(const double*)b;
  return x < y ? -1 : x > y;
}

static void client(int iters) {
  open_binder();
  uint8_t r[8192];
  CHECK(call(3, 0, "", 0, NULL, 0, r) == 8);
  int32_t pid;
  uint32_t uid;
  memcpy(&pid, r, 4);
  memcpy(&uid, r + 4, 4);
  printf("IDENTITY pid=%d (getpid %d) uid=%u\n", pid, getpid(), uid);
  CHECK(pid == getpid());

  static uint8_t payload[4096];
  memset(payload, 0x5a, sizeof(payload));
  double* t = malloc(sizeof(double) * iters);
  size_t sizes[] = {100, 4096};
  for (int s = 0; s < 2; s++) {
    for (int i = 0; i < 500; i++) call(1, 0, payload, sizes[s], NULL, 0, r);
    for (int i = 0; i < iters; i++) {
      double a = now_us();
      CHECK(call(1, 0, payload, sizes[s], NULL, 0, r) == sizes[s]);
      t[i] = now_us() - a;
    }
    qsort(t, iters, sizeof(double), cmp);
    printf("ROUNDTRIP bytes=%zu iters=%d p50_us=%.1f p99_us=%.1f\n", sizes[s], iters,
           t[iters / 2], t[(int)(iters * 0.99)]);
  }

  int f = open("/data/local/tmp/binder_raw.txt", O_RDWR | O_CREAT | O_TRUNC, 0644);
  CHECK(f >= 0);
  CHECK(write(f, "through binder", 14) == 14);
  struct binder_fd_object fo;
  memset(&fo, 0, sizeof(fo));
  fo.hdr.type = BINDER_TYPE_FD;
  fo.fd = f;
  binder_size_t off = 0;
  size_t n = call(2, TF_ACCEPT_FDS, &fo, sizeof(fo), &off, 1, r);
  CHECK(n == 14 && memcmp(r, "through binder", 14) == 0);
  printf("FD_PASSING ok\n");

  struct out o = {.n = 0};
  put32(&o, BC_ACQUIRE);
  put32(&o, 0);
  put32(&o, BC_REQUEST_DEATH_NOTIFICATION);
  put32(&o, 0);
  binder_uintptr_t cookie = 0xfeed;
  put(&o, &cookie, sizeof(cookie));
  put32(&o, BC_ENTER_LOOPER);
  put_tx(&o, BC_TRANSACTION, 0, 99, TF_ONE_WAY, "", 0, NULL, 0);
  double a = now_us();
  int first = 1;
  for (;;) {
    size_t rn = wr(o.b, first ? o.n : 0, 1);
    first = 0;
    for (size_t pos = 0; pos < rn;) {
      uint32_t cmd;
      memcpy(&cmd, rbuf + pos, 4);
      if (cmd == BR_DEAD_BINDER) {
        binder_uintptr_t c;
        memcpy(&c, rbuf + pos + 4, sizeof(c));
        CHECK(c == 0xfeed);
        printf("DEATH ok after %.2f ms\n", (now_us() - a) / 1000);
        return;
      }
      pos += 4 + _IOC_SIZE(cmd);
    }
  }
}

int main(int argc, char** argv) {
  setvbuf(stdout, NULL, _IONBF, 0);
  if (argc >= 2 && strcmp(argv[1], "manager") == 0) {
    manager();
  } else if (argc >= 2 && strcmp(argv[1], "client") == 0) {
    client(argc >= 3 ? atoi(argv[2]) : 5000);
  } else {
    fprintf(stderr, "usage: binder_raw manager|client [iters]\n");
    return 2;
  }
  return 0;
}
