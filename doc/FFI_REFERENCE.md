# C FFI 参考

本文件对应当前 [`src/ffi/api.rs`](../src/ffi/api.rs)、
[`src/ffi/rfvp_api.rs`](../src/ffi/rfvp_api.rs) 和
[`src/ffi/krkr_api.rs`](../src/ffi/krkr_api.rs) 的三张版本化 ABI。

core 的主要入口是：

| ABI | 查询函数 | 用途 |
|---|---|---|
| `Art3m1sApiV1` | `art3m1s_get_api_v1` | Artemis/Art3m1s runtime、Host 状态、资源、媒体和 UI |
| `Art3m1sRfvpApiV1` | `art3m1s_rfvp_get_api_v1` | RFVP runtime、输入、帧和宿主音频命令 |
| `Art3m1sKrkrApiV1` | `art3m1s_krkr_get_api_v1` | KRKR/Kirikiri C++ runtime 的隔离 facade |

三张表不共享 runtime、资源、事件或 surface 语义，新增能力不能塞进错误的表。依赖库
`pfs_upk` 的 `pfs_*` API 独立存在。先阅读 [Host 接入指南](HOST_INTEGRATION.md)
的线程、生命周期和所有权约定。

## 类型约定

- `CoreRuntime` 是不透明类型。Rust 的 `u32/i32/u64/u8/f32/usize` 分别映射为
  `uint32_t/int32_t/uint64_t/uint8_t/float/size_t`；`c_int` 是 C `int`，
  `c_longlong` 是 C `long long`，不能用 Windows 的 32 位 `long` 代替。
- 使用 C calling convention。整型布尔为 `0` 假、非 `0` 真，不是 C++/Dart `bool` ABI。
- `const char*` 是 UTF-8、NUL 结尾，不能包含内嵌 NUL。`uint8_t* + length` 是字节，
  不要求 NUL；返回长度不包含终止符，输出缓冲也不会自动补 NUL。
- `size_t` 随目标位数变化。容量单位均为字节，只有 stat 的 `out_len` 是 `int64_t` 元素数。

## 完整声明

