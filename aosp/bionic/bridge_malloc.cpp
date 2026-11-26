// The allocator of the bridge ld-android.so.
//
// The bridge links the static parts of libc the way bionic's linker does
// (libc_nomalloc), so it needs its own malloc family; like the linker
// (bionic/linker/linker_memory.cpp) it uses bionic's BionicAllocator. The
// linker's crash-dump fallback allocator is not needed here. These symbols
// stay private to the bridge (its version script exports only the bridge
// interface), so libc.so's scudo is the allocator of everything else.

#include <errno.h>
#include <stdlib.h>

#include "private/bionic_allocator.h"

static BionicAllocator g_bridge_allocator;

extern "C" {

void* malloc(size_t byte_count) {
  return g_bridge_allocator.alloc(byte_count);
}

void* memalign(size_t alignment, size_t byte_count) {
  return g_bridge_allocator.memalign(alignment, byte_count);
}

void* aligned_alloc(size_t alignment, size_t byte_count) {
  return g_bridge_allocator.memalign(alignment, byte_count);
}

void* calloc(size_t item_count, size_t item_size) {
  size_t byte_count;
  if (__builtin_mul_overflow(item_count, item_size, &byte_count)) {
    errno = ENOMEM;
    return nullptr;
  }
  // BionicAllocator returns zeroed memory (fresh mappings or cleared
  // blocks), as the linker's calloc relies on.
  return g_bridge_allocator.alloc(byte_count);
}

void* realloc(void* p, size_t byte_count) {
  return g_bridge_allocator.realloc(p, byte_count);
}

void* reallocarray(void* p, size_t item_count, size_t item_size) {
  size_t byte_count;
  if (__builtin_mul_overflow(item_count, item_size, &byte_count)) {
    errno = ENOMEM;
    return nullptr;
  }
  return g_bridge_allocator.realloc(p, byte_count);
}

void free(void* ptr) {
  g_bridge_allocator.free(ptr);
}

}  // extern "C"
