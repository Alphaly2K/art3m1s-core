#include "art3m1s_core_v2.draft.h"

#include <dlfcn.h>
#include <stddef.h>
#include <stdio.h>

static void *library;
static const ArtRuntimeApiV2 *runtime;
static const ArtFrameApiV2 *frame;

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
        .renderer_caps = ART_RENDERER_CAP_SPRITE | ART_RENDERER_CAP_TRANSITION |
                         ART_RENDERER_CAP_SCREENSHOT | ART_RENDERER_CAP_GROUP |
                         ART_RENDERER_CAP_EMOTE | ART_RENDERER_CAP_HLSL,
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
