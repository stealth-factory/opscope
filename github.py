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

Open counts - PRs, issues, drafts, review backlog - are point-in-time totals of
whatever is open right now, at any age. Everything else - the merge rate, the
per-day charts and the per-account merged/rate columns - covers the merge
window, which is the N days ending today.

Credentials: `github.token` in config.json, or $GITHUB_TOKEN. It must be a
*classic* token: a fine-grained one is limited to a single resource owner, so
it cannot span the orgs this board exists to compare. Two scopes - `repo` so search sees private
repositories, and `read:org` to enumerate your orgs. Missing either one does
not fail, it silently undercounts, so the granted scopes are checked and named.
The API is called directly, so the `gh` CLI is not required.

Keys: up/down select an account, r refreshes now, w cycles the window
(7/14/30/60/90 days), q quits.
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
from common import (RST, Keyboard, bar, bg, big, braille_plot,
                    config_token_warning, cycle, dance, draw, mix,
                    heat, load_config, maybe_help, meter, pack_hints, pad, rgb,
                    seg, setup, size, skeleton, stacked_bar, title, vbars,
                    vbars_down)

_CFG = load_config("github", {
    "token": "",
    "token_env": "GITHUB_TOKEN",
    "accounts": [],        # org logins and/or "@me"; empty = discover
    "window_days": 7,      # window the board opens on
    "refresh": 120,        # seconds between polls; GraphQL is 5000 points/hour
})

REFRESH = float(_CFG["refresh"])
WINDOWS = (7, 14, 30, 60, 90)
CONTRIB_WEEKS = 52       # a full year, like the calendar on github.com
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
# the chart fades from these into the real colours as figures land: pale
# enough to read as "not yet", tinted enough to still say which half is which
PR_RGB, OK_RGB = (180, 160, 255), (90, 240, 160)
GHOST = (96, 106, 124)
LOAD_PR, LOAD_OK = mix(GHOST, PR_RGB, 0.45), mix(GHOST, OK_RGB, 0.45)
SETTLE_FRAMES = 8
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


_SCOPES = {"seen": False, "have": set()}


def graphql(query, tok):
    body = json.dumps({"query": query}).encode()
    req = urllib.request.Request(API, data=body, headers={
        "Authorization": "Bearer " + tok,
        "Content-Type": "application/json",
        "User-Agent": "terminal-toys",
    })
    with urllib.request.urlopen(req, timeout=30) as r:
        granted = r.headers.get("X-OAuth-Scopes")
        if granted is not None:      # absent on fine-grained tokens
            _SCOPES["seen"] = True
            _SCOPES["have"] = set(x.strip() for x in granted.split(",") if x.strip())
        return json.load(r)


def scope_warning():
    """Flag a token that will undercount rather than fail.

    A classic token without `repo` still searches happily - it just returns
    public results only, so every figure on the board comes back smaller with
    nothing to say it did. Without `read:org` the account list comes back
    short the same way. Both are worse than an error, so name them.
    """
    if not _SCOPES["seen"]:
        return None
    missing = [x for x in ("repo", "read:org") if x not in _SCOPES["have"]]
    if not missing:
        return None
    why = ("private repos are not counted" if "repo" in missing
           else "orgs cannot be discovered")
    return "token lacks %s - %s" % (" and ".join(missing), why)


def discover_accounts(tok):
    """Every org you belong to, plus your own account."""
    q = "{ viewer { login organizations(first:20) { nodes { login } } } }"
    try:
        d = graphql(q, tok)["data"]["viewer"]
    except Exception:
        return []
    return [o["login"] for o in d["organizations"]["nodes"]] + ["@me"]


def scope(acc, viewer):
    """The search qualifier that limits results to a single account."""
    return ("user:%s" % viewer) if acc == "@me" else ("org:%s" % acc)


