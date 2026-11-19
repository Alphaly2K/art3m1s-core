#ifndef ARTEMIS_DXBC_EFFECT_H
#define ARTEMIS_DXBC_EFFECT_H

#include <stdint.h>

/* ADE1 and ADE2 use little-endian words and fixed-size, NUL-terminated names. */
#define ART_DXBC_EFFECT_MAGIC UINT32_C(0x31454441)
#define ART_DXBC_EFFECT_PAIR_MAGIC UINT32_C(0x32454441)
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
    /* ADE2 arrays store the column count in the low byte and element count above it. */
    uint32_t offset, components;
    char name[ART_DXBC_NAME_BYTES];
} ArtDxbcUniform;

static inline uint32_t art_dxbc_uniform_columns(uint32_t layout) { return layout & 255; }
static inline uint32_t art_dxbc_uniform_elements(uint32_t layout) { return layout >> 8; }
static inline uint32_t art_dxbc_uniform_span(uint32_t layout) {
    uint32_t elements = art_dxbc_uniform_elements(layout);
    return (elements ? (elements - 1) * 16 : 0) + art_dxbc_uniform_columns(layout) * 4;
}

#endif
