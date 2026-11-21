#include "art3m1s_core_v2.draft.h"

#include <assert.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef ARTEMIS_DXVK_HOST_SMOKE
#include "art3m1s_metal_host_v1.h"
void *metal_create_pixel_buffer(uint32_t width, uint32_t height);
IOSurfaceRef metal_pixel_buffer_surface(void *buffer);
void metal_free_pixel_buffer(void *buffer);
int32_t metal_read_pixel_buffer(void *buffer, uint8_t *rgba, uint64_t size);
#endif

_Static_assert(sizeof(ArtApiHeader) == 24, "API table prefix");
_Static_assert(offsetof(ArtMessage, session_epoch) == 16, "message epoch");
_Static_assert(offsetof(ArtStepInfo, monotonic_ns) == 16, "step clock");

static const char ini[] = "[WINDOWS]\nWIDTH=640\nHEIGHT=480\nBOOT=boot.iet\nSAVEPATH=C:\\Profiles\\Game\n";
static const char boot[] = "*top\n[lua]\n"
                           "print('boot', setmetatable({}, {__tostring = function() return 'ready' end}), string.char(255))\n"
                           "[/lua]\n[debug mode=1 level=1]\n[debugprint data=boot]\n"
                           "[lyc id=1 width=2 height=1 color=ff112233]\n"
                           "[takess]\n[savess file=thumb]\n"
                           "[httpget url=https://example.invalid/status varname_code=t.http]\n"
                           "[caption data=ready]\n[@]\n"
                           "[var system=fullscreen name=t.fs]\n[var system=minimize name=t.min]\n"
                           "[if estimate=\"$t.fs == 1 && t.min == 1\"]\n[caption data=advanced]\n[/if]\n"
                           "[stop]\n";
static const char video_boot[] = "*top\n[video file=probe.wmv]\n[caption data=advanced]\n[stop]\n";
static const char *boot_source = boot;
static char video_playback[21];
#define SHADER_SCENE \
    "[lyc id=1 width=4 height=4 color=ff112233]\n"
#define SHADER_BINDINGS \
    "[lyprop id=1 shader=paint shaderconstant=\"tint,param\" tint=\"0,1,0.5,1\" param=1 " \
    "mask=mask shadertexture=textureUser textureUser=user]\n"
static const char shader_boot[] = "*top\n[lyshader id=paint file=paint.hlsl]\n"
    SHADER_SCENE SHADER_BINDINGS "[flip]\n[caption data=ready]\n[stop]\n";
static const char shader_sequence_boot[] = "*top\n[lyshader id=paint file=paint.hlsl]\n"
    "[lyc id=fx.base width=4 height=4 color=ff112233]\n"
    "[lyc id=fx.top width=4 height=2 color=ff114466]\n"
    "[lyprop id=fx.top top=2]\n"
    "[lyprop id=fx shader=paint shaderconstant=\"tint,param\" tint=\"0,1,0.5,1\" "
    "param=1 mask=mask shadertexture=textureUser textureUser=user]\n"
    "[flip]\n[caption data=ready]\n[wait time=1 input=0]\n"
    "[lyprop id=fx param=0]\n[flip]\n[caption data=ready]\n[wait time=1 input=0]\n"
    "[lyprop id=fx.top top=0]\n[flip]\n[caption data=ready]\n[stop]\n";
static const char glsl_boot[] = "*top\n[lyshader id=paint file=paint.Frag]\n"
    SHADER_SCENE SHADER_BINDINGS "[flip]\n[caption data=ready]\n[stop]\n";
static const char shader_ini[] = "[WINDOWS]\nWIDTH=4\nHEIGHT=4\nBOOT=boot.iet\n";
#define HLSL_RESOURCES \
    "Texture2D textureFore : register(t2); Texture2D textureMask : register(t3); " \
    "Texture2D textureUser : register(t0); SamplerState sam : register(s0); " \
    "cbuffer Params : register(b0) { float4 tint; float param; }; "
