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
trap 'rm -f "$scratch/arfp" && rmdir "$scratch"' EXIT
"${CC:-cc}" -std=c11 -Wall -Wextra -Werror -pedantic -Idoc/abi tools/host-smoke/artemis_v2.c \
  -L"$core_lib" -lart3m1s_core -Wl,-rpath,"$core_lib" -o "$scratch/arfp"
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
