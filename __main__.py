#!/usr/bin/env python3
# terminal-toys - small dependency-free terminal widgets
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
"""Makes the directory itself runnable: `python3 terminal-toys`.

Python's own convention for an entry point, so the collection can be started
without knowing which file inside it to name. Everything it does is in
start.py; this is the doorbell, not the door.
"""
import os
import runpy
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.argv[0] = os.path.join(HERE, "start.py")
runpy.run_path(sys.argv[0], run_name="__main__")
