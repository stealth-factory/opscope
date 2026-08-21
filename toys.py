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

    python3 toys.py [WIDGET] [ARGS...]

Nothing is described twice. The name and the summary are each widget's own
first docstring line, and what it needs is the Needs column of the README
table - both already maintained, and both already checked by check.py, so a
widget cannot appear here saying something its own file does not.

Where a widget needs a command, this looks for it and says whether it is
there. Where it needs a token, this looks for one in the config and in the
environment. Nothing is hidden for failing either test: a widget you cannot
run yet is still worth knowing about, and it says what is missing instead.

Keys: up/down select, enter launches, r rechecks, q quits.
"""
import ast
import glob
import os
import re
import shutil
import subprocess
import sys
import termios
import tty

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (CLEAR, HIDE, HOME, RST, SHOW, Keyboard, bg, draw, flush,
                    load_config, maybe_help, out, pack_hints, pad, rgb, seg,
                    setup, size, title)

HERE = os.path.dirname(os.path.abspath(__file__))
# Not widgets: the shared library, the checker, and this.
NOT_A_WIDGET = ("common.py", "check.py", "toys.py")

OK = rgb(90, 240, 160)
WARN = rgb(255, 200, 90)
DIM = rgb(127, 147, 172)
GRID = rgb(60, 78, 98)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
ACCENT = rgb(150, 210, 255)

# pr.py deliberately has no token of its own - it reuses github's rather than
# asking for a second one - so its readiness is github's readiness.
TOKEN_SECTION = {"pr": "github"}
BACKTICKED = re.compile(r"`([^`]+)`")


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
        summary = doc.splitlines()[0] if doc else ""
        found.append({"file": name, "stem": name[:-3], "summary": summary})
    return found


def needs_from_readme():
    """What each widget needs, from the table that already records it.

    Kept in the README rather than restated here: check.py already fails a
    widget that has no row in that table, so the column cannot quietly rot,
    and one description of a requirement is better than two that disagree.
    """
    wants = {}
    try:
        text = open(os.path.join(HERE, "README.md")).read()
    except OSError:
        return wants
    for line in text.splitlines():
        if not line.startswith("| **`"):
            continue
        cells = [c.strip() for c in line.split("|")]
        if len(cells) < 5:
            continue
        name = cells[1].strip("* `")
        wants[name] = cells[3]
    return wants


def token_ready(stem):
    """Whether a token-needing widget has one, in config or the environment."""
    section = TOKEN_SECTION.get(stem, stem.replace("-", "_"))
    cfg = load_config(section, {"token": "", "token_env": ""})
    if (cfg.get("token") or "").strip():
        return True
    return bool(os.environ.get(cfg.get("token_env") or "", "").strip())


def readiness(stem, need):
    """(text, colour) for whether this one will work here, and why not.

    A command either exists or does not, which is worth stating plainly. A
    token is softer: it may be somewhere this cannot see, so a missing one
    is reported as something to set rather than as a failure.
    """
    need = (need or "").strip()
    if not need or need in ("—", "-"):
        return "ready", OK
    commands = BACKTICKED.findall(need)
    if commands:
        missing = [c for c in commands if not shutil.which(c)]
        if missing:
            return "needs " + ", ".join(missing), WARN
        return "ready", OK
    if re.search(r"token|key|login", need, re.I):
        if stem == "usage":
            # Not one credential but whichever agents happen to be signed in
            # on this machine, which the widget itself is the thing that
            # knows. Claiming either way from out here would be a guess.
            return "reads what is logged in", DIM
        return ("ready" if token_ready(stem) else "set " + need,
                OK if token_ready(stem) else WARN)
    return need, DIM


def rows_for(items, w, selected):
    name_w = max(12, min(18, w - 58))
    note_w = max(10, min(26, (w - 1) - name_w - 6 - 34))
    # Every column keeps a space of its own, so a description that fills its
    # width stops short of the state beside it rather than running into it.
    text_w = max(8, (w - 1) - name_w - note_w - 6)
    out_rows = []
    for i, item in enumerate(items):
        here = i == selected
        tint = bg(28, 44, 62) if here else ""
        state, hue = item["state"]
        out_rows.append(seg([
            (tint + (ACCENT if here else DIM), " ▸ " if here else "   "),
            (tint + (TXT if here else LBL),
             pad(item["stem"][:name_w - 1], name_w)),
            (tint + DIM, pad(item["summary"][:text_w - 1], text_w)),
            (tint + hue, pad(state[:note_w - 1], note_w)),
            (tint, " " * w if here else ""),
        ], w - 1))
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
    wants = needs_from_readme()
    items = widgets()
    for item in items:
        item["need"] = wants.get(item["file"], "")
        item["state"] = readiness(item["stem"], item["need"])
    return items


def main():
    args = sys.argv[1:]
    items = collect()
    # A widget name is resolved before --help is looked at, so that
    # `toys.py netwatch --help` is netwatch's help and not this one's. Every
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
        ready = sum(1 for i in items if i["state"][0] == "ready")

        body = [title("terminal toys", w, ACCENT)]
        body.append(seg([(DIM, " %d widgets · " % len(items)),
                         (OK, "%d ready here" % ready),
                         (DIM, "   ↵ launches one, q leaves")], w - 1))
        body.append("")
        if not items:
            body.append(seg([(WARN, "  No widgets beside this script.")],
                            w - 1))
        else:
            body.extend(rows_for(items, w, selected))
        body.append("")
        pick = items[selected] if items else None
        if pick and h - len(body) >= 3:
            body.append(seg([(LBL, " ── %s ── " % pick["stem"].upper()),
                             (DIM, "python3 %s" % pick["file"])], w - 1))
            if pick["need"] and pick["need"] not in ("—", "-"):
                body.append(seg([(DIM, "  needs "), (TXT, pick["need"])],
                                w - 1))

        while len(body) < h - 2:
            body.append("")
        hints = [[(ACCENT, "↑↓"), (DIM, " select")],
                 [(ACCENT, "↵"), (DIM, " launch")],
                 [(DIM, "[r]echeck")], [(DIM, "[q]uit")]]
        for line in pack_hints(hints, w - 2):
            body.append(" " + line)
        draw(body, w, h)
        import time
        time.sleep(0.15)


main()