#define HLSL_BODY \
    "float4 fore = textureFore.Sample(sam, uv) * tint; " \
    "return float4(fore.rgb * textureMask.Sample(sam, uv).r + textureUser.Sample(sam, uv).rgb * param, fore.a); }"
static const char shader[] = HLSL_RESOURCES
    "float4 main(float4 position : SV_Position, float2 uv : TEXCOORD0) : SV_Target { " HLSL_BODY;
static const char shader_sequence_source[] = HLSL_RESOURCES
    "float4 main(float2 uv : TEXCOORD0) : SV_Target { " HLSL_BODY;
#ifdef ARTEMIS_DXVK_HOST_SMOKE
static int shader_host_sprites;
static int shader_host_legacy_alpha;
static int shader_host_mixed;
static const char shader_host_sprites_boot[] = "*top\n[lyshader id=paint file=paint.hlsl]\n"
    "[lyc id=1 width=2 height=4 color=ff112233]\n"
    SHADER_BINDINGS
    "[lyc id=2 width=2 height=4 color=ff114466]\n"
    "[lyprop id=2 left=2 shader=paint shaderconstant=\"tint,param\" tint=\"0,1,0.5,1\" "
    "param=0 mask=mask shadertexture=textureUser textureUser=user]\n"
    "[lyc id=3 width=1 height=1 color=ffabcdef]\n"
    "[flip]\n[caption data=ready]\n[stop]\n";
static const char shader_host_legacy_alpha_boot[] = "*top\n[lyshader id=paint file=paint.hlsl]\n"
    "[lyc id=0 width=4 height=4 color=ff204060]\n"
    "[lyc id=fx.child width=2 height=2 color=80ff0000]\n"
    "[lyprop id=fx shader=paint]\n"
    "[lyc id=overlay width=2 height=2 color=80ff0000]\n"
    "[lyprop id=overlay left=2 shader=paint]\n"
    "[flip]\n[caption data=ready]\n[stop]\n";
static const char shader_host_legacy_alpha_source[] =
    "texture textureFore; sampler samplerFore = sampler_state { Texture = <textureFore>; };\n"
    "float alpha;\n"
    "void ps(float2 uv : TEXCOORD0, out float4 color : COLOR0) {\n"
    "    float4 fore = tex2D(samplerFore, uv);\n"
    "    color = float4(1.0 - fore.rgb, fore.a * alpha);\n"
    "}\n";
static const char shader_host_mixed_boot[] = "*top\n[lyshader id=paint file=paint.hlsl]\n"
    "[lyshader id=back file=back.frag]\n"
    "[lyc id=1 width=2 height=4 color=ff112233]\n"
    SHADER_BINDINGS
    "[lyc id=2 width=2 height=4 color=ff114466]\n"
    "[lyprop id=2 left=2 shader=paint shaderconstant=\"tint,param\" tint=\"0,1,0.5,1\" "
    "param=0 mask=mask shadertexture=textureUser textureUser=user]\n"
    "[lyc id=3 width=4 height=2 color=ffffffff]\n[lyprop id=3 shader=back]\n"
    "[lyc id=4.child width=2 height=2 color=ffffffff]\n"
    "[lyprop id=4.child left=2 top=2]\n[lyprop id=4 shader=back]\n"
    "[lyc id=5 width=1 height=1 color=ffabcdef]\n"
    "[flip]\n[caption data=ready]\n[stop]\n";
static const char shader_host_mixed_glsl[] =
    "varying mediump vec2 resultCoord0, resultCoord1;\n"
    "uniform sampler2D textureBack, textureFore;\n"
    "void main() {\n"
    "    float a = texture2D(textureFore, resultCoord1).a;\n"
    "    vec3 color = texture2D(textureBack, resultCoord0).rgb + vec3(32.0 / 255.0, 0.0, 0.0);\n"
    "    gl_FragColor = vec4(color * a, a);\n"
    "}\n";
