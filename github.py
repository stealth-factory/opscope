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
from common import (RST, Keyboard, bar, bg, big, braille_plot, cycle, draw,
                    heat, load_config, maybe_help, meter, pack_hints, pad, rgb,
                    seg, setup, size, stacked_bar, title)

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


def pipeline(tot, days, w):
    """The funnel every PR falls through, drawn as one.

    Counts alone do not show that review backlog is a *stage* rather than a
    number, so the stages are nested and gauged against the open total.

    Every row is built to exactly `inner` printable cells before its borders,
    including the borders themselves - computing them by separate formulae is
    how the right edge ends up ragged.
    """
    inner = max(30, min(46, w - 5))
    ready = max(0, tot["open"] - tot["draft"] - tot["review"])
    gauge_w = max(6, inner - 26)
    out = [[(LBL, " ┌"), (LBL, ("─ PIPELINE ").ljust(inner, "─")), (LBL, "┐")]]

    def row(segments):
        plain = sum(len(t) for _, t in segments)
        if plain > inner:                      # trim rather than push the border
            segments = segments[:-1] + [(segments[-1][0],
                                         segments[-1][1][:inner - plain])]
            plain = inner
        out.append([(LBL, " │")] + segments +
                   [(RST, " " * (inner - plain)), (LBL, "│")])

    row([(PR, "  ▣ open"), (TXT, "%6d" % tot["open"])])
    for mark, label, count, colour in (("├", "draft", tot["draft"], DIM),
                                       ("├", "review", tot["review"], WARN),
                                       ("└", "ready", ready, OK)):
        frac = count / float(tot["open"]) if tot["open"] else 0
        row([(DIM, "    %s " % mark), (colour, "%-7s" % label),
             (TXT, "%6d " % count), (colour, meter(frac, gauge_w))])
    row([])
    row([(OK, "  ✔ merged"), (TXT, "%5d" % tot["merged"]),
         (DIM, " in %dd" % days), (BAD, "    ✖ %d dropped" % tot["dropped"])])
    out.append([(LBL, " └"), (LBL, "─" * inner), (LBL, "┘")])
    return out


def daily_spark(hist, days, width):
    today = datetime.date.today()
    series = [hist.get((today - datetime.timedelta(days=n)).isoformat(), 0)
              for n in range(days - 1, -1, -1)][-width:]
    peak = max(series) if series else 0
    if not peak:
        return "·" * len(series), 0
    return "".join(SPARK[min(7, int(v / peak * 7.99))] for v in series), peak


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
        rows.append(seg([(LBL, " ── MERGE RATE ── "),
                         (DIM, "last %d days" % store.days)], w - 1))
        rows.append(seg([(rcol, " %-5s" % pct_txt),
                         (rcol, meter((rate_pct or 0) / 100.0, max(10, w - 34))),
                         (OK, "  %d merged" % tot["merged"]),
                         (DIM, " / "), (BAD, "%d dropped" % tot["dropped"])], w - 1))
        rows.append(seg([(PR, " %d" % tot["open"]), (DIM, " PRs open   "),
                         (WARN, "%d" % tot["issues"]), (DIM, " issues open   "),
                         (DIM, "%d accounts" % len(stats))], w - 1))

        merged_all = collections.Counter()
        for s in stats:
            merged_all.update(s["hist"])
        hist_days = int(_CFG["history_days"])
        today = datetime.date.today()
        series = [merged_all.get((today - datetime.timedelta(days=n)).isoformat(), 0)
                  for n in range(hist_days - 1, -1, -1)]
        peak = max(series) if series else 0
        rows.append("")
        rows.append(seg([(LBL, " ── MERGED / DAY ── "),
                         (DIM, "%dd, peak %d" % (hist_days, peak))], w - 1))
        for line in braille_plot(series, max(10, w - 4), 3, lo=0):
            rows.append(seg([(OK, "  " + line)], w - 1))

        rows.append("")
        # donut of open PRs by account, beside a dial of the merge rate
        if tot["open"]:
            ready = max(0, tot["open"] - tot["draft"] - tot["review"])
            legend = [x for x in (("awaiting review", tot["review"], WARN),
                                  ("ready to merge", ready, OK),
                                  ("draft", tot["draft"], DIM)) if x[1]]
            rows.append(seg([(LBL, " ── OPEN PR STATE ── "),
                             (DIM, "%d total" % tot["open"])], w - 1))
            parts = [(n / float(tot["open"]), c) for _, n, c in legend]
            rows.append(seg([(RST, " ")] + stacked_bar(parts, max(10, w - 3)), w - 1))
            key = [(RST, " ")]
            for label, count, colour in legend:
                key += [(colour, "▇ "), (TXT, label),
                        (DIM, " %d (%.0f%%)   " % (count, 100.0 * count / tot["open"]))]
            rows.append(seg(key, w - 1))
            rows.append("")

        for line in pipeline(tot, store.days, w):
            rows.append(seg(line, w - 1))

        if calendar and h > 30:
            grid, peak, total = heatmap(calendar["weeks"], w)
            rows.append("")
            rows.append(seg([(LBL, " ── CONTRIBUTIONS ── "),
                             (DIM, "%d in 26 weeks, peak %d/day"
                              % (calendar.get("totalContributions", total), peak))],
                            w - 1))
            for r, line in enumerate(grid):
                label = ("Mon", "", "Wed", "", "Fri", "", "")[r]
                rows.append(seg([(DIM, " %-4s" % label), (OK, "".join(line))], w - 1))

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
        rows.append(seg([(LBL, " ── BY ACCOUNT ──")], w - 1))
        wide = w >= 62
        head = " %-20s %5s %5s %6s %6s" % ("ACCOUNT", "PRS", "REVW", "MERGED", "RATE")
        if wide:
            head += " %6s" % "ISSUES"
        rows.append(DIM + pad(head, w - 1))
        busiest = max((s["open"] for s in stats), default=0) or 1
        for i, s in enumerate(stats):
            if len(rows) >= h - 3:
                break
            here = i == selected
            tint = bg(38, 56, 76) if here else ""
            r = s["rate"]
            line = [(tint + (ACCENT if here else TXT),
                     ("▸" if here else " ") + pad(s["account"] + (" (you)" if s["is_me"] else ""), 20)),
                    (tint + PR, "%5d" % s["open"]),
                    (tint + (WARN if s["review"] else DIM), "%5d" % s["review"]),
                    (tint + OK, "%6d" % s["merged"]),
                    (tint + (heat(r / 100.0) if r is not None else DIM),
                     "%5s%%" % ("%.0f" % r if r is not None else " --"))]
            if wide:
                line.append((tint + DIM, "%6d" % s["issues"]))
                # relative share of open PRs, so scale is visible not just rank
                line.append((tint + GRID, "  " + meter(s["open"] / float(busiest),
                                                       max(4, w - 62))))
            if here:
                line.append((tint, " " * w))
            rows.append(seg(line, w - 1))

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
