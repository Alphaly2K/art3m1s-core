# Artemis 导出审计与重构范围

审计日期：2026-11-17。源码基线：`a809729bcc179ee56a7bae6aaab71b23fd4ea8e6`。
本文记录当前事实；目标接口见 [HOST_ABI_V2.md](HOST_ABI_V2.md)。v2 是设计草案，尚未实现。
本文为实施前的审计快照，行号、哈希和“双 Lua 后端”等事实对应上述基线；后续实施状态
以 GOAL.md 和当前源码为准，不覆盖这份原始清单。

## 1. 结论与范围

当前问题主要是职责、状态和协议耦合，不能只靠减少动态符号或重新排列函数表解决。
Artemis 已有一个动态查询入口，但入口后面的 59 个函数槽同时承担资源挂载、游戏逻辑、
GPU 执行、平台表面、媒体、UI 和诊断。Rust 公共模块又公开了大量实现细节。

本轮只规划 Artemis/Art3m1s。RFVP、KRKR、Siglus 的 ABI、运行时、上游补丁、构建默认值
和 Host 适配均冻结。它们涉及共享 crate 时也不能被 Artemis 的迁移顺带改变。
本轮产物是审计、逐项清单和接口草案，不修改生产实现或 Flutter Host。

## 2. 实际公开面

| 层次 | 源码事实 | 重构意义 |
|---|---|---|
| Artemis 动态查询入口 | `src/ffi/api.rs` 只有 `art3m1s_get_api_v1` 带 `no_mangle` | 已收敛查询入口；不应声称仍有 85 个 Artemis 平铺动态符号 |
| v1 函数表 | 3 个头字段，59 个函数指针槽；`runtime_set_runtime_media_enabled` 在未启用 `ffmpeg` 时为空 | Host 要理解的实际 ABI 仍很大 |
| C 调用约定实现 | `src/ffi/mod.rs`、`src/host/events.rs`、`src/host/files.rs` 共 85 个 `pub unsafe extern "C" fn` | `extern "C"` 不等于动态导出；其中 26 个未出现在 v1 表里 |
| 其他引擎 | RFVP 1、KRKR 3、Siglus 4 个显式查询入口 | 仅列作边界，不合并、重命名或迁移 |
| PFS | 依赖 `pfs-upk-rust` 有 9 个 `pfs_*` 的 `no_mangle` 声明 | 是独立归档 API；是否进入 core 最终产物需按具体链接产物检查 |
| Rust API | 根模块公开 audio/backend/compositor/ffi/host/profiler/render_pipeline/runtime/save/text/video，重导出 emote/media/script/archive 等 | 实现目录与依赖 crate 被当作公共 API；缺少稳定 facade 与内部实现的区分 |

逐个函数、签名、源码位置、表字段和建议去向见
[abi/artemis-v1-inventory.json](abi/artemis-v1-inventory.json)。计数来自源码声明，包含条件编译项，
不代表任意单一 feature/target 下均可调用。此次没有把旧二进制或源码扫描当作最终链接符号证明；
迁移阶段必须对新构建产物补做 `nm`/`dumpbin` allowlist 检查。

## 3. 主要发现

### A1：资源 Host 实际在 core 内执行物理 IO（高）

证据：`src/host/files.rs` 的 `HostResources` 内持有 `PathBuf`、`File` 和 `PfsArchive`；
`mount_directory`/`mount_pfs` 枚举和打开本机资源，`write`/`delete` 创建目录和写删文件。
`src/lib.rs::Project::open`、`read_file`、`create_interpreter_with_resources` 还保留磁盘回退。

影响：新 Host 必须接受 core 的文件系统、挂载、编码和存档路径策略。对象名称为 HostResources，
并不意味着 IO 已由外部 Host 实现。未来 core 应只接收逻辑资源名、字节和 IO 结果；目录、PFS
卷优先级、SAF/沙箱、网络缓存及物理 save root 归 Host adapter。PFS 解析库可以复用，
但不从新核心 ABI 接收磁盘路径。

### A2：运行时创建和逻辑推进仍依赖 GPU（高）

证据：`src/runtime/mod.rs::CoreRuntime` 持有 `Box<dyn GpuBackend>`；`create` 调用
`src/backend/mod.rs::create_backend`；`advance_without_render` 仍执行 `begin_access` 和
GPU 资源回收。`src/ffi/mod.rs`/`api.rs` 的主运行时 ABI 受图形 feature 门控。
`Cargo.toml` 又按目标 OS 自动启用 `art3m1s-render` 的 Metal/Vulkan，故
`--no-default-features` 不等价于“无原生 GPU 依赖”。

