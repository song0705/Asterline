#!/usr/bin/env bash
set -euo pipefail

if test "$#" -ne 2; then
  echo "usage: smoke-rpm-package.sh <package.rpm> <expected-architecture>" >&2
  exit 64
fi

package=$1
expected_architecture=$2

for tool in dnf grep rpm; do
  command -v "$tool" >/dev/null
done
if test "$(id -u)" -eq 0; then
  privilege=()
else
  command -v sudo >/dev/null
  privilege=(sudo)
fi
test -f "$package"
package=$(cd "$(dirname "$package")" && pwd)/$(basename "$package")
test "$(rpm -qp --qf '%{NAME}' "$package")" = asterline
test "$(rpm -qp --qf '%{ARCH}' "$package")" = "$expected_architecture"
requirements=$(rpm -qpR "$package")
test -n "$requirements"
if grep -F 'libsqlite3.so' <<< "$requirements" >/dev/null; then
  echo "RPM unexpectedly requires a system SQLite library" >&2
  exit 1
fi
rpm -qpl "$package" | grep -Fx /usr/bin/asterline >/dev/null
rpm -qpl "$package" | grep -Fx /usr/bin/ast >/dev/null
rpm -qpl "$package" | grep -Fx /usr/share/licenses/asterline/LICENSE >/dev/null

"${privilege[@]}" dnf install --assumeyes "$package"
/usr/bin/asterline --help >/dev/null
/usr/bin/ast --help >/dev/null
"${privilege[@]}" dnf remove --assumeyes asterline
! rpm -q asterline >/dev/null 2>&1
test ! -e /usr/bin/asterline
test ! -e /usr/bin/ast
