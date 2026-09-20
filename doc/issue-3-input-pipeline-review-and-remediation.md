# Issue #3 点击管线审阅与可落地修复指南

日期：2026-09-15  
范围：Artemis `CoreRuntime` 的鼠标、按键、`setonpush`、`lyevent`、`overrideKey`、`keyconfig` 与脚本等待推进。  
Issue：<https://github.com/Alphaly2K/art3m1s-core/issues/3>

## 结论

Issue #3 报告的卡住现象真实，但“只要当前是 `Generic`，就让原始左键穿过已经成功派发的 `setonpush`”不是正确修复。

标准 Artemis 系统脚本表明，原引擎的核心输入模型比当前实现简单：

1. Host 写入原始按键状态；
2. `onEnterFrame` 用 `overrideKey` 得到本帧有效按键状态；
3. 引擎尝试派发 `setonpush`/`lyevent`；
4. 事件过滤器返回 `2` 或没有处理器时，才执行该键的 `keyconfig` 默认 role；
5. 成功派发的 `setonpush` 可以在 Lua 中设置一个 dummy click；
6. 下一帧 `onEnterFrame` 用 `overrideKey(dummy, isDecide)` 注入该键；
7. dummy 键命中 `keyconfig role=0`，由引擎释放点击等待。

复杂的 UI、选择、自动、跳过状态机属于游戏 Lua。Core 只需实现统一的按键状态位、事件派发结果和默认 role 回退，不应识别 `flg.exclick`、`flg.ui`、`btn.cursor` 或游戏函数名。

当前实现把上述一条通路拆成了互不一致的三条：

- 鼠标左键通过 `global_push_absorbs_default_click` 判断是否推进；
- Enter/滚轮等通过无条件的 `role_advance` 推进；
- Lua dummy click 通过 `scripted_down_edge` 直接唤醒等待，绕过 `keyconfig`。

这是点击管线复杂且容易产生兼容性补丁的根因。

## 审阅证据

### 文档规定

- `overrideKey` 修改当前帧输入，预期在 `onEnterFrame` 使用；`status=0` 可屏蔽输入，`status=32` 表示 `isDecide`：`example/docs/lua/engine/overrideKey.txt`。
- `setEventFilter` 返回 `0` 继续派发，`1` 假装成功但不派发，`2` 假装失败且不派发：`example/docs/lua/engine/setEventFilter.txt`。
- `keyconfig role=0` 是前进；其他 role 管理菜单、隐藏、日志、自动、跳过和回避：`example/docs/tag/system/keyconfig.md`。
- `setonpush keyrepeat=1` 在初次按下后等待 0.5 秒，随后每帧触发：`example/docs/tag/system/setonpush.md`。
- `wait input=1` 接受用户输入；`input=2` 只在进入等待时已经处于跳过状态的情况下不等待：`example/docs/tag/script/wait.md`。
- Windows 键 ID 为左键 `1`、右键 `2`、中键 `4`：`example/docs/spec/key_id.md`。

### 标准系统脚本行为

检查现有 HENPRI Artemis 系统脚本得到以下通用语义：

- `system/adv/keyevent.lua` 明确注明：事件过滤器返回 `2` 时，`setonpush` 对应按键的原本处理继续执行；返回 `1` 时视为成功并阻止后续处理。
- `system/adv/keyconfig.lua` 为物理键注册 `setonpush_calllua`，但把 `keyconfig role=0` 绑定到 dummy key，而不是直接绑定鼠标左键。
- `setonpush_calllua` 只在 Lua 状态机允许前进时调用 `setexclick`；处于 UI、选择或特殊等待时可以拒绝本次物理点击。
- `system/adv/vsync.lua` 在 `onEnterFrame` 中把 `flg.exclick` 转为 `overrideKey{key=dummy,status=32}` 并清除标志。
- `scriptMainloop`/`scriptMainAdd` 同步执行并通过 ASB tag/jump 进入点击等待，没有 Lua coroutine 跨帧 yield。

