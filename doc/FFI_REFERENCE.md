# C FFI 参考

本文件对应当前 [`src/ffi.rs`](src/ffi.rs) 的全部 `art3m1s_*` 导出，不包含依赖库的
`pfs_*` API。先阅读 [Host 接入指南](HOST_INTEGRATION.md) 的线程、生命周期和所有权约定。
以下是 C 声明参考，不是额外的一套实现；修改 ABI 时应同步更新这里。

## 类型约定

- `CoreRuntime` 是不透明类型。Rust 的 `u32/i32/u64/u8/f32/usize` 分别映射为
  `uint32_t/int32_t/uint64_t/uint8_t/float/size_t`；`c_int` 是 C `int`，
  `c_longlong` 是 C `long long`，不能用 Windows 的 32 位 `long` 代替。
- 使用 C calling convention。整型布尔为 `0` 假、非 `0` 真，不是 C++/Dart `bool` ABI。
- `const char*` 是 UTF-8、NUL 结尾，不能包含内嵌 NUL。`uint8_t* + length` 是字节，
  不要求 NUL；返回长度不包含终止符，输出缓冲也不会自动补 NUL。
- 所有 callback 指针必须非空；别把“可不注册”理解为“可传空指针注销”。
- `size_t` 随目标位数变化。容量单位均为字节，只有 stat 的 `out_len` 是 `int64_t` 元素数。

## 完整声明

回调 typedef 名称为本文定义的助记名；符号名、参数顺序和 ABI 类型与源码对应。

