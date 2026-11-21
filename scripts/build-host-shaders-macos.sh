#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 || $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
  printf '%s\n' 'Usage on Apple Silicon: bash scripts/build-host-shaders-macos.sh OUTPUT_DIRECTORY' >&2
  exit 2
fi
if [[ -e $1 || -L $1 ]]; then
  printf '%s\n' 'The output directory must not already exist.' >&2
  exit 2
fi
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
output="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
export MACOSX_DEPLOYMENT_TARGET=15.0

fetch_archive() {
  local url=$1 path=$2 hash=$3
  curl --fail --silent --show-error --location --proto '=https' --retry 3 -o "$path" "$url"
  printf '%s  %s\n' "$hash" "$path" | shasum -a 256 --check
}

tag=v3.1.1-macos.4
source="$scratch/dxvk"
git init -q "$source"
git -C "$source" fetch --no-tags --depth 1 https://github.com/metalsharp/DXVK-MacOS.git e847475b8ea97b92bc65385bad0eb88ef1f79ebf
git -C "$source" checkout -q --detach FETCH_HEAD
git -C "$source" submodule update --init --recursive --depth 1
for commit in dd1af1d412a8b77884df969f4973cbce78cab378 cbd2206d72d3d9dd9941d5d868bb03a858e85f9b; do
  git -C "$source" fetch --no-tags --depth 2 https://github.com/doitsujin/dxvk.git "$commit"
  git -C "$source" diff "$commit^" "$commit" -- . ':(exclude)subprojects/.wraplock' |
    git -C "$source" apply
done
bindings="$root/tools/host-bindings/d3d11"
git -C "$source" apply "$bindings/dxvk-macos-runtime.patch" "$bindings/dxvk-macos-ngx.patch"

archive="moltenvk-$tag-universal"
fetch_archive "https://github.com/metalsharp/DXVK-MacOS/releases/download/$tag/$archive.tar.zst" \
  "$scratch/moltenvk.tar.zst" f2db7e9d19a4759c82b27b240ca54c90d1abdd69fb36c175e06ffb7b1226e90d
tar -xf "$scratch/moltenvk.tar.zst" -C "$scratch"

sdl_version=3.4.16
sdl_name="SDL3-$sdl_version"
fetch_archive "https://github.com/libsdl-org/SDL/releases/download/release-$sdl_version/$sdl_name.tar.gz" \
  "$scratch/$sdl_name.tar.gz" 7322236cd12090c3eb40b9728be4d49c76f66ad17d04369584d4ecad5cf77c68
tar -xf "$scratch/$sdl_name.tar.gz" -C "$scratch"
sdl_prefix="$scratch/sdl-prefix"
cmake -S "$scratch/$sdl_name" -B "$scratch/sdl-build" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$sdl_prefix" \
  -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_OSX_DEPLOYMENT_TARGET="$MACOSX_DEPLOYMENT_TARGET" \
  -DSDL_SHARED=ON -DSDL_STATIC=OFF -DSDL_TEST_LIBRARY=OFF -DSDL_TESTS=OFF
cmake --build "$scratch/sdl-build"
cmake --install "$scratch/sdl-build"
export PKG_CONFIG_PATH="$sdl_prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"

meson setup "$scratch/native" "$source" --buildtype release --wrap-mode nodownload \
  --force-fallback-for libdisplay-info \
  -Denable_d3d8=false -Denable_d3d9=false -Denable_d3d10=false \
  -Dnative_sdl3=enabled -Dnative_sdl2=disabled -Dnative_glfw=disabled
ninja -C "$scratch/native" src/d3d11/libdxvk_d3d11.0.dylib src/dxgi/libdxvk_dxgi.0.dylib
ARTEMIS_DXVK_NATIVE_SOURCE="$source" ARTEMIS_DXVK_NATIVE_BUILD="$scratch/native" \
  ARTEMIS_MOLTENVK_DYLIB="$scratch/$archive/libMoltenVK.dylib" \
  ARTEMIS_SDL3_LICENSE="$scratch/$sdl_name/LICENSE.txt" \
  bash "$root/scripts/build-host-dxvk-native.sh" "$scratch/dxvk-package"

fetch_archive https://dl.winehq.org/vkd3d/source/vkd3d-2.1.tar.xz \
  "$scratch/vkd3d-2.1.tar.xz" 7510146aff2adfb4ae07ab890701a607e5ff7c66e57100cfc9f630ec92eeda6a
tar -xf "$scratch/vkd3d-2.1.tar.xz" -C "$scratch"
vkd3d_patch="$bindings/vkd3d-rdef-buffer-names.patch"
patch -d "$scratch/vkd3d-2.1" -p1 < "$vkd3d_patch"
(
  cd "$scratch/vkd3d-2.1"
  if ! "${PERL:-perl}" -MJSON -e 1 >/dev/null 2>&1; then
    fetch_archive https://www.cpan.org/authors/id/I/IS/ISHIGAKI/JSON-4.10.tar.gz \
      "$scratch/JSON-4.10.tar.gz" df8b5143d9a7de99c47b55f1a170bd1f69f711935c186a6dc0ab56dd05758e35
    tar -xf "$scratch/JSON-4.10.tar.gz" -C "$scratch"
    export PERL5LIB="$scratch/JSON-4.10/lib${PERL5LIB:+:$PERL5LIB}"
  fi
  MAKE=gmake SONAME_LIBVULKAN=libMoltenVK.dylib lt_cv_sys_max_cmd_len=262144 \
    CPPFLAGS="-I$source/include/vulkan/include -I$source/include/spirv/include" \
    CFLAGS='-O2 -fno-lto -Wno-ignored-attributes -Wno-unknown-attributes' \
    ./configure --prefix="$scratch/prefix" --disable-maintainer-mode --disable-tests \
    --without-xcb --without-opengl --without-ncurses
  gmake -j "$(sysctl -n hw.logicalcpu)"
  gmake install
)
bash "$root/scripts/build-host-hlsl-native.sh" "$scratch/prefix" \
  "$scratch/vkd3d-2.1.tar.xz" "$scratch/hlsl-package"
cp "$vkd3d_patch" "$scratch/hlsl-package/source/"

for binary in "$scratch/dxvk-package/"*.dylib "$scratch/hlsl-package/"*.dylib \
  "$scratch/hlsl-package/artemis-hlsl-compile"; do
  minimum=$(xcrun vtool -arch arm64 -show-build "$binary" | awk '
    $1 == "cmd" { command = $2 }
    $1 == "minos" && command == "LC_BUILD_VERSION" { print $2 }
    $1 == "version" && command == "LC_VERSION_MIN_MACOSX" { print $2 }
  ')
  if ! awk -v version="$minimum" -v target="$MACOSX_DEPLOYMENT_TARGET" 'BEGIN {
    if (version !~ /^[0-9]+\.[0-9]+(\.[0-9]+)?$/) exit 1
    split(version, minimum, ".")
    split(target, maximum, ".")
    for (i = 1; i <= 3; i++) {
      if (minimum[i] + 0 != maximum[i] + 0) exit (minimum[i] + 0 > maximum[i] + 0)
    }
  }'; then
    printf '%s requires macOS %s, package target is %s\n' \
      "$(basename "$binary")" "${minimum:-unknown}" "$MACOSX_DEPLOYMENT_TARGET" >&2
    exit 1
  fi
done

mkdir "$output"
mv "$scratch/dxvk-package" "$output/dxvk"
mv "$scratch/hlsl-package" "$output/hlsl"
