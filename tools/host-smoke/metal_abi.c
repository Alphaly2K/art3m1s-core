#include "art3m1s_metal_host_v1.h"

#include <CoreVideo/CoreVideo.h>
#include <dlfcn.h>
#include <stddef.h>
#include <stdio.h>

static void *library;
static const ArtRuntimeApiV2 *runtime;
static const ArtFrameApiV2 *frame;
static void *native_library;
static __typeof__(art3m1s_metal_host_v1_create) *native_create;
static __typeof__(art3m1s_metal_host_v1_destroy) *native_destroy;
static __typeof__(art3m1s_metal_host_v1_set_surface) *native_surface;
static __typeof__(art3m1s_metal_host_v1_present) *native_present;
static __typeof__(art3m1s_metal_host_v1_capture) *native_capture;

void metal_disconnect(void) {
    dlclose(library);
    library = NULL;
    runtime = NULL;
    frame = NULL;
}

ArtStatus metal_connect(const char *path) {
    library = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!library) {
        fprintf(stderr, "%s\n", dlerror());
        return ART_E_UNSUPPORTED;
    }
    typedef ArtStatus (*QueryRoot)(uint32_t, uint32_t, const ArtRootApiV2 **);
    QueryRoot query = (QueryRoot)dlsym(library, "art3m1s_get_api_v2");
    const ArtRootApiV2 *root = NULL;
    const void *table = NULL;
    if (!query || query(ART_ABI_V2_MAJOR, sizeof(*root), &root) != ART_OK ||
        !root || root->header.magic != ART_API_MAGIC ||
        root->query_interface(ART_INTERFACE_RUNTIME, ART_ABI_V2_MAJOR,
                              sizeof(*runtime), &table) != ART_OK) {
        metal_disconnect();
        return ART_E_UNSUPPORTED;
    }
    runtime = table;
    if (root->query_interface(ART_INTERFACE_FRAME, ART_ABI_V2_MAJOR,
                              sizeof(*frame), &table) != ART_OK) {
        metal_disconnect();
        return ART_E_UNSUPPORTED;
    }
    frame = table;
    return ART_OK;
}

ArtStatus metal_create(ArtRuntimeHandle *out) {
    ArtCreateInfo create = {
        .struct_size = sizeof(create),
        .host_service_caps = ART_HOST_CAP_IO,
        .renderer_caps = ART_METAL_HOST_V1_RENDERER_CAPS,
    };
    ArtStatus status = runtime->create(&create, out);
    if (status != ART_OK) return status;
    static const uint8_t ini[] = "[WINDOWS]\nWIDTH=4\nHEIGHT=4\nBOOT=boot.iet\n";
    static const uint8_t platform[] = "windows";
    ArtProjectInfo project = {
        .struct_size = sizeof(project),
        .ini_bytes = {ini, sizeof(ini) - 1},
        .platform_utf8 = {platform, sizeof(platform) - 1},
    };
    status = runtime->open_project(*out, &project);
    if (status != ART_OK) runtime->destroy(*out);
    return status;
}

ArtStatus metal_destroy(ArtRuntimeHandle rt) {
    return runtime->destroy(rt);
}

ArtStatus metal_step(ArtRuntimeHandle rt, uint64_t id, uint64_t ms, uint32_t *waiting) {
    ArtStepInfo step = { .struct_size = sizeof(step), .host_frame_id = id,
                         .monotonic_ns = ms * UINT64_C(1000000) };
    ArtStepResult result = { .struct_size = sizeof(result) };
    ArtStatus status = runtime->step(rt, &step, &result);
    *waiting = status == ART_WOULD_BLOCK ||
        (result.flags & (ART_WAIT_IO | ART_WAIT_UI | ART_WAIT_RENDER)) != 0;
    return status == ART_WOULD_BLOCK ? ART_OK : status;
}

ArtStatus metal_poll(ArtRuntimeHandle rt, uint8_t *buffer, uint64_t capacity, uint64_t *size) {
    ArtPollResult poll = { .struct_size = sizeof(poll) };
    ArtStatus status = runtime->poll_events(rt, buffer, capacity, &poll);
    *size = poll.written_bytes;
    return status == ART_NO_DATA ? ART_OK : status;
}

ArtStatus metal_submit(ArtRuntimeHandle rt, uint16_t domain, uint32_t kind, uint64_t epoch,
                       uint64_t request_id, ArtBytes metadata, ArtBytes blob) {
    ArtMessage reply = { .struct_size = sizeof(reply), .domain = domain,
                         .schema_version = ART_SUBMIT_SCHEMA_VERSION, .kind = kind,
                         .session_epoch = epoch, .request_id = request_id,
                         .metadata_utf8 = metadata, .blob = blob };
    return runtime->submit(rt, &reply, 1);
}