影响：Host 即使只想做无窗口执行、标题探测或自有 renderer，也需要承担 core 的 GPU
选择和平台依赖。目标应保留 core 的图层/文字/E-Mote/转场语义，将 GPU 执行和呈现交给
Host renderer；现有 `art3m1s-render` 可作为 Host 侧复用组件，不能要求 Host 重写算法。
目标边界为 core 生成渲染数据、Host 绘制并呈现，见 v2 草案。

### A3：独立句柄尚未实现完整会话隔离（高）

证据：`src/host/events.rs::ACTIVE_HOST_EVENTS` 路由到最近启用的队列，事件头不含
runtime/session 身份；`src/host/files.rs::DEFAULT_RESOURCES` 提供默认资源；
`src/ffi/mod.rs` 的 debug、脚本日志配置、覆盖字体等是进程全局状态。
`src/runtime/callbacks.rs` 的 surface cache 也需要纳入会话化检查。

影响：两实例同时运行或加载探针与主游戏交错时，输出和配置存在串会话风险。新接口所有
状态归 runtime，线程局部路由只负责选择调用期间的日志目标。

### A4：异步结果缺少统一的过期保护（高）

证据：`runtime_submit_http_result` 只接收 runtime、HTTP 状态和 body，虽然请求事件有
serial，响应入口并不核对它；`submit_dialog` 无请求 ID；音视频完成通知只带业务图层/通道 ID。
翻译已有 serial，可作为迁移参考，但不是全协议统一身份。

影响：旧 HTTP 响应、旧对话框或同名通道上一次播放的完成通知，可能作用于新请求。
目标应校验 `(runtime, session_epoch, request_id)`；媒体另带播放实例 ID，不能只使用可复用的
脚本 ID。重新载入项目必须使旧结果全部失效。

### A5：关键事件可以被队列溢出丢弃（高）

证据：`src/host/events.rs::HostEventsInner::push` 限制 16,384 条、每条 payload 最多 4 MiB。
满队列优先移除日志；没有日志可移除时 `pop_front()` 会移除其他事件；超长 payload 直接返回。
Rust 有 `dropped_events()`，但 59 槽 ABI 没有对应统计入口，也没有总字节预算。

影响：丢失 dialog/HTTP/媒体命令可能导致等待无法完成。新标准必须区分可丢诊断与可靠控制请求，
定义字节预算、背压和可观测丢弃数；不能只扩容队列。

### A6：返回值和对象安全约定不统一（中）

证据：load 用 0 成功/-1 失败，set_resources 用 1/0，present 用 1/0/-1，render 用
字节数/0，profiler 用写入字节数/负的所需大小。很多 setter 是 void。
`CoreRuntime*`、`HostEvents*`、`HostResources*` 都直接解引用裸指针；判空不能识别失效、
伪造或重复销毁。部分函数捕获 panic，覆盖并不统一。

影响：Host 需要逐函数记忆特殊规则；“无新帧”“缓冲不足”“错误”被混在一起。
新 ABI 应统一 status 与 out 参数，使用带类型检查的代际句柄，明确 owner 线程与错误状态。
RFVP/KRKR 的句柄工具可参考设计，但不在本轮改动其共享实现或现有语义。

### A7：同步脚本 IO 是迁移的真实阻点（高）

证据：`crates/asb-interpreter/src/lua_engine.rs` 的 `e:isFileExists` 是同步 Lua 方法；
`src/runtime/callbacks.rs` 的 `file_write`、`bind_surface`、PNG 注释读取、E-Mote 创建及
`src/runtime/save_io.rs` 的存档读取，都同步访问资源。

影响：不能把 `read_file()` 替换为“发事件并返回空值”，也不能重新执行已经产生副作用的 Lua
来恢复加载。拉取式 IO 必须先证明脚本、Lua 5.1 和 Luau 的 continuation/预载方案能保持
执行语义；这是 v2 稳定前的阻断验证，而非尾声优化。

### A8：媒体旁路和时钟也需纳入平台边界（中）

