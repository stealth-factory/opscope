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
"""Coding agents running under Herdr, across every workspace.

On a terminal server hosting many workspaces, agents finish or get stuck in
places you are not currently looking. This lists every agent with the state
Herdr reports, ordered so the ones wanting your attention are at the top:

    blocked   waiting on an approval or a question, right now
    done      finished background work you have not looked at yet
    working   busy
    idle      ready for input
    unknown   an agent is present but Herdr cannot classify it

Each row also carries the workspace, how long the agent has held its current
state, and the real CPU and memory of its process. A duration is prefixed with
≥ when the state was already in place before this tool started, since then it
is only a lower bound.

    python3 agents.py [-n SECONDS]

Keys: r refreshes now, w toggles workspace labels vs pane ids, q quits.
Requires HERDR_ENV; it shells out to the `herdr` CLI.
"""
import collections
import json
import os
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, bg, draw, heat, maybe_help, pad, rgb, seg,
                    setup, size, title)

REFRESH = 4.0           # seconds between herdr polls (-n)

BLOCKED = rgb(255, 105, 115)
DONE = rgb(90, 240, 160)
WORKING = rgb(255, 200, 90)
IDLE = rgb(128, 148, 172)
UNKNOWN = rgb(150, 150, 165)
DIM = rgb(127, 147, 172)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
ACCENT = rgb(150, 210, 255)

# ordering: what needs a human first
RANK = {"blocked": 0, "done": 1, "working": 2, "idle": 3, "unknown": 4}
COLOR = {"blocked": BLOCKED, "done": DONE, "working": WORKING,
         "idle": IDLE, "unknown": UNKNOWN}
MARK = {"blocked": "⚠", "done": "✓", "working": "◐", "idle": "·", "unknown": "?"}
SPINNER = "◐◓◑◒"


def herdr(*args):
    try:
        out = subprocess.run(("herdr",) + args, capture_output=True, text=True,
                             timeout=15)
        return json.loads(out.stdout)["result"]
    except Exception:
        return None


def proc_stats(pid):
    """(cpu_ticks, rss_bytes) for a pid, or None."""
    try:
        with open("/proc/%d/stat" % pid) as f:
            rest = f.read().rpartition(")")[2].split()
        return int(rest[11]) + int(rest[12]), int(rest[21]) * 4096
    except (OSError, IndexError, ValueError):
        return None


class Store(object):
    def __init__(self):
        self.lock = threading.Lock()
        self.agents = []
        self.labels = {}
        self.error = None
        self.wake = threading.Event()
        self.since = {}        # pane_id -> (state, first_seen, exact)
        self.first_poll = True
        self.cpu = {}          # pid -> (ticks, wall) for delta CPU

    def snapshot(self):
        with self.lock:
            return list(self.agents), dict(self.labels), self.error

    def run(self):
        hz = os.sysconf("SC_CLK_TCK")
        while True:
            res = herdr("workspace", "list")
            labels = {}
            if res:
                for w in (res.get("workspaces") or []):
                    labels[w.get("workspace_id")] = w.get("label") or ""
            res = herdr("agent", "list")
            if res is None:
                with self.lock:
                    self.error = "herdr CLI unavailable (is HERDR_ENV set?)"
                self.wake.wait(REFRESH)
                self.wake.clear()
                continue

            now = time.time()
            agents = []
            for a in (res.get("agents") or []):
                pane = a.get("pane_id")
                state = a.get("agent_status") or "unknown"
                was = self.since.get(pane)
                if not was or was[0] != state:
                    # a state already in place when we started is only a lower
                    # bound - we did not see it begin
                    self.since[pane] = (state, now, not self.first_poll)
                a = dict(a)
                a["since"] = now - self.since[pane][1]
                a["exact"] = self.since[pane][2]

                info = herdr("pane", "process-info", "--pane", pane) or {}
                fg = (info.get("process_info") or {}).get("foreground_processes") or []
                a["pid"] = fg[0]["pid"] if fg else None
                a["cpu"] = None
                a["rss"] = None
                if a["pid"]:
                    st = proc_stats(a["pid"])
                    if st:
                        ticks, rss = st
                        a["rss"] = rss
                        prev = self.cpu.get(a["pid"])
                        if prev:
                            dt = now - prev[1]
                            if dt > 0:
                                a["cpu"] = (ticks - prev[0]) / float(hz) / dt * 100.0
                        self.cpu[a["pid"]] = (ticks, now)
                agents.append(a)

            agents.sort(key=lambda x: (RANK.get(x.get("agent_status"), 9),
                                       -x.get("since", 0)))
            with self.lock:
                self.agents, self.labels, self.error = agents, labels, None
            self.first_poll = False
            self.wake.wait(REFRESH)
            self.wake.clear()


