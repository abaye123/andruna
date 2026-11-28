// andruna_app_process: starts ART with the framework, as app_process does
// (frameworks/base/cmds/app_process), and runs one class's main().
//
//   andruna_app_process [-X<vm option>...] [-D<property>=<value>...] <class> [args...]
//
// It uses the framework's own start-up, android::AndroidRuntime::start():
// the VM is created with the runtime's options (properties, BOOTCLASSPATH,
// CLASSPATH), the framework's JNI natives are registered (startReg), then
// <class>.main(args) runs. app_process would first run
// com.android.internal.os.RuntimeInit, whose commonInit() needs modules
// that are not built yet (TrafficStats from framework-connectivity-t,
// MimeMap) and which then starts the Binder thread pool; that path comes
// with Binder and system_server (docs/plan/phase2/framework-build.md).

#include <android_runtime/AndroidRuntime.h>
#include <utils/String8.h>
#include <utils/Vector.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

namespace {

class AndrunaRuntime : public android::AndroidRuntime {
public:
    AndrunaRuntime(char* argBlockStart, size_t argBlockLength)
        : AndroidRuntime(argBlockStart, argBlockLength) {}

    // Called only on the RuntimeInit path (nativeFinishInit), not used here.
    void onStarted() override {}

    void onExit(int code) override {
        fflush(stdout);
        fflush(stderr);
        AndroidRuntime::onExit(code);
    }
};

}  // namespace

int main(int argc, char* argv[]) {
    // The argument block (for setArgv0) spans argv's strings.
    char* block = argv[0];
    size_t blockLength = 0;
    for (int i = 0; i < argc; i++) {
        blockLength += strlen(argv[i]) + 1;
    }
    AndrunaRuntime runtime(block, blockLength);

    int i = 1;
    for (; i < argc && argv[i][0] == '-'; i++) {
        runtime.addOption(strdup(argv[i]));
    }
    if (i >= argc) {
        fprintf(stderr, "usage: andruna_app_process [vm options] <class> [args...]\n");
        return 2;
    }
    const char* className = argv[i++];
    android::Vector<android::String8> args;
    for (; i < argc; i++) {
        args.add(android::String8(argv[i]));
    }
    runtime.setArgv0(className, false);
    // Returns only when the VM could not start or main() threw; the
    // process otherwise ends with System.exit() or when main() returns
    // (AndroidRuntime::start destroys the VM and returns).
    runtime.start(className, args, false);
    return 1;
}
