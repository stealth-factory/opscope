#!/bin/sh
# opscope - small dependency-free terminal widgets
# Copyright (C) 2026 William Li
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU Affero General Public License as published
# by the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU Affero General Public License for more details.
#
# You should have received a copy of the GNU Affero General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.

# The module's [[build]] step: fetch this version's binaries for this host,
# check them against the sha256 the release publishes, and leave them in
# bin/. It is the same job npm/postinstall.js does for the npm package, and
# it exists for the same reason - what ships has to carry what it needs, so
# a module that asked you to install opscope first would not be one.
#
# The version comes from the manifest beside this script and never from
# /releases/latest. Luvus pins an installed module to the commit you
# reviewed; a build that fetched whatever release existed at install time
# would quietly untie those two.
#
# Runs with a scrubbed environment - no LUVUS_*, no socket - so nothing
# here reads one.

set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$here"

# The manifest is at the module root, a level above this script - it has to
# be there for `luvus module install owner/repo` to find it at all. The
# binaries stay down here, which is what the pane commands point at.
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' ../luvus-module.toml | head -1)
if [ -z "$version" ]; then
  echo "no version line in the luvus-module.toml above this script" >&2
  exit 1
fi

# `platforms` in the manifest keeps this off the operating systems with no
# release; it says nothing about the architecture, so that is named here.
# A platform that is only a wish is not a case: anything else stops with a
# sentence saying what is actually published.
case "$(uname -s)/$(uname -m)" in
  Linux/x86_64)  target=x86_64-unknown-linux-gnu ;;
  Darwin/arm64)  target=aarch64-apple-darwin ;;
  Darwin/x86_64) target=x86_64-apple-darwin ;;
  *)
    echo "no opscope release for $(uname -s) $(uname -m)" >&2
    echo "published: Linux x86_64, macOS arm64, macOS x86_64" >&2
    exit 1
    ;;
esac

name="opscope-v${version}-${target}.tar.gz"
url="https://github.com/stealth-factory/opscope/releases/download/v${version}/${name}"

fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 2 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    echo "needs curl or wget to fetch $1" >&2
    exit 1
  fi
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT HUP TERM

echo "opscope $version: fetching $target"
# curl says what went wrong; these say what it means. A 404 here is almost
# always a manifest naming a version whose release does not exist - a
# checkout taken from a branch ahead of the last tag, say - and "error 22"
# does not tell anybody that.
if ! fetch "$url" "$work/$name"; then
  echo "no $name at that release" >&2
  echo "$url" >&2
  echo "this module's manifest names v${version}; if that tag has no" >&2
  echo "release yet then there are no binaries to install" >&2
  exit 1
fi
if ! fetch "$url.sha256" "$work/$name.sha256"; then
  echo "$name has no .sha256 published beside it, and an unchecked" >&2
  echo "download is not one this installs" >&2
  exit 1
fi

# The sidecar names the file it covers, so it is checked from the directory
# holding both. shasum is on both platforms; sha256sum is not on macOS.
if ! ( cd "$work" && if command -v shasum >/dev/null 2>&1; then
         shasum -a 256 -c "$name.sha256"
       else
         sha256sum -c "$name.sha256"
       fi ) >/dev/null 2>&1; then
  echo "$name does not match the sha256 published beside it" >&2
  exit 1
fi

tar -xzf "$work/$name" -C "$work"
unpacked="$work/opscope-v${version}-${target}"
if [ ! -d "$unpacked" ]; then
  echo "$name did not contain opscope-v${version}-${target}" >&2
  exit 1
fi

# Only the binaries. The tarball also carries the docs and each widget's
# own README, which are for a person reading the release rather than for a
# pane: every widget compiles its help and its doc page in.
rm -rf bin
mkdir bin
for binary in "$unpacked"/*; do
  [ -f "$binary" ] && [ -x "$binary" ] || continue
  cp "$binary" bin/
done

if [ ! -x bin/opscope ]; then
  echo "the release carried no opscope launcher, so no pane could start" >&2
  exit 1
fi

# Every pane the manifest names, not just the launcher. A checksummed
# tarball can still omit a widget, and an install that accepted that left
# a menu entry that died on open - the same empty-pane reading this repo
# refuses everywhere else. `menu` is the launcher itself.
#
# Only `[[panes]]` ids. The module header and `[[actions]]` also have
# `id =`, and treating those as panes would demand a binary for
# `open-menu`, which is a shell script. The table is the thing that
# tells them apart; a missing-dot rule cannot.
panes=$(/bin/sh "$here/pane-ids.sh" ../luvus-module.toml)
# A manifest that read as having no panes would walk this loop zero times
# and report success, which is the reading this whole check exists to
# refuse. It cannot be empty: the launcher's own entry is always there.
if [ -z "$panes" ]; then
  echo "no pane ids read out of the manifest, so nothing below was checked" >&2
  exit 1
fi
for pane in $panes; do
  case "$pane" in
    *.*|menu) continue ;;
  esac
  if [ ! -x "bin/${pane}" ]; then
    echo "the manifest declares a ${pane} pane and v${version} has no such binary" >&2
    echo "a pane added since that release is not in the tarball, and leaving" >&2
    echo "it as a dead menu entry would look like a widget with nothing in it" >&2
    exit 1
  fi
done

count=$(find bin -type f | wc -l | tr -d ' ')
echo "opscope $version: $count binaries in $here/bin"
