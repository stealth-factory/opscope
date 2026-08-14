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
"""Linear delivery metrics across every team in the workspace.

What is outstanding, what the running cycles look like, and whether issues are
being closed faster than they arrive.

    python3 linear.py [-n SECONDS] [team-key ...]

Team keys are the prefixes on issue identifiers - XFY, SYS and so on. With
none given every team is included, minus anything in `linear.exclude_teams`.

Triage is counted apart from the backlog throughout. An auto-filed intake
queue and a groomed backlog are different populations, and adding them
together produces a number that means nothing.

Credentials: `linear.token` in config.json, or $LINEAR_API_KEY. A personal API
key from Settings - Security & access - Personal API keys. The API is called
directly, so no CLI is required.

Keys: up/down select a team, r refreshes now, w cycles the window
(7/14/30/60/90 days), q quits.
"""
import collections
import datetime
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, bg, config_token_warning, cycle, dance,
                    draw, heat, load_config, maybe_help, meter, mix,
                    pack_hints, pad, rgb, seg, setup, size, skeleton,
                    stacked_bar, title, vbars, vbars_down)

_CFG = load_config("linear", {
    "token": "",
    "token_env": "LINEAR_API_KEY",
    "exclude_teams": [],   # team keys to drop, e.g. an automated intake queue
    "window_days": 14,
    "refresh": 120,        # seconds; the limit is 2500 requests an hour
})

REFRESH = float(_CFG["refresh"])
WINDOWS = (7, 14, 30, 60, 90)
API = "https://api.linear.app/graphql"
PAGE = 250             # Linear's maximum page size
PAGE_CAP = 12          # pages per query, so one huge team cannot spin forever

OK = rgb(90, 240, 160)
WARN = rgb(255, 200, 90)
BAD = rgb(255, 100, 110)
DIM = rgb(127, 147, 172)
GRID = rgb(60, 78, 98)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
ACCENT = rgb(150, 210, 255)
NEW = rgb(180, 160, 255)          # issues arriving
NEW_RGB, OK_RGB = (180, 160, 255), (90, 240, 160)
GHOST = (96, 106, 124)
LOAD_NEW, LOAD_OK = mix(GHOST, NEW_RGB, 0.45), mix(GHOST, OK_RGB, 0.45)
SETTLE_FRAMES = 8
CHURN_DAYS = 6     # tail of a cycle's history that counts as "lately"

# Linear's own vocabulary, in the order work moves through it
STATE_ORDER = ("triage", "backlog", "unstarted", "started")
STATE_LABEL = {"triage": "triage", "backlog": "backlog",
               "unstarted": "todo", "started": "in progress"}
STATE_COLOUR = {"triage": BAD, "backlog": DIM, "unstarted": ACCENT,
                "started": WARN}


def token():
    """A Linear personal API key, from config.json or the environment."""
    if _CFG["token"]:
        return _CFG["token"], "config"
    tok = os.environ.get(_CFG["token_env"] or "LINEAR_API_KEY")
    if tok:
        return tok, "env"
    return None, "missing"


_QUOTA = {"requests": None, "complexity": None}


def graphql(query, tok, variables=None):
    body = json.dumps({"query": query,
                       "variables": variables or {}}).encode()
    req = urllib.request.Request(API, data=body, headers={
        "Authorization": tok,
        "Content-Type": "application/json",
        "User-Agent": "terminal-toys",
    })
    with urllib.request.urlopen(req, timeout=30) as r:
        for key, hdr in (("requests", "X-RateLimit-Requests-Remaining"),
                         ("complexity", "X-RateLimit-Complexity-Remaining")):
            raw = r.headers.get(hdr)
            if raw is not None:
                try:
                    _QUOTA[key] = int(raw)
                except ValueError:
                    pass
        data = json.load(r)
    if data.get("errors"):
        raise ValueError(data["errors"][0].get("message", "")[:80])
    return data["data"]


