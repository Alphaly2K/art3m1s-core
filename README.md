# art3m1s-core

Artemis visual novel engine 的 Rust runtime。它负责解释脚本、维护图层/文本/音视频逻辑状态、离屏渲染 RGBA 帧，并通过 C FFI 交给宿主应用显示和落盘。

当前生产宿主是 Flutter 项目 `https://github.com/Alphaly2K/art3m1s`。旧的窗口示例只保留为实验入口，不再是主要集成路径。

## 当前架构

```text
Flutter app
  CoreBridge / PlayerScreen
    ├─ 输入：鼠标、键盘、滚轮 → FFI
    ├─ 文件：PFS/目录/沙箱存档 → FileProvider callbacks
    ├─ 音频/视频：MediaBridge + audioplayers/media_kit
    ├─ 翻译：对照补丁 / 在线 API → text inject callback
    └─ 显示：RGBA buffer → ui.Image → RawImage

art3m1s-core
  ffi.rs
    ├─ runtime lifecycle / frame API
    ├─ file reader/writer/delete callbacks
    ├─ log callback
    └─ host media command callback

  runtime/
    ├─ project.rs     system.ini、boot、EngineCallbacks wiring
    ├─ script.rs      wait/stop/queued tag 推进
    ├─ input.rs       hit-test、hover/click/drag、setonpush 派发
    ├─ events.rs      Interpreter Event → runtime 子系统
    ├─ media.rs       audio/video state → host media commands
    ├─ save_io.rs     save/load/syssave/sysload/savess/takess
    └─ render.rs      GL context save/restore + offscreen render

  compositor/
    ├─ Scene          图层树与 LayerProps
    ├─ reduce         Event 归约、事件处理器注册
    ├─ hit_test       transform-aware hit-test + alpha threshold
    ├─ anim           lytween/anime 状态
    └─ build          Scene → DrawList

  backend/gl          glow 离屏 renderer / texture provider
  text/               glyph raster + scetween
  save.rs             numbered save snapshot 数据结构
  host_media.rs       Dart-facing media command protocol
  video/              video logical state, no decoder
```

## 边界约定

### Core 不拥有窗口

`CoreRuntime::advance_and_render(delta_ms)` 每帧推进脚本和子系统，然后渲染到离屏 FBO，最后通过 `glReadPixels` 返回 RGBA。Flutter 负责把这段像素解码为 `ui.Image` 并显示。

渲染前后会保存/恢复宿主 GL context，避免 core 抢占 Flutter 的上下文导致黑屏。

### Core 不直接读写项目文件或存档文件

所有文件访问走 `ffi.rs` 注册的宿主 callback：

- `art3m1s_register_file_reader`
- `art3m1s_register_file_writer`
- `art3m1s_register_file_delete`
- `art3m1s_set_save_dir`

Core 只传逻辑相对路径，例如 `savedata/save0001.dat` 或游戏 `SAVEPATH` 规范化后的路径。Flutter 的 `FileProvider` 负责在 PFS、目录资源和 app support 存档目录之间解析。

### 音频和视频由宿主播放

Core 维护音视频逻辑状态、finish handler 和脚本同步点，但不包含 FFmpeg 等解码器。音视频事件会转换成 `host_media.rs` 的 JSON 命令，由宿主媒体层执行。

全屏视频仍由宿主显示，播放完成后宿主通过 `art3m1s_runtime_notify_video_finished` 通知 core 恢复。图层视频由宿主原生解码层把 RGBA8 裸指针同步传给 `art3m1s_runtime_upload_video_layer_frame`；core 不复制或长期保留 CPU 帧，直接更新动态 GL 纹理并按普通图层参与合成。宿主必须保证该指针在调用返回前有效，并串行化同一 runtime 的上传与渲染调用。

### 存档分两层

- 编号存档：`[save file="save0001.dat"]` 写 `SaveData`，保存局部变量、脚本位置、调用栈、scene snapshot、audio snapshot。
- 系统存档：不带 file 的 `[save]` 走 `syssave()`，写 `saveg.dat` 和 `system.dat`，用于 `sys.saveslot`、config、全局进度等持久域。

编号存档 snapshot 不保存 `g.*` 和 `s.*` 持久域，读档也不覆盖当前持久域。`sys.saveslot` 这类 Lua table 由脚本通过 `pluto.persist` 存入 `g.system`，再由 `syssave()` 落盘。

### 文本翻译由宿主注入

