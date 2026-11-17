# Artemis Host ABI v2：接口标准与迁移计划（草案）

日期：2026-11-17。状态：供评审的设计，未实现，不能用于加载当前动态库。
现状证据见 [EXPORT_AUDIT.md](EXPORT_AUDIT.md)，C 接口骨架见
[abi/art3m1s_core_v2.draft.h](abi/art3m1s_core_v2.draft.h)。

## 1. 范围与设计决策

只重构 Artemis/Art3m1s 的公开接口及其内部 Host 边界。RFVP、KRKR、Siglus 保持现有
独立 ABI、构建和适配，不纳入统一 runtime，不修改它们的上游补丁。共同依赖 crate 的
破坏性调整必须另行评估；本方案不能用一次共享接口替换迫使其他引擎一起迁移。

用户已确认画面边界：**core 生成渲染数据，Host 绘制并呈现**。据此，core 保留引擎语义
与 CPU 侧计算，Host 执行物理 IO、GPU 绘制、音视频解码/输出和最终呈现。Host 拥有
renderer，core 不保留原生 GPU 合成与 present 的生产路径；现有 renderer 以 Host SDK 复用。

其余建议决策：

1. 新增独立 v2，冻结 v1 布局、返回值和 feature 语义；禁止原地把 v1 指针参数改成整数。
2. 一个动态查询符号、小型 runtime 表、独立 frame 表，能力与消息 schema 分开版本化。
3. 继续使用 Host 调 core、core 排队、Host 拉取；不恢复 native→Dart 或其他语言反向回调。
4. 所有游戏状态、输出和请求按 runtime 隔离；会话重载增加 epoch。
5. 统一 status、代际句柄、明确所有权；不公开 Rust 容器、trait、DrawList 内存布局或平台指针。
6. 先完成 IO continuation 与帧契约验证，再冻结 v2 的消息编号、capability 位和绑定。

这份草案冻结的是评审范围和目标约束；尚未冻结可发布 ABI。头文件故意不为尚未验证的
消息 kind、能力位和完整渲染 opcode 编造稳定编号。P0 必须补齐 schema，P1/P2/P3 验证
通过后才可称为可接入的标准。

## 2. 职责边界

| Core | Host / 可复用的 Host SDK |
|---|---|
| ASB/AST/IET/Lua 语义、变量、流程、事件顺序 | 窗口、系统生命周期、输入设备、坐标转换、线程调度 |
| 图层树、布局、命中测试、动画、转场语义 | GPU device/context、纹理上传、命令执行、同步、surface 和 present |
| 文本/Ruby 排版、字形 CPU 数据、E-Mote 求值和网格 | 系统字体发现，提交字体字节，选择 renderer 的质量/升频策略 |
| 资源逻辑路径、缓存版本、必要的纯字节解析/解码 | 目录/PFS/其他归档、分卷优先级、覆盖层、网络缓存、沙箱 |
| 存档格式、快照/恢复、脚本存档钩子 | 读写删除、原子落盘、持久化确认、物理存档目录 |
| 音视频逻辑状态、等待/完成处理器、播放实例标识 | 解码、混音、设备、音频时钟、视频表面/全屏展示 |
| 服务请求和回填规则 | HTTP、对话框、剪贴板、浏览器、翻译、native call 等 |
| 消费 Host 注入的逻辑/日历时间和随机种子 | 时钟来源、后台时间策略、时区和随机种子生成 |

Core 可对 Host 提交的字节做 PNG、字体、PSB 等平台无关计算；“IO 由 Host 实现”
不要求 Host 重写这些格式解析。Core 不读取磁盘路径，不加载图形驱动，不调用系统设备，
不偷偷创建临时文件、线程式媒体提取或窗口。所有平台对象止于 Host adapter。

```mermaid
flowchart LR
    H[Host 帧循环与服务调度] -->|输入 / 时钟 / 结果消息| A[v2 runtime facade]
    A --> C[脚本 / 图层 / 文字 / E-Mote / 存档语义]
    C --> Q[会话控制事件与 IO 请求]
    Q -->|poll_events| H
    C --> F[不可变平台无关帧]
    F -->|acquire / release| R[Host renderer]
    H --> I[Host IO / 媒体 / UI]
    R --> P[Host GPU 与显示表面]
```

