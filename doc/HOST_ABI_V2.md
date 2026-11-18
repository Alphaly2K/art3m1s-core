# Artemis Host ABI v2：接口契约（草案）

状态：v2 C 预览使用 `cargo build --locked --manifest-path crates/artemis-core/Cargo.toml`。
独立预览发布 C 动态库；仓库根目录的构建继续用于 v1 和现有 Rust 调用者。导出审计见
[EXPORT_AUDIT.md](EXPORT_AUDIT.md)，预览头文件见
[abi/art3m1s_core_v2.draft.h](abi/art3m1s_core_v2.draft.h)。

## 1. 范围与设计决策

只重构 Artemis/Art3m1s 的公开接口及其内部 Host 边界。RFVP、KRKR、Siglus 保持现有
独立 ABI、构建和适配，不纳入统一 runtime，不修改它们的上游补丁。共同依赖 crate 的
破坏性调整必须另行评估；本方案不能用一次共享接口替换迫使其他引擎一起迁移。

**core 生成渲染数据，Host 绘制并呈现**。core 保留引擎语义
与 CPU 侧计算，Host 执行物理 IO、GPU 绘制、音视频解码/输出和最终呈现。Host 拥有
renderer，core 不保留原生 GPU 合成与 present 的生产路径；现有 renderer 以 Host SDK 复用。

其余建议决策：

1. 新增独立 v2，冻结 v1 布局、返回值和 feature 语义；禁止原地把 v1 指针参数改成整数。
2. 一个动态查询符号、小型 runtime 表、独立 frame 表，能力与消息 schema 分开版本化。
3. 继续使用 Host 调 core、core 排队、Host 拉取；不恢复 native→Dart 或其他语言反向回调。
4. 所有游戏状态、输出和请求按 runtime 隔离；会话重载增加 epoch。
5. 统一 status、代际句柄、明确所有权；不公开 Rust 容器、trait、DrawList 内存布局或平台指针。
6. 先完成 IO continuation 与帧契约验证，再冻结 v2 的消息编号、capability 位和绑定。

发布的 ABI 必须定义完整的消息 kind、能力位、渲染 opcode 和资源格式，并提供对应的
Host 一致性验证。预览版只承诺本页列出的已实现部分，协议编号尚待冻结。

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
| Frame（按能力查询） | `acquire_latest` / `release` / `texture_rgba` | 获取不可变渲染快照和像素，并释放借用 |

预览版合计 12 个函数指针，分布在三张表。消息 schema 仍是
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
- `ArtMessage` 是固定步幅数组，每项 `struct_size` 等于 `sizeof(ArtMessage)`。`ArtBytes`
  使用固定布局。输入、输出缓冲区与帧借用不能重叠。
- 只承诺 C calling convention。Rust 使用独立 `api` facade，不能把 trait/vtable 或
  `Vec`/`String`/`HashMap` 直接交给 C 或按其布局序列化。

### 返回值

所有函数返回同一 `ArtStatus`：0=OK，1=NO_DATA，2=WOULD_BLOCK，负数为错误。
数据和写入数量走 out 参数，禁止“负容量”“字节数既是成功又是错误”等混用。
错误至少区分 ARGUMENT、HANDLE、THREAD、UNSUPPORTED、BUFFER、STATE、STALE、LIMIT、PANIC。

BUFFER 返回所需大小且不消费队首记录。NO_DATA 表示没有新数据。WOULD_BLOCK 表示本次
操作暂未完成，可在排空队列、消费结果、完成请求或释放借用后重试。`step` 返回
WOULD_BLOCK 时仍填充状态；`out.host_frame_id` 是最近受理的帧。若等于输入 ID，以同一 ID
和单调时间重试；若较小，先完成前一帧。
异步服务失败（not_found/denied/io_error/cancelled 等）走对应结果消息，不冒充 ABI 调用失败。
poll 若至少写出一条完整记录则返回 OK，并在 out 标出下一条所需空间；仅首条装不下时
返回 BUFFER。输出参数清零不得造成已交付记录被隐藏；NO_DATA/BUFFER 都不移除未交付记录。

错误文本线程局部，`copy_last_error` 不覆盖被读取的错误；失败的 create/query 也能报告。
成功调用不要求清空旧文本，只有错误 status 才说明该文本属于本次调用。
可捕获的 panic 统一转为 PANIC 并使相关 runtime Faulted，避免在半完成状态继续推进；
这不承诺恢复 OOM、abort 或非法调用者内存。

## 4. 会话、线程和生命周期

运行时状态为 Created → Loading → Running → Closing → Closed；不可恢复错误进入 Faulted。
IO/UI/截图等等待另用 wait_flags/pending_requests 表示，不把全局状态改为“整个引擎暂停”。
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
- Rust 运行时的 `submit_shutdown(handle, epoch)` 在 Running 阶段受理。游戏暂停在退出点；
  尚有服务结果待处理时先完成它并推进 `step`。受理后状态进入 Closing，不再运行场景，按顺序
  确认全局数据、系统设置和已变化的已读记录。`runtime_closed` 排入队列后才进入 Closed，期间
  拒绝重载。Host 在销毁前拉取关闭结果。
