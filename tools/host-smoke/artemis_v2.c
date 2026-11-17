#include "art3m1s_core_v2.draft.h"

#include <assert.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>

_Static_assert(sizeof(ArtApiHeader) == 24, "API table prefix");
_Static_assert(offsetof(ArtMessage, session_epoch) == 16, "message epoch");
_Static_assert(offsetof(ArtStepInfo, monotonic_ns) == 16, "step clock");

static const char ini[] = "[WINDOWS]\nWIDTH=640\nHEIGHT=480\nBOOT=boot.iet\n";
static const char boot[] = "*top\n[lyc id=1 width=2 height=1 color=ff112233]\n"
                           "[takess]\n[savess file=thumb]\n"
                           "[caption data=ready]\n[@]\n[caption data=advanced]\n[stop]\n";

static ArtBytes bytes(const void *data, size_t size) {
    return (ArtBytes){ data, size };
}

static ArtBytes text(const char *str) {
    return bytes(str, strlen(str));
}

static uint64_t little(const uint8_t *bytes, size_t n) {
    uint64_t value = 0;
    for (size_t i = 0; i < n; ++i) value |= (uint64_t)bytes[i] << (i * 8);
    return value;
}

static ArtMessage message(uint16_t domain, uint32_t kind, uint64_t epoch,
                          uint64_t request, const char *metadata) {
    return (ArtMessage){ sizeof(ArtMessage), domain, ART_SUBMIT_SCHEMA_VERSION,
                         kind, 0, epoch, request, text(metadata), {0} };
}

static void service_io(const ArtRuntimeApiV2 *api, ArtRuntimeHandle rt,
                       uint32_t kind, uint64_t epoch, uint64_t request,
                       const char *metadata) {
    if (kind == ART_IO_STAT) {
        char result[120];
        if (strstr(metadata, "\"key\":\"boot.iet\"")) {
            int written = snprintf(result, sizeof(result),
                "{\"status\":\"ok\",\"found\":true,\"size\":\"%zu\",\"revision\":\"1\"}", strlen(boot));
            assert(written > 0 && (size_t)written < sizeof(result));
        } else {
            strcpy(result, "{\"status\":\"ok\",\"found\":false}");
        }
        ArtMessage reply = message(ART_DOMAIN_IO, kind, epoch, request, result);
        assert(api->submit(rt, &reply, 1) == ART_OK);
        assert(api->submit(rt, &reply, 1) == ART_E_STALE);
    } else {
        assert(kind == ART_IO_READ && strstr(metadata, "\"offset\":\"0\""));
        ArtMessage reply = message(ART_DOMAIN_IO, kind, epoch, request,
                                   "{\"status\":\"ok\",\"revision\":\"1\",\"eof\":true}");
        reply.blob = text(boot);
        assert(api->submit(rt, &reply, 1) == ART_OK);
    }
}

static unsigned pump(const ArtRuntimeApiV2 *api, ArtRuntimeHandle rt, uint64_t frame_id) {
    ArtStepInfo step = { .struct_size = sizeof(step), .host_frame_id = frame_id,
                         .monotonic_ns = frame_id * UINT64_C(1000000) };
    ArtStepResult result = { .struct_size = sizeof(result) };
    unsigned captions = 0;
    for (unsigned turn = 0; turn < 64; ++turn) {
        assert(api->step(rt, &step, &result) == ART_OK);
        uint8_t events[4096];
        ArtPollResult poll = { .struct_size = sizeof(poll) };
        ArtStatus status = api->poll_events(rt, events, sizeof(events), &poll);
        assert(status == ART_OK || status == ART_NO_DATA);
        size_t offset = 0;
        for (uint32_t index = 0; index < poll.record_count; ++index) {
            assert(poll.written_bytes - offset >= 64);
            const uint8_t *record = events + offset;
            size_t size = (size_t)little(record, 4);
            assert(size >= 64 && size <= poll.written_bytes - offset);
            uint16_t domain = (uint16_t)little(record + 6, 2);
            uint32_t kind = (uint32_t)little(record + 8, 4);
            uint64_t epoch = little(record + 24, 8);
            uint64_t request = little(record + 40, 8);
            size_t meta_size = (size_t)little(record + 56, 4);
            char metadata[512] = {0};
            assert(meta_size < sizeof(metadata) && meta_size <= size - 64);
            memcpy(metadata, record + 64, meta_size);
            if (domain == ART_DOMAIN_IO) {
                service_io(api, rt, kind, epoch, request, metadata);
            } else if (domain == ART_DOMAIN_RENDER) {
                assert(result.flags & ART_WAIT_RENDER);
                if (kind == ART_RENDER_CAPTURE) {
                    assert(strstr(metadata, "\"render_frame_id\":"));
                    captions |= 4;
                } else {
                    assert(kind == ART_RENDER_SAVE_CAPTURE && strstr(metadata, "\"key\":\"thumb.png\""));
                    captions |= 8;
                }
                ArtMessage reply = message(domain, kind, epoch, request,
                    kind == ART_RENDER_CAPTURE ? "{\"status\":\"ok\"}" : "{\"status\":\"denied\"}");
                assert(api->submit(rt, &reply, 1) == ART_OK);
            } else if (domain == ART_DOMAIN_UI && kind == ART_UI_CAPTION) {
                if (strstr(metadata, "\"ready\"")) captions |= 1;
                if (strstr(metadata, "\"advanced\"")) captions |= 2;
            }
            offset += size;
        }
        assert(offset == poll.written_bytes);
        if (result.flags & (ART_WAIT_IO | ART_WAIT_RENDER)) continue;
        return captions;
    }
    assert(!"Host fixture did not settle");
    return 0;
}