可复用 renderer 继续使用当前 `art3m1s-render` 的算法与后端。为保护其他引擎，第一步
是新增 Artemis Host adapter/契约桥，而非重写其共享 `GpuBackend` 或删除内置 E-Mote。

## 3. 对外 API 形态

建议唯一新查询入口为 `art3m1s_get_api_v2`，无需 GPU feature 即可存在。旧 v1 查询入口
仅在 legacy 构建中保持，不能把它静默改指向 v2。v2 根表不装载其他引擎。

| 表 | 字段 | 用途 |
|---|---|---|
| Root | `query_interface` | 按 interface ID、major 和最小前缀大小获取表 |
| Root | `copy_last_error` | 即使 create/query 失败也可读取当前线程 UTF-8 错误 |
| Runtime（必选） | `create` / `destroy` | 纯逻辑 runtime；分配/回收代际句柄 |
| Runtime | `open_project` | 提交原始 INI 字节、INI 分节、可选 reported OS，开始 Loading |
| Runtime | `step` | 推进一次 Host frame，恢复可执行 continuation，生成事件和最新帧 |
| Runtime | `submit` | 批量提交输入、Host 状态、配置及带身份的服务结果 |
| Runtime | `poll_events` | 批量读取完整可靠事件和可丢诊断记录 |
| Runtime | `get_state` | 获取会话、舞台尺寸、等待、退出、积压和丢弃计数 |
| Frame（按能力查询） | `acquire_latest` / `release` | 获取/释放不可变渲染快照，无平台对象 |

初始合计 11 个函数指针，分布在三张表。这个数字不作为重构成功标准；消息 schema 仍是
公开 API 的一部分。禁止用任意字符串 `invoke(method, json)` 把不受约束的私有 RPC 塞进
`submit`；每个 domain/kind 都必须有版本、字段、失败分支和一致性测试。

标题探测使用 headless/probe runtime 和相同 IO 协议，在第一条 caption 或预算耗尽时结束，
不创建 renderer。完整游戏的最低 Host 组合是 runtime + IO + renderer + media 完成协议；
不支持 UI/网络等可选服务时按已定义失败分支恢复。

### ABI 演进与类型

- 表头为 magic、struct_size、interface_id、abi_major、abi_minor；同 major 只允许尾部追加。
  Host 校验 major 和所使用的最小前缀，不能要求未来表与自己精确等长。
- C ABI 使用目标平台原生指针和 C 对齐，不使用 packed struct。32/64 位绑定分别生成并核对
  sizeof/offsetof。API 表是本进程 ABI，不是网络格式。
- 状态/枚举固定 `int32_t`/`uint32_t`，句柄/ID/长度用 `uint64_t`；长度转为本机索引前
  检查 `SIZE_MAX` 和各自预算，乘法检查溢出。布尔为明确定义的整数。
- 字符串统一 UTF-8 指针+字节数，无终止 NUL；原始 INI/脚本字节保留其引擎编码。
  长度非零时指针不可空，长度为零时可空。浮点参数必须有限并按字段约束验证。
- 结构均带 struct_size，保留字段写零；未知可选尾字段忽略，未知必选 feature/schema 拒绝。
  表内 required 字段不可空；缺少整个可选接口时 query 返回 UNSUPPORTED。
- 所有输入和输出结构的 struct_size 由 Host 在调用前设置；core 只读写协商前缀内的字段。
  失败时将普通 out 值清零，不改 Host 的 struct_size；BUFFER 的所需容量是明确例外。
- 只承诺 C calling convention。Rust 使用独立 `api` facade，不能把 trait/vtable 或
  `Vec`/`String`/`HashMap` 直接交给 C 或按其布局序列化。

### 返回值

所有函数返回同一 `ArtStatus`：0=OK，1=NO_DATA，2=WOULD_BLOCK，负数为错误。
数据和写入数量走 out 参数，禁止“负容量”“字节数既是成功又是错误”等混用。
错误至少区分 ARGUMENT、HANDLE、THREAD、UNSUPPORTED、BUFFER、STATE、STALE、LIMIT、PANIC。

