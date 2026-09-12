# art3m1s-rfvp

RFVP integration boundary and backend-neutral frame adapter for Art3m1s.

The base crate has no dependency on `rfvp`, `art3m1s-core`, wgpu, FFI, or a
concrete GPU backend. Its protocol and `DrawList` conversion types can be
tested independently. The optional `rfvp-fork` feature adds direct access to
the maintained RFVP fork, and `host-runtime` adds the host-facing runtime used
by `art3m1s-core` for mounting resources, driving frames, forwarding input and
audio commands, and presenting through `art3m1s-render`.

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
- nearest filtering remains outside the conversion layer.

The `host-runtime` path consumes ordered texture create/update/destroy records,
validates their bounds and formats, and uploads them through `GpuBackend`.

Run validation with:

```sh
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
```