#endif
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
static int shader_sequence;
static uint8_t external_shader[256 * 1024];
static size_t external_shader_len;

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
        asset = shader_sequence ? shader_sequence_boot : shader_frame == HLSL ? shader_boot : shader_frame ? glsl_boot : boot_source;
    if (shader_frame == HLSL && strstr(metadata, "\"key\":\"paint.hlsl\""))
        asset = shader_sequence ? shader_sequence_source : shader;
#ifdef ARTEMIS_DXVK_HOST_SMOKE
    if (shader_host_sprites && strstr(metadata, "\"key\":\"boot.iet\"")) asset = shader_host_sprites_boot;
    if (shader_host_mixed) {
        if (strstr(metadata, "\"key\":\"boot.iet\"")) asset = shader_host_mixed_boot;
        if (strstr(metadata, "\"key\":\"back.frag\"")) asset = shader_host_mixed_glsl;
    }
    if (shader_host_legacy_alpha) {
        if (strstr(metadata, "\"key\":\"boot.iet\"")) asset = shader_host_legacy_alpha_boot;
        if (strstr(metadata, "\"key\":\"paint.hlsl\"")) asset = shader_host_legacy_alpha_source;
    }
#endif
    if (strstr(metadata, "\"key\":\"paint.frag\"")) {
        if (shader_frame == GLSL) asset = glsl;
        if (shader_frame == GLES) asset = gles;
        if (shader_frame == GLES100) asset = gles100;
    }
    ArtBytes payload = asset ? text(asset) : bytes(NULL, 0);
    if (external_shader_len && strstr(metadata, shader_frame == HLSL ? "\"key\":\"paint.hlsl\"" : "\"key\":\"paint.frag\""))
        payload = bytes(external_shader, external_shader_len);
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

enum { SAVED_GLOBALS = 32, SAVED_SYSTEM = 64, CLOSED = 128, SCRIPT_PRINTED = 256, VIDEO_PLAYED = 512 };

static unsigned pump(const ArtRuntimeApiV2 *api, ArtRuntimeHandle rt, uint64_t frame_id) {
    ArtStepInfo step = { .struct_size = sizeof(step), .host_frame_id = frame_id,
                         .monotonic_ns = frame_id * UINT64_C(1000000) };
    ArtStepResult result = { .struct_size = sizeof(result) };
    unsigned observed = 0;
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
            if (domain == ART_DOMAIN_IO && kind == ART_IO_WRITE_ATOMIC) {
                size_t blob_size = (size_t)little(record + 60, 4);
                assert(blob_size != 0 && blob_size == size - 64 - meta_size);
                assert(strstr(metadata, "\"namespace\":\"save\""));
                if (strstr(metadata, "\"key\":\"saveg.dat\"")) observed |= SAVED_GLOBALS;
                else {
                    assert(strstr(metadata, "\"key\":\"system.dat\""));
                    observed |= SAVED_SYSTEM;
                }
                ArtMessage reply = message(domain, kind, epoch, request, "{\"status\":\"ok\"}");
                assert(api->submit(rt, &reply, 1) == ART_OK);
            } else if (domain == ART_DOMAIN_IO) {
                service_io(api, rt, kind, epoch, request, metadata);
            } else if (domain == ART_DOMAIN_RENDER) {
                assert(result.flags & ART_WAIT_RENDER);
                if (kind == ART_RENDER_CAPTURE) {
                    assert(strstr(metadata, "\"render_frame_id\":"));
                    observed |= 4;
                } else {
                    assert(kind == ART_RENDER_SAVE_CAPTURE && strstr(metadata, "\"key\":\"thumb.png\""));
                    observed |= 8;
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
                observed |= 16;
            } else if (domain == ART_DOMAIN_UI && kind == ART_UI_CAPTION) {
                if (strstr(metadata, "\"ready\"")) observed |= 1;
                if (strstr(metadata, "\"advanced\"")) observed |= 2;
            } else if (domain == ART_DOMAIN_MEDIA && kind == ART_MEDIA_VIDEO_COMMAND) {
                static const char field[] = "\"playback_id\":\"";
                const char *id = strstr(metadata, field);
                assert(request == 0 && strstr(metadata, "\"op\":\"play\"") &&
                       strstr(metadata, "\"key\":\"probe.wmv\"") && strstr(metadata, "\"layer\":null"));
                assert(id && !(observed & VIDEO_PLAYED));
                id += sizeof(field) - 1;
                size_t digits = strspn(id, "0123456789");
                assert(digits > 0 && digits < sizeof(video_playback) && id[digits] == '\"');
                memcpy(video_playback, id, digits);
                video_playback[digits] = '\0';
                observed |= VIDEO_PLAYED;
            } else if (domain == ART_DOMAIN_RUNTIME && kind == ART_RUNTIME_CLOSED_EVENT) {
                assert(!strcmp(metadata, "{\"save_status\":\"written\"}"));
                observed |= CLOSED;
            } else if (domain == ART_DOMAIN_DIAGNOSTICS && kind == 2) {
                static const char line[] = "boot\tready\t\xff";
                assert(request == 0 && !strcmp(metadata, "{}"));
                assert(size - 64 - meta_size == sizeof(line) - 1);
                assert(memcmp(record + 64 + meta_size, line, sizeof(line) - 1) == 0);
                assert(!(observed & SCRIPT_PRINTED));
                observed |= SCRIPT_PRINTED;
            }
            offset += size;
        }
        assert(offset == poll.written_bytes);
        if (result.flags & (ART_WAIT_IO | ART_WAIT_UI | ART_WAIT_RENDER)) continue;
        return observed;
    }
    assert(!"Host fixture did not settle");
    return 0;
}

