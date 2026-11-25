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
    int32_t (*input)(uint64_t session, int32_t kind, int32_t x, int32_t y,
                     int32_t code, int32_t value);
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

/* All returned tables remain valid while the library is loaded. */
const Art3m1sSiglusHostV1* art3m1s_siglus_get_host_api_v1(size_t* out_size);

#ifdef __cplusplus
}
#endif

#endif