def ago(s):
    s = int(max(0, s))
    if s < 60:
        return "%ds" % s
    if s < 3600:
        return "%dm" % (s / 60)
    if s < 86400:
        return "%dh%02dm" % (s / 3600, s % 3600 / 60)
    return "%dd" % (s / 86400)


def mem(b):
    if b is None:
        return "   -- "
    for unit in ("B", "K", "M", "G"):
        if b < 1024:
            return "%4.0f%s" % (b, unit)
        b /= 1024.0
    return "%4.1fT" % b


def main():
    maybe_help(__doc__)
    global REFRESH
    args = sys.argv[1:]
    if args and args[0] in ("-n", "--refresh"):
        REFRESH = max(1.0, float(args[1]))
        args = args[2:]

    setup()
    keyboard = Keyboard()
    store = Store()
    th = threading.Thread(target=store.run)
    th.daemon = True
    th.start()

    show_labels = True
    tick = 0
    while True:
        tick += 1
        for key in keyboard.poll():
            if key in ("q", "Q"):
                keyboard.restore()
                raise SystemExit(0)
            if key == "r":
                store.wake.set()
            elif key == "w":
                show_labels = not show_labels

        w, h = size()
        agents, labels, err = store.snapshot()
        counts = collections.Counter(a.get("agent_status") for a in agents)

        rows = [title("agents", w, ACCENT)]
        summary = [(DIM, " %d agent%s" % (len(agents), "" if len(agents) == 1 else "s")),
                   (DIM, " · %d workspace%s" % (
                       len({a.get("workspace_id") for a in agents}),
                       "" if len({a.get("workspace_id") for a in agents}) == 1 else "s"))]
        for state in ("blocked", "done", "working", "idle"):
            if counts.get(state):
                summary.append((COLOR[state], "   %d %s" % (counts[state], state)))
        rows.append(seg(summary, w - 1))
        if err:
            rows.append(seg([(BLOCKED, " ! " + err)], w - 1))

        wants = counts.get("blocked", 0) + counts.get("done", 0)
        if wants:
            rows.append(seg([(BLOCKED if counts.get("blocked") else DONE,
                              " ▸ %d agent%s waiting for you" %
                              (wants, "" if wants == 1 else "s"))], w - 1))
        else:
            rows.append(seg([(DIM, " nothing waiting on you")], w - 1))
        rows.append("")

        wide = w >= 66
        head = " %-8s %-8s %-6s %-5s" % ("AGENT", "STATE", "FOR", "CPU")
        if wide:
            head += " %-5s %-18s" % ("MEM", "WORKSPACE")
        rows.append(LBL + pad(head, w - 1))

        for a in agents:
            if len(rows) >= h - 1:
                break
            state = a.get("agent_status") or "unknown"
            col = COLOR.get(state, UNKNOWN)
            loud = state in ("blocked", "done")
            tint = bg(46, 26, 30) if state == "blocked" else (
                bg(22, 46, 34) if state == "done" else "")
            mark = SPINNER[tick % 4] if state == "working" else MARK.get(state, "?")
            cpu = a.get("cpu")
            line = [(tint + col, " %s %-6s" % (mark, a.get("agent", "?")[:6])),
                    (tint + col, " %-8s" % state.upper() if loud else " %-8s" % state),
                    (tint + DIM, " %-6s" % (("" if a.get("exact") else "≥")
                                             + ago(a.get("since", 0)))),
                    (tint + (heat(min(1.0, (cpu or 0) / 100.0)) if cpu else DIM),
                     "%4.0f%%" % cpu if cpu is not None else "   -")]
            if wide:
                place = labels.get(a.get("workspace_id")) or a.get("workspace_id", "")
                if not show_labels:
                    place = a.get("pane_id", "")
                line.append((tint + DIM, " " + mem(a.get("rss"))))
                line.append((tint + ACCENT, " " + pad(place, 18)))
            if loud:
                line.append((tint, " " * w))
            rows.append(seg(line, w - 1))
            if len(rows) < h - 1:
                title_text = (a.get("terminal_title_stripped") or "").strip()
                cwd = (a.get("cwd") or "").replace(os.path.expanduser("~/projects/"), "")
                rows.append(seg([(tint + DIM, "   " + cwd + "  "),
                                 (tint + (TXT if loud else DIM), title_text),
                                 (tint, " " * w if loud else "")], w - 1))
        if not agents and not err:
            rows.append(DIM + "   no agents running")
        while len(rows) < h - 1:
            rows.append("")
        rows.append(seg([(DIM, " [r]efresh  [w]orkspace/pane  [q]uit")], w - 1))
        draw(rows, w, h)
        time.sleep(0.25)


main()
