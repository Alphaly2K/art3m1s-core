# art3m1s-render

Backend-neutral rendering contracts shared by Art3m1s core and engine
adapters such as RFVP.

This crate owns the stable render API:

- `DrawList` and `DrawCommand`;
- the `GpuBackend` ownership and submission boundary;
- texture, render-target and native-surface descriptions;
- external image/video import and GPU synchronization tokens;
- post-process configuration;
- the Artemis runtime-shader ABI, with optional HLSL-to-SPIR-V/MSL compilation.

The GL reference, Apple Metal and native Vulkan backends are included behind
the `gl`, `metal` and `vulkan` features. Runtime adapters emit `DrawCommand`
values without depending on a concrete graphics API.

## Features

- `runtime-shader`: compiles normalized Artemis HLSL to SPIR-V, reflects the
  runtime ABI, and cross-compiles the same module to Metal Shading Language.
- `gl`: builds the OpenGL/ANGLE reference backend.
- `metal`: builds the native Metal backend on macOS and iOS.
- `vulkan`: builds the native Vulkan backend on Android, Windows and Linux.

## Boundary

This crate intentionally has no `art3m1s-core`, interpreter, FFI or host
filesystem dependency. Asset access is injected through `AssetSource`, and the
ANGLE library prefix is configured through
`backend::gl::platform::set_angle_path_prefix`.
