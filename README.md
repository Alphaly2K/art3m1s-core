# art3m1s-core

Artemis 视觉小说引擎的 Rust 重写核心库。连接解析后的 Artemis 项目目录与 ASB 脚本解释器，提供跨平台的渲染、音频、视频、文本和存档子系统。

## 架构概览

```
┌─────────────────────────────────────────────────────────────┐
│  Host (Flutter / winit+glutin / CLI)                        │
│  ┌────────┐  ┌─────────────┐  ┌──────────────┐              │
│  │ Input  │  │ GL Context  │  │ File System  │              │
│  │ Events │  │ (ANGLE/CGL) │  │ (or FFI CB)  │              │
│  └───┬────┘  └──────┬──────┘  └──────┬───────┘              │
└──────┼───────────────┼────────────────┼─────────────────────┘
       │               │                │
       ▼               ▼                ▼
┌─────────────────────────────────────────────────────────────┐
│  art3m1s-core                                               │
│                                                             │
│  ┌───────────────────────────────────────────────────────┐  │
│  │  CoreRuntime (runtime.rs)  ← Flutter 帧循环入口         │  │
│  │  advance_and_render(delta_ms) → Vec<u8>               │  │
│  └───────────────────────────────────────────────────────┘  │
│                                                             │
│  ┌────────────────────┐  ┌────────────────────────────────┐ │
│  │ ffi.rs             │  │ ffi_callbacks.rs               │ │
│  │ C FFI bridge       │  │ EngineCallbacks impl           │ │
│  │ (文件/日志 I/O)     │  │ (magic paths, 按键, 鼠标, 音量)  │ │
│  └────────────────────┘  └────────────────────────────────┘ │
│                                                             │
│  ┌───────────────────────────────────────────────────────┐  │
│  │  Project (lib.rs)                                     │  │
│  │  • system.ini 解析 → ProjectConfig                    │  │
│  │  • 创建 Interpreter（FFI 或磁盘文件加载器）               │  │
│  │  • start_boot() 启动 BOOT 脚本                         │  │
│  └───────────────────────────────────────────────────────┘  │
│                                                             │
│  ┌───────────────────────────────────────────────────────┐  │
│  │  Compositor (compositor/)  ← GPU 后端无关              │  │
│  │  Scene → Anim → Build → DrawList → Renderer trait     │  │
│  │                               ↕                       │  │
│  │                          TextureProvider trait        │  │
│  └───────────────────────────────────────────────────────┘  │
│                                                             │
│  ┌───────────┐  ┌────────────┐  ┌─────────────┐  ┌───────┐  │
│  │ audio/    │  │ video/     │  │ text/       │  │ save  │  │
│  │ BGM/SE/   │  │ 全屏+图层   │  │ glyph atlas │  │ JSON  │  │
│  │ Voice     │  │ 视频播放     │  │ + scetween │  │ 存档   │  │
│  └───────────┘  └────────────┘  └─────────────┘  └───────┘  │
└─────────────────────────────────────────────────────────────┘
```

## 模块

| 模块 | 路径 | 职责 |
|------|------|------|
| **Compositor** | `compositor/` | GPU 无关的图层合成器：场景树、属性缓动、帧构建、事件归约 |
| **Backend** | `backend/gl/` | GLES 渲染后端：`glow` 实现 `Renderer` + `TextureProvider` |
| **Audio** | `audio/` | 音频子系统：BGM/SE/Voice 播放、淡入淡出、完成事件 |
| **Video** | `video/` | 视频子系统：全屏与图层视频、FFmpeg 解码、alpha 通道 |
| **Text** | `text/` | 字体渲染：`ab_glyph` 字形光栅化、scetween 逐字显示、文本注入 |
| **Save** | `save.rs` | 存档/读档：`serde_json` 序列化、目录管理 |
| **Runtime** | `runtime.rs` | `CoreRuntime` 统一帧循环入口（Flutter 前端） |
| **FFI** | `ffi.rs` / `ffi_callbacks.rs` | C FFI 桥接：外部 "C" 函数、文件/日志回调、引擎回调 |