static void check_video_wait(const ArtRuntimeApiV2 *api, const ArtProjectInfo *project) {
    ArtCreateInfo create = { .struct_size = sizeof(create), .host_service_caps = ART_HOST_CAP_IO | ART_HOST_CAP_MEDIA };
    ArtRuntimeHandle runtime = 0;
    assert(api->create(&create, &runtime) == ART_OK);
    boot_source = video_boot;
    assert(api->open_project(runtime, project) == ART_OK);
    assert(pump(api, runtime, 1) == VIDEO_PLAYED);
    assert(strtoull(video_playback, NULL, 10) > INT64_MAX);

    ArtRuntimeState state = { .struct_size = sizeof(state) };
    assert(api->get_state(runtime, &state) == ART_OK);
    uint64_t epoch = state.session_epoch;
    char metadata[96];
    int size = snprintf(metadata, sizeof(metadata),
        "{\"playback_id\":\"%s\",\"status\":\"started\"}", video_playback);
    assert(size > 0 && (size_t)size < sizeof(metadata));
    ArtMessage feedback = message(ART_DOMAIN_MEDIA, ART_MEDIA_PLAYBACK_STATUS, epoch, 0, metadata);
    assert(api->submit(runtime, &feedback, 1) == ART_OK);
    assert(api->submit(runtime, &feedback, 1) == ART_E_STALE);
    assert(pump(api, runtime, 2) == 0);

    size = snprintf(metadata, sizeof(metadata),
        "{\"playback_id\":\"%s\",\"status\":\"ended\"}", video_playback);
    assert(size > 0 && (size_t)size < sizeof(metadata));
    feedback.metadata_utf8 = text(metadata);
    assert(api->submit(runtime, &feedback, 1) == ART_OK);
    assert(api->submit(runtime, &feedback, 1) == ART_E_STALE);
    assert(pump(api, runtime, 3) == 2);
    assert(api->open_project(runtime, project) == ART_OK);
    assert(api->submit(runtime, &feedback, 1) == ART_E_STALE);
    assert(api->destroy(runtime) == ART_OK);
    boot_source = boot;
}

