# Changelog

本文档记录 `art3m1s-core` 的重要变更。

项目尽可能遵循语义化版本。由于 0.2 之前没有持续使用 release tag，0.2.0 条目描述
当前发布树，而不是严格的 tag-to-tag diff。

## [0.2.1] - 2026-07-27

### Added

- 增加独立/分层消息图层模式，兼容脚本通过 `chgmsg` 切换文本承载层。
- 字形缓存支持多页 atlas，并在同一字体族中自动寻找缺失字符。

### Changed

- `pfs-upk-rust` 作为固定提交的 Git submodule 接入，构建前需递归初始化 submodule。

### Fixed

- 修复嵌套 shader group 没有按场景树顺序递归合成，导致部分品牌页和标题画面纹理缺失。
- 修复切换字体后错误复用旧 glyph，以及单页 atlas 填满后文字突然消失的问题。
- 修复 PFS V8 解密在分块读取时从每个 chunk 重新开始 XOR key，导致超过 16 MiB 的
  OTF 等资源后半段损坏；随机访问现在会按归档内偏移继续 key stream。

## [0.2.0] - 2026-07-27

### Added

- 将 `asb-interpreter`、`art3m1s-emote` 作为支持 crate 并入仓库；
  常规构建不再依赖缺失的 Git submodule 或 sibling repository。
- ASB/IET 大规模兼容性补全，扩展控制流、文本、图层、媒体、存档、
  系统和 callback event。

- 增加同步文本补丁与非阻塞异步翻译回填，并保护 Ruby 和逐字显示动画。
- 增加触摸、右键、hover、变换后的拖动和基于 alpha 的 hit-test。
- 分离系统存档与编号存档，并扩充 scene、interpreter 和 audio snapshot。

### Changed

- 重构文本渲染、backlog 和 glyph 状态，让 UI 切换与译文复用同一套 scenario text 生命周期。

### Fixed

- 编号存档不再序列化或恢复持久化 `g.*`/`s.*` 状态，避免读取旧档后抹掉较新的存档槽。
- 修复 stop/wait、queued tag 和 inline event-frame bookkeeping 引起的
  start/continue/load/save 与返回标题卡死。
  drag release 和 drag 绑定变量不更新。
- 修复关闭 backlog 或其他 `mw` 窗口后剧情无法推进。
- 修复仅供渲染的 texture 被回收后反复 cache hit、刷屏日志并丢失 shader/mask 资源。
- 修复图层视频完成通知、重复启动和 24 FPS 视频锁定游戏渲染循环。
- 修复 E-Mote 部件位置、动作插值、口型、眼球/眨眼与多处 visibility/alpha 交互。

### Known limitations

- HLSL 只兼容测试游戏中观察到的 Artemis shader 子集，不支持任意 HLSL。
- E-Mote 对部分私有 easing/pass 语义、外部纹理和未测试 PSB model variant 的支持仍不完整。
- 部分平台 event 需要宿主实现，否则回退为安全的 no-op/default response。
- 画面输出仍使用 RGBA readback buffer。

## [0.1.0] - 2026-07-26

- 首个公开 Rust runtime，包含 ASB 解释、scene composition、OpenGL 离屏渲染、宿主 FFI、
  PFS 资源和基础存档/媒体/输入接入。
- 增加 E-Mote PSB 解析与 runtime 渲染，包括动作、表情、口型和眨眼。（试验性）
- 增加宿主解码的图层视频；宿主可把借用的 RGBA8 帧直接上传为动态 GL 纹理，无需在
    core 中放入解码器。
- 增加原生对话框 request/response、headless caption probe 和更多宿主 UI/平台回调。
- 为 `system.ini`、脚本和 PFS path 增加 Shift_JIS/UTF-8 charset 处理。
