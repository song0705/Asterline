#!/usr/bin/env bash
set -euo pipefail

if test "$#" -ne 2; then
  echo "usage: smoke-deb-package.sh <package.deb> <expected-architecture>" >&2
  exit 64
fi

package=$1
expected_architecture=$2

for tool in apt-get dpkg-deb mktemp; do
  command -v "$tool" >/dev/null
done
if test "$(id -u)" -eq 0; then
  privilege=()
else
  command -v sudo >/dev/null
  privilege=(sudo)
fi
test -f "$package"
test "$(dpkg-deb --field "$package" Package)" = asterline
test "$(dpkg-deb --field "$package" Architecture)" = "$expected_architecture"
test -n "$(dpkg-deb --field "$package" Depends)"
dpkg-deb --contents "$package" | grep -F 'usr/bin/asterline' >/dev/null
dpkg-deb --contents "$package" | grep -F 'usr/bin/ast' >/dev/null
dpkg-deb --contents "$package" | grep -F 'usr/share/doc/asterline/LICENSE' >/dev/null

work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT
dpkg-deb --extract "$package" "$work_dir"
"$work_dir/usr/bin/asterline" --help >/dev/null
"$work_dir/usr/bin/ast" --help >/dev/null

case "$package" in
  /*) install_package=$package ;;
  *) install_package="./$package" ;;
esac
"${privilege[@]}" apt-get install --yes "$install_package"
/usr/bin/asterline --help >/dev/null
/usr/bin/ast --help >/dev/null
"${privilege[@]}" apt-get purge --yes asterline
test ! -e /usr/bin/asterline
test ! -e /usr/bin/ast
