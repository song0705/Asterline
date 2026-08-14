#!/usr/bin/env bash
set -euo pipefail

if test "$#" -ne 5; then
  echo "usage: package-rpm.sh <version> <target> <rpm-arch> <archive.tar.gz> <output-dir>" >&2
  exit 64
fi

version=$1
target=$2
rpm_arch=$3
archive=$4
output_dir=$5

case "$target:$rpm_arch" in
  x86_64-unknown-linux-gnu:x86_64)
    asset_arch=x86_64
    ;;
  aarch64-unknown-linux-gnu:aarch64)
    asset_arch=arm64
    ;;
  *)
    echo "unsupported target/RPM architecture pair: $target / $rpm_arch" >&2
    exit 64
    ;;
esac

for tool in cp grep install mkdir mktemp rpm rpmbuild sed tar; do
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
topdir="$work_dir/rpmbuild"
mkdir -p "$topdir"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

source_name="asterline-$version-$target.tar.gz"
cp "$archive" "$topdir/SOURCES/$source_name"
sed \
  -e "s/@VERSION@/$version/g" \
  -e "s/@TARGET@/$target/g" \
  -e "s/@ARCH@/$rpm_arch/g" \
  packaging/rpm/asterline.spec.in > "$topdir/SPECS/asterline.spec"

rpmbuild \
  --define "_topdir $topdir" \
  --target "$rpm_arch" \
  -bb "$topdir/SPECS/asterline.spec"

built_package="$topdir/RPMS/$rpm_arch/asterline-$version-1.$rpm_arch.rpm"
test -f "$built_package"
test "$(rpm -qp --qf '%{NAME}' "$built_package")" = asterline
test "$(rpm -qp --qf '%{ARCH}' "$built_package")" = "$rpm_arch"
requirements=$(rpm -qpR "$built_package")
test -n "$requirements"
if grep -F 'libsqlite3.so' <<< "$requirements" >/dev/null; then
  echo "RPM unexpectedly requires a system SQLite library" >&2
  exit 1
fi

rpm --checksig --nogpg "$built_package" >/dev/null
package_path="$output_dir/asterline-v${version}-Linux-${asset_arch}.rpm"
cp "$built_package" "$package_path"
printf '%s\n' "$package_path"
