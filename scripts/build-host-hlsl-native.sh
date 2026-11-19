#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 3 || $(uname -s) != Darwin ]]; then
  printf '%s\n' 'Usage on macOS: bash scripts/build-host-hlsl-native.sh VKD3D_PREFIX VKD3D_SOURCE_TAR_XZ OUTPUT_DIRECTORY' >&2
  exit 2
fi
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
prefix=$(cd "$1" && pwd)
source_archive=$2
output=$3
if [[ -e $output || -L $output ]]; then
  printf '%s\n' 'The output directory must not already exist.' >&2
  exit 2
fi
version=$(pkg-config --modversion "$prefix/lib/pkgconfig/libvkd3d-utils.pc")
if [[ $version != 2.1 ]]; then
  printf '%s\n' 'This script requires a vkd3d 2.1 installation.' >&2
  exit 2
fi

scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
mkdir "$scratch/licenses" "$scratch/source"
for name in COPYING LICENSE AUTHORS; do
  tar -xOf "$source_archive" "vkd3d-$version/$name" > "$scratch/licenses/vkd3d-$name.txt"
done
cp "$source_archive" "$scratch/source/vkd3d-$version.tar.xz"

libraries=(libvkd3d-utils.1.dylib libvkd3d-shader.1.dylib libvkd3d.1.dylib)
tool=artemis-hlsl-compile
xcrun clang -std=c11 -O2 -Wall -Wextra -Werror -isystem "$prefix/include/vkd3d" \
  "$root/tools/host-bindings/d3d11/artemis_hlsl_compile.c" "$prefix/lib/${libraries[0]}" \
  -o "$scratch/$tool"

for name in "${libraries[@]}"; do
  cp -L "$prefix/lib/$name" "$scratch/$name"
  chmod u+w "$scratch/$name"
done
for name in "$tool" "${libraries[@]}"; do
  binary="$scratch/$name"
  for dependency in "${libraries[@]}"; do
    install_name_tool -change "$prefix/lib/$dependency" "@loader_path/$dependency" "$binary"
  done
  if [[ $name != "$tool" ]]; then
    install_name_tool -id "@loader_path/$name" "$binary"
  fi
  codesign --force --sign - --timestamp=none "$binary"
done
mkdir "$output"
mv "$scratch"/* "$output/"
