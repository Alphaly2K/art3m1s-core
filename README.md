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

Core 维护音视频逻辑状态、finish handler 和脚本同步点，但不在生产路径中解码或输出音频 PCM/视频帧。音视频事件会转换成 `host_media.rs` 的 JSON 命令，由 Flutter `MediaBridge` 执行。

全屏视频会暂停脚本，播放完成后宿主通过 `art3m1s_runtime_notify_video_finished` 通知 core 恢复。图层 video 当前不以 Flutter overlay 实现，代码中保留 TODO。

### 存档分两层

- 编号存档：`[save file="save0001.dat"]` 写 `SaveData`，保存局部变量、脚本位置、调用栈、scene snapshot、audio snapshot。
- 系统存档：不带 file 的 `[save]` 走 `syssave()`，写 `saveg.dat` 和 `system.dat`，用于 `sys.saveslot`、config、全局进度等持久域。

编号存档 snapshot 不保存 `g.*` 和 `s.*` 持久域，读档也不覆盖当前持久域。`sys.saveslot` 这类 Lua table 由脚本通过 `pluto.persist` 存入 `g.system`，再由 `syssave()` 落盘。

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
| `art3m1s_runtime_notify_sound_finished(rt, id)` | 宿主音频播放完成 |
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

| 仓库 | 职责 |
|------|------|
| `https://github.com/Alphaly2K/art3m1s-interpreter` | ASB/AST/IET 解释器、Lua bridge、tag/event 层 |
| `https://github.com/Alphaly2K/pfs-upk-rust` | Flutter 宿主、UI、媒体播放、PFS/目录资源、沙箱存档 |

## 许可证
[AGPLv3.0](LICENSE)