- 所有 debug、字体、窗口状态、文本表、surface/资源 CPU 缓存和诊断计数归 runtime。
  进程级库初始化日志单独交给 SDK 诊断，不能偷偷路由到最近运行的游戏。

`step` 接收 Host 的 frame ID、单调时间、UTC 时间及时区，首次只锚定单调时钟。
C 入口的帧 ID 从非零开始；状态字段中的零表示尚未受理帧。
相同 frame ID、相同单调时间可用于 IO 回填后继续本帧，不能再次消费输入边沿、触发
onEnterFrame 或累计动画 delta；更小 ID/时间返回 ARGUMENT。后一 Host frame 的时间差
只累计一次。后台冻结通过 Host 明确的 pause/resume 策略处理，不任意截断 E-Mote 时间。
日历 API 可使用更新的 UTC，逻辑时间不得由日历时钟倒退影响；诊断计时与游戏时钟分离。
日历时区使用 Host 提交的 UTC 分钟偏移（-1440..1440），core 不读取本机时区。
e:now、os.clock 和等待截止时间使用逻辑单调时间；os.time/date、date/unixtime 变量
及文件修改时间格式使用日历快照，os.date 的 `!` 前缀使用 UTC。
随机状态由每个 VM 持有，Host 可提供种子；变量快照恢复不替换当前环境服务。

当前 Rust 场景驱动按帧 ID 串行运行逻辑：若前一帧仍有 IO continuation，新帧返回
WOULD_BLOCK 且不受理。Host 先回填并恢复原帧，再提交新帧；结果提交本身不执行回调。
帧 ID 只在同一会话 epoch 之内比较。点击、按键和定时等待最早从到达等待的下一帧解除。
`[@]`、`[wt0]` 使用 `[keyconfig role=0]` 的前进键（默认左键、Enter、滚轮下）；
`[exkey]` 可指定数字键码。
`[wt]` 沿用定时等待语义，默认时长为零。
`[wait time=… input=1]` 也可由其他按下边沿解除；`input=2` 还允许快进解除；`input=0`
只看单调时间。UTC 变化和同帧重入不影响期限。

Core 按 `[skip]`、`[automode]` 和 `[keyconfig]` 维护播放状态，Host 只需提交输入和媒体状态。
普通快进默认在首次出现的正文处停止；按住临时快进键仍可继续。自动播放须等正文显示完成和
指定声音结束（未指定则等待语音），再计入 `s.automodewait`。用于退出自动播放的前进边沿
不会推进当前等待。

Rust 句柄入口的 `submit_input(handle, epoch, events)` 原子受理输入，并在下一次受理新帧时
按提交顺序应用。等待 IO 的原帧继续使用原输入；重载清空输入状态及积压。键码取 1–255，
鼠标键与滚轮也用该键码空间。指针与触点使用舞台坐标，可位于舞台外；同时可查询最多 16 个
触点。失焦清空按键和触点，保留指针位置。待应用的输入最多 4096 条。按住重复由逻辑时钟
驱动，`isPush` 从按下起满 500 毫秒后在后续帧返回 true，等待只消费新的按下边沿。

`onEnterFrame` 可用 `overrideKey` 改写当前帧的脚本输入。该回调结束后，Core 固定本帧用于
派发的输入状态；IO 回填不重新采样。合成的 Decide 可以触发前进角色，要求物理按键的等待
仍需要 Host 提交的边沿。

Core 在指针移动或左键边沿时用舞台坐标判断 `[lyevent]` 的 `rollover`、`rollout` 和 `click`，
使用图层变换、可见性及 `clickablethreshold`。失焦撤销当前悬停，直到 Host 提交新指针坐标。
Core 根据 `penetration` 从上往下选择处理器，选中的处理器按从下往上执行。Luau 事件过滤器可
通过可靠 IO 暂停并恢复本帧；过滤器返回已处理，或默认处理器成功排队时，本次左键不再推进故事。

`[setonpush]` 可注册全局按键处理器；启用 `keyrepeat=1` 后，按住满 500 毫秒会在后续帧重复派发。
派发成功会阻止该键的默认角色，过滤器拒绝派发时仍可使用默认角色。自动播放、普通快进和临时
快进的进入与退出回调使用相同的事件过滤机制，且只在状态切换时触发。

Core 根据舞台坐标捕获可拖动图层，并在 `dragarea` 内更新位置；脚本也可通过 `[lydrag]`
指定捕获图层。物理左键松开或失焦会结束捕获，脚本对按键状态的覆盖不影响释放。文字链接按
消息层的变换和可见性做命中测试，命中有效跳转时消费该次左键。

隐藏消息窗时，恢复显示的左键、右键或 Esc 不推进剧情。存档保留进入隐藏模式前的图层可见性。
启用 `[rclick]` 后，Core 通过 Host IO 加载右键脚本，关闭菜单时恢复原等待。历史记录按键触发
脚本注册的 `backlogin` 或 `backlogout`，Host 无需维护菜单状态。

