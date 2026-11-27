// The two namespace queries that libart calls on Android, for the host
// variant of libnativeloader that Andruna builds (art_elf.cmake): without
// linker namespaces no class loader has a namespace, so none is natively
// bridged.

#include <nativeloader/native_loader.h>

namespace android {
extern "C" {

NativeLoaderNamespace* FindNativeLoaderNamespaceByClassLoader(JNIEnv*, jobject) {
  return nullptr;
}

bool IsNamespaceNativeBridged(const NativeLoaderNamespace*) {
  return false;
}

}  // extern "C"
}  // namespace android