## Feature Flags

| Feature | 默认 | 说明 |
|---------|------|------|
| `gl-backend` | ✓ | `glow` GLES 渲染器 + `image` PNG 解码 |
| `audio-backend` | ✓ | `rodio` 音频播放（WAV/MP3/Ogg/FLAC） |
| `video-backend` | ✓ | `ffmpeg-next` 视频解码 |
| `window` | ✗ | 上述全部 + `winit`/`glutin`/`mlua`（桌面窗口构建） |

## 核心 Trait

| Trait | 位置 | 说明 |
|-------|------|------|
| `Renderer` | `compositor::renderer` | GPU 后端：`render(&mut self, frame: &DrawList)` |
| `TextureProvider` | `compositor::renderer` | 纹理解析：`resolve()`, `upload_rgba()`, `pixel_alpha()` |
| `AudioBackend` | `audio::engine` | 音频播放/控制/音量/淡入淡出/完成事件 |
| `VideoBackend` | `video::engine` | 视频播放/解码/帧获取/完成事件 |
| `TextRenderer` | `text::render` | 文本渲染/字体设置/逐字显示 |
| `TextInject` | `text::inject` | 文本注入（翻译补丁等） |

## 快速开始

### 库模式（Flutter 前端）

```rust
use art3m1s_core::runtime::CoreRuntime;

let mut rt = CoreRuntime::new(stage_width, stage_height, fps, charset)?;
rt.load_project("system.ini内容", project_root)?;
rt.start_boot()?;

// 每帧
loop {
    rt.feed_input(input_snapshot);
    let pixels = rt.advance_and_render(16)?;
    // 将 pixels (RGBA) 送入 Flutter Texture 或 GPU 纹理
}
```

### 独立模式（winit + glutin）

```rust
use art3m1s_core::{Project, Compositor};
use art3m1s_core::backend::gl::{GlRenderer, GlTextureProvider};
use art3m1s_core::text::GlyphTextRenderer;

// 1. 打开项目
let project = Project::open("path/to/project", "WINDOWS")?;
let mut interpreter = project.create_interpreter();

// 2. 创建渲染器
let gl = /* GL context */;
let mut renderer = GlRenderer::new(&gl, 1280, 720, ShaderProfile::Gles300)?;
let mut tex_provider = GlTextureProvider::new(&gl)
    .with_source(|path| project.read_file(path).ok());

// 3. 创建合成器，安装各子系统
let mut compositor = Compositor::new();
let mut text_renderer = GlyphTextRenderer::new();
text_renderer.set_font(/* 字体字节 */)?;
compositor.set_text_renderer(Box::new(text_renderer));
// audio_backend 和 video_backend 已自动初始化

// 4. 启动
interpreter.set_callback(|event| {
    // 将事件应用到 compositor
    compositor.apply_event(&event);
    /* ... */
});
project.start_boot(&mut interpreter)?;

// 5. 帧循环
loop {
    interpreter.fire_enter_frame();
    interpreter.run()?;
    compositor.advance(16);
    compositor.render(&mut renderer, &mut tex_provider);
    // swap buffers
}
```

## 输入键码

脚本使用 **Windows 虚拟键码 (VK)** 进行按键检测：

| 键 | VK |
|----|-----|
| 鼠标左键 | 1 |
| Enter | 13 |
| Escape | 27 |
| Space | 32 |
| 方向键 ←↑→↓ | 37-40 |
| F1-F12 | 112-123 |

## Todo
| 项目                      |  情况  |
|-------------------------|:----:|
| 将音频设备访问迁移到前端，core传输PCM流 |      |
| 完成基本视频解码播放              |  ✅   |

## 许可证

AGPLv3.0
