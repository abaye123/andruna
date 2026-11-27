// The SHA-1 subset of BoringSSL's <openssl/sha.h> that dex2oat uses (the
// build ID of oat files, art/dex2oat/linker/elf_writer_quick.cc), on top of
// the public-domain SHA-1 of the LZMA SDK (external/lzma/C/Sha1.c, wrapped
// in ../src/sha1.c), which the ART build already fetches. Andruna does not
// build BoringSSL.

#ifndef ANDRUNA_OVERLAY_OPENSSL_SHA_H_
#define ANDRUNA_OVERLAY_OPENSSL_SHA_H_

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define SHA_DIGEST_LENGTH 20

// Holds the LZMA SDK's CSha1 (checked in sha1.c).
typedef struct {
  unsigned char opaque[256] __attribute__((aligned(16)));
} SHA_CTX;

int SHA1_Init(SHA_CTX* ctx);
int SHA1_Update(SHA_CTX* ctx, const void* data, size_t len);
int SHA1_Final(uint8_t out[SHA_DIGEST_LENGTH], SHA_CTX* ctx);

#ifdef __cplusplus
}
#endif

#endif  // ANDRUNA_OVERLAY_OPENSSL_SHA_H_
