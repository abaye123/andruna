# 0024 - Starting the framework in an app process before system_server

**Status:** proposed, 2026-11-28

## Context
On Android an app process starts from zygote: app_process runs
AndroidRuntime::start, which creates the VM, registers the framework's JNI
natives (startReg) and calls com.android.internal.os.RuntimeInit, whose
commonInit() touches TrafficStats (framework-connectivity-t) and MimeMap,
and whose nativeFinishInit starts the Binder thread pool. Andruna builds
only part of the framework's natives so far and none of the module jars
RuntimeInit needs.

## Decision (proposed)
- `andruna-app --framework` runs `andruna_app_process` (aosp/guest), a
  small Andruna program that calls AndroidRuntime::start(<main class>)
  directly: the same VM creation and native registration as app_process,
  without RuntimeInit. main() receives the class name as its first
  argument, as every class started by AndroidRuntime::start does.
- The boot class path is the ART module's jars, then framework.jar,
  framework-location.jar, framework-telecom.jar in /system/framework (the
  ART root's `framework` directory, mounted by andruna-app). The existing
  boot image covers the ART module's jars; the framework's jars run
  without an image until the boot image includes them.
- libandroid_runtime registers the natives that are built (a gated patch
  of AndroidRuntime.cpp, `patches/frameworks/base/0001`); a class whose
  natives are not registered throws UnsatisfiedLinkError when one is
  called. android.os.Trace uses cutils' atrace (`tracing_atrace.cpp`), which
  reports tracing as off; the perfetto SDK is not built.
- This path is replaced by the zygote/RuntimeInit path once Binder,
  system_server and the module jars RuntimeInit needs exist.

## Consequences
- Java programs and tests use Looper, Handler, SystemClock, Log,
  SystemProperties and the system Resources now, with the framework's own
  code and natives.
- Behavior that depends on RuntimeInit (the default uncaught exception
  handler that reports to ActivityManager, the HTTP user agent property,
  socket tagging) is absent until then.