def pages(tok, query, node_path, variables=None):
    """Follow pageInfo to the end, or to PAGE_CAP, and return every node.

    Linear has no totalCount on connections, so anything counted has to be
    walked. Only the fields actually needed are requested: complexity is a
    tenth of a point per property against a budget of three million an hour,
    so the page count matters and the field count barely does.
    """
    out, cursor, seen = [], None, 0
    for _ in range(PAGE_CAP):
        v = dict(variables or {})
        v["after"] = cursor
        conn = graphql(query, tok, v)
        for step in node_path:
            conn = conn[step]
        out.extend(conn["nodes"])
        info = conn["pageInfo"]
        seen += 1
        if not info.get("hasNextPage"):
            return out, False
        cursor = info.get("endCursor")
    return out, True          # hit the cap: the caller should say so


OPEN_QUERY = """
query($after: String) {
  issues(first: %d, after: $after,
         filter: { state: { type: { nin: ["completed", "canceled",
                                          "duplicate"] } } }) {
    nodes { identifier estimate startedAt createdAt
            state { type } team { key } }
    pageInfo { hasNextPage endCursor }
  }
}""" % PAGE

CREATED_QUERY = """
query($after: String, $since: DateTimeOrDuration!) {
  issues(first: %d, after: $after, filter: { createdAt: { gte: $since } }) {
    nodes { createdAt team { key } }
    pageInfo { hasNextPage endCursor }
  }
}""" % PAGE

DONE_QUERY = """
query($after: String, $since: DateTimeOrDuration!) {
  issues(first: %d, after: $after, filter: { completedAt: { gte: $since } }) {
    nodes { identifier completedAt startedAt createdAt team { key } }
    pageInfo { hasNextPage endCursor }
  }
}""" % PAGE

CYCLES_QUERY = """
{
  cycles(first: 50, filter: { isActive: { eq: true } }) {
    nodes {
      name number startsAt endsAt progress
      issueCountHistory completedIssueCountHistory
      scopeHistory completedScopeHistory
      team { key name }
    }
    pageInfo { hasNextPage endCursor }
  }
}"""

TEAMS_QUERY = """
{ teams(first: 100) { nodes { key name } pageInfo { hasNextPage } } }"""


def day(ts):
    """The calendar day of an ISO timestamp, as Linear returns them."""
    return (ts or "")[:10]


