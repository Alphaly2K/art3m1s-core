# Artemis Host 接口重构

## 长期目标

- Core 保留脚本、图层、文本、E-Mote、存档格式和平台无关渲染数据。
- Host 负责物理 IO、GPU 绘制与呈现、音视频设备及系统服务。
- 所有平台统一 Luau；RFVP、KRKR、Siglus 的接口和实现保持现有边界。

## 当前阶段

Rust 场景驱动已接入 Host frame 与可恢复的 onEnterFrame。加载期 Lua 完成后才派发回调；
同帧 IO 重入保留原 Lua 栈、单调时间和帧号。前一帧尚有 IO 时不受理新帧，已有普通事件
仍须 Host 确认。同步 v1 沿用现有帧入口。

新增 1 项内存 Host 回归，覆盖加载期与回调两次挂起、同帧时间校验、新帧背压及场景事件
确认顺序。解释器与 core/Host 共 641 项通过、2 项既有测试跳过；全 feature 编译与格式通过。

下一步接入事件过滤、存读档钩子及对应业务调度，并验证 Host 输入边沿的消费边界。

## 已完成：Host 时钟和随机服务

已接入实例内 Host 时钟和随机服务：

- e:now/os.clock、等待截止时间使用单调时间，os.time/date、日期变量和文件日期使用
  Host 的 UTC/偏移快照；新会话默认零时间，不读取本机时钟/时区。
- e:random 与 var random 共享实例内随机状态，math.random 使用显式种子。
- 匿名消息层 ID 使用实例内序列；变量快照恢复保留当前环境服务。

新增 3 项必要回归，验证 UTC 回拨、负日期、显式偏移、IO 恢复、等待截止、
变量反序列化和多个 VM 的随机/消息层隔离。完整解释器 259 项与 core/Host 381 项通过，
共 640 项通过、2 项既有测试跳过；全 feature 编译与格式检查通过。测试均使用内存输入。

## 已完成：外部场景与 BOOT

已接入外部场景资源与 BOOT 的 Host 读取：

- 内联、动态展开、排队 call/jump 保存已解析的目标和调用栈，不重跑过滤器或入栈。
- macroadd 与默认宏支持 Host 交付的文本/ASB；可选文件失败继续，必需场景失败终止会话。
- start/boot、栈重写和位置恢复按需读取缺失脚本，恢复后的 return 也可读取缺失的父脚本。
- ScriptIoSession 新增 open_boot，Host 可只交付逻辑入口名；替换使旧 VM/请求失效。

新增 5 项必要回归，使用内存 Host 和构造的 ASB 字节，覆盖队列屏障、过滤器不重放、
宏/场景失败差异、取消、BOOT 入口选择、栈恢复及错误结果类型。完整解释器 258 项与
core/Host 379 项通过，共 637 项通过、2 项既有测试跳过；全 feature 编译与格式检查通过。
新增测试无需本机素材或图形设备。下一步继续剩余 Lua/事件入口与 Host frame、时钟注入。

## 已完成：tag filter 与文件查询

已完成可恢复 tag filter 和文件查询：

- 过滤器保留原指令、来源和返回值，恢复后先完成原标签，再抽干过滤器生成的命令。
- 内联、动态展开、排队及 elseif 跳转均不重跑过滤器；旧 queued stop 在 continuation 后恢复。
- AST 与 Lua 的 file_exists/file_exist、file_crc、file_update_time 共用 Host 请求路径。
  CRC/日期格式沿用已有逻辑；stat 可携带 modified_utc_ms，日期偏移由 Host 时钟提供。
- restore_position 与 reset 释放旧任务和过滤器状态，阻止旧调用现场继续改写场景。

验证使用内存 Host，包含 IEEE CRC32 参考输入、负时间戳、空文件、save namespace、过滤输出
顺序、旧等待和位置恢复。其余 Lua 入口及 Host frame/时钟调度仍待完成。
本批新增 6 项必要回归；完整解释器 256 项及 core/Host 376 项通过，共 632 项通过、
2 项既有测试跳过。全 feature 编译和格式检查通过；未改绘制算法，不运行图形硬件回归。

## 已完成：加载期 Lua 与 calllua

已接入加载期 Lua 与 calllua 的可恢复场景调度：

- 解释器显式启用 resumable IO；加载期 Lua 块与内联/排队 calllua 均保存真实 continuation。
- IO 等待单独返回 HostIoPending，不当作普通等待推进 PC；嵌套参数表跨暂停保留。
- 跨文件加载的 Lua 初始化先于目标标签；待执行标签队列在 Lua 完成前保持不动。
- ScriptIoSession 的场景入口创建新 VM，以可靠 IO 传输驱动；普通事件由 Host 明确恢复。

验证采用纯内存场景/加载器，覆盖 PC、队列、加载顺序、重复加载、重置、场景替换和旧结果。
本批新增 5 项回归；完整解释器 254 项及 core/Host 372 项通过，共 626 项通过、2 项既有
测试跳过。新增测试均不依赖外部素材/图形设备；全 feature 编译和格式检查通过。
其余调度入口的进度见当前阶段及下一阶段。

## 已完成：Luau IO chunk 接入

已完成 Luau IO chunk continuation 与请求传输接入：