```c
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CoreRuntime CoreRuntime;
typedef struct HostResources HostResources;
typedef struct HostEvents HostEvents;

typedef HostEvents *(*ArtHostEventsCreateFn)(void);
typedef void (*ArtHostEventsDestroyFn)(HostEvents *events);
typedef void (*ArtHostEventsEnableFn)(HostEvents *events, int enabled);
typedef size_t (*ArtHostEventsNextFn)(HostEvents *events);
typedef size_t (*ArtPollEventsFn)(HostEvents *events, uint8_t *out,
                                  size_t capacity, uint32_t *out_count);
typedef int (*ArtSetFontListFn)(HostEvents *events, int monospace, int vertical,
                                const uint8_t *names, size_t names_len);
typedef void (*ArtSetWindowStateFn)(HostEvents *events, int flags);
typedef int (*ArtSetTextReplacementsFn)(HostEvents *events, const uint8_t *json,
                                        size_t json_len);
typedef void (*ArtSetTextTranslationEnabledFn)(HostEvents *events, int enabled);
typedef void (*ArtClearHostStateFn)(HostEvents *events);
typedef HostResources *(*ArtResourcesCreateFn)(void);
typedef void (*ArtResourcesDestroyFn)(HostResources *resources);
typedef void (*ArtResourcesClearFn)(HostResources *resources);
typedef int (*ArtResourcesMountDirectoryFn)(HostResources *resources, const char *path);
typedef int (*ArtResourcesMountPfsFn)(HostResources *resources, const char *path,
                                      const char *encoding);
typedef int (*ArtResourcesSetSaveDirFn)(HostResources *resources, const char *path);
typedef int (*ArtResourcesSetOverrideFn)(HostResources *resources, const char *path,
                                         const uint8_t *data, size_t len);

typedef CoreRuntime *(*ArtRuntimeCreateFn)(uint32_t width, uint32_t height, int32_t backend);
typedef void (*ArtRuntimeDestroyFn)(CoreRuntime *rt);
typedef int32_t (*ArtRuntimeSetResourcesFn)(CoreRuntime *rt, HostResources *resources);
typedef void (*ArtRuntimeSetRuntimeMediaEnabledFn)(CoreRuntime *rt, int enabled);
typedef int32_t (*ArtRuntimeAdvancePresentFn)(CoreRuntime *rt, uint32_t delta_ms);
typedef int32_t (*ArtRuntimeAdvanceWithoutRenderFn)(CoreRuntime *rt, uint32_t delta_ms);
typedef uint32_t (*ArtRuntimeStageFn)(const CoreRuntime *rt);
typedef int32_t (*ArtRuntimeLoadProjectFn)(CoreRuntime *rt, const char *ini,
                                           const char *platform);
typedef int32_t (*ArtRuntimeLoadProjectBytesFn)(CoreRuntime *rt, const uint8_t *ini,
                                                size_t ini_len, const char *platform);
typedef uint32_t (*ArtRuntimePixelBufferSizeFn)(const CoreRuntime *rt);
typedef uint32_t (*ArtRuntimeAdvanceRenderFn)(CoreRuntime *rt, uint32_t delta_ms,
                                              uint8_t *out_pixels, uint32_t capacity);
typedef int32_t (*ArtRuntimeSetExternalSurfaceFn)(CoreRuntime *rt, int32_t kind,
                                                  void *handle, uint32_t width,
                                                  uint32_t height);
typedef void (*ArtRuntimeClearExternalSurfaceFn)(CoreRuntime *rt);
typedef void (*ArtRuntimeFeedMouseFn)(CoreRuntime *rt, int32_t x, int32_t y);
typedef void (*ArtRuntimeFeedClickFn)(CoreRuntime *rt);
typedef void (*ArtRuntimeFeedMouseButtonFn)(CoreRuntime *rt, uint32_t button,
                                            int32_t pressed);
typedef void (*ArtRuntimeFeedTouchFn)(CoreRuntime *rt, uint32_t id, uint8_t phase,
                                      int32_t x, int32_t y);
typedef void (*ArtRuntimeFeedKeyFn)(CoreRuntime *rt, uint32_t vk, int32_t pressed);
typedef int32_t (*ArtRuntimeSubmitDialogFn)(CoreRuntime *rt, int32_t accepted,
                                            const char *text);
typedef int32_t (*ArtRuntimeSubmitTextTranslationFn)(CoreRuntime *rt, uint64_t serial,
                                                     const char *text);
typedef void (*ArtRuntimeSetReportedOsFn)(CoreRuntime *rt, const char *os);
typedef int32_t (*ArtRuntimeSetEmoteBackendFn)(CoreRuntime *rt, int32_t backend);
typedef int32_t (*ArtRuntimeConfigureSpatialUpscaleFn)(CoreRuntime *rt, float render_scale,
                                                       float sharpness);
typedef int32_t (*ArtRuntimeSetRenderQualityPresetFn)(CoreRuntime *rt, int32_t preset);
typedef void (*ArtRuntimeSetProfilerEnabledFn)(const CoreRuntime *rt, int enabled);
typedef int32_t (*ArtRuntimeProfilerSnapshotFn)(const CoreRuntime *rt, uint8_t *out,
                                                uint32_t capacity);
typedef void (*ArtRuntimeSetVolumeFn)(CoreRuntime *rt, const char *channel, float value);
typedef void (*ArtRuntimeNotifyFinishedFn)(CoreRuntime *rt, const char *id);
typedef void (*ArtRuntimeNotifyLifecycleFn)(CoreRuntime *rt, int state);
typedef int32_t (*ArtRuntimeIsExitRequestedFn)(const CoreRuntime *rt);
typedef uint64_t (*ArtRuntimeBackendCapabilitiesFn)(const CoreRuntime *rt);
typedef int32_t (*ArtRuntimeSubmitHttpResultFn)(CoreRuntime *rt, int status_code,
                                                const uint8_t *body, int body_len);
typedef void (*ArtRuntimeSetStringVariableFn)(CoreRuntime *rt, const char *name,
                                              const char *value);
typedef int32_t (*ArtProbeCaptionFn)(HostResources *resources, const uint8_t *ini,
                                     size_t ini_len, const char *platform,
                                     uint8_t *out, int capacity);
typedef void (*ArtSetAnglePathFn)(const char *directory);
typedef void (*ArtSetDebugFn)(int enabled);
typedef int32_t (*ArtSetFontOverrideFn)(const uint8_t *data, int len);
typedef void (*ArtClearFontOverrideFn)(void);
typedef int32_t (*ArtRuntimeUploadVideoLayerFrameFn)(CoreRuntime *rt, const char *id,
                                                     uint32_t width, uint32_t height,
                                                     const uint8_t *rgba,
                                                     size_t rgba_len);

typedef struct ArtHostEventHeaderV1 {
    uint32_t abi_version;
    uint32_t kind;
    uint64_t sequence;
    uint32_t payload_size;
    uint32_t aux;
} ArtHostEventHeaderV1;

typedef struct Art3m1sApiV1 {
    uint32_t struct_size;
    uint32_t abi_version;
    uint64_t magic;

    ArtHostEventsCreateFn host_events_create;
    ArtHostEventsDestroyFn host_events_destroy;
    ArtHostEventsEnableFn host_events_enable;
    ArtHostEventsNextFn host_events_next;
    ArtPollEventsFn poll_events;
    ArtSetFontListFn set_font_list;
    ArtSetWindowStateFn set_window_state;
    ArtSetTextReplacementsFn set_text_replacements;
    ArtSetTextTranslationEnabledFn set_text_translation_enabled;
    ArtClearHostStateFn clear_host_state;

    ArtResourcesCreateFn resources_create;
    ArtResourcesDestroyFn resources_destroy;
    ArtResourcesClearFn resources_clear;
    ArtResourcesMountDirectoryFn resources_mount_directory;
    ArtResourcesMountPfsFn resources_mount_pfs;
    ArtResourcesSetSaveDirFn resources_set_save_dir;
    ArtResourcesSetOverrideFn resources_set_override;
    ArtResourcesClearFn resources_clear_overrides;

    ArtRuntimeCreateFn runtime_create;
    ArtRuntimeDestroyFn runtime_destroy;
    ArtRuntimeSetResourcesFn runtime_set_resources;
    ArtRuntimeSetRuntimeMediaEnabledFn runtime_set_runtime_media_enabled;
    ArtRuntimeAdvancePresentFn runtime_advance_and_present;
    ArtRuntimeAdvanceWithoutRenderFn runtime_advance_without_render;
    ArtRuntimeStageFn runtime_stage_width;
    ArtRuntimeStageFn runtime_stage_height;
    ArtRuntimeLoadProjectFn runtime_load_project;
    ArtRuntimeLoadProjectBytesFn runtime_load_project_bytes;
    ArtRuntimePixelBufferSizeFn runtime_pixel_buffer_size;
    ArtRuntimeAdvanceRenderFn runtime_advance_and_render;
    ArtRuntimeSetExternalSurfaceFn runtime_set_external_surface;
    ArtRuntimeClearExternalSurfaceFn runtime_clear_external_surface;
    ArtRuntimeFeedMouseFn runtime_feed_mouse;
    ArtRuntimeFeedClickFn runtime_feed_click;
    ArtRuntimeFeedMouseButtonFn runtime_feed_mouse_button;
    ArtRuntimeFeedTouchFn runtime_feed_touch;
    ArtRuntimeFeedKeyFn runtime_feed_key;
    ArtRuntimeSubmitDialogFn runtime_submit_dialog;
    ArtRuntimeSubmitTextTranslationFn runtime_submit_text_translation;
    ArtRuntimeSetReportedOsFn runtime_set_reported_os;
    ArtRuntimeSetEmoteBackendFn runtime_set_emote_backend;
    ArtRuntimeConfigureSpatialUpscaleFn runtime_configure_spatial_upscale;
    ArtRuntimeSetRenderQualityPresetFn runtime_set_render_quality_preset;
    ArtRuntimeSetProfilerEnabledFn runtime_set_profiler_enabled;
    ArtRuntimeProfilerSnapshotFn runtime_profiler_snapshot;
    ArtRuntimeSetVolumeFn runtime_set_volume;
    ArtRuntimeNotifyFinishedFn runtime_notify_video_finished;
    ArtRuntimeNotifyFinishedFn runtime_notify_sound_finished;
    ArtRuntimeNotifyLifecycleFn runtime_notify_lifecycle;
    ArtRuntimeIsExitRequestedFn runtime_is_exit_requested;
    ArtRuntimeBackendCapabilitiesFn runtime_backend_capabilities;
    ArtRuntimeSubmitHttpResultFn runtime_submit_http_result;
    ArtRuntimeSetStringVariableFn runtime_set_string_variable;

    ArtProbeCaptionFn probe_caption;
    ArtSetAnglePathFn set_angle_path;
    ArtSetDebugFn set_debug;
    ArtSetDebugFn set_damage_visualization;
    ArtSetFontOverrideFn set_font_override;
    ArtClearFontOverrideFn clear_font_override;
    ArtRuntimeUploadVideoLayerFrameFn runtime_upload_video_layer_frame;
} Art3m1sApiV1;

/* Primary Artemis facade symbol. Flat migration symbols, if present, are not
 * part of the stable host ABI. */
const Art3m1sApiV1 *art3m1s_get_api_v1(size_t *out_size);

#ifdef __cplusplus
}
#endif
```

