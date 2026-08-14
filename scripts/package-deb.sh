#!/usr/bin/env bash
set -euo pipefail

if test "$#" -ne 5; then
  echo "usage: package-deb.sh <version> <target> <deb-arch> <archive.tar.gz> <output-dir>" >&2
  exit 64
fi

version=$1
target=$2
deb_arch=$3
archive=$4
output_dir=$5

case "$target:$deb_arch" in
  x86_64-unknown-linux-gnu:amd64 | aarch64-unknown-linux-gnu:arm64) ;;
  *)
    echo "unsupported target/DEB architecture pair: $target / $deb_arch" >&2
    exit 64
    ;;
esac

for tool in dpkg-deb dpkg-shlibdeps install mktemp sed tar; do
  command -v "$tool" >/dev/null
done

test -f "$archive"
mkdir -p "$output_dir"

archive_root="asterline-$version-$target"
while IFS= read -r entry; do
  case "$entry" in
    "$archive_root" | "$archive_root"/*) ;;
    *)
      echo "unexpected archive entry: $entry" >&2
      exit 1
      ;;
  esac
done < <(tar -tzf "$archive")

work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT
source_dir="$work_dir/source"
staging_dir="$work_dir/staging"
mkdir -p \
  "$source_dir" \
  "$staging_dir/DEBIAN" \
  "$staging_dir/usr/bin" \
  "$staging_dir/usr/share/doc/asterline"
tar -xzf "$archive" -C "$source_dir" --strip-components=1 --no-same-owner --no-same-permissions

for binary in asterline ast; do
  test -x "$source_dir/$binary"
  install -m 755 "$source_dir/$binary" "$staging_dir/usr/bin/$binary"
done
test -f "$source_dir/LICENSE"
install -m 644 "$source_dir/LICENSE" "$staging_dir/usr/share/doc/asterline/LICENSE"
install -m 644 packaging/deb/copyright "$staging_dir/usr/share/doc/asterline/copyright"

dependencies=$(dpkg-shlibdeps -O \
  -e "$staging_dir/usr/bin/asterline" \
  -e "$staging_dir/usr/bin/ast" \
  | sed -n 's/^shlibs:Depends=//p')
if test -z "$dependencies"; then
  echo "dpkg-shlibdeps did not produce runtime dependencies" >&2
  exit 1
fi

sed \
  -e "s/@VERSION@/$version/g" \
  -e "s/@ARCH@/$deb_arch/g" \
  -e "s/@DEPENDS@/$dependencies/g" \
  packaging/deb/control.in > "$staging_dir/DEBIAN/control"

package_path="$output_dir/asterline_${version}_${deb_arch}.deb"
dpkg-deb --root-owner-group --build "$staging_dir" "$package_path" >/dev/null
dpkg-deb --info "$package_path" >/dev/null
printf '%s\n' "$package_path"
