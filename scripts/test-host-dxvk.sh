#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $(uname -s) != Darwin ]]; then
  printf '%s\n' 'This check requires macOS and CrossOver.' >&2
  exit 2
fi
backend=${1:-dxvk}
presentation=${2:-}
if [[ $# -gt 2 || ( $backend != dxvk && $backend != wined3d ) ||
      ( -n $presentation && $presentation != --iosurface && $presentation != --d3d11 ) ]]; then
  printf '%s\n' 'Usage: test-host-dxvk.sh [dxvk|wined3d] [--d3d11|--iosurface]' >&2
  exit 2
fi
crossover=${CROSSOVER_ROOT:-/Applications/CrossOver.app/Contents/SharedSupport/CrossOver}
: "${CX_BOTTLE:?Set CX_BOTTLE to a dedicated CrossOver test bottle}"
cc=${ARTEMIS_D3D_CC:-${ARTEMIS_D3D9_CC:-x86_64-w64-mingw32-gcc}}
api=d3d9
override=d3d9
if [[ $presentation == --d3d11 ]]; then
  api=d3d11
  override=d3d11,dxgi
  candidate=${ARTEMIS_DXVK_D3D11_DIR:-}
  if [[ -n $candidate && ( ! -f $candidate/d3d11.dll || ! -f $candidate/dxgi.dll ) ]]; then
    printf '%s\n' 'The D3D11 candidate directory must contain d3d11.dll and dxgi.dll.' >&2
    exit 2
  fi
else
  candidate=${ARTEMIS_DXVK_D3D9_DLL:-}
  if [[ -n $candidate && ! -f $candidate ]]; then
    printf '%s\n' 'The D3D9 candidate must be a d3d9.dll file.' >&2
    exit 2
  fi
fi
moltenvk=${ARTEMIS_MOLTENVK_DYLIB:-}
wine_args=(--no-gui --debugmsg -all)
graphics_backend=$backend
if [[ -n $candidate || -n $moltenvk ]]; then
  if [[ $backend != dxvk || -z $candidate || $moltenvk != /* || ! -f $moltenvk ]]; then
    printf '%s\n' 'Specify both a DXVK candidate and an absolute MoltenVK dylib path in DXVK mode.' >&2
    exit 2
  fi
  # The bundled DXVK selector would replace the application-local DLL and Vulkan loader.
  graphics_backend=wined3d
  wine_args=(--no-gui --debugmsg +loaddll --dll "$override=n" --env "CX_LIBVULKAN=$moltenvk")
fi
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
"$cc" -std=c11 -O2 -Wall -Wextra -Werror "tools/host-smoke/artemis_$api.c" \
  -o "$scratch/artemis-$api.exe" "-l$api" -ld3dcompiler_47 -luser32
if [[ -n $candidate ]]; then
  if [[ $api == d3d11 ]]; then
    cp "$candidate/d3d11.dll" "$candidate/dxgi.dll" "$scratch/"
  else
    cp "$candidate" "$scratch/d3d9.dll"
  fi
fi
if [[ $presentation == --iosurface ]]; then
  : "${FLUTTER_MACOS_FRAMEWORK_DIR:?Set this to the directory containing FlutterMacOS.framework}"
  framework_dir=$(cd "$FLUTTER_MACOS_FRAMEWORK_DIR" && pwd)
  MACOSX_DEPLOYMENT_TARGET=12.0 xcrun swiftc -parse-as-library -swift-version 5 \
    -strict-concurrency=complete -warnings-as-errors -module-cache-path "$scratch/swift-modules" \
    -F "$framework_dir" -Xlinker -rpath -Xlinker "$framework_dir" \
    tools/host-smoke/ArtemisDXVKTextureSmoke.swift -o "$scratch/readback-texture"
fi

run_wine() (
  cd "$scratch"
  CX_GRAPHICS_BACKEND=$graphics_backend DXVK_LOG_LEVEL=info MVK_CONFIG_LOG_LEVEL=3 \
    "$crossover/bin/wine" "${wine_args[@]}" "artemis-$api.exe" "$@"
)
status=0
if [[ $presentation == --iosurface ]]; then
  run_wine --stream-bgra 2> "$scratch/result.log" | "$scratch/readback-texture" || status=$?
else
  run_wine > "$scratch/result.log" 2>&1 || status=$?
fi

awk '/^info: +DXVK: / && !dxvk++ || /MoltenVK version/ && !mvk++ ||
     /Created VkDevice|D3D9 adapter:|D3D11 device:|^[pv]s_[235]_0|failed:|cannot reserve|^err:/' \
     "$scratch/result.log"
if [[ $backend == dxvk ]]; then
  if ! grep -Eq '^info: +DXVK: ' "$scratch/result.log" ||
     ! grep -Fq '[mvk-info] Created VkDevice' "$scratch/result.log"; then
    printf '%s\n' 'The run did not confirm DXVK and MoltenVK device creation.' >&2
    exit 2
  fi
  if [[ -n $candidate ]]; then
    for dll in ${override//,/ }; do
      if ! grep -Eiq "loaddll:.*Loaded .*$dll\\.dll\".*: native" "$scratch/result.log"; then
        printf 'The run did not load native %s.dll.\n' "$dll" >&2
        exit 2
      fi
    done
  fi
elif grep -Eq '^info: +DXVK: ' "$scratch/result.log"; then
  printf '%s\n' 'The WineD3D control unexpectedly loaded DXVK.' >&2
  exit 2
fi
exit "$status"
