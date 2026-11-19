#include "art3m1s_core_v2.draft.h"

#include <assert.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

_Static_assert(sizeof(ArtApiHeader) == 24, "API table prefix");
_Static_assert(offsetof(ArtMessage, session_epoch) == 16, "message epoch");
_Static_assert(offsetof(ArtStepInfo, monotonic_ns) == 16, "step clock");

static const char ini[] = "[WINDOWS]\nWIDTH=640\nHEIGHT=480\nBOOT=boot.iet\nSAVEPATH=C:\\Profiles\\Game\n";
static const char boot[] = "*top\n[lyc id=1 width=2 height=1 color=ff112233]\n"
                           "[takess]\n[savess file=thumb]\n"
                           "[httpget url=https://example.invalid/status varname_code=t.http]\n"
                           "[caption data=ready]\n[@]\n[caption data=advanced]\n[stop]\n";
#define SHADER_SCENE \
    "[lyc id=1 width=4 height=4 color=ff112233]\n"
#define SHADER_BINDINGS \
    "[lyprop id=1 shader=paint shaderconstant=\"tint,param\" tint=\"0,1,0.5,1\" param=1 " \
    "mask=mask shadertexture=textureUser textureUser=user]\n"
static const char shader_boot[] = "*top\n[lyshader id=paint file=paint.hlsl]\n"
    SHADER_SCENE SHADER_BINDINGS "[flip]\n[caption data=ready]\n[stop]\n";
static const char glsl_boot[] = "*top\n[lyshader id=paint file=paint.Frag]\n"
    SHADER_SCENE SHADER_BINDINGS "[flip]\n[caption data=ready]\n[stop]\n";
static const char shader_ini[] = "[WINDOWS]\nWIDTH=4\nHEIGHT=4\nBOOT=boot.iet\n";
static const char shader[] = "Texture2D textureFore : register(t2); Texture2D textureMask : register(t3); "
    "Texture2D textureUser : register(t0); SamplerState sam : register(s0); "
    "cbuffer Params : register(b0) { float4 tint; float param; }; "
    "float4 main(float2 uv : TEXCOORD0) : SV_Target { "
    "float4 fore = textureFore.Sample(sam, uv) * tint; "
    "return float4(fore.rgb * textureMask.Sample(sam, uv).r + textureUser.Sample(sam, uv).rgb * param, fore.a); }";
#define GLSL_BODY \
    "uniform sampler2D textureFore; uniform sampler2D samplerMask;" \
    "uniform sampler2D textureUser; uniform vec4 tint;\n" \
    "void main() { vec4 fore = SAMPLE(textureFore, v_uv) * tint;" \
    " OUTPUT = vec4(fore.rgb * SAMPLE(samplerMask, v_uv).r + SAMPLE(textureUser, v_uv).rgb, fore.a); }"
static const char glsl[] = "// Artemis fragment\n#version 330 core\n"
    "in vec2 v_uv; out vec4 color;\n#define OUTPUT color\n#define SAMPLE texture\n" GLSL_BODY;
static const char gles[] = "\xef\xbb\xbf#version 300 es\rprecision highp float;\r\n"
    "in vec2 v_uv; out vec4 color;\n#define OUTPUT color\n#define SAMPLE texture\n" GLSL_BODY;
static const char gles100[] = "// \x82\xa0\n"
    "varying vec2 resultCoord0; varying vec2 resultCoord1;\n"
    "#define v_uv (mix(resultCoord0, resultCoord1, 0.5))\n"
    "#define OUTPUT gl_FragColor\n#define SAMPLE texture2D\n" GLSL_BODY;
