#ifndef ART3M1S_SIGLUS_HOST_V1_H
#define ART3M1S_SIGLUS_HOST_V1_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* A session and its handles belong to the thread that opened it. */
typedef struct Art3m1sSiglusRuntimeV1 {
    uint32_t struct_size, abi_version, magic;
    int32_t (*probe_project)(const char* project);
    int32_t (*create)(const char* project, int32_t backend, uint64_t* session);
    void (*destroy)(uint64_t session);
    uint32_t (*stage_width)(uint64_t session);
    uint32_t (*stage_height)(uint64_t session);
    /* kind 0 detaches. Kinds 1–4 match the native surface ABI. */
    int32_t (*set_surface)(uint64_t session, int32_t kind, void* surface,
                           uint32_t width, uint32_t height);
    /* mode 0 advances, 1 copies logical RGBA8, 2 presents to the surface.
     * RGBA8 capacity is in bytes. written may be null. */
    int32_t (*tick)(uint64_t session, uint32_t dt_ms, int32_t mode,
                    uint8_t* rgba, size_t capacity, size_t* written);
    /* kind 0 pointer move, 1 button, 2 touch, 3 key, 4 wheel.
     * code is a button, touch phase or key. value is a pressed flag or wheel delta. */
    int32_t (*input)(uint64_t session, int32_t kind, int32_t code,
                     int32_t x, int32_t y, int32_t value);
    int32_t (*is_exit_requested)(uint64_t session);
    /* Returns the byte length of the UTF-8 error. A null buffer queries size. */
    size_t (*last_error)(uint8_t* output, size_t capacity);
} Art3m1sSiglusRuntimeV1;

typedef struct Art3m1sSiglusRuntimeV2 {
    Art3m1sSiglusRuntimeV1 base;
    int32_t (*create_with_save_dir)(const char* project, const char* save_dir,
                                    int32_t backend, uint64_t* session);
} Art3m1sSiglusRuntimeV2;

typedef struct Art3m1sSiglusAudioV1 {
    uint32_t struct_size, abi_version;
    uint64_t magic;
    uint32_t sample_rate, channels;
    /* Writes frames * channels interleaved samples. */
    int32_t (*render_pcm)(uint64_t session, float* output, size_t frames);
} Art3m1sSiglusAudioV1;

typedef struct Art3m1sSiglusDiagnosticsV1 {
    uint32_t struct_size, abi_version;
    uint64_t magic;
    size_t (*log_next_bytes)(void);
    /* Each record is level:u32, size:u32 and UTF-8 bytes. */
    size_t (*poll_log)(uint8_t* output, size_t capacity);
    int32_t (*set_debug)(uint64_t session, int32_t enabled);
} Art3m1sSiglusDiagnosticsV1;

typedef struct Art3m1sSiglusOutputV1 {
    uint32_t struct_size, abi_version;
    uint64_t magic;
    int32_t (*start)(uint64_t session);
    int32_t (*stop)(uint64_t session);
    int32_t (*suspend)(uint64_t session, int32_t suspended);
    int32_t (*volume)(uint64_t session, float volume);
    int32_t (*initialize_android)(void* vm, void* activity);
} Art3m1sSiglusOutputV1;

typedef struct Art3m1sSiglusHostV1 {
    uint32_t struct_size, abi_version;
    uint64_t magic; /* 0x315648534d334152, "RA3MSHV1" */
    const Art3m1sSiglusRuntimeV2* runtime;
    const Art3m1sSiglusAudioV1* audio;
    const Art3m1sSiglusDiagnosticsV1* diagnostics;
    const Art3m1sSiglusOutputV1* output;
} Art3m1sSiglusHostV1;

/* Meshes are in painter order. Each vertex is x, y, u, v, with pixel positions
 * and normalized UVs. Blend 0 is alpha, 1 add, 2 multiply, 3 screen.
 * The optional clip is x, y, width, height in stage pixels. Clear to opaque black. */
typedef struct Art3m1sSiglusMeshV1 {
    uint64_t texture_id;
    uint32_t blend, clipped;
    float opacity, clip[4], vertices[6][4];
} Art3m1sSiglusMeshV1;

typedef struct Art3m1sSiglusFrameViewV1 {
    uint32_t width, height, mesh_count, texture_count;
    const Art3m1sSiglusMeshV1* meshes;
} Art3m1sSiglusFrameViewV1;

typedef struct Art3m1sSiglusTextureViewV1 {
    uint64_t id;
    uint32_t width, height;
    const uint8_t* rgba;
    size_t rgba_len;
} Art3m1sSiglusTextureViewV1;

typedef struct Art3m1sSiglusFrameApiV1 {
    uint32_t struct_size, abi_version;
    /* A CPU session. Advance with runtime.tick mode 0. save_dir may be null. */
    int32_t (*create_runtime)(const char* project, const char* save_dir, uint64_t* session);
    /* Returns 1 before the first frame. Each lease remains valid across ticks
     * and destruction of the session. Use and release it on the acquiring thread.
     * E-Mote object textures are resolved by the configured presenter at acquire. */
    int32_t (*acquire)(uint64_t session, uint64_t* lease);
    int32_t (*view)(uint64_t lease, Art3m1sSiglusFrameViewV1* view);
    /* The texture ID identifies an immutable straight RGBA8 revision in a session. */
    int32_t (*texture)(uint64_t lease, uint32_t index, Art3m1sSiglusTextureViewV1* view);
    int32_t (*release)(uint64_t lease);
    /* Optional shared Art3m1s renderer. Configure, then use runtime.set_surface
     * and present_latest. Both output functions use the current frame without
     * advancing the VM. A return value of 1 means there was no new output. */
    int32_t (*configure_presenter)(uint64_t session, int32_t backend);
    int32_t (*present_latest)(uint64_t session);
    int32_t (*readback_latest)(uint64_t session, uint8_t* rgba, size_t capacity, size_t* written);
} Art3m1sSiglusFrameApiV1;

