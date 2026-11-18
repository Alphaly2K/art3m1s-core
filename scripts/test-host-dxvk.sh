#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $(uname -s) != Darwin ]]; then
  printf '%s\n' 'This check requires macOS and CrossOver.' >&2
  exit 2
fi
backend=${1:-dxvk}
case "$backend" in
  dxvk|wined3d) ;;
  *) printf '%s\n' 'Usage: test-host-dxvk.sh [dxvk|wined3d]' >&2; exit 2 ;;
esac
crossover=${CROSSOVER_ROOT:-/Applications/CrossOver.app/Contents/SharedSupport/CrossOver}
: "${CX_BOTTLE:?Set CX_BOTTLE to a dedicated CrossOver test bottle}"
cc=${ARTEMIS_D3D9_CC:-x86_64-w64-mingw32-gcc}
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
"$cc" -std=c11 -O2 -Wall -Wextra -Werror tools/host-smoke/artemis_d3d9.c \
  -o "$scratch/artemis-d3d9.exe" -ld3d9 -ld3dcompiler_47 -luser32

status=0
(
  cd "$scratch"
  CX_GRAPHICS_BACKEND=$backend DXVK_LOG_LEVEL=info MVK_CONFIG_LOG_LEVEL=3 \
    "$crossover/bin/wine" --no-gui --debugmsg -all artemis-d3d9.exe
) > "$scratch/result.log" 2>&1 || status=$?

awk '/^info: +DXVK: / && !dxvk++ || /MoltenVK version/ && !mvk++ ||
     /Created VkDevice|D3D9 adapter:|^ps_[23]_0|failed:|cannot reserve|^err: +DxvkGraphicsPipeline:/' \
     "$scratch/result.log"
if [[ $backend == dxvk ]]; then
  if ! grep -Eq '^info: +DXVK: ' "$scratch/result.log" ||
     ! grep -Fq '[mvk-info] Created VkDevice' "$scratch/result.log"; then
    printf '%s\n' 'The run did not confirm DXVK and MoltenVK device creation.' >&2
    exit 2
  fi
elif grep -Eq '^info: +DXVK: ' "$scratch/result.log"; then
  printf '%s\n' 'The WineD3D control unexpectedly loaded DXVK.' >&2
  exit 2
fi
exit "$status"