BUFFER 返回所需大小且不消费队首记录。NO_DATA 表示没有新数据。WOULD_BLOCK 表示本次
操作未被接受，可在排空队列/释放 lease 后重试，不表示应重复推进同一 frame。
异步服务失败（not_found/denied/io_error/cancelled 等）走对应结果消息，不冒充 ABI 调用失败。
poll 若至少写出一条完整记录则返回 OK，并在 out 标出下一条所需空间；仅首条装不下时
返回 BUFFER。输出参数清零不得造成已交付记录被隐藏；NO_DATA/BUFFER 都不移除未交付记录。

错误文本线程局部，`copy_last_error` 不覆盖被读取的错误；失败的 create/query 也能报告。
成功调用不要求清空旧文本，只有错误 status 才说明该文本属于本次调用。
可捕获的 panic 统一转为 PANIC 并使相关 runtime Faulted，避免在半完成状态继续推进；
这不承诺恢复 OOM、abort 或非法调用者内存。

## 4. 会话、线程和生命周期

运行时状态为 Created → Loading → Running → Closing → Closed；不可恢复错误进入 Faulted。
IO/UI 等等待另用 wait_flags/pending_requests 表示，不把全局状态改为“整个引擎暂停”。
Loading 完成必须有事件和状态转换；`open_project` 返回 OK 只表示已受理。

- 句柄 0 无效，registry 记录对象类型、代际和 owner thread。跨类型、陈旧或重复 destroy
  返回 HANDLE；错误线程返回 THREAD。不同 runtime 不能因独立表碰巧编码相同而互认。
- create/open/submit/step/poll/state/frame/destroy 统一在创建它的 owner 线程执行。
  GUI、IO、媒体线程把结果投递给 Host 自己的 owner 队列；C ABI 不接受跨线程直调。
- 每次成功受理 open/reload 增加 session_epoch，取消旧请求并丢弃旧会话未交付控制事件。
  首先交付新 epoch 的 session_changed，Host 据此取消旧 IO/UI/媒体任务并停止呈现旧帧。
  已借出的旧帧字节继续有效到 release；尚有 lease 时允许 reload，但 destroy 返回 STATE。
- `request_id` 在同一 runtime 生命周期内单调分配且不重用；计数耗尽返回 LIMIT。
  结果按 `(runtime, epoch, request_id, service kind)` 校验；重复/过期结果返回 STALE，无副作用。
- 收到退出请求后 Host 提交 graceful shutdown。core 发出必要的最终存档/媒体停止请求；
  Host 执行并回填，直至 Closed 后 destroy。Faulted 可强制销毁，仍须先释放所有帧。
  强制退出可以放弃未确认存档，不能把它记录为成功持久化。
- 所有 debug、字体、窗口状态、文本表、surface/资源 CPU 缓存和诊断计数归 runtime。
  进程级库初始化日志单独交给 SDK 诊断，不能偷偷路由到最近运行的游戏。

`step` 接收 Host 的 frame ID、单调时间、UTC 时间及时区，首次只锚定单调时钟。
相同 frame ID、相同单调时间可用于 IO 回填后继续本帧，不能再次消费输入边沿、触发
onEnterFrame 或累计动画 delta；更小 ID/时间返回 ARGUMENT。后一 Host frame 的时间差
只累计一次。后台冻结通过 Host 明确的 pause/resume 策略处理，不任意截断 E-Mote 时间。
日历 API 可使用更新的 UTC，逻辑时间不得由日历时钟倒退影响；诊断计时与游戏时钟分离。

## 5. 消息、可靠性与能力协商

`submit` 原子受理一个批次并复制其 metadata/blob；返回后 Host 可释放输入。语义校验、
容量和身份检查失败时整批不受理。受理不直接运行 Lua；只在 `step` 的确定性边界执行。
输入、状态和结果在批内按顺序处理。初始建议上限为每批 4096 条、单条 blob 1 MiB，
大文件必须分块，最终限制作为可查询能力冻结。

`poll_events` 按以下固定小端格式写入完整记录，不直接 memcpy C struct：

