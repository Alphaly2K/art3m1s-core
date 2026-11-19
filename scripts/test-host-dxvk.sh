#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $(uname -s) != Darwin ]]; then
  printf '%s\n' 'This check requires macOS and CrossOver.' >&2
  exit 2
fi
backend=${1:-dxvk}
usage() { printf '%s\n' 'Usage: test-host-dxvk.sh [dxvk|wined3d] [--d3d11] [--iosurface|--worker|--interop]' >&2; exit 2; }
[[ $backend == dxvk || $backend == wined3d ]] || usage
if [[ $# -gt 0 ]]; then shift; fi
api=d3d9
iosurface=0
worker=0
interop=0
for option; do
  case $option in
    --d3d11) api=d3d11 ;;
    --iosurface) iosurface=1 ;;
    --worker) worker=1 ;;
    --interop) interop=1 ;;
    *) usage ;;
  esac
done
if ((iosurface + worker + interop > 1)) || [[ $api != d3d11 && ( $worker == 1 || $interop == 1 ) ]]; then usage; fi
crossover=${CROSSOVER_ROOT:-/Applications/CrossOver.app/Contents/SharedSupport/CrossOver}
: "${CX_BOTTLE:?Set CX_BOTTLE to a dedicated CrossOver test bottle}"
cc=${ARTEMIS_D3D_CC:-${ARTEMIS_D3D9_CC:-x86_64-w64-mingw32-gcc}}
override=d3d9
if [[ $api == d3d11 ]]; then
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
sources=("tools/host-smoke/artemis_$api.c")
libraries=("-l$api" -ld3dcompiler_47 -luser32)
includes=()
if [[ $interop == 1 ]]; then
  sources=(tools/host-smoke/artemis_d3d11_interop.c)
  includes=(-I "${ARTEMIS_VULKAN_INCLUDE:?Set this to a directory containing vulkan/vulkan_core.h}")
elif [[ $api == d3d11 ]]; then
  if [[ $worker == 1 ]]; then sources=(tools/host-bindings/d3d11/artemis_shader_worker.c); fi
  sources+=(tools/host-bindings/d3d11/artemis_shader_pass.c)
  libraries+=(-ldxguid)
fi
"$cc" -std=c11 -O2 -Wall -Wextra -Werror ${includes[@]+"${includes[@]}"} "${sources[@]}" \
  -o "$scratch/artemis-$api.exe" "${libraries[@]}"
if [[ -n $candidate ]]; then
  if [[ $api == d3d11 ]]; then
    cp "$candidate/d3d11.dll" "$candidate/dxgi.dll" "$scratch/"
  else
    cp "$candidate" "$scratch/d3d9.dll"
  fi
fi
if [[ $iosurface == 1 ]]; then
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
if [[ $worker == 1 ]]; then
  cargo build --quiet --locked --manifest-path crates/artemis-shader-host/Cargo.toml \
    --target-dir target/shader-host --example smoke
  client="$PWD/target/shader-host/debug/examples/smoke"
  if [[ ${ARTEMIS_D3D11_ARFP:-0} == 1 ]]; then
    core_build=${ART3M1S_V2_TARGET_DIR:-target/host-v2}
    cargo build --quiet --locked --lib --manifest-path crates/artemis-core/Cargo.toml --target-dir "$core_build"
    core_lib=$(cd "$core_build/debug" && pwd)
    "${CC:-cc}" -std=c11 -Wall -Wextra -Werror -pedantic -Idoc/abi tools/host-smoke/artemis_v2.c \
      -L"$core_lib" -lart3m1s_core -Wl,-rpath,"$core_lib" -o "$scratch/arfp"
    "$scratch/arfp" --shader-frame > "$scratch/frame"
    export ARTEMIS_D3D11_FRAME="$scratch/frame"
  fi
  (cd "$scratch"
   CX_GRAPHICS_BACKEND=$graphics_backend DXVK_LOG_LEVEL=info MVK_CONFIG_LOG_LEVEL=3 \
     "$client" "$crossover/bin/wine" "${wine_args[@]}" "artemis-$api.exe") \
     2> "$scratch/result.log" || status=$?
elif [[ $iosurface == 1 ]]; then
  run_wine --stream-bgra 2> "$scratch/result.log" | "$scratch/readback-texture" || status=$?
else
  run_wine > "$scratch/result.log" 2>&1 || status=$?
fi

awk '/^info: +DXVK: / && !dxvk++ || /MoltenVK version/ && !mvk++ ||
     /Created VkDevice|D3D9 adapter:|D3D11 device:|D3D11 interop:|^[pv]s_[235]_0|^\[mvk-error\]|failed:|cannot reserve|^err:|^Error:/ ||
     /panicked at/ { print; if (/panicked at/ && getline > 0) print }' \
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
