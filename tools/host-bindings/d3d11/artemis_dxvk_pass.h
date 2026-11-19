#ifndef ARTEMIS_DXVK_PASS_H
#define ARTEMIS_DXVK_PASS_H

#include <IOSurface/IOSurfaceRef.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct ArtDxvkPass ArtDxvkPass;

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

/* Owner-thread pass for matching BGRA surfaces. Attach before writing foreground. */
ArtDxvkPass *art_dxvk_pass_create(const char *vertex_dxbc, const char *effect_ade,
                                 IOSurfaceRef foreground, IOSurfaceRef output);
void *art_dxvk_pass_metal_device(const ArtDxvkPass *pass);
/* Wait for the producer before drawing. Returns after output is ready for Metal. */
int art_dxvk_pass_draw(ArtDxvkPass *pass, ArtDxvkImage mask, ArtDxvkImage user,
                       const ArtDxvkUniform *uniforms, size_t uniform_count);
void art_dxvk_pass_destroy(ArtDxvkPass *pass);

#ifdef __cplusplus
}
#endif

#endif
