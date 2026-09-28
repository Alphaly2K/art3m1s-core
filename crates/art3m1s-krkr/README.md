# art3m1s-krkr

`art3m1s-krkr` is the isolated adaptation boundary for running the
Kirikiri/KRKR C++ runtime behind Art3m1s.

The crate currently provides:

- a versioned POD ABI contract for a future native host shim
- opaque runtime/frame/audio handles
- a same-thread native render-host vtable used by `art3m1s-render`
- borrowed RGBA fallback and PCM pointer plus length payloads
- deterministic project probing for XP3 and TJS entry files
- a headless upstream host for macOS, iOS, Android, Windows and Linux

The isolated smoke host intentionally has no dependency on `art3m1s-core`, the
Flutter host, or `art3m1s-rfvp`. The production core facade installs the private
render-host vtable and owns the `art3m1s-render` backend and native surface.

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

## Upstream Host

The `native-upstream-smoke` feature exercises the pinned C++ runtime on macOS,
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

The build copies `Res/` from `KRKRSDL3_BUILD_DIR` next to the native host.
Desktop packages place it beside the executable/library; the iOS app bundle
places it under `Res/`; Android packages the font as an APK asset and supplies
its asset manager from the Flutter activity. This supplies the built-in Droid
Sans Fallback font when the game does not ship `default.ttf`.

The smoke host currently covers headless macOS project loading, TJS/KAG startup,
XP3/root patch mounting, input event translation, frame capture, host-owned
audio command extraction, and lifecycle shutdown. The smoke host advances each
playing stream from the wall clock and submits absolute consumed sample counts
back to the runtime; this lets the engine fill audio buffers without making the
smoke host a real speaker backend.

The production host's `--krkr` build option requires `VCPKG_ROOT`,
`KRKRSDL3_SOURCE_DIR`, and `KRKRSDL3_BUILD_DIR` and sets
`ART3M1S_KRKR_REQUIRE_UPSTREAM=1` so no release silently packages bootstrap.
Supported build targets are macOS, native SwiftUI iOS, Android arm64, Windows
x64, and Linux x64. Building each target requires that platform's SDK/NDK and
vcpkg dependencies; source-list validation is not a substitute for a device
test. Arbitrary Windows `.dll`/`.tpm` plugins and real speaker playback remain
separate integration work. Current source and build pins are in
[`UPSTREAM.md`](UPSTREAM.md).
