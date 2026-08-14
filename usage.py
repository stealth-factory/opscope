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
"""How much the coding agents on this machine have been used.

One tab per agent, because they do not agree on what usage even means: one
counts tokens, another counts lines it wrote, and several publish nothing at
all outside their own session. A single table would need a shared schema that
does not exist, so each tab shows that agent's own shape - and an agent that
exposes nothing says so rather than showing a plausible zero.

    python3 usage.py [-n SECONDS]

Everything here is read from local state files. No network, no credentials,
and nothing is inferred from a number that was not published.

Keys: left/right or tab switch agent, r refreshes now, q quits.
"""
import datetime
import json
import os
import shutil
import sqlite3
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, bg, draw, heat, load_config, maybe_help,
                    meter, pack_hints, pad, rgb, seg, setup, size, stacked_bar,
                    title, vbars)

_CFG = load_config("usage", {
    "refresh": 30,
})

REFRESH = float(_CFG["refresh"])

OK = rgb(90, 240, 160)
WARN = rgb(255, 200, 90)
BAD = rgb(255, 100, 110)
DIM = rgb(127, 147, 172)
GRID = rgb(60, 78, 98)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
ACCENT = rgb(150, 210, 255)
AGENT = rgb(180, 160, 255)

CLAUDE_STATS = os.path.expanduser("~/.claude/stats-cache.json")
CURSOR_DB = os.path.expanduser("~/.cursor/ai-tracking/ai-code-tracking.db")


def big_num(n):
    """Token counts run to billions; nobody reads eleven digits."""
    n = float(n or 0)
    for unit, size in (("B", 1e9), ("M", 1e6), ("k", 1e3)):
        if abs(n) >= size:
            return "%.1f%s" % (n / size, unit)
    return "%d" % n


