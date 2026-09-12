#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

run() {
  printf '\n==> %s\n' "$*"
  "$@"
}

if [[ "$(uname -s)" == "Darwin" ]]; then
  # CGL headless contexts are sensitive to other tests that create graphics
  # resources in the same run. Keep them as an explicit hardware test group.
  cgl_tests=(
    backend::gl::tests::texture_present_keeps_the_stage_upright
    backend::gl::tests::texture_present_limits_writes_to_top_left_damage
    backend::gl::tests::iosurface_memory_rows_keep_the_stage_upright
    backend::gl::tests::iosurface_damage_writes_the_first_memory_row_for_stage_top
  )
  skip_args=()
  for test_name in "${cgl_tests[@]}"; do
    skip_args+=(--skip "$test_name")
  done
  if [[ "${ART3M1S_RUN_CGL_TESTS:-0}" == "1" ]]; then
    for test_name in "${cgl_tests[@]}"; do
      run cargo test --lib "$test_name" -- --exact --test-threads=1
    done
  fi
  run cargo test --all-targets -- "${skip_args[@]}"
else
  run cargo test --all-targets
fi
run cargo test --manifest-path crates/asb-interpreter/Cargo.toml --all-targets
run cargo test --manifest-path crates/asb-interpreter/Cargo.toml \
  --no-default-features --features backend-luau --lib --tests
run cargo test --manifest-path crates/art3m1s-emote/Cargo.toml --all-targets
run cargo test --manifest-path crates/eluna/Cargo.toml --all-targets
run cargo test --manifest-path crates/pf8/Cargo.toml --all-targets
run cargo test --manifest-path crates/pfs-upk-rust/Cargo.toml --all-targets
run cargo build --release --lib
run "$repo_root/scripts/check-ffi-exports.sh"
