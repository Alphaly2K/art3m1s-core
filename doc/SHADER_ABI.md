# Runtime Shader ABI

Art3m1s runtime effects use a backend-neutral fragment shader ABI. In v2 the
Core provides the source, language and bindings in the frame packet. Hosts
negotiate HLSL and GLSL independently. The compatibility HLSL renderer for
Vulkan and Metal compiles the supported fragment subset as follows:

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

## GLSL fragment effects

The GL backend compiles a fragment shader with entry point `main` and links
it against the Host's sprite vertex stage. Use UTF-8 for new assets. The Host
accepts a UTF-8 BOM and LF, CRLF or CR line endings. Legacy non-UTF-8 comment
bytes are decoded with replacement. OpenGL Core accepts GLSL
1.50 and 3.30. OpenGL ES accepts GLSL ES 1.00 and 3.00. Without `#version`,
the desktop Host uses GLSL 3.30 Core and the GLES Host uses GLSL ES 1.00.
For an implicit ES 1.00 source without leading directives, the Host provides
a default float precision of `mediump`. An effect can declare its own precision.
An explicit version must match the backend family. The Host reports a
compiler or linker diagnostic and falls back to an unshaded draw on failure.

The vertex stage exposes `vec2 v_uv`, `vec2 resultCoord`, and
`vec2 resultCoord1` for the foreground texture. `vec2 resultCoord0` is the
normalized stage position with a top-left origin; `vec2 v_model_position`
is the layer's unit quad. On GLES, the `resultCoord` names use `mediump`;
`v_uv` and `v_model_position` use `highp`.
GLSL ES 1.00 declares these as `varying`; newer shaders use `in`. The fragment
shader writes one RGBA color to output location zero (`gl_FragColor` in ES 1.00).

| `sampler2D` uniform | Binding |
| --- | --- |
| `u_texture_fore`, `textureFore`, `samplerFore` | Foreground/source |
| `u_texture_mask`, `textureMask`, `samplerMask` | Effect mask; white when absent |
| `u_texture_user`, `textureUser`, `samplerUser` | User texture; transparent when absent |
| `u_texture_back`, `textureBack`, `samplerBack` | Color already drawn into the current target, sampled with `resultCoord0` |

The background includes earlier draws in the same target, before the current
effect. Inside an offscreen group it contains the group's earlier draws; the
effect that composites the group samples the parent target. Its origin is the
top left, including when the foreground is clipped. The GL Host takes a copy
before drawing and redraws the full frame when an active GLSL effect samples
the background.

`alpha` and `colorMultiply` are supplied as `float` and `vec3`. Named
script values bind to active `float`, `vec2`, `vec3` or `vec4` uniforms or
arrays of those types. Each value list must contain whole elements and fit
the active uniform. An effect supplies its fragment color, including any
opacity or multiply it wants to apply before blending.

The macOS ANGLE Host can prepare project GLSL before its first frame. The
Flutter Runner prewarms indexed `.glsl`, `.frag`, `.fsh` and `.fs` files. It
stores ANGLE program binaries in the application's cache directory. Programs
match the effective vertex and fragment sources and the GL driver identity.
If the cache is unavailable or a binary is rejected, the renderer compiles
from source. Effects discovered during play use the same loader and can reuse
the prepared program when the same source is bound again in that context.