```c
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CoreRuntime CoreRuntime;
typedef void (*ArtLogCallback)(const char *level, const char *message);
typedef void (*ArtJsonCallback)(const char *kind, const char *payload_json);
typedef int (*ArtFileReader)(const char *path, uint8_t *buf, int capacity, long long offset);
typedef int (*ArtFileWriter)(const char *path, const uint8_t *buf, int len);
typedef int (*ArtFileDelete)(const char *path);
typedef int (*ArtFileStat)(const char *path, int64_t *out_components, int out_len);
typedef int (*ArtFontQuery)(int monospace, int vertical, uint8_t *buf, int capacity);
typedef int (*ArtWindowQuery)(void);
typedef int (*ArtTextInject)(const char *text, uint8_t *buf, int capacity);

void art3m1s_register_log_callback(ArtLogCallback cb);
void art3m1s_register_media_command_callback(ArtJsonCallback cb);
void art3m1s_register_ui_command_callback(ArtJsonCallback cb);
void art3m1s_register_file_reader(ArtFileReader cb);
void art3m1s_register_file_writer(ArtFileWriter cb);
void art3m1s_register_file_delete(ArtFileDelete cb);
void art3m1s_register_file_stat(ArtFileStat cb);
void art3m1s_register_font_query(ArtFontQuery cb);
void art3m1s_register_window_state_query(ArtWindowQuery cb);
void art3m1s_register_text_inject_callback(ArtTextInject cb);
int art3m1s_set_font_override(const uint8_t *data, int len);
void art3m1s_clear_font_override(void);
void art3m1s_set_debug(int enabled);
void art3m1s_set_damage_visualization(int enabled);
void art3m1s_set_angle_path(const char *directory);
void art3m1s_set_save_dir(const char *directory);
int art3m1s_file_exists(const char *path);
int art3m1s_copy_file(const char *src, const char *dst);
int art3m1s_delete_file(const char *path);
int art3m1s_probe_caption(const uint8_t *ini, size_t ini_len, const char *platform,
                        uint8_t *out, int capacity);

/* The following symbols require the gl-backend feature. */
CoreRuntime *art3m1s_runtime_create(uint32_t width, uint32_t height, int32_t backend);
void art3m1s_runtime_destroy(CoreRuntime *rt);
int art3m1s_runtime_set_emote_backend(CoreRuntime *rt, int32_t backend);
int32_t art3m1s_runtime_load_project(CoreRuntime *rt, const char *ini, const char *platform);
int32_t art3m1s_runtime_load_project_bytes(CoreRuntime *rt, const uint8_t *ini,
                                         size_t ini_len, const char *platform);
uint32_t art3m1s_runtime_stage_width(const CoreRuntime *rt);
uint32_t art3m1s_runtime_stage_height(const CoreRuntime *rt);
uint32_t art3m1s_runtime_pixel_buffer_size(const CoreRuntime *rt);
uint32_t art3m1s_runtime_advance_and_render(CoreRuntime *rt, uint32_t delta_ms,
                                          uint8_t *out_pixels, uint32_t capacity);
int32_t art3m1s_runtime_advance_without_render(CoreRuntime *rt, uint32_t delta_ms);
int32_t art3m1s_runtime_set_external_surface(CoreRuntime *rt, int32_t kind, void *handle,
                                            uint32_t width, uint32_t height);
void art3m1s_runtime_clear_external_surface(CoreRuntime *rt);
int32_t art3m1s_runtime_advance_and_present(CoreRuntime *rt, uint32_t delta_ms);
void art3m1s_runtime_feed_mouse(CoreRuntime *rt, int32_t x, int32_t y);
void art3m1s_runtime_feed_click(CoreRuntime *rt);
void art3m1s_runtime_feed_mouse_button(CoreRuntime *rt, uint32_t button, int32_t pressed);
void art3m1s_runtime_feed_touch(CoreRuntime *rt, uint32_t id, uint8_t phase,
                               int32_t x, int32_t y);
void art3m1s_runtime_feed_key(CoreRuntime *rt, uint32_t vk, int32_t pressed);
int32_t art3m1s_runtime_submit_dialog(CoreRuntime *rt, int32_t accepted, const char *text);
int32_t art3m1s_runtime_submit_text_translation(CoreRuntime *rt, uint64_t serial,
                                               const char *text);
void art3m1s_runtime_set_reported_os(CoreRuntime *rt, const char *os);
int art3m1s_runtime_submit_http_result(CoreRuntime *rt, int status_code,
                                     const uint8_t *body, int body_len);
void art3m1s_runtime_set_string_variable(CoreRuntime *rt, const char *name, const char *value);
void art3m1s_runtime_set_volume(CoreRuntime *rt, const char *channel, float value);
void art3m1s_runtime_notify_video_finished(CoreRuntime *rt, const char *id);
void art3m1s_runtime_notify_sound_finished(CoreRuntime *rt, const char *id);
void *art3m1s_runtime_video_gl_get_proc_address(void *ctx, const char *name);
int art3m1s_runtime_video_gl_begin(CoreRuntime *rt);
uint32_t art3m1s_runtime_video_gl_framebuffer(CoreRuntime *rt, const char *id,
                                            uint32_t width, uint32_t height);
int art3m1s_runtime_video_gl_commit(CoreRuntime *rt, const char *id);
void art3m1s_runtime_video_gl_end(CoreRuntime *rt);
int art3m1s_runtime_upload_video_layer_frame(CoreRuntime *rt, const char *id,
                                            uint32_t width, uint32_t height,
                                            const uint8_t *rgba, size_t rgba_len);
int32_t art3m1s_runtime_is_exit_requested(const CoreRuntime *rt);
void art3m1s_runtime_notify_lifecycle(CoreRuntime *rt, int state);
void art3m1s_runtime_notify_window_button(CoreRuntime *rt, int button);
void art3m1s_runtime_notify_direction_changed(CoreRuntime *rt, int direction);
void art3m1s_runtime_set_profiler_enabled(const CoreRuntime *rt, int enabled);
int32_t art3m1s_runtime_profiler_snapshot(const CoreRuntime *rt, uint8_t *out,
                                         uint32_t capacity);

#ifdef __cplusplus
}
#endif
```

## 注册与全局配置