#ifdef ARTEMIS_DXVK_HOST_SMOKE
static int shader_host_smoke(const ArtRuntimeApiV2 *api, const ArtFrameApiV2 *frames,
                             ArtRuntimeHandle rt, const char *directory, const char *artifact) {
    ArtMetalHost *host = NULL;
    if (shader_host_mixed) {
        const char *angle = getenv("ARTEMIS_GL_SMOKE_ANGLE_DIR");
        assert(angle && art3m1s_metal_host_v1_create_mixed(frames, rt, directory, angle, &host) == ART_OK);
    } else {
        assert(art3m1s_metal_host_v1_create_dxvk(frames, rt, directory, &host) == ART_OK);
    }
    FILE *file = fopen(artifact, "rb");
    assert(file && fseek(file, 0, SEEK_END) == 0);
    long length = ftell(file);
    assert(length > 0 && fseek(file, 0, SEEK_SET) == 0);
    uint8_t *ade = malloc((size_t)length);
    assert(ade && fread(ade, 1, (size_t)length, file) == (size_t)length && fclose(file) == 0);
    ArtBytes source = shader_host_legacy_alpha ? text(shader_host_legacy_alpha_source) : text(shader_sequence_source);
    if (shader_host_legacy_alpha && external_shader_len) source = bytes(external_shader, external_shader_len);
    ArtBytes compiled = bytes(ade, (size_t)length);
    if (!shader_host_mixed)
        assert(art3m1s_metal_host_v1_prepare_ade2(host, source, compiled) == ART_OK);

    void *surface = metal_create_pixel_buffer(4, 4);
    assert(surface && art3m1s_metal_host_v1_set_surface(host, metal_pixel_buffer_surface(surface)) == ART_OK);
    const uint8_t expected[][2][4] = {
        {{5, 24, 24, 255}, {5, 41, 37, 255}},
        {{0, 17, 13, 255}, {0, 34, 26, 255}},
        {{0, 34, 26, 255}, {0, 17, 13, 255}},
    };
    const uint8_t overlay[] = {0xab, 0xcd, 0xef, 255};
    const uint8_t cyan_over_bg[] = {16, 160, 176, 255}, bg[] = {32, 64, 96, 255};
    const unsigned frame_count = shader_host_sprites || shader_host_legacy_alpha || shader_host_mixed ? 1 : 3;
    unsigned turn = 0;
    uint64_t after = 0;
    for (uint64_t id = 1; id <= 16 && turn < frame_count; ++id) {
        if (pump(api, rt, id) != 1) continue;
        uint64_t presented = 0;
        assert(art3m1s_metal_host_v1_present(host, after, &presented) == ART_OK);
        assert(presented > after);
        after = presented;
        uint8_t presentation[64], capture[64];
        if (shader_host_mixed) {
            // The first draw compiles GLSL and samples the unshaded HLSL layer.
            const uint8_t fallback[] = {49, 68, 102, 255};
            assert(metal_read_pixel_buffer(surface, presentation, sizeof(presentation)) == 0);
            assert(!memcmp(presentation + 2 * 4, fallback, sizeof(fallback)));
            assert(art3m1s_metal_host_v1_prepare_ade2(host, source, compiled) == ART_OK);
            assert(art3m1s_metal_host_v1_present(host, 0, &presented) == ART_OK);
            assert(presented == after);
        }
        assert(metal_read_pixel_buffer(surface, presentation, sizeof(presentation)) == 0);
        assert(art3m1s_metal_host_v1_capture(host, after, 4, 4, capture, sizeof(capture)) == ART_OK);
        for (size_t pixel = 0; pixel < 16; ++pixel) {
            const uint8_t *color = shader_host_sprites
                ? (pixel == 0 ? overlay : pixel % 4 < 2 ? expected[0][0] : expected[1][1])
                : expected[turn][pixel / 8];
            const uint8_t *actual = presentation + pixel * 4;
            if (shader_host_legacy_alpha) color = pixel / 4 < 2 ? cyan_over_bg : bg;
            uint8_t mixed[4];
            if (shader_host_mixed) {
                const int right = pixel % 4 >= 2;
                memcpy(mixed, right ? expected[1][1] : expected[0][0], sizeof(mixed));
                if (pixel / 4 < 2 || right) mixed[0] += 32;
                color = pixel == 0 ? overlay : mixed;
            }
            const int tolerance = shader_host_legacy_alpha ? 1 : 0;
            if (abs(actual[0] - color[0]) > tolerance || abs(actual[1] - color[1]) > tolerance ||
                abs(actual[2] - color[2]) > tolerance || actual[3] != color[3]) {
                fprintf(stderr, "DXVK Host frame %u pixel %zu: RGBA %u,%u,%u,%u\n",
                        turn, pixel, actual[0], actual[1], actual[2], actual[3]);
                free(ade);
                return 1;
            }
        }
        assert(!memcmp(capture, presentation, sizeof(capture)));
        ++turn;
    }
    assert(turn == frame_count);
    free(ade);
    assert(art3m1s_metal_host_v1_destroy(host) == ART_OK);
    metal_free_pixel_buffer(surface);
    assert(api->destroy(rt) == ART_OK);
    puts(shader_host_legacy_alpha ? "Metal Host DXVK composited a legacy shader over transparent and translucent regions"
         : shader_host_mixed ? "Metal Host drew GLSL on demand and replayed the same frame with late ADE2"
         : shader_host_sprites ? "Metal Host DXVK composited two sprite effects and an overlay in order"
                            : "Metal Host DXVK presentation and capture matched three Core frames");
    return 0;
}
#endif

