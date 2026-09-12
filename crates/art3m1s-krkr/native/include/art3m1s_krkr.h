#pragma once

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#define ART3M1S_KRKR_EXPORT __declspec(dllexport)
#else
#define ART3M1S_KRKR_EXPORT __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

enum Art3m1sKrkrStatus {
    ART3M1S_KRKR_STATUS_OK = 0,
    ART3M1S_KRKR_STATUS_NO_FRAME = 1,
    ART3M1S_KRKR_STATUS_NO_COMMAND = 2,
    ART3M1S_KRKR_STATUS_INVALID_ARGUMENT = -1,
    ART3M1S_KRKR_STATUS_INVALID_HANDLE = -2,
    ART3M1S_KRKR_STATUS_ENGINE = -3,
    ART3M1S_KRKR_STATUS_UNSUPPORTED = -4,
    ART3M1S_KRKR_STATUS_OUT_OF_MEMORY = -5,
};

#define ART3M1S_KRKR_PROBE_DATA_XP3 1u
#define ART3M1S_KRKR_PROBE_ROOT_XP3 2u
#define ART3M1S_KRKR_PROBE_STARTUP_TJS 3u
#define ART3M1S_KRKR_PROBE_SYSTEM_INITIALIZE_TJS 4u

#define ART3M1S_KRKR_INPUT_KEY 1u
#define ART3M1S_KRKR_INPUT_TEXT 2u
#define ART3M1S_KRKR_INPUT_POINTER_MOVE 3u
#define ART3M1S_KRKR_INPUT_POINTER_BUTTON 4u
#define ART3M1S_KRKR_INPUT_WHEEL 5u
#define ART3M1S_KRKR_INPUT_FOCUS 6u
#define ART3M1S_KRKR_INPUT_QUIT 7u

#define ART3M1S_KRKR_INPUT_PHASE_DOWN 0u
#define ART3M1S_KRKR_INPUT_PHASE_UP 1u
#define ART3M1S_KRKR_INPUT_PHASE_REPEAT 2u
#define ART3M1S_KRKR_INPUT_PHASE_MOVE 3u

#define ART3M1S_KRKR_POINTER_LEFT (1u << 0)
#define ART3M1S_KRKR_POINTER_RIGHT (1u << 1)
#define ART3M1S_KRKR_POINTER_MIDDLE (1u << 2)
#define ART3M1S_KRKR_POINTER_X1 (1u << 3)
#define ART3M1S_KRKR_POINTER_X2 (1u << 4)

#define ART3M1S_KRKR_FRAME_FORMAT_RGBA8 1u

#define ART3M1S_KRKR_AUDIO_CREATE_STREAM 1u
#define ART3M1S_KRKR_AUDIO_SUBMIT_PCM 2u
#define ART3M1S_KRKR_AUDIO_PLAY 3u
#define ART3M1S_KRKR_AUDIO_PAUSE 4u
#define ART3M1S_KRKR_AUDIO_STOP 5u
#define ART3M1S_KRKR_AUDIO_SET_PARAMS 6u
#define ART3M1S_KRKR_AUDIO_DESTROY_STREAM 7u
#define ART3M1S_KRKR_AUDIO_MASTER_VOLUME 8u

#define ART3M1S_KRKR_AUDIO_FORMAT_I16 1u
#define ART3M1S_KRKR_AUDIO_FORMAT_F32 2u
#define ART3M1S_KRKR_AUDIO_FORMAT_I8 3u
#define ART3M1S_KRKR_AUDIO_FORMAT_I24 4u
#define ART3M1S_KRKR_AUDIO_FORMAT_I32 5u

typedef struct Art3m1sKrkrProbeV1 {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t preferred_kind;
    uint32_t has_data_xp3;
    uint32_t root_xp3_count;
    uint32_t has_startup_tjs;
    uint32_t has_patch_tjs;
    uint32_t has_system_initialize_tjs;
    uint64_t reserved[4];
} Art3m1sKrkrProbeV1;

typedef struct Art3m1sKrkrRuntimeConfigV1 {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t width;
    uint32_t height;
    uint32_t audio_sample_rate;
    uint32_t audio_channels;
    uint64_t reserved[4];
} Art3m1sKrkrRuntimeConfigV1;

typedef struct Art3m1sKrkrInputEventV1 {
    uint32_t struct_size;
    uint32_t kind;
    uint32_t code;
    uint32_t phase;
    int32_t x;
    int32_t y;
    int32_t value;
    uint32_t modifiers;
    uint64_t id;
} Art3m1sKrkrInputEventV1;

typedef struct Art3m1sKrkrFrameV1 {
    uint32_t struct_size;
    uint32_t format;
    uint32_t width;
    uint32_t height;
    uint32_t stride;
    uint32_t flags;
    uint64_t frame_id;
    uint64_t generation;
    const uint8_t* pixels;
    size_t pixels_len;
    uint64_t reserved[2];
} Art3m1sKrkrFrameV1;