Run `bash scripts/test-host-v2-gl.sh --offline` on macOS with a native
OpenGL context to check a packet emitted by the Core against expected
pixels. The probe checks clipped effects on the stage, inside an offscreen
group, and when compositing a group into its parent. Set `ARTEMIS_GL_SMOKE_ANGLE_DIR` to the
directory containing ANGLE's EGL and GLES libraries to run the GLSL ES 3.00
and 1.00 checks through ANGLE/Metal, including presentation into a Host-owned
IOSurface and sampling a Metal texture into a separate Metal target. On Linux,
the same script uses `GlBackend::new_host_egl` to test
both ES profiles against system EGL, with Mesa software rendering and the
surfaceless platform as the script defaults. The EGL checks also verify that
closing one renderer restores the other renderer's active context.
These graphics checks run separately from the default Rust test suite.
With a macOS ANGLE runtime, set `ARTEMIS_GL_SMOKE_TEST_CACHE=1` to verify
cache creation, reuse across processes, recovery from corruption, and rebinding
an effect first prepared during play after its disk entry is removed.
The probe binary `target/debug/artemis-gl-smoke` also accepts a list of GLSL
files to compile and link in one context. To draw a single external ES 1.00
effect through ARFP, run the script with `ARTEMIS_GL_SMOKE_SOURCE` set to
the source path and `ARTEMIS_GL_SMOKE_EXPECT_RGBA` set to a four-element JSON
array. The 4x4 fixture uses foreground `#112233`, user texture `#05070b`, and
script constants `param=1` and `tint=(0,1,0.5,1)`.

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

For D3D11 on Apple Silicon, the `metalsharp/DXVK-MacOS` release
`v3.1.1-macos.4` passed the ARFP worker and Flutter texture pixel check with
its x86-64 DLLs and companion universal MoltenVK 1.4.3. Verify both release
archives against their SHA-256 digests before extracting them. The probe
still requires a compatible Wine runner and a D3D compiler.

For D3D9, Swingby DXVK (`1c15610`, based on DXVK 2.4.1) passed with public
MoltenVK 1.4.2. Build the candidate from that
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

`Worker::compile_frame_effect` compiles an ARFP HLSL effect and caches its
texture slots. `draw_frame_effect` reuses that program with the constants and
texture indices from the current packet. The shader must match the one used
at compile time. The Host supplies the target, quad, foreground pixels, and
the current background if the shader reads it. The SDK resolves reflected
`textureFore`, `textureMask`, `textureUser`, and `textureBack` (or their
`samplerFore`, `samplerMask`, `samplerUser`, and `samplerBack` texture resource
aliases). Other texture names are left unbound. An absent mask defaults to
opaque white and an absent user texture defaults to transparent black. A
referenced texture index must resolve to BGRA pixels. Draw and release the
program on the worker that created it; call `release_frame_effect` when it
is no longer needed. Closing the worker releases all its programs.

Set `ARTEMIS_D3D11_ARFP=1` to also draw a Core frame packet's HLSL effect
with its foreground, mask, user texture and named constants. The probe
verifies readback pixels before and after changing a constant on the same
compiled program.
Use the `dxvk` mode for this check. The tested WineD3D configuration rejects
the fixture's pixel shader input when linking the graphics pipeline.
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
- Operation 4 looks up a texture resource by name. Its payload is a `u32`
  program ID and up to 256 non-NUL name bytes. Success returns `u32 slot`
  and `u32 count`; an absent resource has slot `UINT32_MAX` and count zero.

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
entry points can be resolved. If the Win32 external memory extension and
device entry point are available, it also attempts to create a shared NT
handle for a BGRA render texture. It does not submit Vulkan work to DXVK's
queue. The tested DXVK configurations running under CrossOver did not
expose either Win32 external memory or Metal object export. The companion
MoltenVK from `v3.1.1-macos.4` passed the native IOSurface probe.

`scripts/test-host-dxvk-native.sh` tests D3D11 rendering into a Host-owned
BGRA `CVPixelBuffer` through an imported IOSurface. It uses native DXVK and
MoltenVK in one process without Wine. The probe enables `VK_EXT_metal_objects`
before creating the D3D11 device, wraps a Vulkan image backed by the IOSurface
and compares two GPU clears directly in the original pixel buffer. The layout
handoff materializes deferred clears before the D3D11 completion query.

For a clean Apple Silicon runtime and compiler build, install Xcode, Meson,
Ninja, glslang, SDL3, pkg-config, GNU Make 4.3 or newer, Bison 3, Flex and
Perl with JSON. Run `bash scripts/build-host-shaders-macos.sh OUTPUT_DIRECTORY`
from this repository. This creates `dxvk/` and `hlsl/` under the new directory,
ready to bundle together. The pinned MoltenVK requires macOS 15 or newer.
Use an SDL3 build for the oldest macOS version you intend to support.