因此：

- 成功派发 `setonpush` 后不执行原始 role，是原引擎有意的输入门控；
- Issue 中 `flg.exclick=nil` 说明 Lua 状态机没有接受该点击，不能据此让 Core 强制推进；
- 真正需要追查的是为什么切入 scenario 后 UI 状态没有清理，或者 dummy decide 为什么没有经过 `keyconfig role=0` 释放等待。

### Git 历史

`src/runtime/input.rs` 的逻辑从 2026-06-27 起逐次叠加：

- 最初只有单一 hover、layer click 和 push；
- 随后用 `handled_by_layer`/`handled_by_push` 阻止默认点击；
- 鼠标按下/抬起、drag、穿透、多 hover、链接、模式处理、inline return frame 相继加入；
- `global_push_absorbs_default_click` 在一个大型 shader/dialog 提交中加入，只特判 `Timed input=1`；
- `overrideKey` 和 `scripted_down_edge` 后加入，但没有统一改造早期的原始输入派发。

这不是原引擎本身需要如此多的分支，而是不同阶段的兼容补丁没有收敛到同一个输入模型。

### OpenArtemis 参考实现审阅

审阅了 `luxiaoling-mc/openartemis` 的 `f7af3176c47bc44642927a37302c15fb0e857053`
（重点为 `runtime.cpp`、`runtime_lua.cpp`、`layer.cpp`、SDL 触摸桥及
`input_dispatch_test.cpp`）。该实现提供了真实游戏兼容性线索，但不能直接作为引擎规范，
因为输入主链中存在多处与本文档和 `example/docs` 明确冲突的行为：

- `overrideKey{status=0}` 被实现成删除覆盖，无法屏蔽该键；省略 `key` 的全键覆盖也未实现；
- `isPush` 对按住状态逐帧返回真，没有文档规定的初次按下、0.5 秒静默、随后重复语义；
- 物理输入没有进入 `isDecide`，而 dummy decide 依赖跨帧位差，不是当前帧状态位覆盖；
- `keyconfig` role 在 `setonpush` 前执行，并额外保留了 `Timed input=1` 的原始点击穿透特判；
- 中键仍按连续编号 `3` 处理，而文档键 ID 为 `4`；
- `clickablethreshold` 使用 `<`，但文档要求只有 alpha **高于**阈值才命中，因此等于阈值也应透明；
- 重叠事件由下层处理器自己的 `penetration=1` 选择是否接收。文档语义相反：上层处理器
  的 `penetration=1` 才允许继续执行它下方的处理器，穿透链最终从下到上执行；
- SDL 触摸在手指抬起后合成一个桌面鼠标按下帧。它让点击发生在抬手时，但 Lua 看到的仍是
  Windows `isDownEdge` 位，而不是移动端文档要求的 `isUpEdge` 决定语义。

可借鉴的部分仅限经文档复核后的结构性线索，例如 `onEnterFrame` 必须先于输入冻结、覆盖按帧
清理、图层 click 先于全局 push 入队。参考实现中按具体游戏状态保留 hover、特判等待或脚本
队列的分支不移植到 Core；需要真实游戏 A/B 轨迹时只作为假设来源。

## 第一轮审阅结果及修订

第一轮确认了以下真实问题：

- `setonpush` 的事件派发结果被压缩成 `handled: bool`，无法表达“未注册、派发成功、过滤器假成功、过滤器假失败”；
- `overrideKey` 在 pointer/push/keyconfig 之后运行，不能按文档屏蔽本帧所有输入；
- `setonpush` 成功派发后，当前代码仍无条件调用 `handle_role_key_edge`，与事件失败才回退原功能的规则冲突；
- `isDecide(1)` 私自合并 Space，与 Space 默认隐藏消息窗冲突；
- `setonpush keyrepeat` 没有真正派发重复事件；
- 鼠标中键、`wait input=2`、`clickablethreshold` 边界存在文档偏差；
- rclick、link、drag 的部分优先级依赖 Core 自行猜测。

