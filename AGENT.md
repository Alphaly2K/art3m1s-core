# Agent 与 contributor 交接指南

## 工作原则

- 默认用中文交流。先查看 `git status` 和已有 diff，保留用户及其他贡献者的未完成工作。
- 实现兼容性时先遵循原引擎文档和可验证的通用语义。游戏脚本是复现材料，不是引擎规范；不要按游戏名、模板名或私有脚本函数硬编码修补。
- 用户确认已修复的行为是回归基线。排查使用本轮日志、实际输入和当前提交差异，不以旧日志代替验证。
- 性能优化需要优化前后同条件测量；区分微基准、core 探针和完整宿主结果。编译通过不代表性能或行为已验证。
- 完成一项功能或修复后，运行相应检查并作小而聚焦的提交。存在其他 agent 的重叠工作时先协调，不要提交其未完成文件。
- 不要删除不明来源的未跟踪文件；诊断产物、商业游戏数据、临时日志和 `target/` 不应进入提交。

## 生产入口与边界

- 先读 `README.md`、`tests/README.md` 和相关模块代码。
- 生产路径是 Flutter 宿主 → `src/ffi.rs` → `CoreRuntime`。core 不负责创建窗口或音视频解码；不要用旧窗口示例替代生产链路判断问题。
- 宿主提供帧时钟与输入；脚本注册的事件队列以及 `onEnterFrame`/vsync 是运行时行为的一部分。
- 图层 ID 是字符串，必须保留 `1.80` 等原始身份，不能转成数值再格式化。
- `crates/asb-interpreter` 负责脚本，`src/runtime/` 负责运行时集成，`src/render_pipeline/` 与 `src/backend/gl/` 负责绘制与 GPU 上传。

## E-Mote / Eluna

- 原实现：`crates/art3m1s-emote/`；Eluna：`crates/eluna/`；宿主适配：`src/runtime/emote.rs`、`src/runtime/emote/eluna.rs` 与 `eluna_mesh.rs`。
- 保留同一 host frame 内批处理命令、在 `Advance` 时统一推进并求值的行为。不要恢复逐命令全场景重建，也不要因计算过慢截断累计动画时间。
- 优化必须保留嵌套 motion、网格继承、HOLD 帧、前帧位置、物理、口型和眨眼语义。跳过求值前应确认所有时间依赖和外部参数变化均已覆盖。
- 对比原 E-Mote 实现时，优先研究数据布局、静态解析缓存、变化检测和调度；不能靠降低动画速度掩盖 CPU 开销。

## 检查与测试

```sh
cargo check --all-features
cargo fmt --check
./scripts/test-all.sh
```

`test-all.sh` 覆盖 core、Lua 5.1/Luau、两个 E-Mote crate 和 PFS crate。子 crate 不要仅靠顶层 `cargo test` 代替验证。

macOS 硬件 CGL 测试应按脚本单独运行：

```sh
ART3M1S_RUN_CGL_TESTS=1 ./scripts/test-all.sh
```

遇到 `CGLChoosePixelFormat failed`，先区分 context/环境失败和代码回归。外部游戏测试使用 `ART3M1S_FIXTURES_DIR` 或 `ART3M1S_FIXTURE_NEKOMIKO_DIR` 等配置，具体命令以 `tests/README.md` 为准。不要把本机绝对游戏路径写入测试。

## 当前性能交接（2026-09-05，会随后续提交过时）

用户报告的 NekoMiko 双模型实测基线：约 79 updates/s，完整求值约 25 ms/update，mesh build 约 6.4 ms/frame，84k–134k vertices/frame，上传约 73 MiB/s，core 探针物理内存约 500–610 MiB。它们来自此前运行，不能当成后续版本的已验证指标。

本轮开始时已有未提交优化：motion priority 每帧共用、`Arc` 场景发布、profiler 分项、目录型 compatibility probe、纹理上传后释放重复宿主数据、共享 mesh 顶点及相同快照的 draw command 缓存。具体完成状态以 diff、测试和提交记录为准。

当前优先事项是压低 Eluna CPU：削减每次更新对约 1777 个 layer state 的完整重建开销，随后完善静止冻结、静态 schema/scene 缓存与 worker 调度，最后处理动态 VBO 重复上传。用户已验证上一阶段画面，本轮明确要求优先性能，无需重复人工验图；后续涉及行为变更仍需自动回归验证。

交接时明确列出：已排除问题、当前症状、优先怀疑点、建议探查顺序，以及已验证指标和未完成项目。
