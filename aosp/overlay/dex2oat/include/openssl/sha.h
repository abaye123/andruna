// The SHA-1 subset of BoringSSL's <openssl/sha.h> that dex2oat uses (the
// build ID of oat files, art/dex2oat/linker/elf_writer_quick.cc), on top of
// the public-domain SHA-1 of the LZMA SDK (external/lzma/C/Sha1.c), which
// the ART build already fetches. Andruna does not build BoringSSL.

#ifndef ANDRUNA_OVERLAY_OPENSSL_SHA_H_
#define ANDRUNA_OVERLAY_OPENSSL_SHA_H_

#include <stddef.h>
#include <stdint.h>

#include "Sha1.h"

#define SHA_DIGEST_LENGTH 20

typedef CSha1 SHA_CTX;

static inline int SHA1_Init(SHA_CTX* ctx) {
  Sha1_Init(ctx);
  return 1;
}

static inline int SHA1_Update(SHA_CTX* ctx, const void* data, size_t len) {
  Sha1_Update(ctx, (const Byte*)data, len);
  return 1;
}

static inline int SHA1_Final(uint8_t out[SHA_DIGEST_LENGTH], SHA_CTX* ctx) {
  Sha1_Final(ctx, out);
  return 1;
}

#endif  // ANDRUNA_OVERLAY_OPENSSL_SHA_H_
