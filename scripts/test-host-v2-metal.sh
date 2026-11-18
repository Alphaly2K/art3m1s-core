#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" != Darwin ]]; then
  printf '%s\n' 'This smoke test requires macOS and a Metal device.' >&2
  exit 1
fi

bash scripts/test-host-v2.sh "$@"
build_dir="${ART3M1S_V2_TARGET_DIR:-target/host-v2}"
lib_dir="$(cd "$build_dir/debug" && pwd)"
cargo run --locked --manifest-path tools/host-smoke/Cargo.toml --target-dir target "$@" \
  -- "$lib_dir/libart3m1s_core.dylib"