def ago(when):
    if not when:
        return "never"
    s = time.time() - when
    if s < 60:
        return "%ds" % int(s)
    if s < 3600:
        return "%dm" % int(s // 60)
    if s < 86400:
        return "%dh" % int(s // 3600)
    return "%dd" % int(s // 86400)


def read_claude():
    """Claude Code's own stats cache.

    It records what was spent - tokens, messages, sessions - and nothing about
    what is left. There is no limit or reset in the file, so this pane does not
    claim to know one.
    """
    try:
        stat = os.stat(CLAUDE_STATS)
        with open(CLAUDE_STATS) as f:
            d = json.load(f)
    except (OSError, ValueError) as e:
        return {"ok": False, "why": "%s" % type(e).__name__}
    return {"ok": True, "mtime": stat.st_mtime, "data": d}


def read_cursor():
    """Cursor's AI code tracking: how much code it wrote, not what it cost."""
    if not os.path.exists(CURSOR_DB):
        return {"ok": False, "why": "no tracking database"}
    try:
        con = sqlite3.connect("file:%s?mode=ro" % CURSOR_DB, uri=True)
        rows = con.execute(
            "select count(*), count(distinct conversationId),"
            " count(distinct model) from ai_code_hashes").fetchone()
        by_model = con.execute(
            "select model, count(*) from ai_code_hashes"
            " group by model order by 2 desc limit 8").fetchall()
        commits = con.execute(
            "select count(*), sum(linesAdded), sum(humanLinesAdded)"
            " from scored_commits").fetchone()
        recent = con.execute(
            "select max(timestamp) from ai_code_hashes").fetchone()[0]
        con.close()
    except sqlite3.Error as e:
        return {"ok": False, "why": str(e)[:40]}
    return {"ok": True, "hashes": rows[0], "conversations": rows[1],
            "models": rows[2], "by_model": by_model,
            "commits": commits[0] or 0, "lines": commits[1] or 0,
            "human_lines": commits[2] or 0, "last": recent}


# Agents whose usage exists but is not readable from outside their session.
# Naming where the number actually lives beats showing an empty gauge.
ELSEWHERE = {
    "copilot": ("GitHub Copilot",
                ["AI credits are shown live in the session footer, and by",
                 "/usage and /statusline with the `quota` option.",
                 "",
                 "Not reachable from here: there is no CLI subcommand for it,",
                 "and the REST endpoints for a personal plan return 404.",
                 "Organisation-level Copilot metrics do have an API - that",
                 "would be a different widget, about a team rather than you."]),
    "codex": ("OpenAI Codex",
              ["~/.codex holds sessions, history and a logs database, but the",
               "logs are diagnostics - level, target, module - with no usage",
               "counters in them.",
               "",
               "Rate limits are reported inside the running CLI. Nothing on",
               "disk records what is left."]),
    "grok": ("Grok",
             ["~/.grok keeps sessions and config. `grok du` reports disk use,",
              "not quota - the name is a coincidence worth not falling for.",
              "",
              "No usage or limit state was found on disk."]),
}


class Store(object):
    def __init__(self):
        self.lock = threading.Lock()
        self.claude = {}
        self.cursor = {}
        self.installed = {}
        self.fetched = 0
        self.wake = threading.Event()

    def snapshot(self):
        with self.lock:
            return (dict(self.claude), dict(self.cursor),
                    dict(self.installed), self.fetched)

    def run(self):
        while True:
            claude, cursor = read_claude(), read_cursor()
            found = {}
            for name in ("claude", "codex", "copilot", "cursor-agent", "grok"):
                found[name] = bool(shutil.which(name))
            with self.lock:
                self.claude, self.cursor = claude, cursor
                self.installed = found
                self.fetched = time.time()
            self.wake.wait(REFRESH)
            self.wake.clear()


TABS = ("claude", "cursor", "copilot", "codex", "grok")


def tab_bar(active, installed, w):
    # brackets as well as the tint: which tab is open must not depend on a
    # background colour surviving. A dot marks an agent that is installed.
    parts = [(RST, " ")]
    for name in TABS:
        here = name == active
        have = installed.get(name if name != "cursor" else "cursor-agent", False)
        parts.append((bg(38, 56, 76) + ACCENT if here else DIM,
                      ("[%s]" if here else " %s ") % name.upper()))
        parts.append((OK if have else GRID, "·" if have else " "))
    return seg(parts, w - 1)


WEEKDAYS = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")


def token_heatmap(daily, w):
    """Tokens per day, laid out like the contribution calendar in github.py:
    weekdays down the side, weeks across.

    That pane spans a year, so its cells are one character wide and its columns
    go unlabelled - there is no room for fifty-two dates. Four weeks of
    retained history can afford wider cells and a date over each column, which
    is the only difference. Intensity is in the shading glyph as well as the
    colour, so the shape survives a screenshot or a `pane read`.
    """
    levels = " ░▒▓█"
    totals = {}
    for entry in daily:
        try:
            day = datetime.date.fromisoformat(entry["date"])
        except (ValueError, KeyError):
            continue
        totals[day] = sum((entry.get("tokensByModel") or {}).values())
    if not totals:
        return [], 0, None
    peak = max(totals.values()) or 1
    best = max(totals, key=totals.get)
    first = min(totals) - datetime.timedelta(days=min(totals).weekday())
    weeks = []
    week = first
    while week <= max(totals):
        weeks.append(week)
        week += datetime.timedelta(days=7)
    # as many weeks as fit, newest kept
    cell = 6 if w >= 6 * len(weeks) + 10 else 4
    weeks = weeks[-max(1, (w - 8) // cell):]

    rows = [[(DIM, "      ")] + [(DIM, pad(x.strftime("%m-%d"), cell))
                                 for x in weeks]]
    for i, name in enumerate(WEEKDAYS):
        line = [(DIM, " %-5s" % name)]
        for week in weeks:
            day = week + datetime.timedelta(days=i)
            n = totals.get(day)
            if n is None:
                line.append((GRID, pad("  ·", cell)))
                continue
            frac = n / float(peak)
            lvl = 0 if not n else min(4, 1 + int(frac * 3.99))
            line.append((heat(frac), pad("  " + levels[lvl] * 2, cell)))
        rows.append(line)
    return rows, peak, best


def claude_tab(state, w, h):
    rows = []
    if not state.get("ok"):
        return [seg([(BAD, "  no stats cache: %s" % state.get("why"))], w - 1)]
    d = state["data"]
    mu = d.get("modelUsage") or {}
    daily = d.get("dailyActivity") or []

    out_tok = sum(v.get("outputTokens") or 0 for v in mu.values())
    in_tok = sum(v.get("inputTokens") or 0 for v in mu.values())
    cache_r = sum(v.get("cacheReadInputTokens") or 0 for v in mu.values())
    cache_w = sum(v.get("cacheCreationInputTokens") or 0 for v in mu.values())

    rows.append(seg([(LBL, " ── TOTALS ── "),
                     (DIM, "since %s · cache written %s ago"
                      % ((d.get("firstSessionDate") or "")[:10],
                         ago(state.get("mtime"))))], w - 1))
    cells = [("sessions", "%d" % (d.get("totalSessions") or 0), TXT),
             ("messages", "%s" % f"{d.get('totalMessages') or 0:,}", TXT),
             ("output tokens", big_num(out_tok), AGENT),
             ("input tokens", big_num(in_tok), TXT),
             ("cache read", big_num(cache_r), DIM),
             ("cache written", big_num(cache_w), DIM)]
    label_w = max(len(c[0]) for c in cells)
    ncols = 2 if (w - 2) // 2 - label_w - 3 >= 8 else 1
    cw = (w - 2) // ncols
    val_w = max(5, cw - label_w - 3)
    for i in range(0, len(cells), ncols):
        line = [(RST, " ")]
        for label, value, colour in cells[i:i + ncols]:
            line += [(DIM, " " + pad(label, label_w) + " "),
                     (colour, pad(value, val_w))]
        rows.append(seg(line, w - 1))

    # which model did the work
    rows.append("")
    ranked = sorted(mu.items(), key=lambda kv: -(kv[1].get("outputTokens") or 0))
    ranked = [(k, v) for k, v in ranked if (v.get("outputTokens") or 0) > 0]
    rows.append(seg([(LBL, " ── BY MODEL ── "),
                     (DIM, "output tokens")], w - 1))
    if ranked:
        top = ranked[0][1].get("outputTokens") or 1
        for name, v in ranked[:5]:
            tok = v.get("outputTokens") or 0
            rows.append(seg([(TXT, "  " + pad(name.replace("claude-", ""), 20)),
                             (AGENT, "%7s " % big_num(tok)),
                             (heat(tok / float(top)),
                              meter(tok / float(top), max(6, w - 34)))],
                            w - 1))

    # 26 days of activity, straight from the file
    if daily and h > 20:
        rows.append("")
        counts = [x.get("messageCount") or 0 for x in daily]
        peak = max(counts) or 1
        rows.append(seg([(LBL, " ── MESSAGES / DAY ── "),
                         (DIM, "%dd · peak %s" % (len(daily), f"{peak:,}"))],
                        w - 1))
        avail = max(10, w - 3)
        slot = max(1, avail // len(counts))
        cols = []
        for c in counts:
            cols.extend([(c, AGENT)] * slot)
        for line in vbars(cols, 3):
            rows.append(seg([(RST, " ")] + line, w - 1))
        rows.append(seg([(RST, " "), (GRID, "─" * len(cols))], w - 1))
        left = daily[0].get("date", "")[5:]
        right = daily[-1].get("date", "")[5:]
        rows.append(seg([(DIM, " " + left),
                         (DIM, " " * max(1, len(cols) - len(left) - len(right))),
                         (DIM, right)], w - 1))

    # ── tokens per day, as a calendar ───────────────────────────────────
    grid, peak, best = token_heatmap(d.get("dailyModelTokens") or [], w)
    if grid and h > 26:
        rows.append("")
        rows.append(seg([(LBL, " ── TOKENS / DAY ── "),
                         (DIM, "peak "), (AGENT, big_num(peak)),
                         (DIM, " on %s" % (best.strftime("%m-%d") if best
                                           else "--"))], w - 1))
        for line in grid:
            rows.append(seg(line, w - 1))
        rows.append(seg([(DIM, "  less "), (heat(0.05), "░░"),
                         (heat(0.35), "▒▒"), (heat(0.65), "▓▓"),
                         (heat(1.0), "██"), (DIM, " more")], w - 1))

    rows.append("")
    rows.append(seg([(DIM, "  This file records what was spent. It carries no"
                           " limit and no reset,")], w - 1))
    rows.append(seg([(DIM, "  so no percentage of a quota is shown -"
                           " there is none to read.")], w - 1))
    return rows


def cursor_tab(state, w, h):
    if not state.get("ok"):
        return [seg([(BAD, "  %s" % state.get("why"))], w - 1)]
    rows = [seg([(LBL, " ── AI-WRITTEN CODE ── "),
                 (DIM, "last seen %s ago"
                  % ago((state.get("last") or 0) / 1000.0
                        if state.get("last") else None))], w - 1)]
    ai, human = state["lines"], state["human_lines"]
    total = ai + human
    cells = [("tracked edits", f"{state['hashes']:,}", AGENT),
             ("conversations", f"{state['conversations']:,}", TXT),
             ("scored commits", f"{state['commits']:,}", TXT),
             ("lines by agent", f"{ai:,}", AGENT),
             ("lines by hand", f"{human:,}", TXT),
             ("models used", "%d" % state["models"], DIM)]
    label_w = max(len(c[0]) for c in cells)
    ncols = 2 if (w - 2) // 2 - label_w - 3 >= 8 else 1
    cw = (w - 2) // ncols
    val_w = max(5, cw - label_w - 3)
    for i in range(0, len(cells), ncols):
        line = [(RST, " ")]
        for label, value, colour in cells[i:i + ncols]:
            line += [(DIM, " " + pad(label, label_w) + " "),
                     (colour, pad(value, val_w))]
        rows.append(seg(line, w - 1))
    if total:
        rows.append("")
        rows.append(seg([(LBL, " ── WHO WROTE IT ── "),
                         (DIM, "%s lines scored" % f"{total:,}")], w - 1))
        rows.append(seg([(RST, " ")] + stacked_bar(
            [(ai / float(total), AGENT), (human / float(total), DIM)],
            max(10, w - 3)), w - 1))
        rows.append(seg([(AGENT, " ▇ agent %s (%.0f%%)"
                         % (f"{ai:,}", 100.0 * ai / total)),
                         (DIM, "   ▇ hand %s (%.0f%%)"
                          % (f"{human:,}", 100.0 * human / total))], w - 1))
    if state["by_model"]:
        rows.append("")
        rows.append(seg([(LBL, " ── BY MODEL ── "), (DIM, "tracked edits")],
                        w - 1))
        top = state["by_model"][0][1] or 1
        for name, n in state["by_model"][:5]:
            rows.append(seg([(TXT, "  " + pad(str(name or "?"), 22)),
                             (AGENT, "%7s " % f"{n:,}"),
                             (heat(n / float(top)),
                              meter(n / float(top), max(6, w - 36)))], w - 1))
    rows.append("")
    rows.append(seg([(DIM, "  Authorship, not spend: this is how much code the"
                           " agent wrote,")], w - 1))
    rows.append(seg([(DIM, "  which is a different question from what it"
                           " cost.")], w - 1))
    return rows


def elsewhere_tab(name, installed, w, h):
    label, lines = ELSEWHERE[name]
    have = installed.get(name if name != "cursor" else "cursor-agent")
    rows = [seg([(LBL, " ── %s ── " % label.upper()),
                 (OK if have else DIM,
                  "installed" if have else "not installed")], w - 1), ""]
    for line in lines:
        rows.append(seg([(DIM if line else RST, "  " + line)], w - 1))
    rows.append("")
    rows.append(seg([(WARN, "  Nothing is shown for it because nothing is"
                            " published.")], w - 1))
    rows.append(seg([(DIM, "  A plausible-looking zero would be worse than an"
                           " empty tab.")], w - 1))
    return rows


def main():
    maybe_help(__doc__)
    args = sys.argv[1:]
    while args and args[0] in ("-n", "--refresh"):
        global REFRESH
        REFRESH = float(args[1])
        args = args[2:]
    store = Store()
    threading.Thread(target=store.run, daemon=True).start()
    setup()
    keyboard = Keyboard()
    active = 0

    while True:
        for key in keyboard.poll():
            if key in ("q", "Q"):
                raise SystemExit(0)
            if key in ("right", "tab", "l"):
                active = (active + 1) % len(TABS)
            elif key in ("left", "h"):
                active = (active - 1) % len(TABS)
            elif key == "r":
                store.wake.set()

        w, h = size()
        claude, cursor, installed, fetched = store.snapshot()
        rows = [title("agent usage", w, AGENT)]
        rows.append(seg([(DIM, " local state only · read %s ago" % ago(fetched)),
                         (DIM, "   · = installed")], w - 1))
        rows.append(tab_bar(TABS[active], installed, w))
        rows.append("")

        name = TABS[active]
        if name == "claude":
            rows += claude_tab(claude, w, h)
        elif name == "cursor":
            rows += cursor_tab(cursor, w, h)
        else:
            rows += elsewhere_tab(name, installed, w, h)

        hints = [[(ACCENT, "←→"), (DIM, " agent")], [(DIM, "[r]efresh")],
                 [(DIM, "[q]uit")]]
        footer = [" " + line for line in pack_hints(hints, w - 2)]
        rows = rows[:h - len(footer)]
        while len(rows) < h - len(footer):
            rows.append("")
        rows.extend(footer)
        draw(rows, w, h)
        time.sleep(0.3)


main()
