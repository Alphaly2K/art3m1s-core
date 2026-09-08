# Frame Pipeline

The runtime now exposes two dimensions through `RenderDimensions`:

```text
DrawList (scene + current text commands)
    -> SceneTarget(render_size)
    -> PostProcessPipeline (linear passes)
    -> output target(output_size)
    -> native surface / present
```

`render_size` is the persistent scene color target. `output_size` is the
native surface or drawable size. They are intentionally independent; for
example a 1920x1080 scene can be linearly upscaled into a 2560x1440 output.
Metal's existing cached sampler and fullscreen draw implement the baseline
`Upscale(Linear)` pass. Vulkan exposes the same dimensions and pass validation
while remaining Experimental.

`PostProcessPipeline` is a linear list, not a general render graph. It contains
backend-neutral `PostProcessPass` values and format metadata. Backends cache
their native pipeline, sampler, and intermediate target objects and only
recreate them after resize, output-size change, or format change. Spatial
upscaling and sharpen are represented as future passes but currently return an
unsupported error; temporal upscaling is not implemented.

The current DrawList still carries text/UI commands in the scene pass, which
preserves existing Artemis rendering behavior. The post-process boundary is
separate from those commands so a future UI compositor can submit native-size
UI/text after the scene passes without exposing Metal or Vulkan handles to the
runtime. Until that split is enabled, text follows the existing scene target
semantics.

Scene screenshots remain explicit: `FrameTarget::Main` reads SceneColor. The
native presentation path is the final user-visible output. A future final-frame
screenshot API can read the output target without changing the scene capture
contract used by transitions.
