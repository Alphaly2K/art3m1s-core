#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

manifest=crates/artemis-core/Cargo.toml
features=(--all-features)
legacy=0
if [[ $# == 1 && $1 == --legacy-luau ]]; then
  manifest=Cargo.toml
  features=(--features gl-backend,metal-backend,vulkan-backend)
  legacy=1
elif [[ $# != 0 ]]; then
  printf 'Usage: %s [--legacy-luau]\n' "$0" >&2
  exit 2
fi

cargo tree --locked "${features[@]}" --manifest-path "$manifest" \
  --target all --edges normal,build --prefix none --format '{p}|{f}' |
  awk -F '|' -v legacy="$legacy" '
    function reject(message) {
      print message > "/dev/stderr"
      failed = 1
    }
    {
      split($1, description, " ")
      package = description[1]
      features = $2
      sub(/ \(\*\)$/, "", features)
      if (seen[package SUBSEP features]++) next
      features = "," features ","
      if (package == "luau0-src") luau_source = 1

      if (package == "mlua" || package == "mlua-sys") {
        if (features !~ /,luau,/ || features ~ /,(lua5[1-5]|luajit|luajit52|module),/)
          reject("Artemis must use Luau in " package)
      }
      if (legacy) next

      if (package ~ /^(art3m1s-(core|media|native-render|rfvp|krkr|siglus)|art3m1s-artemis-(metal-host|media-host)|pfs-upk-rust|pf8|ash|glow|wgpu|shaderc|spirv-cross2|cpal|sdl3|winit|ffmpeg-next|ffmpeg-sys-next)$/) {
        reject("Host dependency in standalone Core: " package)
      }
      if (package == "art3m1s-render") {
        if (features !~ /,artemis-frame,/ || features ~ /,(artemis|gl|vulkan|metal|metal-dxvk|runtime-shader),/)
          reject("Standalone Core must use only the Artemis frame data model")
      }
      if (package == "art3m1s-emote" && features ~ /,filesystem,/)
        reject("Standalone Core must load E-Mote models from Host-provided bytes")
      if (package == "asb-interpreter" && features ~ /,console-diagnostics,/)
        reject("Standalone Core must not enable interpreter console diagnostics")
      if (package == "asb-interpreter" && features ~ /,filesystem-fallback,/)
        reject("Standalone Core must route file checks through the Host")
      if (package == "asb-interpreter" && features ~ /,system-clock,/)
        reject("Standalone Core must use Host time and seed")
    }
    END {
      if (!luau_source) reject("Luau source is missing from the Artemis build")
      exit failed
    }
  '
