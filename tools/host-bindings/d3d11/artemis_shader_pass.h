#ifndef ARTEMIS_SHADER_PASS_H
#define ARTEMIS_SHADER_PASS_H

#include <d3d11.h>
#include <d3dcompiler.h>
#include <stddef.h>
#include <stdint.h>

typedef struct ArtD3D11Pass ArtD3D11Pass;
typedef struct ArtD3D11Program ArtD3D11Program;

typedef struct ArtD3D11Vertex {
    float xy[2];
    float uv0[2];
    float uv1[2];
} ArtD3D11Vertex;

typedef struct ArtD3D11Image {
    uint32_t width, height, row_bytes;
    size_t bytes;
    const void *bgra;
} ArtD3D11Image;

typedef struct ArtD3D11Draw {
    uint32_t width, height;
    float clear_rgba[4];
    ArtD3D11Vertex quad[4];
    ArtD3D11Image textures[4];
    const void *constants;
    uint32_t constant_bytes;
    BOOL alpha_blend;
    BOOL point_sampler;
} ArtD3D11Draw;

/* Use the immediate context from a single Host thread. */
HRESULT art_d3d11_pass_create(ArtD3D11Pass **pass);
void art_d3d11_pass_destroy(ArtD3D11Pass *pass);

/* Compile unmodified HLSL. The caller owns the program and diagnostics. */
HRESULT art_d3d11_pass_compile(ArtD3D11Pass *pass, const void *source, size_t bytes,
                               ArtD3D11Program **program, ID3DBlob **diagnostics);
void art_d3d11_program_destroy(ArtD3D11Program *program);
uint32_t art_d3d11_constant_bytes(const ArtD3D11Program *program);
/* Fill a zero-initialized b0 block by name. S_FALSE means no such uniform. */
HRESULT art_d3d11_set_uniform(const ArtD3D11Program *program, const char *name,
                              const float *values, uint32_t count, void *block, size_t bytes);

/* Draw a triangle-strip quad, with t0..t3, s0 and b0 supplied by the Host. */
HRESULT art_d3d11_pass_draw(ArtD3D11Pass *pass, const ArtD3D11Program *program,
                            const ArtD3D11Draw *draw, void *bgra, size_t capacity);

#endif