## RFVP ABI（v1）

RFVP 有自己的 runtime 句柄、输入事件、音频命令和 surface 调用。它不接收
`CoreRuntime`、`HostResources` 或 `HostEvents`，也不能把字段追加到
`Art3m1sApiV1`。查询入口是：

```c
const Art3m1sRfvpApiV1 *art3m1s_rfvp_get_api_v1(size_t *out_size);
```

核心常量如下：

```c
enum Art3m1sRfvpStatus {
    ART3M1S_RFVP_STATUS_OK = 0,
    ART3M1S_RFVP_STATUS_NO_FRAME = 1,
    ART3M1S_RFVP_STATUS_NO_COMMAND = 2,
    ART3M1S_RFVP_STATUS_INVALID_ARGUMENT = -1,
    ART3M1S_RFVP_STATUS_INVALID_HANDLE = -2,
    ART3M1S_RFVP_STATUS_ENGINE = -3,
    ART3M1S_RFVP_STATUS_UNSUPPORTED = -4,
    ART3M1S_RFVP_STATUS_OUT_OF_MEMORY = -5,
};

#define ART3M1S_RFVP_NLS_SHIFT_JIS 1u
#define ART3M1S_RFVP_NLS_GBK       2u
#define ART3M1S_RFVP_NLS_UTF8      3u

#define ART3M1S_RFVP_INPUT_KEY             1u
#define ART3M1S_RFVP_INPUT_TEXT            2u
#define ART3M1S_RFVP_INPUT_POINTER_MOVE    3u
#define ART3M1S_RFVP_INPUT_POINTER_BUTTON  4u
#define ART3M1S_RFVP_INPUT_WHEEL           5u
#define ART3M1S_RFVP_INPUT_TOUCH           6u
#define ART3M1S_RFVP_INPUT_FOCUS           7u
#define ART3M1S_RFVP_INPUT_QUIT            8u

#define ART3M1S_RFVP_INPUT_PHASE_DOWN   0u
#define ART3M1S_RFVP_INPUT_PHASE_UP     1u
#define ART3M1S_RFVP_INPUT_PHASE_REPEAT 2u
#define ART3M1S_RFVP_INPUT_PHASE_MOVE   3u

#define ART3M1S_RFVP_POINTER_LEFT   (1u << 0)
#define ART3M1S_RFVP_POINTER_RIGHT  (1u << 1)
#define ART3M1S_RFVP_POINTER_MIDDLE (1u << 2)

#define ART3M1S_RFVP_AUDIO_LOAD_ENCODED     1u
#define ART3M1S_RFVP_AUDIO_CREATE_STREAM    2u
#define ART3M1S_RFVP_AUDIO_SUBMIT_I16       3u
#define ART3M1S_RFVP_AUDIO_SUBMIT_F32       4u
#define ART3M1S_RFVP_AUDIO_PLAY             5u
#define ART3M1S_RFVP_AUDIO_STOP             6u
#define ART3M1S_RFVP_AUDIO_PAUSE            7u
#define ART3M1S_RFVP_AUDIO_RESUME           8u
#define ART3M1S_RFVP_AUDIO_SET_PARAMS       9u
#define ART3M1S_RFVP_AUDIO_DESTROY_STREAM  10u
#define ART3M1S_RFVP_AUDIO_MASTER_VOLUME   11u

#define ART3M1S_RFVP_AUDIO_SAMPLE_I16 1u
#define ART3M1S_RFVP_AUDIO_SAMPLE_F32 2u

#define ART3M1S_RFVP_AUDIO_ENCODED_UNKNOWN 0u
#define ART3M1S_RFVP_AUDIO_ENCODED_WAV     1u
#define ART3M1S_RFVP_AUDIO_ENCODED_OGG     2u
#define ART3M1S_RFVP_AUDIO_ENCODED_MP3     3u
#define ART3M1S_RFVP_AUDIO_ENCODED_FLAC    4u
```

输入、音频和函数表布局如下。`runtime_create` 的路径是 UTF-8 pointer + length，
不是 NUL 结尾字符串；`save_root_utf8=NULL, save_root_len=0` 表示没有单独存档根。

