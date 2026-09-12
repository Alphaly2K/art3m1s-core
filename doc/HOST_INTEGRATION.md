# Host 接入指南

本文面向 Flutter、原生应用及其他嵌入式 Host，以 [`src/ffi_api.rs`](src/ffi_api.rs)
 的版本化 C ABI 为准。
它不是 Artemis 脚本 API 文档，也不要求 Host 使用 Dart 或 libmpv。
完整签名、返回值和命令字段见 [FFI_REFERENCE.md](FFI_REFERENCE.md)。

## 1. 职责与能力边界

| Core | Host |
|---|---|
| 解释脚本、计时器、游戏输入绑定 | 窗口、系统事件、输入坐标转换、帧调度 |
| 图层树、文本/Ruby、E-Mote、转场与合成 | 展示输出帧、创建和管理平台共享表面 |
| 存档序列化、游戏变量与播放状态 | 资源读取、存档落盘、游戏数据隔离 |
| 发送媒体命令、等待播放完成 | 解码、播放、混音、全屏视频、完成通知 |
| 发出对话框、网络、翻译等请求 | 原生 UI、网络权限、翻译服务与异步任务 |

生产入口是 `CoreRuntime` 的版本化 C ABI，不是旧窗口示例。新宿主应只调用
`art3m1s_get_api_v1`，不再逐个解析平铺符号。媒体解码可由 runtime 的 FFmpeg session
承担；真实音频输出和最终 present 仍由 Host 所有。游戏自己的
save/load/config/backlog 通常由脚本绘制，不需要 Host 重写。

跨边界规则：

- 对象与生命周期走不透明句柄，例如 `CoreRuntime*`。句柄内容、Rust trait、C++ vtable
  或平台对象布局都不能直接暴露。
- 数据走裸指针和显式长度，例如像素、INI、字体、替换表、HTTP body 和 uniform block。
  零拷贝数据仍由调用方保证调用期间有效，不由 core 猜测容器布局。
- 只有 `Art3m1sApiV1` 这种定长、版本化、全函数指针的 POD 结构可以按地址跨边界；
  不要新增按值传递的复杂对象结构。

### 构建与加载

- 默认图形 feature 提供 runtime 调用；FFmpeg 视频 session 额外要求 `ffmpeg` feature。
  新宿主只检查 API 表中的函数指针是否为 NULL，不把 feature 差异编码成不同的 ABI 布局。
- `experimental-eluna` 默认编入，仍可通过关闭默认 features 排除；运行时默认使用内置
  E-Mote 后端，Host 显式选择后才启用 Eluna。
