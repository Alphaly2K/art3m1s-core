# art3m1s-krkr

`art3m1s-krkr` is the isolated adaptation boundary for running the
Kirikiri/KRKR C++ runtime behind Art3m1s.

The crate currently provides:

- a versioned POD ABI contract for a future native host shim
- opaque runtime/frame/audio handles
- borrowed RGBA and PCM pointer plus length payloads
- deterministic project probing for XP3 and TJS entry files
- a macOS smoke host that boots the pinned C++ runtime and captures RGBA frames

The smoke host is not a production backend yet. It intentionally has no
dependency on `art3m1s-core`, the Flutter host, or `art3m1s-rfvp`.

Run the isolated tests with:

```sh
cargo test --manifest-path crates/art3m1s-krkr/Cargo.toml
```

The `native-bootstrap` feature builds a small C++ shared library that exports
the versioned ABI with unsupported stubs. It exists only to verify C/Rust
layout, symbol visibility, and dynamic linking:

```sh
cargo test --manifest-path crates/art3m1s-krkr/Cargo.toml \
  --features native-bootstrap
```

This feature is not a runnable KRKR backend.

## Upstream Smoke Host

The `native-upstream-smoke` feature builds the pinned C++ runtime on macOS,
loads a KRKR project, advances the application loop, and exports the latest
RGBA frame:

```sh
VCPKG_ROOT=/path/to/vcpkg \
KRKRSDL3_SOURCE_DIR=/path/to/krkrsdl3 \
KRKRSDL3_BUILD_DIR=/path/to/krkrsdl3_build \
cargo build --manifest-path crates/art3m1s-krkr/Cargo.toml \
  --features native-upstream-smoke \
  --bin krkr_upstream_smoke

cargo run --manifest-path crates/art3m1s-krkr/Cargo.toml \
  --features native-upstream-smoke \
  --bin krkr_upstream_smoke -- \
  /path/to/game/data.xp3 120 /tmp/krkr.ppm
```

To verify a deterministic pointer path, inject one left-button click on a
specific frame. The smoke host sends a move plus pointer-down on `frame`, then
a pointer-up on the following frame:

```sh
cargo run --manifest-path crates/art3m1s-krkr/Cargo.toml \
  --features native-upstream-smoke \
  --bin krkr_upstream_smoke -- \
  /path/to/game/data.xp3 120 /tmp/krkr-after-click.ppm \
  --click 60:640:480
```

The build copies `Res/` from `KRKRSDL3_BUILD_DIR` next to the native host
library and points the runtime's virtual executable path there. This supplies
the built-in Droid Sans Fallback font when the game does not ship
`default.ttf`.

The smoke host currently covers macOS project loading, TJS/KAG startup,
XP3/root patch mounting, input event translation, frame capture, and lifecycle
shutdown. Host-owned audio output, arbitrary Windows `.dll`/`.tpm` plugins,
iOS packaging, and dynamic FFmpeg packaging remain separate integration work.
The current source and build pins are recorded in [`UPSTREAM.md`](UPSTREAM.md).