| 偏移 | 类型 | 字段 |
|---|---|---|
| 0 | u32 | record_bytes，含头和 body |
| 4 | u16 | envelope_version，初值 2 |
| 6 | u16 | domain |
| 8 | u32 | kind，domain 内稳定编号 |
| 12 | u32 | flags，含 requires_reply / critical / body 格式 |
| 16 | u64 | runtime_handle |
| 24 | u64 | session_epoch |
| 32 | u64 | sequence，runtime 生命周期单调递增 |
| 40 | u64 | request_id；无回应通知为 0 |
| 48 | u32 | body_bytes |
| 52 | u32 | reserved=0 |

头长 56，`record_bytes = 56 + body_bytes`。控制记录 body 是小端
`metadata_bytes:u32, blob_bytes:u32, UTF-8 JSON metadata, raw blob`，两段长度相加加 8
必须等于 body_bytes；无 NUL/隐含 padding。高频 input/render 可以使用独立版本化二进制
body，由 flags/kind 唯一标识。JSON 中的 u64/i64 使用十进制字符串，避免宿主 Number 精度丢失。
禁止把图片/存档/HTTP body 转成 JSON 数组或 base64 作为常规传输。

控制队列与诊断队列各有条数和总字节预算。初始建议为控制 4096 条/16 MiB、诊断
1024 条/1 MiB；实际配置和上限需可查询。请求和媒体控制消息可靠，日志可丢并计数。
满载时必须在产生副作用前预留控制记录，或挂起尚未提交的内部操作；不能先推进脚本再
丢请求。即便游戏等待 Host，也要允许 poll 和结果回填；关键请求不能被日志阻塞。

可靠控制按 sequence 交付，诊断可分批穿插；Host 不得等待 sequence 连续，因为日志
允许丢失。新帧可以合并，但影响转场或存档的资源版本不能随着“丢中间帧”丢掉。
未知可选通知可忽略；未知 requires_reply 请求必须回应 unsupported，并有可记录的终止路径。

能力分成两侧：Host 声明 IO/UI/media/renderer 能力；core 报告支持的协议版本和引擎特性。
缺少可选服务按规范失败完成；缺少游戏启动所需的 IO/渲染能力时明确拒绝启动。
不用 nullable 函数槽猜测所有服务能力，也不静默自动选择另一个 GPU backend。

## 6. IO：Host 实现、core 保留语义

所有 IO 通过请求与结果消息完成；core 的 VFS/cache 只保存逻辑资源和字节，不保存
`PathBuf`、File 或本机存档根。Host 可调用独立 PFS SDK、内存包、原生文件系统或远程服务。

| 操作 | 请求 | 完成及约束 |
|---|---|---|
| `io.stat` | namespace、key | found、size、revision、可选 UTC mtime；空文件与不存在有区别 |
| `io.read` | namespace、key、revision、offset、length | 原始 blob、实际长度、EOF；跨 revision 拒绝或明确重试 |
| `io.list` | namespace、prefix、cursor | 一页逻辑条目、revision、下一 cursor；目录枚举不暴露物理路径 |
| `io.write_atomic` | save key、transaction ID、分块描述和 blob | Host 接受全部数据并原子提交后确认；受理不等于落盘 |
| `io.delete` | save key | 明确 deleted/not_found/denied/io_error |
| `io.cancel` | 原 request ID | 取消尽力而为；之后到达的旧完成消息仍按 STALE 处理 |

namespace 至少区分 asset 与 save；逻辑 key 使用 `/`，拒绝绝对路径、驱动器、NUL 和 `..`。
Artemis 的大小写兼容查找、归档覆盖顺序、别名和 savepath 需要独立 conformance 样例；
不能直接照 OS 大小写规则查找，也不能因重构改变已有 patch/sidecar 优先级。
脚本 savepath 是逻辑子目录，实际游戏隔离根由 Host 决定，禁止两侧重复拼接前缀。

Host 可预提交完整资源元数据索引以同步回答 exists/stat；未缓存的结果表示 Pending，
不表示不存在。热路径使用 CPU 缓存；大文件分块，所有块都校验资源 revision。
写入成功后才更新可见版本；缓存的 save 内容与宿主存储版本必须保持 read-your-writes。
Host 的 unsupported/permission/error 必须能进入脚本原有失败分支，不允许无限等待。

