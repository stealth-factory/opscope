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
"""Vercel deployments, live.

Shows deployment activity over time, build-duration trend, and the most recent
deployments with their state, project, branch, commit and build time.

    python3 deployments.py [-n SECONDS] [-t TEAM_ID] [project ...]

Polls every 15s by default (-n changes it, minimum 5s). One request per team
per poll, so the default is 4 polls/min — modest against the API's limits.

Keys while running: up/down (also PgUp/PgDn, Home/End) move the selection,
c or Enter opens a copy sheet for the selected deployment offering its
dashboard, branch-preview, commit-preview and pull-request URLs, r refreshes
now, f cycles the filter (all / failed / production), p cycles which project
is shown, q quits.

Copying uses OSC 52, so the terminal you are sitting at performs it and the
text reaches your local clipboard even over SSH. If your terminal or
multiplexer blocks OSC 52, the sheet still shows each URL in full for mouse
selection.

Credentials: `deployments.token` in config.json, or $VERCEL_TOKEN. Create one
at Account Settings -> Tokens. The Vercel CLI's own session is deliberately not
used - it expires within hours and only the CLI can refresh it, so anything
reading it goes dark overnight. The token is read locally and never printed. `vercel ls --all --format json` is an equivalent data source
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
from common import (RST, Keyboard, bar, bg, clipboard, config_paths, cycle,
                    draw, load_config, maybe_help, pad, rgb, seg, setup, size,
                    title)

_CFG = load_config("deployments", {
    "token": "",             # a Vercel token; keep it in config.json, not here
    "token_env": "VERCEL_TOKEN",
    "refresh": 15,       # seconds between API polls (-n)
    "limit": 100,        # deployments per request (API maximum)
    "teams": [],         # empty = discover every team you can see
    "projects": [],      # empty = all projects
})

REFRESH = float(_CFG["refresh"])
LIMIT = int(_CFG["limit"])
API = "https://api.vercel.com"

FILTERS = ("all", "failed", "production")

READY = rgb(80, 235, 150)
BUILD = rgb(255, 200, 90)
ERROR = rgb(255, 95, 105)
QUEUE = rgb(120, 160, 220)
CANCEL = rgb(140, 145, 160)
# Contrast is measured against both the terminal background and the selected
# row's tint, which is the harder case. Body text clears WCAG AA (4.5:1) on
# both; GRID is decorative gridline dots only and is never used for text.
DIM = rgb(127, 147, 172)      # secondary text: ages, labels   (6.7:1 / 4.5:1)
GRID = rgb(71, 91, 116)       # chart gridlines only, never text (3.0:1)
MSG = rgb(158, 174, 196)      # commit subjects                (9.3:1 / 6.3:1)
URL = rgb(130, 200, 255)      # links                         (11.7:1 / 7.9:1)
HINT = rgb(126, 148, 173)     # key hints                      (6.7:1 / 4.5:1)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
PROD = rgb(120, 180, 255)
SHA = rgb(190, 170, 255)
BRANCH = rgb(150, 210, 255)
SPARK = "▁▂▃▄▅▆▇█"

STATE_COLOR = {"READY": READY, "BUILDING": BUILD, "ERROR": ERROR,
               "QUEUED": QUEUE, "INITIALIZING": QUEUE, "CANCELED": CANCEL}
SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"


def config_token_warning():
    """Warn when a token sits in a file others can read."""
    for path in config_paths():
        if not path or not os.path.exists(path):
            continue
        try:
            mode = os.stat(path).st_mode & 0o077
        except OSError:
            return None
        if mode:
            return "config.json is readable by others; chmod 600 it"
        return None
    return None


def token():
    """A Vercel token, from config.json or the environment.

    Deliberately not the Vercel CLI's session: that expires within hours and
    only the CLI can refresh it, so a panel reading it goes dark overnight.
    Create one at Account Settings -> Tokens instead. Returns (token, source).
    """
    if _CFG["token"]:
        return _CFG["token"], "config"
    tok = os.environ.get(_CFG["token_env"] or "VERCEL_TOKEN")
    if tok:
        return tok, "env"
    return None, "missing"


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
            tok, source = token()
            if not tok:
                with self.lock:
                    self.error = ("no token: set deployments.token in "
                                  "config.json, or $%s"
                                  % (_CFG["token_env"] or "VERCEL_TOKEN"))
            else:
                out, err = [], None
                if source == "config":
                    err = config_token_warning()
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


def links(d):
    """The four URLs worth copying, as (label, url) pairs."""
    meta = d.get("meta") or {}
    out = []
    if d.get("inspectorUrl"):
        out.append(("Deployment dashboard", d["inspectorUrl"]))
    if meta.get("branchAlias"):
        out.append(("Branch preview", "https://" + meta["branchAlias"]))
    if d.get("url"):
        out.append(("Commit preview", "https://" + d["url"]))
    if meta.get("githubPrId") and meta.get("githubOrg") and meta.get("githubRepo"):
        out.append(("Pull request", "https://github.com/%s/%s/pull/%s" % (
            meta["githubOrg"], meta["githubRepo"], meta["githubPrId"])))
    return out


def wrap(text, width):
    return [text[i:i + width] for i in range(0, len(text), width)] or [""]


def copy_overlay(d, rows, w, h, note):
    """Full-panel copy sheet for the selected deployment."""
    meta = d.get("meta") or {}
    rows.append(title("copy links", w, SHA))
    rows.append("")
    rows.append(seg([(TXT, " " + (d.get("name") or "?")),
                     (DIM, "  " + (meta.get("githubCommitSha") or "")[:7]),
                     (BRANCH, "  " + (meta.get("githubCommitRef") or ""))], w - 1))
    rows.append(seg([(MSG, " " + (meta.get("githubCommitMessage") or "")
                      .split("\n")[0])], w - 1))
    rows.append("")
    pairs = links(d)
    for i, (label, url) in enumerate(pairs, 1):
        rows.append(seg([(READY, " [%d] " % i), (TXT, label)], w - 1))
        for line in wrap(url, max(10, w - 6)):
            rows.append(URL + "     " + line)
        rows.append("")
    if not pairs:
        rows.append(DIM + "  (this deployment exposes no links)")
    missing = 4 - len(pairs)
    if missing:
        rows.append(DIM + "  (%d link%s unavailable for this deployment)" %
                    (missing, "" if missing == 1 else "s"))
    while len(rows) < h - 2:
        rows.append("")
    rows.append(seg([(DIM, " press 1-%d to copy · esc or c to close" % max(1, len(pairs)))],
                    w - 1))
    rows.append(seg([(READY, " " + note) if note else (DIM, "")], w - 1))
    return rows


def columns(w):
    """Progressive disclosure: spend extra width on more content, not padding.

    Under 66 columns only the essentials fit. Above that the commit SHA and
    branch appear. From 110 the metadata and commit subject share one line, so
    twice as many deployments are visible in the same height.
    """
    return {
        "detail": w >= 66,
        "single": w >= 110,
        "project": 12 if w < 80 else (16 if w < 110 else 20),
        "branch": max(12, min(34, w // 5)),
    }


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
    teams = list(_CFG["teams"])
    while args and args[0] in ("-n", "--refresh", "-t", "--team"):
        if args[0] in ("-n", "--refresh"):
            REFRESH = max(5.0, float(args[1]))
        else:
            teams.append(args[1])
        args = args[2:]
    projects = set(args) or set(_CFG["projects"])

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
    selected = 0         # index into the filtered list
    scroll = 0           # first visible row
    overlay = False      # copy sheet open
    note = ""            # transient confirmation
    note_until = 0
    visible = 1
    shown = []
    while True:
        tick += 1
        for key in keyboard.poll():
            if overlay:
                if key in ("esc", "c", "q", "Q", "enter"):
                    overlay = False
                elif key.isdigit() and shown:
                    pairs = links(shown[min(selected, len(shown) - 1)])
                    idx = int(key) - 1
                    if 0 <= idx < len(pairs):
                        label, url = pairs[idx]
                        note = ("✓ copied %s" % label.lower()) if clipboard(url) \
                            else "! no clipboard; select the text with the mouse"
                        note_until = time.time() + 3
                continue
            if key in ("q", "Q"):
                keyboard.restore()
                raise SystemExit(0)
            if key == "r":
                store.wake.set()
            elif key == "f":
                flt = cycle(FILTERS, flt)
                selected = 0
            elif key == "up":
                selected = max(0, selected - 1)
            elif key == "down":
                selected += 1
            elif key == "pgup":
                selected = max(0, selected - visible)
            elif key == "pgdn":
                selected += visible
            elif key == "home":
                selected = 0
            elif key == "end":
                selected = max(0, len(shown) - 1)
            elif key in ("c", "enter"):
                if shown:
                    overlay = True
                    note = ""
            elif key == "p":
                deps, _, _ = store.snapshot()
                names = sorted({d.get("name") for d in deps if d.get("name")})
                options = [None] + names
                only = options[(options.index(only) + 1) % len(options)] \
                    if only in options else (names[0] if names else None)
                selected = 0

        w, h = size()
        deps, err, fetched = store.snapshot()
        if note and time.time() > note_until:
            note = ""
        shown = deps
        if only:
            shown = [d for d in shown if d.get("name") == only]
        if flt == "failed":
            shown = [d for d in shown if d.get("state") in ("ERROR", "CANCELED")]
        elif flt == "production":
            shown = [d for d in shown if d.get("target") == "production"]

        selected = max(0, min(selected, len(shown) - 1)) if shown else 0
        if overlay and shown:
            draw(copy_overlay(shown[selected], [], w, h, note), w, h)
            time.sleep(0.1)
            continue

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
        rows.append(seg([(HINT, " ↑↓ select · [c]opy · [r]efresh [f]ilter [p]roject [q]uit"),
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
        rows.append(seg([(LBL, " ── RECENT ── "),
                         (DIM, "%d of %d" % (selected + 1, len(shown)) if shown else "")],
                        w - 1))
        cols = columns(w)
        wide, single = cols["detail"], cols["single"]
        per_item = 1 if single else 2
        visible = max(1, (h - len(rows) - 1) // per_item)
        scroll = min(scroll, max(0, len(shown) - visible))
        if selected < scroll:
            scroll = selected
        elif selected >= scroll + visible:
            scroll = selected - visible + 1
        for i in range(scroll, min(len(shown), scroll + visible)):
            d = shown[i]
            if len(rows) >= h - 1:
                break
            here = (i == selected)
            tint = bg(28, 44, 62) if here else ""
            meta = d.get("meta") or {}
            state = d.get("state", "?")
            col = STATE_COLOR.get(state, DIM)
            mark = SPINNER[tick % len(SPINNER)] if state in (
                "BUILDING", "QUEUED", "INITIALIZING") else (
                "●" if state == "READY" else "✖" if state == "ERROR" else "○")
            msg = (meta.get("githubCommitMessage") or "").split("\n")[0]
            line = [(tint + col, ("▸" if here else " ") + "%s %-9s" % (mark, state.title())),
                    (tint + TXT, pad(d.get("name", "?"), cols["project"])),
                    (tint + DIM, dur(build_seconds(d))),
                    (tint + DIM, " %4s" % age(d.get("created", 0)))]
            if d.get("target") == "production":
                line.append((tint + PROD, "  PROD"))
            elif wide:
                line.append((tint + DIM, "  prev"))
            if wide:
                line.append((tint + SHA, "  " + (meta.get("githubCommitSha") or "")[:7]))
                line.append((tint + BRANCH, " " + pad((meta.get("githubCommitRef") or ""),
                                                      cols["branch"])))
            if single:
                line.append((tint + (TXT if here else MSG), " " + msg))
            if here:
                line.append((tint, " " * w))
            rows.append(seg(line, w - 1))
            if not single and len(rows) < h - 1:
                rows.append(seg([(tint + (TXT if here else MSG), "   " + msg),
                                 (tint, " " * w if here else "")], w - 1))
        if not shown:
            rows.append(DIM + "   (nothing matches the current filter)")
        draw(rows, w, h)
        time.sleep(0.25)


main()
