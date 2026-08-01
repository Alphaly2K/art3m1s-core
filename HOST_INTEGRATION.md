# 宿主（Flutter）FFI 对接清单

本轮功能补齐新增了若干 host 回调/通知 FFI。Flutter 宿主接上后，对应引擎功能即从
"保守默认"切换为真实行为；未接时引擎按安全默认运行（不崩、不卡）。

## 一、宿主需**注册**的查询/命令回调（启动时各调用一次）

| FFI | 用途 | 不接的后果 |
|-----|------|-----------|
| `art3m1s_register_font_query(fn(monospace:int, vertical:int, buf:*u8, cap:int)->int)` | `var system=get_font` 枚举可用字体族。把换行分隔的字体名写入 buf，返回字节数 | 字体列表为空 |
| `art3m1s_register_window_state_query(fn()->int)` | `var system=fullscreen/minimize`。返回位标志 bit0=全屏 bit1=最小化 | 恒返回非全屏、非最小化 |
| `art3m1s_register_file_stat(...)` | `var system=file_update_time` 查存档 mtime | 更新时间不可得 |
| `art3m1s_register_text_inject_callback(fn(text:*c_char, buf:*u8, cap:int)->int)` | 汉化/文本注入：每段剧本文本光栅化前经此替换。返回<0 表示不替换 | 原文直出（无注入） |

已有的 `art3m1s_register_ui_command_callback` 会收到这些 `kind`（JSON payload）：
`caption`（窗口标题）、`mouse`（光标）、`openbrowser`、`statusbar`、`vibrate`、
`write_clipboard`、`http_request`（带 serial，见下）、`avoid`（action=show/hide + file）、
`file_clear_cache`、`dialog_show`。宿主按 kind 落实对应平台能力。

## 二、宿主需在事件发生时**通知**引擎的 FFI

| FFI | 何时调用 |
|-----|---------|
| `art3m1s_runtime_feed_touch(rt, id:u32, phase:u8, x:i32, y:i32)` | 触摸 down(0)/move(1)/up(2)。驱动 getTouchCount/Point、flick、多点触控 |
| `art3m1s_runtime_notify_lifecycle(rt, state:int)` | 生命周期 0=退出/1=切后台/2=回前台。驱动 `[autosave allow=1]` |
| `art3m1s_runtime_notify_direction_changed(rt, direction:int)` | 屏幕方向变化。驱动 setondirchg + s.status.screendirection* |
| `art3m1s_runtime_notify_window_button(rt, button:int)` | 窗口按钮（最小化/关闭等）。驱动 setonwindowbutton |
| `art3m1s_runtime_submit_http_result(rt, status:int, body:*u8, len:int)->int` | 回填 httpget/httppost 结果，解除脚本挂起。收到 ui_command `http_request` 后异步请求，完成时调用 |
| `art3m1s_runtime_set_string_variable(rt, name:*c_char, value:*c_char)` | 宿主向脚本回写字符串变量（http/native 结果等） |

Profiler 为可选接口：宿主用 `art3m1s_runtime_set_profiler_enabled` 开关采样，并以
`art3m1s_runtime_profiler_snapshot` 异步、低频读取 UTF-8 JSON。渲染线程仅写入有界队列；
解释器、事件、E-Mote、合成器、DrawList、GPU 提交、present/readback 与宿主文件 FFI
分别计时。后台线程每约 500ms 发布一次快照，但平均值、峰值和吞吐率在本次 Profiler
开启期间持续累计；关闭后重新开启时重置。

## 三、仍属宿主 UI 职责（引擎已提供全部数据/通道）

- **回想（backlog）历史界面**：引擎经 `var system=get_backlog_size/get_backlog_tags` 提供
  逐页可重放的标签序列；历史界面由**游戏脚本**用这些数据自绘（Artemis 惯例），或宿主
  自建滚动 UI。引擎侧无需再做。
- **紧急回避（avoid）覆盖图**：引擎在 keyconfig role 15 触发时静音并发 ui_command
  `avoid{action:"show", file}`；全屏覆盖图的显示由宿主渲染。
- **内购（purchase）**：按用户要求不实现。

## 四、宿主接入现状（2026-07-27，Flutter 侧已落地）

`lib/services/core_bridge.dart` + `lib/screens/player_screen.dart` 已接入（`flutter analyze` 全绿）：

- **已注册回调**：`art3m1s_register_font_query`（`enumerateFonts` 返回随包+常见 CJK
  字体保守清单，monospace 分支另给等宽字体；Flutter 无系统字体枚举 API，宿主可换平台
  通道枚举）、`art3m1s_register_window_state_query`（返回 `windowStateBits`，bit0 全屏
  bit1 最小化，由壳层/生命周期更新）。两者用 try/catch 可选注册，老 core 未导出则跳过。
- **已处理 ui_command kind**：`avoid`（→ `avoidOverlay` notifier，player_screen 用全屏
  不透明 `ColoredBox` 即时遮罩）、`mouse`（→ `cursorHidden` notifier，`MouseRegion`
  用 `SystemMouseCursors.none` 隐藏光标）、`caption`（→ `windowTitle` notifier，捕获待
  壳层落实到 OS 窗口标题）、`write_clipboard`（`Clipboard.setData`）、`vibrate`
  （`HapticFeedback.mediumImpact`）、`statusbar`（`SystemChrome.setEnabledSystemUIMode`）。
- **已通知 FFI**：`art3m1s_runtime_notify_lifecycle`（`WidgetsBindingObserver`
  的 `didChangeAppLifecycleState`：resumed→2 / paused|hidden→1 / detached→0，驱动
  `[autosave allow=1]`；同时同步最小化位）。

仍未接（宿主按需扩展，core 侧均已就位）：`openbrowser`（需 url_launcher 依赖）、
`http_request`/`http_cancel`（需 http 客户端 + `art3m1s_runtime_submit_http_result`）、
`callnative`/`exec`/`file_wasm_sync`（平台特定）、`art3m1s_runtime_feed_touch`（移动端手势
转发）、`art3m1s_runtime_notify_window_button`/`notify_direction_changed`（桌面窗口/方向钩子）、
`caption` 落到 OS 窗口标题（需 macos_window_utils/window_manager）。backlog 历史 UI 惯例上
由游戏 Lua 自绘（多数游戏如此），引擎已提供 `get_backlog_*` 数据通道。
