#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $(uname -s) != Darwin ]]; then
  printf '%s\n' 'This check requires macOS and a Metal device.' >&2
  exit 2
fi
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
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
ln -s "$ARTEMIS_MOLTENVK_DYLIB" "$scratch/libvulkan.1.dylib"
ln -s "$ARTEMIS_MOLTENVK_DYLIB" "$scratch/libvulkan.dylib"
xcrun clang++ -std=c++17 -Wall -Wextra -Werror -Wno-missing-field-initializers \
  -isystem "$source_dir/include/native/windows" -isystem "$source_dir/include/native/directx" \
  -isystem "$source_dir/include/native" -isystem "$source_dir/include/vulkan/include" \
  -isystem "$source_dir/include/spirv/include" -isystem "$source_dir/src/dxgi" \
  -isystem "$source_dir/src" tools/host-smoke/artemis_dxvk_surface.cpp \
  "$build_dir/src/d3d11/libdxvk_d3d11.0.dylib" "$build_dir/src/dxgi/libdxvk_dxgi.0.dylib" \
  "$ARTEMIS_MOLTENVK_DYLIB" -Wl,-rpath,"$build_dir/src/d3d11" -Wl,-rpath,"$build_dir/src/dxgi" \
  -Wl,-rpath,"$(dirname "$ARTEMIS_MOLTENVK_DYLIB")" -framework CoreVideo -framework CoreFoundation \
  -o "$scratch/artemis-dxvk-native"
(cd "$scratch"
 DYLD_LIBRARY_PATH="$scratch:$sdl_libdir${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" \
   DXVK_WSI_DRIVER=SDL3 SDL_VULKAN_LIBRARY="$ARTEMIS_MOLTENVK_DYLIB" \
   DXVK_LOG_LEVEL=${DXVK_LOG_LEVEL:-warn} MVK_CONFIG_LOG_LEVEL=${MVK_CONFIG_LOG_LEVEL:-2} \
   ./artemis-dxvk-native)
