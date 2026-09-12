# art3m1s-rfvp

Backend-neutral RFVP frame to `art3m1s-render::DrawList` adapter.

The crate intentionally has no dependency on `rfvp`, `art3m1s-core`, wgpu,
FFI, or a concrete GPU backend. It is the conversion boundary that can be
tested while RFVP's production renderer is being split from its wgpu surface
path.

Current mapping:

- `DrawImage` uses `vertices` as authoritative geometry.
- Axis-aligned images become a regular clipped quad.
- Rotated or warped images become a triangle-list `DrawMesh`.
- `SetClip` / `ClearClip` become `DrawCommand::clip_bounds`; texture UV
  cropping remains `ClipRect`.
- `Normal`, `Add`, `Sub`, and `Mul` map to `Alpha`, `Add`,
  `NativeReverseSubtract`, and `Multiply`.
- `DrawGlyph` converts its destination rectangle and optional source rectangle.
- `DrawSolid` uses a pre-bound 1x1 white texture.
- `HitProxyTable` is returned unchanged to the host.

Explicit first-version limits:

- per-vertex colors must be uniform;
- `effect_id != 0` is rejected;
- negative clip or draw extents are rejected;
- texture upload/update/destroy and nearest filtering stay outside this pure
  conversion layer until the backend adapter owns `GpuBackend`.

Run validation with:

```sh
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
```