DAY_CHUNK = 20      # 2 searches a day; the alias ceiling sits between 60 and 90
FRESH_DAYS = 2      # trailing days to always refetch: today is still running,
                    # and the search index lags a little behind a merge


def build_day_query(q, dates):
    """Exact per-day PR counts for one account.

    The charts used to read PR nodes and bucket their timestamps, but a search
    connection returns at most 100 nodes per page, so a busy fortnight lost
    everything past the hundredth record - and the merged series sorted by
    update time, so those hundred were not even the hundred most recently
    merged. A count per day is exact at any volume, and aliased searches cost
    one rate-limit point per request however many are packed into it.
    """
    parts = ["{"]
    for n, day in enumerate(dates):
        parts.append(
            '\n  m%(n)d: search(query:"%(q)s is:pr is:merged merged:%(d)s",'
            ' type:ISSUE) { issueCount }'
            '\n  c%(n)d: search(query:"%(q)s is:pr created:%(d)s",'
            ' type:ISSUE) { issueCount }' % {"n": n, "q": q, "d": day})
    parts.append("\n}")
    return "".join(parts)


def build_query(accounts, days, viewer):
    """Metrics for a batch of accounts in one request.

    Seven aliased searches per account keeps each request within GitHub's
    complexity limit - asking for seven accounts at once returned HTTP 502 -
    while still being far fewer round trips than one query per metric.
    """
    # N days *ending today*, so this spans exactly the dates the per-day
    # charts plot - `days` rather than `days - 1` would cover one day more
    # and quietly disagree with the chart drawn directly beneath it.
    since = (datetime.date.today()
             - datetime.timedelta(days=days - 1)).isoformat()
    parts = ["{"]
    for i, acc in enumerate(accounts):
        q = scope(acc, viewer)
        parts.append('''
  o%(i)d_open:    search(query:"%(q)s is:pr is:open", type:ISSUE) { issueCount }
  o%(i)d_draft:   search(query:"%(q)s is:pr is:open draft:true", type:ISSUE) { issueCount }
  o%(i)d_review:  search(query:"%(q)s is:pr is:open review:required", type:ISSUE) { issueCount }
  o%(i)d_merged:  search(query:"%(q)s is:pr is:merged merged:>=%(s)s", type:ISSUE) { issueCount }
  o%(i)d_dropped: search(query:"%(q)s is:pr is:unmerged is:closed closed:>=%(s)s", type:ISSUE) { issueCount }
  o%(i)d_issues:  search(query:"%(q)s is:issue is:open", type:ISSUE) { issueCount }'''
                     % {"i": i, "q": q, "s": since})
    parts.append("\n  rateLimit { remaining limit }\n}")
    return "".join(parts)


