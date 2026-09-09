# 帧渲染管线

运行时通过 `RenderDimensions` 明确区分两种尺寸：

```text
DrawList（场景及当前文本命令）
    -> SceneTarget(render_size)
    -> PostProcessPipeline（线性 pass 列表）
    -> output target(output_size)
    -> 原生 surface / present
```

`render_size` 是持久化 SceneColor 目标的物理尺寸，`output_size` 是原生
surface 或 drawable 的尺寸。两者彼此独立，例如游戏逻辑舞台为 1920x1080、
Host 提供 2560x1440 的 surface 时，场景可以从 1920x1080 放大到 2560x1440。
Metal 复用已有的 sampler 和
全屏绘制实现基线 `Upscale(Linear)` pass。Vulkan 暴露相同的尺寸和 pass
校验语义，但仍属于 Experimental backend。Metal 在支持的系统/GPU 上可执行
Spatial pass，否则由 backend 回退到 native。

`PostProcessPipeline` 只是线性 pass 列表，不是通用 RenderGraph。它包含
backend-neutral 的 `PostProcessPass` 和格式元数据。Metal/Vulkan 的原生
pipeline、sampler 和中间目标均由各自后端持有；仅当 render scale、尺寸或
格式变化时才重建，正常帧不会重复创建这些资源。Sharpen 仍仅预留配置类型，
使用时会明确返回“不支持”；未实现 temporal
upscaling。

当前 `DrawList` 仍将文本/UI 命令和场景一起提交到 SceneColor，以保持现有
Artemis 渲染行为。后处理边界已经独立，后续可以增加 native output 尺寸的
UI compositor，在 SceneColor 后、Present 前提交 UI/text，而不向 runtime
暴露 Metal 或 Vulkan 原生句柄。本阶段不宣称文本已经从 SceneColor 分离。

`FrameTarget::Main` 仍表示供转场使用的原始 SceneColor。面向用户的 CPU 帧输出和
存档截图目前仍返回逻辑舞台尺寸，并使用 CPU 线性缩放；它们不读取 MetalFX 的
native-size 输出纹理，因此不能作为 MetalFX 输出质量的验证。以后可以增加显式截图
目标，分别读取原始 SceneColor 和原生 output surface，同时不改变已有转场捕获语义。

## 资源生命周期

- `resize` 更新逻辑场景尺寸，并按当前 output size 和 render scale 重建 SceneColor。
- 修改 render scale 只在计算出的物理尺寸变化时重建 SceneColor。
- 修改原生 surface 会更新 `output_size` 并重新解析 render size，不改变逻辑场景尺寸。
- Metal 将仍可能被在途 command buffer 引用的旧目标放入延迟回收队列。
- Vulkan 在替换 SceneColor 前等待相关提交完成，并清理依赖旧目标的缓存。
- group/mask 中间目标跟随 SceneColor 的物理尺寸并按需缓存。

当前 SceneColor/output 格式为 SDR 8-bit UNORM。数据结构保留格式字段，便于
以后加入 HDR，但本阶段没有承诺 HDR pipeline。

## 运行时空间超分

Metal production backend 在 macOS 13+/iOS 16+ 且设备支持时使用原生
`MetalFX SpatialScaler`。SceneColor 是不低于游戏逻辑舞台分辨率的 private
`MTLTexture`，MetalFX 直接把它编码到缓存的更高 native-size private texture，随后再
复制到 drawable；整个
路径不经过 ANGLE、OpenGL 或 CPU readback。MetalFX 不可用时自动回退到 native
render + 线性 present，不会导致 runtime 崩溃。

统一质量档位由 `RenderQualityPreset` 定义：Native=1.0（线性）、Quality=2/3、
Balanced=0.58、Performance=0.5（后三者请求 MetalFX Spatial）。比例相对于
`output_size` 解析，并以游戏逻辑舞台尺寸为下限，避免把游戏先降到低于原始分辨率再
放回原始分辨率。Host 必须创建大于逻辑舞台的 output surface；比例只存在于策略层，
Host 通过 `art3m1s_runtime_set_render_quality_preset` 选择档位。

`BackendCapabilities.spatial_upscaling` 只在实际支持时为 true。Vulkan 当前仍为
Experimental，FSR1 EASU/RCAS 尚未实现，capability 保持 false；这不影响 Metal。
本阶段不实现 MetalFX Temporal、FSR temporal、neural SR 或 frame generation。