剧本文本在送入字形渲染器前调用 `art3m1s_register_text_inject_callback` 注册的宿主回调。回调返回非负字节数时同步替换文本，返回 `-1` 保持原文，返回 `-2` 则立即显示原文并通过 `text_translate` UI 命令排入后台翻译，剧情事件、输入和动画不会等待网络。宿主完成后调用 `art3m1s_runtime_submit_text_translation`：core 会等目标层的逐字显示完成，再热替换仍位于同一页面的文本片段；换页后的迟到结果只进入宿主缓存，不会污染新页面。

Flutter 宿主支持 JSON 字符串映射、`source`/`translation` 条目组成的 JSON 或 JSONL，以及“原文 TAB 译文”的 TSV。对照文件会优先于在线 API，在线结果按项目缓存；后台队列会合并重复文本，并按服务限制为 2 至 4 个并发异步 HTTP 请求。在线服务可选 OpenAI、Anthropic、DeepL、Google 翻译、百度翻译和有道翻译，各服务分别保存 endpoint、模型与凭据。全局设置选择翻译模式和 API 参数，每个项目另有独立启用开关。旧项目缺少该开关字段时默认关闭，第一版在线翻译配置会迁移为 OpenAI Chat Completions 配置。

Ruby 由解释器按 `RubyStart(reading) -> ScenarioText(base) -> RubyEnd` 三个事件渲染。翻译链只替换中间的 base 文本，并把 reading 作为可选上下文交给支持提示词的服务；`RubyStart` / `RubyEnd` 保持原顺序，字形渲染器会基于译文范围重新计算注音位置。

## 关键 FFI

| 函数 | 说明 |
|------|------|
| `art3m1s_runtime_create(w, h, backend)` | 创建离屏 runtime |
| `art3m1s_runtime_load_project(rt, ini, platform)` | 从 system.ini 内容加载项目 |
| `art3m1s_runtime_advance_and_render(rt, delta_ms, out, len)` | 推进一帧并写入 RGBA buffer |
| `art3m1s_runtime_feed_mouse(rt, x, y)` | 更新鼠标坐标 |
| `art3m1s_runtime_feed_mouse_button(rt, button, pressed)` | 更新鼠标按钮 |
| `art3m1s_runtime_feed_key(rt, vk, pressed)` | 更新 Windows VK 键 |
| `art3m1s_runtime_notify_video_finished(rt, id)` | 宿主视频播放完成 |
| `art3m1s_runtime_upload_video_layer_frame(rt, id, w, h, rgba, len)` | 同步上传宿主解码的图层视频 RGBA8 帧 |
| `art3m1s_runtime_notify_sound_finished(rt, id)` | 宿主音频播放完成 |
| `art3m1s_register_text_inject_callback(callback)` | 注册同步文本替换或异步翻译请求回调 |
| `art3m1s_runtime_submit_text_translation(rt, serial, text)` | 回填异步译文；空指针表示使用原文 |
| `art3m1s_runtime_destroy(rt)` | 销毁 runtime |

## 输入模型

脚本使用 Windows VK：

| 输入 | VK |
|------|----|
| 鼠标左键 | `1` |
| 鼠标右键 | `2` |
| Enter | `13` |
| Escape | `27` |
| Space | `32` |
| 方向键 | `37..40` |
| F1-F12 | `112..123` |

Core 每帧处理：

1. 鼠标坐标与按钮 edge。
2. `hit_test_all` 计算 hover/click/drag 命中层。
3. 派发图层事件 `click` / `rollover` / `rollout` / `drag*`。
4. 派发输入事件 `setonpush`，鼠标左键等价于 key `1`。
5. 执行 queued tags，让 Lua handler 通过正常标签管线影响脚本。

## 构建与验证

```bash
cargo fmt
cargo test
cargo build --release
```

Flutter app 使用本地 dylib 时，修改 core 后需要重新复制并重启 app。

## 相关仓库

| 仓库                                                         | 职责                                                          |
|------------------------------------------------------------|-------------------------------------------------------------|
| <s> `https://github.com/Alphaly2K/art3m1s-interpreter`</s> | <s>ASB/AST/IET 解释器、Lua bridge、tag/event 层</s> <br/>主线已并入此仓库 |
| `https://github.com/Alphaly2K/pfs-upk-rust`                | Flutter 宿主、UI、媒体播放、PFS/目录资源、沙箱存档                            |

## 许可证
[AGPLv3.0](LICENSE)
