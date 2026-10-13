#!/bin/bash
# Run only in a disposable Linux container as root. Never on a workstation.
set -euo pipefail
package=$(readlink -f "$1")
mode=${2:-supported}
version=$(dpkg-deb -f "$package" Version)
test "$(dpkg-deb -f "$package" Architecture)" = amd64
test "$(dpkg-deb -f "$package" Depends)" = 'libc6 (>= 2.35), libgcc-s1'
apt-get update -qq
if [ "$mode" = old-glibc ]; then
  if apt-get install -y "$package"; then
    echo 'ERROR: package installed below its glibc floor' >&2
    exit 1
  fi
  test ! -e /usr/bin/opscope
  exit 0
fi
before=$(sha256sum /usr/bin/link)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# Minimal distro images may deliberately exclude /usr/share/doc at install.
# Read the manifest from the package itself, not optional installed docs.
dpkg-deb --fsys-tarfile "$package" | tar -xOf - ./usr/share/doc/opscope/binaries > "$work/binaries"
dpkg-deb -R "$package" "$work/old"
sed -i 's/^Version:.*/Version: 0.0.0/' "$work/old/DEBIAN/control"
printf '#!/bin/sh\nexit 0\n' > "$work/old/usr/libexec/opscope/retired-widget"
chmod 755 "$work/old/usr/libexec/opscope/retired-widget"
dpkg-deb --root-owner-group --build "$work/old" "$work/old.deb"
apt-get install -y "$work/old.deb"
test -x /usr/libexec/opscope/retired-widget
mkdir -p /root/.config/opscope
printf '{}\n' > /root/.config/opscope/config.json
apt-get install -y "$package"
test "$(dpkg-query -W -f='${Version}' opscope)" = "$version"
test ! -e /usr/libexec/opscope/retired-widget
test "$(sha256sum /usr/bin/link)" = "$before"
dpkg-query -S /usr/bin/link | grep '^coreutils:'
test "$(cat /root/.config/opscope/config.json)" = '{}'
stamp=$(opscope --version)
case "$stamp" in "opscope $version ("*) ;; *) exit 1;; esac
while read -r binary; do
  test "$(/usr/libexec/opscope/"$binary" --version)" = "$binary ${stamp#opscope }"
done < "$work/binaries"
opscope clocks --help > "$work/help"
grep -qi clocks "$work/help"
dpkg -V opscope
# Record files before removal to detect every leftover, not just known names.
dpkg -L opscope | while read -r file; do [ ! -f "$file" ] || echo "$file"; done > "$work/files"
dpkg -r opscope
while read -r file; do test ! -e "$file"; done < "$work/files"
test ! -e /usr/libexec/opscope
test "$(sha256sum /usr/bin/link)" = "$before"
test "$(cat /root/.config/opscope/config.json)" = '{}'
# Exercise a clean local apt install separately from the upgrade.
apt-get install -y "$package"
opscope --version
dpkg -r opscope
