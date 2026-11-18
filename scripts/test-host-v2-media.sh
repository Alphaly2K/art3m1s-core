#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" != Darwin ]]; then
  printf '%s\n' 'This smoke test currently targets the macOS FFmpeg bundle.' >&2
  exit 1
fi
export FFMPEG_DIR="$(cd "${FFMPEG_DIR:?Set FFMPEG_DIR to an FFmpeg SDK prefix}" && pwd)"
ffmpeg_cli="${FFMPEG_CLI:-ffmpeg}"
build="${ARTEMIS_MEDIA_TARGET_DIR:-target/host-v2-media}"
cargo build --locked --manifest-path crates/artemis-media-host/Cargo.toml --target-dir "$build" "$@"
scratch="$(mktemp -d)"
trap 'rm -f "$scratch/excluded.mp4" "$scratch/clip.wmv" "$scratch/clip.ogv" "$scratch/probe" && rmdir "$scratch"' EXIT
cc -std=c11 -Wall -Wextra -Werror -Idoc/abi tools/host-smoke/media_host_v1.c -o "$scratch/probe"
video_input='color=blue:s=32x32:r=24:d=1'
"$ffmpeg_cli" -nostdin -v error -f lavfi -i "$video_input" -c:v mpeg4 "$scratch/excluded.mp4"
"$ffmpeg_cli" -nostdin -v error -f lavfi -i "$video_input" -f lavfi -i sine=frequency=440:duration=1 \
  -c:v wmv2 -c:a wmav2 "$scratch/clip.wmv"
clips=("$scratch/clip.wmv")
if "$ffmpeg_cli" -hide_banner -encoders 2>/dev/null | awk '$2 == "libtheora" { found = 1 } END { exit !found }'; then
  "$ffmpeg_cli" -nostdin -v error -f lavfi -i "$video_input" -c:v libtheora -g 1 "$scratch/clip.ogv"
  clips+=("$scratch/clip.ogv")
else
  printf '%s\n' 'Theora fixture skipped: FFmpeg CLI lacks the libtheora encoder.'
fi
DYLD_LIBRARY_PATH="$FFMPEG_DIR/lib${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" \
  "$scratch/probe" "$build/debug/libart3m1s_artemis_media_host.dylib" "$scratch/excluded.mp4" "${clips[@]}"
