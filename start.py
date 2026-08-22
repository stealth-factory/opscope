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
"""Every widget here, what it does, and whether it will work on this machine.

Thirteen scripts in a directory is a list you have to already know. This is
the front door: pick one and it runs, quit it and you are back here.

    python3 start.py [WIDGET] [ARGS...]

Nothing is described twice. The name and the summary are each widget's own
first docstring line, and the note underneath is the paragraph that follows
it - both already maintained, and already checked by check.py, so a widget
cannot appear here saying something its own file does not.

Nothing is said here about whether a widget will work. A widget that cannot
run says so itself, on its own screen, in its own words - which is where
somebody who has just tried to start it is already looking, and is the only
place that knows what it actually needs.

Keys: up/down select, enter launches, r rechecks, q quits.
"""
import ast
import glob
import os
import subprocess
import sys
import termios
import time
import tty

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (CLEAR, HIDE, HOME, RST, SHOW, Keyboard, bg, draw, flush,
                    maybe_help, out, pack_hints, pad, rgb, seg, setup, size,
                    title)

HERE = os.path.dirname(os.path.abspath(__file__))
# Not widgets: the shared library, the checker, and this.
NOT_A_WIDGET = ("common.py", "check.py", "start.py",
                "__main__.py")

DIM = rgb(127, 147, 172)
GRID = rgb(60, 78, 98)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
ACCENT = rgb(150, 210, 255)



def wrap(text, width):
    """Break a paragraph at spaces, for the note under the list."""
    lines, rest = [], (text or "").strip()
    while rest and len(lines) < 3:
        if len(rest) <= width:
            lines.append(rest)
            break
        cut = rest.rfind(" ", 0, width + 1)
        cut = cut if cut > width // 2 else width
        lines.append(rest[:cut])
        rest = rest[cut:].lstrip()
    return lines


def widgets():
    """Every widget beside this script, with its own description of itself."""
    found = []
    for path in sorted(glob.glob(os.path.join(HERE, "*.py"))):
        name = os.path.basename(path)
        if name in NOT_A_WIDGET:
            continue
        try:
            doc = ast.get_docstring(ast.parse(open(path).read())) or ""
        except (OSError, SyntaxError):
            doc = ""
        lines = doc.splitlines()
        # The first line is the row; the first paragraph under it is the
        # aside, which is where each widget explains why it exists. Only
        # that paragraph: what follows is the usage synopsis and the key
        # list, which are for somebody reading --help, not somebody
        # deciding whether this is the thing they want.
        para = []
        for line in lines[2:]:
            if line.startswith("    "):      # an indented usage block
                break
            if not line.strip():
                if para:
                    break
                continue
            para.append(line.strip())
        about = " ".join(para)
        found.append({"file": name, "stem": name[:-3],
                      "summary": lines[0] if lines else "",
                      "about": about[:400], "sample": sample(name[:-3])})
    return found


def sample(stem):
    """The picture from this widget's doc page, if it has one.

    Every doc opens with a rendering of the widget it describes, kept by
    whoever wrote it and read by whoever is deciding whether to run the
    thing. Using that means no second copy of anything - and, more to the
    point, no widget has to be started to be looked at.
    """
    path = os.path.join(HERE, "docs", "%s.md" % stem)
    try:
        text = open(path).read()
    except OSError:
        return []
    block, inside = [], False
    for line in text.splitlines():
        if line.startswith("```"):
            if inside:
                break
            inside = True
            continue
        if inside:
            block.append(line)
    # Only a block that is actually a picture of the widget: the docs also
    # hold shell snippets and JSON, and a config listing is not a preview.
    return block if block and block[0].startswith("╺━") else []


def rows_for(items, w, selected):
    name_w = max(12, min(18, w - 58))
    # The column exists only if something is in it. With nothing to do on
    # this machine - the common case - it takes no width at all and the
    # descriptions get it instead.
    # Every column keeps a space of its own, so a description that fills its
    # width stops short of whatever is beside it rather than running into it.
    text_w = max(8, (w - 1) - name_w - 6)
    out_rows = []
    for i, item in enumerate(items):
        here = i == selected
        tint = bg(28, 44, 62) if here else ""
        line = [(tint + (ACCENT if here else DIM), " ▸ " if here else "   "),
                (tint + (TXT if here else LBL),
                 pad(item["stem"][:name_w - 1], name_w)),
                (tint + DIM, pad(item["summary"][:text_w - 1], text_w))]
        if here:
            line.append((tint, " " * w))
        out_rows.append(seg(line, w - 1))
    return out_rows


