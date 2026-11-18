# Runtime Shader ABI

Art3m1s runtime effects use a backend-neutral fragment shader ABI. The host
registers an Artemis-style HLSL source with a logical name. In v2 the core
provides source and bindings in the frame packet. The Host renderer normalizes
legacy names, compiles the supported fragment subset to SPIR-V, reflects its
resources, and produces the backend artifact:

```text
Artemis HLSL -> compatibility normalization -> canonical HLSL
             -> shaderc/glslang HLSL frontend -> SPIR-V + reflection
             -> Vulkan SPIR-V
             -> Metal SPIRV-Cross MSL
```

The compatibility frontend still parses the legacy `ps()` shape. It is only a
source compatibility layer; shaderc/glslang, rather than DXC, is the compiler
currently used for native backends.

## Canonical bindings

All resources are descriptor set 0:

| Binding | Resource | Meaning |
| ---: | --- | --- |
| 0 | `Art3m1sDrawParameters` uniform buffer | reflected draw/frame values |
| 1 | `art3m1s_texture_fore` | foreground/source texture |
| 2 | `art3m1s_texture_mask` | mask texture, white when absent |
| 3 | `art3m1s_texture_user` | optional user texture, transparent when absent |
| 4 | `art3m1s_texture_back` | background texture, transparent when absent |
| 5 | `art3m1s_sampler` | linear clamp sampler |

The uniform block contains `float4x4 art3m1s_transform`, then `float4`
`art3m1s_uv`, `art3m1s_clip`, `art3m1s_wipe`, `art3m1s_color`,
`art3m1s_color_flags`, and `art3m1s_frame`, followed by effect-specific
globals declared by the source. Native backends use reflection for offsets,
sizes, resource bindings, and the fragment entry point.

`art3m1s_color` is RGB multiply plus opacity. `art3m1s_color_flags` contains
grayscale and negative flags. `art3m1s_frame` contains resolution, elapsed time,
and an optional frame index.

Legacy aliases remain available: `alpha`, `colorMultiply`, `samplerFore`,
`samplerMask`, `samplerUser`, `samplerBack`, and `tex2D(...)`.

## Registration and reload

The legacy native runtime exposes shader registration through:

- `art3m1s_runtime_register_hlsl_shader`
- `art3m1s_runtime_replace_hlsl_shader` (also exposed as `..._reload_hlsl_shader`)
- `art3m1s_runtime_unregister_hlsl_shader`

Registration returns a stable logical `ShaderId` as a positive `int64`, or
`-1` on invalid arguments or a compile error. Compilation errors are logged
with shader name, stage, compiler message, and source location when supplied by
the compiler. A replacement preserves the logical ID and invalidates every
native pipeline cache entry for that shader. Vulkan module and pipeline objects
are retired until their submission serial completes.

## Supported subset

The runtime currently promises only the Artemis fragment-effect subset used by
tested games: legacy global `float`/`float2`/`float3`/`float4` values, constants,
texture sampling through the compatibility aliases, arithmetic, conditionals,
and the `ps()` return value. It does not promise a complete Direct3D HLSL
runtime. Vertex shaders, techniques, arbitrary sampler states, geometry or
compute stages, UAVs, structured buffers, tessellation, and unsupported HLSL
intrinsics remain outside the ABI.

## Direct3D backend probes

On macOS with CrossOver and MinGW, select a dedicated 64-bit Windows test
bottle and run:

```sh
CX_BOTTLE=DedicatedTestBottle bash scripts/test-host-dxvk.sh wined3d
CX_BOTTLE=DedicatedTestBottle bash scripts/test-host-dxvk.sh dxvk
CX_BOTTLE=DedicatedTestBottle bash scripts/test-host-dxvk.sh wined3d --d3d11
CX_BOTTLE=DedicatedTestBottle bash scripts/test-host-dxvk.sh dxvk --d3d11
```

Set `CROSSOVER_ROOT` for an alternate CrossOver installation and
`ARTEMIS_D3D_CC` for an alternate Windows C compiler. The DXVK run requires
both a DXVK version line and MoltenVK device creation. The default D3D9 probe
checks `ps_2_0` and `ps_3_0`. The D3D11 probe requests feature level `11_0`,
compiles `main` to `vs_5_0` and `ps_5_0`, and checks BGRA readback for two
texture coordinates, a constant buffer, and alpha blending. A pixel mismatch
fails the run even when the draw call itself succeeded. These probes do not
need game files and are outside the default test suite.

To test a candidate instead of CrossOver's bundled DXVK, set both
`ARTEMIS_DXVK_D3D9_DLL` to a native Windows DLL of the same architecture
as the probe and `ARTEMIS_MOLTENVK_DYLIB` to the absolute path to its
MoltenVK library. With `--d3d11`, use `ARTEMIS_DXVK_D3D11_DIR` containing
`d3d11.dll` and `dxgi.dll` instead of the D3D9 DLL. The script reports the
loaded backend and checks native DLL loading for an alternate candidate.

On Apple Silicon the passing candidate used Swingby DXVK (`1c15610`, based
on DXVK 2.4.1) and public MoltenVK 1.4.2. Build the candidate from that
source revision with its initialized submodules, Meson, MinGW and glslang:

```sh
meson setup build --cross-file build-win64.txt --buildtype release \
  -Denable_dxgi=false -Denable_d3d8=false -Denable_d3d10=false -Denable_d3d11=false
ninja -C build src/d3d9/d3d9.dll
```