static void check_api_header(const ArtApiHeader *header, uint32_t id, size_t minimum_size) {
    assert(header->magic == ART_API_MAGIC);
    assert(header->struct_size >= minimum_size);
    assert(header->interface_id == id);
    assert(header->abi_major == ART_ABI_V2_MAJOR);
}

int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--shader-frame")) shader_frame = HLSL;
    else if (argc == 2 && !strcmp(argv[1], "--shader-sequence")) {
        shader_frame = HLSL;
        shader_sequence = 1;
    }
    else if (argc == 2 && !strcmp(argv[1], "--glsl-frame")) shader_frame = GLSL;
    else if (argc == 2 && !strcmp(argv[1], "--gles-frame")) shader_frame = GLES;
    else if (argc == 2 && !strcmp(argv[1], "--gles100-frame")) shader_frame = GLES100;
#ifdef ARTEMIS_DXVK_HOST_SMOKE
    else if (argc == 2 && !strcmp(argv[1], "--shader-host-mixed-source")) {
        return fputs(shader_sequence_source, stdout) < 0;
    }
    else if (argc == 2 && !strcmp(argv[1], "--shader-host-legacy-alpha-source")) {
        return fputs(shader_host_legacy_alpha_source, stdout) < 0;
    }
    else if (argc == 4 && (!strcmp(argv[1], "--shader-host") || !strcmp(argv[1], "--shader-host-sprites") ||
                         !strcmp(argv[1], "--shader-host-legacy-alpha") || !strcmp(argv[1], "--shader-host-mixed"))) {
        shader_frame = HLSL;
        shader_sequence = 1;
        shader_host_sprites = !strcmp(argv[1], "--shader-host-sprites");
        shader_host_legacy_alpha = !strcmp(argv[1], "--shader-host-legacy-alpha");
        shader_host_mixed = !strcmp(argv[1], "--shader-host-mixed");
    }
#endif
    else if (argc != 1) return 2;
    const char *source = shader_frame == GLES100 ? getenv("ARTEMIS_GL_SMOKE_SOURCE") : NULL;
#ifdef ARTEMIS_DXVK_HOST_SMOKE
    if (shader_host_legacy_alpha) source = getenv("ARTEMIS_DXVK_SMOKE_LEGACY_ALPHA_SOURCE");
