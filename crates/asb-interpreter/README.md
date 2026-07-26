# ASB Interpreter

Artemis Engine 脚本解释器库。它负责解析/执行 ASB、AST、IET 文本化脚本，提供变量、表达式、宏、Lua bridge、tag handler 和事件回调。渲染、音频、视频、文件系统和输入不在本 crate 内直接实现，而是通过 `Event` 与 `EngineCallbacks` 交给宿主 runtime。

生产使用方是 `https://github.com/Alphaly2K/art3m1s-core`。

## 职责边界

```text
asb-interpreter
  ├─ script.rs        Artemis 标签文本解析
  ├─ expression.rs    $var、算术、比较、逻辑表达式
  ├─ macro.rs         macroadd/macrodel 展开
  ├─ variable.rs      local / g. / t. / s. 变量域
  ├─ lua_engine.rs    Lua EngineApi: e:tag, e:var, e:file, e:include...
  ├─ tags/            内建 tag handler
  └─ interpreter.rs   step/run、call stack、queued tags、Lua dispatch

art3m1s-core
  ├─ 接收 Event
  ├─ 归约到 compositor/audio/video/save/input/text
  └─ 实现 EngineCallbacks
```

解释器不认识游戏脚本里的 `btn_click`、`sv.save` 等函数语义。它只负责调用 Lua、排队标签、发出事件。

## 主要能力

- ASB 二进制解码：通过 `asb-decrypt`。
- 文本脚本解析：label、tag、Lua block、宏。
- 控制流：`jump`、`call`、`return`、跨脚本加载。
- 等待：`stop`、`wait`、`exkey` 等转换为 `ExecutionResult::Wait`。
- 变量系统：local、`g.*`、`t.*`、`s.*` 四个域。
- Lua 集成：`[lua]`、`[calllua]`、Lua `tags` 表自定义标签分发。
- Queue 语义：Lua `e:tag{}` / `e:enqueueTag{}` 进入 tag queue，由解释器按 Artemis 顺序抽干。
- 图层/输入/音视频/存档标签：转换为 `Event`，由 runtime 消费。
- Pluto 替代实现：用 JSON 保存 Lua table，同时保留混合数字/字符串 key。

## 变量域

| 域 | 示例 | 语义 |
|----|------|------|
| local | `scr`, `log`, `btn` | 编号存档随 `SaveData` 保存/恢复 |
| global | `g.system`, `g.config` | 跨编号存档持久域，通常由 `syssave()` 落入 `saveg.dat` |
| temp | `t.tmp` | 临时变量，不序列化 |
| system | `s.savepath`, `s.bgmvol` | 系统/宿主变量，通常由 runtime 种入或维护 |

`SaveData` 的编号存档应只保存 local 域；`g.*` / `s.*` 由 runtime 的系统存档链维护，避免读旧编号档时覆盖当前存档索引和配置。

## Lua Bridge

Lua 侧注入 `__engine`，并暴露常用 Artemis API：

- `e:tag{...}`：把标签排进 queue；`var` 会同步落值，保证同一 Lua 函数内 `e:var()` 可读回。
- `e:enqueueTag{...}`：显式排队，支持控制流标签、`calllua` 和嵌套 `params`。
- `e:var(name)`：读取共享变量，缺失时返回 `"0"`。
- `e:file(path)` / `e:isFileExists(path)`：委托 `EngineCallbacks`。
- `e:include(path)`：读取并在同一个 Lua VM 执行 Lua 文件。
- `e:getMousePoint()`、`e:isPush()`、`e:isDown()`、`e:isUpEdge()` 等：委托宿主输入快照。
- `e:setEventHandler{onEnterFrame=..., onSave=..., onLoad=...}`：注册 runtime 回调。

`flush_tag_queue()` 支持 queued tag 内的 `Jump` / `Call` / `Return` / `Wait`，并记录 queue wait 来源，避免恢复等待时跳过下一条脚本指令。

## Pluto JSON 兼容层

Artemis 游戏脚本常用：

