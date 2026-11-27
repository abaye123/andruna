// heapprofd client API without a heapprofd service: heaps are registered
// and get ids, no session ever enables them, so reports are dropped and
// return false (the documented result for a disabled heap).

#include <perfetto/heap_profile.h>

#include <stdatomic.h>
#include <stdlib.h>

struct AHeapInfo {
  const char* name;
};

static atomic_uint g_next_heap_id = 1;

AHeapInfo* AHeapInfo_create(const char* heap_name) {
  AHeapInfo* info = calloc(1, sizeof(AHeapInfo));
  if (info != NULL) info->name = heap_name;
  return info;
}

AHeapInfo* AHeapInfo_setEnabledCallback(
    AHeapInfo* info, void (*callback)(void*, const AHeapProfileEnableCallbackInfo*), void* data) {
  (void)callback;
  (void)data;
  return info;
}

AHeapInfo* AHeapInfo_setDisabledCallback(
    AHeapInfo* info, void (*callback)(void*, const AHeapProfileDisableCallbackInfo*), void* data) {
  (void)callback;
  (void)data;
  return info;
}

uint32_t AHeapProfile_registerHeap(AHeapInfo* info) {
  free(info);
  return atomic_fetch_add(&g_next_heap_id, 1) + 1;
}

uint64_t AHeapProfileEnableCallbackInfo_getSamplingInterval(
    const AHeapProfileEnableCallbackInfo* session_info) {
  (void)session_info;
  return 0;
}

bool AHeapProfile_reportAllocation(uint32_t heap_id, uint64_t alloc_id, uint64_t size) {
  (void)heap_id;
  (void)alloc_id;
  (void)size;
  return false;
}

bool AHeapProfile_reportSample(uint32_t heap_id, uint64_t alloc_id, uint64_t size) {
  (void)heap_id;
  (void)alloc_id;
  (void)size;
  return false;
}

void AHeapProfile_reportFree(uint32_t heap_id, uint64_t alloc_id) {
  (void)heap_id;
  (void)alloc_id;
}
