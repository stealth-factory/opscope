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
"""Vercel deployments, live.

Shows deployment activity over time, build-duration trend, and the most recent
deployments with their state, project, branch, commit and build time.

    python3 deployments.py [-n SECONDS] [-t TEAM_ID] [project ...]

Keys while running: r refreshes now, f cycles the filter (all / failed /
production), p cycles which project is shown, q quits.

Credentials: reuses the Vercel CLI's own login, so if `vercel whoami` works
this does too. Reads $VERCEL_TOKEN first, then the CLI's auth.json. The token
is never printed. `vercel ls --all --format json` is an equivalent data source
but spawns a Node process per refresh, so this queries the REST API directly.
"""
import collections
import json
import os
import ssl
import sys
import threading
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, bar, cycle, draw, maybe_help, pad, rgb, seg,
                    setup, size, title)

REFRESH = 30            # seconds between API polls (-n)
LIMIT = 100             # deployments per request (API maximum)
AUTH_PATH = "~/.local/share/com.vercel.cli/auth.json"
API = "https://api.vercel.com"

FILTERS = ("all", "failed", "production")

READY = rgb(80, 235, 150)
BUILD = rgb(255, 200, 90)
ERROR = rgb(255, 95, 105)
QUEUE = rgb(120, 160, 220)
CANCEL = rgb(140, 145, 160)
DIM = rgb(80, 95, 115)
GRID = rgb(45, 58, 74)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
PROD = rgb(120, 180, 255)
SHA = rgb(190, 170, 255)
BRANCH = rgb(150, 210, 255)
SPARK = "▁▂▃▄▅▆▇█"

STATE_COLOR = {"READY": READY, "BUILDING": BUILD, "ERROR": ERROR,
               "QUEUED": QUEUE, "INITIALIZING": QUEUE, "CANCELED": CANCEL}
SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"


def token():
    tok = os.environ.get("VERCEL_TOKEN")
    if tok:
        return tok
    try:
        with open(os.path.expanduser(AUTH_PATH)) as f:
            return json.load(f).get("token")
    except (OSError, ValueError):
        return None


def api(path, tok):
    req = urllib.request.Request(API + path,
                                 headers={"Authorization": "Bearer " + tok})
    ctx = ssl.create_default_context()
    with urllib.request.urlopen(req, timeout=25, context=ctx) as r:
        return json.load(r)


def discover_teams(tok):
    try:
        return [t["id"] for t in api("/v2/teams", tok).get("teams", [])]
    except Exception:
        return []


class Store(object):
    """Deployments fetched in the background, so the UI never blocks on HTTP."""

    def __init__(self, teams, projects):
        self.teams = teams
        self.projects = projects
        self.lock = threading.Lock()
        self.deployments = []
        self.error = None
        self.fetched_at = 0
        self.wake = threading.Event()

    def snapshot(self):
        with self.lock:
            return list(self.deployments), self.error, self.fetched_at

    def run(self):
        while True:
            tok = token()
            if not tok:
                with self.lock:
                    self.error = "no credential: run `vercel login` or set VERCEL_TOKEN"
            else:
                out, err = [], None
                scopes = self.teams or [None]
                for team in scopes:
                    q = "/v6/deployments?limit=%d" % LIMIT
                    if team:
                        q += "&teamId=" + team
                    try:
                        out.extend(api(q, tok).get("deployments", []))
                    except urllib.error.HTTPError as e:
                        err = "HTTP %s from Vercel%s" % (
                            e.code, " (token expired? run `vercel login`)"
                            if e.code in (401, 403) else "")
                    except Exception as e:
                        err = "%s: %s" % (type(e).__name__, e)
                if self.projects:
                    out = [d for d in out if d.get("name") in self.projects]
                out.sort(key=lambda d: d.get("created", 0), reverse=True)
                with self.lock:
                    if out or not err:
                        self.deployments = out
                        self.fetched_at = time.time()
                    self.error = err
            self.wake.wait(REFRESH)
            self.wake.clear()


def age(ms):
    s = max(0, time.time() - ms / 1000.0)
    if s < 90:
        return "%ds" % s
    if s < 5400:
        return "%dm" % (s / 60)
    if s < 172800:
        return "%dh" % (s / 3600)
    return "%dd" % (s / 86400)


