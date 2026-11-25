#!/usr/bin/env bash
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
case "$(uname -s)" in
  Darwin) extension=dylib ;;
  Linux) extension=so ;;
  *) printf '%s\n' 'The RFVP smoke test supports macOS and Linux.' >&2; exit 1 ;;
esac
build_dir="${CARGO_TARGET_DIR:-target}"
cargo build --locked --no-default-features --features rfvp-host-v2 --lib "$@"
lib_dir="$(cd "$build_dir/debug" && pwd)"
python3 - "$lib_dir/libart3m1s_core.$extension" <<'PY'
import os
import subprocess
import sys
from pathlib import Path
from tempfile import TemporaryDirectory

library = Path(sys.argv[1])
with TemporaryDirectory(prefix='rfvp-v2-smoke-') as directory:
    root = Path(directory)
    game = root / 'game'
    game.mkdir()
    subprocess.run([sys.executable, 'tools/host-smoke/rfvp_tile.py', game / 'tile.hcb'], check=True)
    compiler = os.environ.get('CC', 'cc')
    runner = root / 'host'
    subprocess.run([compiler, '-std=c11', '-Wall', '-Wextra', '-Werror', '-pedantic', '-pthread',
                    '-Idoc/abi', 'tools/host-smoke/rfvp_v2.c', library,
                    '-Wl,-rpath,' + str(library.parent), '-o', runner], check=True)
    subprocess.run([runner, game], check=True)
PY
