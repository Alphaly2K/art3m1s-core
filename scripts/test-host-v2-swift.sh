#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" != Darwin ]]; then
  printf '%s\n' 'This smoke test requires macOS.' >&2
  exit 1
fi

compile_only=false
if [[ "${1:-}" == --compile-only ]]; then
  compile_only=true
  shift
fi

build_dir="${ART3M1S_V2_TARGET_DIR:-target/host-v2}"
scratch="$(mktemp -d)"
trap 'rm -f "$scratch/artemis-swift" && rmdir "$scratch"' EXIT
MACOSX_DEPLOYMENT_TARGET=12.0 swiftc -swift-version 6 -module-cache-path "$build_dir/swift-modules" \
  -Xcc -Idoc/abi \
  -import-objc-header tools/host-bindings/swift/ArtemisHostBridge.h \
  tools/host-bindings/swift/ArtemisMacSession.swift tools/host-smoke/ArtemisMacSmoke.swift \
  -o "$scratch/artemis-swift"
if "$compile_only"; then exit 0; fi

cargo build --locked --lib --manifest-path crates/artemis-core/Cargo.toml --target-dir "$build_dir" "$@"
cargo build --locked --manifest-path crates/artemis-metal-host/Cargo.toml --target-dir target "$@"
lib_dir="$(cd "$build_dir/debug" && pwd)"
"$scratch/artemis-swift" "$lib_dir/libart3m1s_core.dylib" "$PWD/target/debug/libart3m1s_artemis_metal_host.dylib"