def dur(seconds):
    if seconds is None:
        return "  --  "
    if seconds < 60:
        return "%5.0fs" % seconds
    return "%dm%02ds" % (seconds // 60, seconds % 60)


def build_seconds(d):
    if d.get("ready") and d.get("buildingAt"):
        return (d["ready"] - d["buildingAt"]) / 1000.0
    if d.get("state") in ("BUILDING", "QUEUED", "INITIALIZING") and d.get("buildingAt"):
        return time.time() - d["buildingAt"] / 1000.0
    return None


def activity(deps, w, hours=48):
    """Deployments per time bucket, coloured by the worst outcome in it."""
    cols = max(10, w - 2)
    now = time.time() * 1000
    span = hours * 3600000.0
    buckets = [[] for _ in range(cols)]
    for d in deps:
        off = now - d.get("created", now)
        if 0 <= off < span:
            buckets[cols - 1 - int(off / span * cols)].append(d)
    peak = max((len(b) for b in buckets), default=0)
    if not peak:
        return [DIM + " no deployments in the last %dh" % hours], 0
    out, last = [" "], None
    for b in buckets:
        if not b:
            if last != GRID:
                out.append(GRID)
                last = GRID
            out.append("·")
            continue
        states = {x.get("state") for x in b}
        col = (ERROR if "ERROR" in states else
               BUILD if states & {"BUILDING", "QUEUED", "INITIALIZING"} else READY)
        if col != last:
            out.append(col)
            last = col
        out.append(SPARK[min(7, int((len(b) / float(peak)) * 7.99))])
    return ["".join(out)], peak


def main():
    maybe_help(__doc__)
    global REFRESH
    args = sys.argv[1:]
    teams = []
    while args and args[0] in ("-n", "--refresh", "-t", "--team"):
        if args[0] in ("-n", "--refresh"):
            REFRESH = max(5.0, float(args[1]))
        else:
            teams.append(args[1])
        args = args[2:]
    projects = set(args)

    setup()
    keyboard = Keyboard()
    tok = token()
    if tok and not teams:
        teams = discover_teams(tok)
    store = Store(teams, projects)
    th = threading.Thread(target=store.run)
    th.daemon = True
    th.start()

    flt = "all"
    only = None          # project cycled with `p`
    tick = 0
    while True:
        tick += 1
        for key in keyboard.poll():
            if key in ("q", "Q"):
                keyboard.restore()
                raise SystemExit(0)
            if key == "r":
                store.wake.set()
            elif key == "f":
                flt = cycle(FILTERS, flt)
            elif key == "p":
                deps, _, _ = store.snapshot()
                names = sorted({d.get("name") for d in deps if d.get("name")})
                options = [None] + names
                only = options[(options.index(only) + 1) % len(options)] \
                    if only in options else (names[0] if names else None)

        w, h = size()
        deps, err, fetched = store.snapshot()
        shown = deps
        if only:
            shown = [d for d in shown if d.get("name") == only]
        if flt == "failed":
            shown = [d for d in shown if d.get("state") in ("ERROR", "CANCELED")]
        elif flt == "production":
            shown = [d for d in shown if d.get("target") == "production"]

        states = collections.Counter(d.get("state") for d in deps)
        projects_seen = len({d.get("name") for d in deps})
        rows = [title("vercel deployments", w, PROD)]

        live = sum(states[s] for s in ("BUILDING", "QUEUED", "INITIALIZING"))
        head = [(DIM, " %d deploys" % len(deps)),
                (DIM, " · %d proj" % projects_seen),
                (READY, "  %d ready" % states.get("READY", 0))]
        if states.get("ERROR"):
            head.append((ERROR, "  %d error" % states["ERROR"]))
        if live:
            head.append((BUILD, "  %s %d building" % (SPINNER[tick % len(SPINNER)], live)))
        head.append((DIM, "   %s ago" % (age(fetched * 1000) if fetched else "--")))
        rows.append(seg(head, w - 1))
        if err:
            rows.append(seg([(ERROR, " ! " + err)], w - 1))
        filt_bits = []
        if flt != "all":
            filt_bits.append(flt)
        if only:
            filt_bits.append(only)
        rows.append(seg([(GRID, " [r]efresh [f]ilter [p]roject [q]uit"),
                         (BUILD, ("   filter: " + " + ".join(filt_bits))
                          if filt_bits else "")], w - 1))
        rows.append("")

        # --- deployments over time ---
        rows.append(LBL + " ── ACTIVITY ── " + DIM + "deploys/hour, last 48h")
        act, peak = activity(deps, w)
        rows.extend(act)
        if peak:
            rows.append(seg([(DIM, " 48h ago"),
                             (DIM, " " * max(1, w - 22)),
                             (DIM, "peak %d/h" % peak)], w - 1))

        durs = sorted(x for x in (build_seconds(d) for d in deps) if x)
        if durs:
            med = durs[len(durs) // 2]
            p95 = durs[min(len(durs) - 1, int(len(durs) * 0.95))]
            rows.append("")
            rows.append(seg([(LBL, " ── BUILD TIME ── "),
                             (DIM, "median "), (TXT, dur(med)),
                             (DIM, "  p95 "), (TXT, dur(p95)),
                             (DIM, "  max "), (TXT, dur(durs[-1]))], w - 1))
            recent = [build_seconds(d) for d in deps[:max(10, w - 2)]][::-1]
            recent = [x for x in recent if x]
            if recent:
                hi = max(recent)
                spark = "".join(SPARK[min(7, int(x / hi * 7.99))] for x in recent)
                rows.append(" " + READY + spark)
        rows.append("")

        # --- recent deployments ---
        rows.append(LBL + " ── RECENT ──")
        wide = w >= 66
        for d in shown:
            if len(rows) >= h - 1:
                break
            meta = d.get("meta") or {}
            state = d.get("state", "?")
            col = STATE_COLOR.get(state, DIM)
            mark = SPINNER[tick % len(SPINNER)] if state in (
                "BUILDING", "QUEUED", "INITIALIZING") else (
                "●" if state == "READY" else "✖" if state == "ERROR" else "○")
            line = [(col, " %s %-9s" % (mark, state.title())),
                    (TXT, pad(d.get("name", "?"), 16)),
                    (DIM, dur(build_seconds(d))),
                    (DIM, " %4s" % age(d.get("created", 0)))]
            if d.get("target") == "production":
                line.append((PROD, "  PROD"))
            elif wide:
                line.append((DIM, "  prev"))
            if wide:
                line.append((SHA, "  " + (meta.get("githubCommitSha") or "")[:7]))
                line.append((BRANCH, " " + (meta.get("githubCommitRef") or "")[:22]))
            rows.append(seg(line, w - 1))
            if len(rows) < h - 1:
                msg = (meta.get("githubCommitMessage") or "").split("\n")[0]
                rows.append(seg([(GRID, "   " + msg)], w - 1))
        if not shown:
            rows.append(DIM + "   (nothing matches the current filter)")
        draw(rows, w, h)
        time.sleep(0.25)


main()
