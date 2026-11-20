#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 2 ]]; then
  printf 'Usage: %s {legacy|v2} LIBRARY\n' "$0" >&2
  exit 2
fi

case "$1" in
  legacy)
    expected=(
      art3m1s_get_api_v1
      pfs_close
      pfs_entry_count
      pfs_entry_path
      pfs_entry_size
      pfs_file_size
      pfs_open
      pfs_open_with_encoding
      pfs_read
      pfs_unpack
    )
    ;;
  v2) expected=(art3m1s_get_api_v2) ;;
  *) printf 'Unknown Artemis library type: %s\n' "$1" >&2; exit 2 ;;
esac

case "$(uname -s)" in
  Darwin) actual="$(nm -gU "$2" | awk '{ print $NF }' | sed 's/^_//' | LC_ALL=C sort)" ;;
  Linux) actual="$(nm -D --defined-only "$2" | awk '{ print $NF }' | LC_ALL=C sort)" ;;
  *) printf '%s\n' 'This export check supports macOS and Linux.' >&2; exit 2 ;;
esac

if ! diff -u <(printf '%s\n' "${expected[@]}") <(printf '%s\n' "$actual"); then
  printf 'Unexpected %s exports in %s\n' "$1" "$2" >&2
  exit 1
fi
