#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" != Darwin ]]; then
  printf '%s\n' 'This smoke test requires macOS and a Metal device.' >&2
  exit 1
fi
framework_dir="${FLUTTER_MACOS_FRAMEWORK_DIR:?Set this to the directory containing FlutterMacOS.framework}"
framework_dir="$(cd "$framework_dir" && pwd)"
if [[ ! -f "$framework_dir/FlutterMacOS.framework/FlutterMacOS" ]]; then
  printf '%s\n' 'FlutterMacOS.framework must contain a macOS binary for this architecture.' >&2
  exit 1
fi

build_dir="${ART3M1S_V2_TARGET_DIR:-target/host-v2}"
cargo build --locked --lib --manifest-path crates/artemis-core/Cargo.toml --target-dir "$build_dir" "$@"
cargo build --locked --manifest-path crates/artemis-metal-host/Cargo.toml --target-dir target "$@"
lib_dir="$(cd "$build_dir/debug" && pwd)"
scratch="$(mktemp -d)"
trap 'rm -f "$scratch/artemis-flutter" && rmdir "$scratch"' EXIT
MACOSX_DEPLOYMENT_TARGET=12.0 swiftc -swift-version 5 -strict-concurrency=complete -warnings-as-errors \
  -module-cache-path "$build_dir/swift-modules" -Xcc -Idoc/abi \
  -import-objc-header tools/host-bindings/swift/ArtemisHostBridge.h \
  -F "$framework_dir" -Xlinker -rpath -Xlinker "$framework_dir" \
  tools/host-bindings/swift/ArtemisMacSession.swift tools/host-bindings/swift/ArtemisFlutterHost.swift \
  tools/host-bindings/swift/ArtemisLegacyMedia.swift \
  tools/host-smoke/ArtemisFlutterSmoke.swift -o "$scratch/artemis-flutter"
args=("$lib_dir/libart3m1s_core.dylib" "$PWD/target/debug/libart3m1s_artemis_metal_host.dylib")
if [[ -n ${ARTEMIS_GL_SMOKE_ANGLE_DIR:-} ]]; then args+=("$ARTEMIS_GL_SMOKE_ANGLE_DIR"); fi
"$scratch/artemis-flutter" "${args[@]}"
