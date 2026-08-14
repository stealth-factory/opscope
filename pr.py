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
"""Watch the pull requests you have to follow up on.

A list of open PRs, and a dashboard for whichever one is selected: checks,
reviews, mergeability, and - when the PR belongs to a stack - the stack it
sits in and the order that stack has to merge in.

    python3 pr.py [-n SECONDS] [search terms ...]

Extra arguments are appended to the search, so `pr.py org:acme` narrows to one
organisation and `pr.py author:@me` to your own. With none given it uses
`pr.query` from config, which defaults to everything you are involved in.

Credentials: reuses `github.token` from config.json, or $GITHUB_TOKEN. A
classic token with `repo` and `read:org`, the same one github.py uses.

Keys: up/down select, enter opens a PR, esc goes back, / filters, s cycles the
sort, o reverses it, r refreshes, q quits.
"""
import datetime
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, bg, clipboard, config_token_warning, cycle,
                    draw, heat,
                    load_config, maybe_help, pack_hints, pad, rgb, seg, setup,
                    size, skeleton, stacked_bar, title, vbars)

_GH = load_config("github", {"token": "", "token_env": "GITHUB_TOKEN"})
_CFG = load_config("pr", {
    "token": "",
    "token_env": "GITHUB_TOKEN",
    # GitHub search has no OR, so anything that is a union of conditions has
    # to be several searches merged. Each entry here is one search; results
    # are pooled and de-duplicated. `@mine` expands to every org you belong
    # to plus your own account, as owner qualifiers.
    "sources": {
        "orgs": "is:open is:pr @mine",
        "authored": "is:open is:pr author:@me",
        "assigned": "is:open is:pr assignee:@me",
    },
    "limit": 50,           # per source; 3 x 100 nodes returns HTTP 502
    "refresh": 60,
})

REFRESH = float(_CFG["refresh"])
API = "https://api.github.com/graphql"
SORTS = ("updated", "created")

SPARK = "▁▂▃▄▅▆▇█"
OPENED_DAYS = 30   # width of the opened-per-day chart

OK = rgb(90, 240, 160)
WARN = rgb(255, 200, 90)
BAD = rgb(255, 100, 110)
DIM = rgb(127, 147, 172)
GRID = rgb(60, 78, 98)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
ACCENT = rgb(150, 210, 255)
PR = rgb(180, 160, 255)

# GitHub's vocabulary, rendered as words rather than colour alone so it
# survives a screenshot, a colourblind reader and a `pane read`
REVIEW_LABEL = {"APPROVED": ("approved", OK),
                "CHANGES_REQUESTED": ("CHANGES REQ", BAD),
                "REVIEW_REQUIRED": ("needs review", WARN),
                None: ("—", DIM)}
CHECK_LABEL = {"SUCCESS": ("pass", OK), "FAILURE": ("FAIL", BAD),
               "ERROR": ("ERROR", BAD), "PENDING": ("running", WARN),
               "EXPECTED": ("waiting", DIM), None: ("—", DIM)}
MERGE_LABEL = {"CLEAN": ("ready", OK), "DIRTY": ("CONFLICT", BAD),
               "BLOCKED": ("blocked", WARN), "BEHIND": ("behind", WARN),
               "UNSTABLE": ("checks red", WARN), "HAS_HOOKS": ("ready", OK),
               "DRAFT": ("draft", DIM), "UNKNOWN": ("…", DIM),
               None: ("—", DIM)}


def token():
    """The GitHub token, shared with github.py rather than duplicated."""
    for value, src in ((_CFG["token"], "config"), (_GH["token"], "config")):
        if value:
            return value, src
    tok = os.environ.get(_CFG["token_env"] or "GITHUB_TOKEN")
    return (tok, "env") if tok else (None, "missing")


_RATE = {"remaining": None}


def graphql(query, tok, variables=None):
    body = json.dumps({"query": query, "variables": variables or {}}).encode()
    req = urllib.request.Request(API, data=body, headers={
        "Authorization": "Bearer " + tok,
        "Content-Type": "application/json",
        "User-Agent": "terminal-toys",
    })
    with urllib.request.urlopen(req, timeout=45) as r:
        data = json.load(r)
    if data.get("errors"):
        raise ValueError(data["errors"][0].get("message", "")[:100])
    return data["data"]


PR_FIELDS = """
      number title url isDraft createdAt updatedAt
      additions deletions changedFiles
      author { login }
      repository { nameWithOwner }
      headRefName baseRefName reviewDecision mergeable
      stackEntry { position stack { number size } }
      commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }"""


def list_query(queries, limit):
    """One request, one aliased search per source.

    The ceiling is on result nodes rather than field complexity: three
    searches of 100 return HTTP 502 with or without the check rollup, three
    of 50 do not. So the page size is per source and deliberately modest.
    """
    parts = ["rateLimit { remaining }"]
    for i, q in enumerate(queries):
        parts.append('s%d: search(query: %s, type: ISSUE, first: %d) '
                     '{ issueCount nodes { ... on PullRequest { %s } } }'
                     % (i, json.dumps(q), limit, PR_FIELDS))
    return "{ %s }" % " ".join(parts)