```c
typedef struct Art3m1sRfvpInputEventV1 {
    uint32_t struct_size;
    uint32_t kind;
    uint32_t code;
    uint32_t phase;
    int32_t x;
    int32_t y;
    int32_t value;
    uint32_t modifiers;
    uint64_t id;
} Art3m1sRfvpInputEventV1;

typedef struct Art3m1sRfvpAudioCommandV1 {
    uint32_t struct_size;
    uint32_t kind;
    uint32_t stream_id;
    uint32_t sample_format;
    uint32_t encoded_kind;
    uint32_t sample_rate;
    uint32_t channels;
    uint32_t repeat;
    uint32_t fade_ms;
    float volume;
    float pan;
    size_t sample_count;
    const uint8_t *payload;
    size_t payload_size;
    uint64_t reserved[2];
} Art3m1sRfvpAudioCommandV1;

typedef void (*Art3m1sRfvpLogCallbackFn)(uint32_t level, const uint8_t *message,
                                         size_t message_len, void *user_data);

typedef int32_t (*ArtRfvpRuntimeCreateFn)(
    const uint8_t *game_root_utf8, size_t game_root_len,
    const uint8_t *save_root_utf8, size_t save_root_len,
    uint32_t width, uint32_t height, int32_t backend, uint32_t nls,
    uint64_t *out_runtime);
typedef void (*ArtRfvpRuntimeDestroyFn)(uint64_t runtime);
typedef int32_t (*ArtRfvpRuntimeStepFn)(uint64_t runtime, uint32_t delta_ms);
typedef int32_t (*ArtRfvpRuntimeIsExitRequestedFn)(uint64_t runtime);
typedef uint32_t (*ArtRfvpRuntimeStageFn)(uint64_t runtime);
typedef uint64_t (*ArtRfvpRuntimeCapabilitiesFn)(uint64_t runtime);
typedef uint32_t (*ArtRfvpRuntimePixelBufferSizeFn)(uint64_t runtime);
typedef int32_t (*ArtRfvpRuntimeFeedInputFn)(
    uint64_t runtime, const Art3m1sRfvpInputEventV1 *events, size_t event_count);
typedef int32_t (*ArtRfvpRuntimePollAudioCommandFn)(
    uint64_t runtime, Art3m1sRfvpAudioCommandV1 *out_command);
typedef int32_t (*ArtRfvpRuntimeSetExternalSurfaceFn)(
    uint64_t runtime, int32_t kind, void *handle, uint32_t width, uint32_t height);
typedef void (*ArtRfvpRuntimeClearExternalSurfaceFn)(uint64_t runtime);
typedef int32_t (*ArtRfvpRuntimeAdvancePresentFn)(uint64_t runtime, uint32_t delta_ms);
typedef uint32_t (*ArtRfvpRuntimeAdvanceRenderFn)(
    uint64_t runtime, uint32_t delta_ms, uint8_t *out_pixels, uint32_t capacity);
typedef void (*ArtRfvpRuntimeSetLogCallbackFn)(
    Art3m1sRfvpLogCallbackFn callback, void *user_data);
typedef size_t (*ArtRfvpLogNextBytesFn)(void);
typedef size_t (*ArtRfvpPollLogFn)(uint8_t *output, size_t capacity);

typedef struct Art3m1sRfvpApiV1 {
    uint32_t struct_size;
    uint32_t abi_version; /* 1 */
    uint64_t magic;       /* 0x315646524D334152, "RA3MRFV1" */

    ArtRfvpRuntimeCreateFn runtime_create;
    ArtRfvpRuntimeDestroyFn runtime_destroy;
    ArtRfvpRuntimeStepFn runtime_step;
    ArtRfvpRuntimeIsExitRequestedFn runtime_is_exit_requested;
    ArtRfvpRuntimeStageFn runtime_stage_width;
    ArtRfvpRuntimeStageFn runtime_stage_height;
    ArtRfvpRuntimeCapabilitiesFn runtime_capabilities;
    ArtRfvpRuntimePixelBufferSizeFn runtime_pixel_buffer_size;
    ArtRfvpRuntimeFeedInputFn runtime_feed_input;
    ArtRfvpRuntimePollAudioCommandFn runtime_poll_audio_command;
    ArtRfvpRuntimeSetExternalSurfaceFn runtime_set_external_surface;
    ArtRfvpRuntimeClearExternalSurfaceFn runtime_clear_external_surface;
    ArtRfvpRuntimeAdvancePresentFn runtime_advance_and_present;
    ArtRfvpRuntimeAdvanceRenderFn runtime_advance_and_render;
    ArtRfvpRuntimeSetLogCallbackFn runtime_set_log_callback;
    ArtRfvpLogNextBytesFn log_next_bytes;
    ArtRfvpPollLogFn poll_log;
} Art3m1sRfvpApiV1;
```

RFVP 的 `runtime_feed_input` 最多接受 4096 条事件，输入结构体的 `struct_size` 必须
精确匹配。`runtime_poll_audio_command` 返回 `OK` 时 `payload` 和
`Art3m1sRfvpAudioCommandV1` 只在下次 poll 前有效；返回 `NO_COMMAND` 表示队列为空。

RFVP runtime 句柄是代际句柄，不是指针数值。失效、伪造或二次销毁的句柄一律返回
`INVALID_HANDLE`（或返回型函数的 0），core 不会解引用未知数值；facade 调用在句柄
表上串行化，但不替代 Host 的单 owner 线程约定。

日志走拉取式队列，不再依赖反向回调：

- `log_next_bytes()` 返回队首完整记录的字节数（8 字节头 + 消息），队列为空时为 0。
- `poll_log(output, capacity)` 按队首顺序写完整记录并返回实际字节数；缓冲不足时
  保留下一条完整记录。记录头固定小端：`level: u32` + `message_len: u32`，随后是
  UTF-8 消息。队列上限 1024 条、单条消息 16 KiB，溢出丢弃最旧记录。

`runtime_set_log_callback` 是已弃用的直接回调例外，仅为迁移保留；不能安全暴露
native 回调蹦床的宿主（如修改过的 iOS 设备上的 Dart `NativeCallable`）必须保持
未设置状态，改用上面的拉取队列。

## KRKR ABI（v1）

`Art3m1sKrkrApiV1` 由 core 的 public facade 暴露，Host 只能调用
`art3m1s_krkr_get_api_v1`。native shim 内部的
`art3m1s_krkr_native_get_api_v1` 使用整数 runtime handle，仅供 core 加载，不是 Host
接口。公开 facade 的 `uint64_t` runtime 同样是代际句柄，不是指针数值：失效、伪造或
二次销毁的句柄一律返回 `INVALID_HANDLE`（或返回型函数的 0），core 不会解引用未知
数值；Host 不应依赖 handle 数值或 C++ 类型。