| 回调/函数（省略 `art3m1s_`） | 约定 |
|---|---|
| `register_log_callback` | `level`/`message` 为借用字符串，常用级别 `D/I/W/E`，按字符串处理 |
| `register_media_command_callback` / `register_ui_command_callback` | 借用的 `kind` 和 JSON 对象；复制入队，不在回调里阻塞或回调 runtime |
| `register_file_reader` | offset=-1 查询大小，否则范围读取；负数失败，读取 EOF=0 |
| `register_file_writer` | 返回写入字节数，必须等于 len；负数/短写失败 |
| `register_file_delete` | 成功 0，失败负数 |
| `register_file_stat` | 写 6 个本地时间分量，返回 6；负数失败；Core 目前忽略少于 6 的结果 |
| `register_font_query` | 非零参数分别筛选等宽/竖排；输出 UTF-8 换行分隔字体族，无 NUL，返回字节数；负数无结果；目前容量 16384 |
| `register_window_state_query` | 返回位标志 bit0=全屏、bit1=最小化；未注册视为两者 false |
| `register_text_inject_callback` | 返回替换 UTF-8 字节数，0 可替换为空；-1 保留原文，-2 后台翻译；目前容量 8192 |
| `set_font_override` / `clear_font_override` | 安装/清除运行时覆盖字体（TTF/OTF 字节，进程级全局，core 复制）；返回 1 成功，0 参数无效或非法字体；变更下一帧生效，不回溯已排版文本 |
| `set_debug` | 全局调试开关；关闭同时清除脏区着色开关，不自动关闭 per-runtime profiler |
| `set_damage_visualization` | 仅 debug 开启时允许启用；Host 调试 UI 关闭时还应关闭 profiler |
| `set_angle_path` | ANGLE 库目录；首次设置生效，需早于创建 runtime |
| `set_save_dir` | 历史保留配置，首次设置生效；不代替 Host 的每游戏文件路径映射 |
| `file_exists` | 1 存在，0 不存在/无效；经 reader 查询 |
| `copy_file` / `delete_file` | 0 成功，-1 失败；通过注册的文件回调，不是直接系统调用 |
| `probe_caption` | 返回 UTF-8 字节数，无 NUL；0 表示未找到/失败/缓冲不足；使用当前全局资源回调 |

可选回调未注册时并非所有功能都会自动跳过：字体为空、窗口状态为 false、文本保留原文；
文件读写会失败；原生 dialog 会保持等待。HTTP 未注册 UI 时按失败结果完成；注册了 UI 却
不处理 `http_request` 则可能一直等待。媒体回调是正常播放和完成时序的必要接线。

## Runtime 返回值与枚举

下表函数名省略 `art3m1s_runtime_`。

| 函数 | 成功/返回数据 | 失败与注意事项 |
|---|---|---|
| `create` | 非空 runtime 指针 | NULL；尺寸须合理，细节读日志 |
| `destroy` | 无返回 | NULL 无操作；有效指针只能销毁一次 |
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
| `notify_video_finished` / `notify_sound_finished` | 无返回；更新状态并触发对应完成处理 | 只用于当前播放；video id=NULL 全屏，sound id=NULL BGM |
| `video_gl_get_proc_address` | GL 函数指针，ctx 必须为 runtime | NULL 未解析；使用同一 GL 实现 |
| `video_gl_begin` | 1 获取 GL lease | 0 失败/已持有 lease；不可嵌套 |
| `video_gl_framebuffer` | 非零 GL FBO 名称 | 0 无 lease/图层不播放/失败；width/height 是视频帧尺寸 |
| `video_gl_commit` | 1 标记新帧可用 | 0 无 lease/目标不存在；不是结束播放通知 |
| `video_gl_end` | 无返回，恢复上下文 | 与每次成功 begin 配对 |
| `upload_video_layer_frame` | 1 同步上传成功 | 0 参数无效/图层过期/失败；id 非空，尺寸非零，rgba_len 至少 w*h*4 |
| `is_exit_requested` | 1 已请求退出，0 未请求 | 不会自动 destroy；NULL=0 |
| `notify_lifecycle` / `notify_window_button` / `notify_direction_changed` | 无返回 | 枚举见下表，回调必须仍有效 |
| `set_profiler_enabled` | 无返回 | per-runtime；读取仍与其他调用串行 |
| `profiler_snapshot` | 写入字节数；out=NULL 或 capacity=0 时返回所需容量 | 缓冲小返回负的所需容量；NULL rt=-1；无 NUL；可能要扩容重试 |