class Store(object):
    def __init__(self, accounts, days):
        self.lock = threading.Lock()
        self.accounts = accounts
        self.days = days
        self.stats = []
        self.day_cache = {}       # account -> {date: (merged, opened)}
        self.bust_days = False    # set by [r]: drop the day cache and refetch
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
        # A daemon thread that raises just stops, and a dead poller looks
        # exactly like a source with no data - which is how deployments.py
        # showed "0 deploys" for a day after an import went missing.
        try:
            self.poll()
        except Exception as e:
            with self.lock:
                self.error = "poller stopped: %s: %s" % (type(e).__name__,
                                                         str(e)[:70])

    def poll(self):
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
                    cal = graphql(contribution_query(CONTRIB_WEEKS), tok)["data"]["viewer"]
                    with self.lock:
                        self.calendar = cal["contributionsCollection"]["contributionCalendar"]
                except Exception:
                    pass
                with self.lock:
                    days_now, bust = self.days, self.bust_days
                    self.bust_days = False
                    # Start the pass from what is already on screen, keyed by
                    # account, so rows are replaced in place as each lands
                    # instead of the table emptying and refilling every pass.
                    by_acc = dict((x["key"], x) for x in self.stats)
                today = datetime.date.today()
                dates = [(today - datetime.timedelta(days=k)).isoformat()
                         for k in range(days_now - 1, -1, -1)]
                keep_from = (today - datetime.timedelta(
                    days=max(WINDOWS) - 1)).isoformat()
                failed, rate = [], None

                def publish():
                    with self.lock:
                        self.stats = [by_acc[a] for a in self.accounts
                                      if a in by_acc]
                        self.rate = rate
                        self.fetched = time.time()

                # Aggregates first, for every account, before any per-day work.
                # One request each, so the headline is live in seconds; the day
                # charts below can cost fifty requests on a cold 90d window and
                # would otherwise hold the whole board grey for minutes.
                for acc in self.accounts:
                    try:
                        data = graphql(build_query([acc], days_now, viewer), tok)
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
                    prev = by_acc.get(acc) or {}
                    by_acc[acc] = {
                        "key": acc,
                        "window": days_now,     # which window these figures cover
                        "account": viewer if acc == "@me" else acc,
                        "is_me": acc == "@me",
                        "open": g("open"), "draft": g("draft"),
                        "review": g("review"), "issues": g("issues"),
                        "merged": merged, "dropped": dropped,
                        "rate": (100.0 * merged / (merged + dropped)
                                 if merged + dropped else None),
                        # carried over until this account's day counts land, and
                        # tagged with the window they actually cover so a
                        # half-updated board cannot sum two windows together
                        "hist": prev.get("hist") or collections.Counter(),
                        "opened_hist": prev.get("opened_hist") or collections.Counter(),
                        "hist_window": prev.get("hist_window"),
                    }
                    publish()

                # Then the per-day counts. A past day cannot change - a PR
                # merged on the 3rd stays merged on the 3rd - so only days never
                # seen before, plus the trailing few (still running, and the
                # search index lags), cost a request. Widening the window
                # therefore buys only the days it adds; narrowing is free.
                for acc in self.accounts:
                    if acc not in by_acc:
                        continue
                    cache = self.day_cache.setdefault(acc, {})
                    if bust:
                        cache.clear()
                    want = [x for x in dates
                            if x not in cache or x in dates[-FRESH_DAYS:]]
                    for c in range(0, len(want), DAY_CHUNK):
                        chunk = want[c:c + DAY_CHUNK]
                        try:
                            dd = graphql(build_day_query(scope(acc, viewer),
                                                         chunk), tok)["data"]
                        except Exception:
                            continue
                        for n, day in enumerate(chunk):
                            cache[day] = ((dd.get("m%d" % n) or {}).get("issueCount", 0),
                                          (dd.get("c%d" % n) or {}).get("issueCount", 0))
                    for old_day in [x for x in cache if x < keep_from]:
                        del cache[old_day]            # older than any window
                    if not all(x in cache for x in dates):
                        continue                      # a chunk failed; leave it
                    by_acc[acc]["hist"] = collections.Counter(
                        dict((x, cache[x][0]) for x in dates))
                    by_acc[acc]["opened_hist"] = collections.Counter(
                        dict((x, cache[x][1]) for x in dates))
                    by_acc[acc]["hist_window"] = days_now
                    publish()
                with self.lock:
                    # with nothing else to report, surface a token sitting in a
                    # file other users on the box can read
                    self.error = (("could not read: " + ", ".join(failed))
                                  if failed else
                                  (scope_warning() or
                                   (config_token_warning()
                                    if source == "config" else None)))
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


WEEKDAYS = ("Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat")


