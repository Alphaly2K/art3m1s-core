# Changelog

本文档记录 `art3m1s-core` 的重要变更。

## [Unreleased]

### 变更

- 项目许可证由 AGPL-3.0 更换为 MPL-2.0（文件级 copyleft，兼容 App Store
  分发；`crates/pf8` 保留上游 MIT）。
- PFS 归档实现整体替换为基于 pf8 的新实现：条目哈希索引、分卷串联读取、
  显式条目名编码与范围读取；移除 GPL 重写实现及其 git submodule。

### 新增

- 宿主运行时覆盖字体接口（`art3m1s_set_font_override` /
  `art3m1s_clear_font_override`）：脚本自带字体缺译文字形时，由宿主提供
  TTF/OTF 覆盖全部脚本字体的光栅化来源。

### 修复

- 编译 ASB/IET 直接解析二进制记录，保留 Lua 字符串、源行号和编译器跳转地址，
  修复含内嵌引号的脚本在启动时解析失败，以及编译条件分支和循环错误。
- 编译宏保留原文件标签、条件返回和调用期间的参数作用域，支持动态调用目标，
  防止按钮、语音等宏的可选参数相互污染。
- 按接口约定让 `e:var` 返回字符串、标签过滤器接收求值后的参数，修复非数字
  字符串比较错误，并补充宏局部变量的查询、写入和恢复。
- 补充 `getFrameNumber`、`getScriptSize`，图层查询返回可见性、透明度及变换属性，
  修复另一类标签宏模板中按钮未注册、开始游戏后脚本停止的问题。
- 存读档回调传入文件名，编号存档保留编译宏调用参数，不回滚全局存档索引。
- magicpath 支持反斜杠分隔符，避免带尾部分隔符的映射产生重复斜杠。
- 空字体图集及上传失败时不再被当作游戏文件读取，避免误报 `image/text/atlas` 缺失。
- 修复 Lua 标签过滤器返回放行时吞掉同名宏的问题，使背景、事件 CG 和立绘宏
  可以继续执行；保留非零返回值拦截及过滤器替换命令的行为。
- 启动时按项目字符编码读取可选的 `tag.ini`，支持分节编号参数表和带前缀的
  日文行标签，避免立绘命令被当成对白；参数表按解释器隔离，不串用其他游戏定义。
- 图层信息查询包含同批尚未派发的图层事件；首次加载图片时按需读取文件头尺寸，
  避免立绘定位读到宽高 0 后被移出舞台。查询复用合成器规则，不触发额外绘制。

## [0.3.0] - 2026-09-01

### 新增

- 新增移动端侧 ASTC 纹理压缩支持。
- 新增 DXT5 纹理压缩支持。
- 新增静止帧跳过重新渲染机制。
- 新增脏区渲染更新机制。
- 调试模式新增 Profiler 功能。
- 新增 libmpv 解码器预热以加速视频解码播放。
- 新增 E-Mote 纹理缓存和释放机制。
- 新增字体纹理缓存机制。

### 变更

- 将 ANGLE 实现统一为 Google 官方分支。
- 画面输出路径更改为系统共享纹理。

### 修复

- 修复了 skip mode 相关 bug。
- 修复了 E-mote 播放计时器和游戏渲染时序不同步的问题。
- 修复了由于自定义参数丢失而造成的逐字动画示例文本无法显示的问题。

### 已知边界

- HLSL 仍只兼容已验证游戏使用的 Artemis shader 子集。
- E-Mote 对少量未验证的 PSB model variant 和私有 motion 语义仍可能不完整，将在下个版本中提供实验性路径扩展对 E-mote 的支持。

## [0.2.2] - 2026-07-31

### 新增

- 增加 intermediate-render 图层组的独立合成阶段，支持组级 alpha、颜色、混合模式、
  mask 和 shader 结果。