int main(void) {
    const ArtRootApiV2 *root = NULL;
    assert(art3m1s_get_api_v2(ART_ABI_V2_MAJOR, sizeof(*root), &root) == ART_OK);
    assert(root && root->header.magic == ART_API_MAGIC);
    const void *table = NULL;
    assert(root->query_interface(99, ART_ABI_V2_MAJOR, 0, &table) == ART_E_UNSUPPORTED);
    uint64_t needed = 0;
    assert(root->copy_last_error(NULL, 0, &needed) == ART_E_BUFFER);
    uint8_t error[120];
    assert(needed > 0 && needed < sizeof(error));
    assert(root->copy_last_error(error, sizeof(error), &needed) == ART_OK);
    assert(root->query_interface(ART_INTERFACE_RUNTIME, ART_ABI_V2_MAJOR, sizeof(ArtRuntimeApiV2), &table) == ART_OK);
    const ArtRuntimeApiV2 *api = table;
    assert(root->query_interface(ART_INTERFACE_FRAME, ART_ABI_V2_MAJOR, sizeof(ArtFrameApiV2), &table) == ART_OK);
    const ArtFrameApiV2 *frames = table;

    ArtCreateInfo info = { .struct_size = sizeof(info), .host_service_caps = ART_HOST_CAP_IO,
                           .renderer_caps = ART_RENDERER_CAP_SPRITE | ART_RENDERER_CAP_SCREENSHOT };
    ArtRuntimeHandle rt = 0;
    assert(api->create(&info, &rt) == ART_OK && rt != 0);
    ArtProjectInfo project = { .struct_size = sizeof(project), .ini_bytes = text(ini),
                               .platform_utf8 = text("windows") };
    assert(api->open_project(rt, &project) == ART_OK);
    ArtPollResult short_poll = { .struct_size = sizeof(short_poll) };
    assert(api->poll_events(rt, NULL, 0, &short_poll) == ART_E_BUFFER);
    assert(short_poll.required_next_bytes > 0 && short_poll.record_count == 0);
    assert(pump(api, rt, 1) == (1 | 4 | 8));

    struct { ArtRuntimeState prefix; uint64_t tail; } state = {
        .prefix.struct_size = sizeof(state), .tail = UINT64_C(0x123456789abcdef0)
    };
    assert(api->get_state(rt, &state.prefix) == ART_OK);
    uint64_t epoch = state.prefix.session_epoch;
    assert(state.prefix.state == ART_RUNTIME_RUNNING && state.prefix.stage_width == 640);
    assert(state.tail == UINT64_C(0x123456789abcdef0));
    ArtFrameView view = { .struct_size = sizeof(view) };
    assert(frames->acquire_latest(rt, 0, &view) == ART_OK);
    assert(view.packet.size >= 4 && !memcmp(view.packet.data, "ARFP", 4));
    ArtBytes pixels = {0};
    assert(frames->texture_rgba(rt, view.token, 0, &pixels) == ART_OK);
    const uint8_t expected[] = {0x11, 0x22, 0x33, 0xff};
    assert(pixels.size == sizeof(expected) && !memcmp(pixels.data, expected, sizeof(expected)));
    assert(api->destroy(rt) == ART_E_STATE);

    ArtMessage keys[] = {
        message(ART_DOMAIN_INPUT, ART_INPUT_KEY, epoch, 0, "{\"code\":13,\"down\":true}"),
        message(ART_DOMAIN_INPUT, ART_INPUT_KEY, epoch, 0, "{\"code\":0,\"down\":true}")
    };
    assert(api->submit(rt, keys, 2) == ART_E_ARGUMENT);
    assert(pump(api, rt, 2) == 0);
    assert(api->submit(rt, keys, 1) == ART_OK);
    assert(pump(api, rt, 3) == 2);
    assert(api->open_project(rt, &project) == ART_OK);
    assert(api->submit(rt, keys, 1) == ART_E_STALE);
    assert(!memcmp(pixels.data, expected, sizeof(expected)));
    assert(frames->release(rt, view.token) == ART_OK);
    assert(frames->release(rt, view.token) == ART_E_HANDLE);
    assert(api->destroy(rt) == ART_OK);
    assert(api->destroy(rt) == ART_E_HANDLE);
    puts("Artemis v2 Host smoke passed");
    return 0;
}