To compile DXVK separately, use the source and patches pinned in the script.
Initialize the submodules, apply the two upstream changes and the repository's
runtime and NGX patches. Build with the following settings:

```sh
meson setup build-native --buildtype release \
  -Denable_d3d8=false -Denable_d3d9=false -Denable_d3d10=false \
  -Dnative_sdl3=enabled -Dnative_sdl2=disabled -Dnative_glfw=disabled
ninja -C build-native src/d3d11/libdxvk_d3d11.0.dylib src/dxgi/libdxvk_dxgi.0.dylib
```

From this repository, run the probe with `ARTEMIS_DXVK_NATIVE_SOURCE` and
`ARTEMIS_DXVK_NATIVE_BUILD` pointing to that source and build, and
`ARTEMIS_MOLTENVK_DYLIB` set to the absolute path to the companion MoltenVK
1.4.3 library. The probe uses a private DXVK adapter interface to request
the device extension, so the source headers and libraries must match.

Set `CX_BOTTLE` and add `--hlsl` to the native probe command to exercise
shader execution. The Windows compiler helper reads raw HLSL from stdin,
compiles `main` to `vs_5_0` or `ps_5_0`, and writes DXBC to stdout with
diagnostics on stderr. The `effect` mode compiles `ps_5_0` and includes the
compiler's reflection in an `ADE1` artifact. The native probe uses
`effect-pair`, which also generates a matching `vs_5_0` shader and packs
both stages into `ADE2`. It supports optional `SV_Position` and `float2`
`TEXCOORD0` and `TEXCOORD1` inputs. Both texture coordinates use the quad's
normalized coordinates. The compiler checks the output registers and
masks against the pixel inputs and rejects unsupported signatures. Both
modes accept a sampler at `s0`, 2D textures at `t0` through `t3`, and
float scalars and vectors in `b0`. `effect-pair` also accepts the legacy
`ps` entry point with up to four ordinary sampler slots and float arrays
in `b0`. These compile modes create no graphics device. The
native [DXVK pass interface](../tools/host-bindings/d3d11/artemis_dxvk_pass.h)
prepares an ADE2 program from Host-owned bytes, independently of any
texture. A single context may reuse multiple programs and replace the
foreground/output IOSurface pair without preparing the programs again.
Use all three objects on the owner thread and destroy the programs and
targets before destroying the context. The pass binds foreground, mask,
user textures, and parameters by reflected name. Combined legacy samplers
use linear filtering with clamped coordinates; separate samplers use point
filtering. Combined legacy effects use straight alpha. The Metal Host
converts offscreen groups before sampling and applies the output alpha
when compositing into the parent. The Host restores reflected initial
values before applying the current frame's overrides. Legacy `alpha`
and `colorMultiply` default to
the multiplicative identity when the source gives no initial value.
Metal writes the foreground into a separate IOSurface on the same GPU.
The probe transitions the imported foreground image before the first Metal
write, waits for Metal completion before sampling, and waits for D3D11
completion before reusing either surface. Three draws verify the initial
result, an update to the same foreground surface, and a constant change
with that foreground held steady. Metal copies each DXVK result to a
separate presentation IOSurface on the GPU. Only the final pixel
comparison reads the presentation buffer on the CPU. The `--hlsl` probe
switches between modern and legacy programs, replaces the DXVK targets,
and checks reflected defaults, frame overrides, and retained output.
The probe requires neither game assets nor Flutter.