```c
typedef struct Art3m1sKrkrRuntime Art3m1sKrkrRuntime;

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

#define ART3M1S_KRKR_PROBE_DATA_XP3             1u
#define ART3M1S_KRKR_PROBE_ROOT_XP3             2u
#define ART3M1S_KRKR_PROBE_STARTUP_TJS          3u
#define ART3M1S_KRKR_PROBE_SYSTEM_INITIALIZE_TJS 4u

#define ART3M1S_KRKR_INPUT_KEY            1u
#define ART3M1S_KRKR_INPUT_TEXT           2u
#define ART3M1S_KRKR_INPUT_POINTER_MOVE   3u
#define ART3M1S_KRKR_INPUT_POINTER_BUTTON 4u
#define ART3M1S_KRKR_INPUT_WHEEL          5u
#define ART3M1S_KRKR_INPUT_FOCUS          6u
#define ART3M1S_KRKR_INPUT_QUIT           7u

#define ART3M1S_KRKR_INPUT_PHASE_DOWN   0u
#define ART3M1S_KRKR_INPUT_PHASE_UP     1u
#define ART3M1S_KRKR_INPUT_PHASE_REPEAT 2u
#define ART3M1S_KRKR_INPUT_PHASE_MOVE   3u

#define ART3M1S_KRKR_POINTER_LEFT   (1u << 0)
#define ART3M1S_KRKR_POINTER_RIGHT  (1u << 1)
#define ART3M1S_KRKR_POINTER_MIDDLE (1u << 2)
#define ART3M1S_KRKR_POINTER_X1     (1u << 3)
#define ART3M1S_KRKR_POINTER_X2     (1u << 4)

#define ART3M1S_KRKR_FRAME_FORMAT_RGBA8 1u

#define ART3M1S_KRKR_AUDIO_CREATE_STREAM  1u
#define ART3M1S_KRKR_AUDIO_SUBMIT_PCM     2u
#define ART3M1S_KRKR_AUDIO_PLAY           3u
#define ART3M1S_KRKR_AUDIO_PAUSE          4u
#define ART3M1S_KRKR_AUDIO_STOP           5u
#define ART3M1S_KRKR_AUDIO_SET_PARAMS     6u
#define ART3M1S_KRKR_AUDIO_DESTROY_STREAM 7u
#define ART3M1S_KRKR_AUDIO_MASTER_VOLUME  8u

#define ART3M1S_KRKR_AUDIO_FORMAT_I16 1u
#define ART3M1S_KRKR_AUDIO_FORMAT_F32 2u
#define ART3M1S_KRKR_AUDIO_FORMAT_I8  3u
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
    const uint8_t *pixels;
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
    uint64_t sample_count;
    float volume;
    float pan;
    const uint8_t *payload;
    size_t payload_size;
    uint64_t reserved[2];
} Art3m1sKrkrAudioCommandV1;

typedef struct Art3m1sKrkrAudioConsumedV1 {
    uint32_t struct_size;
    uint32_t stream_id;
    uint64_t consumed_samples;
    uint64_t generation;
    uint64_t reserved[2];
} Art3m1sKrkrAudioConsumedV1;

typedef int32_t (*ArtKrkrProbeProjectFn)(
    const char *game_root_utf8, Art3m1sKrkrProbeV1 *out_probe);
typedef int32_t (*ArtKrkrRuntimeCreateFn)(
    const char *game_root_utf8, const char *save_root_utf8,
    const Art3m1sKrkrRuntimeConfigV1 *config,
    Art3m1sKrkrRuntime **out_runtime);
typedef void (*ArtKrkrRuntimeDestroyFn)(Art3m1sKrkrRuntime *runtime);
typedef uint32_t (*ArtKrkrRuntimeStageFn)(Art3m1sKrkrRuntime *runtime);
typedef uint32_t (*ArtKrkrRuntimePixelBufferSizeFn)(Art3m1sKrkrRuntime *runtime);
typedef int32_t (*ArtKrkrRuntimePushInputFn)(
    Art3m1sKrkrRuntime *runtime, const Art3m1sKrkrInputEventV1 *events,
    size_t event_count);
typedef int32_t (*ArtKrkrRuntimeTickFn)(Art3m1sKrkrRuntime *runtime);
typedef int32_t (*ArtKrkrRuntimeAcquireFrameFn)(
    Art3m1sKrkrRuntime *runtime, Art3m1sKrkrFrameV1 *out_frame);
typedef int32_t (*ArtKrkrRuntimeReleaseFrameFn)(
    Art3m1sKrkrRuntime *runtime, uint64_t frame_id);
typedef int32_t (*ArtKrkrRuntimePollAudioCommandFn)(
    Art3m1sKrkrRuntime *runtime, Art3m1sKrkrAudioCommandV1 *out_command);
typedef int32_t (*ArtKrkrRuntimeSubmitAudioConsumedFn)(
    Art3m1sKrkrRuntime *runtime, const Art3m1sKrkrAudioConsumedV1 *consumed);
typedef int32_t (*ArtKrkrRuntimeIsExitRequestedFn)(Art3m1sKrkrRuntime *runtime);
typedef int32_t (*ArtKrkrRuntimeSetExternalSurfaceFn)(
    Art3m1sKrkrRuntime *runtime, int32_t kind, void *handle,
    uint32_t width, uint32_t height);

typedef struct Art3m1sKrkrApiV1 {
    uint32_t struct_size;
    uint32_t abi_version; /* 1 */
    uint64_t magic;       /* 0x31564B524D334152, "RA3MKRV1" */

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

const Art3m1sKrkrApiV1 *art3m1s_krkr_get_api_v1(size_t *out_size);
```

KRKR 的 Host 侧语义和生命周期如下：

- `Art3m1sKrkrProbeV1`、`Art3m1sKrkrRuntimeConfigV1`、每个
  `Art3m1sKrkrInputEventV1`、`Art3m1sKrkrFrameV1`、`Art3m1sKrkrAudioCommandV1` 和
  `Art3m1sKrkrAudioConsumedV1` 都必须在调用前设置精确的 `struct_size`；版本不匹配
  返回 `INVALID_ARGUMENT`。
- `art3m1s_krkr_get_api_v1` 返回的 `struct_size` 必须等于当前版本结构体大小，
  `abi_version` 必须为 `1`，`magic` 必须为 `0x31564B524D334152`。native shim 还会
  校验所有必需函数指针；`runtime_set_external_surface` 是可选能力，当前 upstream
  smoke 会返回 `ART3M1S_KRKR_STATUS_UNSUPPORTED`。
- `probe_project` 接收 UTF-8 NUL 结尾的游戏根路径。`preferred_kind` 为
  `DATA_XP3`、`ROOT_XP3`、`STARTUP_TJS` 或 `SYSTEM_INITIALIZE_TJS`；`0` 表示未识别。
  探针只用于发现入口，不执行脚本。
- `runtime_create` 接收 UTF-8 NUL 结尾的 game/save root、精确 `struct_size` 的
  `Art3m1sKrkrRuntimeConfigV1` 和不透明 runtime 输出。当前 upstream smoke 对非空的
  `save_root_utf8` 返回 `UNSUPPORTED`；Host 在 adapter 明确实现存档重定向前应传 `NULL`。
- `runtime_push_input` 的每一个事件的 `struct_size` 必须精确匹配，`events=NULL` 只允许
  在 `event_count=0` 时使用，单次最多 4096 条。
- `runtime_tick` 推进一次 KRKR application iteration，没有 delta 参数。Host 负责
  定时和暂停，不能并发调用同一 runtime。
- `runtime_acquire_frame` 成功时 `format=RGBA8`，`stride` 按字节计，`frame_id` 用于
  配对释放。Core facade 会把原生帧复制到 core 缓冲，但 `pixels` 仍只在本次
  `runtime_release_frame(frame_id)` 前有效；Host 不能保留指针或跨 tick 使用。
- `runtime_poll_audio_command` 每次返回一条命令；`NO_COMMAND` 表示暂时没有新命令。
  `payload` 只在下次 poll 前有效。`sample_count` 是每声道 sample frame 数，
  `SUBMIT_PCM` 的 payload 长度必须为
  `sample_count * channels * bytes_per_sample`。
- 当前 KRKR audio command 的字段语义为：

  | kind | 字段 |
  |---|---|
  | `CREATE_STREAM` | `stream_id,sample_format,sample_rate,channels` |
  | `SUBMIT_PCM` | `stream_id,sample_format,sample_rate,channels,sample_count,payload*` |
  | `PLAY` / `PAUSE` / `STOP` / `DESTROY_STREAM` | `stream_id` |
  | `SET_PARAMS` | `stream_id,volume,pan` |

  `I8/I16/I24/I32/F32` 的每样本字节数分别是 `1/2/3/4/4`。
