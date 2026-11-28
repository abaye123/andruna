/*
 * libbinder end to end through servicemanager (docs/plan/phase2/binder.md).
 *
 *   binder_sm_test service          registers "andruna.test.echo" with
 *                                   servicemanager and serves it
 *   binder_sm_test client <iters> [app]
 *                                   finds it and calls it: identity, round
 *                                   trips (latency), file descriptors in
 *                                   both directions, death notification
 *                                   when the service exits; with `app`
 *                                   (the process runs as an app uid),
 *                                   servicemanager's access policy
 *
 * Every result is a line on stdout that the Rust test checks
 * (crates/andruna-sysd/tests/servicemanager.rs). Exit status 0 is success.
 */

#include <binder/Binder.h>
#include <binder/IBinder.h>
#include <binder/IPCThreadState.h>
#include <binder/IServiceManager.h>
#include <binder/Parcel.h>
#include <binder/ProcessState.h>
#include <utils/String16.h>
#include <utils/String8.h>

#include <algorithm>
#include <condition_variable>
#include <fcntl.h>
#include <mutex>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <vector>

using namespace android;

namespace {

const String16 kName("andruna.test.echo");
const String16 kDescriptor("andruna.test.IEcho");

enum : uint32_t {
    ECHO = IBinder::FIRST_CALL_TRANSACTION,
    IDENTITY,
    READ_FD,   // client sends a descriptor, service returns its content
    OPEN_FD,   // service returns a descriptor of a file it wrote
    EXIT,      // one-way: the service process exits
};

#define CHECK_OR_DIE(cond, ...)                                    \
    do {                                                           \
        if (!(cond)) {                                             \
            fprintf(stdout, "FAIL %s:%d: ", __FILE__, __LINE__);   \
            fprintf(stdout, __VA_ARGS__);                          \
            fprintf(stdout, "\n");                                 \
            fflush(stdout);                                        \
            exit(1);                                               \
        }                                                          \
    } while (0)

uint64_t nowNs() {
    timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return uint64_t(ts.tv_sec) * 1000000000ull + uint64_t(ts.tv_nsec);
}

std::string readAll(int fd) {
    std::string s;
    char buf[256];
    lseek(fd, 0, SEEK_SET);
    ssize_t n;
    while ((n = read(fd, buf, sizeof(buf))) > 0) s.append(buf, size_t(n));
    return s;
}

class EchoService : public BBinder {
public:
    const String16& getInterfaceDescriptor() const override { return kDescriptor; }

