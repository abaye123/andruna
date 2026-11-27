// Declarations of the heapprofd client API (libheapprofd_client_api on
// Android) as libart uses it. Andruna has no heapprofd yet: the
// implementation in ../../src/heapprofd_client_none.c registers heaps but
// never enables them, which is what the real client does while no profiling
// session is active.

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct AHeapInfo AHeapInfo;
typedef struct AHeapProfileEnableCallbackInfo AHeapProfileEnableCallbackInfo;
typedef struct AHeapProfileDisableCallbackInfo AHeapProfileDisableCallbackInfo;

AHeapInfo* AHeapInfo_create(const char* heap_name);
AHeapInfo* AHeapInfo_setEnabledCallback(
    AHeapInfo* info,
    void (*callback)(void* data, const AHeapProfileEnableCallbackInfo* session_info),
    void* data);
AHeapInfo* AHeapInfo_setDisabledCallback(
    AHeapInfo* info,
    void (*callback)(void* data, const AHeapProfileDisableCallbackInfo* session_info),
    void* data);
uint32_t AHeapProfile_registerHeap(AHeapInfo* info);
uint64_t AHeapProfileEnableCallbackInfo_getSamplingInterval(
    const AHeapProfileEnableCallbackInfo* session_info);
bool AHeapProfile_reportAllocation(uint32_t heap_id, uint64_t alloc_id, uint64_t size);
bool AHeapProfile_reportSample(uint32_t heap_id, uint64_t alloc_id, uint64_t size);
void AHeapProfile_reportFree(uint32_t heap_id, uint64_t alloc_id);

#ifdef __cplusplus
}
#endif