- `runtime_submit_audio_consumed` 的 `consumed_samples` 是自 stream 创建或最近
  stop/reset 后的绝对 sample frame 数，不是增量；`stream_id` 和 `generation` 必须与
  当前 stream 对应。真实播放、混音和设备输出都在 Host。
- KRKR ABI 没有 Dart 反向回调，也没有把 `CoreRuntime`、`HostResources`、PFS 或
  Artemis media event 复用给 KRKR。宿主必须每次检查 `struct_size`，并对可选函数指针
  做 NULL 检查。

## Siglus ABI（v1）

仅当 core 构建包含 `siglus-engine` 时导出
`const SiglusApiV1 *art3m1s_siglus_get_api_v1(size_t *out_size)`。
表头 `struct_size=u32`、`abi_version=1`、`magic=0x53494731`；
Host 必须校验返回的 `out_size` 与自身结构大小一致，且检查所有函数指针。

| 表字段 | C 参数及语义 |
|---|---|
| `probe_project` | `int32(const char *root_utf8)`：1=Siglus 目录，0=否，负数=错误 |
| `runtime_create` | `int32(const char *root_utf8, int32 backend, uint64_t *out_handle)`：自动从 Gameexe 读取舞台大小；backend 与共用 GPU 选择 ABI 一致 |
| `runtime_destroy` | `void(uint64_t handle)`：须在创建线程调用；重复销毁无效果 |
| `runtime_stage_width/height` | `uint32(uint64_t handle)`：无效句柄返回 0 |
| `runtime_set_external_surface` | `int32(uint64_t handle, int32 kind, void *ptr, uint32 width, uint32 height)`；kind 0 解绑，2=IOSurface，3=Metal texture，4=CAMetalLayer |
| `runtime_tick` | `int32(uint64_t handle, uint32 delta_ms, int32 mode, uint8_t *rgba, size_t capacity, size_t *written)`；mode 0=只推进，1=RGBA 回读，2=present；mode 1 要求完整 `width×height×4` 容量 |
| `runtime_input` | `int32(uint64_t handle, int32 kind, int32 code, int32 x, int32 y, int32 value)`；kind 0=移动，1=鼠标键，2=触摸，3=键盘，4=滚轮；x/y 为舞台像素坐标 |
| `runtime_is_exit_requested` | `int32(uint64_t handle)`：0/1，失效句柄返回 -2 |
| `last_error` | `size_t(uint8_t *buffer, size_t capacity)`：返回当前线程最近错误的 UTF-8 字节数；buffer 可空 |

返回状态：0 成功，-1 参数错误，-2 错误线程/未知句柄，-3 引擎或 GPU 错误。
所有输入内存仅在本次调用期间借用；core 不存储传入的 RGBA 指针。
Siglus VM 非 `Send`：Host 必须在同一线程创建、输入、推进和销毁。当前适配器
不提供音视频/对话框的 Host 拉取协议，尚不能按其它引擎的媒体语义宣称完整接入。

## Host 状态、文件与全局配置

| 回调/函数（省略 `art3m1s_`） | 约定 |
|---|---|
| `set_font_override` / `clear_font_override` | 安装/清除运行时覆盖字体（TTF/OTF 字节，进程级全局，core 复制）；返回 1 成功，0 参数无效或非法字体；变更下一帧生效，不回溯已排版文本 |
| `set_debug` | 全局调试开关；关闭同时清除脏区着色开关，不自动关闭 per-runtime profiler |
| `set_damage_visualization` | 仅 debug 开启时允许启用；Host 调试 UI 关闭时还应关闭 profiler |
| `set_angle_path` | ANGLE 库目录；首次设置生效，需早于创建 runtime |
| `host_events_create` / `host_events_destroy` | 创建/释放独立 host-events 句柄；队列、窗口状态、字体表和文本替换表都归该句柄所有 |
| `host_events_enable` | 启用/停用该句柄；启用时清空队列并成为日志、media、UI 的当前活动路由 |
| `resources_create` / `resources_destroy` | 创建/释放独立资源句柄；runtime 可持有同一句柄 |
| `resources_mount_directory` / `resources_mount_pfs` | 返回 1 成功；重建该句柄的资源索引并替换当前挂载 |
| `resources_set_save_dir` / `resources_set_override` / `resources_clear_overrides` | 设置存档根或资源覆盖；失败返回 0 |
| `resources_clear` | 清空该句柄的挂载、覆盖和存档根 |
| `probe_caption` | 第一个参数是资源句柄；返回 UTF-8 字节数，无 NUL；0 表示未找到/失败/缓冲不足 |

Host 必须调用 `art3m1s_get_api_v1` 并校验 `struct_size`、`abi_version` 和 `magic`；
不匹配时必须拒绝读取函数表。core 不再导出表中的平铺符号。

旧日志、media、UI、字体、窗口和文本注入回调入口已删除。Host 未启用 host events 时，
日志和 UI/media 事件不会交付，字体为空、窗口状态为 false、文本保留原文。文件读写失败；
原生 dialog 会保持等待。媒体事件是正常播放和完成时序的必要接线。

### 无反向回调 host events v1

Host 先调用 `host_events_create()`，再调用
`host_events_enable_v1(events, 1)`。启用后 core 把输出写入该句柄持有的有界队列：

- `host_events_next_v1(events)` 返回队首完整事件的字节数，队列为空时为 0。
- `poll_events_v1(events, ...)` 只写完整事件，返回实际字节数并通过 `out_count` 返回
  事件数；调用后已写事件从队列移除。缓冲区不足时保留下一个完整事件，不会写半条记录。
- 事件按 `sequence` 单调递增。当前版本定义 `1=log`、`2=media`、`3=UI`。
- 24 字节事件头（`version`/`kind`/`sequence`/`payload_len`/`aux`）全部固定小端，
  与 Host 原生字节序无关。
- log 的 `aux` 是 ASCII 级别首字符（`D/I/W/E`），payload 是 UTF-8 消息。
- media/UI 的 payload 是 `{"kind":"...","payload":{...}}` JSON，语义与原回调相同。

`set_font_list_v1(events, ...)` 推送换行分隔的 UTF-8 字体族列表；
`set_window_state_v1(events, ...)` 推送 bit0=全屏、bit1=最小化。文本替换表是 JSON
字符串映射；精确命中时同步替换。在线翻译开启而未命中时，core 保留原文并继续通过 UI
事件下发 `text_translate`，由 `runtime_submit_text_translation` 回填。

句柄本身可以独立创建和释放，但日志、media、UI 的产生点位于进程级 core 代码，因此当前
进程只应向最近启用的一组句柄路由输出。关闭会话时必须先 `host_events_enable(events, 0)`，
再 `host_events_destroy(events)`；销毁后不得继续 poll 或提交状态。

