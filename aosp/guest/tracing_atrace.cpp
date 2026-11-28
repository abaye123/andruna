// tracing_perfetto's API (frameworks/native/libs/tracing_perfetto) on
// cutils' atrace, for libandroid_runtime's android.os.Trace natives.
//
// AOSP's libtracing_perfetto sends events to perfetto when a perfetto
// session enables a category and to atrace otherwise; it links the perfetto
// C SDK (libperfetto_c), which Andruna does not build yet. This file is the
// atrace half only: with no tracing service, atrace reports every tag as
// disabled (debug.atrace.tags.enableflags is unset) and nothing is written.

#include <cutils/trace.h>
#include <tracing_perfetto.h>

namespace tracing_perfetto {

void registerWithPerfetto(bool) {}

bool isTagEnabled(uint64_t category) {
  return atrace_is_tag_enabled(category);
}

void traceBegin(uint64_t category, const char* name) {
  atrace_begin(category, name);
}

void traceEnd(uint64_t category) {
  atrace_end(category);
}

void traceAsyncBegin(uint64_t category, const char* name, int32_t cookie) {
  atrace_async_begin(category, name, cookie);
}

void traceAsyncEnd(uint64_t category, const char* name, int32_t cookie) {
  atrace_async_end(category, name, cookie);
}

void traceAsyncBeginForTrack(uint64_t category, const char* name, const char* trackName,
                             int32_t cookie) {
  atrace_async_for_track_begin(category, trackName, name, cookie);
}

void traceAsyncEndForTrack(uint64_t category, const char* trackName, int32_t cookie) {
  atrace_async_for_track_end(category, trackName, cookie);
}

void traceInstant(uint64_t category, const char* name) {
  atrace_instant(category, name);
}

void traceInstantForTrack(uint64_t category, const char* trackName, const char* name) {
  atrace_instant_for_track(category, trackName, name);
}

void traceCounter(uint64_t category, const char* name, int64_t value) {
  atrace_int64(category, name, value);
}

void traceCounter32(uint64_t category, const char* name, int32_t value) {
  atrace_int(category, name, value);
}

}  // namespace tracing_perfetto