DETAIL_QUERY = """
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      number title url state isDraft createdAt updatedAt
      additions deletions changedFiles
      author { login } headRefName baseRefName
      mergeable mergeStateStatus reviewDecision
      commitCount: commits { totalCount }
      stack { number size baseRefName
        entries(first: 40) { nodes { position pullRequest {
          number title isDraft reviewDecision mergeable
          additions deletions author { login } headRefName } } } }
      reviewThreads(first: 60) { nodes { isResolved } }
      reviews(last: 20) { nodes { author { login } state submittedAt } }
      reviewRequests(first: 12) { nodes { requestedReviewer {
        ... on User { login } ... on Team { name } } } }
      commits(last: 1) { nodes { commit { statusCheckRollup {
        state contexts(first: 25) { nodes {
          ... on CheckRun { name conclusion status startedAt completedAt }
          ... on StatusContext { context state } } } } } } }
    }
  }
}"""

# every open PR in one repository, for reconstructing a stack that was not
# made with `gh stack` - the API's own stack field is authoritative when it
# is there, and null everywhere else
REPO_PRS_QUERY = """
query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: OPEN, first: 100) {
      nodes { number title isDraft headRefName baseRefName
              additions deletions author { login }
              reviewDecision mergeable }
    }
  }
}"""


def parse(ts):
    if not ts:
        return None
    try:
        return datetime.datetime.strptime(ts[:19], "%Y-%m-%dT%H:%M:%S").replace(
            tzinfo=datetime.timezone.utc)
    except ValueError:
        return None