Use `--arfp` in place of `--hlsl` to run Core's three-frame scene fixture.
The probe compiles the first packet's HLSL once. Metal renders the group's
layers into a shared foreground, and DXVK binds the packet's mask, user
texture, and named constants. The final Metal texture is checked pixel by
pixel after both a uniform change and a layer move. The fixture's pixel
shader declares `TEXCOORD0` without `SV_Position`, so a vertex shader with
position first would put the texture coordinates in the wrong register.
The packaged runtime check also compares translucent sprite and group pixels
over an opaque background. To use an original combined-sampler inversion
effect in that fixture, set `ARTEMIS_DXVK_SMOKE_LEGACY_ALPHA_SOURCE` to its
absolute path. The effect must invert foreground RGB and preserve the
foreground alpha with a neutral mask and parameters. The default check
uses only its bundled synthetic source.

To assemble the macOS effect runtime, apply
[the adjacent library patch](../tools/host-bindings/d3d11/dxvk-macos-runtime.patch)
to the DXVK source and rebuild both native libraries. Run
`bash scripts/build-host-dxvk-native.sh OUTPUT_DIRECTORY` with the same
source, build, and MoltenVK variables as the probe. The SDK and native
libraries must match the architecture of their Host process. The script
expects `LICENSE.MoltenVK` alongside MoltenVK. It also includes both statically linked DXVK subproject notices and
SDL3's installed license. Set `ARTEMIS_SDL3_LICENSE` to use a different path
for the latter.

Keep the five dylibs together when moving the runtime and include its
`licenses` directory when distributing it. Load
`libart3m1s_artemis_dxvk_host.dylib` from the application's runtime directory
and sign the dylibs with the app's signing identity when packaging them.
The runtime consumes ADE2 bytes and needs no Wine or compiler to draw.
Package the compiler and bridge from the same revision when distributing
effects with reflected initial values or array constants. The updated
bridge still accepts existing ADE2 artifacts. Use the matching Metal Host
and DXVK bridge for straight-alpha group composition.
Set `ARTEMIS_DXVK_RUNTIME_DIR` to the assembled directory and run
`bash scripts/test-host-dxvk-native.sh --arfp` to check the packaged libraries.
To generate the fixture's ADE2 without CrossOver, set
`ARTEMIS_NATIVE_HLSL_COMPILER` to the absolute path of the native compiler.
Run `bash scripts/test-host-dxvk-native.sh --bundle APP_BUNDLE` to verify a
packaged compiler and mixed renderer together. The probe compiles a synthetic
effect and checks the shared surface and capture using the bundle's Core,
Metal Host, DXVK and ANGLE libraries. Use `--bundle-compile APP_BUNDLE` to
exercise the packaged compiler on a runner without a graphics device.

To build the compiler separately, build and install vkd3d 2.1 with GNU Make 4.3 or newer, Bison 3,
Flex, Perl with JSON, and Vulkan and SPIR-V headers. Use
`--disable-maintainer-mode --disable-tests --without-xcb --without-opengl
--without-ncurses` for the compiler build. Set `SONAME_LIBVULKAN` if
configure cannot resolve the Vulkan library; the compile and reflect operations
do not create a graphics device. The release archive includes the generated IDL
headers. If sandboxed configure cannot read the maximum command length, set
`lt_cv_sys_max_cmd_len=262144`.

Run `bash scripts/build-host-hlsl-native.sh VKD3D_PREFIX VKD3D_SOURCE_TAR_XZ
OUTPUT_DIRECTORY` with the installation and its corresponding source archive.
The output contains `artemis-hlsl-compile`, the dynamically linked vkd3d
libraries, license notices and source. Keep the tool and libraries together and
retain `licenses/` and `source/` when distributing the package. The same
`effect-pair` command reads HLSL from stdin and returns ADE2 on stdout on
both macOS and Windows.

The [macOS frame Host](abi/art3m1s_metal_host_v1.h) can open that runtime
without initializing a DXVK device. Before drawing an effect, pass it and
its exact HLSL source to `art3m1s_metal_host_v1_prepare_ade2`. The first
preparation initializes the device and reports any initialization failure.
The caller can release both byte buffers after the call. A session can reuse
the program for multiple layers and frames. An effect with no prepared
program draws without the effect. Present and capture use the same programs
and frame composition.

