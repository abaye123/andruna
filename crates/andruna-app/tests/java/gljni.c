// JNI library of GlMain.java: GlMain.render() runs the rendering loop of
// andruna-gles's tests/guest/gles_window.c (the compositor's window, GLES 3
// through ANGLE, resizes, until the window closes) on the Java main
// thread and returns its result. Built against the guest bionic sysroot
// by crates/andruna-app/tools/build_gl_java.py.

#include <jni.h>

#define main gles_window_main
#include "gles_window.c"
#undef main

JNIEXPORT jint JNICALL Java_GlMain_render(JNIEnv *env, jclass cls) {
  (void)env;
  (void)cls;
  return gles_window_main(0, 0);
}
