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
surface 或 drawable 的尺寸。两者彼此独立，例如场景可以先渲染为
1920x1080，再线性放大到 2560x1440 输出。Metal 复用已有的 sampler 和
全屏绘制实现基线 `Upscale(Linear)` pass。Vulkan 暴露相同的尺寸和 pass
校验语义，但仍属于 Experimental backend。

`PostProcessPipeline` 只是线性 pass 列表，不是通用 RenderGraph。它包含
backend-neutral 的 `PostProcessPass` 和格式元数据。Metal/Vulkan 的原生
pipeline、sampler 和中间目标均由各自后端持有；仅当 render scale、尺寸或
格式变化时才重建，正常帧不会重复创建这些资源。当前 Spatial upscale 和
Sharpen 仅预留配置类型，使用时会明确返回“不支持”；未实现 temporal
upscaling。

当前 `DrawList` 仍将文本/UI 命令和场景一起提交到 SceneColor，以保持现有
Artemis 渲染行为。后处理边界已经独立，后续可以增加 native output 尺寸的
UI compositor，在 SceneColor 后、Present 前提交 UI/text，而不向 runtime
暴露 Metal 或 Vulkan 原生句柄。本阶段不宣称文本已经从 SceneColor 分离。

`FrameTarget::Main` 仍表示供转场使用的原始 SceneColor。面向用户的 CPU 帧
输出和存档截图会执行与当前线性 pass 对应的尺寸转换，默认结果保持为用户
可见的最终画面内容（逻辑舞台尺寸）。以后可以增加显式截图目标，分别读取原始 SceneColor
和原生 output surface，同时不改变已有转场捕获语义。

## 资源生命周期

- `resize` 更新逻辑场景尺寸，并按当前 render scale 重建 SceneColor。
- 修改 render scale 只在计算出的物理尺寸变化时重建 SceneColor。
- 修改原生 surface 只更新 `output_size`，不改变逻辑场景尺寸。
- Metal 将仍可能被在途 command buffer 引用的旧目标放入延迟回收队列。
- Vulkan 在替换 SceneColor 前等待相关提交完成，并清理依赖旧目标的缓存。
- group/mask 中间目标跟随 SceneColor 的物理尺寸并按需缓存。

当前 SceneColor/output 格式为 SDR 8-bit UNORM。数据结构保留格式字段，便于
以后加入 HDR，但本阶段没有承诺 HDR pipeline。
