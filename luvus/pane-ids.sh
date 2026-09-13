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

# Print the pane ids from a luvus-module.toml, and only those.
#
# The module header and [[actions]] also have `id =`. A scanner that took
# every id and then demanded a binary for each would refuse the install
# over `open-menu`, which is a shell script, not a widget. The module id
# is told apart by its dot; an action id is not, so the table is the
# thing that has to be read.
#
# Portable awk: GNU's `match(..., array)` is not on macOS /bin/awk.

set -eu

manifest=${1:?}

awk '
  /^\[\[panes\]\]/ { in_panes=1; next }
  /^\[\[/ { in_panes=0 }
  in_panes && /^id = "/ {
    sub(/^id = "/, "")
    sub(/".*/, "")
    print
  }
' "$manifest"
