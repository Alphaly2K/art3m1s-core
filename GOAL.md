# Artemis Host 接口重构

## 长期目标

- Core 保留脚本、图层、文本、E-Mote、存档格式和平台无关渲染数据。
- Host 负责物理 IO、GPU 绘制与呈现、音视频设备及系统服务。
- 所有平台统一 Luau；RFVP、KRKR、Siglus 的接口和实现保持现有边界。

## 当前阶段

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

继续定义 v2 的请求身份与可靠队列，落实 Luau IO continuation 和 Host 帧协议。
P1 的代际句柄、请求世代和时钟注入仍待实现，纯核心依赖拆分与 Host renderer SDK
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

后续仍待完成：v2 C 查询入口与可发布 schema、Luau IO continuation、纯核心依赖拆分
以及独立 FramePacket/Host renderer SDK。
Rust 注入入口与已完成的会话隔离属于前置改造；P1–P5 的其余项目仍待完成。

本批验证：`cargo check --locked --offline --all-features`、根 crate 与解释器的 fmt 检查、
完整 `scripts/test-all.sh`（655 passed、9 ignored，含原有未跟踪测试）以及 release 构建通过。
未执行外部商业游戏 fixture 或可选 CGL 硬件组。macOS 默认 release 动态库的 `nm -gU`
结果为 `art3m1s_get_api_v1` 和 9 个独立 `pfs_*`，未新增平铺 runtime 符号或 v2 查询入口。
