# art3m1s-core

`art3m1s-core` 是 Artemis visual novel engine 的 Rust 兼容运行时。它负责解释
ASB/IET 脚本、维护游戏状态和图层树、渲染文本与特效，并通过稳定的 C FFI 向宿主
输出离屏 RGBA 帧。

当前版本为 **0.2.0**。生产宿主是 Flutter 项目
[Art3m1s](https://github.com/Alphaly2K/art3m1s)；core 本身不创建窗口，也不直接
访问用户文件系统或承担音视频解码。

## 功能概览

- ASB/AST/IET 解释器、Lua bridge、标签过滤和队列化控制流
- 图层树、变换、混合、裁切、遮罩、转场、动画和 transform-aware hit-test
- Artemis HLSL shader 子集与 shader texture/pass 支持
- E-Mote PSB 解析、部件合成、动作、表情、口型和眨眼
- 场景文本、Ruby、逐字显示、backlog 和宿主文本翻译注入
- BGM、SE、Voice、全屏视频和图层视频的逻辑状态与完成事件
- 编号存档、系统存档、场景/音频/解释器状态恢复
- Shift_JIS 与 UTF-8 项目、PFS/目录资源和多平台启动分支
- 鼠标、键盘、触摸、hover、右键、拖动和脚本事件处理器
- 面向 Flutter、原生应用和 headless 工具的 C ABI

## 架构

```text
Host application
  ├─ window / frame clock / input
  ├─ PFS, directory and save-file access
  ├─ audio and video decoding
  ├─ dialogs, browser, HTTP and platform services
  └─ translated text and layer-video RGBA frames
                 │ C FFI callbacks
                 ▼
art3m1s-core
  ├─ ffi.rs                 runtime lifecycle and host ABI
  ├─ runtime/               project, control flow, input, save and media state
  ├─ compositor/            scene graph, events, animation and draw-list building
  ├─ backend/gl/            offscreen OpenGL renderer and texture provider
  ├─ text/                  glyph rasterization, Ruby and reveal animation
  ├─ shader/ + transition/  Artemis shader and transition pipeline
  ├─ video/                 logical video state; no decoder
  └─ crates/
      ├─ asb-interpreter/   ASB/AST/IET interpreter and Lua bridge
      ├─ art3m1s-emote/     E-Mote PSB parser and evaluator
      └─ pfs-upk-rust/      PFS reader and streaming FFI
```

The support crates are ordinary directories in this repository. A checkout does not need Git
submodules or a separate interpreter repository.

## Runtime 边界

### 窗口与帧

`CoreRuntime::advance_and_render(delta_ms)` advances the script and subsystems, renders into an
offscreen FBO, then returns RGBA through `glReadPixels`. The host owns the window, display scaling,
frame pacing and presentation. The GL backend saves and restores the host context around rendering.

### 文件与存档

Project and save I/O is provided by callbacks registered in `ffi.rs`. Core only passes logical
paths; the host resolves them against a PFS archive, unpacked directory or application sandbox.

Numbered saves contain local variables, interpreter position/call stack, scene state and audio
state. Persistent `g.*` and `s.*` domains are deliberately excluded, so loading an older slot does
not roll back or erase the current save index. System state is stored separately through
`syssave()` in `saveg.dat` and `system.dat`.

### 音频与视频

Core contains no FFmpeg, mpv or platform decoder. It emits host-media commands and waits for the
corresponding completion callbacks.

Fullscreen video is displayed by the host. For layer video, the host decodes the newest RGBA8 frame
and calls `art3m1s_runtime_upload_video_layer_frame`. The pointer is borrowed only for that call;
core uploads it directly to a dynamic GL texture and composites it as a normal layer. Upload and
render calls for one runtime must be serialized.

### 文本与翻译

Before glyph layout, scenario text is passed to the registered text-injection callback:

- non-negative length: render the returned replacement immediately;
- `-1`: keep the source text;
- `-2`: display the source immediately and enqueue an asynchronous host translation.

The host later calls `art3m1s_runtime_submit_text_translation`. Core only replaces a segment while
it still belongs to the same page and waits for the active reveal animation before changing it, so
a late network response cannot overwrite a newer line.

Ruby remains an ordered `RubyStart(reading) -> ScenarioText(base) -> RubyEnd` event sequence. Only
the base text is replaceable; the reading is preserved as optional translation context and its
layout is recomputed against the translated range.

## ASB/IET 兼容层

The 0.2.0 cycle includes a large Fable-assisted compatibility pass across the interpreter, runtime,
compositor and text system. It expands the implemented Artemis surface for:

- queued tags, jumps/calls/returns, waits, stop/resume and inline event frames;
- text pages, links, backlog, read-state, automode and skip;
- layer creation/editing, event registration, dragging, tween/anime and transitions;
- BGM/SE/Voice control, fades, panning, crossfade and finish handlers;
- save/load, dialogs, browser/HTTP/native requests and platform state callbacks;
- shader, screenshot, video and other system-facing events.

This is compatibility work derived from documented and observed Artemis behavior; it is not a
claim that every proprietary engine revision or game-specific extension is implemented.

## Lua 后端

| Target | Backend | Reason |
|---|---|---|
| iOS | Luau | Avoids the Lua 5.1 `system()` build failure on iOS |
| Other targets | Lua 5.1 | Preserves the weak typing and legacy script behavior used by games |

The interpreter provides the compatibility shims required by supported games. Backend selection is
made through target-specific Cargo features in the root `Cargo.toml`.

## 关键 FFI

| Function | Purpose |
|---|---|
| `art3m1s_runtime_create` / `destroy` | Runtime lifecycle |
| `art3m1s_runtime_load_project` | Load `system.ini` and start a project |
| `art3m1s_runtime_advance_and_render` | Advance one host frame and return RGBA |
| `art3m1s_runtime_feed_mouse` | Update pointer position |
| `art3m1s_runtime_feed_mouse_button` | Send left/right button edges |
| `art3m1s_runtime_feed_touch` | Send touch phases |
| `art3m1s_runtime_feed_key` | Send Windows virtual-key input |
| `art3m1s_runtime_notify_video_finished` | Complete a host video operation |
| `art3m1s_runtime_notify_sound_finished` | Complete a host sound operation |
| `art3m1s_runtime_upload_video_layer_frame` | Upload a borrowed RGBA8 layer-video frame |
| `art3m1s_register_file_reader` / `writer` / `delete` | Register host filesystem callbacks |
| `art3m1s_register_media_command_callback` | Receive host media commands |
| `art3m1s_register_ui_command_callback` | Receive dialogs and platform requests |
| `art3m1s_register_text_inject_callback` | Synchronous patch lookup or async translation request |
| `art3m1s_runtime_submit_text_translation` | Submit an asynchronous translation result |
| `art3m1s_probe_caption` | Headless project-title probe for library import |

See [HOST_INTEGRATION.md](HOST_INTEGRATION.md) for callback payloads, threading rules and host wiring
status.

## 输入模型

Scripts use Windows virtual-key values. Common mappings are mouse left `1`, mouse right `2`, Enter
`13`, Escape `27`, Space `32`, arrows `37..40` and F1-F12 `112..123`.

Each frame, core updates pointer/button edges, performs transformed alpha-aware hit-testing,
dispatches layer events such as `click`, `rollover`, `rollout` and `drag*`, then runs queued script
tags through the normal interpreter path. Modal descendants absorb empty clicks so an open
save/load/config/backlog window does not advance the story behind it.

## 构建

Requirements:

- Rust stable with edition 2024 support
- a usable OpenGL context when the default `gl-backend` feature is enabled
- platform SDK/toolchain for the selected target

```bash
cargo fmt --check
cargo test
cargo build --release
```

The default build includes the GL renderer. Parser/runtime-only users can disable it:

```bash
cargo build --no-default-features
```

For Flutter and iOS packaging, use the build instructions in the host repository. The iOS build
selects Luau automatically; desktop builds select Lua 5.1.

## 状态与限制

- HLSL support targets the shader forms observed in Artemis games such as HENPRI; it is not a
  general DirectX shader compiler.
- E-Mote currently targets the PSB/model variants used by tested games. Some proprietary easing,
  pass/step behavior and external texture formats remain incomplete.
- Host services such as HTTP, native calls, browser opening and vibration are callback-driven and
  only work when the embedding application implements them.
- The RGBA readback API is intentionally simple and portable, but it is not a zero-copy
  presentation path.

Release details are available in [CHANGELOG.md](CHANGELOG.md) and
[RELEASE_NOTES_0.2.0.md](RELEASE_NOTES_0.2.0.md).

## 许可证

[GNU Affero General Public License v3.0](LICENSE)