## Runtime 返回值与枚举

下表是 `Art3m1sApiV1` 中以 `runtime_` 开头的函数表字段，表中省略该前缀。

| 函数 | 成功/返回数据 | 失败与注意事项 |
|---|---|---|
| `create` | 非空 runtime 指针 | NULL；尺寸须合理，细节读日志 |
| `destroy` | 无返回 | NULL 无操作；有效指针只能销毁一次 |
| `set_resources` | 1 绑定资源句柄 | 0 runtime 为空；必须在 `load_project` 前绑定 |
| `set_emote_backend` | 1；0=内置、1=Eluna | 返回 0 表示失败/未编入；加载项目前设置；不要传未知值 |
| `load_project` / `load_project_bytes` | **0** 成功 | -1 失败；参数是 INI 内容，不是文件路径 |
| `stage_width` / `stage_height` | 舞台尺寸 | NULL 返回 0 |
| `pixel_buffer_size` | width*height*4 字节 | u32 返回，Host 预先校验尺寸不溢出；NULL=0 |
| `advance_and_render` | 非零为实际写入字节数 | 0 无新帧/参数不足/panic；不是退出指示 |
| `advance_without_render` | 1 成功 | 0 无效/panic；也消费本 tick 输入边沿 |
| `set_external_surface` | 1 成功 | 0 无效/不支持/导入失败，旧绑定可能已清除 |
| `clear_external_surface` | 无返回 | 解绑，不代替 Host 释放原生对象 |
| `advance_and_present` | 1 新帧，0 无变化 | -1 失败，可能已经推进逻辑 |
| `feed_mouse` / `feed_click` / `feed_mouse_button` / `feed_touch` / `feed_key` | 无返回 | 只喂状态；后续 tick 处理输入 |
| `submit_dialog` | 1 接收当前对话框响应 | 0 无挂起对话框/无效；text 可 NULL；accepted=0 取消 |
| `submit_text_translation` | 1 接收请求结果，不保证立即显示 | 0 serial 未登记/无效；text=NULL 表示失败 |
| `set_reported_os` | 无返回；设置 `var system="os"` 的上报机种串（如 "switch"/"ps4"） | NULL/空串清除覆盖，回到项目平台；与 ini 分节选择解耦，不影响加载 |
| `submit_http_result` | 1 完成当前请求 | 0 无挂起请求/无效；status=0 表失败；NULL body/非正长度视为空 |
| `set_string_variable` | 无返回；支持 `result.title` 等路径 | name/value 必须有效 UTF-8；无 RPC 完成语义 |
| `set_volume` | 无返回；value 限制到 [0,1] | channel 为 master/bgm/se/voice；不要传 NaN 或未知名称 |
| `set_runtime_media_enabled` | 切换 runtime 视频解码；1=启用、0=停用并清空 | core 未编入 `ffmpeg` 时该函数指针为 NULL；启用后 `video` 事件不再下发宿主播放命令 |
| `notify_video_finished` / `notify_sound_finished` | 无返回；更新状态并触发对应完成处理 | 只用于当前播放；video id=NULL 全屏，sound id=NULL BGM |
| `upload_video_layer_frame` | 1 同步 CPU RGBA 上传 | 0 参数无效/图层过期/失败；fallback，不是 Darwin 生产路径 |
| `is_exit_requested` | 1 已请求退出，0 未请求 | 不会自动 destroy；NULL=0 |
| `notify_lifecycle` | 无返回 | state 枚举见下表 |
| `set_profiler_enabled` | 无返回 | per-runtime；读取仍与其他调用串行 |
| `profiler_snapshot` | 写入字节数；out=NULL 或 capacity=0 时返回所需容量 | 缓冲小返回负的所需容量；NULL rt=-1；无 NUL；可能要扩容重试 |

返回约定并不统一，特别是 load 的 `0` 才是成功，不能全部按布尔解读。

| 参数 | 值 |
|---|---|
| create.backend | 0=CGL、1=ANGLE OpenGL、2=ANGLE Vulkan、3=ANGLE Metal、4=ANGLE D3D11；未知值落 CGL，不是自动选择 |
| external_surface.kind | 1=ANativeWindow、2=IOSurface、3=MTLTexture、4=CAMetalLayer。这是呈现表面，不是视频帧 |
| mouse button / key | Windows VK；左键=1、右键=2、中键=4、Ctrl=17 |
| touch.phase | 0=down、1=move、2=up；id 为手指跟踪标识 |
| lifecycle.state | 0=退出前、1=后台、2=前台 |

CGL 仅 macOS 可用。ANGLE 创建失败会尝试 CGL，因此 create 成功不证明实际用了 ANGLE；
当前无实际后端查询 ABI，以日志与外部表面调用结果判断。Windows/方向通知是否被脚本使用
取决于游戏注册的处理器，Host 只报告真实事件。

`runtime_configure_spatial_upscale(rt, scale, sharpness)` 原子设置 spatial pass 与
SceneColor 比例，供 Host 实现固定 1.5x、2x 或固定输出分辨率；不支持 spatial 的 backend
应由 Host 根据 `runtime_backend_capabilities` 选择 native fallback。



## 已移出稳定 ABI

旧平铺入口中的 native video import/surface、`video_gl_*`、截图、render/output size、
upscale mode/render scale、HLSL shader 和窗口方向通知目前不再是导出符号，也不在
`Art3m1sApiV1` 中。Host 不得再按旧文档解析这些符号；需要恢复时必须先设计新的版本化
函数和明确的所有权语义，再提升 ABI 版本或在兼容的后续结构中追加字段。

## UI 命令协议

UI 事件是 `kind` 字符串 + JSON 对象；字段名区分大小写。兼容宿主仍可用原回调。
`null` 和缺失不要随意变成空字符串
或 0。未实现的能力按 Host 安全策略拒绝；对需完成通知的能力必须明确结束等待。