**同步 Lua 是必须先解决的实现门槛：**当前同步 EngineCallbacks 需要改成可保留调用现场的
资源依赖/continuation，或在进入该调用前完整准备它所需数据。索引预载只能解决已知查询，
不能宣称覆盖任意动态文件名、写入确认和 Lua 中间加载。统一 Luau 后必须验证能从
原位置恢复且不重复前面的副作用。若任一生产路径不能安全挂起，先保留 v1，不发布不完整 v2；
也不偷偷恢复磁盘回退、阻塞 GUI 的桥接或反向回调。

最小语义用例：Lua 先将计数加 1，再读取尚未加载的动态资源，恢复后仍只加 1；写存档
失败进入失败分支；IO 等待时 onEnterFrame/音频等待和输入边沿仍遵守原引擎行为。

## 7. 画面：core 帧契约与 Host renderer

core 输出 `FramePacket`，Host 管理 GPU、编译执行 shader、上传、同步和呈现。现有
`DrawList` 是 Rust 实现契约，不能直接作为跨语言 ABI。先做可复用 Host renderer SDK，
其他 Host 可以使用 SDK，也可以实现同一渲染协议。

帧契约至少必须覆盖：

- 舞台大小、逻辑 frame ID、资源 revision、dirty/damage，坐标原点左上、x 右/y 下；
  Host 负责 viewport、DPI 和输入反向转换，不能修改脚本图层身份。
- sprite/mesh、UV、完整变换、颜色/透明度、混合和采样模式、clip、mask/stencil、
  shader group、render pass 依赖及临时 target，不能只导出矩形列表。
- 文本 glyph 数据、Ruby、E-Mote mesh/material/stencil 语义；图层 ID 以原始字符串保留，
  不把 `1.80` 转成浮点。CPU 命中测试所需 alpha/尺寸由 core CPU 资源层维护。
- 纹理逻辑 ID + revision + 像素/编码描述，不含 GL 名、Metal/Vulkan 指针、IOSurface
  或窗口句柄。Host 对象始终留在 renderer 内；核心最多引用协议里的 opaque resource ID。
- 明确颜色空间、alpha（每个资源的 straight/premultiplied 标志）、纹理原点、行跨度和格式；
  Host SDK 用回归像素证明与旧后端一致，不能在草案阶段随意统一而改变游戏画面。
- 转场旧帧捕获、滤镜/HLSL、lyedit、存档截图的准确时点与资源保活。捕获用逻辑 target
  和显式完成消息；core 不再自己调用 GPU readback，脚本等待依赖按 request ID 恢复。
- 视频图层绑定播放实例/resource revision，由 Host 解码并在 renderer 侧更新图像。
  全屏视频仍由 Host 管理；时间、结束和失败回报进入同一媒体协议。

FramePacket 必须采用有版本的二进制 schema，所有偏移以 packet 起点计算、做范围/溢出检查；
不能包含 Rust 内存地址。schema 完整定义、资源数据组织和 opcode 编号是 P0/P3 冻结项。
不能因为 C 骨架可编译就宣称任意 Host 已能绘制。

`acquire_latest(after_render_frame_id)` 返回不可变快照和 lease token；无新帧为 NO_DATA。
快照及所有引用的 CPU 资源版本在 release 前跨 step/reload 保持有效；默认至多两个
外借 lease，超过返回 WOULD_BLOCK。CPU 不可变块可以共享，不能每帧复制整个资源库。
Host 完成 CPU 读取/上传暂存后可 release，GPU 对象由 Host 按自身 fence 保活，不要求
core 理解原生 fence。纹理销毁由 Host 在 GPU 引用结束后执行。

逻辑 tick 与出帧独立：Host 每个 frame 调一次 step，无新画面时继续执行 onEnterFrame。
新快照可合并，但仍被外借的帧数据不能被改写。转场源 target/版本在后续帧引用期间保留，
必须显式结束其逻辑生命周期。GPU 设备丢失由 Host 重建，并从完整资源版本快照重放，
无需重跑剧情或重建游戏变量。

## 8. 媒体、UI、配置与诊断

媒体命令保留当前 BGM/SE/voice 的增益、pan、fade、交叉淡入、A/B 循环段、skippable
语义，不把 gain=1000 当作百分比。每次播放分配不可复用的 playback_id，脚本通道 ID
可以重复。Host 回报 started/ended/failed/cancelled 和可选播放时钟；旧播放完成不得停止
新声音。视频音轨由 Host 解码，不经 core 临时 WAV 路径。