第一轮提出过一个临时方案：让 `Generic/Generic0` 不被 global push 吸收。进一步检查标准系统脚本后，此建议撤回。原因是成功派发的 `setonpush` 本来就应阻止该物理键的默认 role；只有派发失败或过滤器返回 `2` 才应回退。无条件穿透会导致 UI、选择和按钮点击同时推进剧情。

仍然成立的第一轮结论是：不要合入下面这些游戏特化方案：

- Core 轮询 `flg.exclick`；
- Host 修改 `btn.cursor` 或其他 Lua 私有表；
- Host 暴露 `host_decide_wake()` 绕过等待状态；
- 为 `scriptMainloop` 全面 coroutine 化 `calllua`。

## 第二轮：目标输入模型

### 1. 用一种状态位表示所有输入

在 `src/runtime/callbacks.rs` 中保留 raw state，但新增一个帧内有效视图：

```rust
bitflags::bitflags! {
    struct InputBits: u8 {
        const PUSH      = 2;
        const DOWN      = 4;
        const DOWN_EDGE = 8;
        const UP_EDGE   = 16;
        const DECIDE    = 32;
    }
}

struct EffectiveInputFrame {
    keys: BTreeMap<u32, InputBits>,
    pointer: PointerFrame,
}
```

这些位直接对应 `overrideKey` 文档，不再额外创造 `clicked`、`role_advance`、`scripted_down_edge` 三套语义。

Host 输入转换规则：

- Windows 鼠标按下：key `1` 获得 `PUSH|DOWN|DOWN_EDGE|DECIDE`；
- Windows 鼠标抬起：key `1` 获得 `UP_EDGE`；
- 移动端 tap 释放：由 Host/平台适配层在 key `1` 上生成 `UP_EDGE|DECIDE`；
- legacy `feed_click`：只生成一次 key `1` 的 `PUSH|DOWN_EDGE|DECIDE`，不建立 held 状态；
- `overrideKey` 对 raw bits 做替换，单键覆盖优先于全键覆盖。

### 2. 用枚举保留事件派发结果

把 `HandlerDispatch.handled` 替换为：

```rust
enum DispatchOutcome {
    NotRegistered,
    Delivered,
    SuppressedSuccess, // event filter = 1
    SuppressedFailure, // event filter = 2
    DeliveryFailed,    // file/label/handler 无法解析
}

impl DispatchOutcome {
    fn allows_default_role(self) -> bool {
        matches!(
            self,
            Self::NotRegistered | Self::SuppressedFailure | Self::DeliveryFailed
        )
    }
}
```

映射必须严格为：

| 情况 | 结果 | 是否执行默认 keyconfig role |
|---|---|---|
| 没有 `setonpush` | `NotRegistered` | 是 |
| filter 返回 0，处理器成功入队 | `Delivered` | 否 |
| filter 返回 1 | `SuppressedSuccess` | 否 |
| filter 返回 2 | `SuppressedFailure` | 是 |
| 处理器目标真实解析失败 | `DeliveryFailed` | 是 |

`queued` 和 `needs_return_frame` 可继续作为正交字段保留，不能再承担消费语义。

当前 `enqueue_handler_tags` 返回 `()`，无法在路由阶段区分成功派发与 file/label/handler
解析失败。应增加一个不执行脚本的目标校验入口，并让 enqueue 返回明确结果；否则实际派发
失败时仍会错误吞掉原按键功能。

### 3. 每帧只走一条固定管线

建议把 `CoreRuntime::advance_logic` 调整为以下顺序：

```text
begin raw input frame
    -> onEnterFrame（Lua 可读 raw input 并写 overrideKey）
    -> freeze EffectiveInputFrame
    -> pointer hit test + rollover/rollout
    -> lyevent click/drag dispatch
    -> setonpush dispatch
    -> 仅在 dispatch outcome 允许时执行 keyconfig role
    -> role 生成 EngineInputAction
    -> 当前 WaitReason 消费 action
    -> 执行/排空脚本标签和事件返回帧
    -> clear frame edges and overrides
```