## 5. 消息、可靠性与能力协商

`submit` 原子受理一个批次并复制其 metadata/blob；返回后 Host 可释放输入。语义校验、
容量和身份检查失败时整批不受理。受理不直接运行 Lua；只在 `step` 的确定性边界执行。
输入、状态和结果在批内按顺序处理。初始建议上限为每批 4096 条、单条 blob 1 MiB，
大文件必须分块，最终限制作为可查询能力冻结。

C 预览版 `ArtMessage.schema_version=1`、`flags=0`。一个批次可包含一组 IO/UI/Render 回复、一组输入，
或单条控制消息。回复沿用原请求的 domain、kind、epoch 和 request_id。输入和控制的
`request_id=0`，metadata 为 UTF-8 JSON object；下表未列出 blob 的消息不携带 blob。

| domain | kind | metadata / blob |
|---|---|---|
| Input (2) | 1 | `code`、`down`（布尔） |
| Input (2) | 2 | 舞台坐标 `x`、`y` |
| Input (2) | 3 | `id`、`phase`（`down`、`move`、`up`）、`x`、`y` |
| Input (2) | 4 | `{}`，失焦 |
| Input (2) | 5 | `button`：0 关闭，1 最大化，2 最小化 |
| Runtime (1) | 16 | `{}`，请求正常关闭 |
| Runtime (1) | 17 | `{}`，blob 为默认字体 |
| Media (5) | 16 | `playback_id`、`status`（`started`、`ended`、`failed`、`cancelled`） |
| Media (5) | 17 | `playback_id`、`frame_id`、`width`、`height`；blob 为 RGBA8 |

`ArtCreateInfo` 的 Host 能力位分别为 IO=1、UI=2、Media=4。Renderer 位分别为 sprite=1、
transition=2、screenshot=4、group=8、emote=16、HLSL=32。
预览版检查未知位；Host 仍须处理收到的服务请求，缺少可选服务时返回相应失败。

`poll_events` 按以下固定小端格式写入完整记录，不直接 memcpy C struct：

| 偏移 | 类型 | 字段 |
|---|---|---|
| 0 | u32 | record_bytes，含头和 body |
| 4 | u16 | envelope_version，初值 2 |
| 6 | u16 | domain |
| 8 | u32 | kind，domain 内稳定编号 |
| 12 | u32 | flags：0x1 requires_reply，0x2 critical，0x100 JSON/blob body |
| 16 | u64 | runtime_handle |
| 24 | u64 | session_epoch |
| 32 | u64 | sequence，runtime 生命周期单调递增 |
| 40 | u64 | request_id；无回应通知为 0 |
| 48 | u32 | body_bytes |
| 52 | u32 | reserved=0 |

头长 56，`record_bytes = 56 + body_bytes`。控制记录 body 是小端
`metadata_bytes:u32, blob_bytes:u32, UTF-8 JSON metadata, raw blob`，两段长度相加加 8
必须等于 body_bytes；metadata 必须为 JSON object，无 NUL/隐含 padding。诊断使用相同 body。
Runtime domain=1、kind=1 为 `session_changed`，request_id=0，metadata=`{}`，blob 为空。
Runtime kind=2 为 `project_ready`，字段同上；BOOT 与加载期 Lua 完成后交付。记录排入
可靠队列时状态由 Loading 转为 Running，且排在首条场景输出之前。
Runtime kind=3 为 `exit_requested`，字段同 `session_changed`。
Runtime kind=4 为 `runtime_closed`，request_id=0、blob 为空；metadata 为
`{"save_status":"written"}` 或 `{"save_status":"failed","reason":"…","files_written":1}`。
`files_written` 是本次关闭期间已经收到持久化确认的文件数。
Runtime kind=5 为 `persistence_result`，request_id=0、blob 为空。`operation` 为 `save`
或 `load`；`status` 为 `written`、`restored` 或 `failed`。失败包含 `reason`；保存失败
还包含 `files_written`，表示此前已确认的文件数。结果先进入可靠队列，再继续场景。
服务请求只使用 IO/UI/media/render domain；其余 service kind 和 payload 由各自 schema 定义。
高频 input/render 可以使用独立版本化二进制
body，由 flags/kind 唯一标识。JSON 中的 u64/i64 使用十进制字符串，避免宿主 Number 精度丢失。
禁止把图片/存档/HTTP body 转成 JSON 数组或 base64 作为常规传输。

UI domain=4 的记录不携带 blob：

| kind | 请求或通知 | 结果 |
|---|---|---|
| 1 `ui.caption` | 可靠通知，`{"text":"…"}` | 无回复 |
| 2 `ui.dialog` | 请求，`title`、`message` 为字符串，`allow_cancel` 为布尔值，`initial_text` 为字符串或 null，`max_characters` 为十进制字符串或 null | `{"status":"ok","accepted":true,"text":"…"}`；`text` 可省略。`cancelled`、`denied`、`unsupported`、`io_error` 按取消完成 |
| 3 `ui.window_action` | 可靠通知，`command` 为 `fullscreen`（切换全屏）或 `minimize` | 无回复 |
| 4 `ui.avoid` | 可靠通知，`visible`、`mute` 为布尔值；开启时包括 asset 命名空间下的逻辑 `asset` 和 `windowbutton` | 无回复 |