返回约定并不统一，特别是 load 的 `0` 才是成功，不能全部按布尔解读。

| 参数 | 值 |
|---|---|
| create.backend | 0=CGL、1=ANGLE OpenGL、2=ANGLE Vulkan、3=ANGLE Metal、4=ANGLE D3D11；未知值落 CGL，不是自动选择 |
| external_surface.kind | 1=ANativeWindow、2=IOSurface、3=MTLTexture/EGLImage |
| mouse button / key | Windows VK；左键=1、右键=2、中键=4、Ctrl=17 |
| touch.phase | 0=down、1=move、2=up；id 为手指跟踪标识 |
| lifecycle.state | 0=退出前、1=后台、2=前台 |
| window_button.button | 0=关闭、1=最大化、2=最小化 |
| direction.direction | 0=纵向、1=横向 Home 右、2=倒置纵向、3=横向 Home 左 |

CGL 仅 macOS 可用。ANGLE 创建失败会尝试 CGL，因此 create 成功不证明实际用了 ANGLE；
当前无实际后端查询 ABI，以日志与外部表面调用结果判断。Windows/方向通知是否被脚本使用
取决于游戏注册的处理器，Host 只报告真实事件。

`art3m1s_runtime_render_width/height` 返回 SceneColor 的 render size，
`art3m1s_runtime_output_width/height` 返回 native presentation output size。
两者可以不同；post-process 设计见 [`doc/POST_PROCESS.md`](POST_PROCESS.md)。
`art3m1s_runtime_set_upscale_mode(rt, mode, sharpness)` 中 `mode=0` 为线性采样，
`mode=1` 请求 backend-native spatial upscaler；`sharpness` 必须在 `[0,1]`。
`art3m1s_runtime_set_render_scale(rt, scale)` 设置 SceneColor 相对 native output 的物理比例，
范围为 `[0.1,1.0]`，且 native backend 的实际 render size 不会低于游戏逻辑舞台尺寸。
只有 Host 提供的 output surface 大于逻辑舞台时，MetalFX 才可能执行真正的超分。
`art3m1s_runtime_configure_spatial_upscale(rt, scale, sharpness)` 原子设置 spatial pass 与
SceneColor 比例，供 Host 实现固定 1.5x、2x 或固定输出分辨率；不支持 spatial 的 backend
应由 Host 根据 capabilities 选择 native fallback。

## Runtime HLSL shader

`art3m1s_runtime_register_hlsl_shader(rt,name,source,len)`、
`art3m1s_runtime_replace_hlsl_shader(rt,name,source,len)`（别名
`art3m1s_runtime_reload_hlsl_shader`）接收 UTF-8 shader
名称及 Artemis fragment HLSL 字节串，成功返回正数逻辑 `ShaderId`，失败返回 `-1`。
替换保留该 ID 并作废 Metal/Vulkan native pipeline cache。删除使用
`art3m1s_runtime_unregister_hlsl_shader(rt,name)`，成功返回 1，未找到或参数无效返回 0。
编译失败不会跨越 C ABI 抛出 panic；错误会包含 shader 名称、阶段、编译器消息及可用的
行/列位置并写入 Core 日志。ABI 详情和支持的 HLSL 子集见
[`doc/SHADER_ABI.md`](SHADER_ABI.md)。

## UI 命令协议

回调是 `kind` 字符串 + JSON 对象；字段名区分大小写。`null` 和缺失不要随意变成空字符串
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

UI/媒体回调没有 runtime 标识。HTTP/对话框/媒体完成没有统一 request ID 回填保护，详见
接入指南。不能直接从回调同步调用任何 `submit_*`，应返回后在 owner 队列处理。

## 媒体命令协议

权威字段定义位于 [`src/host_media.rs`](src/host_media.rs)，以下列出当前全部 kind。
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

完整结构见 [`src/profiler.rs`](src/profiler.rs) 的 `ProfilerSnapshot` / `ProfileTimings`。
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