def ago(ts):
    when = parse(ts) if isinstance(ts, str) else ts
    if not when:
        return "--"
    s = (datetime.datetime.now(datetime.timezone.utc) - when).total_seconds()
    if s < 3600:
        return "%dm" % max(1, int(s // 60))
    if s < 86400:
        return "%dh" % int(s // 3600)
    if s < 86400 * 365:
        return "%dd" % int(s // 86400)
    return "%.1fy" % (s / (86400 * 365.0))


def rollup(pr):
    node = (pr.get("commits") or {}).get("nodes") or []
    if not node:
        return None
    return ((node[0].get("commit") or {}).get("statusCheckRollup") or {}).get(
        "state")


def stack_of(pr, repo_prs):
    """The chain this PR belongs to, newest-first from the trunk.

    GitHub's own `stack` is used when present. Otherwise the chain is
    reconstructed: a PR whose base branch is another open PR's head branch is
    sitting on top of it. That inference produces a tree rather than a line,
    so each PR keeps its list of children.
    """
    by_head = {}
    for other in repo_prs:
        by_head[other["headRefName"]] = other
    parent = {}
    kids = {}
    for other in repo_prs:
        up = by_head.get(other["baseRefName"])
        if up and up["number"] != other["number"]:
            parent[other["number"]] = up["number"]
            kids.setdefault(up["number"], []).append(other["number"])
    if pr["number"] not in parent and pr["number"] not in kids:
        return None, {}, {}
    root = pr["number"]
    seen = set()
    while root in parent and root not in seen:
        seen.add(root)
        root = parent[root]
    return root, parent, kids


class Store(object):
    def __init__(self, extra):
        self.lock = threading.Lock()
        self.extra = extra
        self.viewer = None
        self.orgs = []
        self.query = ""
        self.prs = []
        self.total = 0
        self.capped = []
        self.detail = None          # the open PR's full record
        self.stack_rows = []        # (depth, pr, is_current) for the stack map
        self.want = None            # (owner, name, number) to fetch detail for
        self.loading_detail = False
        self.error = None
        self.fetched = 0
        self.wake = threading.Event()

    def snapshot(self):
        with self.lock:
            return (list(self.prs), self.total, self.detail,
                    list(self.stack_rows), self.loading_detail, self.error,
                    self.fetched, self.query)

    def searches(self):
        """Each configured source, with `@mine` expanded and args appended.

        Repeated qualifiers of the same kind are OR'd by GitHub, so one
        search covers every org and your own account at once; relationships
        that reach outside them - authored, assigned - need their own.
        """
        mine = " ".join(["org:%s" % o for o in self.orgs]
                        + (["user:%s" % self.viewer] if self.viewer else []))
        out = []
        for name, q in _CFG["sources"].items():
            out.append((name, " ".join([q.replace("@mine", mine)]
                                       + self.extra)))
        return out

    def open_detail(self, pr):
        with self.lock:
            owner, _, name = pr["repository"]["nameWithOwner"].partition("/")
            self.want = (owner, name, pr["number"])
            self.detail, self.stack_rows = None, []
            self.loading_detail = True
        self.wake.set()

    def close_detail(self):
        with self.lock:
            self.want, self.detail, self.stack_rows = None, None, []
            self.loading_detail = False

    def run(self):
        while True:
            tok, source = token()
            if not tok:
                with self.lock:
                    self.error = ("no token: set github.token in config.json "
                                  "or $GITHUB_TOKEN")
                self.wake.wait(REFRESH)
                self.wake.clear()
                continue
            try:
                with self.lock:
                    want = self.want
                    have = (self.detail or {}).get("number")
                if want and want[2] != have:
                    self.fetch_detail(tok, want)
                self.fetch_list(tok, source)
            except urllib.error.HTTPError as e:
                with self.lock:
                    self.error = "HTTP %s from GitHub%s" % (
                        e.code, " (token scope?)" if e.code == 403 else "")
                    self.loading_detail = False
            except Exception as e:
                with self.lock:
                    self.error = "%s: %s" % (type(e).__name__, str(e)[:70])
                    self.loading_detail = False
            self.wake.wait(REFRESH)
            self.wake.clear()

    def fetch_list(self, tok, source):
        if self.viewer is None:
            who = graphql("{ viewer { login organizations(first:20)"
                          " { nodes { login } } } }", tok)["viewer"]
            with self.lock:
                self.viewer = who["login"]
                self.orgs = [o["login"] for o in who["organizations"]["nodes"]]
        pairs = self.searches()
        d = graphql(list_query([q for _n, q in pairs], int(_CFG["limit"])), tok)
        rate = d.get("rateLimit") or {}
        if rate.get("remaining") is not None:
            _RATE["remaining"] = rate["remaining"]
        # pool the sources, remembering which found each PR and noting when a
        # source filled its page, so a truncated union is not read as a total
        pool, capped = {}, []
        for i, (name, _q) in enumerate(pairs):
            block = d["s%d" % i]
            got = [n for n in block["nodes"] if n]
            if block["issueCount"] > len(got):
                capped.append(name)
            for n in got:
                pool.setdefault(n["url"], dict(n, sources=[]))["sources"].append(name)
        nodes = list(pool.values())
        with self.lock:
            self.query = ", ".join(n for n, _q in pairs)
            self.capped = capped
            self.prs = nodes
            self.total = len(nodes)
            self.fetched = time.time()
            self.error = (config_token_warning() if source == "config" else None)

    def fetch_detail(self, tok, want):
        owner, name, number = want
        d = graphql(DETAIL_QUERY, tok, {"owner": owner, "name": name,
                                        "number": number})
        pr = (d.get("repository") or {}).get("pullRequest")
        rows = []
        if pr:
            native = pr.get("stack")
            if native:
                # GitHub hands the order over directly, position 1 nearest the
                # base. A native stack is a line, not a tree, so it draws flat
                # - eleven levels of indentation would be unreadable and would
                # imply a branching that is not there.
                entries = sorted(native["entries"]["nodes"],
                                 key=lambda x: x.get("position") or 0)
                for i, e in enumerate(entries):
                    child = e["pullRequest"]
                    twig = "└─ " if i == len(entries) - 1 else "├─ "
                    rows.append((twig, child, child["number"] == number,
                                 e.get("position")))
            else:
                repo = graphql(REPO_PRS_QUERY, tok,
                               {"owner": owner, "name": name})
                others = (repo.get("repository") or {}).get(
                    "pullRequests", {}).get("nodes", [])
                root, parent, kids = stack_of(pr, others)
                if root is not None:
                    by_num = dict((o["number"], o) for o in others)

                    def walk(num, prefix, last):
                        # an inferred stack really is a tree - one PR here has
                        # two others branched off it - so draw the connectors
                        # properly rather than indenting by depth alone
                        node = by_num.get(num)
                        if node:
                            rows.append((prefix + ("└─ " if last else "├─ "),
                                         node, num == number, None))
                        children = sorted(kids.get(num, []))
                        below = prefix + ("   " if last else "│  ")
                        for i, kid in enumerate(children):
                            walk(kid, below, i == len(children) - 1)

                    walk(root, "", True)
        with self.lock:
            self.detail = pr
            self.stack_rows = rows
            self.loading_detail = False


def hours_since(ts):
    when = parse(ts)
    if not when:
        return None
    return (datetime.datetime.now(datetime.timezone.utc)
            - when).total_seconds() / 3600.0


def span(hours):
    if hours is None:
        return "--"
    if hours < 48:
        return "%dh" % int(hours)
    days = hours / 24.0
    return "%dd" % int(days) if days < 365 else "%.1fy" % (days / 365.0)


def ready_to_merge(pr):
    """Approved, green, no conflict, not a draft - the actionable count.

    Everything else on this board describes work in flight; this is the one
    number that says something can be done right now.
    """
    return (pr.get("reviewDecision") == "APPROVED"
            and rollup(pr) in ("SUCCESS", None)
            and pr.get("mergeable") != "CONFLICTING"
            and not pr.get("isDraft"))


def stats_view(prs, w):
    """Shape and age of whatever the list is currently showing."""
    rows = [""]
    if not prs:
        return rows
    n = len(prs)
    review = {"APPROVED": 0, "CHANGES_REQUESTED": 0, "REVIEW_REQUIRED": 0,
              None: 0}
    checks = {"SUCCESS": 0, "FAILURE": 0, "PENDING": 0, "other": 0}
    drafts = conflicts = ready = 0
    for pr in prs:
        review[pr.get("reviewDecision") if pr.get("reviewDecision") in review
               else None] += 1
        state = rollup(pr)
        checks[state if state in checks else "other"] += 1
        drafts += 1 if pr.get("isDraft") else 0
        conflicts += 1 if pr.get("mergeable") == "CONFLICTING" else 0
        ready += 1 if ready_to_merge(pr) else 0

    rows.append(seg([(LBL, " ── STATE ── "), (TXT, "%d" % n),
                     (DIM, " shown · "), (DIM, "%d draft" % drafts),
                     (DIM, " · "),
                     (BAD if conflicts else DIM, "%d conflicting" % conflicts),
                     (DIM, " · "),
                     (OK if ready else DIM, "%d ready to merge" % ready)],
                    w - 1))
    order = [("APPROVED", OK), ("CHANGES_REQUESTED", BAD),
             ("REVIEW_REQUIRED", WARN), (None, DIM)]
    parts = [(review[k] / float(n), c) for k, c in order if review[k]]
    rows.append(seg([(RST, " ")] + stacked_bar(parts, max(10, w - 3)), w - 1))
    key = [(RST, " ")]
    for k, colour in order:
        if review[k]:
            key += [(colour, "▇ "), (TXT, REVIEW_LABEL[k][0]),
                    (DIM, " %d   " % review[k])]
    for k, colour, label in (("SUCCESS", OK, "checks pass"),
                             ("FAILURE", BAD, "checks FAIL"),
                             ("PENDING", WARN, "running")):
        if checks[k]:
            key += [(colour, "· "), (TXT, label), (DIM, " %d   " % checks[k])]
    rows.append(seg(key, w - 1))

    ages = sorted((hours_since(p.get("createdAt")), p) for p in prs
                  if hours_since(p.get("createdAt")) is not None)
    idles = [(hours_since(p.get("updatedAt")), p) for p in prs
             if hours_since(p.get("updatedAt")) is not None]

    # ── when the open ones arrived ──────────────────────────────────────
    today = datetime.date.today()
    days = [(today - datetime.timedelta(days=k)).isoformat()
            for k in range(OPENED_DAYS - 1, -1, -1)]
    per_day = dict((d, 0) for d in days)
    inside = 0
    for pr in prs:
        key = (pr.get("createdAt") or "")[:10]
        if key in per_day:
            per_day[key] += 1
            inside += 1
    avail = max(10, w - 3)
    slot = max(1, avail // len(days))
    gap = 1 if slot >= 3 else 0
    barw = slot - gap
    cols = []
    for i, d in enumerate(days):
        cols.extend([(per_day[d], PR)] * barw)
        if gap and i < len(days) - 1:
            cols.extend([(0, PR)] * gap)
    peak = max(per_day.values()) if per_day else 0
    rows.append("")
    rows.append(seg([(LBL, " ── OPENED / DAY ── "),
                     (DIM, "last %dd · " % OPENED_DAYS),
                     (TXT, "%d" % inside), (DIM, " of %d still open · " % n),
                     (DIM, "peak %d/day" % peak)], w - 1))
    if peak:
        for line in vbars(cols, 3):
            rows.append(seg([(RST, " ")] + line, w - 1))
        rows.append(seg([(RST, " "), (GRID, "─" * len(cols))], w - 1))
        left = "%dd ago" % OPENED_DAYS
        rows.append(seg([(DIM, " " + left),
                         (DIM, " " * max(1, len(cols) - len(left) - 5)),
                         (DIM, "today")], w - 1))
    else:
        rows.append(seg([(DIM, "  none of the open PRs were opened in the "
                               "last %dd" % OPENED_DAYS)], w - 1))

    # ── how old they are ────────────────────────────────────────────────
    def at(pairs, frac):
        if not pairs:
            return None
        vals = sorted(x[0] for x in pairs)
        return vals[min(len(vals) - 1, int(len(vals) * frac))]

    def worst(pairs, colour):
        if not pairs:
            return ("--", DIM)
        hours, pr = max(pairs, key=lambda x: x[0])
        return ("#%d %s" % (pr["number"], span(hours)), colour)

    rows.append("")
    rows.append(seg([(LBL, " ── AGE ── "),
                     (DIM, "median "), (TXT, span(at(ages, 0.5))),
                     (DIM, "  p95 "), (TXT, span(at(ages, 0.95))),
                     (DIM, "  max "), (WARN, span(at(ages, 1.0))),
                     (DIM, "   idle median "), (TXT, span(at(idles, 0.5)))],
                    w - 1))
    if ages:
        # One bar per open PR, youngest left to oldest right - the x axis is
        # rank, not time. It fills the pane and carries a baseline and end
        # labels, because a sparkline that stops in the middle of the screen
        # gives no way to tell where the chart ends and the blank begins.
        room = max(10, w - 3)
        drawn = ages[-room:] if len(ages) > room else ages
        # spread the remainder across the leftmost bars so the chart reaches
        # the right edge exactly: stopping short of it left no way to tell a
        # finished chart from a truncated one
        if len(drawn) >= room:
            slot, extra = 1, 0
        else:
            slot, extra = divmod(room, len(drawn))
        hi = max(x[0] for x in drawn) or 1
        bars = []
        for i, (hours, _pr) in enumerate(drawn):
            wide_bar = slot + (1 if i < extra else 0)
            bars.extend([SPARK[min(7, int(hours / hi * 7.99))]] * wide_bar)
        rows.append(seg([(RST, " "), (heat(0.4), "".join(bars))], w - 1))
        rows.append(seg([(RST, " "), (GRID, "─" * len(bars))], w - 1))
        left = "youngest %s" % span(drawn[0][0])
        right = "oldest %s" % span(drawn[-1][0])
        note = ("%d of %d PRs" % (len(drawn), len(ages))
                if len(drawn) < len(ages) else "%d PRs" % len(drawn))
        mid = max(1, len(bars) - len(left) - len(right) - len(note) - 2)
        rows.append(seg([(DIM, " " + left),
                         (DIM, " " * (mid // 2)), (GRID, note),
                         (DIM, " " * (mid - mid // 2 + 2)),
                         (DIM, right)], w - 1))
    fattest = max(prs, key=lambda p: (p.get("additions") or 0)
                  + (p.get("deletions") or 0))
    old_txt, old_col = worst(ages, WARN)
    idle_txt, idle_col = worst(idles, WARN)
    rows.append(seg([(DIM, "  oldest "), (old_col, pad(old_txt, 12)),
                     (DIM, "  untouched longest "), (idle_col, pad(idle_txt, 12)),
                     (DIM, "  biggest "),
                     (TXT, "#%d +%d/-%d" % (fattest["number"],
                                            fattest["additions"],
                                            fattest["deletions"]))], w - 1))
    return rows


def sort_prs(prs, field, newest_first):
    key = "updatedAt" if field == "updated" else "createdAt"
    return sorted(prs, key=lambda p: p.get(key) or "", reverse=newest_first)


def matches(pr, needle):
    if not needle:
        return True
    hay = " ".join([
        str(pr.get("number") or ""), pr.get("title") or "",
        (pr.get("author") or {}).get("login") or "",
        (pr.get("repository") or {}).get("nameWithOwner") or "",
        pr.get("headRefName") or "", pr.get("baseRefName") or "",
    ]).lower()
    return needle.lower() in hay


def main():
    maybe_help(__doc__)
    args = sys.argv[1:]
    while args and args[0] in ("-n", "--refresh"):
        global REFRESH
        REFRESH = float(args[1])
        args = args[2:]
    store = Store(args)
    threading.Thread(target=store.run, daemon=True).start()
    setup()
    keyboard = Keyboard()
    selected, tick, first = 0, 0, 0
    sort_field, newest_first = SORTS[0], True
    needle, typing = "", False
    show_stats = True
    stack_sel = 0
    copied, copied_at = "", 0.0
    source_filter = "all"

    while True:
        tick += 1
        (prs, total, detail, stack_rows, loading, err, fetched,
         active_query) = store.snapshot()
        shown = [p for p in sort_prs(prs, sort_field, newest_first)
                 if matches(p, needle)
                 and (source_filter == "all"
                      or source_filter in (p.get("sources") or []))]

        for key in keyboard.poll():
            if typing:
                # while filtering, keys are text - only escape and enter are
                # navigation, or the filter could never contain "q"
                if key == "esc":
                    needle, typing = "", False
                elif key == "enter":
                    typing = False
                elif key == "backspace":
                    needle = needle[:-1]
                elif len(key) == 1 and key.isprintable():
                    needle += key
                continue
            if key in ("q", "Q"):
                raise SystemExit(0)
            if key == "/":
                typing = True
            elif key == "esc":
                if detail or loading:
                    store.close_detail()
                    stack_sel = 0
                else:
                    needle = ""
            elif key == "enter":
                if detail and stack_rows:
                    # walk the stack from inside the stack: the row under the
                    # cursor becomes the PR on screen
                    node = stack_rows[min(stack_sel,
                                          len(stack_rows) - 1)][1]
                    owner_repo = "/".join(
                        (detail.get("url") or "").split("/")[3:5])
                    if owner_repo and node["number"] != detail["number"]:
                        store.open_detail({"repository":
                                           {"nameWithOwner": owner_repo},
                                           "number": node["number"]})
                        stack_sel = 0
                elif shown and not detail and not loading:
                    store.open_detail(shown[min(selected, len(shown) - 1)])
            elif key == "c":
                # the URL of whatever is on screen: the open PR in the
                # dashboard, the highlighted row in the list
                target = detail if detail else (
                    shown[min(selected, len(shown) - 1)] if shown else None)
                url = (target or {}).get("url")
                if url:
                    copied = url if clipboard(url) else "no clipboard: " + url
                    copied_at = time.time()
            elif key == "r":
                store.wake.set()
            elif key == "f":
                # every PR remembers which sources found it, so narrowing to
                # one is instant and costs no request
                names = ["all"] + list(_CFG["sources"].keys())
                source_filter = cycle(names, source_filter)
            elif key == "s":
                sort_field = cycle(SORTS, sort_field)
            elif key == "o":
                newest_first = not newest_first
            elif key == "t":
                show_stats = not show_stats
            elif key == "up":
                if detail:
                    stack_sel = max(0, stack_sel - 1)
                else:
                    selected = max(0, selected - 1)
            elif key == "down":
                if detail:
                    stack_sel += 1
                else:
                    selected += 1

        w, h = size()
        rows = [title("pr watch", w, PR)]
        head = [(DIM, " %d of %d" % (len(shown), total)),
                (DIM, " shown" if needle or source_filter != "all" else " open"),
                (DIM, "   updated %s ago" % ago(
                    datetime.datetime.fromtimestamp(fetched,
                                                    datetime.timezone.utc)
                    if fetched else None))]
        if _RATE["remaining"] is not None:
            head.append((DIM, "   %d api" % _RATE["remaining"]))
        if copied and time.time() - copied_at < 4:
            head.append((OK, "   copied "))
            head.append((DIM, copied[:max(10, w - 46)]))
        rows.append(seg(head, w - 1))
        if err:
            rows.append(seg([(BAD, " ! " + err)], w - 1))

        if detail or loading:
            if stack_rows:
                stack_sel = max(0, min(stack_sel, len(stack_rows) - 1))
            rows += detail_view(detail, stack_rows, stack_sel, loading, w, h,
                                tick)
            hints = [[(DIM, "[c]opy url")], [(DIM, "[esc] back")],
                     [(DIM, "[r]efresh")], [(DIM, "[q]uit")]]
            if stack_rows:
                hints = ([[(ACCENT, "↑↓"), (DIM, " stack")],
                          [(DIM, "[↵] open it")]] + hints)
        else:
            selected = max(0, min(selected, len(shown) - 1)) if shown else 0
            # the stats cost eight rows; below thirty they would leave the
            # list too short to be a list, so they stand down without asking
            if show_stats and h >= 30:
                rows += stats_view(shown, w)
            rows += list_view(shown, selected, sort_field, newest_first,
                              needle, typing, w, h, tick, not fetched,
                              source_filter)
            hints = [[(ACCENT, "↑↓"), (DIM, " select")], [(DIM, "[↵] open")],
                     [(DIM, "[/]filter")],
                     [(DIM, "[s]ort %s" % sort_field)],
                     [(DIM, "[o]rder %s" % ("newest" if newest_first
                                            else "oldest"))],
                     [(DIM, "[f]rom %s" % source_filter)],
                     [(DIM, "[t]stats %s" % ("on" if show_stats else "off"))],
                     [(DIM, "[c]opy url")], [(DIM, "[r]efresh")],
                     [(DIM, "[q]uit")]]
        if typing:
            hints = [[(ACCENT, "/" + needle + "▌")],
                     [(DIM, "[↵] keep")], [(DIM, "[esc] clear")]]
        footer = [" " + line for line in pack_hints(hints, w - 2)]
        rows = rows[:h - len(footer)]
        while len(rows) < h - len(footer):
            rows.append("")
        rows.extend(footer)
        draw(rows, w, h)
        time.sleep(0.3)


def list_view(prs, selected, sort_field, newest_first, needle, typing,
              w, h, tick, waiting, source_filter="all"):
    rows = [""]
    arrow = "↓" if newest_first else "↑"
    rows.append(seg([(LBL, " ── OPEN PRs ── "),
                     (DIM, "by %s %s" % (sort_field, arrow)),
                     (DIM, "   from %s" % source_filter
                      if source_filter != "all" else ""),
                     (ACCENT, "   /%s" % needle if needle else "")], w - 1))
    if not prs:
        # "collecting" is only true before the first fetch: an empty filter
        # or an empty source is a result, not a wait
        if needle:
            why = "  nothing matches /%s" % needle
        elif source_filter != "all":
            why = "  no open PRs from %s" % source_filter
        elif waiting:
            why = "  collecting…"
        else:
            why = "  no open PRs"
        rows.append(seg([(DIM, why)], w - 1))
        return rows

    # Columns are budgeted rather than guessed: the fixed ones are summed and
    # the title takes exactly what is left, so nothing runs off the right edge
    # or into its neighbour.
    wide = w >= 96
    repo_w = 18 if wide else 0
    size_w = 12 if wide else 0
    fixed = 8 + repo_w + 13 + 8 + 6 + size_w
    title_w = max(16, w - 1 - fixed)
    head = " %-7s" % "PR"
    if repo_w:
        head += "%-*s" % (repo_w, "REPO")
    # the time column follows the sort, so the number you ordered by is the
    # number you can see. Labelling both "AGE" had it reporting idle time
    # while the stats above reported true age, and the two disagreed.
    when_label = "AGE" if sort_field == "created" else "IDLE"
    head += "%-*s%13s%8s%6s" % (title_w, "TITLE", "REVIEW", "CHECKS",
                                when_label)
    if size_w:
        head += "%*s" % (size_w, "SIZE")
    rows.append(DIM + pad(head, w - 1))

    room = max(1, h - len(rows) - 3)
    first = 0
    if len(prs) > room:
        first = min(max(0, selected - room // 2), len(prs) - room)
    for i, pr in list(enumerate(prs))[first:first + room]:
        here = i == selected
        tint = bg(38, 56, 76) if here else ""
        rlabel, rcol = REVIEW_LABEL.get(pr.get("reviewDecision"),
                                        REVIEW_LABEL[None])
        clabel, ccol = CHECK_LABEL.get(rollup(pr), CHECK_LABEL[None])
        stacked = bool(pr.get("stackEntry"))
        line = [(tint + (ACCENT if here else PR),
                 ("▸" if here else " ") + pad("#%d" % pr["number"], 7))]
        if repo_w:
            # clip one short of the column so it never touches the title
            repo = pr["repository"]["nameWithOwner"].split("/")[-1]
            line.append((tint + DIM, pad(repo[:repo_w - 1], repo_w)))
        name = ("⣿ " if stacked else "") + (pr.get("title") or "")
        if pr.get("isDraft"):
            name = "draft · " + name
        line += [(tint + TXT, pad(name[:title_w - 1], title_w)),
                 (tint + rcol, "%13s" % rlabel),
                 (tint + ccol, "%8s" % clabel),
                 (tint + DIM, "%6s" % ago(pr.get(
                     "createdAt" if sort_field == "created"
                     else "updatedAt")))]
        if size_w:
            line.append((tint + DIM, "%*s" % (size_w, "+%d/-%d"
                                              % (pr["additions"],
                                                 pr["deletions"]))))
        if here:
            line.append((tint, " " * w))
        rows.append(seg(line, w - 1))
    return rows


def detail_view(pr, stack_rows, stack_sel, loading, w, h, tick):
    rows = [""]
    if loading or not pr:
        rows.append(seg([(LBL, " ── PULL REQUEST ── "), (DIM, "opening…")],
                        w - 1))
        for _ in range(4):
            rows.append(seg([(RST, " ")] + skeleton(max(10, w - 4), tick),
                            w - 1))
        return rows

    draft = " · draft" if pr.get("isDraft") else ""
    rows.append(seg([(PR, " #%d " % pr["number"]),
                     (TXT, (pr.get("title") or "")[:max(10, w - 24)]),
                     (DIM, draft)], w - 1))
    rows.append(seg([(DIM, "  "), (DIM, (pr.get("author") or {}).get("login")
                                  or "?"),
                     (DIM, "   "), (ACCENT, pr.get("headRefName") or "?"),
                     (DIM, " → "), (ACCENT, pr.get("baseRefName") or "?")],
                    w - 1))

    rlabel, rcol = REVIEW_LABEL.get(pr.get("reviewDecision"),
                                    REVIEW_LABEL[None])
    mlabel, mcol = MERGE_LABEL.get(pr.get("mergeStateStatus"),
                                   MERGE_LABEL[None])
    threads = (pr.get("reviewThreads") or {}).get("nodes") or []
    unresolved = sum(1 for t in threads if not t.get("isResolved"))
    rows.append("")
    cells = [("review", rlabel, rcol),
             ("merge", mlabel, mcol),
             ("unresolved threads", str(unresolved),
              BAD if unresolved else OK),
             ("size", "+%d/-%d in %d files" % (pr["additions"], pr["deletions"],
                                               pr["changedFiles"]), TXT),
             ("commits", str((pr.get("commitCount") or {}).get("totalCount")
                             or 0), TXT),
             ("opened / updated", "%s ago / %s ago" % (ago(pr.get("createdAt")),
                                                       ago(pr.get("updatedAt"))),
              TXT)]
    label_w = max(len(c[0]) for c in cells)
    ncols = 2 if (w - 2) // 2 - label_w - 3 >= 18 else 1
    cw = (w - 2) // ncols
    val_w = max(6, cw - label_w - 3)
    for n in range(0, len(cells), ncols):
        line = [(RST, " ")]
        for label, value, colour in cells[n:n + ncols]:
            line += [(DIM, " " + pad(label, label_w) + " "),
                     (colour, pad(value, val_w))]
        rows.append(seg(line, w - 1))

    # ── who has looked at it ─────────────────────────────────────────────
    # last state per person wins: someone who requested changes and later
    # approved has approved, and showing both would misreport the gate
    latest = {}
    for r in (pr.get("reviews") or {}).get("nodes") or []:
        who = (r.get("author") or {}).get("login")
        if who:
            latest[who] = r.get("state")
    pending = []
    for n in (pr.get("reviewRequests") or {}).get("nodes") or []:
        who = n.get("requestedReviewer") or {}
        name = who.get("login") or who.get("name")
        if name and name not in latest:
            pending.append(name)
    groups = [("approved", OK, [k for k, v in latest.items() if v == "APPROVED"]),
              ("changes requested", BAD,
               [k for k, v in latest.items() if v == "CHANGES_REQUESTED"]),
              ("commented", DIM,
               [k for k, v in latest.items() if v == "COMMENTED"]),
              ("awaiting", WARN, pending)]
    rows.append("")
    live = [g for g in groups if g[2]]
    rows.append(seg([(LBL, " ── REVIEWERS ── "),
                     (DIM, "nobody has been asked" if not live
                      else " · ".join("%d %s" % (len(g[2]), g[0])
                                      for g in live))], w - 1))
    for label, colour, who in live:
        rows.append(seg([(DIM, "  " + pad(label, 18)),
                         (colour, ", ".join(sorted(who))[:max(10, w - 24)])],
                        w - 1))

    # ── checks ───────────────────────────────────────────────────────────
    node = ((pr.get("commits") or {}).get("nodes") or [{}])
    roll = ((node[0].get("commit") or {}).get("statusCheckRollup")
            if node else None)
    rows.append("")
    if not roll:
        rows.append(seg([(LBL, " ── CHECKS ── "),
                         (DIM, "none on the last commit")], w - 1))
    else:
        state, scol = CHECK_LABEL.get(roll.get("state"), CHECK_LABEL[None])
        ctx = [c for c in (roll.get("contexts") or {}).get("nodes") or [] if c]
        rows.append(seg([(LBL, " ── CHECKS ── "), (scol, state),
                         (DIM, "   %d total" % len(ctx))], w - 1))
        bad = [c for c in ctx if (c.get("conclusion") or c.get("state"))
               not in ("SUCCESS", "NEUTRAL", "SKIPPED", None)]
        # failures first: a green wall of passing checks is not why anyone
        # opens this view
        for c in (bad + [c for c in ctx if c not in bad])[:8]:
            name = c.get("name") or c.get("context") or "?"
            verdict = c.get("conclusion") or c.get("state") or c.get("status")
            lab, col = CHECK_LABEL.get(verdict, (str(verdict or "—").lower(),
                                                 DIM))
            took = ""
            a, b = parse(c.get("startedAt")), parse(c.get("completedAt"))
            if a and b:
                took = "%ds" % int((b - a).total_seconds())
            rows.append(seg([(DIM, "  "), (TXT, pad(name, max(12, w - 30))),
                             (col, "%10s" % lab), (DIM, "%8s" % took)], w - 1))

    # ── the stack, when there is one ─────────────────────────────────────
    if stack_rows:
        native = bool(pr.get("stack"))
        rows.append("")
        # the stack scrolls: eleven-deep stacks exist, and a pane that has
        # already spent its height on checks cannot show them all
        room = max(3, h - len(rows) - 6)
        first = 0
        if len(stack_rows) > room:
            first = min(max(0, stack_sel - room // 2), len(stack_rows) - room)
        rows.append(seg([(LBL, " ── STACK ── "),
                         (DIM, "%d pull requests · %s" % (
                             len(stack_rows),
                             "from GitHub" if native
                             else "inferred from branches")),
                         (ACCENT, "   ↑↓ %d-%d of %d"
                          % (first + 1, min(first + room, len(stack_rows)),
                             len(stack_rows))
                          if len(stack_rows) > room else "")], w - 1))
        rows.append(seg([(DIM, "  merge bottom-up: "),
                         (TXT, "the one nearest the base branch first"),
                         (DIM, "   ▸ cursor · ● on screen")], w - 1))
        base = pr.get("baseRefName") if not native else (
            pr["stack"].get("baseRefName") or "")
        rows.append(seg([(DIM, "  "), (ACCENT, base or "trunk")], w - 1))
        for idx, (twig, node, is_here, position) in list(
                enumerate(stack_rows))[first:first + room]:
            lab, col = REVIEW_LABEL.get(node.get("reviewDecision"),
                                        REVIEW_LABEL[None])
            merge = node.get("mergeable")
            mlab, mcol = (("CONFLICT", BAD) if merge == "CONFLICTING"
                          else ("ok", OK) if merge == "MERGEABLE"
                          else ("…", DIM))
            on_cursor = idx == stack_sel
            tint = bg(38, 56, 76) if on_cursor else ""
            name = (node.get("title") or "")
            if position:
                name = "%d. %s" % (position, name)
            # two gutter marks, because they answer different questions:
            # ▸ is where the cursor is, ● is the PR actually on screen. One
            # symbol plus a colour could not say both.
            gutter = ("▸" if on_cursor else " ") + ("●" if is_here else " ")
            rows.append(seg([
                (tint + (ACCENT if on_cursor else DIM), gutter),
                (tint + DIM, twig),
                (tint + (ACCENT if on_cursor else PR),
                 "#%-5d " % node["number"]),
                (tint + (TXT if (is_here or on_cursor) else DIM),
                 pad(name[:max(10, w - 34 - len(twig))],
                     max(10, w - 34 - len(twig)))),
                (tint + col, "%13s" % lab),
                (tint + mcol, "%10s" % mlab),
            ] + ([(tint, " " * w)] if on_cursor else []), w - 1))
    return rows


main()