Host 按同一条 `ui.avoid` 记录更新遮罩和静音；解除静音时沿用原音量。`windowbutton=0` 忽略
窗口按钮，`1` 使用平台默认动作，`2` 把按钮提交给 Core。Core 先交付退出回避通知，再派发
`setonwindowbutton`。会话重载或关闭后，Host 清除遮罩和会话静音状态。

`initial_text=null` 表示无输入框。Core 在 `step` 应用结果，接受时按约定字符上限写入
输入变量，取消时保留旧输入内容。Host 重载时按 `session_changed` 取消旧对话框。

控制队列与诊断队列各有条数和总字节预算。初始建议为控制 4096 条/16 MiB、诊断
1024 条/1 MiB；实际配置和上限需可查询。请求和媒体控制消息可靠，日志可丢并计数。
满载时必须在产生副作用前预留控制记录，或挂起尚未提交的内部操作；不能先推进脚本再
丢请求。即便游戏等待 Host，也要允许 poll 和结果回填；关键请求不能被日志阻塞。

控制记录按 FIFO 优先交付，排空后才交付诊断；跨队列不承诺 sequence 递增。
诊断满载或超过单条预算时丢弃新记录，
累计丢弃次数；其容量不占用控制预算。重载清除两个队列与旧结果，保留生命周期丢弃计数。
已受理结果另有默认 4096 条/16 MiB 预算，等待请求默认上限为 4096。metadata 和 blob
各默认限 1 MiB。字节预算按信封、长度头、metadata 和 blob 的编码总长计算。
单条或整批超过空队列总预算返回 LIMIT；仅因现有积压而放不下时返回 WOULD_BLOCK。

只有已被 poll 交付的请求可以接收结果，提前提交返回 STATE。结果批次的重复身份、
错误 runtime/epoch/service kind、已受理/取消的请求均返回 STALE，并保留整批原状态。
结果受理后从等待请求中移除，按提交顺序进入结果队列；等待计数与已受理结果计数分别查询。
消费结果后释放其容量。取消尚未交付的请求同时移除该控制记录；已派发的外部任务需要
服务层另发取消通知。本地取消不代表 Host 已停止外部工作。

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

### stat、read、单 blob 原子写入与 delete payload

这些控制消息均使用 JSON object metadata，二进制数据只放 blob。IO domain=3，
stat/read/write_atomic/delete 的 kind 分别为 1/2/4/5。结果使用原请求的 service kind；
request ID、epoch 和 runtime 均由信封/提交描述符携带。

| 消息 | metadata | blob |
|---|---|---|
| stat 请求 | `namespace: "asset"/"save"`，`key: string` | 空 |
| stat 成功 | `status: "ok"`，`found: bool`；found=true 时还需 `size`、`revision`，可选 `modified_utc_ms` | 空 |
| read 请求 | namespace、key、revision、offset、length | 空 |
| read 成功 | `status: "ok"`、revision、`eof: bool` | 读取的原始字节 |
| write_atomic 请求 | `namespace: "save"`、key | 整个待提交文件 |
| write_atomic 成功 | `status: "ok"` | 空 |
| delete 请求 | `namespace: "save"`、key | 空 |
| delete 成功 | `status: "ok"`（Host 已确认删除） | 空 |
| 任何失败 | status 为 `not_found`/`denied`/`io_error`/`cancelled`/`unsupported`，可选 `message: string` | 空 |

size、revision、offset、length 均为规范十进制 u64 字符串，不使用 JSON number。
modified_utc_ms 为规范十进制 i64 字符串，表示 UTC Unix 毫秒；缺失或 null 表示无时间元数据。
读取须保持 stat 返回的 revision，单次不超过 length；短读允许，但非 EOF 必须有数据，
累计数据必须与 stat 大小一致，准确到达文件末尾时 eof=true。revision/EOF/大小不一致的
结果返回 ARGUMENT，原请求仍等待 Host 修正。Host 发现资源变化时可返回 io_error，让
脚本进入原有失败分支。空文件在 stat 后即可完成读取。

单 blob 写入以 `(runtime, epoch, request_id)` 作为事务身份；Host 原子持久化后确认。
超出 blob 预算时不受理；多 blob 事务需要另行定义分块/提交 schema，不能将部分成功表示为
文件写入完成。删除只适用于 save 文件，目标不存在返回 not_found；完成前脚本不能继续。
完整服务能力由 facade 协商。