For the Flutter macOS v2 preview, set `ARTEMIS_V2_DXVK_RUNTIME_DIR` to the
assembled runtime when building the Runner. Set `ARTEMIS_V2_HLSL_COMPILER_DIR`
to the native compiler package to bundle automatic compilation. The bundled
tool prepares indexed `.hlsl` and `.fx` assets before the first frame, subject
to the startup budget. When the Core marks a runtime read as HLSL, the Host
can prepare it before replying even if its suffix was outside the startup
scan. The application cache reuses compiled effects across launches and
keys entries by the compiler binaries and exact source bytes.

To supply precompiled effects, store them at
`artemis-shaders/ade2/<source-path>.<sha256>.ade2`, where
`<source-path>` is the project-relative path and `<sha256>` is the lowercase
digest of the exact HLSL bytes read by the Host. The Host looks up a matching
artifact at startup or when the script first loads the shader. A matching
project artifact takes precedence over the application cache and compilation.
The bundled compiler is optional when the project supplies the needed effects.
Explicit ANGLE sessions use GLSL. For a session that needs both languages,
negotiate `ART_MIXED_HOST_V1_RENDERER_CAPS` and call
`art3m1s_metal_host_v1_create_mixed` with both runtime directories. Use the
same ADE2 and GLSL preparation calls. Effects run in frame order, so GLSL can
sample the scene after earlier DXVK effects. The Flutter Runner selects this
mode for projects with both languages when both runtimes are available.

The Flutter project's `tool/integration_tests/artemis_v2_dxvk_smoke.dart`
checks presentation, screenshots and source changes. Build it as the Runner's
target with `ARTEMIS_V2=true` and the v2 and DXVK runtimes bundled. Set
`ARTEMIS_DXVK_TEST_ADE2` at launch to the `effect-pair` output for the exact
shader source in that fixture to check project-supplied artifacts. With the
compiler bundled, set `ARTEMIS_DXVK_TEST_CACHE_TOKEN` to a new alphanumeric
token for a cold launch. Launch again with the same token and
`ARTEMIS_DXVK_TEST_WARM=1` to verify reuse across processes.
With ANGLE also bundled, set `ARTEMIS_DXVK_TEST_MIXED=1` to verify that GLSL
samples earlier HLSL pixels in both the presented texture and a screenshot.
Set `ARTEMIS_DXVK_TEST_LEGACY=1` on both launches to exercise combined
samplers and reflected defaults. Set `ARTEMIS_DXVK_TEST_1080P=1` to use a
1920×1080 stage and print project-open and steady presentation timings with
and without the effect. Frame timings include Core stepping, scene rendering,
and submission to Flutter's texture registry, but exclude display vsync.
With both options set, the probe measures one, two, and four full-screen
draws of the same legacy program. The multi-effect checks use translucent
layers and verify the combined color before measuring.

`ADE1` begins with five little-endian `u32` values: magic (`ADE1`), DXBC
length, `b0` byte size, texture count, and uniform count. Each texture has
a `u32` slot and a 256-byte NUL-terminated name. Each uniform has a `u32`
byte offset, a `u32` float component count, and the same name format. The
pixel DXBC bytes follow the records. `ADE2` uses the same layout with
magic `ADE2`, then appends a little-endian `u32` vertex bytecode length and
the vertex DXBC. For array constants, the low byte of the component count
gives the element's float width, and the upper bytes give the array length;
elements occupy consecutive 16-byte registers. An optional tail of exactly
`b0` byte size holds the initial constant block. The Host restores this block
on every draw, or starts with zeroes if it is absent, before applying frame
parameters. The compiler and decoder share the limits defined in
`artemis_dxbc_effect.h`.

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

Add `--worker` to the D3D11 command to generate the frames from an ARFP
fixture instead. The SDK reuses one HLSL program for three draws, checks
each readback against expected pixels, then passes the frames to the same
Flutter texture probe. This catches pixel changes from a uniform update,
resizing, and the lifetime of buffers retained across later draws.