def calendar_stats(weeks_data):
    """Streaks and totals behind the contribution calendar.

    A streak is consecutive days carrying at least one contribution, counted
    the way github.com does it: a day that has scored nothing *so far* does
    not break the current streak, because it is not over yet.
    """
    days = sorted((d["date"], d["contributionCount"], d.get("weekday", 0))
                  for wk in weeks_data for d in wk["contributionDays"])
    if not days:
        return None
    today = datetime.date.today().isoformat()

    longest = run = 0
    for _d, count, _wd in days:
        run = run + 1 if count else 0
        longest = max(longest, run)

    done = [x for x in days if x[0] <= today]
    if done and not done[-1][1]:
        done = done[:-1]              # today is still in progress
    current = 0
    for _d, count, _wd in reversed(done):
        if not count:
            break
        current += 1

    per_weekday = collections.Counter()
    for _d, count, wd in days:
        per_weekday[wd] += count
    top_wd = per_weekday.most_common(1)[0][0] if per_weekday else 0
    return {
        "today": dict((d, c) for d, c, _ in days).get(today, 0),
        "current": current,
        "longest": longest,
        "active": sum(1 for _d, c, _wd in days if c),
        "span": len(days),
        "busiest": max(days, key=lambda x: x[1]),
        "weekday": (WEEKDAYS[top_wd], per_weekday[top_wd]),
    }


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
    store = Store(accounts, days)
    th = threading.Thread(target=store.run)
    th.daemon = True
    th.start()

    selected = 0
    tick = 0
    settle_t, settle_from = 0, None
    while True:
        tick += 1
        for key in keyboard.poll():
            if key in ("q", "Q"):
                keyboard.restore()
                raise SystemExit(0)
            if key == "r":
                with store.lock:      # manual refresh re-reads even past days
                    store.bust_days = True
                store.wake.set()
            elif key == "w":
                with store.lock:
                    store.days = cycle(WINDOWS, store.days)
                store.wake.set()
            elif key == "up":
                selected = max(0, selected - 1)
            elif key == "down":
                selected += 1

        w, h = size()
        stats, rate, err, fetched, calendar = store.snapshot()
        # Busiest first: open PRs decide it, and merged-in-window breaks ties
        # so an idle backlog ranks below an account of the same size that is
        # actually moving. Name last, to keep the order steady frame to frame.
        stats.sort(key=lambda x: (-x["open"], -x["merged"], x["account"].lower()))
        # Windowed figures are stale until every account has reported for the
        # window now selected; rows carry the window they were fetched for.
        # The charts are tracked apart from the headline because their data
        # costs far more requests and so lands well after it.
        stale = not stats or any(x.get("window") != store.days for x in stats)
        chart_stale = not stats or any(x.get("hist_window") != store.days
                                       for x in stats)
        selected = max(0, min(selected, len(stats) - 1)) if stats else 0

        rows = [title("github ops", w, PR)]
        head = [(DIM, " %d account%s" % (len(stats), "" if len(stats) == 1 else "s")),
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
        # What is outstanding right now leads the board: it is the question
        # asked most often, and it is the only section that is not windowed.
        if tot["open"]:
            ready = max(0, tot["open"] - tot["draft"] - tot["review"])
            legend = [x for x in (("awaiting review", tot["review"], WARN),
                                  ("ready to merge", ready, OK),
                                  ("draft", tot["draft"], DIM)) if x[1]]
            rows.append(seg([(LBL, " ── OPEN PR STATE ── "),
                             (PR, "%d" % tot["open"]), (DIM, " PRs · "),
                             (WARN, "%d" % tot["issues"]),
                             (DIM, " issues open   (any age)")], w - 1))
            parts = [(n / float(tot["open"]), c) for _, n, c in legend]
            rows.append(seg([(RST, " ")] + stacked_bar(parts, max(10, w - 3)),
                            w - 1))
            key = [(RST, " ")]
            for label, count, colour in legend:
                key += [(colour, "▇ "), (TXT, label),
                        (DIM, " %d (%.0f%%)   " % (count, 100.0 * count / tot["open"]))]
            rows.append(seg(key, w - 1))

        rows.append("")
        pct_txt = ("%.0f%%" % rate_pct) if rate_pct is not None else "--"
        rcol = heat((rate_pct or 0) / 100.0) if rate_pct is not None else DIM
        rows.append(seg([(LBL, " ── MERGE RATE ── "),
                         (DIM, "last %d days" % store.days)], w - 1))
        bar_w = max(10, w - 34)
        if stale:
            rows.append(seg([(DIM, " %-5s" % "···")] + skeleton(bar_w, tick) +
                            [(DIM, "  loading %dd…" % store.days)], w - 1))
        else:
            rows.append(seg([(rcol, " %-5s" % pct_txt),
                             (rcol, meter((rate_pct or 0) / 100.0, bar_w)),
                             (OK, "  %d merged" % tot["merged"]),
                             (DIM, " / "), (BAD, "%d dropped" % tot["dropped"])],
                            w - 1))
        merged_all, opened_all = collections.Counter(), collections.Counter()
        for st in stats:
            if st.get("hist_window") != store.days:
                continue           # covers a different window; adding it lies
            merged_all.update(st["hist"])
            opened_all.update(st.get("opened_hist") or {})
        hist_days = store.days
        today = datetime.date.today()

        days = [(today - datetime.timedelta(days=n)).isoformat()
                for n in range(hist_days - 1, -1, -1)]

        # The chart always fills the pane. Where there is room to spare a day
        # takes several columns (with a gap between bars once they are wide
        # enough to need one); where there is not, the oldest days are cropped
        # rather than the whole chart being squeezed into a corner.
        avail = max(10, w - 3)
        if len(days) > avail:
            days = days[-avail:]
        slot = max(1, avail // len(days))
        gap = 1 if slot >= 3 else 0
        barw = slot - gap

        def spread(per_day):
            """One value per day, widened to the bar it will be drawn as.

            Everything the chart draws goes through this, the loading
            animation included, so the placeholder has exactly the bars the
            real chart will have - same count, same width, same gaps.
            """
            cols = []
            for n, v in enumerate(per_day):
                cols.extend([v] * barw)
                if gap and n < len(per_day) - 1:
                    cols.extend([0] * gap)
            return cols

        # One chart, two directions: PRs opened grow up, PRs merged grow down
        # from a shared baseline. Read together they answer whether the queue
        # is filling faster than it drains - which two separate charts made
        # you compare by eye across a heading.
        opened_day = [opened_all.get(d, 0) for d in days]
        merged_day = [merged_all.get(d, 0) for d in days]
        up, down = spread(opened_day), spread(merged_day)
        chart_cols = len(up)
        # One scale both ways, or the comparison lies.
        span_hi = max(up + down) or 1
        # A narrow pane cannot draw 90 columns, so the chart shows the most
        # recent days that fit and says so - the totals are of what is drawn,
        # not of the window, and would otherwise contradict the merge rate.
        span = ("%dd of %dd" % (len(days), hist_days)
                if len(days) < hist_days else "%dd" % len(days))
        rows.append("")
        if chart_stale:
            # totals across a half-updated board would sum two windows
            rows.append(seg([(LBL, " ── PR FLOW ── "),
                             (DIM, "counting %dd…" % hist_days)], w - 1))
        else:
            # totals come from the days themselves: a day spans several
            # columns now, so summing the columns would multiply by bar width
            rows.append(seg([(LBL, " ── PR FLOW ── "), (DIM, "%s · " % span),
                             (PR, "▲ %d opened" % sum(opened_day)),
                             (DIM, " · "),
                             (OK, "▼ %d merged" % sum(merged_day)),
                             (DIM, "   peak %d/day" % span_hi)], w - 1))
        # While the figures are still arriving the bars bounce like a level
        # meter in pale versions of their own colours, then settle onto the
        # real values rather than cutting to them. Three rows each side, always
        # - trimming the unused half would make the chart change height at the
        # end of the animation, which is exactly when it should be still.
        if chart_stale:
            # dance per day, then widen: bouncing each column on its own would
            # show a twelve-column day as twelve separate thin bars
            hu = spread(dance(len(days), tick))
            hd = spread(dance(len(days), tick, phase=2.1))
            cu, cd = LOAD_PR, LOAD_OK
            settle_from, settle_t = (hu, hd), 0
        else:
            real_u = [v / float(span_hi) for v in up]
            real_d = [v / float(span_hi) for v in down]
            if (settle_from and settle_t < SETTLE_FRAMES
                    and len(settle_from[0]) == chart_cols):
                settle_t += 1
                q = settle_t / float(SETTLE_FRAMES)
                q = q * q * (3 - 2 * q)          # ease in and out of the move
                hu = [a + (b - a) * q for a, b in zip(settle_from[0], real_u)]
                hd = [a + (b - a) * q for a, b in zip(settle_from[1], real_d)]
                cu, cd = mix(GHOST, PR_RGB, 0.45 + 0.55 * q), mix(
                    GHOST, OK_RGB, 0.45 + 0.55 * q)
            else:
                hu, hd, cu, cd = real_u, real_d, PR, OK
        for line in vbars([(v, cu) for v in hu], 3, hi=1.0):
            rows.append(seg([(RST, " ")] + line, w - 1))
        # an explicit baseline: without it the two series abut and the eye
        # cannot tell which row the bars grow from
        rows.append(seg([(RST, " "), (GRID, "─" * chart_cols)], w - 1))
        for line in vbars_down([(v, cd) for v in hd], 3, hi=1.0):
            rows.append(seg([(RST, " ")] + line, w - 1))
        left = "%dd ago" % len(days)
        rows.append(seg([(DIM, " " + left),
                         (DIM, " " * max(1, chart_cols - len(left) - 5)),
                         (DIM, "today")], w - 1))

        rows.append("")

        # The account table earns the remaining height: it scrolls within
        # whatever is left, while the calendar below it spends eight rows on
        # decoration. The calendar keeps its place only where the pane is tall
        # enough for both.
        if calendar and h > 38:
            grid, peak, total = heatmap(calendar["weeks"], w)
            total_c = calendar.get("totalContributions", total)
            rows.append(seg([(LBL, " ── CONTRIBUTIONS ── "),
                             (DIM, "%d in %d weeks, peak %d/day"
                              % (total_c, CONTRIB_WEEKS, peak))],
                            w - 1))
            for r, line in enumerate(grid):
                label = ("Mon", "", "Wed", "", "Fri", "", "")[r]
                rows.append(seg([(DIM, " %-4s" % label), (OK, "".join(line))],
                                w - 1))
            cs = calendar_stats(calendar["weeks"])
            if cs:
                bd, bc, _bw = cs["busiest"]
                cells = [
                    ("current streak", "%d days" % cs["current"],
                     OK if cs["current"] else DIM),
                    ("longest streak", "%d days" % cs["longest"], TXT),
                    ("today", "%d" % cs["today"],
                     OK if cs["today"] else DIM),
                    ("active days", "%d of %d (%.0f%%)"
                     % (cs["active"], cs["span"],
                        100.0 * cs["active"] / cs["span"]), TXT),
                    ("busiest", "%s (%d)" % (bd, bc), TXT),
                    ("most on", "%s (%d)" % cs["weekday"], TXT),
                ]
                # as many columns as the width honestly allows, never fewer
                # than one - the labels are what make these readable
                ncols = 3 if w >= 86 else (2 if w >= 58 else 1)
                cw = (w - 2) // ncols
                for n in range(0, len(cells), ncols):
                    line = [(RST, " ")]
                    for label, value, colour in cells[n:n + ncols]:
                        used = len(label) + 1 + len(value)
                        line += [(DIM, label + " "), (colour, value),
                                 (RST, " " * max(2, cw - used))]
                    rows.append(seg(line, w - 1))
            rows.append("")

        # Scroll rather than truncate: the selection has to stay on screen, or
        # the arrows move something invisible. Keep it centred where there is
        # room either side, and pinned at the ends of the list. Two header
        # lines and the footer come out of the height before the rows do.
        room = max(1, h - 5 - len(rows))
        first = 0
        if len(stats) > room:
            first = min(max(0, selected - room // 2), len(stats) - room)
        counter = ("   %d-%d of %d" % (first + 1, min(first + room, len(stats)),
                                       len(stats))
                   if len(stats) > room else "")
        rows.append(seg([(LBL, " ── BY ACCOUNT ──"), (DIM, counter)], w - 1))
        wide = w >= 62
        # No separators between these fields: the row emits %5d/%6s
        # back-to-back, so a space here drifts the header one column per
        # field - four by the time it reaches RATE.
        # MRG takes seven: "MRG60D" is six characters and would sit flush
        # against REVW in every window but the seven-day one.
        head = " %-20s%5s%5s%7s%6s" % ("ACCOUNT", "OPEN", "REVW",
                                       "MRG%dD" % store.days, "RATE")
        bar_cols = max(4, w - 64)
        spark_days = [(today - datetime.timedelta(days=n)).isoformat()
                      for n in range(min(store.days, bar_cols) - 1, -1, -1)]
        if wide:
            head += "%7s" % "ISSUES"
            # Each row is scaled to its own busiest day, so a full block is
            # 31 merged on one account and 16 on another. Say what the reader
            # may do with it - read the shape - rather than naming the
            # mechanism, which is what "OWN PEAK" did until someone asked.
            # Pick the longest label that fits rather than clipping one:
            # "MERGED/DAY" cut to "MERGED" would be a truncated hint, and the
            # scale caveat is worth keeping down to the narrowest width it
            # will fit in.
            for label in ("MERGED/DAY · SHAPE ONLY, NOT TO SCALE",
                          "MERGED/DAY · SHAPE ONLY",
                          "MERGED/DAY (shape)",
                          "MERGED/DAY", ""):
                if len(label) <= bar_cols:
                    break
            head += ("  " + label) if label else ""
        rows.append(DIM + pad(head, w - 1))
        for i, s in list(enumerate(stats))[first:first + room]:
            here = i == selected
            tint = bg(38, 56, 76) if here else ""
            r = s["rate"]
            # this row's own staleness: accounts land one at a time, so an
            # account already refetched for the new window shows real numbers
            # while the ones behind it still shimmer
            old = s.get("window") != store.days
            line = [(tint + (ACCENT if here else TXT),
                     ("▸" if here else " ") + pad(s["account"] + (" (you)" if s["is_me"] else ""), 20)),
                    (tint + PR, "%5d" % s["open"]),
                    (tint + (WARN if s["review"] else DIM), "%5d" % s["review"]),
                    (tint + (DIM if old else OK),
                     "%7s" % ("···" if old else s["merged"])),
                    (tint + (DIM if old else
                             (heat(r / 100.0) if r is not None else DIM)),
                     "%6s" % ("···" if old else
                              ("%.0f%%" % r if r is not None else "--")))]
            if wide:
                line.append((tint + DIM, "%7d" % s["issues"]))
                # Each account's own merged-per-day. The columns carry totals
                # but no shape, and a fortnight of nothing ending in a spike
                # reads very differently from a steady trickle. Scaled to this
                # account's own peak: the absolute is in MRG two columns left,
                # so the useful thing here is the shape.
                hist = s.get("hist") or {}
                if s.get("hist_window") != store.days:
                    line.append((tint + GRID, "  " + "·" * len(spark_days)))
                else:
                    top = max(hist.values()) if hist else 0
                    marks = ""
                    for d in spark_days:
                        v = hist.get(d, 0)
                        marks += (SPARK[min(7, int(v / float(top) * 7.99))]
                                  if v and top else " ")
                    line.append((tint + OK, "  " + marks))
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