namespace 至少区分 asset 与 save；逻辑 key 使用 `/`，拒绝绝对路径、驱动器、NUL 和 `..`。
Artemis 的大小写兼容查找、归档覆盖顺序、别名和 savepath 需要独立 conformance 样例；
不能直接照 OS 大小写规则查找，也不能因重构改变已有 patch/sidecar 优先级。
脚本 savepath 是逻辑子目录，实际游戏隔离根由 Host 决定，禁止两侧重复拼接前缀。

Host 可预提交完整资源元数据索引以同步回答 exists/stat；未缓存的结果表示 Pending，
不表示不存在。热路径使用 CPU 缓存；大文件分块，所有块都校验资源 revision。
写入成功后才更新可见版本；缓存的 save 内容与宿主存储版本必须保持 read-your-writes。
Host 的 unsupported/permission/error 必须能进入脚本原有失败分支，不允许无限等待。

Artemis Rust 入口在图层首次引用 PNG 或蒙版时，用 IO 读取逻辑资源并在内存解码。资产与
尺寸按当前会话复用。缺失或不可解码的图片仍保留逻辑图层；未指定尺寸时查询返回零。

编号存档可包含场景树和保存时的缓动进度。读档先校验场景树并通过相同 IO 预读图片与离屏组遮罩，
然后提交恢复；取消预读不替换场景。原已借用的帧仍有效。仅含解释器状态的旧快照可读，
含文字与音频的快照同时恢复对应状态。从剧情等待调用菜单后写出的编号存档记录原等待位置、
调用栈及局部变量，并保留本次 `onSave` 写入的存档数据。没有等待标记的旧档按脚本位置推断。
Core 在替换剧情前通过 Host IO 读取未加载的当前脚本和调用者，校验每个保存位置。读取或校验
失败时保留原剧情和待执行命令。新加载脚本的 Luau 初始化在提交恢复之后、`onLoad` 之前执行。
编号存档可带有图层编辑记录，截图存档使用捕获时的记录。Core 从 Host 预读原图和叠图并按原顺序
重建像素。缺少素材或超出编辑预算的读档保留原场景。超出预算的保存返回失败，不写入不完整的快照。
读档清空 E-Mote 模型和播放状态，脚本可在 `onLoad` 中重建；编号存档不包含模型或图集。
`[load type=1]` 和 `[load type=2]` 用此前发布的画面淡入恢复后的场景，脚本可以继续运行。
读档没有规则图参数，类型 2 使用普通淡入；类型 0 为瞬切。

**同步 Lua 是必须先解决的实现门槛：**当前同步 EngineCallbacks 需要改成可保留调用现场的
资源依赖/continuation，或在进入该调用前完整准备它所需数据。索引预载只能解决已知查询，
还需覆盖任意动态文件名、写入确认和 Lua 中间加载。统一 Luau 后必须验证能从
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
Host renderer 接入以完整帧 schema 和绘制回归为验收依据。

Rust 草案先提供 `acquire_latest(handle, after)`，可借用基础图层快照。`sprites` 按绘制顺序
给出纹理表索引、原始图层 ID、仿射变换、UV、舞台裁剪、颜色、透明度和混合方式。纹理是
紧密排列的 RGBA8，直通 alpha，首行在顶端；资源缓存键为 `(runtime, epoch, revision)`。
同尺寸蒙版用独立纹理索引表示：采样前逐像素以 `alpha * mask_red / 255` 取整得到新 alpha。
缺失或尺寸不符的蒙版回退为原图。帧支持精灵、转场、内置组效果、E-Mote 和 HLSL 效果。
Rust 借用随 `FrameLease` 释放。

`FrameLease::packet()` 返回 `ARFP` 草案版 1 的二进制描述；`texture(index)` 借用对应像素。
C 草案的 `texture_rgba` 也使用帧内索引，像素和帧描述一起由同一个 token 保活。
帧描述最长 16 MiB；所有整数和 IEEE 754 `f32` 都用小端，无结构体填充。

