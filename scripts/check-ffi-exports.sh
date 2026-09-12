#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

case "$(uname -s)" in
  Darwin)
    default_lib="target/release/libart3m1s_core.dylib"
    symbols() { nm -gU "$1"; }
    ;;
  Linux)
    default_lib="target/release/libart3m1s_core.so"
    symbols() { nm -D --defined-only "$1"; }
    ;;
  *)
    echo "Unsupported platform for FFI export check: $(uname -s)" >&2
    exit 1
    ;;
esac

library="${1:-$default_lib}"
if [[ ! -f "$library" ]]; then
  echo "Missing $library; run cargo build --release --lib first." >&2
  exit 1
fi

actual="$(
  symbols "$library" |
    awk '{print $NF}' |
    sed 's/^_//' |
    grep '^art3m1s_' |
    sort -u || true
)"

expected="art3m1s_get_api_v1"
if [[ "$actual" != "$expected" ]]; then
  printf 'Unexpected art3m1s FFI exports in %s.\n' "$library" >&2
  printf 'Expected:\n%s\nActual:\n%s\n' "$expected" "$actual" >&2
  exit 1
fi

printf 'FFI export allowlist OK: %s\n' "$library"
