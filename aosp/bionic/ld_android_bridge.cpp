/*
 * Copyright (C) 2026 The Andruna Authors
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

// The bridge: bionic's loader-side state for andruna-linker, installed as
// ld-android.so.
//
// On Android the dynamic linker statically contains a private copy of libc
// that initializes the main thread and owns __libc_shared_globals (the
// static TLS layout and the TLS module table that libc.so uses for every
// later thread, __tls_get_addr and dl_iterate_phdr). andruna-linker is
// Rust, so these parts live here, built from bionic's own sources against
// the same private headers as libc.so (linked with libc_nomalloc's
// archives, all hidden). andruna-linker calls the functions below at the
// points where bionic's linker does the same work (linker_main.cpp,
// linker_tls.cpp); see crates/andruna-linker/src/bridge.rs.

#include <stddef.h>
#include <stdint.h>

#include "private/KernelArgumentBlock.h"
#include "private/ScopedRWLock.h"
#include "private/bionic_elf_tls.h"
#include "private/bionic_globals.h"
#include "private/bionic_tls.h"

extern "C" void __libc_init_main_thread_early(const KernelArgumentBlock& args,
                                              bionic_tcb* temp_tcb);
extern "C" void __libc_init_main_thread_late();
extern "C" void __libc_init_main_thread_final();

#if defined(__aarch64__)
// bionic/linker/arch/arm64/tlsdesc_resolver.S
extern "C" size_t tlsdesc_resolver_dynamic(size_t);
#endif

// The segment description andruna-linker passes (AndrunaTlsSegment).
struct AndrunaTlsSegment {
  const void* init_ptr;
  size_t init_size;
  size_t size;
  size_t align;
};

namespace {

// The module table. bionic's linker keeps a std::vector; the bridge has no
// allocator, so it uses a fixed table (module ids are never reused while a
// module is loaded, and freed slots are reused as bionic does).
constexpr size_t kMaxTlsModules = 4096;
TlsModule g_tls_modules[kMaxTlsModules];
size_t g_tls_module_count = 0;
bool g_static_tls_finished = false;

// The main thread's temporary TCB until __libc_init_main_thread_final.
bionic_tcb g_temp_tcb;

TlsSegment to_segment(const AndrunaTlsSegment* s) {
  TlsSegment seg;
  seg.aligned_size.size = s->size;
  seg.aligned_size.align.value = s->align ? s->align : 1;
  seg.aligned_size.align.skew = reinterpret_cast<uintptr_t>(s->init_ptr) % seg.aligned_size.align.value;
  seg.init_ptr = s->init_ptr ? s->init_ptr : "";
  seg.init_size = s->init_size;
  return seg;
}

size_t unused_module_index() {
  for (size_t i = 0; i < g_tls_module_count; ++i) {
    if (g_tls_modules[i].soinfo_ptr == nullptr) return i;
  }
  if (g_tls_module_count == kMaxTlsModules) return SIZE_MAX;
  TlsModules& m = __libc_shared_globals()->tls_modules;
  ++g_tls_module_count;
  m.module_count = g_tls_module_count;
  m.module_table = g_tls_modules;
  return g_tls_module_count - 1;
}

// bionic's register_tls_module. Returns the module id or 0.
size_t register_module(const TlsSegment& seg, size_t static_offset, void* handle) {
  TlsModules& m = __libc_shared_globals()->tls_modules;
  ScopedWriteLock locker(&m.rwlock);
  size_t idx = unused_module_index();
  if (idx == SIZE_MAX) return 0;
  const size_t generation = ++m.generation;
  if (m.generation_libc_so != nullptr) *m.generation_libc_so = generation;
  g_tls_modules[idx].segment = seg;
  g_tls_modules[idx].static_offset = static_offset;
  g_tls_modules[idx].first_generation = generation;
  g_tls_modules[idx].soinfo_ptr = handle;
  return __tls_module_idx_to_id(idx);
}

intptr_t tp_relative(size_t static_offset) {
  const StaticTlsLayout& layout = __libc_shared_globals()->static_tls_layout;
  return static_cast<intptr_t>(static_offset) - static_cast<intptr_t>(layout.offset_thread_pointer());
}

}  // namespace

extern "C" {

__attribute__((visibility("default"))) libc_shared_globals* __loader_shared_globals() {
  return __libc_shared_globals();
}

// Main thread set-up before anything else runs (bionic's linker calls the
// same two functions at its start). raw_args is the initial stack block.
__attribute__((visibility("default"))) void __andruna_bridge_init(void* raw_args) {
  KernelArgumentBlock args(raw_args);
  __libc_init_main_thread_early(args, &g_temp_tcb);
  __libc_init_main_thread_late();
  libc_shared_globals* g = __libc_shared_globals();
  g->init_progname = args.argv[0];
  g->init_environ = args.envp;
}

// linker_setup_exe_static_tls: the executable's segment (or none), the TCB
// and bionic_tls. Returns the module id (0 without a segment) and the
// segment's offset from the thread pointer.
__attribute__((visibility("default"))) size_t __andruna_bridge_tls_exe(
    const AndrunaTlsSegment* s, const char* progname, intptr_t* tp_offset) {
  StaticTlsLayout& layout = __libc_shared_globals()->static_tls_layout;
  size_t id = 0;
  if (s == nullptr) {
    layout.reserve_exe_segment_and_tcb(nullptr, progname);
  } else {
    TlsSegment seg = to_segment(s);
    size_t off = layout.reserve_exe_segment_and_tcb(&seg, progname);
    id = register_module(seg, off, const_cast<char*>(progname));
    if (tp_offset) *tp_offset = tp_relative(off);
  }
  layout.reserve_bionic_tls();
  return id;
}

// register_soinfo_tls: static while the start-up set loads, dynamic after
// __andruna_bridge_tls_finalize.
__attribute__((visibility("default"))) size_t __andruna_bridge_tls_register(
    const AndrunaTlsSegment* s, uintptr_t handle, intptr_t* tp_offset, int* is_static) {
  TlsSegment seg = to_segment(s);
  size_t off = SIZE_MAX;
  if (!g_static_tls_finished) {
    off = __libc_shared_globals()->static_tls_layout.reserve_solib_segment(seg);
  }
  size_t id = register_module(seg, off, reinterpret_cast<void*>(handle));
  if (is_static) *is_static = off != SIZE_MAX;
  if (tp_offset && off != SIZE_MAX) *tp_offset = tp_relative(off);
  return id;
}

__attribute__((visibility("default"))) void __andruna_bridge_tls_unregister(size_t module_id) {
  TlsModules& m = __libc_shared_globals()->tls_modules;
  ScopedWriteLock locker(&m.rwlock);
  size_t idx = __tls_module_id_to_idx(module_id);
  if (idx < g_tls_module_count && g_tls_modules[idx].static_offset == SIZE_MAX) {
    g_tls_modules[idx] = {};
  }
}

__attribute__((visibility("default"))) size_t __andruna_bridge_tls_generation(size_t module_id) {
  size_t idx = __tls_module_id_to_idx(module_id);
  return idx < g_tls_module_count ? g_tls_modules[idx].first_generation : 0;
}

// linker_finalize_static_tls.
__attribute__((visibility("default"))) void __andruna_bridge_tls_finalize() {
  g_static_tls_finished = true;
  __libc_shared_globals()->static_tls_layout.finish_layout();
  TlsModules& m = __libc_shared_globals()->tls_modules;
  m.static_module_count = m.module_count;
}

// After every start-up object is relocated: the main thread's final static
// TLS with the TLS images copied.
__attribute__((visibility("default"))) void __andruna_bridge_finish_main_thread() {
  __libc_init_main_thread_final();
}

// The TLSDESC function for dynamic TLS modules (arm64; x86_64 Android code
// uses __tls_get_addr instead).
__attribute__((visibility("default"))) uintptr_t __andruna_bridge_tlsdesc_dynamic() {
#if defined(__aarch64__)
  return reinterpret_cast<uintptr_t>(&tlsdesc_resolver_dynamic);
#else
  return 0;
#endif
}

}  // extern "C"
