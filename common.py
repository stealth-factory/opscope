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
"""Shared terminal helpers for the sci-fi panel scripts."""
import atexit
import os
import select
import signal
import sys
import termios
import time
import tty

HIDE = "\x1b[?25l"
SHOW = "\x1b[?25h"
HOME = "\x1b[H"
CLEAR = "\x1b[2J"
EL = "\x1b[K"
RST = "\x1b[0m"


def maybe_help(doc):
    """Print the tool's docstring and exit if -h/--help was passed."""
    if any(a in ("-h", "--help") for a in sys.argv[1:]):
        print((doc or "").strip())
        raise SystemExit(0)


def size():
    try:
        c = os.get_terminal_size()
        return max(8, c.columns), max(4, c.lines)
    except OSError:
        return 80, 24


def rgb(r, g, b):
    return "\x1b[38;2;%d;%d;%dm" % (r, g, b)


def bg(r, g, b):
    return "\x1b[48;2;%d;%d;%dm" % (r, g, b)


def out(s):
    sys.stdout.write(s)


def flush():
    try:
        sys.stdout.flush()
    except BrokenPipeError:
        raise SystemExit(0)


def _bye(*_a):
    out(SHOW + RST + CLEAR + HOME)
    flush()
    raise SystemExit(0)


def setup():
    signal.signal(signal.SIGINT, _bye)
    signal.signal(signal.SIGTERM, _bye)
    out(HIDE + CLEAR + HOME)
    flush()


def draw(rows, w, h):
    """Paint `rows` (list of pre-colored strings) from the top-left."""
    buf = [HOME]
    for i in range(h):
        line = rows[i] if i < len(rows) else ""
        buf.append(line + RST + EL)
        if i != h - 1:
            buf.append("\r\n")
    out("".join(buf))
    flush()


def pad(s, n):
    """Truncate/pad a *plain* (uncolored) string to n cells."""
    if len(s) > n:
        return s[:n]
    return s + " " * (n - len(s))


def bar(frac, n, on="█", off="░"):
    frac = 0.0 if frac < 0 else (1.0 if frac > 1 else frac)
    k = int(round(frac * n))
    return on * k + off * (n - k)


def seg(parts, width):
    """Join (color, text) segments, hard-clipped to `width` printable cells."""
    out = []
    n = 0
    for color, text in parts:
        if n >= width:
            break
        room = width - n
        if len(text) > room:
            text = text[:room]
        out.append(color + text)
        n += len(text)
    return "".join(out)


def heat(frac):
    """Green -> amber -> red gradient."""
    frac = 0.0 if frac < 0 else (1.0 if frac > 1 else frac)
    if frac < 0.5:
        t = frac / 0.5
        return rgb(int(40 + 200 * t), 255, int(120 - 100 * t))
    t = (frac - 0.5) / 0.5
    return rgb(255, int(240 - 200 * t), int(20 + 10 * t))


def title(text, w, color=None, accent="│"):
    color = color or rgb(0, 255, 170)
    t = " " + text.upper() + " "
    left = "╺━"
    fill = "━" * max(0, w - len(t) - len(left) - 1)
    return color + left + RST + rgb(220, 255, 240) + t + RST + color + fill + "╸" + RST


def now():
    return time.strftime("%H:%M:%S")


class Keyboard(object):
    """Non-blocking single-key input, restoring the terminal on exit."""

    def __init__(self):
        self.fd = None
        self.saved = None
        if sys.stdin.isatty():
            try:
                self.fd = sys.stdin.fileno()
                self.saved = termios.tcgetattr(self.fd)
                tty.setcbreak(self.fd)
                atexit.register(self.restore)
            except (termios.error, ValueError):
                self.fd = None

    def restore(self):
        if self.fd is not None and self.saved is not None:
            try:
                termios.tcsetattr(self.fd, termios.TCSADRAIN, self.saved)
            except (termios.error, ValueError):
                pass

    def poll(self):
        keys = []
        if self.fd is None:
            return keys
        while select.select([self.fd], [], [], 0)[0]:
            try:
                ch = os.read(self.fd, 1)
            except OSError:
                break
            if not ch:
                break
            keys.append(ch.decode("utf-8", "replace"))
        return keys


def cycle(seq, current):
    try:
        return seq[(seq.index(current) + 1) % len(seq)]
    except ValueError:
        return seq[0]