```lua
fsave_pluto("g.system", sys)
sys = fload_pluto("g.system") or {}
```

本解释器用 JSON 实现 `pluto.persist` / `pluto.unpersist`。注意 Lua table 不能简单按数组编码，因为 `sys.saveslot` 是混合表：

```lua
sys.saveslot[1] = { file = "save0001" }
sys.saveslot[4] = { file = "save0004" }
sys.saveslot.last = 4
sys.saveslot.check = { save0001 = true }
```

因此编码时所有 table 都保存为 JSON object，数字 key 转字符串；解码时规范整数 key 转回 Lua number key。这保证稀疏槽位和字符串字段不会在 syssave/sysload 往返中丢失。

## `lyevent` 语义

按 Artemis 文档，原生 `[lyevent]` 使用：

```text
[lyevent id=... type=click|rollover|rollout|dragin|drag|dragout handler=...]
```

`click=...`、`over=...`、`out=...` 这类字段不是原生 `lyevent` 简写；它们可能是游戏 Lua 包装层的自定义参数。解释器只按 `type` 注册一个事件，其余未知字段透传给 handler，不做硬编码展开。

## 快速开始

```rust
use asb_interpreter::{CallbackResult, ExecutionResult, Interpreter, InterpreterConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut interpreter = Interpreter::new(InterpreterConfig::default());

    interpreter.load_script(
        "test",
        r#"
*main
[var name="message" data="'Hello'"]
[calllua function="showMessage"]
[stop]
"#,
    )?;

    interpreter.lua().load(r#"
        function showMessage(e, p)
            e:tag{"var", name="seen", data="1"}
        end
    "#).exec()?;

    interpreter.set_callback(|event| {
        println!("{event:?}");
        CallbackResult::Continue
    });

    interpreter.start("test", "main")?;
    loop {
        match interpreter.run()? {
            ExecutionResult::Completed | ExecutionResult::Wait(_) => break,
            ExecutionResult::CallScript { file, label }
            | ExecutionResult::JumpScript { file, label } => {
                eprintln!("external script transfer requested: {file}:{label}");
                break;
            }
        }
    }

    Ok(())
}
```

## 常用 API

| API | 说明 |
|-----|------|
| `Interpreter::new(config)` | 创建解释器 |
| `load_script(name, text)` | 加载文本脚本 |
| `load_asb(name, bytes)` | 解码并加载 ASB |
| `set_file_loader(loader)` | 设置跨脚本/包含文件读取器 |
| `set_engine_callbacks(callbacks)` | 注入宿主 EngineCallbacks |
| `set_callback(callback)` | 接收 `Event` |
| `start(script, label)` | 从 label 启动 |
| `run()` / `step()` | 推进执行 |
| `advance_line()` | 外部等待完成后推进 |
| `fire_enter_frame()` | 执行 `onEnterFrame` |
| `fire_save_handler()` / `fire_load_handler()` | 执行 `onSave` / `onLoad` |
| `flush_pending_tags()` | 抽干 Lua 排队标签 |
| `restore_variables()` / `restore_position()` | 读档恢复 |

## 支持的标签类别

- 控制流：`jump`、`call`、`return`、`reset`
- 脚本等待：`stop`、`wait`、`wt`、`wt0`、`exkey`
- 变量：`var` 及常用 `system=` 查询
- Lua：`lua`、`calllua`
- 图层：`lyc`、`lydel`、`lyprop`、`lyevent`、`lyrename`、`lyedit`、`lydrag`、`anime`
- 输入/系统事件：`setonpush`、`delonpush`、`setonwindowbutton` 等 `seton*` / `delon*`
- 音频/视频：`splay`、`seplay`、`voice`、`video`、finish handler
- 存档/UI：`save`、`load`、`syssave`、`takess`、`savess`、`dialog`、`yesno`

## 测试

```bash
cargo fmt
cargo test --lib
```

与 runtime 交互的回归通常在 `art3m1s-core/tests` 中补 probe。
