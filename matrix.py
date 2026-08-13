#!/usr/bin/env python3
# terminal-toys - small dependency-free terminal tools
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
"""Digital rain.

Falling glyphs with truecolor fade trails: near-white head, bright green
shoulder, and a smooth decay over each drop's length. Glyphs mutate in place
independently of the drops, and the field reflows on terminal resize.

    python3 matrix.py
"""
import os
import random
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import RST, draw, maybe_help, rgb, setup, size

GLYPHS = "ｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜﾝ0123456789ABCDEF<>*+=$#%&@"

HEAD = rgb(210, 255, 225)
NEAR = rgb(120, 255, 170)


def shade(level):
    # level 0..1 (1 = closest to head)
    g = int(60 + 175 * level)
    return rgb(int(10 + 20 * level), g, int(30 + 50 * level))


def new_drop(h):
    return [-random.uniform(0, h * 1.5), random.uniform(0.25, 1.15),
            random.randint(max(4, h // 5), max(6, h))]


def main():
    maybe_help(__doc__)
    setup()
    w, h = size()
    chars = [[random.choice(GLYPHS) for _ in range(w)] for _ in range(h)]
    drops = [new_drop(h) for _ in range(w)]
    while True:
        nw, nh = size()
        if (nw, nh) != (w, h):
            w, h = nw, nh
            chars = [[random.choice(GLYPHS) for _ in range(w)] for _ in range(h)]
            drops = [new_drop(h) for _ in range(w)]
        level = [[0.0] * w for _ in range(h)]
        for x, d in enumerate(drops):
            y, speed, ln = d
            d[0] = y + speed
            if y - ln > h:
                drops[x] = new_drop(h)
                continue
            hy = int(y)
            for i in range(ln):
                yy = hy - i
                if 0 <= yy < h:
                    level[yy][x] = max(level[yy][x], 1.0 - i / float(ln))
        for _ in range(max(6, (w * h) // 90)):
            chars[random.randrange(h)][random.randrange(w)] = random.choice(GLYPHS)

        rows = []
        for y in range(h):
            parts = []
            last = None
            lv = level[y]
            cs = chars[y]
            for x in range(w):
                v = lv[x]
                if v <= 0.02:
                    if last is not None:
                        parts.append(RST)
                        last = None
                    parts.append(" ")
                    continue
                col = HEAD if v > 0.985 else (NEAR if v > 0.9 else shade(v))
                if col != last:
                    parts.append(col)
                    last = col
                parts.append(cs[x])
            rows.append("".join(parts))
        draw(rows, w, h)
        time.sleep(0.07)


main()
