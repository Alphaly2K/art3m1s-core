# Host 接入指南

本文面向 Flutter、原生应用及其他嵌入式 Host，以 [`src/ffi/api.rs`](../src/ffi/api.rs)
的 Artemis 版本化 C ABI、[`src/ffi/rfvp_api.rs`](../src/ffi/rfvp_api.rs) 的 RFVP ABI、
[`src/ffi/krkr_api.rs`](../src/ffi/krkr_api.rs) 的 KRKR ABI 和
[`src/ffi/siglus_api.rs`](../src/ffi/siglus_api.rs) 的 Siglus ABI 为准。它不是 Artemis 脚本
API 文档，也不要求 Host 使用 Dart 或 libmpv。完整签名、返回值和命令字段见
[FFI_REFERENCE.md](FFI_REFERENCE.md)。

### Artemis Rust Host 接入基础

Artemis 解释器在所有平台统一使用 Luau。现有 v1 C 函数表保持原布局；v2 可通过预览构建
接入，契约见 [HOST_ABI_V2.md](HOST_ABI_V2.md)。

### Artemis v2 C 预览

使用 `cargo build --locked --manifest-path crates/artemis-core/Cargo.toml` 编译纯 Core 动态库。
独立清单只产出 C 动态库，无需检出其他引擎。加载库后
通过 [C 头文件](abi/art3m1s_core_v2.draft.h) 中的 `art3m1s_get_api_v2` 查询 Runtime 和 Frame 表。
创建 runtime 的线程负责所有调用；工作线程把结果交回该线程。Host 循环推进逻辑、处理请求并
借用帧，绘制完释放令牌。只绘制精灵的 Host 可提交 `ART_RENDERER_CAP_SPRITE`；支持旧场景
捕获及转场合成的 Host 同时提交 `ART_RENDERER_CAP_TRANSITION`。
支持存档缩略图的 Host 可声明 `ART_RENDERER_CAP_SCREENSHOT`，按捕获请求中的帧 ID 绘制并保留
图像，再按保存请求写入 save 命名空间。
支持子场景离屏合成的 Host 声明 `ART_RENDERER_CAP_GROUP`，按帧包的组范围、效果与遮罩合成。
支持 E-Mote 材质、网格和 stencil 的 Host 声明 `ART_RENDERER_CAP_EMOTE`，并通过同一资源
协议提供 PSB 字节。
支持 Artemis HLSL 子集的 Host 声明 `ART_RENDERER_CAP_HLSL`，根据帧包交付的源码版本
和绑定编译绘制。使用 `SpriteRenderer` 时，可用 `clear_cached_shaders()` 释放已注册的程序。

[内存 Host 示例](../tools/host-smoke/artemis_v2.c) 不依赖窗口或游戏素材。以下命令构建预览库、
检查导出并运行示例。macOS 或 Linux 使用：

```sh
bash scripts/test-host-v2.sh
```

Windows 使用装有 MSVC x64 工具的 PowerShell：

```powershell
./scripts/test-host-v2.ps1
```

独立 [Dart 绑定](../tools/host-bindings/dart/lib/artemis_v2.dart) 从同一份 C 头文件生成。
使用 Dart 3.10 或以上及 libclang，从仓库根目录运行以下命令可重新生成并调用动态库
（库文件名按平台替换）：

```sh
cd tools/host-bindings/dart
dart pub get --enforce-lockfile
dart run ffigen --config ffigen.yaml
dart run bin/smoke.dart ../../../target/host-v2/debug/libart3m1s_core.dylib
```

Rust Host 可用 `art3m1s-render` 的 `artemis` feature 解码和绘制 v2 帧。macOS 离屏示例
通过 C ABI 借用帧并用 Metal 绘制，需要可访问的 Metal 设备，不需要窗口或游戏素材：

```sh
bash scripts/test-host-v2-metal.sh
```