`onEnterFrame` 必须移到任何 pointer、push、role 判断之前。Lua 在回调内先查询 raw edge、再调用 `overrideKey` 的写法仍然有效；回调返回后，Core 只使用最终 effective state。

### 4. 等待系统只接受动作，不接受来源布尔值

新增：

```rust
enum EngineInputAction {
    UserInput,
    Advance,
    MenuIn,
    MenuOut,
    HideIn,
    HideOut,
    BacklogIn,
    BacklogOut,
    BacklogPrev,
    BacklogNext,
    AutoIn,
    AutoOut,
    SkipIn,
    SkipOut,
    ForceSkip,
    AvoidIn,
    AvoidOut,
}
```

`UserInput` 表示一个没有被 pointer/setonpush 成功截获的有效输入边沿；`keyconfig` 再把
该 key 映射为零个或多个具体 action。默认 role 0 应包含文档列出的鼠标左键 `1`、
Enter `13` 和滚轮下 `137`；游戏重新配置 role 0 为 dummy key 后，物理左键不再隐式
拥有 `Advance`。

离散 role 使用 `DECIDE`/对应边沿，role 14 强制跳过使用 `DOWN` 持续态；不能再把所有
role 都绑定到原始 `keys_down_edge`。`advance_wait_state` 按等待类型消费 action：

- `Generic/Generic0`：接受 `Advance`；文字未完全显示时先 reveal，再次 `Advance` 才换页；
- `Timed input=1`：接受 `UserInput`，不要求该键恰好配置为 role 0；
- `Timed input=2`：不接受普通 `Advance`，仅在进入或保持 skip 状态时放行；
- `Stop`：不再查看“任意 override down edge”；只由明确的 `setScriptStatus(0)`、命名完成事件或经实机确认的 documented action 恢复；
- SE、video、transition 等等待保留各自完成条件。

删除：

- `global_push_absorbs_default_click`；
- `role_advance: bool`；
- `scripted_down_edge`；
- `physical_clicked` 作为通用等待推进信号。

dummy click 不需要特殊逻辑：`overrideKey(dummy, DECIDE)` 进入 effective frame，dummy key 没有 `setonpush` 时自然落到其 `keyconfig role=0`，生成 `Advance`。

### 5. Pointer 与键盘路由分离，但共享 DispatchOutcome

Pointer 只负责：

- 坐标、hover、hit-test；
- link、click、dragin/drag/dragout 的目标选择；
- 按文档执行 penetration 顺序。

键盘路由只负责：

- `setonpush`；
- `keyrepeat`；
- `keyconfig` fallback。

鼠标左键同时产生 pointer event 和 key `1` event，但两部分不要再混在同一个 `clicked` 返回值里。现有游戏依赖 layer click 先入队、全局 push 后入队，因此迁移时先保留这一队列顺序。

对于 link、drag 和 layer click 是否阻止默认 role，应由对应的 `DispatchOutcome` 决定。第一阶段保留现有行为；之后通过原引擎对照测试分别确认，禁止依据游戏名或图层 ID 特判。

## Issue #3 的定位与修复判定树

重构输入管线后，用通用遥测定位 Issue，不在 Core 中读取 `flg.*`：

```text
physical key 1 PUSH?
  no  -> Host 输入问题
  yes -> setEventFilter(setonpush) result?
           2 / no handler -> 是否执行默认 keyconfig role?
           0 / 1          -> 不得执行物理键默认 role
                               |
                               +-> handler 是否真正执行?
                               +-> 下一帧是否出现 dummy DECIDE?
                               +-> dummy 是否命中 role 0?
                               +-> 是否生成 Advance?
                               +-> 当前 WaitReason 是否消费 Advance?
```

根据结果修复：

