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
"""GitHub delivery metrics across every org and account you can see.

Open pull requests, how many are actually merging, review backlog and issue
counts - for one org, several, or your personal account alongside them.

    python3 github.py [-n SECONDS] [account ...]

Accounts are org logins, or @me for your own. With none given it uses
`github.accounts` from config, and failing that every org you belong to plus
your personal account.

Credentials: `github.token` in config.json, or $GITHUB_TOKEN. A classic
personal access token with `repo` and `read:org` covers private repositories
and org discovery. The API is called directly, so the `gh` CLI is not required.

Keys: up/down select an account, r refreshes now, w cycles the merge window
(7/14/30/90 days), q quits.
"""
import collections
import datetime
import math
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, bar, bg, braille_plot, cycle, draw, heat,
                    load_config, maybe_help, meter, pack_hints, pad, panel, rgb,
                    seg, setup, side_by_side, size, stacked_bar, title)

_CFG = load_config("github", {
    "token": "",
    "token_env": "GITHUB_TOKEN",
    "accounts": [],        # org logins and/or "@me"; empty = discover
    "window_days": 7,      # merge-rate window
    "refresh": 120,        # seconds between polls; GraphQL is 5000 points/hour
    "history_days": 14,    # width of the merged-per-day sparkline
})

REFRESH = float(_CFG["refresh"])
WINDOWS = (7, 14, 30, 90)
API = "https://api.github.com/graphql"

OK = rgb(90, 240, 160)
WARN = rgb(255, 200, 90)
BAD = rgb(255, 100, 110)
DIM = rgb(127, 147, 172)
GRID = rgb(60, 78, 98)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
ACCENT = rgb(150, 210, 255)
PR = rgb(180, 160, 255)
SPARK = "▁▂▃▄▅▆▇█"


def token():
    """A GitHub token from config.json or the environment.

    Deliberately not shelled out to the `gh` CLI: this widget talks to the API
    directly and should not require another program to be installed, logged in
    and current just to read a number.
    """
    if _CFG["token"]:
        return _CFG["token"], "config"
    tok = os.environ.get(_CFG["token_env"] or "GITHUB_TOKEN")
    if tok:
        return tok, "env"
    return None, "missing"


def graphql(query, tok):
    body = json.dumps({"query": query}).encode()
    req = urllib.request.Request(API, data=body, headers={
        "Authorization": "Bearer " + tok,
        "Content-Type": "application/json",
        "User-Agent": "terminal-toys",
    })
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def discover_accounts(tok):
    """Every org you belong to, plus your own account."""
    q = "{ viewer { login organizations(first:20) { nodes { login } } } }"
    try:
        d = graphql(q, tok)["data"]["viewer"]
    except Exception:
        return []
    return [o["login"] for o in d["organizations"]["nodes"]] + ["@me"]


def build_query(accounts, days, history_days, viewer):
    """Metrics for a batch of accounts in one request.

    Seven aliased searches per account keeps each request within GitHub's
    complexity limit - asking for seven accounts at once returned HTTP 502 -
    while still being far fewer round trips than one query per metric.
    """
    since = (datetime.date.today() - datetime.timedelta(days=days)).isoformat()
    hist_since = (datetime.date.today()
                  - datetime.timedelta(days=history_days)).isoformat()
    parts = ["{"]
    for i, acc in enumerate(accounts):
        who = viewer if acc == "@me" else acc
        kind = "user" if acc == "@me" else "org"
        q = "%s:%s" % (kind, who)
        parts.append('''
  o%(i)d_open:    search(query:"%(q)s is:pr is:open", type:ISSUE) { issueCount }
  o%(i)d_draft:   search(query:"%(q)s is:pr is:open draft:true", type:ISSUE) { issueCount }
  o%(i)d_review:  search(query:"%(q)s is:pr is:open review:required", type:ISSUE) { issueCount }
  o%(i)d_merged:  search(query:"%(q)s is:pr is:merged merged:>=%(s)s", type:ISSUE) { issueCount }
  o%(i)d_dropped: search(query:"%(q)s is:pr is:unmerged is:closed closed:>=%(s)s", type:ISSUE) { issueCount }
  o%(i)d_issues:  search(query:"%(q)s is:issue is:open", type:ISSUE) { issueCount }
  o%(i)d_hist:    search(query:"%(q)s is:pr is:merged merged:>=%(h)s", type:ISSUE, first:100) {
    nodes { ... on PullRequest { mergedAt } }
  }''' % {"i": i, "q": q, "s": since, "h": hist_since})
    parts.append("\n  rateLimit { remaining limit }\n}")
    return "".join(parts)