| 区段 | 按顺序的字段 |
|---|---|
| 帧头，56 字节 | `"ARFP"` [4]；schema `u16`；头长 `u16`；总字节数、纹理数、绘制记录数、保留零各 `u32`；epoch、render frame ID、Host frame ID 各 `u64`；舞台宽、高各 `u32` |
| 纹理描述，每条 24 字节 | revision `u64`；宽、高、行字节数各 `u32`；像素格式 `u16`（1 = RGBA8，直通 alpha，首行在顶端）；保留零 `u16` |
| 转场记录，可选，24 字节 | 记录总长度 `u32`（24）；opcode `u16`（2 = 转场）；flags `u16`（0）；旧场景精灵数 `u32`；进度 `f32`；规则纹理索引 `u32`；软边宽度 `f32` |
| 精灵记录，104 字节前缀 | 记录总长度 `u32`；opcode `u16`（1 = 精灵）；flags `u16`；纹理索引、蒙版索引、混合方式、图层 ID 长度各 `u32`；变换 6 个 `f32`，矩形尺寸、UV 偏移、UV 缩放各 2 个 `f32`，裁剪 4 个 `f32`，opacity 1 个 `f32`，RGB 乘色 3 个 `f32` |
| 精灵记录尾部 | 按长度读取 UTF-8 图层 ID，含在该记录总长度内，无 NUL 与填充 |
| 合成组记录，56 字节 | 记录总长度 `u32`（56）；opcode `u16`（3 = 合成组）；flags `u16`；精灵起点、结束点、蒙版索引、混合方式各 `u32`；opacity `f32`，RGB 乘色 3 个 `f32`，裁剪 4 个 `f32` |
| 扩展精灵记录 | 与精灵记录共用 104 字节前缀和 UTF-8 图层 ID，opcode `u16`（4）；图层 ID 后依次为顶点数 `u32`、可选材质、顶点数据 |
| Stencil 组记录，40 字节 | 记录总长度 `u32`（40）；opcode `u16`（5）；flags `u16`；精灵起点、结束点、遮罩起点、结束点各 `u32`；裁剪 4 个 `f32` |
| HLSL 源码记录 | 记录总长度 `u32`；opcode `u16`（6）；flags `u16`（0）；源码 revision `u64`；原始 HLSL 字节（记录长度减 16） |
| HLSL 效果记录 | 记录总长度 `u32`；opcode `u16`（7）；flags `u16`；源码 revision `u64`；精灵起点、结束点、蒙版索引、用户纹理索引、常量数各 `u32`；裁剪 4 个 `f32`；随后依次读取指定数量的常量 |

索引从 0 开始；无蒙版用 `UINT32_MAX`。混合方式按 Alpha、Add、Screen、Multiply
依次编号 0 至 3。flags 的低三位依次表示裁剪、灰阶和反色，其余写零。裁剪为舞台上的
左、上、宽、高，未设置时四项均为零。变换顺序为两列基向量、平移。
Host 解析时应先校验版本、总长度、各段计数与记录边界，确认纹理索引和像素长度有效后再绘制。
C 预览通过帧表交付同一帧包和纹理借用。

有转场时，opcode 2 是首条绘制记录，计入帧头的记录数。紧随其后的指定数量精灵属于旧场景，
余下的精灵记录属于新场景，两组共用本帧纹理表。Host 将两组分别用相同背景清屏后绘制，再将旧场景
合成到新场景上。规则索引为 `UINT32_MAX` 时，旧场景的 alpha 乘以 `1-progress`。
有规则图时，以舞台归一化坐标采样规则纹理的红通道 `r`，取 `t=progress*(1+vague)-vague`，
旧画面 alpha 乘以 `smoothstep(t, t+vague, r)`。进度由 Core 的单调时钟计算，范围为 `[0, 1]`；
结束后仅交付新场景。缺失或解码失败的规则图使用交叉淡入。C Host 创建运行时时通过
`ART_RENDERER_CAP_TRANSITION`（位值 `2`）启用该记录；未启用则直接切换场景。

合成组记录在所有精灵记录之后。flags 的低五位依次表示裁剪、灰阶、反色、不透明组、属于转场旧
场景，其余为零。起点包含、结束点排除，索引相对于该组所属场景的精灵数组。子组在父组之前；
范围可以嵌套或并列。范围相同的组以后记录为外层。Host 将范围内的精灵和内层组绘制到透明
目标，再按舞台裁剪、颜色、透明度和混合方式合成到父目标。使用蒙版时，按全舞台 UV 采样蒙版
alpha；不透明组先将透明目标的合成 alpha 视为 1，再乘以组 opacity 和蒙版 alpha。
C Host 通过 `ART_RENDERER_CAP_GROUP` 启用合成组记录；未启用则按精灵顺序直接绘制。

扩展精灵沿用精灵 flags 的低三位；位值 `8` 表示有材质，`16` 表示遮罩精灵，`32` 表示转场
旧场景的遮罩精灵。普通绘制记录（opcode 1 或 4）之后是旧场景、新场景的遮罩精灵，两者分别
编号，最后是合成组和 stencil 组。扩展精灵的混合值 4 至 7 分别为 E-Mote 加色、反向减色、
乘色和滤色。顶点数为零时绘制四边形；非零时为三角列表，顶点为 `x,y,u,v` 四个 `f32`，
位置使用图层局部像素，UV 相对于精灵的 UV 矩形。

材质共 112 字节，依次为左上、右上、左下、右下的 RGBA 乘色（各 4 个 `f32`），图集归一化
UV 范围（最小与最大值，各 2 个 `f32`），原始 E-Mote blend 字段 `u32`，局部裁剪范围
（最小与最大值，各 2 个 `f32`），以及 wipe 的乘数、加数和启用值（各 `f32`）。Stencil 组
flags 仅使用 `1`（有裁剪）和 `16`（属于转场旧场景）。Host 用两段各自的半开区间绘制内容与
遮罩，再按遮罩 alpha 合成；子组先于父组，范围相同时后记录为外层。

C Host 声明 `ART_RENDERER_CAP_EMOTE` 后，Core 按 Host 的单调时间求值，并为一份带内嵌
DXT5 的 PSB 提交资源读取。Host 通过帧内 CPU 图集、材质、网格和 stencil 指令绘制模型；
不需要解析 PSB。未声明时，Luau 的模型创建调用返回错误。

