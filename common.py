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
import base64
import os
import re
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


KEY_SEQUENCES = {
    "\x1b[A": "up", "\x1b[B": "down", "\x1b[C": "right", "\x1b[D": "left",
    "\x1bOA": "up", "\x1bOB": "down", "\x1bOC": "right", "\x1bOD": "left",
    "\x1b[5~": "pgup", "\x1b[6~": "pgdn",
    "\x1b[H": "home", "\x1b[F": "end", "\x1b[1~": "home", "\x1b[4~": "end",
    "\r": "enter", "\n": "enter", "\x7f": "backspace", "\t": "tab",
}
CSI_RE = re.compile(r"\x1b(\[[0-9;]*[A-Za-z~]|O[A-Za-z])")


class Keyboard(object):
    """Non-blocking key input, decoding arrows and navigation sequences.

    Returns names ("up", "pgdn", "enter", "esc") for special keys and the bare
    character otherwise. No-ops when stdin is not a tty, so piped and cron runs
    are unaffected. Restores termios on exit.
    """

    def __init__(self):
        self.fd = None
        self.saved = None
        self.buf = ""
        self._lone_esc = False
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
                chunk = os.read(self.fd, 64)
            except OSError:
                break
            if not chunk:
                break
            self.buf += chunk.decode("utf-8", "replace")

        while self.buf:
            if self.buf[0] == "\x1b":
                match = None
                for seq, name in KEY_SEQUENCES.items():
                    if len(seq) > 1 and self.buf.startswith(seq):
                        if match is None or len(seq) > len(match[0]):
                            match = (seq, name)
                if match:
                    self.buf = self.buf[len(match[0]):]
                    keys.append(match[1])
                    continue
                m = CSI_RE.match(self.buf)
                if m:                       # a sequence we don't map; drop it
                    self.buf = self.buf[m.end():]
                    continue
                if self.buf == "\x1b":
                    # bare ESC, or the start of a sequence still arriving. Only
                    # treat it as Escape once a second poll finds nothing more.
                    if self._lone_esc:
                        self.buf = ""
                        self._lone_esc = False
                        keys.append("esc")
                    else:
                        self._lone_esc = True
                    break
                self.buf = self.buf[1:]     # malformed; discard the ESC
                continue
            ch, self.buf = self.buf[0], self.buf[1:]
            keys.append(KEY_SEQUENCES.get(ch, ch))
        if self.buf != "\x1b":
            self._lone_esc = False
        return keys


def cycle(seq, current):
    try:
        return seq[(seq.index(current) + 1) % len(seq)]
    except ValueError:
        return seq[0]


def clipboard(text):
    """Ask the terminal to put `text` on the system clipboard, via OSC 52.

    The terminal emulator performs the copy, so this reaches the machine you
    are sitting at even when the program runs on a remote host over SSH.
    Multiplexers must be willing to forward it. Returns False when stdout is
    not a terminal, so callers can fall back to showing the text instead.
    """
    if not sys.stdout.isatty():
        return False
    payload = base64.b64encode(text.encode("utf-8")).decode("ascii")
    out("\x1b]52;c;%s\x07" % payload)
    flush()
    return True
