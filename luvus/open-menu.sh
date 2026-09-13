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

# The right-click entry point: open the launcher in a pane.
#
# An action is a fire-and-forget subprocess with no terminal of its own, so
# it cannot be the widget - it asks luvus for a pane and lets the
# declaration decide where that lands. Which is why this is three lines
# rather than an exec.
#
# Nothing here names a widget. The launcher is the way in and it lists all
# of them with a preview; somebody who already knows which one they want
# types `luvus module pane open <module-id> <widget>` and skips this.

set -eu

luvus=${LUVUS_BIN_PATH:-luvus}
module=${LUVUS_MODULE_ID:-opscope.widgets}

# `module pane open` honours the placement the manifest declared, so the
# one place that decides how a pane arrives stays the manifest.
exec "$luvus" module pane open "$module" menu