证据：启用 FFmpeg 时 `src/runtime/media_session.rs::start_audio_extraction` 创建线程、
生成本机临时 WAV、删除文件并发出含本机路径的 `video_audio_play`。
输入重复计时使用 `Instant::now()`；解释器、Lua API 和部分 tag 读取 `SystemTime`。

影响：只有“声音设备由 Host 打开”仍不足以保证 IO/调度边界。媒体解码与临时文件策略应移到
Host service；逻辑时间、日历时间和随机种子由 Host 注入，诊断耗时测量可在可选层使用本机时钟。
Lua 标准库/宿主库中可访问的 IO、os、时间入口也必须单独审计，不能仅检查顶层 Rust 源码。

### A9：文档、注释和生产 Host 存在漂移（中）

- `src/ffi/api.rs` 开头仍写平铺符号暂时保留，`src/ffi/mod.rs` 则说明它们不再动态导出。
- README 和 HOST_INTEGRATION 写默认包含 RFVP/KRKR；实际 Cargo metadata 的 default 只有
  `gl-backend`。本轮不改其他引擎 feature，后续另做事实校正。
- FFI_REFERENCE 仍把 backend=0 描述成 CGL，实际工厂选择平台默认 Metal/Vulkan 并可回退 GL。
- FFI_REFERENCE 的媒体枚举没有覆盖 `src/host/media.rs` 已有的 `video_audio_play`。
- v1 源码注释要求表大小精确相等；生产 Flutter 的 `CoreApiV1.tryLoad` 接受
  `structSize >= sizeOf`，且未使用查询返回的 size 作相同检查。
- 生产 Host 在 `lib/engine/backends/art3m1s/core_api.dart` 手写映射和函数转换，
  `file_provider.dart` 又包了一层 core 资源挂载。后续应从唯一 schema/头文件生成绑定。

生产 Host 仅作只读交叉核对，没有修改；这些发现不构成对旧 Host ABI 的自动修复授权范围扩展。

## 4. 旧函数的处置原则

| 当前职责 | v2 去向 |
|---|---|
| runtime 创建/加载/退出/状态 | 小型 runtime 表；状态显式返回 |
| 各种 feed_input | 版本化输入批次；保留舞台坐标和同帧边沿顺序 |
| HTTP/dialog/翻译/媒体 submit/notify | 统一有身份的结果消息；按 service 保留明确 schema |
| HostEvents create/enable/全局路由 | runtime 自有可靠控制队列和有界诊断队列 |
| 目录/PFS 挂载、save_dir | 移到 Host IO adapter；新核心没有物理路径接口 |
| 覆盖资源/字体/窗口状态/文本替换 | runtime 自有资源版本和 Host 状态消息 |
| GPU 选择、surface、present、RGBA readback | Host renderer adapter；core 输出平台无关帧数据 |
| 内置视频解码/GL video lease | Host media + renderer adapter |
| profiler/debug | 可选诊断协议；分清 core 与 Host/GPU 指标 |
| caption 探针 | 无 renderer 的 probe runtime，复用同一 IO 协议 |
| 26 个未入表的 C 实现 | 逐项决定迁移或删除；不能因不在 v1 表就认定没有 Rust/内部调用方 |

所有现有业务能力必须有去向。函数合并不意味着删掉媒体时序、字幕、覆盖字体、存档截图、
HLSL、damage、升频、转场捕获或 E-Mote 特殊绘制语义。

## 5. 已执行的核对与限制

- 核对当前 tracked diff（审计开始时为空）；保留已有未跟踪探针、测试和子目录。
- 阅读项目交接规则、README、测试说明、v1 实现、Host 边界、运行时和 render 契约。
- 使用 `cargo metadata --locked --offline --no-deps --format-version 1` 核对实际 features。
- 清单按源码声明核对 59 个表槽、85 个 C ABI 实现、26 个未入表函数及显式查询入口。
- 只读核对生产 Flutter 的 v1 表加载和文件挂载入口。
- 校验清单的源码 SHA-256、85 个函数位置、18 个显式导出声明与 59 槽覆盖；各实现均有迁移去向。
- C 头文件草案通过本机 Clang 的 C11/C++17 严格语法检查；本轮不验证其未实现的运行时行为。

审计阶段未构建新动态库、运行商业游戏或修改执行代码。ABI 二进制兼容、跨平台构建和
行为回归按实施阶段验收，顺序见 [GOAL.md](../GOAL.md)。