- `IoTask` 从原 Lua 栈挂起/恢复，Lua 包装层负责 `e:file`、嵌套 include、exists 及文件确认。
- `ScriptIoSession` 把读取拆为 stat 和按 revision 的分块 read；等待 Host 确认后才完成写入。
- 结果提交不运行脚本；取消和替换任务按 epoch/请求身份失效，队列满载保留当前 continuation。
- 不调用物理文件加载器或 GPU；新路径拦截仍带磁盘回退的旧文件 var 查询。

该检查点以显式 Lua chunk 为入口，复用解释器的引擎对象、变量和内存文件方法；同步 v1
调用链保持独立。后续场景入口与调度接入的进度见上文，完整契约完成前不发布 v2 C ABI。

验证覆盖动态路径读取前副作用仅执行一次、嵌套 include 与局部变量、空文件/短读、读写可见性、
失败确认与重试、非法结果整批保留、双实例交错、容量阻塞及派发前/后的取消。输入均为内存
构造，无外部素材或系统图形条件；公共缓冲文件实现有改动，需要完整解释器回归与 core Host 回归。
本批完整解释器单元/集成 251 项与 core 单元/Host 集成 370 项通过，共 621 项通过、
2 项既有测试跳过；全 feature 编译与格式检查通过。新增 8 项测试均保留用于明确的
continuation/确认/边界回归，无硬件或专有输入依赖。

## 已完成：v2 会话传输

已实现独立的 v2 Rust 会话传输核心（`host::protocol`）：

- 请求身份包含 runtime、epoch、request_id 和 service kind；请求与序列计数耗尽时报错。
- 控制、诊断及已受理结果各有条数/总字节预算；可靠控制不被日志挤出，满载时不受理操作。
- 结果批次完整验证后复制入队，在 step 边界由调用方消费；重复、取消、重载和串实例结果被拒绝。
- 输出使用 56 字节小端信封及 JSON metadata/raw blob；容量不足时保留记录并报告所需大小。

本阶段不接入旧 runtime 输出路径，不发布 v2 C 入口。代际句柄注册表、owner 线程、
服务 payload schema 和 Luau continuation 接入由后续 facade/解释器阶段完成。

新增 9 项纯内存协议回归，防止控制丢失、批次部分受理、身份误认和容量重试消费记录；
同时覆盖请求/序列/epoch 耗尽、满载重载及取消后的迟到结果。复查确认所有新增测试
无需外部素材、硬件或本机文件。core 单元测试 359 项与已有 Host 集成回归 6 项通过，
共 365 项通过、2 项既有测试跳过；全 feature 编译与格式检查通过。
本阶段未改变解释器、绘制算法或 C 函数表，不重复解释器全套测试、图形回归和 release 构建。

## 已完成：运行时实例隔离

已完成运行时 Host 状态与脚本查询的实例隔离：

- 根目录使用 AGENTS.md，移除旧 AGENT.md；阶段计划集中在本文件。
- 显式 Rust Host 使用 runtime 自有 HostSession，独立持有事件、窗口/字体/文本状态、
  覆盖字体及 debug/profiler 配置和 IO 计数。旧 v1 入口保留原有兼容路由。
- Host 查询安装到解释器实例；声音、历史文本和度量快照归 runtime 所有。读档保留当前
  实例的查询服务，重载时重新绑定，销毁时只释放本实例状态。
- 共享后端日志按当前调用的会话交付；媒体 worker 捕获来源会话，销毁时使提取任务失效。

验证采用纯内存资源与不创建 GPU 的 renderer，覆盖双实例交错、Host 状态与输出隔离、
序列化/内存快照恢复、重载和销毁，以及嵌套调用和工作线程日志路由。新增测试均不依赖
外部素材、系统字体、窗口或图形设备；移除仅对静态快照赋值再读取的冗余测试。

本批通过全 feature 编译检查、格式检查，以及 core 单元测试 350 项、相关集成回归
6 项和 Luau 解释器单元/集成测试 248 项，共 604 项通过，2 项既有测试跳过。未执行 release 全量构建或图形像素回归：本批修改集中在会话路由、查询归属
和 C 队列包装，未改动 shader、绘制算法或稳定 C 函数表布局。

## 下一阶段

继续接入其余 Lua/事件入口，落实同 Host frame 内 IO resume
不重复输入/onEnterFrame 的调度契约，并注入 Host 时钟。
P1 的代际句柄 facade 和时钟注入仍待实现，纯核心依赖拆分与 Host renderer SDK
按后续阶段推进。

## 后续阶段

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

验证按根目录 `AGENTS.md` 选择与变更风险相称的检查；图形和外部素材回归显式运行。

## 已完成的基础改造

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

后续仍待完成：v2 C 查询入口与可发布 schema、剩余场景/事件入口的 IO 调度、纯核心依赖拆分
以及独立 FramePacket/Host renderer SDK。
Rust 注入入口与已完成的会话隔离属于前置改造；P1–P5 的其余项目仍待完成。

本批验证：`cargo check --locked --offline --all-features`、根 crate 与解释器的 fmt 检查、
完整 `scripts/test-all.sh`（655 passed、9 ignored，含原有未跟踪测试）以及 release 构建通过。
未执行外部商业游戏 fixture 或可选 CGL 硬件组。macOS 默认 release 动态库的 `nm -gU`
结果为 `art3m1s_get_api_v1` 和 9 个独立 `pfs_*`，未新增平铺 runtime 符号或 v2 查询入口。
