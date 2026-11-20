#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo tree --locked --all-features --manifest-path crates/artemis-core/Cargo.toml \
  --target all --edges normal,build --prefix none --format '{p}|{f}' |
  awk -F '|' '
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

      if (package ~ /^(art3m1s-(core|media|native-render|rfvp|krkr|siglus)|art3m1s-artemis-(metal-host|media-host)|pfs-upk-rust|pf8|ash|glow|wgpu|shaderc|spirv-cross2|cpal|sdl3|winit|ffmpeg-next|ffmpeg-sys-next)$/) {
        reject("Host dependency in standalone Core: " package)
      }
      if (package == "art3m1s-render") {
        if (features !~ /,artemis,/ || features ~ /,(gl|vulkan|metal|metal-dxvk|runtime-shader),/)
          reject("Standalone Core must use the renderer data model without native backends")
      }
      if (package == "art3m1s-emote" && features ~ /,filesystem,/)
        reject("Standalone Core must load E-Mote models from Host-provided bytes")
      if (package == "asb-interpreter" && features ~ /,console-diagnostics,/)
        reject("Standalone Core must not enable interpreter console diagnostics")
      if (package == "asb-interpreter" && features ~ /,system-clock,/)
        reject("Standalone Core must use Host time and seed")
      if (package == "mlua" || package == "mlua-sys") {
        if (features !~ /,luau,/ || features ~ /,(lua5[1-5]|luajit|luajit52|module),/)
          reject("Standalone Core must use Luau in " package)
      }
    }
    END {
      if (!luau_source) reject("Luau source is missing from standalone Core")
      exit failed
    }
  '
