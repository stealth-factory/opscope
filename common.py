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
"""Shared drawing and input helpers for the terminal widgets."""
import atexit
import base64
import json
import math
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


CONFIG_NAME = "config.json"


def config_paths():
    """Where settings are looked for, in order of preference."""
    env = os.environ.get("TERMINAL_TOYS_CONFIG")
    xdg = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    here = os.path.dirname(os.path.abspath(__file__))
    return [p for p in (env,
                        os.path.join(xdg, "terminal-toys", CONFIG_NAME),
                        os.path.join(here, CONFIG_NAME)) if p]


def config_token_warning():
    """Warn when a config file holding a token is readable by others.

    Any widget that takes a token writes it into this file, so the check
    belongs beside the loader rather than in whichever widget happened to
    need it first.
    """
    for path in config_paths():
        if not path or not os.path.exists(path):
            continue
        try:
            mode = os.stat(path).st_mode & 0o077
        except OSError:
            return None
        return "config.json is readable by others; chmod 600 it" if mode else None
    return None


def load_config(section, defaults):
    """Settings for `section`, overlaid on `defaults`.

    Keeps personal data — hostnames, targets, city lists — out of the source
    tree and therefore out of a public repository. The first readable file in
    `config_paths()` wins; unknown keys are ignored, and a malformed file falls
    back to the defaults rather than crashing a running panel.
    """
    merged = dict(defaults)
    for path in config_paths():
        try:
            with open(path) as f:
                data = json.load(f)
        except (OSError, ValueError):
            continue
        chunk = data.get(section)
        if isinstance(chunk, dict):
            for key, value in chunk.items():
                if key in merged:
                    merged[key] = value
        return merged
    return merged


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


BIG_DIGITS = {
    "0": ["███", "█ █", "█ █", "█ █", "███"],
    "1": ["  █", "  █", "  █", "  █", "  █"],
    "2": ["███", "  █", "███", "█  ", "███"],
    "3": ["███", "  █", "███", "  █", "███"],
    "4": ["█ █", "█ █", "███", "  █", "  █"],
    "5": ["███", "█  ", "███", "  █", "███"],
    "6": ["███", "█  ", "███", "█ █", "███"],
    "7": ["███", "  █", "  █", "  █", "  █"],
    "8": ["███", "█ █", "███", "█ █", "███"],
    "9": ["███", "█ █", "███", "  █", "███"],
    ":": ["   ", " █ ", "   ", " █ ", "   "],
    "%": ["█ █", "  █", " █ ", "█  ", "█ █"],
    ".": ["   ", "   ", "   ", "   ", " █ "],
    " ": ["   ", "   ", "   ", "   ", "   "],
    "-": ["   ", "   ", "███", "   ", "   "],
}


def big(text, width=None):
    """Render text as five rows of block digits."""
    rows = ["", "", "", "", ""]
    for ch in text:
        glyph = BIG_DIGITS.get(ch, BIG_DIGITS[" "])
        for i in range(5):
            rows[i] += glyph[i] + " "
    return [r[:width] if width else r for r in rows]


# Braille packs 2x4 sub-pixels into one cell, so a chart drawn with it has
# eight times the resolution of block characters. Dot bit per (column, row):
BRAILLE_DOTS = ((0x01, 0x02, 0x04, 0x40), (0x08, 0x10, 0x20, 0x80))