- 图层信息查询可返回缓动中的实时位置，并在脚本未指定尺寸时回退到纹理逻辑尺寸。
- BGM 事件支持 Artemis `*_a` 引导段与 `*_b` 循环段的分段播放信息。

### 变更

- 跳过模式不再为每页固定停留三帧；当前页完整显示一帧后即可继续推进。
- 文本排版按脚本的 `spacetop`、`spacemiddle`、`spacebottom`、逐行对齐和字距语义计算。
- 事件过滤器接收注册标签的完整参数，并区分“假装成功”和“假装失败”。

### 修复

- 修复隐藏或删除消息窗父图层、切换 UI、返回标题和转场时剧情文本仍残留的问题。
- 修复侧边浮窗缓动期间 `get_layer_info` 返回旧坐标，导致展开后无法按脚本收回的问题。
- 修复 intermediate-render 把父透明度重复乘到人物面部等重叠子图层，造成局部透明异常。
- 修复异步译文长于原文时 reveal 进度提前结束、后半段文字保持透明的问题。
- 修复居中/右对齐换行、Ruby 行高和脚本字距没有按 Artemis 排版参数生效的问题。
- 修复若干预处理器、系统变量、输入事件参数和消息层恢复语义的兼容性缺口。

### 已知边界

- HLSL 仍只兼容已验证游戏使用的 Artemis shader 子集。
- E-Mote 对少量未验证的 PSB model variant 和私有 motion 语义仍可能不完整。
- 画面输出仍通过宿主读取 RGBA buffer。

## [0.2.1] - 2026-07-27

### 新增

- 增加独立/分层消息图层模式，兼容脚本通过 `chgmsg` 切换文本承载层。
- 字形缓存支持多页 atlas，并在同一字体族中自动寻找缺失字符。

### 变更

- `pfs-upk-rust` 作为固定提交的 Git submodule 接入，构建前需递归初始化 submodule。

### 修复

- 修复嵌套 shader group 没有按场景树顺序递归合成，导致部分品牌页和标题画面纹理缺失。
- 修复切换字体后错误复用旧 glyph，以及单页 atlas 填满后文字突然消失的问题。
- 修复 PFS V8 解密在分块读取时从每个 chunk 重新开始 XOR key，导致超过 16 MiB 的
  OTF 等资源后半段损坏；随机访问现在会按归档内偏移继续 key stream。

## [0.2.0] - 2026-07-27

### 新增

- 将 `asb-interpreter`、`art3m1s-emote` 作为支持 crate 并入仓库；
  常规构建不再依赖缺失的 Git submodule 或 sibling repository。
- ASB/IET 大规模兼容性补全，扩展控制流、文本、图层、媒体、存档、
  系统和 callback event。

- 增加同步文本补丁与非阻塞异步翻译回填，并保护 Ruby 和逐字显示动画。
- 增加触摸、右键、hover、变换后的拖动和基于 alpha 的 hit-test。
- 分离系统存档与编号存档，并扩充 scene、interpreter 和 audio snapshot。

### 变更

- 重构文本渲染、backlog 和 glyph 状态，让 UI 切换与译文复用同一套 scenario text 生命周期。

### 修复

- 编号存档不再序列化或恢复持久化 `g.*`/`s.*` 状态，避免读取旧档后抹掉较新的存档槽。
- 修复 stop/wait、queued tag 和 inline event-frame bookkeeping 引起的
  start/continue/load/save 与返回标题卡死。
  drag release 和 drag 绑定变量不更新。
- 修复关闭 backlog 或其他 `mw` 窗口后剧情无法推进。
- 修复仅供渲染的 texture 被回收后反复 cache hit、刷屏日志并丢失 shader/mask 资源。
- 修复图层视频完成通知、重复启动和 24 FPS 视频锁定游戏渲染循环。
- 修复 E-Mote 部件位置、动作插值、口型、眼球/眨眼与多处 visibility/alpha 交互。

### 已知边界

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
