# art3m1s-core

`art3m1s-core` 是 Artemis 视觉小说引擎的 Rust 兼容运行时。它负责解释 ASB/IET
脚本、维护游戏状态和图层树、渲染文本与特效，并通过稳定的 C FFI 向宿主输出离屏
RGBA 帧。

当前版本为 **0.2.0**。生产宿主是 Flutter 项目
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

三个支持 crate 都是仓库中的普通源码目录。完整 checkout 不需要初始化 Git submodule，
也不依赖单独的解释器仓库。

## 运行时边界

### 窗口与帧

`CoreRuntime::advance_and_render(delta_ms)` 每帧推进脚本和各个子系统，渲染到离屏
FBO，最后通过 `glReadPixels` 返回 RGBA。宿主负责窗口、显示缩放、帧率调度和画面
呈现。GL 后端会在渲染前后保存并恢复宿主的 OpenGL context。

### 文件与存档

项目文件和存档 I/O 都由 `ffi.rs` 中注册的回调提供。core 只传递逻辑路径，宿主负责
将其解析到 PFS 归档、解包目录或应用沙箱。

编号存档包含局部变量、解释器位置与调用栈、场景状态和音频状态。持久化的 `g.*` 和
`s.*` 域不会写入编号存档，因此读取旧档不会回滚或清空当前存档索引。系统状态通过
`syssave()` 单独写入 `saveg.dat` 和 `system.dat`。

### 音频与视频

core 不包含 FFmpeg、mpv 或平台解码器。它只向宿主发送媒体命令，并等待对应的播放
完成回调。

全屏视频由宿主直接显示。图层视频则由宿主解码最新的 RGBA8 帧，再调用
`art3m1s_runtime_upload_video_layer_frame`。传入指针只在该次调用期间借用；core
会直接将其上传到动态 GL 纹理，并作为普通图层参与合成。同一 runtime 的上传和
渲染调用必须串行执行。

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

0.2.0 开发周期包含 Fable 对解释器、运行时、合成器和文本系统进行的大规模兼容性
补全，扩展的 Artemis 功能包括：

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
| `art3m1s_runtime_load_project` | 读取 `system.ini` 并启动项目 |
| `art3m1s_runtime_advance_and_render` | 推进一帧并返回 RGBA |
| `art3m1s_runtime_feed_mouse` | 更新鼠标坐标 |
| `art3m1s_runtime_feed_mouse_button` | 发送鼠标左右键状态变化 |
| `art3m1s_runtime_feed_touch` | 发送触摸阶段 |
| `art3m1s_runtime_feed_key` | 发送 Windows 虚拟键输入 |
| `art3m1s_runtime_notify_video_finished` | 通知宿主视频操作完成 |
| `art3m1s_runtime_notify_sound_finished` | 通知宿主音频操作完成 |
| `art3m1s_runtime_upload_video_layer_frame` | 上传借用的 RGBA8 图层视频帧 |
| `art3m1s_register_file_reader` / `writer` / `delete` | 注册宿主文件系统回调 |
| `art3m1s_register_media_command_callback` | 接收宿主媒体命令 |
| `art3m1s_register_ui_command_callback` | 接收对话框和平台请求 |
| `art3m1s_register_text_inject_callback` | 同步查找补丁或发起异步翻译 |
| `art3m1s_runtime_submit_text_translation` | 回填异步翻译结果 |
| `art3m1s_probe_caption` | 导入资料库时无界面探测项目标题 |

回调数据格式、线程约束和宿主接线状态见
[HOST_INTEGRATION.md](HOST_INTEGRATION.md)。

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
cargo test
cargo build --release
```

默认构建包含 GL 渲染器。只使用解析器或运行时逻辑时可以关闭它：

```bash
cargo build --no-default-features
```

Flutter 和 iOS 打包方式见宿主仓库。iOS 构建会自动选择 Luau，桌面和 Android 构建
会选择 Lua 5.1。

## 状态与限制

- HLSL 支持以 HENPRI 等 Artemis 游戏实际使用的 shader 形式为目标，并不是通用
  DirectX shader 编译器。
- E-Mote 当前针对已测试游戏使用的 PSB/model 变体。部分私有 easing、pass/step
  行为和外部纹理格式仍未完整支持。
- HTTP、native call、浏览器打开和振动等宿主服务只有在嵌入应用实现相应回调后才可用。
- RGBA 回读接口以简单和跨平台为优先，目前不是零拷贝的画面呈现路径。

版本详情见 [CHANGELOG.md](CHANGELOG.md)

## 许可证

[AGPLv3](LICENSE)
