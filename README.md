# art3m1s-core

Art3m1s 的多引擎视觉小说运行时。core 通过独立 adapter 接入 Artemis/Art3m1s、
RFVP 和 KRKR/Kirikiri，共享日志、媒体、GPU 渲染和版本化 C ABI；adapter 输出引擎无关
的 `DrawList` 或最终合成帧，render 后端只消费统一渲染契约。生产宿主是 Flutter 项目
[Art3m1s](https://github.com/Alphaly2K/art3m1s)。

Artemis 仍是当前功能最完整、默认启用的引擎路径。RFVP 是独立的可选 adapter。KRKR
adapter 处于早期接入阶段，当前 crate 主要提供 ABI、探针和 macOS smoke，不应把
`native-bootstrap` 当作可运行游戏后端。Artemis 路径不创建窗口；KRKR 当前由上游 C++
runtime 管理其内部 application/window 生命周期，但最终显示和真实音频输出仍固定由
Host 负责。

当前版本为 **0.4.0**，对应生产宿主 Art3m1s **1.3.0**。

## 功能

- 多引擎 runtime 边界：`Art3m1sApiV1`、`Art3m1sRfvpApiV1` 和可选的
  `Art3m1sKrkrApiV1` 各自版本化，不互相复用引擎对象语义
- ASB/AST/IET 脚本解释与 Lua 桥接（桌面/Android 用 Lua 5.1，iOS 用 Luau）
- 图层树、变换、混合、转场、动画与命中测试；Artemis HLSL shader 子集
- 共享 GPU 后端：Apple 默认原生 Metal（可选 MetalFX Spatial），GL/ANGLE 为参考路径，
  Vulkan 为实验路径；RFVP/KRKR adapter 不直接依赖具体图形 API
- E-Mote PSB 立绘（内置后端；`crates/eluna` 为实验后端）
- 场景文本、Ruby、逐字显示、backlog、宿主文本翻译注入与覆盖字体
- PFS 归档（含分卷、pf8 加密）与目录资源、编号/系统存档
- 鼠标、键盘、触摸、拖动的脚本事件派发
- 无 Dart 反向回调的 host-events 队列：日志、媒体和 UI 命令由 Host 批量取出
- 可选 runtime 媒体解码、宿主音频命令和共享纹理/外部表面路径

## 仓库结构

```text
src/
  ffi/                Art3m1s、RFVP、KRKR 版本化 C ABI facade
  runtime/            Artemis 运行时、输入、媒体和帧推进
  compositor/         图层树、转场、动画和 DrawList 构建
  host/               事件、资源、媒体和日志边界
  render_pipeline/    共享渲染契约的 core 侧组合层
  profiler/           性能采样和快照
crates/
  art3m1s-log/        独立日志 crate
  art3m1s-media/      独立媒体/FFmpeg 边界
  art3m1s-render/     共享 DrawList、GPU backend、surface、shader 和 post-process
  art3m1s-rfvp/       RFVP adapter 与 host-runtime
  art3m1s-siglus/     Siglus VM 到 art3m1s-render 的兼容接口（实验）
  art3m1s-krkr/       Kirikiri/KRKR adapter、版本化 ABI 和原生 smoke host
  asb-interpreter/    ASB/AST/IET 解释器与 Lua 桥
  art3m1s-emote/      内置 E-Mote 后端
  eluna/              实验性 E-Mote 后端（基于 xmoezzz/eluna 适配，MPL-2.0）
  pf8/                PFS 归档库（vendored 自 sakarie9/pfs-rs，MIT；
                      本地扩展见 crates/pf8/VENDORED.md）
  pfs-upk-rust/       PFS 的 C ABI 封装（产物即 libpfs_upk）
tools/game-probes/    兼容性探针
tests/                集成与兼容性测试
doc/                  宿主接入指南与 FFI 参考
```

## 构建与测试

```bash
cargo fmt --check
./scripts/test-all.sh
cargo build --release
```

默认 features 包含 GL、原生 Metal、实验 Vulkan、实验 Eluna 和 `rfvp-engine`，
不包含 `ffmpeg` 或 `krkr-engine`。只用无 GPU 的核心模块时
`cargo build --no-default-features`。需要商业游戏资源的兼容性测试默认不执行，见
[tests/README.md](tests/README.md)。

KRKR 的当前构建层次如下：

```bash
# core ABI/bootstrap 检查；native shim 返回 unsupported，不能运行游戏
cargo test --locked --offline \
  --manifest-path crates/art3m1s-krkr/Cargo.toml \
  --features native-bootstrap

# macOS 上游真实 runtime smoke；需要已准备的 KRKRSDL3 source/build checkout
VCPKG_ROOT=/path/to/vcpkg \
KRKRSDL3_SOURCE_DIR=/path/to/krkrsdl3 \
KRKRSDL3_BUILD_DIR=/path/to/krkrsdl3_build \
cargo build --manifest-path crates/art3m1s-krkr/Cargo.toml \
  --features native-upstream-smoke \
  --bin krkr_upstream_smoke
```

上游 revision、CMake 前置条件和 smoke 运行参数见
[crates/art3m1s-krkr/README.md](crates/art3m1s-krkr/README.md) 与
[crates/art3m1s-krkr/UPSTREAM.md](crates/art3m1s-krkr/UPSTREAM.md)。`native-upstream-smoke`
目前只支持 macOS；任意 Windows `.dll`/`.tpm` 插件、真实扬声器输出和 iOS 打包仍是
后续工作。

## 宿主接入

线程、生命周期、帧循环、资源边界和 KRKR 接入约定见
[doc/HOST_INTEGRATION.md](doc/HOST_INTEGRATION.md)；三个入口的完整 C ABI 声明与协议见
[doc/FFI_REFERENCE.md](doc/FFI_REFERENCE.md)。
E-Mote 后端选择：默认内置；宿主在加载项目前调用
`art3m1s_runtime_set_emote_backend(..., 1)` 才切到实验性 Eluna。
运行时 HLSL 的 ABI、资源 binding、编译流程和限制见
[doc/SHADER_ABI.md](doc/SHADER_ABI.md)。
SceneColor、render/output size 与线性 post-process 设计见
[doc/POST_PROCESS.md](doc/POST_PROCESS.md)。

## 状态与限制

- 原生 Vulkan 仍为实验后端；Android 生产路径继续使用 ANGLE / OpenGL ES。
- MetalFX Spatial 需要 macOS 13+ / iOS 16+ 且 GPU 支持，否则回退 native render。
- HLSL 支持面向已测试游戏实际使用的 shader 形态，不是通用 DirectX shader 编译器。
- E-Mote 针对已测试游戏的 PSB 变体；部分私有 easing 与外部纹理格式未覆盖。
- HTTP、native call、浏览器、振动等宿主服务需宿主实现回调后方可用。
- KRKR 当前以 C++ runtime 和 XP3/TJS/KAG 为边界，不把 TJS2/KAG 移植到 Rust；
  save root/真实扬声器/平台 surface 和插件桥仍以 adapter 的实际能力为准。
- `art3m1s_krkr_native_get_api_v1` 只属于 native shim，Host 只能调用 core 导出的
  `art3m1s_krkr_get_api_v1`。

版本详情见 [CHANGELOG.md](CHANGELOG.md)。

## 许可证

[MPL-2.0](LICENSE)：文件级 copyleft——修改本仓库已覆盖的文件需以 MPL-2.0
提供对应源码；由其他许可文件组成的 Larger Work（宿主应用、闭源分发版）
可保持各自许可。例外：`crates/pf8` 保留上游
[MIT 许可](crates/pf8/LICENSE)。

`crates/art3m1s-krkr` 的上游 KRKRSDL3 代码不属于 MPL-2.0；其 pin、许可证和附加条款见
[THIRD_PARTY_NOTICES.md](crates/art3m1s-krkr/THIRD_PARTY_NOTICES.md)。若分发基于
KRKRSDL3 修改版的商业游戏移植二进制，必须按其条款公开相应修改源码并保留上游版权和
许可证文本。
