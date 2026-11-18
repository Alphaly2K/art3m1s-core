#include "art3m1s_metal_host_v1.h"

ArtStatus metal_host_check_frames(const ArtFrameApiV2 *frames) {
    if (!frames || frames->header.magic != ART_API_MAGIC ||
        frames->header.struct_size < sizeof(*frames) ||
        frames->header.interface_id != ART_INTERFACE_FRAME ||
        frames->header.abi_major != ART_ABI_V2_MAJOR ||
        !frames->acquire_latest || !frames->release || !frames->texture_rgba) {
        return ART_E_UNSUPPORTED;
    }
    return ART_OK;
}

ArtStatus metal_host_borrow(const ArtFrameApiV2 *frames, ArtRuntimeHandle runtime, uint64_t after,
                           ArtFrameToken *token, uint64_t *frame_id,
                           const uint8_t **packet, uint64_t *size) {
    ArtFrameView view = { .struct_size = sizeof(view) };
    ArtStatus status = frames->acquire_latest(runtime, after, &view);
    if (status != ART_OK) return status;
    if (view.schema_version != ART_FRAME_SCHEMA_VERSION) {
        frames->release(runtime, view.token);
        return ART_E_UNSUPPORTED;
    }
    *token = view.token;
    *frame_id = view.render_frame_id;
    *packet = view.packet.data;
    *size = view.packet.size;
    return ART_OK;
}

ArtStatus metal_host_release(const ArtFrameApiV2 *frames, ArtRuntimeHandle runtime, ArtFrameToken token) {
    return frames->release(runtime, token);
}

ArtStatus metal_host_texture(const ArtFrameApiV2 *frames, ArtRuntimeHandle runtime, ArtFrameToken token,
                            uint32_t index, const uint8_t **data, uint64_t *size) {
    ArtBytes pixels = {0};
    ArtStatus status = frames->texture_rgba(runtime, token, index, &pixels);
    *data = pixels.data;
    *size = pixels.size;
    return status;
}

ArtStatus metal_host_surface_extent(IOSurfaceRef surface, uint32_t *width, uint32_t *height) {
    /* 32BGRA, as used by the shared CVPixelBuffer. */
    if (IOSurfaceGetPixelFormat(surface) != UINT32_C(0x42475241)) return ART_E_ARGUMENT;
    const size_t w = IOSurfaceGetWidth(surface);
    const size_t h = IOSurfaceGetHeight(surface);
    if (!w || !h || w > UINT32_MAX || h > UINT32_MAX) return ART_E_ARGUMENT;
    *width = (uint32_t)w;
    *height = (uint32_t)h;
    return ART_OK;
}
