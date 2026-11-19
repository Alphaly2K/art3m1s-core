#ifndef ARTEMIS_DXVK_PASS_H
#define ARTEMIS_DXVK_PASS_H

#include <IOSurface/IOSurfaceRef.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct ArtDxvkContext ArtDxvkContext;
typedef struct ArtDxvkProgram ArtDxvkProgram;
typedef struct ArtDxvkTargets ArtDxvkTargets;

typedef struct ArtDxvkImage {
    const uint8_t *rgba;
    uint32_t width, height;
} ArtDxvkImage;

typedef struct ArtDxvkUniform {
    const uint8_t *name;
    size_t name_length;
    const float *values;
    size_t count;
} ArtDxvkUniform;

/* Use one context per owner thread. Destroy its programs and targets before the context. */
ArtDxvkContext *art_dxvk_context_create(void);
void *art_dxvk_context_metal_device(const ArtDxvkContext *context);
void art_dxvk_context_destroy(ArtDxvkContext *context);

/* Load an ADE2 shader pair from caller-owned bytes. No byte borrow survives the call. */
ArtDxvkProgram *art_dxvk_program_create(ArtDxvkContext *context, const uint8_t *ade, size_t size);
void art_dxvk_program_destroy(ArtDxvkProgram *program);

/* Import distinct BGRA IOSurfaces of equal size. Call before the first foreground write. */
ArtDxvkTargets *art_dxvk_targets_create(ArtDxvkContext *context, IOSurfaceRef foreground, IOSurfaceRef output);
void art_dxvk_targets_destroy(ArtDxvkTargets *targets);

/* Wait for the foreground producer before calling. Returns after output is ready for Metal. */
int art_dxvk_draw(ArtDxvkContext *context, ArtDxvkProgram *program, const ArtDxvkTargets *targets,
                  ArtDxvkImage mask, ArtDxvkImage user, const ArtDxvkUniform *uniforms, size_t uniform_count);

#ifdef __cplusplus
}
#endif

#endif
