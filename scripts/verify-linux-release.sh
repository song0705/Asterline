#!/usr/bin/env bash
set -euo pipefail

target=${1:?usage: verify-linux-release.sh <target> [maximum-glibc]}
maximum_glibc=${2:-2.28}

for name in asterline ast; do
  binary="target/$target/release/$name"
  test -x "$binary"

  if readelf -d "$binary" | grep -Eq 'Shared library: \[libsqlite3\.so'; then
    echo "$binary dynamically links libsqlite3; release builds must use bundled SQLite" >&2
    exit 1
  fi

  if ! readelf -l "$binary" | grep -q '/ld-linux'; then
    echo "$binary is not a GNU/glibc ELF executable" >&2
    exit 1
  fi

  newest_glibc=$(
    readelf --version-info "$binary" \
      | grep -Eo 'GLIBC_[0-9]+(\.[0-9]+)+' \
      | sed 's/^GLIBC_//' \
      | sort -Vu \
      | tail -n 1
  )
  test -n "$newest_glibc"
  if test "$(printf '%s\n%s\n' "$maximum_glibc" "$newest_glibc" | sort -V | tail -n 1)" != "$maximum_glibc"; then
    echo "$binary requires glibc $newest_glibc, newer than the $maximum_glibc release baseline" >&2
    exit 1
  fi

  echo "$binary: bundled SQLite, GNU/glibc <= $maximum_glibc (highest symbol $newest_glibc)"
done