def run_widget(keyboard, item, extra=()):
    """Hand the terminal over, and take it back when the widget exits."""
    keyboard.restore()
    out(SHOW + RST + CLEAR + HOME)
    flush()
    try:
        subprocess.call([sys.executable,
                         os.path.join(HERE, item["file"])] + list(extra))
    except OSError as exc:
        out("%s\r\n" % exc)
        flush()
    # The widget left the terminal however it left it, so take it back
    # rather than assuming: raw mode again, cursor away again, screen clear.
    if keyboard.fd is not None:
        try:
            tty.setcbreak(keyboard.fd)
        except (termios.error, ValueError):
            pass
    out(HIDE + CLEAR + HOME)
    flush()


def collect():
    return widgets()


def main():
    args = sys.argv[1:]
    items = collect()
    # A widget name is resolved before --help is looked at, so that
    # `start.py netwatch --help` is netwatch's help, not this one's. Every
    # argument after the name belongs to the widget, including that one.
    if args and not args[0].startswith("-"):
        # A name, so run it straight away and pass the rest through: the menu
        # is for browsing, not something to sit between you and a widget you
        # already know the name of.
        wanted = args[0][:-3] if args[0].endswith(".py") else args[0]
        match = next((i for i in items if i["stem"] == wanted), None)
        if match is None:
            sys.stderr.write("no widget called %r - try: %s\n"
                             % (args[0], ", ".join(i["stem"] for i in items)))
            raise SystemExit(2)
        os.execv(sys.executable, [sys.executable,
                                  os.path.join(HERE, match["file"])] + args[1:])

    maybe_help(__doc__)
    setup()
    keyboard = Keyboard()
    selected = 0
    while True:
        for key in keyboard.poll():
            if key in ("q", "Q"):
                raise SystemExit(0)
            if key in ("up", "k", "K"):
                selected -= 1
            elif key in ("down", "j", "J"):
                selected += 1
            elif key in ("r", "R"):
                items = collect()
            elif key in ("enter", "right", "i") and items:
                run_widget(keyboard, items[min(selected, len(items) - 1)])
                items = collect()

        w, h = size()
        selected = max(0, min(selected, len(items) - 1)) if items else 0

        body = [title("terminal toys", w, ACCENT)]
        body.append(seg([(DIM, " %d widgets   ↵ starts one, q leaves"
                          % len(items))], w - 1))
        body.append("")
        if not items:
            body.append(seg([(DIM, "  No widgets beside this script.")],
                            w - 1))
        else:
            body.extend(rows_for(items, w, selected))
        body.append("")
        # What the highlighted one is for, in its own words - the rest of
        # its opening paragraph, which the row has no room for. Not the
        # command to run it: that is this screen's job, not the reader's.
        pick = items[selected] if items else None
        if pick and h - len(body) >= 3:
            body.append(seg([(LBL, " ── %s ── " % pick["stem"].upper())],
                            w - 1))
            tall = h - len(body) >= 12
            for line in wrap(pick["about"], w - 4)[:1 if tall else 3]:
                body.append(seg([(DIM, "  " + line)], w - 1))

        # And what it looks like. A picture from the docs rather than the
        # widget itself: starting one to look at it would ping hosts, spend
        # API quota and read the whole agent transcript tree, and browsing a
        # menu should cost nothing at all.
        # Measured against the footer that will actually be drawn, rather
        # than a guess at its height.
        hints = [[(ACCENT, "↑↓"), (DIM, " select")],
                 [(ACCENT, "↵"), (DIM, " launch")],
                 [(DIM, "[r]echeck")], [(DIM, "[q]uit")]]
        foot = [" " + line for line in pack_hints(hints, w - 2)]
        room = h - len(body) - len(foot)
        shown = pick["sample"] if pick else []
        if shown and room >= 6 and w >= 44:
            rule = "─" * max(1, w - 15)
            body.append(seg([(GRID, " ┌── "), (DIM, "example"),
                             (GRID, " " + rule + "┐")], w - 1))
            for line in shown[:room - 1]:
                body.append(seg([(GRID, " │"), (DIM, line[:w - 4])], w - 1))

        while len(body) < h - len(foot):
            body.append("")
        body.extend(foot)
        draw(body[:h], w, h)
        time.sleep(0.15)


main()