def braille_plot(values, width, height, lo=None, hi=None):
    """A continuous line chart in braille.

    Consecutive samples are joined rather than plotted as isolated dots -
    without that the line reads as scattered specks wherever it moves quickly.
    Returns `height` strings of `width` cells.
    """
    if not values:
        return [""] * height
    lo = min(values) if lo is None else lo
    hi = max(values) if hi is None else hi
    span = (hi - lo) or 1.0
    px_w, px_h = width * 2, height * 4
    cells = [[0] * width for _ in range(height)]

    def row_of(v):
        return max(0, min(px_h - 1, int(round((1 - (v - lo) / span) * (px_h - 1)))))

    prev = None
    for px in range(px_w):
        v = values[min(len(values) - 1, int(px * len(values) / float(px_w)))]
        y = row_of(v)
        span_y = (y, y) if prev is None else (min(prev, y), max(prev, y))
        for py in range(span_y[0], span_y[1] + 1):
            cells[py // 4][px // 2] |= BRAILLE_DOTS[px % 2][py % 4]
        prev = y
    return ["".join(chr(0x2800 + c) for c in row) for row in cells]


def stacked_bar(parts, width):
    """Proportions as one bar: [(fraction, colour), ...] -> coloured segments.

    A bar beats a pie in a character grid - no aliasing, and the eye compares
    lengths far better than angles.
    """
    out, used = [], 0
    for i, (frac, colour) in enumerate(parts):
        n = width - used if i == len(parts) - 1 else int(round(frac * width))
        n = max(0, min(n, width - used))
        if n:
            out.append((colour, "█" * n))
            used += n
    return out


def meter(frac, n, on="█", off="░"):
    """A gauge bar. Rectangular blocks, not parallelograms: ▰▱ are literally
    tilted and read as slanted next to everything else on a character grid."""
    frac = 0.0 if frac < 0 else (1.0 if frac > 1 else frac)
    k = int(round(frac * n))
    return on * k + off * (n - k)


def spread(count, room):
    """Cell widths for `count` bars that fill `room` columns exactly.

    The remainder goes to the leftmost bars rather than being dropped on the
    floor by integer division: stopping short of the right edge leaves no way
    to tell a finished chart from a truncated one. Twenty-eight days across
    fifty-nine columns is two cells each and three columns wasted, which
    reads as a chart that gave up.
    """
    if count <= 0:
        return []
    if count >= room:
        # One cell each and the caller decides what to drop: silently
        # returning fewer widths than bars would lose data without saying so.
        return [1] * count
    slot, extra = divmod(room, count)
    return [slot + (1 if i < extra else 0) for i in range(count)]


def vbars(columns, height, hi=None):
    """Vertical bar chart. `columns` is [(value, colour), ...].

    Each cell resolves an eighth of a row via the partial-block glyphs, so a
    five-row chart has forty levels rather than five. Pass `hi` to fix the
    full-scale value, so two charts can share a scale and stay comparable.
    """
    steps = " ▁▂▃▄▅▆▇█"
    hi = hi or max((v for v, _ in columns), default=0) or 1
    rows = []
    for r in range(height):
        top = hi * (height - r) / float(height)
        bottom = hi * (height - r - 1) / float(height)
        line = []
        for value, colour in columns:
            if value >= top:
                ch = "█"
            elif value <= bottom:
                ch = " "
            else:
                ch = steps[max(1, int((value - bottom) / (top - bottom) * 8))]
            line.append((colour, ch))
        rows.append(line)
    return rows


def vbars_down(columns, height, hi=None):
    """Bar chart hanging downward from a baseline above it.

    Paired with `vbars` and a shared `hi`, this makes a diverging chart: one
    series growing up, another down, one column per day.

    The partial-block glyphs are all bottom-anchored, so a downward bar cannot
    resolve an eighth of a cell the way `vbars` does - only `▀` exists as a
    top-anchored partial. Half a cell is ample once peaks are scaled, and the
    alternative (inverting foreground and background) needs the terminal's
    background painted, which these widgets deliberately never do.
    """
    hi = hi or max((v for v, _ in columns), default=0) or 1
    rows = []
    for r in range(height):
        full = hi * (r + 1) / float(height)     # value that fills this row
        empty = hi * r / float(height)          # value at which it starts
        line = []
        for value, colour in columns:
            if value >= full:
                ch = "█"
            elif value <= empty:
                ch = " "
            else:
                ch = "▀" if (value - empty) / (full - empty) >= 0.5 else " "
            line.append((colour, ch))
        rows.append(line)
    return rows


def dance(width, tick, phase=0.0):
    """Column heights in 0..1 bouncing like a level meter, for pending data.

    Two sine waves of different periods per column, so neighbours move
    together enough to read as one instrument but never march in lockstep.
    Deterministic in `tick`, so every frame is reproducible and no random
    source is needed.
    """
    out = []
    for i in range(width):
        a = math.sin(tick * 0.55 + i * 0.85 + phase)
        b = math.sin(tick * 0.31 + i * 0.41 + phase * 1.7)
        out.append(min(1.0, max(0.08, 0.5 + 0.33 * a + 0.17 * b)))
    return out


def mix(c1, c2, t):
    """Blend two (r, g, b) tuples, for fading a placeholder into real data."""
    t = 0.0 if t < 0 else (1.0 if t > 1 else t)
    return rgb(*[int(round(c1[i] + (c2[i] - c1[i]) * t)) for i in range(3)])


def skeleton(width, tick, span=7):
    """A placeholder bar with a highlight sweeping across it.

    For values that are being refetched: showing the previous number while a
    new one is in flight states something false, and blanking the row makes
    the layout jump. A shimmering grey bar says "pending" without either.
    """
    period = width + span * 2
    centre = (tick % period) - span
    out, last, run = [], None, []
    for i in range(width):
        near = max(0.0, 1.0 - abs(i - centre) / float(span))
        level = int(58 + near * 118)
        colour = rgb(level, level, level + 8)
        if colour != last:
            if run:
                out.append((last, "".join(run)))
            last, run = colour, []
        run.append("█")
    if run:
        out.append((last, "".join(run)))
    return out


def pack_hints(hints, width, sep="  "):
    """Lay key hints across as many lines as they need.

    Each hint is a list of (colour, text) segments and is kept whole: footers
    are the one place a truncated line is actively harmful, since a hint cut to
    "[±]25" teaches the wrong key. Returns the rendered rows, so a caller can
    reserve exactly that many at the bottom.
    """
    rows, parts, used = [], [], 0
    for hint in hints:
        length = sum(len(text) for _, text in hint)
        gap = len(sep) if parts else 0
        if parts and used + gap + length > width:
            rows.append("".join(parts))
            parts, used, gap = [], 0, 0
        if gap:
            parts.append(sep)
        for color, text in hint:
            parts.append(color + text)
        used += gap + length
    if parts:
        rows.append("".join(parts))
    return rows or [""]


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