ArtStatus metal_acquire(ArtRuntimeHandle rt, uint64_t after, ArtFrameToken *token, ArtBytes *packet) {
    ArtFrameView view = { .struct_size = sizeof(view) };
    ArtStatus status = frame->acquire_latest(rt, after, &view);
    if (status != ART_OK) return status;
    if (view.schema_version != ART_FRAME_SCHEMA_VERSION) {
        frame->release(rt, view.token);
        return ART_E_UNSUPPORTED;
    }
    *token = view.token;
    *packet = view.packet;
    return ART_OK;
}

ArtStatus metal_pixels(ArtRuntimeHandle rt, ArtFrameToken token, uint32_t index, ArtBytes *pixels) {
    return frame->texture_rgba(rt, token, index, pixels);
}

ArtStatus metal_release(ArtRuntimeHandle rt, ArtFrameToken token) {
    return frame->release(rt, token);
}

ArtStatus metal_native_open(const char *path, ArtRuntimeHandle rt, ArtMetalHost **out) {
    native_library = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!native_library) {
        fprintf(stderr, "%s\n", dlerror());
        return ART_E_UNSUPPORTED;
    }
    native_create = dlsym(native_library, "art3m1s_metal_host_v1_create");
    native_destroy = dlsym(native_library, "art3m1s_metal_host_v1_destroy");
    native_surface = dlsym(native_library, "art3m1s_metal_host_v1_set_surface");
    native_present = dlsym(native_library, "art3m1s_metal_host_v1_present");
    native_capture = dlsym(native_library, "art3m1s_metal_host_v1_capture");
    ArtStatus status = ART_E_UNSUPPORTED;
    if (native_create && native_destroy && native_surface && native_present && native_capture) {
        status = native_create(frame, rt, out);
    }
    if (status != ART_OK) dlclose(native_library);
    return status;
}

ArtStatus metal_native_check_binding(ArtRuntimeHandle rt) {
    ArtMetalHost *host = NULL;
    ArtStatus status = native_create(frame, rt, &host);
    if (host) native_destroy(host);
    return status;
}

void metal_native_close(ArtMetalHost *host) {
    native_destroy(host);
    dlclose(native_library);
}

ArtStatus metal_native_surface(ArtMetalHost *host, IOSurfaceRef surface) {
    return native_surface(host, surface);
}

ArtStatus metal_native_present(ArtMetalHost *host, uint64_t after, uint64_t *out) {
    return native_present(host, after, out);
}

ArtStatus metal_native_capture(ArtMetalHost *host, uint64_t id, uint32_t width, uint32_t height,
                              uint8_t *rgba, uint64_t size) {
    return native_capture(host, id, width, height, rgba, size);
}

void *metal_create_pixel_buffer(uint32_t width, uint32_t height) {
    CFDictionaryRef properties = CFDictionaryCreate(NULL, NULL, NULL, 0,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    if (!properties) return NULL;
    const void *keys[] = { kCVPixelBufferIOSurfacePropertiesKey,
                           kCVPixelBufferMetalCompatibilityKey };
    const void *values[] = { properties, kCFBooleanTrue };
    CFDictionaryRef attributes = CFDictionaryCreate(NULL, keys, values, 2,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFRelease(properties);
    if (!attributes) return NULL;
    CVPixelBufferRef buffer = NULL;
    CVReturn status = CVPixelBufferCreate(NULL, width, height, kCVPixelFormatType_32BGRA,
                                         attributes, &buffer);
    CFRelease(attributes);
    return status == kCVReturnSuccess ? buffer : NULL;
}

void *metal_pixel_buffer_surface(void *buffer) {
    return CVPixelBufferGetIOSurface(buffer);
}

void metal_free_pixel_buffer(void *buffer) {
    CVPixelBufferRelease(buffer);
}

CVReturn metal_read_pixel_buffer(void *buffer, uint8_t *rgba, uint64_t size) {
    const size_t width = CVPixelBufferGetWidth(buffer);
    const size_t height = CVPixelBufferGetHeight(buffer);
    if (size != width * height * 4) return kCVReturnInvalidArgument;
    const CVPixelBufferLockFlags lock = kCVPixelBufferLock_ReadOnly;
    CVReturn status = CVPixelBufferLockBaseAddress(buffer, lock);
    if (status != kCVReturnSuccess) return status;
    const size_t stride = CVPixelBufferGetBytesPerRow(buffer);
    const uint8_t *base = CVPixelBufferGetBaseAddress(buffer);
    for (size_t y = 0; y < height; ++y) {
        const uint8_t *row = base + y * stride;
        for (size_t i = 0; i < width * 4; i += 4) {
            rgba[0] = row[i + 2];
            rgba[1] = row[i + 1];
            rgba[2] = row[i];
            rgba[3] = row[i + 3];
            rgba += 4;
        }
    }
    return CVPixelBufferUnlockBaseAddress(buffer, lock);
}