def ago(t):
    if not t:
        return "--"
    s = int(time.time() - t)
    if s < 60:
        return "%ds" % s
    if s < 3600:
        return "%dm" % (s // 60)
    return "%dh" % (s // 3600)


class Store(object):
    def __init__(self, days, keep):
        self.lock = threading.Lock()
        self.days = days
        self.keep = keep          # team keys to include; empty = everything
        self.teams = []
        self.states = collections.Counter()
        self.by_team = {}
        self.cycles = []
        self.created = collections.Counter()
        self.completed = collections.Counter()
        self.lead = []            # created -> completed, in hours
        self.cycle_time = []      # started -> completed, in hours
        # extremes, each as (hours, identifier): a median says the shape of
        # the distribution, these say which issue to go and look at
        self.quickest = self.slowest = None
        self.oldest_open = self.oldest_wip = None
        self.window = None        # which window the counters describe
        self.truncated = False
        self.error = None
        self.fetched = 0
        self.wake = threading.Event()

    def snapshot(self):
        with self.lock:
            return (list(self.teams), collections.Counter(self.states),
                    dict(self.by_team), list(self.cycles),
                    collections.Counter(self.created),
                    collections.Counter(self.completed), list(self.lead),
                    list(self.cycle_time),
                    (self.quickest, self.slowest, self.oldest_open,
                     self.oldest_wip),
                    self.window, self.truncated, self.error, self.fetched)

    def wanted(self, key):
        if self.keep:
            return key in self.keep
        return key not in (_CFG["exclude_teams"] or [])

    def run(self):
        while True:
            tok, source = token()
            if not tok:
                with self.lock:
                    self.error = ("no key: set linear.token in config.json or "
                                  "$%s" % (_CFG["token_env"] or "LINEAR_API_KEY"))
                self.wake.wait(REFRESH)
                self.wake.clear()
                continue
            try:
                self.pass_(tok, source)
            except urllib.error.HTTPError as e:
                with self.lock:
                    self.error = "HTTP %s from Linear%s" % (
                        e.code, " (key rejected?)" if e.code == 401 else "")
            except Exception as e:
                with self.lock:
                    self.error = "%s: %s" % (type(e).__name__, str(e)[:60])
            self.wake.wait(REFRESH)
            self.wake.clear()

    def pass_(self, tok, source):
        with self.lock:
            days_now = self.days
        since = (datetime.datetime.now(datetime.timezone.utc)
                 - datetime.timedelta(days=days_now - 1)).strftime(
                     "%Y-%m-%dT00:00:00.000Z")

        teams = graphql(TEAMS_QUERY, tok)["teams"]["nodes"]
        teams = [t for t in teams if self.wanted(t["key"])]
        keys = set(t["key"] for t in teams)
        with self.lock:
            self.teams = teams

        # what is outstanding right now, at any age
        rows, capped = pages(tok, OPEN_QUERY, ["issues"])
        states = collections.Counter()
        by_team = {}
        now = datetime.datetime.now(datetime.timezone.utc)
        oldest_open = oldest_wip = None
        for it in rows:
            key = (it.get("team") or {}).get("key")
            if key not in keys:
                continue
            st = (it.get("state") or {}).get("type")
            if st not in STATE_ORDER:
                continue
            states[st] += 1
            slot = by_team.setdefault(key, collections.Counter())
            slot[st] += 1
            slot["open"] += 1
            born = parse(it.get("createdAt"))
            if born:
                age = (now - born).total_seconds() / 3600.0
                if oldest_open is None or age > oldest_open[0]:
                    oldest_open = (age, it.get("identifier"))
            began = parse(it.get("startedAt"))
            if st == "started" and began:
                age = (now - began).total_seconds() / 3600.0
                if oldest_wip is None or age > oldest_wip[0]:
                    oldest_wip = (age, it.get("identifier"))

        # the running cycles, each already carrying its own burndown
        cyc = [c for c in graphql(CYCLES_QUERY, tok)["cycles"]["nodes"]
               if (c.get("team") or {}).get("key") in keys]

        # arrivals and departures over the window
        made, cap2 = pages(tok, CREATED_QUERY, ["issues"], {"since": since})
        done, cap3 = pages(tok, DONE_QUERY, ["issues"], {"since": since})
        created, completed = collections.Counter(), collections.Counter()
        lead, ctime = [], []
        quickest = slowest = None
        for it in made:
            if (it.get("team") or {}).get("key") in keys:
                created[day(it["createdAt"])] += 1
        for it in done:
            if (it.get("team") or {}).get("key") not in keys:
                continue
            completed[day(it["completedAt"])] += 1
            fin = parse(it.get("completedAt"))
            if fin and parse(it.get("createdAt")):
                hrs = (fin - parse(it["createdAt"])).total_seconds() / 3600.0
                lead.append(hrs)
                if quickest is None or hrs < quickest[0]:
                    quickest = (hrs, it.get("identifier"))
                if slowest is None or hrs > slowest[0]:
                    slowest = (hrs, it.get("identifier"))
            if fin and parse(it.get("startedAt")):
                ctime.append((fin - parse(it["startedAt"])).total_seconds() / 3600.0)

        for key in by_team:
            by_team[key]["done"] = sum(
                1 for it in done
                if (it.get("team") or {}).get("key") == key)

        with self.lock:
            self.states, self.by_team, self.cycles = states, by_team, cyc
            self.created, self.completed = created, completed
            self.lead, self.cycle_time = lead, ctime
            self.quickest, self.slowest = quickest, slowest
            self.oldest_open, self.oldest_wip = oldest_open, oldest_wip
            self.window = days_now
            self.truncated = capped or cap2 or cap3
            self.fetched = time.time()
            self.error = (config_token_warning() if source == "config" else None)


def parse(ts):
    if not ts:
        return None
    try:
        return datetime.datetime.strptime(ts[:19], "%Y-%m-%dT%H:%M:%S").replace(
            tzinfo=datetime.timezone.utc)
    except ValueError:
        return None


def dur(hours):
    """A span at whatever unit keeps it readable.

    Rolls over to years because these figures reach them: an issue open for
    "1021.6d" is arithmetic, one open for "2.8y" is a decision.
    """
    if hours is None:
        return "--"
    if hours < 1:
        return "%dm" % max(1, int(hours * 60))
    if hours < 48:
        return "%.1fh" % hours
    days = hours / 24.0
    if days < 365:
        return "%.1fd" % days
    return "%.1fy" % (days / 365.0)


def median(xs):
    if not xs:
        return None
    s = sorted(xs)
    n = len(s)
    return s[n // 2] if n % 2 else (s[n // 2 - 1] + s[n // 2]) / 2.0


def main():
    maybe_help(__doc__)
    args = sys.argv[1:]
    while args and args[0] in ("-n", "--refresh"):
        global REFRESH
        REFRESH = float(args[1])
        args = args[2:]
    store = Store(int(_CFG["window_days"]), [a.upper() for a in args])
    threading.Thread(target=store.run, daemon=True).start()
    setup()
    keyboard = Keyboard()
    # Two sections scroll, so the arrows need to know which one they are in.
    # Tab moves the focus; the focused heading says so and carries the range.
    CYCLES, TEAMS = 0, 1
    focus, sel = CYCLES, [0, 0]
    tick = 0
    settle_t, settle_from = 0, None

    while True:
        tick += 1
        for key in keyboard.poll():
            if key in ("q", "Q"):
                raise SystemExit(0)
            if key == "r":
                store.wake.set()
            elif key == "w":
                with store.lock:
                    store.days = cycle(WINDOWS, store.days)
                store.wake.set()
            elif key == "tab":
                focus = TEAMS if focus == CYCLES else CYCLES
            elif key == "up":
                sel[focus] = max(0, sel[focus] - 1)
            elif key == "down":
                sel[focus] += 1

        w, h = size()
        (teams, states, by_team, cycles, created, completed, lead, ctime,
         extremes, window, truncated, err, fetched) = store.snapshot()
        quickest, slowest, oldest_open, oldest_wip = extremes
        stale = window != store.days
        rows = [title("linear ops", w, NEW)]

        head = [(DIM, " %d team%s" % (len(teams), "" if len(teams) == 1 else "s")),
                (DIM, "   updated %s ago" % ago(fetched))]
        if _QUOTA["requests"] is not None:
            left = _QUOTA["requests"]
            head.append((OK if left > 500 else WARN,
                         "   %d req left/hr" % left))
        rows.append(seg(head, w - 1))
        if err:
            rows.append(seg([(BAD, " ! " + err)], w - 1))
        if not teams:
            rows.append(seg([(DIM, " collecting…")], w - 1))
            rows += [""] * max(0, h - len(rows) - 1)
            draw(rows, w, h)
            time.sleep(0.4)
            continue

        # ── how long work takes, across every team ──────────────────────
        # Leads the board: it is the one figure that says whether the machine
        # is getting faster or slower, and it is an aggregate over all teams
        # rather than any one of them - which the heading has to say, or it
        # reads as whichever team happens to be selected below.
        med_lead, med_cycle = median(lead), median(ctime)
        rows.append(seg([(LBL, " ── HOW LONG ── "),
                         (DIM, "all teams · "),
                         (DIM, "counting…" if stale
                          else "median of %d completed in %dd"
                          % (len(lead), store.days))], w - 1))
        # Every figure here goes through one grid. The medians used to be
        # hand-padded and drifted out of step with the extremes beneath them,
        # and the arrow definitions floated after the values instead of
        # attaching to the terms they define.
        def extreme(label, pair, colour):
            if stale:
                return (label, "···", DIM)
            if not pair:
                return (label, "--", DIM)
            hours, ident = pair
            return (label, "%s %s" % (ident or "?", dur(hours)), colour)

        cells = [
            ("lead (created→completed)",
             "···" if stale else dur(med_lead), DIM if stale else TXT),
            ("cycle (started→completed)",
             "···" if stale else dur(med_cycle), DIM if stale else TXT),
            extreme("quickest", quickest, OK),
            extreme("slowest", slowest, WARN),
            extreme("oldest open", oldest_open, BAD),
            extreme("oldest in progress", oldest_wip, WARN),
        ]
        label_w = max(len(x[0]) for x in cells)
        # Two columns only when a value still gets room for the longest thing
        # it holds - an identifier and a duration. Cells are a fixed width so
        # a long value cannot push the next column out of line.
        ncols = 2 if (w - 2) // 2 - label_w - 3 >= 15 else 1
        cw = (w - 2) // ncols
        val_w = max(6, cw - label_w - 3)
        for n in range(0, len(cells), ncols):
            line = [(RST, " ")]
            for label, value, colour in cells[n:n + ncols]:
                line += [(DIM, " " + pad(label, label_w) + " "),
                         (colour, pad(value, val_w))]
            rows.append(seg(line, w - 1))
        rows.append("")

        # ── what is outstanding right now ────────────────────────────────
        total_open = sum(states[s] for s in STATE_ORDER)
        rows.append(seg([(LBL, " ── OPEN ── "),
                         (NEW, "%d" % total_open), (DIM, " issues open"),
                         (DIM, "   (any age)"),
                         (WARN, "   truncated" if truncated else "")], w - 1))
        if total_open:
            parts = [(states[s] / float(total_open), STATE_COLOUR[s])
                     for s in STATE_ORDER if states[s]]
            rows.append(seg([(RST, " ")] + stacked_bar(parts, max(10, w - 3)),
                            w - 1))
            key = [(RST, " ")]
            for s in STATE_ORDER:
                if states[s]:
                    key += [(STATE_COLOUR[s], "▇ "), (TXT, STATE_LABEL[s]),
                            (DIM, " %d (%.0f%%)   "
                             % (states[s], 100.0 * states[s] / total_open))]
            rows.append(seg(key, w - 1))

        # ── the running cycles, each with its own burndown ───────────────
        rows.append("")
        # Busiest first. The burndown arrays already say where the action is:
        # day-over-day movement in completed scope and in scope itself, summed
        # over the tail. A cycle nothing has touched in a week is not
        # interesting however close its deadline, and an empty one scores zero
        # and sinks without needing a special case. Deadline breaks ties.
        def churn(c):
            moved = 0.0
            for series in ("completedScopeHistory", "scopeHistory"):
                tail = (c.get(series) or [])[-CHURN_DAYS:]
                moved += sum(abs(tail[i] - tail[i - 1])
                             for i in range(1, len(tail)))
            ends = parse(c.get("endsAt"))
            left = ((ends - datetime.datetime.now(datetime.timezone.utc)).days
                    if ends else 999)
            return (-moved, left)

        ranked_cycles = sorted(cycles, key=churn)
        if ranked_cycles:
            sel[CYCLES] = max(0, min(sel[CYCLES], len(ranked_cycles) - 1))
        shown = max(2, min(6, (h - len(rows)) // 4))
        cfirst = 0
        if len(ranked_cycles) > shown:
            cfirst = min(max(0, sel[CYCLES] - shown // 2),
                         len(ranked_cycles) - shown)
        here_now = focus == CYCLES
        rows.append(seg([(ACCENT if here_now else LBL, " ── ACTIVE CYCLES ── "),
                         (DIM, "%d running" % len(cycles)),
                         (ACCENT if here_now else DIM,
                          ("   %s%d-%d of %d"
                           % ("↑↓ " if here_now else "",
                              cfirst + 1,
                              min(cfirst + shown, len(ranked_cycles)),
                              len(ranked_cycles)))
                          if len(ranked_cycles) > shown else "")], w - 1))
        if not cycles:
            rows.append(seg([(DIM, "  no cycle is running in any team")], w - 1))
        for ci, c in list(enumerate(ranked_cycles))[cfirst:cfirst + shown]:
            scope = (c.get("scopeHistory") or [0])[-1]
            done = (c.get("completedScopeHistory") or [0])[-1]
            opened_at = (c.get("scopeHistory") or [0])[0]
            ends = parse(c.get("endsAt"))
            left = ((ends - datetime.datetime.now(datetime.timezone.utc)).days
                    if ends else None)
            frac = (done / float(scope)) if scope else 0.0
            name = "%s %s" % ((c.get("team") or {}).get("key", "?"),
                              c.get("name") or ("Cycle %g" % (c.get("number") or 0)))
            on = focus == CYCLES and ci == sel[CYCLES]
            tint = bg(38, 56, 76) if on else ""
            line = [(tint + (ACCENT if on else TXT),
                     ("▸" if on else " ") + pad(name, 18)),
                    (tint + heat(frac), meter(frac, max(8, min(28, w - 54)))),
                    (tint + (heat(frac) if scope else DIM),
                     " %3s" % ("%.0f%%" % (frac * 100) if scope else "--")),
                    (tint + DIM, "  %g/%g pts" % (done, scope) if scope
                     else "  nothing scoped")]
            if left is not None:
                line.append((tint + (WARN if left <= 2 else DIM),
                             "  %dd left" % left))
            # scope added after the cycle opened is the number that explains a
            # cycle that is working hard and still slipping
            if scope > opened_at:
                line.append((tint + BAD, "  +%g added" % (scope - opened_at)))
            if on:
                line.append((tint, " " * w))
            rows.append(seg(line, w - 1))

        # ── arrivals against departures ─────────────────────────────────
        hist_days = store.days
        today = datetime.date.today()
        days = [(today - datetime.timedelta(days=n)).isoformat()
                for n in range(hist_days - 1, -1, -1)]
        avail = max(10, w - 3)
        if len(days) > avail:
            days = days[-avail:]
        slot = max(1, avail // len(days))
        gap = 1 if slot >= 3 else 0
        barw = slot - gap

        def spread(per_day):
            cols = []
            for n, v in enumerate(per_day):
                cols.extend([v] * barw)
                if gap and n < len(per_day) - 1:
                    cols.extend([0] * gap)
            return cols

        made_day = [created.get(d, 0) for d in days]
        done_day = [completed.get(d, 0) for d in days]
        up, down = spread(made_day), spread(done_day)
        chart_cols = len(up)
        span_hi = max(up + down) or 1
        span = ("%dd of %dd" % (len(days), hist_days)
                if len(days) < hist_days else "%dd" % len(days))
        rows.append("")
        if stale:
            rows.append(seg([(LBL, " ── ISSUE FLOW ── "),
                             (DIM, "counting %dd…" % hist_days)], w - 1))
        else:
            rows.append(seg([(LBL, " ── ISSUE FLOW ── "), (DIM, "%s · " % span),
                             (NEW, "▲ %d created" % sum(made_day)),
                             (DIM, " · "),
                             (OK, "▼ %d completed" % sum(done_day)),
                             (DIM, "   peak %d/day" % span_hi)], w - 1))
        if stale:
            hu = spread(dance(len(days), tick))
            hd = spread(dance(len(days), tick, phase=2.1))
            cu, cd = LOAD_NEW, LOAD_OK
            settle_from, settle_t = (hu, hd), 0
        else:
            real_u = [v / float(span_hi) for v in up]
            real_d = [v / float(span_hi) for v in down]
            if (settle_from and settle_t < SETTLE_FRAMES
                    and len(settle_from[0]) == chart_cols):
                settle_t += 1
                q = settle_t / float(SETTLE_FRAMES)
                q = q * q * (3 - 2 * q)
                hu = [a + (b - a) * q for a, b in zip(settle_from[0], real_u)]
                hd = [a + (b - a) * q for a, b in zip(settle_from[1], real_d)]
                cu = mix(GHOST, NEW_RGB, 0.45 + 0.55 * q)
                cd = mix(GHOST, OK_RGB, 0.45 + 0.55 * q)
            else:
                hu, hd, cu, cd = real_u, real_d, NEW, OK
        for line in vbars([(v, cu) for v in hu], 3, hi=1.0):
            rows.append(seg([(RST, " ")] + line, w - 1))
        rows.append(seg([(RST, " "), (GRID, "─" * chart_cols)], w - 1))
        for line in vbars_down([(v, cd) for v in hd], 3, hi=1.0):
            rows.append(seg([(RST, " ")] + line, w - 1))
        left_lbl = "%dd ago" % len(days)
        rows.append(seg([(DIM, " " + left_lbl),
                         (DIM, " " * max(1, chart_cols - len(left_lbl) - 5)),
                         (DIM, "today")], w - 1))

        # ── by team ─────────────────────────────────────────────────────
        rows.append("")
        ranked = sorted(teams, key=lambda t: (
            -(by_team.get(t["key"], {}).get("open", 0)), t["key"]))
        if ranked:
            sel[TEAMS] = max(0, min(sel[TEAMS], len(ranked) - 1))
        room = max(1, h - 5 - len(rows))
        first = 0
        if len(ranked) > room:
            first = min(max(0, sel[TEAMS] - room // 2), len(ranked) - room)
        on_teams = focus == TEAMS
        counter = ("   %s%d-%d of %d"
                   % ("↑↓ " if on_teams else "", first + 1,
                      min(first + room, len(ranked)), len(ranked))
                   if len(ranked) > room else "")
        rows.append(seg([(ACCENT if on_teams else LBL, " ── BY TEAM ──"),
                         (ACCENT if on_teams else DIM, counter)], w - 1))
        rows.append(DIM + pad(" %-22s%6s%7s%8s%8s"
                              % ("TEAM", "OPEN", "TRIAGE", "DOING", "DONE%dD"
                                 % store.days), w - 1))
        for i, t in list(enumerate(ranked))[first:first + room]:
            c = by_team.get(t["key"], collections.Counter())
            here = on_teams and i == sel[TEAMS]
            tint = bg(38, 56, 76) if here else ""
            rows.append(seg([
                (tint + (ACCENT if here else TXT),
                 ("▸" if here else " ") + pad("%s  %s" % (t["key"], t["name"]), 22)),
                (tint + NEW, "%6d" % c.get("open", 0)),
                (tint + (BAD if c.get("triage") else DIM), "%7d" % c.get("triage", 0)),
                (tint + (WARN if c.get("started") else DIM), "%8d" % c.get("started", 0)),
                (tint + (OK if c.get("done") else DIM), "%8d" % c.get("done", 0)),
            ] + ([(tint, " " * w)] if here else []), w - 1))

        hints = [[(ACCENT, "↑↓"), (DIM, " scroll")],
                 [(DIM, "[tab] section")], [(DIM, "[w]indow")],
                 [(DIM, "[r]efresh")], [(DIM, "[q]uit")]]
        footer = [" " + line for line in pack_hints(hints, w - 2)]
        rows = rows[:h - len(footer)]
        while len(rows) < h - len(footer):
            rows.append("")
        rows.extend(footer)
        draw(rows, w, h)
        time.sleep(0.3)


main()
