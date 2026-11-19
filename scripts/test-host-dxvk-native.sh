#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $(uname -s) != Darwin ]]; then
  printf '%s\n' 'This check requires macOS and a Metal device.' >&2
  exit 2
fi
mode=${1:-}
if [[ $# -gt 1 || ( -n $mode && $mode != --hlsl && $mode != --arfp ) ]]; then
  printf '%s\n' 'Usage: test-host-dxvk-native.sh [--hlsl|--arfp]' >&2
  exit 2
fi
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
runtime_dir=${ARTEMIS_DXVK_RUNTIME_DIR:-}
xcrun clang++ -std=c++17 -fobjc-arc -Wall -Wextra -Werror \
  -c tools/host-smoke/artemis_dxvk_metal_writer.mm -o "$scratch/metal-writer.o"
if [[ -n $runtime_dir ]]; then
  if [[ $mode != --arfp ]]; then
    printf '%s\n' 'Use --arfp to test a packaged DXVK runtime.' >&2
    exit 2
  fi
  runtime_dir=$(cd "$runtime_dir" && pwd)
else
  : "${ARTEMIS_DXVK_NATIVE_SOURCE:?Set this to the DXVK source checkout}"
  : "${ARTEMIS_DXVK_NATIVE_BUILD:?Set this to the matching native build directory}"
  : "${ARTEMIS_MOLTENVK_DYLIB:?Set this to an absolute path to libMoltenVK.dylib}"
  source_dir=$(cd "$ARTEMIS_DXVK_NATIVE_SOURCE" && pwd)
  build_dir=$(cd "$ARTEMIS_DXVK_NATIVE_BUILD" && pwd)
  sdl_libdir=$(pkg-config --variable=libdir sdl3)
  if [[ $ARTEMIS_MOLTENVK_DYLIB != /* || ! -f $ARTEMIS_MOLTENVK_DYLIB ]]; then
    printf '%s\n' 'ARTEMIS_MOLTENVK_DYLIB must be an absolute path to a MoltenVK dylib.' >&2
    exit 2
  fi
  ln -s "$ARTEMIS_MOLTENVK_DYLIB" "$scratch/libvulkan.1.dylib"
  ln -s "$ARTEMIS_MOLTENVK_DYLIB" "$scratch/libvulkan.dylib"
  compile=(xcrun clang++ -std=c++17 -Wall -Wextra -Werror -Wno-missing-field-initializers
    -isystem "$source_dir/include/native/windows" -isystem "$source_dir/include/native/directx"
    -isystem "$source_dir/include/native" -isystem "$source_dir/include/vulkan/include"
    -isystem "$source_dir/include/spirv/include" -isystem "$source_dir/src/dxgi" -isystem "$source_dir/src")
  link=(
    tools/host-bindings/d3d11/artemis_dxvk_device.cpp tools/host-bindings/d3d11/artemis_dxvk_pass.cpp "$scratch/metal-writer.o"
    "$build_dir/src/d3d11/libdxvk_d3d11.0.dylib" "$build_dir/src/dxgi/libdxvk_dxgi.0.dylib"
    "$ARTEMIS_MOLTENVK_DYLIB" -Wl,-rpath,"$build_dir/src/d3d11" -Wl,-rpath,"$build_dir/src/dxgi"
    -Wl,-rpath,"$(dirname "$ARTEMIS_MOLTENVK_DYLIB")"
    -framework CoreVideo -framework CoreFoundation -framework Foundation -framework IOSurface -framework Metal)
fi

run_native() (
  cd "$scratch"
  if [[ -n $runtime_dir ]]; then
    unset DYLD_LIBRARY_PATH DYLD_FALLBACK_LIBRARY_PATH DXVK_WSI_DRIVER SDL_VULKAN_LIBRARY
    DXVK_LOG_LEVEL=${DXVK_LOG_LEVEL:-warn} MVK_CONFIG_LOG_LEVEL=${MVK_CONFIG_LOG_LEVEL:-2} "$@"
  else
    DYLD_LIBRARY_PATH="$scratch:$sdl_libdir${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" \
      DXVK_WSI_DRIVER=SDL3 SDL_VULKAN_LIBRARY="$ARTEMIS_MOLTENVK_DYLIB" \
      DXVK_LOG_LEVEL=${DXVK_LOG_LEVEL:-warn} MVK_CONFIG_LOG_LEVEL=${MVK_CONFIG_LOG_LEVEL:-2} \
      "$@"
  fi
)

effect_src="$PWD/tools/host-smoke/shaders/native-effect.hlsl"
if [[ $mode == --arfp ]]; then
  probe_link=(-L "native=$scratch" -l dylib=artemis_dxvk_probe)
  if [[ -n $runtime_dir ]]; then
    xcrun clang++ -dynamiclib -Wl,-install_name,@rpath/libartemis_dxvk_probe.dylib \
      "$scratch/metal-writer.o" -framework Metal -framework Foundation -framework IOSurface \
      -o "$scratch/libartemis_dxvk_probe.dylib"
    probe_link+=(-L "native=$runtime_dir" -l dylib=art3m1s_artemis_dxvk_host -C "link-arg=-Wl,-rpath,$runtime_dir")
  else
    "${compile[@]}" -dynamiclib -Wl,-install_name,@rpath/libartemis_dxvk_probe.dylib \
      "${link[@]}" -o "$scratch/libartemis_dxvk_probe.dylib"
  fi
  probe_link+=(-C "link-arg=-Wl,-rpath,$scratch")
  core_build=${ART3M1S_V2_TARGET_DIR:-target/host-v2}
  cargo build --quiet --locked --lib --manifest-path crates/artemis-core/Cargo.toml --target-dir "$core_build"
  core_lib=$(cd "$core_build/debug" && pwd)
  "${CC:-cc}" -std=c11 -Wall -Wextra -Werror -pedantic -Idoc/abi tools/host-smoke/artemis_v2.c \
    -L"$core_lib" -lart3m1s_core -Wl,-rpath,"$core_lib" -o "$scratch/arfp"
  "$scratch/arfp" --shader-sequence > "$scratch/frames"
  cargo rustc --quiet --locked --manifest-path tools/host-smoke/Cargo.toml --target-dir target/host-v2 \
    --features dxvk-smoke --bin artemis-dxvk-arfp -- "${probe_link[@]}"
  executable="$PWD/target/host-v2/debug/artemis-dxvk-arfp"
  effect_src="$scratch/effect.hlsl"
  run_native "$executable" frames > "$effect_src"
else
  executable="$scratch/artemis-dxvk-native"
  "${compile[@]}" tools/host-smoke/artemis_dxvk_surface.cpp "${link[@]}" -o "$executable"
fi

if [[ -z $mode ]]; then
  run_native "$executable"
else
  if [[ -n ${ARTEMIS_NATIVE_HLSL_COMPILER:-} ]]; then
    if [[ $ARTEMIS_NATIVE_HLSL_COMPILER != /* || ! -x $ARTEMIS_NATIVE_HLSL_COMPILER ]]; then
      printf '%s\n' 'ARTEMIS_NATIVE_HLSL_COMPILER must be an absolute executable path.' >&2
      exit 2
    fi
    compile_effect() { "$ARTEMIS_NATIVE_HLSL_COMPILER" "$1"; }
  else
    : "${CX_BOTTLE:?Set CX_BOTTLE to a dedicated CrossOver test bottle for HLSL compilation}"
    crossover=${CROSSOVER_ROOT:-/Applications/CrossOver.app/Contents/SharedSupport/CrossOver}
    "${ARTEMIS_D3D_CC:-x86_64-w64-mingw32-gcc}" -std=c11 -O2 -Wall -Wextra -Werror \
      tools/host-bindings/d3d11/artemis_hlsl_compile.c -o "$scratch/artemis-hlsl-compile.exe" -ld3dcompiler_47 -ldxguid
    compile_effect() (
      cd "$scratch"
      CX_GRAPHICS_BACKEND=wined3d "$crossover/bin/wine" --no-gui --debugmsg -all artemis-hlsl-compile.exe "$1"
    )
  fi
  compile_effect effect-pair < "$effect_src" > "$scratch/effect.ade"
  if [[ $mode == --arfp ]]; then
    run_native "$executable" frames effect.ade
    if [[ -n $runtime_dir ]]; then
      cargo build --quiet --locked --manifest-path crates/artemis-metal-host/Cargo.toml --target-dir target
      host_lib="$PWD/target/debug"
      "${CC:-cc}" -std=c11 -Wall -Wextra -Werror -DARTEMIS_DXVK_HOST_SMOKE -Idoc/abi \
        tools/host-smoke/artemis_v2.c tools/host-smoke/metal_abi.c \
        -L"$core_lib" -lart3m1s_core -Wl,-rpath,"$core_lib" \
        -L"$host_lib" -lart3m1s_artemis_metal_host -Wl,-rpath,"$host_lib" \
        -framework CoreFoundation -framework CoreVideo -o "$scratch/metal-host"
      run_native "$scratch/metal-host" --shader-host "$runtime_dir" effect.ade
      run_native "$scratch/metal-host" --shader-host-sprites "$runtime_dir" effect.ade
    fi
  else
    if printf '%s\n' 'float4 main(float3 normal : NORMAL0) : SV_Target { return float4(normal, 1); }' |
        compile_effect effect-pair > "$scratch/unsupported.ade" 2> "$scratch/unsupported.log"; then
      printf '%s\n' 'The compiler accepted an unsupported pixel input.' >&2
      exit 1
    fi
    if [[ -s "$scratch/unsupported.ade" ]] || ! grep -q 'Unsupported pixel input:' "$scratch/unsupported.log"; then
      printf '%s\n' 'Unsupported pixel input did not produce the expected diagnostic.' >&2
      exit 1
    fi
    printf '%s\n' 'float4 main() : SV_Target { return float4(0, 0, 1, 1); }' |
      compile_effect effect-pair > "$scratch/alternate.ade"
    run_native "$executable" effect.ade alternate.ade
  fi
fi