macOS 应用也可以用 `cargo build --locked --manifest-path crates/artemis-metal-host/Cargo.toml`
构建独立渲染库，通过 [Metal Host C 接口](abi/art3m1s_metal_host_v1.h) 接收 v2 帧并呈现到
应用提供的 IOSurface。Core 和渲染库分别打包；调用顺序见 [v2 契约](HOST_ABI_V2.md#9-rust-公开面与-sdk)。
Swift 应用可使用同一契约中的 `ArtemisMacSession` 管理主线程上的 v2 会话与绘制，应用自行处理
资源、媒体及纹理注册。使用 `bash scripts/test-host-v2-swift.sh` 运行合成项目的异步 IO 与呈现检查。

Rust Host 通过 `host::ResourceProvider` 提供逻辑资源，通过
`CoreRuntime::with_host_backend` 提供 renderer，入口不自行选择平台 GPU：

```rust,no_run
use std::sync::Arc;
use art3m1s_core::{
    backend::GpuBackend,
    host::ResourceProvider,
    host_files::HostResources,
    runtime::CoreRuntime,
};

fn create_runtime(
    provider: Arc<dyn ResourceProvider>,
    renderer: Box<dyn GpuBackend>,
    ini: &[u8],
) -> Result<CoreRuntime, String> {
    let resources = HostResources::from_provider(provider);
    let mut runtime = CoreRuntime::with_host_backend(1280, 720, resources, renderer)?;
    runtime.load_project_bytes(ini, "WINDOWS")?;
    Ok(runtime)
}
```

provider 接收已解析 magic path 的逻辑 key（`/` 分隔、ASCII 小写），自行决定目录、归档、
内存和存档沙箱。允许短读；空文件与不存在分开。写入成功表示 Host 已完成该文件的原子替换。
注入 provider 后，挂载本机目录/PFS 和设置物理 save root 会被拒绝；clear 只清除 core
覆盖，不解绑 provider。读取、Luau 的 `e:file`/`io.open` 和媒体 source 都使用同一个资源实例。
未提供可用资源时，新构造入口报资源错误，不回退到工作目录。

`with_host_backend` 为每个 runtime 创建独立 `HostSession`，由 `runtime.host_session()`
取得。Host 通过 `session.events()` 推送窗口状态、字体列表和文本替换，并用
`poll_events(&mut buffer)` 拉取完整的 24 字节头记录；缓冲区不足时保留队首记录。
覆盖字体、debug 和脏区显示通过该 session 的方法设置。事件与配置不会切换旧 v1 的活动路由。
项目重载保留 Host 配置并更新该实例的脚本查询；读档保留该实例的 Host 查询服务。
旧 `create`/v1 runtime 的 `host_session()` 返回 None，继续使用原有全局兼容入口。

这是同步 Rust 接口，Host 方法不得重入同一个 runtime，也不应依赖正在等待 core 的 GUI
线程；它没有注册 native→Dart 回调。renderer 执行现有 DrawList/GpuBackend 契约，窗口、
GPU 对象和呈现由其实现管理。可用 `cargo build --no-default-features` 构建纯 Core 的
Rust 队列运行时。默认构建继续提供 v1 C ABI。

### v2 Rust 会话传输

`host::protocol::SessionTransport` 提供 v2 facade 的纯内存传输组件。创建时传入已校验的
非零 runtime handle 和 `TransportLimits`；句柄代际、owner 线程和服务 schema 由 facade
负责。它独立于 `HostSession` 的 v1 格式队列，不能直接替代当前 runtime 的同步执行路径。

`begin_session()` 递增 epoch，清除旧请求、结果和未交付输出，首先排入 `session_changed`。
core 在提交业务副作用前调用 `request(service, body)` 或 `notify(kind, body)`；
`WouldBlock` 表示本次没有受理，也没有消耗请求/序列 ID。两种 ID 在 runtime 生命周期内
不重用。`MessageBody` 保留 UTF-8 JSON object 和原始 blob；具体字段由服务层校验。

Host 用 `poll_events(&mut buffer)` 获取 56 字节头的完整小端记录。返回值同时给出状态、
记录数、写入字节数和下一条所需大小；`Buffer` 和部分成功均保留尚未交付的记录。
控制记录优先交付，诊断独立限额并计数丢弃。格式、flags 和默认预算见
[消息契约](HOST_ABI_V2.md#5-消息可靠性与能力协商)，实际预算与积压可通过 `limits()`/`state()` 查询。

Host 以 `HostReply` 批量回填，完整保留原请求的 runtime、epoch、request_id 和 service。
`submit_replies(&batch)` 先校验整批身份和容量，再复制并统一受理；失败保留全部请求。
它不执行脚本，core 只在 step 边界按顺序 `pop_reply()`。`cancel(identity)` 使本地请求失效；
外部任务取消由服务层另行处理。重载、取消、重复或串实例结果不会重新进入可执行队列。

### Luau IO chunk 驱动

`host::script_io::ScriptIoSession::new(handle, config, limits, max_file_bytes)` 创建不安装物理
文件加载器和 renderer 的解释器。`open_chunk(bytes, name)` 编译并替换当前任务，递增
epoch；语法错误保留旧任务。相同解释器的 globals 保留，换游戏应创建新实例。

Host 循环调用 `step()`、`poll_events()` 和 `submit_replies()`。只有 `step()` 执行 Lua，
等待或容量阻塞时保留原调用栈；提交结果后需再次 step 才恢复执行。`cancel_pending()`
既可取消已排队的请求，也可取消因容量不足尚未派发的操作；取消分支同样在下一次 step 执行。

支持 `e:file(path)`、`e:isFileExists(path)`、嵌套 `e:include(path)`，以及 `io.open` 的
读写/追加/更新模式、seek、lines、flush 和 close。读取先 stat，再按 revision 分块，允许
短读且要求前进，空文件不发 read。累计大小受 `max_file_bytes` 限制。写入使用单 blob
原子请求，受传输 blob 预算限制；成功确认后才清除 dirty 状态，失败保留文件供脚本重试。
`io.open` 失败返回 nil/error，flush/close 失败产生可由 `pcall` 捕获的 Lua 错误。
`e:file{command='delete', target=key}` 与 `[file command=delete target=...]` 均等待 Host 确认。
目标只在 save 命名空间解析，可省略配置的 save 前缀。Lua 调用成功返回 nil，失败返回
`nil, message, kind`；场景标签报告失败后继续。copy/move 尚未接入可靠 IO。

资源 key 使用 UTF-8、`/` 和 ASCII 小写，拒绝绝对路径、驱动器、NUL 和 `..`。
`config.savepath` 为逻辑 save 前缀，默认 `save`；此前缀下的路径映射到 save namespace
并剥离前缀，其余路径映射到 asset。写入必须使用 save 前缀。此 chunk 接口不解析 magic path，
不执行 ASB/AST 调度，不推进输入或 onEnterFrame。文件 var 查询与直接文件方法共用同一 IO 路径。
Host 提交原始场景字节和编码配置，完整帧调度由调用方管理。

`open_scenario(bytes, name, label)` 解析 Host 提交的 AST/IET/ASB，并在新 VM 中受理场景。
加载期 Lua 块在 step 中执行完后，才进入标签流；内联与排队的 calllua 支持相同 IO 挂起。
`ScriptIoProgress::Event` 表示 `pending_event()` 可读取一个普通场景事件，Host 处理/等待后
调用 `resume_scenario()` 释放该等待。等待 IO 时 PC 由 continuation 管理，Host 不额外加一。
替换场景使旧 VM 和 IO 失效；解析或起点标签校验失败则保留旧场景。Lua 执行错误在 step 报告。
tag filter 支持相同挂起路径，保留通过/拦截、动态展开和排队顺序。跨文件 call/jump、
macroadd 及栈定位遇到未加载脚本时，通过相同的 Host 读取协议暂停和恢复。场景及其标签
缺失会使会话报错；可选宏文件读取失败沿用继续执行的行为。
Rust 的 `ExecutionResult`/`TagResult` 新增 `HostIoPending`，穷举匹配需处理该分支；
未启用 resumable IO 的解释器不产生它，v1 C 函数表与同步路径保持原语义。

`open_boot(name)` 创建新 VM，先从 save 命名空间恢复可选的 `saveg.dat`、`system.dat` 和 `aread.dat`，
再按逻辑名称请求 BOOT 和可选的 `macro.iet`。缺少或读不出系统文件时继续启动；只恢复可持久化
的设置。脚本支持文本与 ASB，按 top/main/start/_start/文件开头选择入口。文件请求和加载在 step
中推进。替换会话会使旧请求失效，非法逻辑名称保留旧会话。

`open_project(ini_bytes, platform, reported_os)` 解析所选 `system.ini` 分节并设置舞台尺寸、
脚本编码和逻辑存档前缀。该入口在读取持久状态和启动 BOOT 前先请求可选的 `tag.ini`，
以配置的编码解码并应用到脚本解析。若 INI 或逻辑路径校验失败，旧会话继续有效。

`host::runtime` 是 Artemis 的 Rust 句柄入口。`create`、`open_project`、`step`、
`poll_events`、`submit_replies`、`get_state` 和 `destroy` 均由创建线程调用。运行时需在
该线程退出前销毁。工作线程把结果投递给创建线程；跨线程直调返回 `Thread`，已销毁的句柄
返回 `Handle`。`get_state` 分开报告项目阶段、IO 等待和场景等待。`session_changed` 后可以
先收到启动 IO；`project_ready` 在 BOOT 与加载期 Lua 完成后发布，表示项目已进入 Running。
标题与对话框通过 [UI 消息](HOST_ABI_V2.md#5-消息可靠性与能力协商) 交付。标题无需回复；
对话框只接受 `poll_events` 已交付的请求身份。`submit_replies` 只入队，脚本在下次 `step`
处理结果。直接使用 `ScriptIoSession` 时，场景事件仍由调用方通过 `pending_event()` 处理。

用 `submit_input(handle, epoch, &[HostInput])` 提交键盘、鼠标或触点输入。Host 将按钮映射到
Artemis 键码，并把屏幕坐标换算为舞台坐标。Core 在下一个受理的新帧应用输入，在 Lua
回调及场景等待中使用该快照。IO 回填后重试同一帧时继续使用原快照。失焦时提交 `FocusLost`。

音频命令通过 `poll_events` 交付，Host 根据逻辑资产 key 读取、解码和播放。用
`submit_playback_status(handle, epoch, playback_id, status)` 回报开播及终态；`playback_id`
以本次命令为准。命令和反馈格式见 [媒体契约](HOST_ABI_V2.md#8-媒体ui配置与诊断)。
全屏视频由 Host 呈现。图层视频由 Host 解码后调用 `submit_video_frame`，Core 将其放进场景帧，
由同一 Host renderer 绘制。视频终态也通过 `submit_playback_status` 提交。

收到 `exit_requested` 后，调用 `submit_shutdown(handle, epoch)`。继续 `step`、拉取写入请求
并回传结果，直至收到 `runtime_closed` 和其中的存档结果。Host 也可主动请求关闭；仍有 IO 或
对话框结果待处理时需先完成这些请求。`destroy` 可以直接释放句柄并放弃未确认的写入。

`acquire_latest(handle, after_render_frame_id)` 借用最新基础图层帧，`None` 表示暂无更新。
需要文本时，在 `open_project` 后、首帧前用 `set_default_font(handle, epoch, bytes)` 提交字体
数据。字形和图集通过同一帧接口交付。
Host 从快照的纹理表读取像素，并按 `sprites` 的顺序绘制。`[flip]` 提交当前视觉状态，
IO 挂起期间可继续呈现上次取得的帧。借用跨重载有效；单个运行时最多借用两帧，销毁前需
归还。`packet()` 可读取同一快照的二进制描述，像素继续通过纹理表借用。格式、蒙版和资源
版本约定见 [帧契约](HOST_ABI_V2.md#7-画面core-帧契约与-host-renderer)。

使用项目自带 GPU 后端时，将 Host 创建的后端传给 `SpriteRenderer::new`。Host 通过
`backend_mut()` 配置目标及舞台映射，调用 `begin_access`、`begin_frame` 并清理目标，
然后调用 `render(&lease)`。成功后由 Host 结束帧并呈现；出错时也要结束已打开的访问和帧。
转场的旧场景和新场景用相同的背景色；自定义背景时调用 `render_with_clear_color`。
清理适配器缓存须在后端上下文可用时调用 `clear_cached_textures()`。

句柄入口的 `step` 会自动处理 `[save]` 和编号 `[load]`。Host 按现有 IO 请求读写并回填结果，
Core 会在继续场景前将 `persistence_result` 排入可靠事件队列。成功读档从恢复后的脚本位置
继续。编号快照覆盖解释器、场景图层、缓动进度、当前消息页和活动音频。读档时 Core 通过
Host 预读缺失的图层图片、蒙版和字体，恢复文字，并发送从起点播放的音频恢复命令。

独立解释器启用 resumable IO 后，未安装同步加载器时，start/boot/restore_position 可能
受理一个尚待 Host 完成的加载，调用方随后处理 HostIoPending。显式安装的同步内存加载器
仍可使用；ScriptIoSession 始终通过请求传输读取。

场景及 Lua `e:tag` 的 `file_exists`/`file_exist`、`file_crc`、`file_update_time` 均等待
Host 结果再更新变量，支持原有 CRC 大小写/前缀、日期 format/noexist 和 writelocal。
save=1 使用配置的逻辑 save 前缀；`.exe` 存在性检查保留引擎的虚拟存在语义。
修改时间由 stat 的可选 `modified_utc_ms` 提供，以规范十进制 i64 字符串传输，日期
分量使用 Host 提交的时区偏移；未提供时间时使用 noexist，不查询系统文件或本机时区。

未使用帧驱动时，`set_host_time(HostTime { monotonic_ms, utc_unix_ms, utc_offset_minutes })` 仅更新时钟快照，
不执行 Lua。单调时间用于 e:now/os.clock 和等待截止时间；日历时间用于 os.time/date、
date/unixtime 变量和文件日期。时区偏移范围为 -1440 到 1440 分钟；os.date 的 `!` 前缀
始终使用 UTC，os.time(table) 按当前 Host 偏移解释本地时间。UTC 可回拨，Host 应保持
单调时间不回退。

场景可用 `step_frame(HostFrame { id, monotonic_ns, utc_unix_ms, utc_offset_minutes })` 推进。
每个新 ID 递增一次脚本帧号；加载期 Lua 先完成，随后执行至多一次 `onEnterFrame`。
同一 ID 必须保持相同的单调时间，可以更新 UTC 和偏移；旧 ID 或回退的单调时间会被拒绝。
IO 等待期间，新帧返回 `WouldBlock` 且不受理；提交结果后以相同帧 ID 或 `step()` 恢复。
若开始帧驱动前已有未完成的 IO，应先用 `step()` 完成该操作。
直接使用 `ScriptIoSession` 时，已有场景事件保持到 `resume_scenario()`。帧回调产生的标签
在确认该事件后继续派发，模态对话期间暂停帧回调。

已完成命中判断的事件可用 `submit_event_filter(tag, params)` 排队，参数使用注册标签名和原始
字符串参数。一次只能有一个待判定事件；旧 IO 未完成时返回 `WouldBlock`，调用方保留原事件。
过滤函数在 `step` 或 `step_frame` 执行，可以通过同一 IO 通道暂停。收到 `EventFilterReady`
后调用 `take_event_filter_decision()`。`DispatchDefault` 继续原派发，`Handled` 和 `Rejected`
都抑制原派发，分别表示成功和失败。未安装过滤器时继续默认派发；过滤器报错时拒绝。
取走判定前不会推进场景。已有场景事件仍须单独确认。替换会话会清除过滤请求和判定。

使用 `host::runtime` 或 v2 C 入口时，左键和指针坐标由 `submit_input` 或对应 C 消息提交。
图层点击、进入和离开由 Core 命中并执行过滤器；过滤期间的 IO 沿用原帧恢复规则。

使用纯脚本驱动时，收到 `Event::SaveGame` 可调用 `submit_save()`。有文件名时，`step`
运行 `onSave` 及它排队的状态更新，等待回调内的 IO，并生成解释器快照。回调可以
修改存档变量、执行 Lua 和请求 IO；若尝试跳转、等待或派发普通场景事件，本次保存失败。
调用方原有的标签队列保留。快照使用现有 `SaveData` 格式，记录存档事件后的脚本位置，
当前只包含解释器状态。有文件名时先写编号存档，随后依次写 `saveg.dat` 和 `system.dat`；
无文件名时直接写后两份，不调用 `onSave`。如果出现新已读位置，最后写 `aread.dat`。
收到 `SaveReady` 后用 `take_save_result()` 取走成功或失败，再用
`resume_scenario()` 继续。每份文件单独提交，全部确认才返回成功；失败时 `files_written`
记录已确认的文件数。`aread.dat` 失败会保留待保存的已读记录；画面和音频快照仍由运行时集成承担。

首次返回一条正文事件时，`s.status.alreadyread` 表示该脚本位置在这次派发前是否已读。
宏文本使用宏调用者的位置；重复查询同一个待确认事件不会刷新已读状态。

收到 `Event::LoadGame` 后可调用 `submit_numbered_load()`。Core 先读取并校验解释器快照，
缺少目标脚本时再请求其字节。成功恢复位置后，加载期 Lua 先于 `onLoad` 运行，两者均可挂起。
收到 `LoadReady` 后取 `take_numbered_load_result()`：成功已替换旧场景及队列，直接继续
`step`；读档失败则确认原事件后继续旧场景。快照显式记录的输入等待会在 `onLoad` 排队标签
执行后恢复。恢复已生效后，Lua 初始化或 `onLoad` 的未处理错误会使会话进入故障状态。
此入口只接受解释器快照；含画面、音频或消息文本快照的完整存档仍由原运行时处理。

新会话时钟和随机种子默认为零，Host 在首次 step 前提交时间，并可在首次 open 前调用
`set_random_seed(u64)`。e:random 与 var random 使用实例内状态；Luau math.random
使用种子的低 32 位。新场景 VM 使用相同配置重新初始化，chunk 替换保留当前 VM 状态。
变量快照恢复保留当前实例的时钟、随机状态及匿名消息层 ID 序列，不从存档恢复环境服务。

IO domain 的 kind=1 为 stat、2 为 read、4 为 write_atomic、5 为 delete，结果保留请求的 service kind。
具体 payload 见 [IO 契约](HOST_ABI_V2.md#6-iohost-实现core-保留语义)。底层
`asb_interpreter::io_continuation` 可独立使用：在新解释器加载源码前 `install()`，编译
`IoTask`，以 `step(Some(response))` 从当前 IO 点继续；重复等待不重新执行脚本。

## 1. 职责与能力边界

| Core | Host |
|---|---|
| 解释脚本、计时器、游戏输入绑定 | 窗口、系统事件、输入坐标转换、帧调度 |
| 图层树、文本/Ruby、E-Mote、转场与合成 | 展示输出帧、创建和管理平台共享表面 |
| 存档序列化、游戏变量与播放状态 | 资源读取、存档落盘、游戏数据隔离 |
| 发送媒体命令、等待播放完成 | 解码、播放、混音、全屏视频、完成通知 |
| 发出对话框、网络、翻译等请求 | 原生 UI、网络权限、翻译服务与异步任务 |

Host 只依赖引擎总抽象，不应直接依赖某个 adapter 的内部类型。当前有四个独立入口：

| 引擎 | 查询入口 | 当前状态 |
|---|---|---|
| Artemis / Art3m1s | `art3m1s_get_api_v1` / `art3m1s_get_api_v2` | v1 为生产路径；v2 为独立预览 |
| RFVP | `art3m1s_rfvp_get_api_v1` | 独立 adapter；默认 feature `rfvp-engine` |
| KRKR / Kirikiri | `art3m1s_krkr_get_api_v1` | 早期 adapter；仅在使用 `krkr-engine` 且 native shim 可用时存在 |
| Siglus | `art3m1s_siglus_get_api_v1` | 早期 adapter；`--features siglus-engine`，macOS Host 已接入；Kira 只混音，Host 通过独立 PCM ABI 输出声音；视频/对话框仍待迁移 |

各入口不共享 runtime、资源、事件或 surface 对象。Artemis Host 根据构建产物查询对应版本的
函数表；RFVP、KRKR 和 Siglus 使用各自入口。媒体解码可由 v1 runtime 的 FFmpeg session 承担；
v2 通过 Host 媒体协议解码。真实音频输出和最终 present 由 Host 所有。
游戏自己的 save/load/config/backlog 通常由脚本绘制，不需要 Host 重写。

跨边界规则：

- 对象与生命周期走不透明句柄，例如 `CoreRuntime*`。句柄内容、Rust trait、C++ vtable
  或平台对象布局都不能直接暴露。RFVP/KRKR 的 `u64` runtime 是代际句柄：失效、伪造
  或二次销毁返回 `INVALID_HANDLE`，core 不解引用未知数值；facade 调用在句柄表上
  串行化，但这不替代下文 owner 线程约定。
- 数据走裸指针和显式长度，例如像素、INI、字体、替换表、HTTP body 和 uniform block。
  零拷贝数据仍由调用方保证调用期间有效，不由 core 猜测容器布局。跨边界的字节流
  （host-events 事件头、RFVP 日志记录头）一律固定小端。
- 只有 `Art3m1sApiV1`、`Art3m1sRfvpApiV1`、`Art3m1sKrkrApiV1`、`SiglusApiV1`
  及其独立诊断/音频函数表这类定长、版本化的 POD 结构可以按地址跨边界；
  不要新增按值传递的复杂对象结构。
- 通信方向是 Host 调 core、core 排队、Host 拉取。不要新增 native→Host 回调入口；
  RFVP 的 `runtime_set_log_callback` 是仅存的迁移期例外，不能安全暴露回调蹦床的
  宿主（如修改过的 iOS 设备上的 Dart `NativeCallable`）必须改用
  `log_next_bytes`/`poll_log`。

Siglus ABI 特别约定：`u64` handle 进程内不重复使用，但 VM 含 `Rc`，只能在创建它的
同一线程访问；错误线程返回 `STATUS_HANDLE`，销毁也必须在 owner 线程执行。
`siglus-engine` 同时启用进程内 FFmpeg 解码 UCI/MARK 图片，不调用系统
`ffmpeg` 命令；Host 打包与 core 链接兼容的 FFmpeg 动态库。
`runtime_tick` 的 `mode=0/1/2` 分别推进、回读 RGBA、向 Host 表面 present；
`mode=1` 的输出缓冲至少 `stage_width × stage_height × 4` 字节。
`runtime_set_external_surface(kind=0)` 在 Host 释放 surface 前解绑。
Siglus 另有可选、拉取式 `art3m1s_siglus_get_diagnostics_api_v1`，Host 应在创建、
每帧及销毁后拉取日志并按 `E/W/I/D` 级别持久化；调试模式通过有效 runtime 句柄
调用 `runtime_set_debug`。它只控制上游 Rust `log` facade 的 Debug 输出，
不提供 VM 断点或 profiler，详见 [FFI_REFERENCE.md](FFI_REFERENCE.md#siglus-abiv1)。
Siglus 上游在 `art3m1s-host-audio` feature 下以无设备 Kira 后端解码/混音；
`art3m1s_siglus_get_audio_api_v1` 由 Host 按播放时钟拉取 48 kHz 双声道 f32 PCM，
macOS Flutter Host 将其提交到自身拥有的 AVAudioEngine。VM 不直接打开音频设备。
冻结/挂起时 Host 暂停播放并停止拉取 PCM；其他平台的 Siglus 音频 Host 尚未接入。
Siglus 的存档路径目前也仍由原 VM 决定（通常位于游戏目录）；
`EngineRuntime.setSaveDir` 暂未接线，不能按 Artemis 的每游戏沙箱隔离保证处理存档。

### 构建与加载

- 默认图形 feature 提供 runtime 调用；FFmpeg 视频 session 额外要求 `ffmpeg` feature。
  新宿主只检查 API 表中的函数指针是否为 NULL，不把 feature 差异编码成不同的 ABI 布局。
- 动态库、ANGLE 和可选媒体库由 Host 打包/加载。媒体库应只加载一份实例，避免重复
  全局状态或 Objective-C 类。构建方式见 [README](../README.md#构建)。
- `art3m1s_get_api_v1` 必须返回匹配的 `struct_size`、`abi_version` 和 `magic`；否则
  拒绝读取函数表。可选能力通过函数指针是否为空判断，不再用散装符号探测。
- JSON 的未知字段应忽略；未知命令记录一次诊断。必须回应的已知请求不能静默丢弃。

## 2. 线程、指针与进程级状态

**推荐一个专用 owner 线程串行持有 runtime。** 创建、加载、推进、输入、结果回填、
视频 GL 调用和销毁都在此线程执行。GUI/网络/解码线程只往 owner 队列投递消息。
`*const CoreRuntime` 不等于允许并发读取；FFI 没有通用并发调用保证。
`advance_without_render` 也可能处理纹理加载等 GL 工作，不是任意线程可调用的纯逻辑 API。

文件资源由 Host 通过 `resources_mount_*` 一次性提交到一个 `HostResources` 句柄，
再通过 `runtime_set_resources` 绑定给 runtime；不再注册逐次读取回调，也不依赖进程全局文件表。
host-events 队列、窗口状态、字体表和文本替换表归独立 `HostEvents` 句柄所有；任意 core
线程可写入当前启用句柄，Host 在 owner 线程批量 poll。媒体/UI 事件复制到 Host 队列后
应立即返回。不要等待被当前调用阻塞的 GUI 线程，也不要把语言异常或 panic 抛过 C 边界。

| 数据 | 所有权与有效期 |
|---|---|
| `CoreRuntime*` | Core 分配；不透明；不能制造第二个所有者；仅用 `runtime_destroy` 释放一次 |
| `HostResources*` / `HostEvents*` | Core 分配；不透明；生命周期归创建它的 Host；分别用 `resources_destroy` / `host_events_destroy` 释放一次 |
| 传入字符串/INI/结果/视频帧 | Host 所有；调用期间有效且不可同时修改；调用返回后可释放 |
| 资源挂载路径与覆盖字节 | 调用期间借用；成功挂载后 core 保存索引/副本，Host 可释放原参数 |
| 帧像素、caption、profiler 缓冲 | 调用方提供；只能写容量内，不替换指针、不释放 |
| host-events 记录 | `poll_events_v1(events, ...)` 返回后由 Host 拥有；同一事件不会再次返回 |
| 视频 FBO 名称 | Core 所有，仅在有效 GL lease 中使用；不能删除或跨 context 使用 |
| 外部平台表面 | Host 所有；绑定期间保持强引用，解绑且消费者完成后才释放/复用 |

除显式声明可空的参数外不要传 `NULL`。判空不代表非法指针、短缓冲或重复销毁安全。
部分入口捕获 Rust panic，但这不是内存安全检查或崩溃恢复保证。

### 当前不是完全多实例隔离 ABI

- 文件挂载已经绑定到独立的 `HostResources` 句柄；runtime 不直接碰进程全局资源状态。
- host-events 队列和宿主状态已经绑定到独立 `HostEvents` 句柄；但 core 内部日志、media、
  UI 产生点仍通过“当前启用句柄”路由，因此当前一个进程只应运行一个活动游戏会话。
- `art3m1s_set_angle_path` 使用进程级一次性设置。存档根通过
  `resources_set_save_dir` 在资源句柄内切换；媒体/UI 输出仍由 Host owner 队列消费。
- 调试开关和部分内部快照也有进程级状态。当前建议一个进程只运行一个游戏会话；
  不要一边运行游戏，一边替换全局资源挂载做 caption 探测。

## 3. 资源、编码与存档

先安装资源命名空间，再加载项目。Core 传逻辑路径，Host 根据当前游戏映射到解包目录、
具体 PFS 条目、补丁覆盖或可写应用数据目录。

Host 先通过 `resources_mount_directory` 或 `resources_mount_pfs` 向资源句柄提交资源根。
Core 建立索引后，解释器只使用逻辑路径；Host 不再提供逐次读取回调。

PFS 多卷和补丁覆盖在 core 内按统一索引解析；目录挂载只暴露选中目录。存档写入通过
`resources_set_save_dir` 指定的独立根目录完成，不会回写资源归档。覆盖内容通过
`resources_set_override` 提交，core 复制后参与后续读取。

### 编码与路径

- 优先调用 `runtime_load_project_bytes`，传原始 INI 字节。Core 按所选节的 `CHARSET`
  解码，缺省 Shift_JIS；`load_project` 则要求 UTF-8 字符串。
- `platform` 是 INI 节名，例如 `WINDOWS`、`ANDROID`、`IOS`，不是图形后端，也不必与
  当前 Host OS 一致。没有对应节会失败；舞台尺寸与 BOOT 以该节为准。
- Host 打开 PFS 时也要选择文件名编码。独立 API 见
  [`crates/pfs-upk-rust/src/lib.rs`](../crates/pfs-upk-rust/src/lib.rs)；不应假定 core 动态库
  一定导出依赖库的 `pfs_*` 符号。
- C 字符串、JSON 和回调路径始终为 UTF-8；文件内容是原始字节，不要统一转码再交给 core。
- 解包模式只访问选中目录，不暗中寻找同名 PFS。PFS 模式绑定具体归档，不只绑定父目录。

### 游戏隔离

INI 的 `SAVEPATH=savedata` 会产生 `savedata/save0001.dat`、`savedata/saveg.dat` 等路径。
Core 已添加逻辑前缀，Host 不要重复添加。将这些路径映射到当前游戏专属的可写根目录，
不能只用常见的 `root.pfs` 文件名作为游戏身份。

编号存档与 `saveg.dat`、`system.dat`、已读记录是不同文件。load 旧编号存档不能回滚
整个可写目录。封面和翻译缓存等 Host 元数据也应由稳定游戏 ID 隔离。

读档会清理旧场景图层及其处理器，但保留游戏启动时注册的全局 `seton*` 事件；
它们由脚本显式 `delon*` 或重新注册管理，不随消息窗重建而注销。

Host 最终负责拒绝目录穿越、越权绝对路径和不允许的写入位置。脚本发起的 `exec`、
`shell_execute`、`callnative`、网络及购买请求不是可信授权，按 Host 权限策略处理。

`probe_caption` 不创建 GL，但会使用传入资源句柄中已挂载的资源运行解释器。它不是无副作用
的文件名扫描器，必须有对应资源上下文且与正在运行的游戏隔离。返回 `0` 包括未探测到、
出错和缓冲不足。

## 4. 启动与帧循环

下面是顺序伪代码。除 `art3m1s_get_api_v1` 外，所有 `*_v1`、`runtime_*` 和 `set_*`
名称都表示 `Art3m1sApiV1` 中的函数表字段；`host_*` 由 Host 实现，不是导出符号：

```text
resources = resources_create()
resources_mount_directory(game_root)            // 或 resources_mount_pfs
resources_set_save_dir(save_root)
events = host_events_create()
host_events_enable_v1(events, 1)
set_font_list_v1(events, ...), set_window_state_v1(events, ...)
set_text_replacements_v1(events, ...)            // 可选
set_font_override(font_bytes, font_len)          // 可选：译文缺字时覆盖运行时字体
set_angle_path(directory)                       // 第一次加载 ANGLE 之前

rt = runtime_create(initial_width, initial_height, gfx_backend)
if rt == NULL: report_error_and_stop()
runtime_set_resources(rt, resources)             // 必须在 load_project 之前
if runtime_load_project_bytes(rt, ini, ini_len, platform) != 0:
    runtime_destroy(rt)
    report_error_and_stop()
width = runtime_stage_width(rt)
height = runtime_stage_height(rt)
host_allocate_output(width, height)
shared = host_try_attach_external_surface(rt)   // 失败则使用像素路径

on_each_engine_tick:
    host_drain_input_and_async_replies(rt)      // 校验会话 generation
    host_render_latest_layer_video_frames(rt)  // 无新帧不提交
    delta_ms = host_monotonic_elapsed_ms_with_fractional_carry()
    if host_output_busy:
        runtime_advance_without_render(rt, delta_ms)
    else if shared:
        result = runtime_advance_and_present(rt, delta_ms)
        if result == 1: host_mark_frame_available()
        if result < 0: host_detach_and_schedule_pixel_fallback()
    else:
        n = runtime_advance_and_render(rt, delta_ms, pixels, capacity)
        if n > 0: host_present_or_copy_before_reusing_buffer(pixels, n)
    host_drain_events(rt)                        // host-events v1，批量处理
    host_dispatch_queued_media_and_ui_commands()
    if runtime_is_exit_requested(rt): host_begin_shutdown()
```

- 创建尺寸须非零且合理；加载会按 INI 调整舞台，随后查询尺寸与 `pixel_buffer_size`。
  没有独立公开的舞台 resize 接口。
- 每个 tick 只调用一个推进 API，它们都会消费输入边沿、执行脚本与计时器。不能先
  `advance_without_render` 再用同一 delta 调 `advance_and_present`。
- 用单调时钟传经过时间，不是绝对时间。60 Hz 不能始终传 `16`，应携带小数余量；
  显示器刷新率、引擎 tick 频率和视频帧率也不能混为一谈。
- 静止帧返回 `0` 只是没新像素，不表示退出。继续驱动计时器、`onEnterFrame`、
  auto/skip 和媒体完成处理，不要因暂停渲染而停止音频。
- 恢复前台时重置帧时钟基准，别把后台时长作为单帧 delta，也别忙循环补数千帧。
  后台媒体策略由 Host 决定。

### 4.1 KRKR/Kirikiri 适配边界

KRKR 使用独立的 `Art3m1sKrkrApiV1`，不是在 `Art3m1sApiV1` 上追加一组字段。Host
只传游戏根目录、输入事件、显示尺寸和可选配置；TJS2/KAG/XP3、补丁覆盖、内部图层和
软合成由 KRKR C++ runtime 自己管理。不要把 KRKR 项目伪装成 Artemis `system.ini`
项目，也不要向它传 `HostResources`、`CoreRuntime` 或 Artemis 的 PFS/存档句柄。

典型顺序如下。实际接入仍应逐项检查每个函数的返回状态：

```text
krkr = art3m1s_krkr_get_api_v1(&api_size)
validate krkr.struct_size, krkr.abi_version == 1,
         krkr.magic == 0x31564B524D334152 ("RA3MKRV1")

probe = zeroed Art3m1sKrkrProbeV1
probe.struct_size = sizeof(probe)
if krkr.probe_project(game_root_utf8, &probe) != OK or probe.preferred_kind == 0:
    reject_or_use_another_engine()

config = zeroed Art3m1sKrkrRuntimeConfigV1
config.struct_size = sizeof(config)
config.width/config.height = initial_display_size
config.audio_sample_rate = 48000
config.audio_channels = 2

rt = NULL
if krkr.runtime_create(game_root_utf8, NULL, &config, &rt) != OK:
    report_error_and_stop()
width = krkr.runtime_stage_width(rt)
height = krkr.runtime_stage_height(rt)
host_allocate_or_resize_presentation(width, height)

on_owner_thread_each_tick:
    krkr.runtime_push_input(rt, events, event_count)   // 可为 0 条事件
    krkr.runtime_tick(rt)                              // 生产 render-host 模式请求异步推进
    host_drain_krkr_audio(rt)                           // 取到 NO_COMMAND 为止
    frame.struct_size = sizeof(Art3m1sKrkrFrameV1)
    status = krkr.runtime_acquire_frame(rt, &frame)
    if status == OK:
        host_present_or_copy_before_release(frame)
        krkr.runtime_release_frame(rt, frame.frame_id)
    else if status != NO_FRAME:
        report_engine_error()
    if krkr.runtime_is_exit_requested(rt):
        begin_shutdown()

krkr.runtime_destroy(rt)
```

- 全部 KRKR 调用应在同一个 owner 线程串行执行。创建、输入、tick、帧获取/释放、音频
  提交和销毁都不能与另一线程并发调用同一个 runtime。
- Kirikiri 的 TJS 对象池是进程全局的。`runtime_destroy` 会走到
  `TVPUninitScriptEngine()`，之后同一进程再 `runtime_create` 会在
  `tTJSScriptBlock` 构造时 SIGSEGV。Host 必须把 KRKR 会话按 `frozen` /
  `suspended` 保活（与 RFVP/Artemis 一样），回主页时不要销毁 runtime。
  若运行时已被销毁，adapter 会返回 `ART3M1S_KRKR_STATUS_ENGINE` 而不是再次初始化。
- `runtime_tick` 没有 delta 参数。无 render-host 的原生 smoke 同步推进一次 KRKR
  application iteration；生产 render-host 模式则只请求一次异步推进（重复请求合并），
  避免脚本 `showModal` 独占 Flutter UI 线程。Host 继续按自己的帧时钟调用，并允许
  `runtime_acquire_frame` 暂时返回 `NO_FRAME`；输入会排队，在包括模态循环在内的 KRKR
  iteration 中消费。不要拿 Artemis 的 `advance_*` delta 语义套用。
- `runtime_stage_width` / `runtime_stage_height` 应以创建后的实际结果为准，
  `Art3m1sKrkrRuntimeConfigV1` 只提供初始窗口尺寸。
- `runtime_acquire_frame` 成功时返回 RGBA8、显式 `stride` 和 `frame_id`。Core facade
  已把原生像素复制到 core 管理的缓冲，但 `pixels` 只在对应
  `runtime_release_frame(frame_id)` 调用前有效；Host 必须先展示或复制，不能跨 tick
  保存指针。`NO_FRAME` 不是退出或错误。
- `runtime_poll_audio_command` 每次取出一个宿主音频命令；`NO_COMMAND` 表示队列已空。
  `payload` 只在下次 poll 前有效，收到后应立即复制或交给音频线程。Apple 嵌入构建
  目前由 native SDL 音频设备直接播放，命令队列通常为空；其余平台仍走 Host 命令路径。
  不能把模拟的 sample 消费时钟当作扬声器播放证明。
- `runtime_submit_audio_consumed` 回传的是自 stream 创建或最近 stop/reset 以来的
  **绝对**已消费 sample frame 数，不是本次增量。每个结构体的 `struct_size` 都必须按
  对应版本填写。
- KRKR 输入是宿主无关事件结构。鼠标、键盘、文本、wheel、focus 和 quit 的 `kind`
  不能与 Artemis 的 VK 事件混用；Host 负责把平台事件转换成 KRKR adapter 定义的
  `code/phase/x/y/value`，并在失焦或窗口取消时补齐 release。
- 当前上游 smoke 的 `runtime_set_external_surface` 返回 unsupported。Host 应优先
  使用 RGBA frame 回读路径，不要假定 KRKR 已支持 Artemis 的 IOSurface、Metal texture
  或 Android `ANativeWindow`。最终显示仍在 Host。
- 当前上游 smoke 对非空的 `save_root_utf8` 返回 unsupported；在 adapter 完成存档
  重定向前，请传 `NULL` 或空字符串并让 KRKR 使用其自身的路径语义。不要静默把它映射到
  Artemis 的 `resources_set_save_dir`。

KRKR 的 `probe_project` 是目录/XP3/TJS 入口探针，不是完整的游戏身份识别，也不替代
Host 的权限和路径校验。`Art3m1sKrkrRuntimeConfigV1.flags` 和预留字段当前没有 Host
语义，必须清零；`Art3m1sKrkrProbeV1` 的其他字段只作为诊断信息，不能据此执行插件。

KRKR 日志通过独立的 `art3m1s_krkr_get_diagnostics_api_v1` 拉取，不改动主运行 ABI。
启动失败也要先拉取队列再报告错误；运行中按 tick 批量拉取，退出前后再清空。
`runtime_set_debug` 需要有效代际句柄，调试开关只能控制日志级别，不应被当作
TJS 断点调试器或 profiler 支持。记录格式和缓冲规则见 [FFI_REFERENCE.md](FFI_REFERENCE.md#krkr-abiv1)。

## 5. 输出路径

### 通用像素回读

`advance_and_render` 写连续 RGBA8，左上原点，stride 为 `width * 4`，完整帧大小为
`width * height * 4`。返回 `0` 表示无新帧，也可能是参数不足或出错，结合日志区分。
内部局部重绘不意味着此接口只返回脏区像素。

Core 不持有 Host 输出指针。异步图像解码/上传若还在读旧缓冲，应双缓冲或等消费完成，
不能立刻覆写/释放。不要每帧无限创建图像对象或累积待显示帧。
输出为最终 FBO 通道值，接口不做额外 alpha 去预乘或色彩管理；普通游戏通常输出不透明
舞台。透明嵌入时需验证 Host 的 alpha/色彩配置，不要盲目再次乘 alpha。

### 平台共享表面

| kind | Host 传入对象 | 注意 |
|---|---|---|
| `1` | Android `ANativeWindow*` | 不是 Java Surface 对象或 Flutter texture ID |
| `2` | Apple `IOSurfaceRef` | 单平面 BGRA8；不是 `CVPixelBufferRef`，需取其 IOSurface |
| `3` | Apple `MTLTexture` 对象指针 | Metal 直接提交；GL/ANGLE 走 EGLImage 导入 |
| `4` | Apple `CAMetalLayer` | MetalBackend 每帧取 drawable |

需要支持对应扩展的 ANGLE；CGL 不支持此路径。设置返回 `1` 才启用，失败后解绑并降级。
替换失败时旧绑定也可能已被清除。建议先用舞台同尺寸表面，再验证缩放、旋转和颜色。

Core 仍在内部持久 FBO 合成，再在 GPU 上提交到外部表面，不是导出内部纹理指针。
Android BufferQueue 目标会复制完整最终图像，内部场景仍可局部重绘；Apple 保留内容的
目标可局部提交。输出朝向已按 kind 处理，不要无条件再翻转。

`advance_and_present == 1` 表示提交新帧，**不等于任意消费者已完成 GPU 访问**。
ABI 不导出跨设备 fence 或释放回调；Host 必须按平台生产/消费同步协议接线，不能无同步
地同时采样和改写同一表面。消费者通知、缓冲池和引用计数归 Host 管。
换表面前先停止提交、解绑，等待消费者结束后释放对象。

共享提交失败时当前 tick 可能已推进脚本，不要拿同一 delta 再推进一次；后续 tick 进入
回退路径。当前也没有独立的“读取已缓存帧”或“强制重绘”C 接口，Host 应保留最后显示帧
直到拿到新的有效像素，不能假定切换后的首次回读一定非零。诊断记录 kind、尺寸、后端
及 EGL 错误。

## 6. 输入和系统事件

鼠标与触摸位置都是舞台坐标，左上为 `(0,0)`。移除 letterbox 偏移，再按显示区域缩放；
窗口逻辑像素、屏幕物理像素和舞台像素不能直接混用。

- 先 `feed_mouse` 更新位置，再发按钮变化。按钮用 Windows VK：左 `1`、右 `2`、中 `4`，
  不是从零起的序号。拖动持续更新位置，抬起/取消/失焦时补齐 release。
- `feed_click` 是旧单次点击入口，不产生完整按下/抬起状态，不能代替拖动。同一个物理
  操作不要既发它又发完整鼠标按钮序列。
- `feed_key` 用 Windows 虚拟键码，不是字符码或本地 scancode，按下/抬起都发；Ctrl 为
  `17`。Host 不要将 Ctrl 硬编码成 skip 开关，也不要猜游戏 Lua 回调名称。
- `feed_touch` 维护触摸点；需要鼠标兼容时由 Host 转换，避免同一次手势重复触发。
  取消按 up 清理该 ID。没有独立滚轮/触摸板手势入口，由 Host 显式选择映射策略。
- 原生对话框、全屏视频及 Host 菜单应隔离输入，防止点击穿透推进游戏。
- 生命周期 `0/1/2` 分别是退出前/后台/前台。退出和后台可能触发自动存档，此时文件
  回调须仍有效；生命周期通知不代替 Host 帧调度和媒体管理。

## 7. 媒体与视频合成

媒体回调字段见参考文档。遵循每通道命令顺序，不把慢初始化、读盘或解码放在回调里。
播放器可后台预热、长期 idle 复用；启动耗时和持续解码开销分别记录。即使画面静止或
视频仅 24 fps，音频与引擎 tick 也要独立运行。

### 完成与循环

- `notify_sound_finished(NULL)` 完成 BGM；非空 ID 完成对应 SE/语音。
- `notify_video_finished(NULL)` 完成全屏视频；非空 ID 完成图层视频并清理其纹理。
- 自然结束、被允许的用户跳过、当前播放真正失败时回报完成。Core 发起的 stop、
  被新播放替换或循环回绕不能重复当作旧播放自然结束通知。
- 命令没有播放 generation，通知只带 ID。Host 要为每次播放生成序号，过滤旧播放器
  迟到事件；ID 相同不代表同一次播放，BGM 空 ID 也需要序号保护。
- `loop_file` 非空时 A 段播一次，随后重复 B 段；A 到 B 不报完成、不重播 A，提前准备
  B 避免空隙。普通 `loop=true` 循环同一文件。淡变由独立媒体时钟驱动。
- ID 是字符串，`"1.80"` 与 `"1.8"` 不等价。`resolved_file` 是解析后的逻辑资源路径，
  不一定是可交给系统播放器的本地文件；Host 仍需提供归档读取或资源流。

### Runtime 视频

启用 `runtime_set_runtime_media_enabled(rt, 1)` 后，`video` 命令由 core 的 FFmpeg
session 在 runtime 侧解码，并作为普通图层纹理参与合成。Host 不再通过 native video
import/surface ABI 交付解码帧；最终显示仍由 Host 的外部表面承担。

### 图层视频：CPU RGBA fallback

`runtime_upload_video_layer_frame` 是只有 CPU RGBA 结果时的同步 fallback：借用连续、
左上原点 RGBA8，至少 `width * height * 4` 字节，无 stride 参数；调用返回后不保留指针。
正常的 runtime FFmpeg 路径不需要 Host 每帧上传。

## 8. 对话框、HTTP 与翻译

异步操作复制 payload 后开始，结果排回 owner 线程。每个任务携带 Host 会话 generation，
销毁/换游戏后丢弃旧结果。

`dialog_show` 需要一次 `submit_dialog`，取消也要回应；关掉 UI 不等于完成等待。
未注册 UI 时 Core 会一直等待，因此无 GUI Host 也应有明确拒绝策略。`textfieldSize`
按 Unicode 字符数而非字节截断；Core 接受空字符串，没有统一非空限制。
回填不带对话框 ID，Host 要拒绝旧窗口响应。

`http_request` 会挂起脚本，成功或失败都要回填；失败/超时/权限拒绝用
`submit_http_result(rt, 0, NULL, 0)`。`http_cancel` 取消并丢弃对应 serial 的结果。
**回填函数没有 serial 参数**，只完成当前挂起请求，Host 必须确认请求仍匹配当前
serial，不能让旧响应完成新请求。

`callnative`/`purchase` 用 `set_string_variable` 回注指定变量或子键；不是带请求 ID
的 RPC，没有通用等待/恢复承诺，不能假定脚本会等异步结果。按权限策略实现或拒绝。
存在 `file_wasm_sync` 命令也不表示当前已有完整 WASM Host。

同步翻译注入仅做内存查询：非负长度表示替换，`-1` 保留原文，`-2` 请求异步翻译。
输出容量目前为 8192 字节，不足时走异步或保留原文，不能截断 UTF-8 或越界。
`-2` 需要 UI 回调，然后收到 `text_translate`，网络不能阻塞引擎。

`text_translate` 带 `serial`、`text`、`ruby`、`blocking:false`。完成调用
`submit_text_translation(serial, text)`，失败传 `NULL`；返回 `1` 只表示接收结果，不
保证立即可见。Core 等逐字动画结束再替换，并丢弃失效页面的视觉回填。Host 可缓存译文，
但不能自行改写 reveal 状态。Ruby 正文进入注入，注音作为上下文，不要重复翻译成两段正文。

游戏脚本指定的字体可能缺少译文字形（缺字渲染为空白）。`set_font_override` 安装一份
覆盖字体（TTF/OTF 字节，Core 内部复制并立即校验，非法数据返回 0 不生效），此后所有
脚本 face 请求都光栅化到该字体；`clear_font_override` 恢复脚本字体。两者是进程级全局
设置，对所有 runtime 生效，可在任意时刻调用，变更在下一帧重解当前字体，不需要重启
游戏。覆盖只作用于之后光栅化的文本（含异步译文热替换），已排版的既有字形不回溯。
字号、描边、行距等排版属性仍由脚本控制，覆盖只替换字形来源。

## 9. 日志、Profiler 与退出

宿主通过当前启用的 `HostEvents` 句柄批量拉取日志。Host 按游戏会话持久化，再给 UI
有界显示缓冲；不要把 overlay 缓冲当完整日志。上传日志前处理路径/脚本文本等隐私，
不记录网络密钥。

Profiler 在线程内异步聚合，约每 500 ms 发布快照，按低频读取 JSON。读取仍遵守 runtime
串行约束；先查询容量，不足时按负返回值扩容重试。字段/计时关系见参考文档。

退出顺序：

1. 停止新输入、计时任务和异步结果，保留回调和资源上下文。
2. owner 线程 `notify_lifecycle(rt, 0)`，让自动保存有机会完成。
3. 停止/取消媒体与网络；在有效 GL lease 内销毁外部视频 renderer，配对 end；
   令会话 generation 失效，丢弃迟到事件。
4. 停止共享提交/消费并解绑，按平台同步等消费者结束后释放表面。
5. owner 线程 `runtime_destroy` 一次，清空指针，不再调用任何 `runtime_*`。
6. 调用 `resources_clear`，然后 `resources_destroy`；runtime 已销毁后不应再使用该句柄。
7. 最后释放像素缓冲和 UI；调用 `host_events_enable_v1(events, 0)` 后停止拉取，
   再调用 `host_events_destroy(events)` 释放句柄。

## 10. 新 Host 验收清单

- 创建/加载失败可清理，重复进出游戏不崩溃，异步任务不访问旧 runtime。
- UTF-8/Shift_JIS、解包/PFS、同名归档存档隔离正确。
- 画面四角标记与输入一致，缩放/旋转无偏移和翻转；hover、拖动 release、失焦正常。
- 静止页面计时器继续；24 fps 视频不把游戏锁到 24 Hz；Host UI 不穿透输入。
- BGM A/B 无缝，SE/语音/视频完成恢复等待，替换播放不触发旧完成。
- 存档即时读取及重启读取、取消/空文本对话框、HTTP 失败/取消均不卡住。
- 翻译迟到/过长/失败不破坏逐字动画或覆盖下一页，Ruby 正常。
- 回读、共享和失败回退分别验证；Profiler 区分 tick 与实际呈现帧率。

## 源码导航

| 主题 | 源码 |
|---|---|
| C ABI/事件/全局配置 | [`src/ffi/mod.rs`](../src/ffi/mod.rs)、[`src/ffi/api.rs`](../src/ffi/api.rs) |
| 创建/帧推进/共享提交 | [`src/runtime/mod.rs`](../src/runtime/mod.rs) |
| 项目/存档 | [`src/runtime/project.rs`](../src/runtime/project.rs)、[`src/runtime/save_io.rs`](../src/runtime/save_io.rs) |
| 媒体协议/完成 | [`src/host/media.rs`](../src/host/media.rs)、[`src/runtime/media.rs`](../src/runtime/media.rs) |
| UI/HTTP/对话框 | [`src/runtime/events.rs`](../src/runtime/events.rs)、[`src/runtime/dialog.rs`](../src/runtime/dialog.rs) |
| 翻译 | [`src/runtime/text.rs`](../src/runtime/text.rs) |
| RFVP ABI | [`src/ffi/rfvp_api.rs`](../src/ffi/rfvp_api.rs) |
| KRKR ABI | [`src/ffi/krkr_api.rs`](../src/ffi/krkr_api.rs)、[`crates/art3m1s-krkr/README.md`](../crates/art3m1s-krkr/README.md) |
| 表面导入 | [`crates/art3m1s-render/src/backend/gl/platform.rs`](../crates/art3m1s-render/src/backend/gl/platform.rs) |
| Profiler | [`src/profiler/mod.rs`](../src/profiler/mod.rs) |
