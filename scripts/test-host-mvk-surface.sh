#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $(uname -s) != Darwin ]]; then
  printf '%s\n' 'This check requires macOS and a Metal device.' >&2
  exit 2
fi
: "${ARTEMIS_MOLTENVK_DYLIB:?Set ARTEMIS_MOLTENVK_DYLIB to libMoltenVK.dylib}"
: "${ARTEMIS_VULKAN_INCLUDE:?Set ARTEMIS_VULKAN_INCLUDE to the Vulkan header directory}"
library_dir=$(cd "$(dirname "$ARTEMIS_MOLTENVK_DYLIB")" && pwd)
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
xcrun clang -std=c11 -O2 -Wall -Wextra -Werror -I "$ARTEMIS_VULKAN_INCLUDE" \
  tools/host-smoke/artemis_mvk_surface.c -o "$scratch/artemis-mvk-surface" \
  "$ARTEMIS_MOLTENVK_DYLIB" -Xlinker -rpath -Xlinker "$library_dir" \
  -framework CoreVideo -framework CoreFoundation
MVK_CONFIG_LOG_LEVEL=${MVK_CONFIG_LOG_LEVEL:-2} "$scratch/artemis-mvk-surface"
