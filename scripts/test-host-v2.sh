#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
case "$(uname -s)" in
  Darwin) extension=dylib ;;
  Linux) extension=so ;;
  *) printf '%s\n' 'This smoke test supports native macOS and Linux.' >&2; exit 1 ;;
esac

build_dir="${ART3M1S_V2_TARGET_DIR:-target/host-v2}"
cargo build --locked --lib --manifest-path crates/artemis-core/Cargo.toml --target-dir "$build_dir" "$@"
lib_dir="$(cd "$build_dir/debug" && pwd)"
library="$lib_dir/libart3m1s_core.$extension"
if [[ "$extension" == dylib ]]; then
  exports="$(nm -gU "$library" | awk '{print $NF}' | sed 's/^_//')"
else
  exports="$(nm -D --defined-only "$library" | awk '{print $NF}')"
fi
if [[ "$exports" != art3m1s_get_api_v2 ]]; then
  printf 'Unexpected v2 exports:\n%s\n' "$exports" >&2
  exit 1
fi

printf '#include "art3m1s_core_v2.draft.h"\n' |
  "${CXX:-c++}" -std=c++17 -Wall -Wextra -Werror -pedantic -Idoc/abi -x c++ -fsyntax-only -
scratch="$(mktemp -d)"
trap 'rm -f "$scratch/artemis-v2" && rmdir "$scratch"' EXIT
"${CC:-cc}" -std=c11 -Wall -Wextra -Werror -pedantic -Idoc/abi tools/host-smoke/artemis_v2.c \
  -L"$lib_dir" -lart3m1s_core -Wl,-rpath,"$lib_dir" -o "$scratch/artemis-v2"
"$scratch/artemis-v2"
