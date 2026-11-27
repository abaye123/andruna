// libunwindstack.so for the guest while Andruna does not build AOSP's
// libunwindstack (it needs libzstd and a Rust demangler). It defines the
// part of libunwindstack's interface that libart and libsigchain link
// against, and every unwind reports ERROR_UNSUPPORTED, so ART's native
// stack dumps say that unwinding failed instead of printing frames. Only
// diagnostics use it (stack dumps on SIGQUIT and crashes, gcstress).

#include <memory>
#include <optional>
#include <string>
#include <vector>

#include <unwindstack/AndroidUnwinder.h>
#include <unwindstack/DexFiles.h>
#include <unwindstack/Elf.h>
#include <unwindstack/Error.h>
#include <unwindstack/JitDebug.h>
#include <unwindstack/MapInfo.h>
#include <unwindstack/Maps.h>
#include <unwindstack/Memory.h>
#include <unwindstack/Regs.h>
#if defined(__aarch64__)
#include <unwindstack/RegsArm64.h>
#endif
#include <unwindstack/Unwinder.h>

namespace unwindstack {

std::string AndroidUnwinderData::GetErrorString() {
  return "unwinding is not available (libunwindstack is not built)";
}

void AndroidUnwinderData::DemangleFunctionNames() {}

bool AndroidUnwinder::Unwind(std::optional<pid_t>, AndroidUnwinderData& data) {
  data.frames.clear();
  data.error.code = ERROR_UNSUPPORTED;
  return false;
}

bool AndroidUnwinder::Unwind(AndroidUnwinderData& data) {
  data.frames.clear();
  data.error.code = ERROR_UNSUPPORTED;
  return false;
}

bool AndroidUnwinder::Unwind(void*, AndroidUnwinderData& data) {
  data.frames.clear();
  data.error.code = ERROR_UNSUPPORTED;
  return false;
}

bool AndroidLocalUnwinder::InternalInitialize(ErrorData& error) {
  error.code = ERROR_UNSUPPORTED;
  return false;
}

bool AndroidLocalUnwinder::InternalUnwind(std::optional<pid_t>, AndroidUnwinderData& data) {
  data.error.code = ERROR_UNSUPPORTED;
  return false;
}

std::shared_ptr<MapInfo> Maps::Find(uint64_t) {
  return nullptr;
}

bool Maps::Parse() {
  return false;
}

LocalUpdatableMaps::LocalUpdatableMaps() : Maps() {}

std::shared_ptr<MapInfo> LocalUpdatableMaps::Find(uint64_t) {
  return nullptr;
}

bool LocalUpdatableMaps::Parse() {
  return false;
}

const std::string LocalUpdatableMaps::GetMapsFile() const {
  return "/proc/self/maps";
}

void LocalUpdatableMaps::ForEachMapInfo(std::function<bool(MapInfo*)> const&) {}

bool LocalUpdatableMaps::Reparse(bool* any_changed) {
  if (any_changed != nullptr) *any_changed = false;
  return false;
}

void Unwinder::Unwind(const std::vector<std::string>*, const std::vector<std::string>*) {
  frames_.clear();
  last_error_.code = ERROR_UNSUPPORTED;
}

void Unwinder::SetJitDebug(JitDebug*) {}

void Unwinder::SetDexFiles(DexFiles*) {}

MapInfo::ElfFields& MapInfo::GetElfFields() {
  // Only reached for frames of a successful unwind, which never happens.
  static thread_local ElfFields fields;
  return fields;
}

std::string MapInfo::GetPrintableBuildID() {
  return "";
}

void Elf::SetCachingEnabled(bool) {}

ArchEnum Regs::CurrentArch() {
#if defined(__x86_64__)
  return ARCH_X86_64;
#else
  return ARCH_ARM64;
#endif
}

Regs* Regs::CreateFromLocal() {
  return nullptr;
}

#if defined(__aarch64__)
// RegsGetLocal (inline, used by ART's backtrace helper) stores the SVE
// vector length; without SVE state to unwind, 0.
uint64_t RegsArm64::GetVgFromLocal() {
  return 0;
}
#endif

std::shared_ptr<Memory> Memory::CreateProcessMemoryThreadCached(pid_t) {
  return nullptr;
}

std::unique_ptr<JitDebug> CreateJitDebug(ArchEnum, std::shared_ptr<Memory>&,
                                         std::vector<std::string>) {
  return nullptr;
}

std::unique_ptr<DexFiles> CreateDexFiles(ArchEnum, std::shared_ptr<Memory>&,
                                         std::vector<std::string>) {
  return nullptr;
}

}  // namespace unwindstack

#if defined(__x86_64__)
extern "C" void AsmGetRegs(void*) {}
#endif
