#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 || $(uname -s) != Darwin ]]; then
  printf '%s\n' 'Usage on macOS: bash scripts/build-host-dxvk-native.sh OUTPUT_DIRECTORY' >&2
  exit 2
fi
: "${ARTEMIS_DXVK_NATIVE_SOURCE:?Set this to the patched DXVK source checkout}"
: "${ARTEMIS_DXVK_NATIVE_BUILD:?Set this to the matching native build directory}"
: "${ARTEMIS_MOLTENVK_DYLIB:?Set this to the bundled MoltenVK dylib}"

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
source_dir=$(cd "$ARTEMIS_DXVK_NATIVE_SOURCE" && pwd)
build_dir=$(cd "$ARTEMIS_DXVK_NATIVE_BUILD" && pwd)
molten_dir=$(cd "$(dirname "$ARTEMIS_MOLTENVK_DYLIB")" && pwd)
if ! git -C "$source_dir" apply --reverse --check "$root/tools/host-bindings/d3d11/dxvk-macos-runtime.patch"; then
  printf '%s\n' 'Apply the macOS runtime patch to DXVK and rebuild before packaging.' >&2
  exit 2
fi
if [[ -e $1 || -L $1 ]]; then
  printf '%s\n' 'The output directory must not already exist.' >&2
  exit 2
fi
sdl_libdir=$(pkg-config --variable=libdir sdl3)
sdl_prefix=$(pkg-config --variable=prefix sdl3)
sdl_license=${ARTEMIS_SDL3_LICENSE:-$sdl_prefix/share/licenses/SDL3/LICENSE.txt}
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
mkdir "$scratch/licenses"

d3d11=libdxvk_d3d11.0.dylib
dxgi=libdxvk_dxgi.0.dylib
molten=libMoltenVK.dylib
sdl=libSDL3.0.dylib
bridge=libart3m1s_artemis_dxvk_host.dylib
cp -L "$build_dir/src/d3d11/$d3d11" "$build_dir/src/dxgi/$dxgi" "$sdl_libdir/$sdl" "$scratch/"
cp -L "$ARTEMIS_MOLTENVK_DYLIB" "$scratch/$molten"
cp "$source_dir/LICENSE" "$scratch/licenses/DXVK.txt"
cp "$source_dir/subprojects/dxbc-spirv/LICENSE" "$scratch/licenses/dxbc-spirv.txt"
cp "$source_dir/subprojects/libdisplay-info/LICENSE" "$scratch/licenses/libdisplay-info.txt"
cp "$molten_dir/LICENSE.MoltenVK" "$scratch/licenses/MoltenVK.txt"
cp "$sdl_license" "$scratch/licenses/SDL3.txt"
if [[ -f $molten_dir/NOTICE ]]; then
  cp "$molten_dir/NOTICE" "$scratch/licenses/MoltenVK-NOTICE.txt"
fi
printf '%s\n' 'DXVK was modified for the native macOS build and to load adjacent runtime libraries.' \
  > "$scratch/licenses/DXVK-changes.txt"

for name in "$d3d11" "$dxgi" "$molten" "$sdl"; do
  chmod u+w "$scratch/$name"
  install_name_tool -id "@rpath/$name" "$scratch/$name"
done
install_name_tool -change "@rpath/$dxgi" "@loader_path/$dxgi" "$scratch/$d3d11"

bindings="$root/tools/host-bindings/d3d11"
xcrun clang++ -std=c++17 -Wall -Wextra -Werror -Wno-missing-field-initializers \
  -fvisibility=hidden -dynamiclib -Wl,-install_name,"@rpath/$bridge" -Wl,-rpath,@loader_path \
  -isystem "$source_dir/include/native/windows" -isystem "$source_dir/include/native/directx" \
  -isystem "$source_dir/include/native" -isystem "$source_dir/include/vulkan/include" \
  -isystem "$source_dir/include/spirv/include" -isystem "$source_dir/src/dxgi" -isystem "$source_dir/src" \
  "$bindings/artemis_dxvk_device.cpp" "$bindings/artemis_dxvk_pass.cpp" \
  "$scratch/$d3d11" "$scratch/$dxgi" "$scratch/$molten" \
  -framework IOSurface -framework CoreVideo -framework CoreFoundation -o "$scratch/$bridge"

for name in "$d3d11" "$dxgi" "$molten" "$sdl" "$bridge"; do
  codesign --force --sign - --timestamp=none "$scratch/$name"
done
mkdir "$1"
mv "$scratch"/* "$1/"