#endif
    if (source && *source) {
        FILE *shader_file = fopen(source, "rb");
        if (!shader_file) return 2;
        external_shader_len = fread(external_shader, 1, sizeof(external_shader), shader_file);
        int valid = external_shader_len && feof(shader_file) && !ferror(shader_file);
        if (fclose(shader_file) || !valid) return 2;
    }
    const ArtRootApiV2 *root = NULL;
    assert(art3m1s_get_api_v2(ART_ABI_V2_MAJOR, sizeof(ArtApiHeader), &root) == ART_OK);
    assert(root);
    check_api_header(&root->header, 0, sizeof(*root));
    const ArtRootApiV2 *incompatible_root = root;
    assert(art3m1s_get_api_v2(ART_ABI_V2_MAJOR + 1, sizeof(ArtApiHeader), &incompatible_root) == ART_E_UNSUPPORTED);
    assert(!incompatible_root);
    incompatible_root = root;
    assert(art3m1s_get_api_v2(ART_ABI_V2_MAJOR, UINT32_MAX, &incompatible_root) == ART_E_UNSUPPORTED);
    assert(!incompatible_root);
    const void *table = root;
    assert(root->query_interface(99, ART_ABI_V2_MAJOR, 0, &table) == ART_E_UNSUPPORTED);
    assert(!table);
    uint64_t needed = 0;
    assert(root->copy_last_error(NULL, 0, &needed) == ART_E_BUFFER);
    uint8_t error[120];
    assert(needed > 0 && needed < sizeof(error));
    assert(root->copy_last_error(error, sizeof(error), &needed) == ART_OK);
    assert(root->query_interface(ART_INTERFACE_RUNTIME, ART_ABI_V2_MAJOR, sizeof(ArtRuntimeApiV2), &table) == ART_OK);
    const ArtRuntimeApiV2 *api = table;
    assert(api);
    check_api_header(&api->header, ART_INTERFACE_RUNTIME, sizeof(*api));
    assert(root->query_interface(ART_INTERFACE_FRAME, ART_ABI_V2_MAJOR, UINT32_MAX, &table) == ART_E_UNSUPPORTED);
    assert(!table);
    assert(root->query_interface(ART_INTERFACE_FRAME, ART_ABI_V2_MAJOR, sizeof(ArtFrameApiV2), &table) == ART_OK);
    const ArtFrameApiV2 *frames = table;
    assert(frames);
    check_api_header(&frames->header, ART_INTERFACE_FRAME, sizeof(*frames));

    ArtCreateInfo info = { .struct_size = sizeof(info), .host_service_caps = ART_HOST_CAP_IO | ART_HOST_CAP_HTTP,
                           .renderer_caps = ART_RENDERER_CAP_SPRITE | ART_RENDERER_CAP_SCREENSHOT };
    if (shader_frame == HLSL) info.renderer_caps |= ART_RENDERER_CAP_HLSL;
    else if (shader_frame) info.renderer_caps |= ART_RENDERER_CAP_GLSL;
    if (shader_sequence) info.renderer_caps |= ART_RENDERER_CAP_GROUP;
#ifdef ARTEMIS_DXVK_HOST_SMOKE
    if (shader_host_mixed) info.renderer_caps = ART_MIXED_HOST_V1_RENDERER_CAPS;
#endif
    ArtRuntimeHandle rt = 0;
    assert(api->create(&info, &rt) == ART_OK && rt != 0);
    ArtProjectInfo project = { .struct_size = sizeof(project),
                               .ini_bytes = text(shader_frame ? shader_ini : ini),
                               .platform_utf8 = text("windows") };
    assert(api->open_project(rt, &project) == ART_OK);
#ifdef ARTEMIS_DXVK_HOST_SMOKE
    if (argc == 4) return shader_host_smoke(api, frames, rt, argv[2], argv[3]);