static const uint8_t mask_png[] = { // RGBA (128, 0, 0, 255)
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
    0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
    0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
    0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x68, 0x60, 0x60, 0xf8,
    0x0f, 0x00, 0x03, 0x04, 0x01, 0x80, 0x0b, 0x83, 0xc8, 0x14, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
};
static const uint8_t user_png[] = { // RGBA (5, 7, 11, 255)
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
    0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
    0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
    0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x65, 0xe7, 0xfe,
    0x0f, 0x00, 0x01, 0x43, 0x01, 0x17, 0xd9, 0x5a, 0x21, 0x7f, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
};
static enum { NO_SHADER, HLSL, GLSL, GLES, GLES100 } shader_frame;
static uint8_t external_glsl[256 * 1024];
static size_t external_glsl_len;

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
    const char *asset = NULL;
    if (strstr(metadata, "\"key\":\"boot.iet\""))
        asset = shader_frame == HLSL ? shader_boot : shader_frame ? glsl_boot : boot;
    if (shader_frame == HLSL && strstr(metadata, "\"key\":\"paint.hlsl\"")) asset = shader;
    if (strstr(metadata, "\"key\":\"paint.frag\"")) {
        if (shader_frame == GLSL) asset = glsl;
        if (shader_frame == GLES) asset = gles;
        if (shader_frame == GLES100) asset = gles100;
    }
    ArtBytes payload = asset ? text(asset) : bytes(NULL, 0);
    if (external_glsl_len && strstr(metadata, "\"key\":\"paint.frag\""))
        payload = bytes(external_glsl, external_glsl_len);
    if (strstr(metadata, "\"key\":\"mask.png\"")) payload = bytes(mask_png, sizeof(mask_png));
    if (strstr(metadata, "\"key\":\"user.png\"")) payload = bytes(user_png, sizeof(user_png));
    if (kind == ART_IO_STAT) {
        char result[120];
        if (payload.data) {
            int written = snprintf(result, sizeof(result),
                "{\"status\":\"ok\",\"found\":true,\"size\":\"%zu\",\"revision\":\"1\"}", (size_t)payload.size);
            assert(written > 0 && (size_t)written < sizeof(result));
        } else {
            strcpy(result, "{\"status\":\"ok\",\"found\":false}");
        }
        ArtMessage reply = message(ART_DOMAIN_IO, kind, epoch, request, result);
        assert(api->submit(rt, &reply, 1) == ART_OK);
        assert(api->submit(rt, &reply, 1) == ART_E_STALE);
    } else {
        assert(payload.data && kind == ART_IO_READ && strstr(metadata, "\"offset\":\"0\""));
        ArtMessage reply = message(ART_DOMAIN_IO, kind, epoch, request,
                                   "{\"status\":\"ok\",\"revision\":\"1\",\"eof\":true}");
        reply.blob = payload;
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
            } else if (domain == ART_DOMAIN_UI && kind == ART_UI_HTTP_REQUEST) {
                assert(result.flags & ART_WAIT_UI);
                assert(strstr(metadata, "\"method\":\"get\""));
                ArtMessage reply = message(domain, kind, epoch, request,
                                           "{\"status\":\"ok\",\"status_code\":204}");
                assert(api->submit(rt, &reply, 1) == ART_OK);
                captions |= 16;
            } else if (domain == ART_DOMAIN_UI && kind == ART_UI_CAPTION) {
                if (strstr(metadata, "\"ready\"")) captions |= 1;
                if (strstr(metadata, "\"advanced\"")) captions |= 2;
            }
            offset += size;
        }
        assert(offset == poll.written_bytes);
        if (result.flags & (ART_WAIT_IO | ART_WAIT_UI | ART_WAIT_RENDER)) continue;
        return captions;
    }
    assert(!"Host fixture did not settle");
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--shader-frame")) shader_frame = HLSL;
    else if (argc == 2 && !strcmp(argv[1], "--glsl-frame")) shader_frame = GLSL;
    else if (argc == 2 && !strcmp(argv[1], "--gles-frame")) shader_frame = GLES;
    else if (argc == 2 && !strcmp(argv[1], "--gles100-frame")) shader_frame = GLES100;
    else if (argc != 1) return 2;
    const char *source = getenv("ARTEMIS_GL_SMOKE_SOURCE");
    if (source && shader_frame == GLES100) {
        FILE *shader_file = fopen(source, "rb");
        if (!shader_file) return 2;
        external_glsl_len = fread(external_glsl, 1, sizeof(external_glsl), shader_file);
        int valid = external_glsl_len && feof(shader_file) && !ferror(shader_file);
        if (fclose(shader_file) || !valid) return 2;
    }
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

    ArtCreateInfo info = { .struct_size = sizeof(info), .host_service_caps = ART_HOST_CAP_IO | ART_HOST_CAP_HTTP,
                           .renderer_caps = ART_RENDERER_CAP_SPRITE | ART_RENDERER_CAP_SCREENSHOT };
    if (shader_frame == HLSL) info.renderer_caps |= ART_RENDERER_CAP_HLSL;
    else if (shader_frame) info.renderer_caps |= ART_RENDERER_CAP_GLSL;
    ArtRuntimeHandle rt = 0;
    assert(api->create(&info, &rt) == ART_OK && rt != 0);
    ArtProjectInfo project = { .struct_size = sizeof(project),
                               .ini_bytes = text(shader_frame ? shader_ini : ini),
                               .platform_utf8 = text("windows") };
    assert(api->open_project(rt, &project) == ART_OK);
    if (shader_frame) {
        unsigned captions = 0;
        for (uint64_t id = 1; id <= 8 && !captions; ++id) captions = pump(api, rt, id);
        assert(captions == 1);
        ArtFrameView frame = { .struct_size = sizeof(frame) };
        assert(frames->acquire_latest(rt, 0, &frame) == ART_OK);
        assert(frame.packet.size >= 56);
        const uint8_t *packet = frame.packet.data;
        uint32_t textures = (uint32_t)little(packet + 12, 4);
        assert(fwrite(packet, 1, (size_t)frame.packet.size, stdout) == frame.packet.size);
        for (uint32_t i = 0; i < textures; ++i) {
            ArtBytes rgba = {0};
            assert(frames->texture_rgba(rt, frame.token, i, &rgba) == ART_OK);
            assert(fwrite(rgba.data, 1, (size_t)rgba.size, stdout) == rgba.size);
        }
        assert(frames->release(rt, frame.token) == ART_OK);
        assert(api->destroy(rt) == ART_OK && fflush(stdout) == 0);
        return 0;
    }
    ArtPollResult short_poll = { .struct_size = sizeof(short_poll) };
    assert(api->poll_events(rt, NULL, 0, &short_poll) == ART_E_BUFFER);
    assert(short_poll.required_next_bytes > 0 && short_poll.record_count == 0);
    assert(pump(api, rt, 1) == (1 | 4 | 8 | 16));

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
    info.host_service_caps = ART_HOST_CAP_IO | ART_HOST_CAP_PROJECT_SAVE_PATH;
    assert(api->create(&info, &rt) == ART_OK);
    assert(api->open_project(rt, &project) == ART_OK);
    uint8_t opened[256];
    ArtPollResult first = { .struct_size = sizeof(first) };
    assert(api->poll_events(rt, opened, sizeof(opened), &first) == ART_OK);
    static const char save_path[] = "{\"save_path\":\"Profiles/Game\"}";
    assert(first.record_count == 1 && first.written_bytes == 64 + sizeof(save_path) - 1);
    assert(little(opened + 8, 4) == ART_RUNTIME_SESSION_CHANGED);
    assert(!memcmp(opened + 64, save_path, sizeof(save_path) - 1));
    assert(pump(api, rt, 1) == (1 | 4 | 8));
    assert(api->destroy(rt) == ART_OK);
    puts("Artemis v2 Host smoke passed");
    return 0;
}
