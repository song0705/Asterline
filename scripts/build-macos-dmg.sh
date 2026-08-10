#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "build-macos-dmg.sh must run on macOS" >&2
  exit 1
fi

if [[ $# -lt 3 || $# -gt 4 ]]; then
  echo "usage: $0 <version> <x86_64-bin-dir> <arm64-bin-dir> [output.dmg]" >&2
  exit 2
fi

version=$1
x86_dir=$2
arm_dir=$3
output=${4:-"dist/asterline-${version}-macos-universal.dmg"}
repo_root=$(
  cd "$(dirname "$0")/.."
  pwd
)

case "$version" in
  *[!0-9A-Za-z.+-]* | "")
    echo "invalid package version: $version" >&2
    exit 2
    ;;
esac

for binary in asterline ast; do
  for directory in "$x86_dir" "$arm_dir"; do
    if [[ ! -x "$directory/$binary" ]]; then
      echo "missing executable: $directory/$binary" >&2
      exit 2
    fi
  done
done

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/asterline-macos-package.XXXXXX")
cleanup() {
  rm -rf "$work_dir"
}
trap cleanup EXIT

payload="$work_dir/payload"
dmg_root="$work_dir/dmg"
mkdir -p "$payload/usr/local/bin" "$dmg_root" "$(dirname "$output")"

for binary in asterline ast; do
  destination="$payload/usr/local/bin/$binary"
  lipo -create "$x86_dir/$binary" "$arm_dir/$binary" -output "$destination"
  lipo "$destination" -verify_arch x86_64 arm64
  chmod 755 "$destination"
  if [[ -n "${ASTERLINE_MACOS_APPLICATION_IDENTITY:-}" ]]; then
    codesign --force --options runtime --timestamp \
      --sign "$ASTERLINE_MACOS_APPLICATION_IDENTITY" "$destination"
    codesign --verify --strict "$destination"
  else
    codesign --force --sign - "$destination"
  fi
done

pkg_args=(
  --root "$payload"
  --identifier io.github.song0705.asterline
  --version "$version"
  --install-location /
  --ownership recommended
)
if [[ -n "${ASTERLINE_MACOS_INSTALLER_IDENTITY:-}" ]]; then
  pkg_args+=(--sign "$ASTERLINE_MACOS_INSTALLER_IDENTITY")
fi
pkgbuild "${pkg_args[@]}" "$dmg_root/Install Asterline.pkg"

cp "$repo_root/LICENSE" "$dmg_root/LICENSE.txt"
sed "s/@VERSION@/$version/g" \
  "$repo_root/packaging/macos/README.txt" > "$dmg_root/README.txt"

hdiutil create \
  -volname "Asterline $version" \
  -srcfolder "$dmg_root" \
  -format UDZO \
  -ov \
  "$output"

if [[ -n "${ASTERLINE_MACOS_APPLICATION_IDENTITY:-}" ]]; then
  codesign --force --timestamp \
    --sign "$ASTERLINE_MACOS_APPLICATION_IDENTITY" "$output"
  codesign --verify --strict "$output"
fi

hdiutil verify "$output"
echo "created $output"