HLSL 源码记录在普通和遮罩精灵之后，每个源码 revision 只出现一次。随后是绑定单一精灵的
效果记录，再按每个场景的组顺序写入组效果、合成组及 stencil 组。HLSL 效果 flags 的位值
`1` 表示有裁剪，`2` 表示离屏组效果，`16` 表示属于转场旧场景。起点包含、结束点排除；单一
精灵效果的区间长度为 1。缺省纹理索引使用 `UINT32_MAX`。常量依次为名称长度 `u16`、值数
`u16`、UTF-8 名称字节和对应数量的 `f32`，没有终止符或填充。

C Host 通过 `ART_RENDERER_CAP_HLSL` 启用 `[lyshader]` 读取和 HLSL 记录。Host 以
`(runtime, epoch, shader_revision)` 缓存平台程序，按本帧效果绑定纹理和常量。源码和效果
随帧借用；重载不会修改旧帧的源码。源码缺失或编译失败时直接绘制精灵，组内已有的其他效果
继续执行。着色器语言的约定见 [Runtime Shader ABI](SHADER_ABI.md)。

编号存档可记录 shader ID 到逻辑源码键的映射。使用截图时记录捕获时的映射。支持 HLSL 的
Core 在提交读档前通过 Host IO 预读这些源码和采样纹理；源码不可读时，读档失败并保留原场景。
恢复后的帧使用新的 shader revision，旧帧仍使用其借用的版本。缺少此映射的旧档沿用当前会话
已注册的 shader。

Rust Host 可使用 `host::renderer::SpriteRenderer` 在已开启的目标上绘制基础帧。适配器使用
同一帧借用的类型化视图，维护会话和资源版本对应的后端纹理。Host 控制 backend 的访问、
目标、帧开始与结束及呈现；转场需要后端支持从当前目标捕获画面。新设备可以用仍有效的帧借用
重新上传。

`acquire_latest(after_render_frame_id)` 返回不可变快照和 lease token；无新帧为 NO_DATA。
快照及所有引用的 CPU 资源版本在 release 前跨 step/reload 保持有效；默认至多两个
外借 lease，超过返回 WOULD_BLOCK。CPU 不可变块可以共享，不能每帧复制整个资源库。
Host 完成 CPU 读取/上传暂存后可 release，GPU 对象由 Host 按自身 fence 保活，不要求
core 理解原生 fence。纹理销毁由 Host 在 GPU 引用结束后执行。

逻辑 tick 与出帧独立：Host 每个 frame 调一次 step，无新画面时继续执行 onEnterFrame。
新快照可合并，但仍被外借的帧数据不能被改写。转场源 target/版本在后续帧引用期间保留，
必须显式结束其逻辑生命周期。GPU 设备丢失由 Host 重建，并从完整资源版本快照重放，
无需重跑剧情或重建游戏变量。

基础帧的属性缓动按受理的 Host 单调时钟求值。脚本结束后 Host 可继续提交新帧，直到图层
缓动结束；同一帧的 IO 恢复不推进动画时间。同步缓动等待目标图层完成，缓动组收齐后启动，
并等待标记同步的目标。完成处理器在 Core 的脚本队列运行，所需 IO 仍通过 Host；返回原
等待点时沿用已有的计时起点和排队续跑位置。

`[anime]` 在登记帧时读入缺失的图片和蒙版。播放直接使用会话缓存，由 Core 对受理的
Host 时钟求值并交付图层变化。

`[lyedit]` 的反色、灰度、棕褐、加色、乘色和叠图生成新的纹理版本。叠图预读缺失资产；
同一图层的连续加工从上次结果继续。重新创建图层使用原始资产，已借出的旧帧仍有效。

### 截图与缩略图

C Host 通过 `ART_RENDERER_CAP_SCREENSHOT` 启用截图服务。Core 在 `[takess]` 发布捕获用快照，
并发出 Render 域（6）kind 1 请求，metadata 为 `{"render_frame_id":"…"}`。Host 借用该帧并按
舞台尺寸绘制和捕获，按捕获请求 ID 保留结果。该帧在请求确认前保持最新；Host 可用
`acquire_latest(render_frame_id - 1)` 取得它。确认成功后可释放帧借用。

`[savess]` 发出同域 kind 2 请求，metadata 包含 `capture_id`（前次成功捕获请求 ID 的十进制
字符串）、`namespace:"save"`、带 `.png` 后缀的逻辑 `key`，以及输出像素尺寸 `width`、`height`
（`null` 表示沿用捕获尺寸）。Host 从该捕获缩放、编码 PNG，并完成原子替换后确认。后续读取
使用同一 save 命名空间。编号存档使用捕获时的场景和文字。