1. filter 返回 `2` 但没有默认 action：修 `DispatchOutcome -> keyconfig fallback`；
2. handler 已设置 dummy decide，但没有 Advance：修 effective override/keyconfig 路由；
3. Advance 已生成但 Generic 不释放：修 `advance_wait_state`；
4. handler 成功执行但没有 dummy decide，且游戏仍处于 UI 状态：追查 UI close 的 tag queue/inline return frame，而不是强制点击穿透；
5. UI 状态本来就不应存在但清理函数没有执行：最可能的修复点是 `drain_queued_tags_while_waiting`、`settle_inline_event_frame` 或 jump/return 队列所有权；需要 issue 作者提供脱敏后的 PC、stack、queue before/after 轨迹。

## 按提交拆分的实施计划

### Commit 1：锁定现状，不改行为

建议提交标题：

```text
test(input): characterize event fallback and dummy decide routing
```

新增少量表驱动测试：

- event filter `0/1/2` 到派发结果和默认 role 的映射；
- 物理 key `1`、dummy key `124` 的状态位；
- layer click 在 push 之前入队；
- 当前 modal/UI 空白点击不会穿透。

### Commit 2：统一有效输入帧

建议提交标题：

```text
refactor(input): route each tick through an effective input frame
```

修改：

- `src/runtime/callbacks.rs`：raw/effective bits 和 override 应用；
- `src/runtime/mod.rs`：移动 `onEnterFrame` 阶段；
- `src/runtime/input.rs`：所有输入查询改读 effective frame；
- `crates/asb-interpreter/src/lua_engine.rs`：保持 API，不暴露新的游戏状态。

这一提交先保留现有 dispatch 顺序，避免同时改变太多语义。

### Commit 3：恢复原引擎的事件 fallback

建议提交标题：

```text
fix(input): run key roles only after unhandled push events
```

修改：

- 引入 `DispatchOutcome`；
- filter `2`/未注册才走 `keyconfig`；
- filter `0` 成功派发和 filter `1` 均阻止默认 role；
- 删除 `global_push_absorbs_default_click` 和无条件 `role_advance`。

### Commit 4：让 dummy decide 走 keyconfig

建议提交标题：

```text
fix(input): route overridden decide bits through keyconfig
```

修改：

- effective frame 枚举 override 产生的 key；
- `DECIDE` 命中 role 0 后生成 `Advance`；
- 删除 `scripted_down_edge`；
- bare Stop 不再因任意 override down edge 被唤醒。

这一步是 Issue #3 最关键的通用修复候选。

### Commit 5：机械性文档对齐

建议提交标题：

```text
fix(input): align key ids repeat waits and decide semantics
```

逐项修复：

- 鼠标按钮集合改为 `1 | 2 | 4`；
- 默认 role 0 补齐文档规定的鼠标左键 `1`，不再靠独立 `clicked` 路径推进；
- Space 不再隐式成为 `isDecide(1)`；
- `Timed input=2` 不接受普通点击；
- `setonpush keyrepeat=1` 使用 `PUSH` 位在 0.5 秒后逐帧派发；
- `clickablethreshold` 等于阈值时不命中；
- `getScriptStatus` 至少准确报告已经实现的 hide、dialog、HTTP 状态。

如果改动互不相关，仍建议拆为多个小提交。

### Commit 6：只在有证据时修 pointer 优先级

候选范围：

- rollover-only 顶层是否阻挡下层 click；
- draggable 层是否同时派发 click 与 dragin；
- link 命中时是否仍派发 key `1` 的 `setonpush`；
- rclick 启用时 key `2/27` 的 `setonpush` 优先级；
- rclick 内嵌套 call 后再次右键的 leave 行为。

这些必须用原引擎最小工程或真实游戏 A/B 轨迹决定，不能继续按“看起来合理”修改。

## 测试要求

建议使用一个自制的、没有商业资源依赖的最小 Artemis fixture：