- 动态库、ANGLE 和可选媒体库由 Host 打包/加载。媒体库应只加载一份实例，避免重复
  全局状态或 Objective-C 类。构建方式见 [README](README.md#构建)。
- `art3m1s_get_api_v1` 必须返回匹配的 `struct_size`、`abi_version` 和 `magic`；否则
  拒绝读取函数表。可选能力通过函数指针是否为空判断，不再用散装符号探测。
- JSON 的未知字段应忽略；未知命令记录一次诊断。必须回应的已知请求不能静默丢弃。

## 2. 线程、指针与进程级状态

**推荐一个专用 owner 线程串行持有 runtime。** 创建、加载、推进、输入、结果回填、
视频 GL 调用和销毁都在此线程执行。GUI/网络/解码线程只往 owner 队列投递消息。
`*const CoreRuntime` 不等于允许并发读取；FFI 没有通用并发调用保证。
`advance_without_render` 也可能处理纹理加载等 GL 工作，不是任意线程可调用的纯逻辑 API。

文件资源由 Host 通过 `resources_mount_*` 一次性提交到一个 `HostResources` 句柄，
再通过 `runtime_set_resources` 绑定给 runtime；不再注册逐次读取回调，也不依赖进程全局文件表。
host-events 队列、窗口状态、字体表和文本替换表归独立 `HostEvents` 句柄所有；任意 core
线程可写入当前启用句柄，Host 在 owner 线程批量 poll。媒体/UI 事件复制到 Host 队列后
应立即返回。不要等待被当前调用阻塞的 GUI 线程，也不要把语言异常或 panic 抛过 C 边界。

| 数据 | 所有权与有效期 |
|---|---|
| `CoreRuntime*` | Core 分配；不透明；不能制造第二个所有者；仅用 `runtime_destroy` 释放一次 |
| `HostResources*` / `HostEvents*` | Core 分配；不透明；生命周期归创建它的 Host；分别用 `resources_destroy` / `host_events_destroy` 释放一次 |
| 传入字符串/INI/结果/视频帧 | Host 所有；调用期间有效且不可同时修改；调用返回后可释放 |
| 资源挂载路径与覆盖字节 | 调用期间借用；成功挂载后 core 保存索引/副本，Host 可释放原参数 |
| 帧像素、caption、profiler 缓冲 | 调用方提供；只能写容量内，不替换指针、不释放 |
| host-events 记录 | `poll_events_v1(events, ...)` 返回后由 Host 拥有；同一事件不会再次返回 |
| 视频 FBO 名称 | Core 所有，仅在有效 GL lease 中使用；不能删除或跨 context 使用 |
| 外部平台表面 | Host 所有；绑定期间保持强引用，解绑且消费者完成后才释放/复用 |

除显式声明可空的参数外不要传 `NULL`。判空不代表非法指针、短缓冲或重复销毁安全。
部分入口捕获 Rust panic，但这不是内存安全检查或崩溃恢复保证。

### 当前不是完全多实例隔离 ABI

- 文件挂载已经绑定到独立的 `HostResources` 句柄；runtime 不直接碰进程全局资源状态。
- host-events 队列和宿主状态已经绑定到独立 `HostEvents` 句柄；但 core 内部日志、media、
  UI 产生点仍通过“当前启用句柄”路由，因此当前一个进程只应运行一个活动游戏会话。
- `art3m1s_set_angle_path` 使用进程级一次性设置。存档根通过
  `resources_set_save_dir` 在资源句柄内切换；媒体/UI 输出仍由 Host owner 队列消费。
- 调试开关和部分内部快照也有进程级状态。当前建议一个进程只运行一个游戏会话；
  不要一边运行游戏，一边替换全局资源挂载做 caption 探测。

## 3. 资源、编码与存档

先安装资源命名空间，再加载项目。Core 传逻辑路径，Host 根据当前游戏映射到解包目录、
具体 PFS 条目、补丁覆盖或可写应用数据目录。

Host 先通过 `resources_mount_directory` 或 `resources_mount_pfs` 向资源句柄提交资源根。
Core 建立索引后，解释器只使用逻辑路径；Host 不再提供逐次读取回调。

PFS 多卷和补丁覆盖在 core 内按统一索引解析；目录挂载只暴露选中目录。存档写入通过
`resources_set_save_dir` 指定的独立根目录完成，不会回写资源归档。覆盖内容通过
`resources_set_override` 提交，core 复制后参与后续读取。

### 编码与路径

- 优先调用 `runtime_load_project_bytes`，传原始 INI 字节。Core 按所选节的 `CHARSET`
  解码，缺省 Shift_JIS；`load_project` 则要求 UTF-8 字符串。
- `platform` 是 INI 节名，例如 `WINDOWS`、`ANDROID`、`IOS`，不是图形后端，也不必与
  当前 Host OS 一致。没有对应节会失败；舞台尺寸与 BOOT 以该节为准。
- Host 打开 PFS 时也要选择文件名编码。独立 API 见
  [`crates/pfs-upk-rust/src/ffi.rs`](crates/pfs-upk-rust/src/ffi.rs)；不应假定 core 动态库
  一定导出依赖库的 `pfs_*` 符号。
- C 字符串、JSON 和回调路径始终为 UTF-8；文件内容是原始字节，不要统一转码再交给 core。
- 解包模式只访问选中目录，不暗中寻找同名 PFS。PFS 模式绑定具体归档，不只绑定父目录。

### 游戏隔离

INI 的 `SAVEPATH=savedata` 会产生 `savedata/save0001.dat`、`savedata/saveg.dat` 等路径。
Core 已添加逻辑前缀，Host 不要重复添加。将这些路径映射到当前游戏专属的可写根目录，
不能只用常见的 `root.pfs` 文件名作为游戏身份。

编号存档与 `saveg.dat`、`system.dat`、已读记录是不同文件。load 旧编号存档不能回滚
整个可写目录。封面和翻译缓存等 Host 元数据也应由稳定游戏 ID 隔离。

读档会清理旧场景图层及其处理器，但保留游戏启动时注册的全局 `seton*` 事件；
它们由脚本显式 `delon*` 或重新注册管理，不随消息窗重建而注销。

Host 最终负责拒绝目录穿越、越权绝对路径和不允许的写入位置。脚本发起的 `exec`、
`shell_execute`、`callnative`、网络及购买请求不是可信授权，按 Host 权限策略处理。

`probe_caption` 不创建 GL，但会使用传入资源句柄中已挂载的资源运行解释器。它不是无副作用
的文件名扫描器，必须有对应资源上下文且与正在运行的游戏隔离。返回 `0` 包括未探测到、
出错和缓冲不足。

## 4. 启动与帧循环

下面是顺序伪代码。除 `art3m1s_get_api_v1` 外，所有 `*_v1`、`runtime_*` 和 `set_*`
名称都表示 `Art3m1sApiV1` 中的函数表字段；`host_*` 由 Host 实现，不是导出符号：

```text
resources = resources_create()
resources_mount_directory(game_root)            // 或 resources_mount_pfs
resources_set_save_dir(save_root)
events = host_events_create()
host_events_enable_v1(events, 1)
set_font_list_v1(events, ...), set_window_state_v1(events, ...)
set_text_replacements_v1(events, ...)            // 可选
set_font_override(font_bytes, font_len)          // 可选：译文缺字时覆盖运行时字体
set_angle_path(directory)                       // 第一次加载 ANGLE 之前

rt = runtime_create(initial_width, initial_height, gfx_backend)
if rt == NULL: report_error_and_stop()
runtime_set_resources(rt, resources)             // 必须在 load_project 之前
runtime_set_emote_backend(rt, chosen_backend)    // 可选，检查返回值
if runtime_load_project_bytes(rt, ini, ini_len, platform) != 0:
    runtime_destroy(rt)
    report_error_and_stop()
width = runtime_stage_width(rt)
height = runtime_stage_height(rt)
host_allocate_output(width, height)
shared = host_try_attach_external_surface(rt)   // 失败则使用像素路径

on_each_engine_tick:
    host_drain_input_and_async_replies(rt)      // 校验会话 generation
    host_render_latest_layer_video_frames(rt)  // 无新帧不提交
    delta_ms = host_monotonic_elapsed_ms_with_fractional_carry()
    if host_output_busy:
        runtime_advance_without_render(rt, delta_ms)
    else if shared:
        result = runtime_advance_and_present(rt, delta_ms)
        if result == 1: host_mark_frame_available()
        if result < 0: host_detach_and_schedule_pixel_fallback()
    else:
        n = runtime_advance_and_render(rt, delta_ms, pixels, capacity)
        if n > 0: host_present_or_copy_before_reusing_buffer(pixels, n)
    host_drain_events(rt)                        // host-events v1，批量处理
    host_dispatch_queued_media_and_ui_commands()
    if runtime_is_exit_requested(rt): host_begin_shutdown()
```

- 创建尺寸须非零且合理；加载会按 INI 调整舞台，随后查询尺寸与 `pixel_buffer_size`。
  没有独立公开的舞台 resize 接口。
- 每个 tick 只调用一个推进 API，它们都会消费输入边沿、执行脚本与计时器。不能先
  `advance_without_render` 再用同一 delta 调 `advance_and_present`。
- 用单调时钟传经过时间，不是绝对时间。60 Hz 不能始终传 `16`，应携带小数余量；
  显示器刷新率、引擎 tick 频率和视频帧率也不能混为一谈。
- 静止帧返回 `0` 只是没新像素，不表示退出。继续驱动计时器、`onEnterFrame`、
  auto/skip 和媒体完成处理，不要因暂停渲染而停止音频。
- 恢复前台时重置帧时钟基准，别把后台时长作为单帧 delta，也别忙循环补数千帧。
  后台媒体策略由 Host 决定。

## 5. 输出路径

### 通用像素回读

`advance_and_render` 写连续 RGBA8，左上原点，stride 为 `width * 4`，完整帧大小为
`width * height * 4`。返回 `0` 表示无新帧，也可能是参数不足或出错，结合日志区分。
内部局部重绘不意味着此接口只返回脏区像素。

Core 不持有 Host 输出指针。异步图像解码/上传若还在读旧缓冲，应双缓冲或等消费完成，
不能立刻覆写/释放。不要每帧无限创建图像对象或累积待显示帧。
输出为最终 FBO 通道值，接口不做额外 alpha 去预乘或色彩管理；普通游戏通常输出不透明
舞台。透明嵌入时需验证 Host 的 alpha/色彩配置，不要盲目再次乘 alpha。

### 平台共享表面

| kind | Host 传入对象 | 注意 |
|---|---|---|
| `1` | Android `ANativeWindow*` | 不是 Java Surface 对象或 Flutter texture ID |
| `2` | Apple `IOSurfaceRef` | 单平面 BGRA8；不是 `CVPixelBufferRef`，需取其 IOSurface |
| `3` | Apple `MTLTexture` 对象指针 | Metal 直接提交；GL/ANGLE 走 EGLImage 导入 |
| `4` | Apple `CAMetalLayer` | MetalBackend 每帧取 drawable |

需要支持对应扩展的 ANGLE；CGL 不支持此路径。设置返回 `1` 才启用，失败后解绑并降级。
替换失败时旧绑定也可能已被清除。建议先用舞台同尺寸表面，再验证缩放、旋转和颜色。

Core 仍在内部持久 FBO 合成，再在 GPU 上提交到外部表面，不是导出内部纹理指针。
Android BufferQueue 目标会复制完整最终图像，内部场景仍可局部重绘；Apple 保留内容的
目标可局部提交。输出朝向已按 kind 处理，不要无条件再翻转。

`advance_and_present == 1` 表示提交新帧，**不等于任意消费者已完成 GPU 访问**。
ABI 不导出跨设备 fence 或释放回调；Host 必须按平台生产/消费同步协议接线，不能无同步
地同时采样和改写同一表面。消费者通知、缓冲池和引用计数归 Host 管。
换表面前先停止提交、解绑，等待消费者结束后释放对象。

共享提交失败时当前 tick 可能已推进脚本，不要拿同一 delta 再推进一次；后续 tick 进入
回退路径。当前也没有独立的“读取已缓存帧”或“强制重绘”C 接口，Host 应保留最后显示帧
直到拿到新的有效像素，不能假定切换后的首次回读一定非零。诊断记录 kind、尺寸、后端
及 EGL 错误。

## 6. 输入和系统事件

鼠标与触摸位置都是舞台坐标，左上为 `(0,0)`。移除 letterbox 偏移，再按显示区域缩放；
窗口逻辑像素、屏幕物理像素和舞台像素不能直接混用。

- 先 `feed_mouse` 更新位置，再发按钮变化。按钮用 Windows VK：左 `1`、右 `2`、中 `4`，
  不是从零起的序号。拖动持续更新位置，抬起/取消/失焦时补齐 release。
- `feed_click` 是旧单次点击入口，不产生完整按下/抬起状态，不能代替拖动。同一个物理
  操作不要既发它又发完整鼠标按钮序列。
- `feed_key` 用 Windows 虚拟键码，不是字符码或本地 scancode，按下/抬起都发；Ctrl 为
  `17`。Host 不要将 Ctrl 硬编码成 skip 开关，也不要猜游戏 Lua 回调名称。
- `feed_touch` 维护触摸点；需要鼠标兼容时由 Host 转换，避免同一次手势重复触发。
  取消按 up 清理该 ID。没有独立滚轮/触摸板手势入口，由 Host 显式选择映射策略。
- 原生对话框、全屏视频及 Host 菜单应隔离输入，防止点击穿透推进游戏。
- 生命周期 `0/1/2` 分别是退出前/后台/前台。退出和后台可能触发自动存档，此时文件
  回调须仍有效；生命周期通知不代替 Host 帧调度和媒体管理。

## 7. 媒体与视频合成

媒体回调字段见参考文档。遵循每通道命令顺序，不把慢初始化、读盘或解码放在回调里。
播放器可后台预热、长期 idle 复用；启动耗时和持续解码开销分别记录。即使画面静止或
视频仅 24 fps，音频与引擎 tick 也要独立运行。

### 完成与循环

- `notify_sound_finished(NULL)` 完成 BGM；非空 ID 完成对应 SE/语音。
- `notify_video_finished(NULL)` 完成全屏视频；非空 ID 完成图层视频并清理其纹理。
- 自然结束、被允许的用户跳过、当前播放真正失败时回报完成。Core 发起的 stop、
  被新播放替换或循环回绕不能重复当作旧播放自然结束通知。
- 命令没有播放 generation，通知只带 ID。Host 要为每次播放生成序号，过滤旧播放器
  迟到事件；ID 相同不代表同一次播放，BGM 空 ID 也需要序号保护。
- `loop_file` 非空时 A 段播一次，随后重复 B 段；A 到 B 不报完成、不重播 A，提前准备
  B 避免空隙。普通 `loop=true` 循环同一文件。淡变由独立媒体时钟驱动。
- ID 是字符串，`"1.80"` 与 `"1.8"` 不等价。`resolved_file` 是解析后的逻辑资源路径，
  不一定是可交给系统播放器的本地文件；Host 仍需提供归档读取或资源流。

### Runtime 视频

启用 `runtime_set_runtime_media_enabled(rt, 1)` 后，`video` 命令由 core 的 FFmpeg
session 在 runtime 侧解码，并作为普通图层纹理参与合成。Host 不再通过 native video
import/surface ABI 交付解码帧；最终显示仍由 Host 的外部表面承担。

### 图层视频：CPU RGBA fallback

`runtime_upload_video_layer_frame` 是只有 CPU RGBA 结果时的同步 fallback：借用连续、
左上原点 RGBA8，至少 `width * height * 4` 字节，无 stride 参数；调用返回后不保留指针。
正常的 runtime FFmpeg 路径不需要 Host 每帧上传。

## 8. 对话框、HTTP 与翻译

异步操作复制 payload 后开始，结果排回 owner 线程。每个任务携带 Host 会话 generation，
销毁/换游戏后丢弃旧结果。

`dialog_show` 需要一次 `submit_dialog`，取消也要回应；关掉 UI 不等于完成等待。
未注册 UI 时 Core 会一直等待，因此无 GUI Host 也应有明确拒绝策略。`textfieldSize`
按 Unicode 字符数而非字节截断；Core 接受空字符串，没有统一非空限制。
回填不带对话框 ID，Host 要拒绝旧窗口响应。

`http_request` 会挂起脚本，成功或失败都要回填；失败/超时/权限拒绝用
`submit_http_result(rt, 0, NULL, 0)`。`http_cancel` 取消并丢弃对应 serial 的结果。
**回填函数没有 serial 参数**，只完成当前挂起请求，Host 必须确认请求仍匹配当前
serial，不能让旧响应完成新请求。

`callnative`/`purchase` 用 `set_string_variable` 回注指定变量或子键；不是带请求 ID
的 RPC，没有通用等待/恢复承诺，不能假定脚本会等异步结果。按权限策略实现或拒绝。
存在 `file_wasm_sync` 命令也不表示当前已有完整 WASM Host。

同步翻译注入仅做内存查询：非负长度表示替换，`-1` 保留原文，`-2` 请求异步翻译。
输出容量目前为 8192 字节，不足时走异步或保留原文，不能截断 UTF-8 或越界。
`-2` 需要 UI 回调，然后收到 `text_translate`，网络不能阻塞引擎。

`text_translate` 带 `serial`、`text`、`ruby`、`blocking:false`。完成调用
`submit_text_translation(serial, text)`，失败传 `NULL`；返回 `1` 只表示接收结果，不
保证立即可见。Core 等逐字动画结束再替换，并丢弃失效页面的视觉回填。Host 可缓存译文，
但不能自行改写 reveal 状态。Ruby 正文进入注入，注音作为上下文，不要重复翻译成两段正文。

游戏脚本指定的字体可能缺少译文字形（缺字渲染为空白）。`set_font_override` 安装一份
覆盖字体（TTF/OTF 字节，Core 内部复制并立即校验，非法数据返回 0 不生效），此后所有
脚本 face 请求都光栅化到该字体；`clear_font_override` 恢复脚本字体。两者是进程级全局
设置，对所有 runtime 生效，可在任意时刻调用，变更在下一帧重解当前字体，不需要重启
游戏。覆盖只作用于之后光栅化的文本（含异步译文热替换），已排版的既有字形不回溯。
字号、描边、行距等排版属性仍由脚本控制，覆盖只替换字形来源。

## 9. 日志、Profiler 与退出

宿主通过当前启用的 `HostEvents` 句柄批量拉取日志。Host 按游戏会话持久化，再给 UI
有界显示缓冲；不要把 overlay 缓冲当完整日志。上传日志前处理路径/脚本文本等隐私，
不记录网络密钥。

Profiler 在线程内异步聚合，约每 500 ms 发布快照，按低频读取 JSON。读取仍遵守 runtime
串行约束；先查询容量，不足时按负返回值扩容重试。字段/计时关系见参考文档。

退出顺序：

1. 停止新输入、计时任务和异步结果，保留回调和资源上下文。
2. owner 线程 `notify_lifecycle(rt, 0)`，让自动保存有机会完成。
3. 停止/取消媒体与网络；在有效 GL lease 内销毁外部视频 renderer，配对 end；
   令会话 generation 失效，丢弃迟到事件。
4. 停止共享提交/消费并解绑，按平台同步等消费者结束后释放表面。
5. owner 线程 `runtime_destroy` 一次，清空指针，不再调用任何 `runtime_*`。
6. 调用 `resources_clear`，然后 `resources_destroy`；runtime 已销毁后不应再使用该句柄。
7. 最后释放像素缓冲和 UI；调用 `host_events_enable_v1(events, 0)` 后停止拉取，
   再调用 `host_events_destroy(events)` 释放句柄。

## 10. 新 Host 验收清单

- 创建/加载失败可清理，重复进出游戏不崩溃，异步任务不访问旧 runtime。
- UTF-8/Shift_JIS、解包/PFS、同名归档存档隔离正确。
- 画面四角标记与输入一致，缩放/旋转无偏移和翻转；hover、拖动 release、失焦正常。
- 静止页面计时器继续；24 fps 视频不把游戏锁到 24 Hz；Host UI 不穿透输入。
- BGM A/B 无缝，SE/语音/视频完成恢复等待，替换播放不触发旧完成。
- 存档即时读取及重启读取、取消/空文本对话框、HTTP 失败/取消均不卡住。
- 翻译迟到/过长/失败不破坏逐字动画或覆盖下一页，Ruby 正常。
- 回读、共享和失败回退分别验证；Profiler 区分 tick 与实际呈现帧率。

## 源码导航

| 主题 | 源码 |
|---|---|
| C ABI/全局回调 | [`src/ffi.rs`](src/ffi.rs) |
| 创建/帧推进/共享提交 | [`src/runtime.rs`](src/runtime.rs) |
| 项目/存档 | [`src/runtime/project.rs`](src/runtime/project.rs)、[`src/runtime/save_io.rs`](src/runtime/save_io.rs) |
| 媒体协议/完成 | [`src/host_media.rs`](src/host_media.rs)、[`src/runtime/media.rs`](src/runtime/media.rs) |
| UI/HTTP/对话框 | [`src/runtime/events.rs`](src/runtime/events.rs)、[`src/runtime/dialog.rs`](src/runtime/dialog.rs) |
| 翻译 | [`src/runtime/text.rs`](src/runtime/text.rs) |
| 表面导入 | [`src/backend/gl/platform.rs`](src/backend/gl/platform.rs) |
| Profiler | [`src/profiler.rs`](src/profiler.rs) |
