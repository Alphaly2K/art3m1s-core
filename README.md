# art3m1s-core

`art3m1s-core` 是 Artemis 视觉小说引擎的 Rust 兼容运行时。它负责解释 ASB/IET
脚本、维护游戏状态和图层树、渲染文本与特效，并通过稳定的 C FFI 向宿主输出离屏
RGBA 帧。

当前版本为 **0.3.0**。生产宿主是 Flutter 项目
[Art3m1s](https://github.com/Alphaly2K/art3m1s)；core 本身不创建窗口，也不直接
访问用户文件系统或承担音视频解码。

## 功能概览

- ASB/AST/IET 解释器、Lua 桥接、标签过滤和队列化控制流
- 图层树、变换、混合、裁切、遮罩、转场、动画和感知变换的命中测试
- Artemis HLSL shader 子集以及 shader texture/pass 支持
- E-Mote PSB 解析、部件合成、动作、表情、口型和眨眼
- 场景文本、Ruby、逐字显示、backlog 和宿主文本翻译注入
- BGM、SE、Voice、全屏视频和图层视频的逻辑状态与完成事件
- 编号存档、系统存档、场景/音频/解释器状态恢复
- Shift_JIS 与 UTF-8 项目、PFS/目录资源和多平台启动分支
- 大型加密 PFS 的分块读取与随机访问
- 鼠标、键盘、触摸、悬停、右键、拖动和脚本事件处理器
- 面向 Flutter、原生应用和无界面工具的 C ABI

## 架构

```text
宿主应用
  ├─ 窗口、帧时钟和输入
  ├─ PFS、目录资源和存档文件访问
  ├─ 音频与视频解码
  ├─ 对话框、浏览器、HTTP 和平台服务
  └─ 译文与图层视频 RGBA 帧
                 │ C FFI 回调
                 ▼
art3m1s-core
  ├─ ffi.rs                 运行时生命周期与宿主 ABI
  ├─ runtime/               项目、控制流、输入、存档和媒体状态
  ├─ compositor/            场景树、事件、动画和绘制列表构建
  ├─ backend/gl/            OpenGL 离屏渲染器与纹理提供器
  ├─ text/                  字形光栅化、Ruby 和逐字动画
  ├─ shader/ + transition/  Artemis shader 与转场管线
  ├─ video/                 视频逻辑状态，不含解码器
  └─ crates/
      ├─ asb-interpreter/   ASB/AST/IET 解释器与 Lua 桥接
      ├─ art3m1s-emote/     E-Mote PSB 解析与参数计算
      └─ pfs-upk-rust/      PFS reader 与流式 FFI
```

`asb-interpreter` 和 `art3m1s-emote` 作为普通源码目录随仓库提供；
`pfs-upk-rust` 是显式固定版本的 Git submodule。首次 checkout 后需要执行：

```bash
git submodule update --init --recursive
```

core 不依赖 sibling interpreter 仓库。

## 运行时边界

### 窗口与帧

每次 tick 通过 `advance_and_render`、`advance_and_present` 或
`advance_without_render` 之一推进脚本和子系统。合成结果保留在内部 FBO；支持时通过
ANGLE 提交到宿主共享表面，否则 `glReadPixels` 回读 RGBA。静止帧可以不重绘，但仍要
推进逻辑。宿主负责窗口、显示缩放、帧率调度和呈现。常规渲染调用保存并恢复宿主 GL context。

### 文件与存档

项目文件和存档 I/O 都由 `ffi.rs` 中注册的回调提供。core 只传递逻辑路径，宿主负责
将其解析到 PFS 归档、解包目录或应用沙箱。

编号存档包含局部变量、解释器位置与调用栈、场景状态和音频状态。持久化的 `g.*` 和
`s.*` 域不会写入编号存档，因此读取旧档不会回滚或清空当前存档索引。系统状态通过
`syssave()` 单独写入 `saveg.dat` 和 `system.dat`。

### 音频与视频

core 不包含 FFmpeg、mpv 或平台解码器。它只向宿主发送媒体命令，并等待对应的播放
完成回调。

全屏视频由宿主直接显示。图层视频优先使用 `art3m1s_runtime_video_gl_*`，让宿主的
外部 renderer 在 core 的 GL context 中直接绘制到图层 FBO，避免 RGBA 跨边界搬运。
不支持时使用 `art3m1s_runtime_upload_video_layer_frame` 同步上传借用的 RGBA8 指针。
两条路径都参与图层合成；同一 runtime 的上传、GL lease 和帧推进必须串行执行。

### 文本与翻译

场景文本进入字形布局前，会先交给已注册的文本注入回调：

- 返回非负长度：立即渲染回调提供的替换文本；
- 返回 `-1`：保留原文；
- 返回 `-2`：立即显示原文，同时让宿主把翻译任务加入异步队列。

宿主完成翻译后调用 `art3m1s_runtime_submit_text_translation`。core 只会在目标文本
仍属于同一页面时进行替换，并等待当前逐字动画结束后再更新可见文本，因此迟到的
网络响应不会覆盖下一页内容。

Ruby 始终保持 `RubyStart(reading) -> ScenarioText(base) -> RubyEnd` 的事件顺序。
只有中间的正文可以被替换；注音可作为翻译上下文保留，最终位置会根据译文范围重新
计算。

## ASB/IET 兼容层

- 直接读取编译 ASB/IET 记录，保留内嵌 Lua、标签及编译控制流地址；
- 编译宏的原文件跳转、条件返回、嵌套参数作用域和动态调用目标；
- 队列标签、跳转/调用/返回、等待、停止/恢复和内联事件栈帧；
- 文本页、链接、backlog、已读状态、自动模式和快进；
- 图层创建/编辑、事件注册、拖动、tween/anime 和转场；
- BGM/SE/Voice 控制、淡入淡出、声像、交叉淡化和完成事件；
- 存档/读档、对话框、浏览器/HTTP/native 请求和平台状态回调；
- shader、截图、视频和其他系统事件。

这些兼容实现来自可用文档和真实 Artemis 游戏行为，并不代表已经覆盖每个私有引擎
版本或游戏自定义扩展。

## Lua 后端

| 目标平台 | 后端 | 原因 |
|---|---|---|
| iOS | Luau | 避免 Lua 5.1 的 `system()` 在 iOS 上导致编译失败 |
| 其他平台 | Lua 5.1 | 保留旧游戏依赖的弱类型和脚本行为 |

解释器会为已支持游戏提供必要的兼容层。根目录 `Cargo.toml` 通过按目标选择的 Cargo
feature 自动决定后端。

## 关键 FFI

| 函数 | 用途 |
|---|---|
| `art3m1s_runtime_create` / `destroy` | 创建和销毁 runtime |
| `art3m1s_runtime_set_emote_backend` | 在项目加载前选择内置或实验性 Eluna E-Mote 后端 |
| `art3m1s_runtime_load_project_bytes` / `load_project` | 从原始字节/UTF-8 内容加载 `system.ini` 并启动项目 |
| `art3m1s_runtime_advance_and_render` | 推进一帧并返回 RGBA |
| `art3m1s_runtime_set_external_surface` / `clear_external_surface` | 绑定或解绑 Android `ANativeWindow` / Apple `IOSurface` / `MTLTexture` |
| `art3m1s_runtime_advance_and_present` | 推进一帧并直接提交到宿主纹理，静止画面不重复提交 |
| `art3m1s_runtime_advance_without_render` | 显示链繁忙时仅推进逻辑，保持 `onEnterFrame` 时序 |
| `art3m1s_runtime_set_profiler_enabled` / `profiler_snapshot` | 开启异步性能采样并读取 JSON 快照 |
| `art3m1s_runtime_feed_mouse` | 更新鼠标坐标 |
| `art3m1s_runtime_feed_mouse_button` | 发送鼠标左右键状态变化 |
| `art3m1s_runtime_feed_touch` | 发送触摸阶段 |
| `art3m1s_runtime_feed_key` | 发送 Windows 虚拟键输入 |
| `art3m1s_runtime_notify_video_finished` | 通知宿主视频操作完成 |
| `art3m1s_runtime_notify_sound_finished` | 通知宿主音频操作完成 |
| `art3m1s_runtime_upload_video_layer_frame` | 上传借用的 RGBA8 图层视频帧 |
| `art3m1s_runtime_video_gl_*` | 外部视频 renderer 借用 core GL context 并直接绘制图层 FBO |
| `art3m1s_register_file_reader` / `writer` / `delete` | 注册宿主文件系统回调 |
| `art3m1s_register_media_command_callback` | 接收宿主媒体命令 |
| `art3m1s_register_ui_command_callback` | 接收对话框和平台请求 |
| `art3m1s_register_text_inject_callback` | 同步查找补丁或发起异步翻译 |
| `art3m1s_runtime_submit_text_translation` | 回填异步翻译结果 |
| `art3m1s_probe_caption` | 导入资料库时无界面探测项目标题 |

其他 Host 接入所需的生命周期、线程、指针所有权、帧循环和验收清单见
[HOST_INTEGRATION.md](HOST_INTEGRATION.md)；完整 C 声明、返回值、媒体/UI JSON 和
Profiler 字段见 [FFI_REFERENCE.md](FFI_REFERENCE.md)。当前回调为进程级注册，不提供
多 runtime 的资源隔离，也没有统一 ABI 版本查询接口。

## 输入模型

脚本使用 Windows 虚拟键值。常见映射包括：鼠标左键 `1`、鼠标右键 `2`、Enter
`13`、Escape `27`、Space `32`、方向键 `37..40` 和 F1-F12 `112..123`。

每帧中，core 会更新鼠标位置和按键边沿，执行感知变换与 alpha 的命中测试，派发
`click`、`rollover`、`rollout`、`drag*` 等图层事件，最后让排队的脚本标签通过
正常解释器路径运行。模态窗口的子图层会吸收空白点击，避免打开 save/load/config/
backlog 后点击空白处仍推进底层剧情。

## 构建

前置要求：

- 支持 Rust 2024 edition 的稳定版 Rust
- 启用默认 `gl-backend` feature 时，需要可用的 OpenGL context
- 对应目标平台的 SDK 和工具链

```bash
cargo fmt --check
./scripts/test-all.sh
cargo build --release
```

`scripts/test-all.sh` 会依次运行 core、Lua 5.1/Luau 两种解释器后端、两个
E-Mote 实现和 PFS 子模块的自包含测试。需要本地商业游戏资源的兼容性测试默认
不会执行；fixture 配置和显式运行方法见 [`tests/README.md`](tests/README.md)。

默认构建包含 GL 渲染器和实验性 Eluna 适配器。仅使用不依赖 GPU 的核心模块时可以
关闭全部默认 features（此时不提供 `art3m1s_runtime_*` C 接口）：

```bash
cargo build --no-default-features
```

### 实验性 Eluna E-Mote 后端

除内置的 `art3m1s-emote` 外，默认构建也包含 `experimental-eluna` feature，
无需额外传入 `--features`。只保留 GL 渲染器和内置 E-Mote 后端时使用：

```bash
cargo build --release --no-default-features --features gl-backend
```

[`crates/eluna`](crates/eluna/README.md) 是基于
[`xmoezzz/eluna`](https://github.com/xmoezzz/eluna) commit
`12e4d2fa03b64714a83a0363eaadf26a125d9fe6` 的仓库内兼容性适配版本，包含性能和渲染修复。
默认编入不等于默认使用：运行时仍选择内置后端，宿主必须在加载项目之前显式调用
`art3m1s_runtime_set_emote_backend(..., 1)` 才会切换。

仓库内 Eluna crate 声明 `MPL-2.0`，许可证正文见
[`crates/eluna/LICENSE`](crates/eluna/LICENSE)。

Flutter 和 iOS 打包方式见宿主仓库。iOS 构建会自动选择 Luau，桌面和 Android 构建
会选择 Lua 5.1。

## 状态与限制

- HLSL 支持以 HENPRI 等 Artemis 游戏实际使用的 shader 形式为目标，并不是通用
  DirectX shader 编译器。
- E-Mote 当前针对已测试游戏使用的 PSB/model 变体。部分私有 easing、pass/step
  行为和外部纹理格式仍未完整支持。
- HTTP、native call、浏览器打开和振动等宿主服务只有在嵌入应用实现相应回调后才可用。

版本详情见 [CHANGELOG.md](CHANGELOG.md)

## 许可证

[AGPLv3](LICENSE)
