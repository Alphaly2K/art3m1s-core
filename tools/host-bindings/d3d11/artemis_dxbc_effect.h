#ifndef ARTEMIS_DXBC_EFFECT_H
#define ARTEMIS_DXBC_EFFECT_H

#include <stdint.h>

/* ADE1 uses little-endian words and fixed-size, NUL-terminated names. */
#define ART_DXBC_EFFECT_MAGIC UINT32_C(0x31454441)
#define ART_DXBC_MAX_BYTECODE (16u * 1024 * 1024)
#define ART_DXBC_MAX_CONSTANTS (64u * 1024)
#define ART_DXBC_MAX_UNIFORMS 256
#define ART_DXBC_TEXTURE_SLOTS 4
#define ART_DXBC_NAME_BYTES 256

typedef struct ArtDxbcHeader {
    uint32_t magic, bytecode_bytes, constant_bytes, textures, uniforms;
} ArtDxbcHeader;

typedef struct ArtDxbcTexture {
    uint32_t slot;
    char name[ART_DXBC_NAME_BYTES];
} ArtDxbcTexture;

typedef struct ArtDxbcUniform {
    uint32_t offset, components;
    char name[ART_DXBC_NAME_BYTES];
} ArtDxbcUniform;

#endif