For the original runtime comparison, record the compiler entry point, profile,
sampler assignments and constants before comparing a game effect. A D3D9
bytecode header identifies shader model and stage; it cannot distinguish
compiler profiles that emit the same version token. Use the same effect,
textures and render state for both captures, and compare the resulting pixels.

The [original-runtime fixture](../tools/host-smoke/original-shader) exercises
`[lyshader]` using a self-contained scene. Use the Windows x64 Artemis executable
from the public release identified in [GOAL.md](../GOAL.md#原版-d3d11-路径与像素对照).
Copy the fixture and the executable into an empty working directory for each
backend, then launch the executable from that directory in a dedicated bottle.
Select `CX_GRAPHICS_BACKEND=wined3d` or `CX_GRAPHICS_BACKEND=dxvk` and use
`wine --no-gui --debugmsg '-all,+d3dcompiler'` to record compiler arguments.
Close the window after `save/baseline.png` and `save/effect.png` have appeared.
The compiler trace for `sample` must show `D3DCompile`, `main`, and `ps_5_0`.
Compare decoded RGBA bytes between the two runs, for example with FFmpeg:

```sh
ffmpeg -v error -i save/baseline.png -f rawvideo -pix_fmt rgba - | shasum -a 256
ffmpeg -v error -i save/effect.png -f rawvideo -pix_fmt rgba - | shasum -a 256
```

The baseline is 320×200 opaque white; the effect is 320×200 opaque `#204080`.
This fixture uses a constant-color effect. The independent D3D11 pixel probe
exercises texture and constant-buffer bindings.

## D3D11 Host Worker

The optional worker in `tools/host-bindings/d3d11` compiles unmodified
`main`/`ps_5_0` HLSL and draws an offscreen quad. It binds up to four 2D BGRA
textures (`t0` through `t3`), one sampler (`s0`), and float scalars or vectors
in `b0`. The compiler's reflection supplies uniform offsets. Other resources
are rejected; the engine's frame packet and the default renderer are unchanged.

Run `CX_BOTTLE=DedicatedTestBottle bash scripts/test-host-dxvk.sh dxvk --d3d11 --worker`
to exercise the Windows worker from a macOS process. The probe also accepts
`wined3d`. The Rust SDK in `crates/artemis-shader-host` uses the worker's
stdout and stdin; the Host owns process launch, timeouts and configuration.
Set `ARTEMIS_D3D11_MEASURE=1` to measure a 1920×1080 draw with two full-size
inputs and one full-size output across the pipe. It excludes the Host's scene
rendering and final presentation.

Every request uses a little-endian `ADX1` header: four bytes of magic, then
`u32 operation`, `u32 request_id`, and `u32 payload_bytes`. IDs are nonzero
after startup. Responses echo the operation and request ID and begin with a
signed 32-bit HRESULT counted in their payload length. The startup response
has zero operation and ID. EOF on stdin closes and releases the worker.

- Operation 1 compiles its raw HLSL payload (up to 1 MiB) and uses the request
  ID as its program ID. A failure can carry compiler diagnostics.
- Operation 2 draws a program. The payload starts with four `u32` values:
  program ID, width, height, and flags (bit 0 alpha blend, bit 1 point sampler).
  Next are four clear-color `f32` values, four vertices of six `f32` values
  (`xy`, `uv0`, `uv1`), and a `u32` uniform count. Each uniform is a `u32`
  name length, its non-NUL name bytes, a `u32` component count and one to four
  `f32` values. Four textures follow in register order: width and height as
  `u32`, then tightly packed BGRA bytes; two zero dimensions leave a slot
  unbound. Success returns tightly packed BGRA pixels at the requested size.
- Operation 3 releases the program ID in its `u32` payload.

One worker keeps up to 64 programs. Requests and output images are capped at
64 MiB. Invalid framing ends the process; a framed request with invalid fields
returns an error and leaves the next frame readable. The transport does not
bundle a Direct3D implementation or choose the Host's Wine/Vulkan configuration.

`scripts/test-host-mvk-surface.sh` checks the native presentation boundary.
Set `ARTEMIS_MOLTENVK_DYLIB` and `ARTEMIS_VULKAN_INCLUDE` from the public
MoltenVK distribution. The probe imports the IOSurface of a BGRA
`CVPixelBuffer`, writes two frames with Vulkan and checks each in the same
buffer after GPU completion. This probe creates a native Vulkan device
with `VK_EXT_metal_objects` enabled. The tested Wine/DXVK device did not
enable that extension.

Run `scripts/test-host-dxvk.sh dxvk --d3d11 --interop` with the same
`ARTEMIS_VULKAN_INCLUDE` to query `IDXGIVkInteropDevice` in Wine. The probe
checks the device and queue, enumerates the visible device extensions and
reports whether Metal object export or Win32 external memory and semaphore
entry points can be resolved. It does not submit Vulkan work to DXVK's queue.

To test transfer from Wine to a Flutter texture, set
`FLUTTER_MACOS_FRAMEWORK_DIR` to the directory containing
`FlutterMacOS.framework`, select the bottle, and run:

```sh
bash scripts/test-host-dxvk.sh dxvk --d3d11 --iosurface
```

The DXVK probe reads back three frames and streams BGRA pixels to a native
Host process. The Host copies rows into its own IOSurfaces and returns the
buffers through `FlutterTexture.copyPixelBuffer()`. It checks retained pixels
across an update, a resize, and a detach. Omit `--d3d11` for the D3D9 variant.
This run uses a CPU copy and calls the texture directly. Flutter engine presentation,
shared GPU surfaces from Wine, and full-size frame throughput need separate
validation.
