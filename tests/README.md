# 测试结构

- 生产模块旁的测试是小型、自包含的单元测试。
- 顶层 `tests/*.rs` 是自包含的集成回归测试，默认在 CI 中执行。
- `tests/compatibility/` 依赖无法随仓库分发的商业游戏数据。这些测试统一标记为
  `#[ignore]`，仅在明确要求时执行。
- 依赖真实游戏数据的手动诊断程序放在 `tools/game-probes`，输入路径必须通过命令行
  参数传入。这些目标要求默认关闭的 `game-probes` feature，默认构建和 CI 不会编译
  或运行它们。

## 手动诊断工具

按需从本地游戏项目运行：

```bash
cargo run --features game-probes \
  --bin probe_caption_test -- /path/to/project

cargo run --features game-probes \
  --bin compatibility_probe -- /path/to/game.pfs config /path/to/output
```

## 外部 fixture

设置一个包含稳定 fixture ID 的根目录：

```text
$ART3M1S_FIXTURES_DIR/
  hamidashi/
  loli/
  nekomiko/
```

每个子目录都必须是包含 `system.ini` 的解包项目根目录。也可以单独覆盖某个
fixture，无需修改源码：

```bash
export ART3M1S_FIXTURE_HAMIDASHI_DIR=/path/to/hamidashi
```

仅运行外部兼容性测试：

```bash
ART3M1S_FIXTURES_DIR=/path/to/fixtures \
  cargo test --test compatibility -- --ignored --test-threads=1
```

显式运行这些测试却缺少 fixture 时，测试会给出配置错误并失败，不再静默通过。

使用 `scripts/test-all.sh` 运行整个仓库的逻辑测试。macOS 上四个依赖硬件的 CGL
测试会被单独分组，因为它们创建 context 时会受到同一轮其他图形测试的影响。
需要验证该组时，使用独立进程在其余测试之前运行：

```bash
ART3M1S_RUN_CGL_TESTS=1 ./scripts/test-all.sh
```

其他测试和平台保持 Cargo 默认的并行执行方式。