两个请求和结果均无 blob；结果为 `{"status":"ok"}` 或失败状态 `cancelled`、`denied`、
`unsupported`、`io_error`。失败后脚本继续，失败的捕获保留上一份成功结果。新捕获成功后
可释放上一份，重载和销毁清空 Host 的捕获。无可用捕获时跳过 `[savess]`。等待截图时 C 标记
`ART_WAIT_RENDER`，收到结果后用原 Host frame ID 恢复；队列满时先排空队列再用同一帧重试。

Host 可在首帧前通过 `set_default_font(handle, epoch, bytes)` 提交默认字体。Core 排版并生成
RGBA8 字形图集，将字形四边形按消息层顺序放进帧包；图集遵循纹理版本及借用约定。逐字显示
使用逻辑帧时钟；在 `[@]` 或 `[wt0]` 中按下前进键时，若文字尚未完整揭示，本次输入只展开
文字。未提交字体时，脚本仍推进并记录文本，Core 不查询系统字体。
脚本指定的 `face` 通过 Host 文件协议读取并按会话缓存；`medium` 文件找不到或无效时依次尝试
`regular`、`bold`，再使用已提交的默认字体。编号存档保存当前消息页的文本和字体设置。读档
先预读缺失字体，再恢复页面；取消预读取时保留原场景。

## 8. 媒体、UI、配置与诊断

媒体命令保留当前 BGM/SE/voice 的增益、pan、fade、交叉淡入、A/B 循环段、skippable
语义，不把 gain=1000 当作百分比。每次播放分配不可复用的 playback_id，脚本通道 ID
可以重复。Host 回报 started/ended/failed/cancelled 和可选播放时钟；旧播放完成不得停止
新声音。视频音轨由 Host 解码，不经 core 临时 WAV 路径。

Rust 句柄当前输出 Media domain=5、kind=1 的可靠音频命令，blob 为空。

| `op` | metadata |
|---|---|
| `play` | `playback_id`、`replaces`（旧播放 ID 或 null）、`channel`（`bgm`、`se`、`voice`）、`id`（脚本通道，BGM 为 null）、资产 `key`、`loop`、可选 A/B 循环 `loop_key`、`gain`、`pan`、`fade_ms`、`buffer_ms`、`skippable`、`crossfade`。`loop_key`、`gain`、`pan`、`buffer_ms` 未指定时为 null。 |
| `stop` / `gain` / `pan` | `playback_ids`、渐变 `time_ms`、`value`（`stop` 为 null） |
| `stop_all` | `fade_ms` |
| `restore` | `plays`：一组完整的 `play` metadata。Host 停止当前会话的音频，再从起点开始播放列表中的新 ID。空列表表示静音。`stop_video` 为 true 时同时停止当前视频。 |

`playback_id`、`replaces`、`playback_ids` 的元素及 u64 时间均使用十进制字符串。Host 按事件
顺序处理命令，从资产命名空间读取并播放。完成引导段后按非空 `loop_key` 循环；交叉淡入时
平滑替换 `replaces`。`submit_playback_status(handle, epoch, playback_id, status)` 在下一次受理
新帧时应用；每次播放可报一次 `Started`、一次终态。`Ended` 触发注册的完成处理器；失败和取消
也解除对应等待。`[wait se=… time=N]` 从 Core 应用 `Started` 的帧计时。停止、替换或重载后，旧
播放状态回报返回 `Stale`；带淡出的停止在 Host 回报终态后结束。重载时 Host 应停止旧 epoch。
编号读档成功时，Core 在 `onLoad` 前将音频恢复命令排入可靠队列。存档保存曲目、循环、增益和
声像；不保存设备播放位置。读取没有音频快照的旧存档按静音恢复。

Media kind=2 交付视频命令。`op=play` 带有十进制字符串 `playback_id`、可空旧播放 ID
`replaces`、可空图层 ID `layer`、资产 `key`、布尔 `loop`、整数 `skip` 和可空 `delay_margin_ms`。
`layer=null` 由 Host 全屏呈现；`skip` 的 0、1、2 分别表示禁止跳过、可点击跳过和仅通过菜单
跳过。`op=stop_all` 停止当前会话的视频。视频沿用音频的开播和终态回报，编号读档停止旧视频。

图层视频使用 `submit_video_frame` 提交连续的 `frame_id`、尺寸和紧密排列的自上而下 RGBA8
像素。Core 复制像素；同一播放只接受递增的帧 ID，下一受理逻辑帧只使用最新的一份。过期帧
和终态后的帧返回 `Stale`。单张帧最多 64 MiB，当前和待用数据合计最多 128 MiB；已借出的
帧有自己的有效期。图层视频随场景帧绘制；旧借用不因新画面或播放完成而改变。

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

独立 C Host 可用 `bash scripts/test-host-v2.sh` 或 `scripts/test-host-v2.ps1` 构建并验证。
在 `tools/host-bindings/dart` 目录运行 `dart pub get --enforce-lockfile` 后，使用
`dart run bin/smoke.dart <纯 Core 动态库路径>` 可验证 Dart 接入。样例用内存资源启动场景，
提交输入并校验通知、帧数据与纹理借用。

实施阶段与进度见 [GOAL.md](../GOAL.md)。
