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

## D3D9 backend probe

On macOS with CrossOver and MinGW, select a dedicated 64-bit Windows test
bottle and run:

```sh
CX_BOTTLE=<test-bottle> bash scripts/test-host-dxvk.sh wined3d
CX_BOTTLE=<test-bottle> bash scripts/test-host-dxvk.sh dxvk
```

Set `CROSSOVER_ROOT` for an alternate CrossOver installation and
`ARTEMIS_D3D9_CC` for an alternate Windows C compiler. The DXVK run requires
both a DXVK version line and MoltenVK device creation. Both modes compile
synthetic HLSL and check a 4×4 GPU readback for each shader profile. A pixel
mismatch fails the run even when the draw call itself succeeded. The probe
does not need game files and is outside the default test suite.