typedef struct Art3m1sSiglusOutputClockApiV1 {
    uint32_t struct_size, abi_version;
    /* Starts device output. runtime.tick will leave audio mixing to pump. */
    int32_t (*start)(uint64_t session);
    /* Queue up to 4800 PCM frames at 48 kHz, limited by device buffer space.
     * For another output device, use Art3m1sSiglusAudioV1.render_pcm instead. */
    int32_t (*pump)(uint64_t session, uint32_t frames);
} Art3m1sSiglusOutputClockApiV1;

/* The engine retains user_data once while the project is in use. Workers can
 * hold this reference past runtime.destroy. Release signals the final use.
 * Callbacks must be thread-safe and must not reenter the current session.
 * Namespace 0 selects project assets, 1 selects saves. Asset keys are relative
 * UTF-8 paths with / separators and case-insensitive lookup. Save keys are names.
 * A query result of 1 means absent. Reads can return fewer bytes than requested.
 * A write replaces the entire save atomically. Return 0 for success. */
typedef struct Art3m1sSiglusProjectIoV1 {
    uint32_t struct_size, abi_version;
    void* user_data;
    void (*retain)(void* user_data);
    void (*release)(void* user_data);
    int32_t (*query_size)(void* user_data, uint32_t space, const char* key, uint64_t* size);
    int32_t (*read_range)(void* user_data, uint32_t space, const char* key, uint64_t offset,
                          uint8_t* output, size_t capacity, size_t* written);
    int32_t (*write_save)(void* user_data, const char* key, const uint8_t* bytes, size_t len);
    int32_t (*remove_save)(void* user_data, const char* key);
} Art3m1sSiglusProjectIoV1;

typedef struct Art3m1sSiglusHostV2 {
    uint32_t struct_size, abi_version;
    uint64_t magic; /* 0x325648534d334152, "RA3MSHV2" */
    const Art3m1sSiglusHostV1* base;
    /* Opt in before the first tick. Without this, dialogs are drawn by the VM. */
    int32_t (*enable_dialogs)(uint64_t session);
    /* Sets required to the pending UTF-8 JSON's byte count, or zero.
     * A null or undersized buffer leaves the pending request unchanged.
     * Fields: id, kind (0 OK, 1 OK/Cancel, 2 Yes/No, 3 Yes/No/Cancel),
     * title, message, and buttons (ordered {label, value} objects).
     * Reads do not consume the request. */
    int32_t (*read_dialog)(uint64_t session, uint8_t* output, size_t capacity,
                          size_t* required);
    /* A reply must use the pending session, request ID and a listed button value. */
    int32_t (*submit_dialog)(uint64_t session, uint64_t id, int64_t value);
    /* Sends committed UTF-8. Returns 1 if an edit box handled it, 0 if ignored. */
    int32_t (*send_text)(uint64_t session, const uint8_t* text, size_t length);
    /* Optional. Check struct_size before reading this field. */
    const Art3m1sSiglusFrameApiV1* frames;
    /* Optional. Check struct_size before reading this field. */
    const Art3m1sSiglusOutputClockApiV1* output_clock;
    /* Optional. Check struct_size before reading this field. Creates a CPU
     * session from Scene.pck and Gameexe.dat or UTF-8 Gameexe.ini. Supply any
     * project decode options through key.toml. Set append to null for the base. */
    int32_t (*open_project_io)(const Art3m1sSiglusProjectIoV1* io, const char* append,
                               uint64_t* session);
} Art3m1sSiglusHostV2;

/* All returned tables remain valid while the library is loaded. */
const Art3m1sSiglusHostV1* art3m1s_siglus_get_host_api_v1(size_t* out_size);
const Art3m1sSiglusHostV2* art3m1s_siglus_get_host_api_v2(size_t* out_size);

typedef struct Art3m1sSiglusTranslationText {
    const uint8_t* data;
    size_t size;
} Art3m1sSiglusTranslationText;

typedef struct Art3m1sSiglusTranslationEntry {
    Art3m1sSiglusTranslationText source, target;
} Art3m1sSiglusTranslationEntry;

/* The optional table uses the session's owning thread. All strings are UTF-8.
 * configure copies up to 262144 entries containing up to 64 MiB of data.
 * Individual strings are limited to 16 KiB. Reconfiguring releases pending text.
 * poll returns 1 for no request or -4 for insufficient capacity and reports the
 * required size. A successful poll transfers the request to the caller.
 * complete accepts NULL to fall back to the original and returns 1 for stale replies.
 * Unanswered requests fall back to the original after three seconds of session time. */
typedef struct Art3m1sSiglusTranslationV1 {
    uint32_t abi_version, struct_size;
    int32_t (*configure)(uint64_t session, uint32_t online,
                         const Art3m1sSiglusTranslationEntry* entries, size_t count);
    int32_t (*poll)(uint64_t session, uint64_t* id, uint8_t* output,
                    size_t capacity, size_t* written);
    int32_t (*complete)(uint64_t session, uint64_t id, const Art3m1sSiglusTranslationText* reply);
} Art3m1sSiglusTranslationV1;

const Art3m1sSiglusTranslationV1* art3m1s_siglus_get_translation_api_v1(void);

#ifdef __cplusplus
}
#endif

#endif