HTTP/dialog/翻译各有请求 kind 和结果 schema。HTTP 保留 headers/data/file_data 的
现有多值顺序语义，响应必须匹配请求 ID；取消、失败、超时均完成一次。翻译继续允许非阻塞
热替换，过期页面/文本 revision 的结果不再应用。callnative、浏览器、剪贴板等行为由 Host
决定支持情况，未实现时必须有明确的反馈规则。

窗口状态、字体列表/字体 blob、文本替换、reported OS、音量等作为 runtime 配置消息。
INI 分节选择与 reported OS 保持独立。默认值和缺失/null/空串的区别必须写进 schema。
渲染质量、backend、原生 surface、显示分辨率、升频和截图文件保存归 Host renderer SDK，
core 只关心影响引擎语义的舞台/资源尺寸和捕获结果。

诊断保留可选 profiler；core 测逻辑/构建，Host 测 IO、解码、上传、GPU/present，按 frame ID
关联，不重复求和或把 CPU submit 当 GPU 时间。日志可丢但丢弃数可查，初始化错误可直接读取。

## 9. Rust 公开面与 SDK

目标 Rust 稳定入口仅包括 `api::{Runtime, RuntimeOptions, ProjectInfo, InputBatch,
HostMessage, RuntimeEvent, FrameSnapshot, Error}` 和版本化协议类型。内部 compositor、
Lua 对象、GPU backend 和缓存不作为稳定 API 承诺；第三方依赖不再从新 facade 整包重导出。
Rust API 和 C facade 使用同一套消息/生命周期实现，不能维护两种引擎行为。

在 v1 兼容期，不直接删掉现有 pub mod/re-export。先记录内部调用者、tests/probes 和真实
Rust 消费者，再放入明确 legacy 层或在一次标明破坏性版本中收窄。跨 crate 移动不得改变
脚本语义、存档格式和内置 E-Mote 后端。

SDK 产物应包含唯一 schema、可编译 C 头、生成的 Dart/Swift/C# 绑定，以及一个不依赖
Flutter 的最小 Host 样例。平台 IO/render/media 适配器可以复用，但不链接进纯核心。
不用在每种语言里手写第二份函数顺序、长度类型和 magic。

## 10. 分阶段实施与退出条件

| 阶段 | 工作和可评审产物 | 通过条件 |
|---|---|---|
| P0：标准与基线 | 落实已确认画面边界；冻结 v1 inventory；确定状态、消息 schema、kind/能力位、错误、所有权；建立独立 C 调用样例和 schema 生成源 | 所有 59 槽与 26 个未入表函数有去向；输入/媒体/IO/渲染格式无未定义关键项；v1 不动 |
| P1：runtime 会话化 | 显式 RuntimeContext，移除 Artemis 对 ACTIVE/default/global font/debug/cache 的依赖；注入时钟；给 v2 facade 加代际句柄与统一错误 | 两 runtime 的不同资源/字体/窗口/请求交错不串流；reload 后旧完成无效；旧 v1 回归通过 |
| P2：Host IO | 先做 Luau continuation 验证；重建纯内存资源/cache 与异步 IO 状态机；Host SDK 承担目录/PFS/save | 动态资源 miss 不重跑 Lua；写入失败/取消/短读/空文件可恢复；无磁盘回退；旧存档兼容 |
| P3：Host renderer | core CPU 资源与 render plan 分离；新增帧 schema 与 Host adapter，复用现有 renderer；迁出 surface/present/媒体临时文件 | 无 GPU 创建也能启动逻辑/probe；帧 lease/设备重建可靠；文字/E-Mote/转场/HLSL/截图像素与行为对照通过 |
| P4：新 Host 验证 | 完成生成绑定和独立最小 Host，再迁移 Flutter 的 Artemis adapter；IO/render/media 分别可替换 | 第二种 Host 不依赖 Flutter 或 v1 资源挂载；完整帧循环和媒体完成协议运行；v1/v2 可分别构建 |
| P5：收口 | v2 默认纯核心；v1 平台依赖移到独立 legacy 包/构建产物；清理弃用 pub API 和内部 C 壳 | core 依赖图无原生 GPU/物理 IO adapter；符号 allowlist 与绑定 CI 固定；其他引擎特征构建无行为变化 |