typedef struct Art3m1sKrkrAudioCommandV1 {
    uint32_t struct_size;
    uint32_t kind;
    uint32_t stream_id;
    uint32_t sample_format;
    uint32_t sample_rate;
    uint32_t channels;
    /* Per-channel sample frames for SUBMIT_PCM. */
    uint64_t sample_count;
    float volume;
    float pan;
    const uint8_t* payload;
    size_t payload_size;
    uint64_t reserved[2];
} Art3m1sKrkrAudioCommandV1;

typedef struct Art3m1sKrkrAudioConsumedV1 {
    uint32_t struct_size;
    uint32_t stream_id;
    /* Absolute sample frames consumed since stream creation or stop/reset. */
    uint64_t consumed_samples;
    uint64_t generation;
    uint64_t reserved[2];
} Art3m1sKrkrAudioConsumedV1;

typedef int32_t (*ArtKrkrProbeProjectFn)(const char* game_root_utf8,
                                         Art3m1sKrkrProbeV1* out_probe);
typedef int32_t (*ArtKrkrRuntimeCreateFn)(const char* game_root_utf8,
                                          const char* save_root_utf8,
                                          const Art3m1sKrkrRuntimeConfigV1* config,
                                          uint64_t* out_runtime);
typedef void (*ArtKrkrRuntimeDestroyFn)(uint64_t runtime);
typedef uint32_t (*ArtKrkrRuntimeStageFn)(uint64_t runtime);
typedef uint32_t (*ArtKrkrRuntimePixelBufferSizeFn)(uint64_t runtime);
typedef int32_t (*ArtKrkrRuntimePushInputFn)(uint64_t runtime,
                                             const Art3m1sKrkrInputEventV1* events,
                                             size_t event_count);
typedef int32_t (*ArtKrkrRuntimeTickFn)(uint64_t runtime);
typedef int32_t (*ArtKrkrRuntimeAcquireFrameFn)(uint64_t runtime,
                                                Art3m1sKrkrFrameV1* out_frame);
typedef int32_t (*ArtKrkrRuntimeReleaseFrameFn)(uint64_t runtime, uint64_t frame_id);
typedef int32_t (*ArtKrkrRuntimePollAudioCommandFn)(
    uint64_t runtime,
    Art3m1sKrkrAudioCommandV1* out_command);
typedef int32_t (*ArtKrkrRuntimeSubmitAudioConsumedFn)(
    uint64_t runtime,
    const Art3m1sKrkrAudioConsumedV1* consumed);
typedef int32_t (*ArtKrkrRuntimeIsExitRequestedFn)(uint64_t runtime);
typedef int32_t (*ArtKrkrRuntimeSetExternalSurfaceFn)(uint64_t runtime,
                                                      int32_t kind,
                                                      void* handle,
                                                      uint32_t width,
                                                      uint32_t height);

typedef struct Art3m1sKrkrApiV1 {
    uint32_t struct_size;
    uint32_t abi_version;
    uint64_t magic;

    ArtKrkrProbeProjectFn probe_project;
    ArtKrkrRuntimeCreateFn runtime_create;
    ArtKrkrRuntimeDestroyFn runtime_destroy;
    ArtKrkrRuntimeStageFn runtime_stage_width;
    ArtKrkrRuntimeStageFn runtime_stage_height;
    ArtKrkrRuntimePixelBufferSizeFn runtime_pixel_buffer_size;
    ArtKrkrRuntimePushInputFn runtime_push_input;
    ArtKrkrRuntimeTickFn runtime_tick;
    ArtKrkrRuntimeAcquireFrameFn runtime_acquire_frame;
    ArtKrkrRuntimeReleaseFrameFn runtime_release_frame;
    ArtKrkrRuntimePollAudioCommandFn runtime_poll_audio_command;
    ArtKrkrRuntimeSubmitAudioConsumedFn runtime_submit_audio_consumed;
    ArtKrkrRuntimeIsExitRequestedFn runtime_is_exit_requested;
    ArtKrkrRuntimeSetExternalSurfaceFn runtime_set_external_surface;
} Art3m1sKrkrApiV1;

/* Public entry point exported by the Art3m1s core library. */
ART3M1S_KRKR_EXPORT const Art3m1sKrkrApiV1* art3m1s_krkr_get_api_v1(
    size_t* out_size);

/* Private entry point exported by the native KRKR host shim. */
ART3M1S_KRKR_EXPORT const Art3m1sKrkrApiV1* art3m1s_krkr_native_get_api_v1(
    size_t* out_size);

#ifdef __cplusplus
}
#endif
