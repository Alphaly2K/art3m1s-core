#ifndef ARTEMIS_DXVK_METAL_WRITER_H
#define ARTEMIS_DXVK_METAL_WRITER_H

#include <IOSurface/IOSurfaceRef.h>
#include <stdint.h>

struct ArtMetalWriter;

extern "C" {
ArtMetalWriter *art_metal_writer_create(void *device, IOSurfaceRef surface, uint32_t width, uint32_t height);
bool art_metal_writer_clear(ArtMetalWriter *writer, const uint8_t rgba[4]);
bool art_metal_writer_copy(ArtMetalWriter *writer, IOSurfaceRef source);
void art_metal_writer_destroy(ArtMetalWriter *writer);
}

#endif