1. 注册 `setEventFilter`；
2. 注册物理 key `1` 的 `setonpush`；
3. 将 dummy key `124` 配到 role 0；
4. `onEnterFrame` 把 Lua flag 转为 `overrideKey(124, DECIDE)`；
5. scenario 停在 `[@]`；
6. 分别覆盖 filter 返回 `0/1/2`、handler 接受/拒绝点击、UI modal、选择、普通正文。

核心断言：

- filter `0` + handler 拒绝：不推进；
- filter `0` + handler 注入 dummy：下一帧推进一次；
- filter `1`：不派发、不推进；
- filter `2`：不派发，执行物理键默认 role；
- 同一 decide 只消费一次；
- UI/选择按钮不会同时推进正文；
- `overrideKey{status=0}` 能阻止本帧 layer、push、role 和 wait；
- handler jump/return 后不会把同一 decide 误用于新页面。

建议验证命令：

```bash
cargo test runtime::input::tests
cargo test runtime::callbacks::tests
cargo test runtime::script::tests
cargo test --test runtime_inline_event_return
cargo test --test runtime_stop_queue_control_flow
cargo test
```

真实游戏验收至少包括：

- Lua `scriptMainloop/scriptMainAdd` 型作品：标题、OP 跳过、scenario 首句、连续翻页；
- 直接 AST chunk 型作品：标题到正文不回归；
- HENPRI：标题、config、返回标题、正文、选择、save/load/backlog；
- Host 原生菜单、dialog、全屏视频期间输入不穿透。

## 通用遥测

每个输入 tick 只记录以下引擎字段：

```text
raw_bits
override_bits
effective_bits
pointer_target
layer_dispatch_outcome
push_dispatch_outcome
default_role_allowed
generated_actions
wait_before / wait_after
script_pc_before / script_pc_after
queue_len_before / queue_len_after
```

不要在生产 Core 中记录或访问：

```text
flg.exclick
flg.ui
scr.mw.msg
btn.cursor
setonpush_calllua
scriptMainloop
```

这些名称只可由调试 Host 临时读取，用于确认游戏 Lua 是否按预期运行。

## 明确不采用的方案

- 不为某个游戏放行 Generic 原始点击；
- 不把 `setonpush` 处理器是否存在当成含糊的消费标志；
- 不让 Host 强制跳过脚本行或直接解除 wait；
- 不在 Core 中轮询 Lua 私有表；
- 不把 Space、Enter、鼠标点击合并成不可配置的硬编码 decide；
- 不通过当前脚本文件名猜测 UI/rclick 状态；
- 不在缺少 Lua yield 证据时 coroutine 化 `calllua`。

## 本轮验证状态

- 当前本地 master：`a3c4c15f164a7852d29f6c56b6be2234616cf6d0`，已包含
  `onEnterFrame -> effective frame -> layer/push -> keyconfig` 的主链重排；工作树继续补齐
  hit-test 失效条件、事件类型过滤和上层 `penetration` 控制语义。
- `cargo test --no-default-features --all-targets` 通过：341 passed、8 ignored；其中新增
  dummy decide/filter fixture、非 click 顶层不遮挡下层 click、上层 penetration 链测试。
- HENPRI `compatibility_probe` 使用原生 Metal 在沙箱外完成真实资源回归。命令行探针不实现
  宿主视频解码，开场全屏视频通过 `notify_video_finished(None)` 显式完成，之后按顺序验证：
  标题 CONFIG -> 左侧 SOUND 菜单 -> SOUND 页 ON 按钮（画面由 OFF 切为 ON）-> RETURN ->
  标题 START -> 正文背景。每次点击均命中预期图层，菜单切换后后续按钮和跨页按钮没有失效。
- 该结果覆盖已知的“菜单点击后按钮全部失效”形态，但不替代 Issue #3 原报告作品及真实
  Android/iOS Host 的最终验收；移动端还应由 Host 轨迹确认触摸抬起到鼠标兼容事件只转换一次。