class Store(object):
    def __init__(self, accounts, days, history_days):
        self.lock = threading.Lock()
        self.accounts = accounts
        self.days = days
        self.history_days = history_days
        self.stats = []
        self.calendar = None
        self.rate = None
        self.error = None
        self.fetched = 0
        self.wake = threading.Event()

    def snapshot(self):
        with self.lock:
            return (list(self.stats), self.rate, self.error, self.fetched,
                    self.calendar)

    def run(self):
        viewer = None
        while True:
            tok, source = token()
            if not tok:
                with self.lock:
                    self.error = ("no token: set github.token in config.json "
                                  "or $%s (needs repo + read:org)"
                                  % (_CFG["token_env"] or "GITHUB_TOKEN"))
                self.wake.wait(REFRESH)
                self.wake.clear()
                continue
            try:
                if viewer is None:
                    viewer = graphql("{ viewer { login } }",
                                     tok)["data"]["viewer"]["login"]
                if not self.accounts:
                    self.accounts = discover_accounts(tok) or ["@me"]
                try:
                    cal = graphql(contribution_query(26), tok)["data"]["viewer"]
                    with self.lock:
                        self.calendar = cal["contributionsCollection"]["contributionCalendar"]
                except Exception:
                    pass
                stats, failed, rate = [], [], None
                # one request per account: results appear as they arrive, and a
                # single bad account cannot blank the whole board
                for acc in self.accounts:
                    try:
                        data = graphql(build_query([acc], self.days,
                                                   self.history_days, viewer), tok)
                        if data.get("errors"):
                            raise ValueError(data["errors"][0].get("message", "")[:50])
                    except Exception as e:
                        failed.append("%s (%s)" % (acc, type(e).__name__))
                        continue
                    d = data["data"]
                    rate = d.get("rateLimit") or rate
                    i = 0
                    g = lambda k: (d.get("o%d_%s" % (i, k)) or {}).get("issueCount", 0)
                    merged, dropped = g("merged"), g("dropped")
                    hist = collections.Counter()
                    for n in ((d.get("o%d_hist" % i) or {}).get("nodes") or []):
                        when = (n or {}).get("mergedAt")
                        if when:
                            hist[when[:10]] += 1
                    stats.append({
                        "account": viewer if acc == "@me" else acc,
                        "is_me": acc == "@me",
                        "open": g("open"), "draft": g("draft"),
                        "review": g("review"), "issues": g("issues"),
                        "merged": merged, "dropped": dropped,
                        "rate": (100.0 * merged / (merged + dropped)
                                 if merged + dropped else None),
                        "hist": hist,
                    })
                    with self.lock:            # publish as each one lands
                        self.stats = list(stats)
                        self.rate = rate
                        self.fetched = time.time()
                with self.lock:
                    self.error = ("could not read: " + ", ".join(failed)) if failed else None
            except urllib.error.HTTPError as e:
                with self.lock:
                    self.error = "HTTP %s from GitHub%s" % (
                        e.code, " (token lacks scope?)" if e.code == 403 else "")
            except Exception as e:
                with self.lock:
                    self.error = "%s: %s" % (type(e).__name__, str(e)[:60])
            self.wake.wait(REFRESH)
            self.wake.clear()


def contribution_query(weeks):
    """GitHub's own contribution calendar - the green squares.

    contributionsCollection is per-viewer rather than per-org, so this is your
    activity across everything, which is what the calendar means on github.com.
    """
    since = (datetime.datetime.now(datetime.timezone.utc)
             - datetime.timedelta(weeks=weeks)).strftime("%Y-%m-%dT%H:%M:%SZ")
    return """{ viewer { contributionsCollection(from:"%s") {
      contributionCalendar { totalContributions
        weeks { contributionDays { date contributionCount weekday } } } } } }""" % since


def heatmap(weeks_data, w):
    """The calendar as seven rows of one cell per week."""
    levels = " ░▒▓█"
    counts = [d["contributionCount"] for wk in weeks_data
              for d in wk["contributionDays"]]
    peak = max(counts) if counts else 0
    cols = max(4, min(len(weeks_data), w - 8))
    weeks_data = weeks_data[-cols:]
    grid = [[" "] * len(weeks_data) for _ in range(7)]
    for x, wk in enumerate(weeks_data):
        for d in wk["contributionDays"]:
            n = d["contributionCount"]
            lvl = 0 if not n else min(4, 1 + int(n / (peak or 1) * 3.99))
            grid[d["weekday"]][x] = levels[lvl]
    return grid, peak, sum(counts)


