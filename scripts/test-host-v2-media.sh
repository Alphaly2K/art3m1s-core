#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" != Darwin ]]; then
  printf '%s\n' 'This smoke test requires macOS.' >&2
  exit 1
fi
ffmpeg_cli="${FFMPEG_CLI:-ffmpeg}"
if ! command -v "$ffmpeg_cli" >/dev/null; then
  printf '%s\n' 'Install FFmpeg or set FFMPEG_CLI to generate the media fixtures.' >&2
  exit 2
fi
scratch="$(mktemp -d)"
trap 'rm -rf -- "$scratch"' EXIT
if [[ ${1:-} == --bundle ]]; then
  if [[ $# != 2 ]]; then
    printf '%s\n' 'Usage: test-host-v2-media.sh --bundle APP_BUNDLE' >&2
    exit 2
  fi
  contents="$(cd "$2/Contents" && pwd)"
  library="$contents/MacOS/libart3m1s_artemis_media_host.dylib"
  library_dir="$contents/Frameworks"
  unset DYLD_LIBRARY_PATH DYLD_FALLBACK_LIBRARY_PATH
else
  cd "$root"
  export FFMPEG_DIR="$(cd "${FFMPEG_DIR:?Set FFMPEG_DIR to an FFmpeg SDK prefix}" && pwd)"
  build="${ARTEMIS_MEDIA_TARGET_DIR:-target/host-v2-media}"
  cargo build --locked --manifest-path crates/artemis-media-host/Cargo.toml --target-dir "$build" "$@"
  library="$build/debug/libart3m1s_artemis_media_host.dylib"
  library_dir="$FFMPEG_DIR/lib"
fi
cd "$root"
cc -std=c11 -Wall -Wextra -Werror -Idoc/abi tools/host-smoke/media_host_v1.c \
  -Xlinker -rpath -Xlinker "$library_dir" -o "$scratch/probe"
video_input='color=blue:s=32x32:r=24:d=1'
"$ffmpeg_cli" -nostdin -v error -f lavfi -i "$video_input" -c:v mpeg4 "$scratch/excluded.mp4"
"$ffmpeg_cli" -nostdin -v error -f lavfi -i "$video_input" -f lavfi -i sine=frequency=440:duration=1 \
  -c:v msmpeg4v3 -c:a pcm_s16le "$scratch/clip.avi"
"$ffmpeg_cli" -nostdin -v error -f lavfi -i "$video_input" -f lavfi -i sine=frequency=440:duration=1 \
  -c:v wmv2 -c:a wmav2 "$scratch/clip.wmv"
"$ffmpeg_cli" -nostdin -v error -f lavfi -i 'color=blue:s=32x32:r=24:d=6' \
  -itsoffset 0.75 -f lavfi -i sine=frequency=440:duration=5 \
  -af 'aselect=not(between(t\,2\,3))' -c:v wmv2 -c:a wmav2 -output_ts_offset 5 "$scratch/timeline.wmv"
"$scratch/probe" "$library" "$scratch/excluded.mp4" "$scratch/clip.wmv" "$scratch/timeline.wmv" \
  tools/host-smoke/fixtures/blue-theora.ogv tools/host-smoke/fixtures/blue-theora-repeat.ogv "$scratch/clip.avi"
if [[ ${ARTEMIS_MEDIA_SWIFT_SMOKE:-0} == 1 ]]; then
  "$ffmpeg_cli" -nostdin -v error -f lavfi -i 'color=blue:s=32x32:r=24:d=18' \
    -itsoffset 0.75 -f lavfi -i sine=frequency=440:duration=17 \
    -af 'aselect=not(between(t\,2\,3))' -c:v wmv2 -c:a wmav2 -output_ts_offset 5 "$scratch/long.wmv"
  MACOSX_DEPLOYMENT_TARGET=12.0 swiftc -swift-version 5 -strict-concurrency=complete -warnings-as-errors \
    -module-cache-path "$scratch/swift-modules" -Xcc -Idoc/abi \
    -import-objc-header tools/host-bindings/swift/ArtemisHostBridge.h \
    tools/host-bindings/swift/ArtemisMacSession.swift tools/host-bindings/swift/ArtemisLegacyMedia.swift \
    tools/host-smoke/ArtemisLegacyMediaSmoke.swift \
    -Xlinker -rpath -Xlinker "$library_dir" -o "$scratch/swift-probe"
  "$scratch/swift-probe" "$library" "$scratch/long.wmv"
fi