#endif
    if (shader_frame) {
        unsigned emitted = 0;
        const unsigned expected = shader_sequence ? 3 : 1;
        uint64_t after = 0;
        for (uint64_t id = 1; id <= 16 && emitted < expected; ++id) {
            if (pump(api, rt, id) != 1) continue;
            ArtFrameView frame = { .struct_size = sizeof(frame) };
            assert(frames->acquire_latest(rt, after, &frame) == ART_OK);
            assert(frame.packet.size >= 56);
            const uint8_t *packet = frame.packet.data;
            uint32_t textures = (uint32_t)little(packet + 12, 4);
            assert(fwrite(packet, 1, (size_t)frame.packet.size, stdout) == frame.packet.size);
            for (uint32_t i = 0; i < textures; ++i) {
                ArtBytes rgba = {0};
                assert(frames->texture_rgba(rt, frame.token, i, &rgba) == ART_OK);
                assert(fwrite(rgba.data, 1, (size_t)rgba.size, stdout) == rgba.size);
            }
            after = frame.render_frame_id;
            assert(frames->release(rt, frame.token) == ART_OK);
            ++emitted;
        }
        assert(emitted == expected);
        assert(api->destroy(rt) == ART_OK && fflush(stdout) == 0);
        return 0;
    }
    ArtPollResult short_poll = { .struct_size = sizeof(short_poll) };
    assert(api->poll_events(rt, NULL, 0, &short_poll) == ART_E_BUFFER);
    assert(short_poll.required_next_bytes > 0 && short_poll.record_count == 0);
    assert(pump(api, rt, 1) == (1 | 4 | 8 | 16 | SCRIPT_PRINTED));

    struct { ArtRuntimeState prefix; uint64_t tail; } state = {
        .prefix.struct_size = sizeof(state), .tail = UINT64_C(0x123456789abcdef0)
    };
    assert(api->get_state(rt, &state.prefix) == ART_OK);
    uint64_t epoch = state.prefix.session_epoch;
    assert(state.prefix.state == ART_RUNTIME_RUNNING && state.prefix.stage_width == 640);
    assert(state.tail == UINT64_C(0x123456789abcdef0));
    ArtStepInfo background = { .struct_size = sizeof(background), .flags = ART_STEP_BACKGROUND_AUTOSAVE };
    ArtStepResult background_result = { .struct_size = sizeof(background_result) };
    assert(api->step(rt, &background, &background_result) == ART_OK);
    assert(background_result.host_frame_id == 1 && background_result.flags == ART_WAIT_SCENARIO);
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
    ArtMessage input[] = {
        keys[0],
        message(ART_DOMAIN_INPUT, ART_INPUT_WINDOW_STATE, epoch, 0, "{\"fullscreen\":true,\"minimized\":true}")
    };
    assert(api->submit(rt, input, 2) == ART_OK);
    assert(pump(api, rt, 3) == 2);
    assert(api->open_project(rt, &project) == ART_OK);
    assert(api->submit(rt, input, 2) == ART_E_STALE);
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
    assert(pump(api, rt, 1) == (1 | 4 | 8 | SCRIPT_PRINTED));
    ArtMessage shutdown = message(ART_DOMAIN_RUNTIME, ART_RUNTIME_SHUTDOWN, little(opened + 24, 8), 0, "{}");
    assert(api->submit(rt, &shutdown, 1) == ART_OK);
    assert(api->get_state(rt, &state.prefix) == ART_OK && state.prefix.state == ART_RUNTIME_CLOSING);
    assert(pump(api, rt, 1) == (SAVED_GLOBALS | SAVED_SYSTEM | CLOSED));
    assert(api->get_state(rt, &state.prefix) == ART_OK && state.prefix.state == ART_RUNTIME_CLOSED);
    assert(api->destroy(rt) == ART_OK);
    check_video_wait(api, &project);
    puts("Artemis v2 Host smoke passed");
    return 0;
}