def stage_rows(tot, days, gauge_w):
    """Open PRs by stage, as flat rows rather than a nested tree.

    Indentation made the block look ragged for no information gain; the gauge
    already shows each stage as a share of the open total.
    """
    ready = max(0, tot["open"] - tot["draft"] - tot["review"])
    rows = []
    for label, count, colour in (("awaiting review", tot["review"], WARN),
                                 ("ready to merge", ready, OK),
                                 ("draft", tot["draft"], DIM)):
        frac = count / float(tot["open"]) if tot["open"] else 0
        rows.append([(TXT, " %-16s" % label), (colour, "%5d " % count),
                     (colour, meter(frac, gauge_w)),
                     (DIM, "%4.0f%%" % (frac * 100))])
    return rows


def ago(t):
    if not t:
        return "--"
    s = time.time() - t
    return "%ds" % s if s < 90 else ("%dm" % (s / 60) if s < 5400 else "%dh" % (s / 3600))


def main():
    maybe_help(__doc__)
    global REFRESH
    args = sys.argv[1:]
    while args and args[0] in ("-n", "--refresh"):
        REFRESH = max(30.0, float(args[1]))
        args = args[2:]
    accounts = args or list(_CFG["accounts"])
    days = int(_CFG["window_days"])

    setup()
    keyboard = Keyboard()
    store = Store(accounts, days, int(_CFG["history_days"]))
    th = threading.Thread(target=store.run)
    th.daemon = True
    th.start()

    selected = 0
    while True:
        for key in keyboard.poll():
            if key in ("q", "Q"):
                keyboard.restore()
                raise SystemExit(0)
            if key == "r":
                store.wake.set()
            elif key == "w":
                store.days = cycle(WINDOWS, store.days)
                store.wake.set()
            elif key == "up":
                selected = max(0, selected - 1)
            elif key == "down":
                selected += 1

        w, h = size()
        stats, rate, err, fetched, calendar = store.snapshot()
        selected = max(0, min(selected, len(stats) - 1)) if stats else 0

        rows = [title("github ops", w, PR)]
        head = [(DIM, " %d account%s" % (len(stats), "" if len(stats) == 1 else "s")),
                (DIM, " · %dd window" % store.days),
                (DIM, "   updated %s ago" % ago(fetched))]
        if rate:
            left = rate.get("remaining", 0)
            head.append((OK if left > 1000 else WARN, "   %d/%d api" % (left, rate.get("limit", 0))))
        rows.append(seg(head, w - 1))
        if err:
            rows.append(seg([(BAD, " ! " + err)], w - 1))
        if not stats:
            rows.append(seg([(DIM, " collecting…")], w - 1))
            draw(rows, w, h)
            time.sleep(0.4)
            continue

        # totals across every account
        tot = {k: sum(s[k] for s in stats)
               for k in ("open", "draft", "review", "issues", "merged", "dropped")}
        rate_pct = (100.0 * tot["merged"] / (tot["merged"] + tot["dropped"])
                    if tot["merged"] + tot["dropped"] else None)
        rows.append("")
        pct_txt = ("%.0f%%" % rate_pct) if rate_pct is not None else "--"
        rcol = heat((rate_pct or 0) / 100.0) if rate_pct is not None else DIM
        half = max(26, (w - 6) // 2)
        left = panel("MERGE RATE", [
            [(rcol, " %-5s" % pct_txt), (DIM, "of PRs closed in %dd merged" % store.days)],
            [(rcol, " " + meter((rate_pct or 0) / 100.0, min(24, half - 4)))],
            [(OK, " %d merged" % tot["merged"]), (DIM, "   "),
             (BAD, "%d dropped" % tot["dropped"])],
        ], half, frame=GRID, accent=LBL)
        right = panel("OPEN WORK", [
            [(PR, " %-5d" % tot["open"]), (DIM, "pull requests open")],
            [(WARN, " %-5d" % tot["issues"]), (DIM, "issues open")],
            [(DIM, " across %d account%s" % (len(stats), "" if len(stats) == 1 else "s"))],
        ], half, frame=GRID, accent=LBL)
        for line in side_by_side([left, right], gap=2):
            rows.append(seg([(RST, " ")] + line, w - 1))

        merged_all = collections.Counter()
        for st in stats:
            merged_all.update(st["hist"])
        hist_days = int(_CFG["history_days"])
        today = datetime.date.today()
        series = [merged_all.get((today - datetime.timedelta(days=n)).isoformat(), 0)
                  for n in range(hist_days - 1, -1, -1)]
        peak = max(series) if series else 0
        rows.append("")
        chart_w = max(20, w - 6)
        body = [[(OK, line)] for line in braille_plot(series, chart_w, 3, lo=0)]
        body.append([(DIM, "%dd" % hist_days),
                     (DIM, " " * max(1, chart_w - 14)), (DIM, "peak %d/day" % peak)])
        for line in panel("MERGED PER DAY", body, chart_w, frame=GRID, accent=LBL):
            rows.append(seg([(RST, " ")] + line, w - 1))

        rows.append("")
        # donut of open PRs by account, beside a dial of the merge rate
        if tot["open"]:
            pw = max(28, w - 6)
            ready = max(0, tot["open"] - tot["draft"] - tot["review"])
            parts = [(tot["review"] / float(tot["open"]), WARN),
                     (ready / float(tot["open"]), OK),
                     (tot["draft"] / float(tot["open"]), DIM)]
            body = [stacked_bar(parts, pw)] + stage_rows(tot, store.days,
                                                         max(6, pw - 32))
            rows.append("")
            for line in panel("OPEN PR STATE", body, pw, frame=GRID, accent=LBL):
                rows.append(seg([(RST, " ")] + line, w - 1))

        if calendar and h > 30:
            grid, peak, total = heatmap(calendar["weeks"], w - 8)
            cw = max(28, w - 6)
            body = []
            for r, line in enumerate(grid):
                body.append([(DIM, " %-4s" % ("Mon", "", "Wed", "", "Fri", "", "")[r]),
                             (OK, "".join(line))])
            body.append([(DIM, " %d contributions, peak %d/day"
                          % (calendar.get("totalContributions", total), peak))])
            for line in panel("CONTRIBUTIONS · 26 WEEKS", body, cw,
                              frame=GRID, accent=LBL):
                rows.append(seg([(RST, " ")] + line, w - 1))

        rows.append("")
        rows.append(LBL + " ── NEEDS ATTENTION ──")
        flags = []
        if tot["review"]:
            flags.append((BAD if tot["review"] > 20 else WARN,
                          " ⚠ %d PRs awaiting review" % tot["review"]))
        if tot["draft"]:
            flags.append((DIM, " ○ %d drafts" % tot["draft"]))
        if tot["dropped"]:
            flags.append((DIM, " ✖ %d closed unmerged in %dd" % (tot["dropped"], store.days)))
        for colour, text in flags or [(OK, " ✓ nothing waiting")]:
            rows.append(seg([(colour, text)], w - 1))

        rows.append("")
        aw = max(30, w - 6)
        wide = w >= 68
        acct_body = [[(DIM, " %-18s %5s %5s %6s %6s%s"
                       % ("ACCOUNT", "PRS", "REVW", "MERGED", "RATE",
                          "  ISSUES" if wide else ""))]]
        busiest = max((x["open"] for x in stats), default=0) or 1
        for i, s in enumerate(stats):
            here = i == selected
            tint = bg(38, 56, 76) if here else ""
            r = s["rate"]
            line = [(tint + (ACCENT if here else TXT),
                     ("▸" if here else " ") + pad(s["account"] + (" (you)" if s["is_me"] else ""), 18)),
                    (tint + PR, "%5d" % s["open"]),
                    (tint + (WARN if s["review"] else DIM), "%5d" % s["review"]),
                    (tint + OK, "%6d" % s["merged"]),
                    (tint + (heat(r / 100.0) if r is not None else DIM),
                     "%5s%%" % ("%.0f" % r if r is not None else " --"))]
            if wide:
                line.append((tint + DIM, "  %6d" % s["issues"]))
                # relative share of open PRs, so scale is visible not just rank
                line.append((tint + GRID, " " + meter(s["open"] / float(busiest),
                                                      max(4, aw - 56))))
            acct_body.append(line)
        for line in panel("BY ACCOUNT", acct_body, aw, frame=GRID, accent=LBL):
            rows.append(seg([(RST, " ")] + line, w - 1))

        hints = [[(ACCENT, "↑↓"), (DIM, " account")], [(DIM, "[w]indow")],
                 [(DIM, "[r]efresh")], [(DIM, "[q]uit")]]
        footer = [" " + line for line in pack_hints(hints, w - 2)]
        rows = rows[:h - len(footer)]
        while len(rows) < h - len(footer):
            rows.append("")
        rows.extend(footer)
        draw(rows, w, h)
        time.sleep(0.3)


main()
