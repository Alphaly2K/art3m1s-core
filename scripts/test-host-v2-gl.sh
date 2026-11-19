#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
case "$(uname -s)" in
  Darwin) gles=${ARTEMIS_GL_SMOKE_ANGLE_DIR:-} ;;
  Linux)
    gles=1
    export EGL_PLATFORM=${EGL_PLATFORM:-surfaceless}
    export LIBGL_ALWAYS_SOFTWARE=${LIBGL_ALWAYS_SOFTWARE:-1}
    ;;
  *) printf '%s\n' 'This pixel check supports native macOS and Linux.' >&2; exit 2 ;;
esac

core_build=${ART3M1S_V2_TARGET_DIR:-target/host-v2}
cargo build --locked --lib --manifest-path crates/artemis-core/Cargo.toml --target-dir "$core_build" "$@"
cargo build --locked --manifest-path tools/host-smoke/Cargo.toml --target-dir target \
  --features gl-smoke --bin artemis-gl-smoke "$@"
core_lib=$(cd "$core_build/debug" && pwd)
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
"${CC:-cc}" -std=c11 -Wall -Wextra -Werror -pedantic -Idoc/abi tools/host-smoke/artemis_v2.c \
  -L"$core_lib" -lart3m1s_core -Wl,-rpath,"$core_lib" -o "$scratch/arfp"
if [[ ${ARTEMIS_GL_SMOKE_TEST_CACHE:-} == 1 ]]; then
  if [[ $(uname -s) != Darwin || -z $gles ]]; then
    printf '%s\n' 'The cache check requires a macOS ANGLE runtime.' >&2
    exit 2
  fi
  "$scratch/arfp" --gles-frame > "$scratch/frame"
  export ARTEMIS_GL_SMOKE_CACHE_DIR="$scratch/cache"
  target/debug/artemis-gl-smoke < "$scratch/frame"
  entries=("$ARTEMIS_GL_SMOKE_CACHE_DIR"/*.glbin)
  [[ ${#entries[@]} == 1 && -s ${entries[0]} ]]
  cache=${entries[0]}
  touch -t 200001010000 "$cache"
  cp -p "$cache" "$scratch/cold.glbin"
  target/debug/artemis-gl-smoke < "$scratch/frame"
  [[ ! "$cache" -nt "$scratch/cold.glbin" ]]
  cmp "$cache" "$scratch/cold.glbin"
  printf 'invalid program cache' > "$cache"
  target/debug/artemis-gl-smoke < "$scratch/frame"
  [[ $(wc -c < "$cache") -gt 21 ]]
  printf '%s\n' 'ANGLE cache: cold creation, warm reuse and corruption recovery passed'
  exit
fi
if [[ -n "$gles" ]]; then
  if [[ -z ${ARTEMIS_GL_SMOKE_SOURCE:-} ]]; then
    "$scratch/arfp" --gles-frame | target/debug/artemis-gl-smoke
  else
    : "${ARTEMIS_GL_SMOKE_EXPECT_RGBA:?Set the expected pixel when testing an external source}"
  fi
  "$scratch/arfp" --gles100-frame | target/debug/artemis-gl-smoke
else
  "$scratch/arfp" --glsl-frame | target/debug/artemis-gl-smoke
fi