    status_t onTransact(uint32_t code, const Parcel& data, Parcel* reply,
                        uint32_t flags) override {
        switch (code) {
            case ECHO: {
                if (!data.checkInterface(this)) return PERMISSION_DENIED;
                std::vector<uint8_t> bytes;
                status_t st = data.readByteVector(&bytes);
                if (st != OK) return st;
                return reply->writeByteVector(bytes);
            }
            case IDENTITY: {
                if (!data.checkInterface(this)) return PERMISSION_DENIED;
                IPCThreadState* ipc = IPCThreadState::self();
                reply->writeInt32(ipc->getCallingPid());
                reply->writeInt32(int32_t(ipc->getCallingUid()));
                return OK;
            }
            case READ_FD: {
                if (!data.checkInterface(this)) return PERMISSION_DENIED;
                int fd = data.readFileDescriptor();
                if (fd < 0) return BAD_VALUE;
                std::string s = readAll(fd);
                return reply->writeUtf8AsUtf16(s);
            }
            case OPEN_FD: {
                if (!data.checkInterface(this)) return PERMISSION_DENIED;
                const char* path = "/data/local/tmp/binder_sm_service.txt";
                int fd = open(path, O_RDWR | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
                if (fd < 0) return -errno;
                const char text[] = "written by the service";
                if (write(fd, text, sizeof(text) - 1) != ssize_t(sizeof(text) - 1)) return -EIO;
                // takeOwnership: the parcel closes it after the reply is sent.
                return reply->writeFileDescriptor(fd, true);
            }
            case EXIT:
                fprintf(stdout, "SERVICE_EXIT\n");
                fflush(stdout);
                _exit(0);
            default:
                return BBinder::onTransact(code, data, reply, flags);
        }
    }
};

int runService() {
    sp<ProcessState> ps = ProcessState::self();
    ps->startThreadPool();
    sp<IServiceManager> sm = defaultServiceManager();
    CHECK_OR_DIE(sm != nullptr, "no service manager");
    sp<EchoService> service = sp<EchoService>::make();
    status_t st = sm->addService(kName, service);
    CHECK_OR_DIE(st == OK, "addService: %d", st);
    fprintf(stdout, "SERVICE_READY pid=%d uid=%d\n", getpid(), getuid());
    fflush(stdout);
    IPCThreadState::self()->joinThreadPool();
    return 0;
}

class DeathWaiter : public IBinder::DeathRecipient {
public:
    void binderDied(const wp<IBinder>&) override {
        std::lock_guard<std::mutex> l(mLock);
        mDied = true;
        mCond.notify_all();
    }
    bool wait(int seconds) {
        std::unique_lock<std::mutex> l(mLock);
        return mCond.wait_for(l, std::chrono::seconds(seconds), [&] { return mDied; });
    }

private:
    std::mutex mLock;
    std::condition_variable mCond;
    bool mDied = false;
};

void latency(const sp<IBinder>& b, size_t bytes, int iters) {
    std::vector<uint8_t> payload(bytes, 0x5a);
    std::vector<uint64_t> t;
    t.reserve(size_t(iters));
    for (int i = 0; i < iters + 500; i++) {
        Parcel data, reply;
        data.writeInterfaceToken(kDescriptor);
        data.writeByteVector(payload);
        uint64_t start = nowNs();
        status_t st = b->transact(ECHO, data, &reply);
        uint64_t end = nowNs();
        CHECK_OR_DIE(st == OK, "ECHO: %d", st);
        if (i >= 500) t.push_back(end - start);
        if (i == 0) {
            std::vector<uint8_t> back;
            CHECK_OR_DIE(reply.readByteVector(&back) == OK && back == payload, "ECHO reply");
        }
    }
    std::sort(t.begin(), t.end());
    auto pct = [&](double p) { return double(t[size_t(p * double(t.size() - 1))]) / 1000.0; };
    fprintf(stdout, "ROUNDTRIP bytes=%zu iters=%d p50_us=%.1f p90_us=%.1f p99_us=%.1f max_us=%.1f\n",
            bytes, iters, pct(0.5), pct(0.9), pct(0.99), double(t.back()) / 1000.0);
    fflush(stdout);
}

int runClient(int iters, bool asApp) {
    sp<ProcessState> ps = ProcessState::self();
    ps->startThreadPool();
    sp<IServiceManager> sm = defaultServiceManager();
    CHECK_OR_DIE(sm != nullptr, "no service manager");

    // servicemanager registers itself as "manager".
    CHECK_OR_DIE(sm->checkService(String16("manager")) != nullptr, "no 'manager' service");

    sp<IBinder> b = sm->waitForService(kName);
    CHECK_OR_DIE(b != nullptr, "waitForService returned null");
    CHECK_OR_DIE(b->remoteBinder() != nullptr, "service is not a proxy");
    fprintf(stdout, "LOOKUP ok descriptor=%s\n", String8(b->getInterfaceDescriptor()).c_str());

    {
        Parcel data, reply;
        data.writeInterfaceToken(kDescriptor);
        CHECK_OR_DIE(b->transact(IDENTITY, data, &reply) == OK, "IDENTITY");
        fprintf(stdout, "IDENTITY pid=%d uid=%d\n", reply.readInt32(), reply.readInt32());
    }

    // A descriptor to the service and back.
    {
        const char* path = "/data/local/tmp/binder_sm_client.txt";
        int fd = open(path, O_RDWR | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
        CHECK_OR_DIE(fd >= 0, "open %s: %d", path, errno);
        const char text[] = "written by the client";
        CHECK_OR_DIE(write(fd, text, sizeof(text) - 1) == ssize_t(sizeof(text) - 1), "write");
        Parcel data, reply;
        data.writeInterfaceToken(kDescriptor);
        data.writeFileDescriptor(fd);
        status_t st = b->transact(READ_FD, data, &reply);
        CHECK_OR_DIE(st == OK, "READ_FD: %d", st);
        std::string got;
        CHECK_OR_DIE(reply.readUtf8FromUtf16(&got) == OK && got == text, "READ_FD reply '%s'",
                     got.c_str());
        close(fd);

        Parcel data2, reply2;
        data2.writeInterfaceToken(kDescriptor);
        st = b->transact(OPEN_FD, data2, &reply2);
        CHECK_OR_DIE(st == OK, "OPEN_FD: %d", st);
        int rfd = reply2.readFileDescriptor();
        CHECK_OR_DIE(rfd >= 0, "OPEN_FD: no descriptor");
        std::string s = readAll(rfd);
        CHECK_OR_DIE(s == "written by the service", "OPEN_FD content '%s'", s.c_str());
        fprintf(stdout, "FD_PASSING ok\n");
    }

    // servicemanager's policy for an app uid (ADR 0041): find yes, add and
    // list no. The uid is the broker's (sysd's), not getuid() in the guest.
    if (asApp) {
        sp<BBinder> mine = sp<BBinder>::make();
        status_t st = sm->addService(String16("andruna.test.app"), mine);
        CHECK_OR_DIE(st != OK, "an app added a service");
        Vector<String16> list = sm->listServices();
        CHECK_OR_DIE(list.isEmpty(), "an app listed %zu services", list.size());
        fprintf(stdout, "POLICY ok add=%d list=denied\n", st);
    }

    latency(b, 100, iters);
    latency(b, 4096, iters / 4 > 0 ? iters / 4 : 1);

    // Death notification: the service exits on EXIT.
    sp<DeathWaiter> waiter = sp<DeathWaiter>::make();
    CHECK_OR_DIE(b->linkToDeath(waiter) == OK, "linkToDeath");
    {
        Parcel data;
        data.writeInterfaceToken(kDescriptor);
        CHECK_OR_DIE(b->transact(EXIT, data, nullptr, IBinder::FLAG_ONEWAY) == OK, "EXIT");
    }
    uint64_t start = nowNs();
    CHECK_OR_DIE(waiter->wait(20), "no death notification");
    fprintf(stdout, "DEATH ok ms=%.1f\n", double(nowNs() - start) / 1e6);
    CHECK_OR_DIE(!b->isBinderAlive(), "binder alive after death");
    fflush(stdout);
    return 0;
}

}  // namespace

int main(int argc, char** argv) {
    setvbuf(stdout, nullptr, _IOLBF, 0);
    if (argc >= 2 && strcmp(argv[1], "service") == 0) return runService();
    if (argc >= 2 && strcmp(argv[1], "client") == 0) {
        return runClient(argc >= 3 ? atoi(argv[2]) : 2000,
                         argc >= 4 && strcmp(argv[3], "app") == 0);
    }
    fprintf(stderr, "usage: %s service | client [iterations [app]]\n", argv[0]);
    return 2;
}