| kind | payload 字段 | Host 行为/回应 |
|---|---|---|
| `caption` | `data` 字符串 | 更新标题/元数据 |
| `mouse` | `left,top,hide,autohide` 可空 | 位置为舞台坐标；null 保持原设置；Host 管理系统/软件光标 |
| `openbrowser` | `url` | 按权限打开 URL |
| `statusbar` | `visible` | 状态栏显隐 |
| `vibrate` | `time` | 振动时间（毫秒） |
| `write_clipboard` | `string` | 写剪贴板 |
| `avoid` | `action:"show",file,windowbutton` 或 `action:"hide"` | 显示/撤销紧急回避覆盖层 |
| `file_clear_cache` | `{}` | 清 Host 资源缓存，不删除存档 |
| `file_wasm_sync` | `url,baseurl,list` | Web 文件同步请求；其他 Host 可不实现 |
| `dialog_show` | `title,message,hasCancel,textfield,textfieldSize,initialText` | 异步显示原生对话框；用 submit_dialog 回应一次；textfieldSize 可 null |
| `http_request` | `serial,method,url,headers,data,file_data` | 后三项是 `[key,value]` 二元数组列表，不是 JSON map；file_data 的值为资源路径；submit_http_result 完成 |
| `http_cancel` | `serial` | 取消该请求；丢弃旧响应，不再回填它 |
| `callnative` | `result,module,method,param` | result/module/param 可 null；经 set_string_variable 写指定结果变量 |
| `purchase` | `purchase,varname,productid,restore,key,sku,consume` | 可选购买桥接；变量回注，不应未经用户授权执行购买 |
| `exec` | `command` | 只执行 Host 明确允许的命令，不直接交给 shell |
| `shell_execute` | `file,params` | params 是字符串映射；打开文件/应用等，按权限策略处理 |
| `text_translate` | `serial,text,ruby,blocking:false` | 非阻塞队列；ruby 为可空注音上下文；submit_text_translation 回填 |

UI/媒体事件没有 runtime 标识。HTTP/对话框/媒体完成没有统一 request ID 回填保护，详见
接入指南。不能直接从回调同步调用任何 `submit_*`，应返回后在 owner 队列处理。

## 媒体命令协议

权威字段定义位于 [`src/host/media.rs`](../src/host/media.rs)，以下列出当前全部 kind。
`?` 表示字段可能为 JSON null；`loop` 为布尔；`*_ms` 为毫秒。普通路径/ID 均为字符串。

| kind | payload 字段 |
|---|---|
| `audio_set_volume` | `channel,value` |
| `audio_bgm_play` | `file,resolved_file?,loop,gain?,pan?,fade_ms,loop_file?,resolved_loop_file?` |
| `audio_bgm_stop` | `fade_ms` |
| `audio_bgm_fade` | `gain,time_ms` |
| `audio_bgm_pan` | `pan,time_ms` |
| `audio_bgm_crossfade` | `file,resolved_file?,loop,gain?,pan?,time_ms,loop_file?,resolved_loop_file?` |
| `audio_se_play` | `id,file,resolved_file?,loop,gain?,pan?,fade_ms,skippable` |
| `audio_se_stop` | `id,fade_ms` |
| `audio_se_fade` | `id,gain,time_ms` |
| `audio_se_pan` | `id,pan,time_ms` |
| `audio_voice_play` | `id,file,resolved_file?,loop,gain?,pan?,fade_ms` |
| `audio_stop_all` | `{}` |
| `video_play` | `id?,file,resolved_file?,skippable,loop` |
| `video_stop_all` | `{}` |

`audio_set_volume.value` 为 [0,1]，通道为 master/bgm/se/voice。`gain/pan` 是脚本原始整数，
不是播放器百分比；常用 gain 0..1000、pan -1000..1000。当前 Flutter Host 的兼容转换为
gain>1 时除以 1000，否则直接使用；abs(pan)>1 时除以 1000，最后限制在 [-1,1]。
null gain 缺省为 1（淡变更新保留已有值），null pan 缺省居中。最终增益为
master * channel * gain 并限制在 [0,1]；移植时别重复乘通道音量或把 1000 当成百分比。

`video_play.id=null` 是全屏显示，否则参与图层合成。`loop_file` 是 A/B 音乐的循环段，
不应把它忽略后只循环引导段。Core 没有 PCM 拉取 C ABI，也没有在本协议中回报播放器
解码耗时/队列深度的接口；这些指标需由 Host 自己采样。

## Profiler JSON

完整结构见 [`src/profiler/mod.rs`](../src/profiler/mod.rs) 的 `ProfilerSnapshot` /
`ProfileTimings`。
聚合 worker 大约每 500 ms 发布，保留最近 10 秒（最多 4096 样本）。

| 字段 | 含义 |
|---|---|
| `enabled,session_ms,sample_window_ms,sample_count` | 开关、会话时间、实际滚动窗口时间和样本数 |
| `window_ms` | 历史兼容字段，当前等于 session_ms，不是滚动窗口时长 |
| `current` | 最新采样帧各项耗时，单位 ms |
| `average` | 当前滚动窗口各项平均耗时，单位 ms |
| `one_percent` | 各项耗时最慢 1% 样本的平均值，不是 FPS 1% low |
| `maximum` | 旧字段，当前为 one_percent 的别名，不再表示峰值 |
| `tick_hz,rendered_fps` | 窗口内逻辑 tick 频率与实际重绘帧率，静止时两者不同 |
| `damage_percent,current_rendered,draw_calls,vertices,texture_binds,draw_list_commands` | 最新帧状态/计数，不取平均 |
| `rendered_frames,skipped_frames` | 窗口内重绘/跳过帧数 |
| `host_ffi_calls_per_second,host_ffi_mib_per_second` | 宿主文件回调频率与吞吐 |
| `uploaded_mib_per_second,video_uploaded_mib_per_second,video_uploaded_frames_per_second,dynamic_mesh_uploaded_mib_per_second` | 窗口上传吞吐/帧率；不是播放器解码统计 |
| `texture_count,texture_gpu_mib,texture_cpu_mib,emote_layers,emote_source_mib` | 当前资源计数/估算内存，不是进程 RSS |
| `dropped_samples` | 有界采样队列丢弃的样本数 |

耗时字段包括 `ffi_call_ms`、`logic_ms`、`input_ms`、`interpreter_ms`、`events_ms`、
`event_runtime_ms`、`event_media_ms`、`event_text_ms`、`event_transition_ms`、
`event_compositor_ms`、`event_layer_sync_ms`、`event_drain_ms`、`event_log_ms`、
`event_post_ms`、`emote_ms`、`audio_media_ms`、`compositor_ms`、`text_ms`、
`frame_build_ms`、`damage_compute_ms`、`transition_capture_ms`、`texture_upload_ms`、
`video_upload_ms`、`gpu_submit_ms`、`present_ms`、`readback_ms`、`host_ffi_ms`。

这些计时有嵌套：logic 包含解释器和派发，events 又包含其子项，文件回调可能发生在上述
任意阶段，不能全部相加当成一帧总耗时。`ffi_call_ms` 不是“纯跨语言桥接开销”；
`gpu_submit_ms`/`present_ms` 是 CPU 侧提交耗时，不是 GPU timestamp 测量。
Host UI、异步网络、播放器解码和 GPU 的真实执行时间不在这些数字中。