P0/P1 可以先在现有 crate 新增内部层，P2/P3 验证期间不更改默认生产路径。
最终 core 与 legacy 的独立产物边界必须落实，否则只加一个 `core-only` feature 无法保证
目标依赖不会重新拉入 Metal/Vulkan。其他引擎继续使用原有聚合/legacy 构建；不以本轮
v2 纯核心目标为由更改它们的功能或强迫上游迁移。

v1 adapter 可以复用逐步拆出的内部逻辑，但继续暴露原语义。**不能**把同步 v1 调用简单
转发给异步 v2 后返回“成功”；在无法保持原同步时序时，兼容 facade 必须保留独立实现，
直到 Host 迁移。删除旧代码前逐项证明没有内部/Rust 调用者。

### 每阶段需要的验证

1. ABI：C/C++ 编译、sizeof/offsetof、生成绑定匹配、大小前缀协商、未知能力与空指针、
   buffer 重试、错误文本；对新产物执行 Mach-O/ELF/PE 动态符号 allowlist。
2. 会话和协议：两个实例交错、wrong thread、假/旧/错类型 handle、重复销毁、reload epoch、
   取消后迟到结果、队列总字节预算/溢出、控制事件不丢，诊断丢弃可见。
3. 时间和输入：同帧 IO resume 不重复输入或 onEnterFrame；跳过渲染不跳逻辑；后台恢复、
   日历变化、按键重复、touch/鼠标顺序；保留字符串图层 ID。
4. 业务：存档/系统存档/已读/截图回归，HTTP/dialog/翻译，BGM/SE/voice/视频完成与 A/B 循环，
   E-Mote 同 Host frame 批处理和 Advance 求值；统一 Luau 后端验证。
5. 画面与性能：同素材/脚本/输入比较文字、clip/mask、混合、转场、lyedit、HLSL 和 E-Mote；
   对比 tick/构帧/拷贝字节数/CPU/GPU 内存，不能仅以函数数量减少或编译通过认定成功。
6. 平台边界：无文件系统/无 renderer 的内存 Host 跑 probe 与逻辑；依赖图、Lua 标准库和
   源码检查覆盖间接 IO；真实平台/游戏使用外部 fixture，路径不写进仓库。

生产逻辑变更按 `AGENT.md` 执行 `cargo check --all-features`、`cargo fmt --check` 和
`./scripts/test-all.sh`；图形变更补独立 CGL 与实际 Host 测试，各子 crate 和 Lua 后端
不能由顶层一次 cargo test 代替。仅文档/头文件草案阶段做静态一致性和 C/C++ 语法检查，
不把它们描述为已完成运行时验证。

## 11. 实施记录

2026-11-17，首批基础改造：

- 所有平台的 asb-interpreter 固定使用 Luau，移除可选择的标准 Lua feature 和目标平台分叉。
  mlua 的 Luau 源码自动随库构建，不再启用额外的 vendored Lua/LuaJIT 源码依赖。
- Luau 的 Host 文件兼容层支持读、写、追加、更新、seek、flush、lines，均使用解释器的
  Host 资源读写入口；不回退标准 Lua 的本机 io 库。
- 新增同步 Rust `host::ResourceProvider`；`HostResources::from_provider` 允许 Host
  实现资源和持久化，保留短读、空文件、失败和实例隔离。此接口是过渡层，不是 v2 异步协议。
- 新增 `CoreRuntime::with_host_backend`，显式注入资源与 Host renderer，不选择原生后端或
  默认资源，不允许工作目录读取回退。原 create/v1 路径保留兼容行为。
- 项目初始化读取 tag.ini 时使用该 runtime 的资源；surface 绑定缓存改为 runtime 所有，
  销毁一个实例不会清空另一个实例的 surface 引用。

尚未完成：v2 C 查询入口与可发布 schema、全部全局配置/查询快照的会话化、Luau IO
continuation、纯核心依赖拆分以及独立 FramePacket/Host renderer SDK。不要把本阶段的
Rust 注入入口标记为 v2 ABI 已完成，或将 P1–P5 整体标记为通过。
