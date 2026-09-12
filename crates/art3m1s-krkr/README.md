# art3m1s-krkr

`art3m1s-krkr` is the isolated adaptation boundary for running the
Kirikiri/KRKR C++ runtime behind Art3m1s.

The crate currently provides:

- a versioned POD ABI contract for a future native host shim
- opaque runtime/frame/audio handles
- borrowed RGBA and PCM pointer plus length payloads
- deterministic project probing for XP3 and TJS entry files

It does not yet link the C++ runtime. It intentionally has no dependency on
`art3m1s-core`, the Flutter host, or `art3m1s-rfvp`.

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

The native host shim will be added only after the pinned KRKR source/build
revisions and dynamic FFmpeg packaging have been validated. The current source
and build pins are recorded in [`UPSTREAM.md`](UPSTREAM.md).
