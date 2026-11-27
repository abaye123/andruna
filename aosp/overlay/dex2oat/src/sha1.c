// SHA1_Init/Update/Final (see ../include/openssl/sha.h) on the LZMA SDK's
// SHA-1. A separate translation unit: the SDK's include directory has a
// Compiler.h that would shadow ART's compiler.h on a case-insensitive file
// system.

#include <openssl/sha.h>

#include "Sha1.h"

_Static_assert(sizeof(CSha1) <= sizeof(((SHA_CTX*)0)->opaque), "SHA_CTX too small");
_Static_assert(_Alignof(CSha1) <= 16, "SHA_CTX alignment");

int SHA1_Init(SHA_CTX* ctx) {
  Sha1_Init((CSha1*)ctx->opaque);
  return 1;
}

int SHA1_Update(SHA_CTX* ctx, const void* data, size_t len) {
  Sha1_Update((CSha1*)ctx->opaque, (const Byte*)data, len);
  return 1;
}

int SHA1_Final(uint8_t out[SHA_DIGEST_LENGTH], SHA_CTX* ctx) {
  Sha1_Final((CSha1*)ctx->opaque, out);
  return 1;
}
