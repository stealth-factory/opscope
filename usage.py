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

Most of this is read from local state files. The exception is the remaining
quota, which no agent writes to disk in a current form: Claude, Codex, Cursor
and Copilot each publish one over an endpoint, fetched with the credential
that agent already holds and sent only to that agent's own host. Nothing here is
inferred from a number that was not published.

Keys: left/right or tab switch agent, up/down scroll it, pgup/pgdn by
the page, home/end to either edge, r refreshes now, q quits.
"""
import datetime
import json
import os
import glob
import re
import shutil
import sqlite3
import sys
import urllib.error
import urllib.request
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, bg, draw, heat, load_config, maybe_help, mix,
                    meter, pack_hints, pad, rgb, seg, setup, size, stacked_bar,
                    title, vbars)

_CFG = load_config("usage", {
    # Empty discovers whatever this machine has, which is the default and
    # what most people want. Naming agents instead pins the set and its order
    # - listing one is how you say "keep the tab even though it is not
    # installed yet", and it is also how you turn discovery off. The same
    # empty-means-discover idiom as github.accounts and linear.exclude_teams.
    "agents": [],
    "exclude_agents": [],
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

# One hue, four steps, the way /stats and the contribution calendar do it.
# heat() runs green to amber to red, which reads as a change of *kind* rather
# than of amount - wrong for "more of the same thing".
# Claude keeps the terracotta of its own /stats. Codex gets a white ramp, so
# two calendars side by side are told apart by hue rather than by reading the
# heading - the steps are the same four, only the colour differs.
HEAT_STEPS = ((74, 52, 46), (140, 78, 58), (196, 100, 66), (240, 132, 84))
CODEX_STEPS = ((66, 72, 82), (122, 130, 144), (182, 190, 202), (240, 244, 250))
GROK_STEPS = ((44, 62, 88), (62, 104, 156), (86, 150, 210), (120, 196, 250))
EMPTY_CELL = rgb(58, 66, 80)


def shade(frac, steps=HEAT_STEPS):
    """Which of the four steps a day falls in."""
    return rgb(*steps[min(3, max(0, int(frac * 3.999)))])


CLAUDE_STATS = os.path.expanduser("~/.claude/stats-cache.json")
CLAUDE_CREDS = os.path.expanduser("~/.claude/.credentials.json")
CLAUDE_CONFIG = os.path.expanduser("~/.claude.json")
CLAUDE_USAGE_API = "https://api.anthropic.com/api/oauth/usage"
CLAUDE_PROFILE_API = "https://api.anthropic.com/api/oauth/profile"
# a plan does not change between refreshes; the windows do
PLAN_TTL = 3600
# The windows are not stated in limits[], but the same response names them in
# its own top-level keys: five_hour and seven_day.
CLAUDE_WINDOW = {"session": "5h", "weekly": "7d"}
CODEX_SESSIONS = os.path.expanduser("~/.codex/sessions/**/*.jsonl")
CLAUDE_TRANSCRIPTS = os.path.expanduser("~/.claude/projects/*/*.jsonl")
RATE_FILES = 3           # newest transcripts to sample for a rate
MIN_GAP = 1.0            # seconds; below this the timestamps are not a turn
COPILOT_DB = os.path.expanduser("~/.copilot/session-store.db")
COPILOT_CONFIG = os.path.expanduser("~/.copilot/config.json")
COPILOT_USER_API = "https://api.github.com/copilot_internal/user"
TAIL = 256 * 1024        # enough to reach the last token_count in a rollout
CURSOR_DB = os.path.expanduser("~/.cursor/ai-tracking/ai-code-tracking.db")
# One hue per lane, as cursor-agent's own Usage view does it. These are
# categories rather than one gauge, so a green-to-red severity ramp would
# imply a relationship between them that does not exist - and each bar is
# labelled and carries its own percentage, so the colour is decoration.
CURSOR_LANES = (("included", (126, 208, 176)),
                ("auto", (138, 168, 204)),
                ("api", (217, 160, 192)))
GROK_SESSIONS = os.path.expanduser("~/.grok/sessions/**/updates.jsonl")
# the quota is not in the session transcripts: it arrives on the client log
GROK_LOG = os.path.expanduser("~/.grok/logs/unified.jsonl")
CURSOR_AUTH = os.path.expanduser("~/.config/cursor/auth.json")
CURSOR_RPC = "https://api2.cursor.sh/aiserver.v1.DashboardService/%s"
CURSOR_USAGE_API = CURSOR_RPC % "GetCurrentPeriodUsage"


def span_ms(ms):
    """A duration in milliseconds as days, hours and minutes."""
    s = int((ms or 0) / 1000)
    d, s = divmod(s, 86400)
    h, s = divmod(s, 3600)
    m = s // 60
    if d:
        return "%dd %dh %dm" % (d, h, m)
    return "%dh %dm" % (h, m) if h else "%dm" % m


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
    if s < 365 * 86400:
        return "%dd" % int(s // 86400)
    # a subscription can be years old, and "890d" is not a span anyone reads
    return "%.1fy" % (s / (365.0 * 86400))


def claude_live():
    """Rate-limit utilization, live from the endpoint /usage itself calls.

    The OAuth token is the one Claude Code already holds. It goes only to
    Anthropic, is never printed, and an expired one is not used at all: the
    refresh token sits beside it, but spending it would race Claude Code's
    own credential handling for a number that has a local cache anyway.
    """
    try:
        with open(CLAUDE_CREDS) as f:
            o = (json.load(f) or {}).get("claudeAiOauth") or {}
    except (OSError, ValueError):
        return None
    tok = o.get("accessToken")
    if not tok or (o.get("expiresAt") or 0) / 1000.0 <= time.time():
        return None
    req = urllib.request.Request(CLAUDE_USAGE_API, headers={
        "Authorization": "Bearer " + tok, "User-Agent": "terminal-toys"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return {"u": json.load(r), "source": "live", "at": time.time(),
                    "plan": o.get("subscriptionType") or ""}
    except (urllib.error.URLError, ValueError, OSError):
        return None


def claude_profile():
    """Which subscription the windows belong to.

    A separate endpoint from the usage one, and near-static, so it is held
    far longer - a plan does not change between refreshes, and the usage
    endpoint answers 429 if these are called at usage cadence.
    """
    try:
        with open(CLAUDE_CREDS) as f:
            o = (json.load(f) or {}).get("claudeAiOauth") or {}
    except (OSError, ValueError):
        return None
    tok = o.get("accessToken")
    if not tok or (o.get("expiresAt") or 0) / 1000.0 <= time.time():
        return None
    req = urllib.request.Request(CLAUDE_PROFILE_API, headers={
        "Authorization": "Bearer " + tok, "User-Agent": "terminal-toys"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            d = json.load(r)
    except (urllib.error.URLError, ValueError, OSError):
        return None
    d["_plan"] = o.get("subscriptionType") or ""
    return d


def claude_creds_plan():
    """What the credentials file alone can say about the plan.

    The profile endpoint is richer, but this needs no network and is always
    there, so the section degrades to two true lines instead of vanishing.
    """
    try:
        with open(CLAUDE_CREDS) as f:
            o = (json.load(f) or {}).get("claudeAiOauth") or {}
    except (OSError, ValueError):
        return None
    if not o.get("subscriptionType"):
        return None
    return {"_plan": o["subscriptionType"], "_local": True,
            "organization": {"rate_limit_tier": o.get("rateLimitTier") or ""}}


def claude_plan_rows(prof, w):
    org = (prof.get("organization") or {})
    pairs = []
    since = iso_epoch(org.get("subscription_created_at") or "")
    day = iso_day(org.get("subscription_created_at") or "")
    if since and day:
        pairs.append(("member since", "%s · %s ago" % (day, ago(since))))
    if org.get("subscription_status"):
        pairs.append(("status", org["subscription_status"]))
    if org.get("rate_limit_tier"):
        pairs.append(("rate limit tier", org["rate_limit_tier"]))
    if org.get("billing_type"):
        pairs.append(("billing", org["billing_type"].replace("_", " ")))
    return plan_rows(prof.get("_plan") or org.get("organization_type"),
                     pairs, w,
                     note="from credentials" if prof.get("_local") else "")


def claude_stale():
    """What Claude Code last fetched, for when the live call cannot run.

    It is a cache with a timestamp, so it is shown with its age - and a
    window whose reset has already gone by is said to have passed rather
    than counted down to, because a stale five-hour window describes a
    period that has ended.
    """
    try:
        with open(CLAUDE_CONFIG) as f:
            c = (json.load(f) or {}).get("cachedUsageUtilization") or {}
    except (OSError, ValueError):
        return None
    if not c.get("utilization"):
        return None
    return {"u": c["utilization"], "source": "cached",
            "at": (c.get("fetchedAtMs") or 0) / 1000.0, "plan": ""}


def read_claude():
    """Claude Code's own stats cache, plus what is left of the limits.

    The cache records what was spent - tokens, messages, sessions - and
    carries no limit at all. The remaining quota is a separate reading
    entirely, and account-wide rather than about this machine.
    """
    quota = cached("claude", lambda: claude_live() or claude_stale())
    profile = (cached("claude-plan", claude_profile, ttl=PLAN_TTL)
               or claude_creds_plan())
    try:
        stat = os.stat(CLAUDE_STATS)
        with open(CLAUDE_STATS) as f:
            d = json.load(f)
    except (OSError, ValueError) as e:
        return {"ok": False, "why": "%s" % type(e).__name__,
                "quota": quota, "profile": profile}
    rates, sampled = claude_rates()
    return {"ok": True, "mtime": stat.st_mtime, "data": d, "quota": quota,
            "profile": profile, "rates": rates, "sampled": sampled}


def iso_epoch(s):
    """ISO-8601 to epoch seconds.

    fromisoformat rejects a trailing Z before Python 3.11, and these APIs mix
    Z with +00:00 in the same response. A value carrying no zone at all is
    read as local time, which is why the callers here pass zoned strings.
    """
    try:
        return datetime.datetime.fromisoformat(
            re.sub(r"Z$", "+00:00", s)).timestamp()
    except (ValueError, TypeError, AttributeError):
        return None


def iso_day(s):
    """A date, kept in the zone it arrived in.

    assigned_date carries the account's own offset. Converting it to this
    machine's zone can move it a day - 3 Jun at 12:10 -07:00 is 4 Jun in UTC -
    and then the pane disagrees with what GitHub shows for the same seat.
    """
    try:
        d = datetime.datetime.fromisoformat(re.sub(r"Z$", "+00:00", s))
    except (ValueError, TypeError, AttributeError):
        return ""
    return d.strftime("%-d %b %Y")


def scope_phrase(w, used):
    """The scope note, shortened before it can push a reset off the line.

    "not this machine" is the point of the sentence, but it is also sixteen
    characters, and seg() clips whatever runs past the pane. Losing the
    clause leaves a shorter true line; losing the end of "resets in 15d"
    leaves "resets in 1", which is a different and wrong number.
    """
    full = " · account-wide, not this machine   "
    return full if used + len(full) <= w - 1 else " · account-wide   "


def quota_window(reset_ts):
    """The span a monthly quota covers, worked back from its reset.

    Copilot states when the quota resets but never how long the window is.
    It can be derived, but only safely when the reset lands on midnight UTC
    on the first of a month - which is what a calendar-month cycle looks
    like, and what this account shows. Anything else and the window is not
    known, so nothing is claimed about it.
    """
    if not reset_ts:
        return None
    end = datetime.datetime.fromtimestamp(reset_ts, datetime.timezone.utc)
    if (end.day, end.hour, end.minute, end.second) != (1, 0, 0, 0):
        return None
    start = (end.replace(year=end.year - 1, month=12) if end.month == 1
             else end.replace(month=end.month - 1))
    return "%s → %s" % (start.strftime("%-d %b"), end.strftime("%-d %b"))


def left_span(secs):
    d, r = divmod(int(secs), 86400)
    h, r = divmod(r, 3600)
    if d:
        return "%dd %dh" % (d, h)
    return "%dh %dm" % (h, r // 60) if h else "%dm" % (r // 60)


def claude_quota(q, w):
    """The windows Claude Code's own /usage shows.

    Read from limits[], which is the server's own curated list: the rest of
    the response carries a dozen null pools with names like nimbus_quill and
    iguana_necktie that /usage does not render either. Each entry names
    itself, so a model-scoped weekly limit arrives labelled Fable or Opus
    without this having to know either name.
    """
    if not q:
        return []
    u = q.get("u") or {}
    lanes = [l for l in (u.get("limits") or []) if l.get("percent") is not None]
    if not lanes:
        return []
    live = q.get("source") == "live"
    src = "live" if live else "cached %s ago" % ago(q.get("at"))
    plan = q.get("plan") or ""
    rows = [seg([(LBL, " ── QUOTA ── "), (OK if live else WARN, src),
                 (DIM, scope_phrase(w, 13 + len(src) + len(plan))),
                 (DIM, plan)], w - 1)]

    def label(l):
        scope = ((l.get("scope") or {}).get("model") or {}).get("display_name")
        group = l.get("group") or ""
        name = scope or ("overall" if l.get("kind") == "weekly_all"
                         else group or l.get("kind") or "?")
        return ("%s %s" % (name, CLAUDE_WINDOW.get(group, ""))).strip()

    texts = [label(l) for l in lanes]
    label_w = max(9, max(len(t) for t in texts))
    for l, text in zip(lanes, texts):
        pct = float(l.get("percent") or 0)
        used = max(0.0, min(1.0, pct / 100.0))
        ts = iso_epoch(l.get("resets_at"))
        when = ""
        if ts:
            left = ts - time.time()
            when = ("resets in " + left_span(left) if left > 0
                    else "resetting" if live else "already reset")
        sev = (l.get("severity") or "normal").lower()
        bar = meter(used, max(8, w - 27 - label_w))
        filled = bar.count("█")
        # is_active marks the limit currently doing the binding - the one that
        # will stop you first - so it is the one worth reading brightly.
        rows.append(seg([(TXT if l.get("is_active") else DIM,
                          " " + pad(text, label_w) + " "),
                         (heat(used), bar[:filled]), (GRID, bar[filled:]),
                         (heat(used), " %3.0f%%" % pct),
                         (BAD if sev not in ("normal", "") else DIM,
                          "  " + (when if sev in ("normal", "")
                                  else "%s · %s" % (sev, when)))], w - 1))
    extra = u.get("extra_usage") or {}
    spend = u.get("spend") or {}
    if extra.get("is_enabled") and spend.get("limit"):
        def money(m):
            return "%.2f" % ((m.get("amount_minor") or 0) /
                             (10.0 ** (m.get("exponent") or 2)))
        rows.append(seg([(DIM, "  extra usage "),
                         (TXT, money(spend.get("used") or {})),
                         (DIM, " of "), (TXT, money(spend["limit"])),
                         (DIM, " " + ((spend["limit"].get("currency")) or "")),
                         (DIM, " monthly")], w - 1))
    rows.append("")
    return rows


def claude_rates():
    """Output tokens per second, from the newest transcripts.

    A turn is a `user` record followed by an `assistant` one, and the rate is
    that assistant's output tokens over the gap between them. Measuring from
    *any* previous record instead inflates it wildly - two assistant records
    can be milliseconds apart while the second reports a whole turn's output -
    and even with the right boundary a few gaps are impossible, 1073 tokens in
    0.07s among them, where the timestamps plainly do not bracket generation.

    So the median is what gets shown. It sits at 74-75 whichever way the
    outliers are trimmed, which is the reason to trust it; the maximum moves
    from 15328 to 800 on the same data, which is the reason not to show one.
    """
    files = sorted(glob.glob(CLAUDE_TRANSCRIPTS), key=os.path.getmtime,
                   reverse=True)[:RATE_FILES]
    out, sampled = [], 0
    for path in files:
        prev, prev_type = None, None
        lines = tail_lines(path, 4 * 1024 * 1024)
        sampled += 1
        for line in lines:
            if '"timestamp"' not in line:
                continue
            try:
                d = json.loads(line)
            except ValueError:
                continue
            ts, typ = d.get("timestamp"), d.get("type")
            if (typ == "assistant" and ts and prev and prev_type == "user"
                    and not d.get("isAbortedMidStream")):
                tok = ((d.get("message") or {}).get("usage")
                       or {}).get("output_tokens") or 0
                if tok:
                    try:
                        a = datetime.datetime.fromisoformat(
                            prev.replace("Z", "+00:00"))
                        b = datetime.datetime.fromisoformat(
                            ts.replace("Z", "+00:00"))
                    except ValueError:
                        prev, prev_type = ts, typ
                        continue
                    gap = (b - a).total_seconds()
                    if MIN_GAP <= gap < 300:
                        out.append(tok / gap)
            if ts:
                prev, prev_type = ts, typ
    out.sort()
    return out, sampled


def cursor_live():
    """Plan usage, from the endpoint cursor-agent's own Usage view calls.

    Not the documented cursor.com dashboard API - that one wants a browser
    cookie and returns 401 to anything this machine has. The CLI talks Connect
    to aiserver.v1.DashboardService with the bearer token it stores in
    ~/.config/cursor/auth.json, which is the credential this reuses.

    Undocumented and versioned only by the CLI bundle it was read out of, so
    every failure is silent and the tab simply falls back to authorship.
    """
    try:
        with open(CURSOR_AUTH) as f:
            tok = json.load(f).get("accessToken")
    except (OSError, ValueError):
        return None
    if not tok:
        return None
    return cursor_rpc("GetCurrentPeriodUsage", {}, tok)


def cursor_token():
    try:
        with open(CURSOR_AUTH) as f:
            return json.load(f).get("accessToken")
    except (OSError, ValueError):
        return None


def cursor_rpc(method, body, tok=None):
    tok = tok or cursor_token()
    if not tok:
        return None
    req = urllib.request.Request(CURSOR_RPC % method,
                                 data=json.dumps(body).encode(), headers={
        "Authorization": "Bearer " + tok,
        "Content-Type": "application/json",
        "Connect-Protocol-Version": "1",
        "User-Agent": "terminal-toys"})
    try:
        with urllib.request.urlopen(req, timeout=25) as r:
            return json.load(r)
    except (urllib.error.URLError, ValueError, OSError):
        return None


def cursor_plan():
    """Which Cursor plan the percentages are percentages of.

    GetPlanInfo is where the plan's name and price live - the usage call
    carries neither, and its $400 limit is meaningless without knowing that
    is what Ultra includes. Found in the CLI bundle the same way the usage
    endpoint was.
    """
    return cursor_rpc("GetPlanInfo", {})


def cursor_plan_rows(info, w):
    plan = (info or {}).get("planInfo") or {}
    if not plan:
        return []
    pairs = []
    if plan.get("price"):
        pairs.append(("price", plan["price"]))
    inc = plan.get("includedAmountCents")
    if inc:
        pairs.append(("included", "$%.2f per cycle" % (int(inc) / 100.0)))
    owner = (plan.get("planOwner") or "").replace("PLAN_OWNER_", "").lower()
    if owner:
        pairs.append(("billing", owner))
    return plan_rows(plan.get("planName"), pairs, w)


def cursor_spend(days=30):
    """Per-model tokens and real cost over a window.

    This is what the plan percentages are made of: which model spent the
    money. `totalCents` is Cursor's own figure, not an estimate.
    """
    now = int(time.time() * 1000)
    return cursor_rpc("GetAggregatedUsageEvents",
                      {"startDate": str(now - days * 86400 * 1000),
                       "endDate": str(now)})


def read_cursor():
    """Cursor's AI code tracking: how much code it wrote, not what it cost."""
    if not os.path.exists(CURSOR_DB):
        return {"ok": False, "why": "no tracking database",
                "live": cached("cursor", cursor_live),
                "plan": cached("cursor-plan", cursor_plan, ttl=PLAN_TTL),
                "spend": cached("cursor-spend", cursor_spend)}
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
    return {"ok": True, "live": cached("cursor", cursor_live),
                "plan": cached("cursor-plan", cursor_plan, ttl=PLAN_TTL),
                "spend": cached("cursor-spend", cursor_spend),
            "hashes": rows[0], "conversations": rows[1],
            "models": rows[2], "by_model": by_model,
            "commits": commits[0] or 0, "lines": commits[1] or 0,
            "human_lines": commits[2] or 0, "last": recent}


def tail_lines(path, size=TAIL):
    """The last `size` bytes as lines, for files that run to tens of MB.

    A rollout carries its running total on every token_count event, so the
    newest one is all that is needed - reading 30MB per refresh to learn a
    number that is repeated at the end would be daft.
    """
    try:
        with open(path, "rb") as f:
            f.seek(0, 2)
            end = f.tell()
            f.seek(max(0, end - size))
            return f.read().decode("utf-8", "ignore").split("\n")
    except OSError:
        return []


CODEX_AUTH = os.path.expanduser("~/.codex/auth.json")
CODEX_USAGE_API = "https://chatgpt.com/backend-api/wham/usage"
_CODEX_CACHE = {}

LIVE_TTL = 120
_LIVE = {}


def cached(key, fn, ttl=LIVE_TTL):
    """Hold a reading for a while, but never hold a failure that long.

    The pane redraws every 30 seconds; these windows move over hours, and
    three tabs between them were making six requests a minute to say the
    same thing. A failure is cached too, so a dead endpoint is retried
    occasionally rather than on every frame - but only ever for the short
    interval, never the long one. A subscription is held an hour because it
    does not change; one transient 429 should not blank its section for an
    hour, which is exactly what happened here.
    """
    now = time.time()
    hit = _LIVE.get(key)
    if hit and now - hit[0] < hit[2]:
        return hit[1]
    val = fn()
    _LIVE[key] = (now, val, ttl if val else min(ttl, LIVE_TTL))
    return val


def codex_live():
    """Account-wide quota, live from the same endpoint the Codex CLI uses.

    The rollouts carry a rate_limits snapshot, but only from whenever Codex
    last ran - it can be days stale. This is the current figure, and it is the
    account rather than this machine.

    The token comes from ~/.codex/auth.json and goes to the same host Codex
    itself talks to; it is never printed. Any failure falls back to the
    snapshot, so an expired token costs nothing but freshness.

    Found by reading how CodexBar does it (github.com/steipete/CodexBar),
    which documents this endpoint.
    """
    try:
        with open(CODEX_AUTH) as f:
            auth = json.load(f)
    except (OSError, ValueError):
        return None
    tok = (auth.get("tokens") or {}).get("access_token") or auth.get("access_token")
    if not tok:
        return None
    req = urllib.request.Request(CODEX_USAGE_API, headers={
        "Authorization": "Bearer " + tok, "User-Agent": "terminal-toys"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return json.load(r)
    except (urllib.error.URLError, ValueError, OSError):
        return None


def scan_rollout(path):
    """Per-day tokens, the running total, and the newest quota snapshot.

    Cached on (mtime, size): a finished rollout never changes, and some run to
    30MB, so the full parse happens once per file rather than every refresh.
    """
    try:
        st = os.stat(path)
    except OSError:
        return None
    key = (st.st_mtime, st.st_size)
    hit = _CODEX_CACHE.get(path)
    if hit and hit[0] == key:
        return hit[1]
    daily = {}
    total, limits, limits_at = None, None, None
    try:
        with open(path, errors="ignore") as f:
            for line in f:
                if '"token_count"' not in line:
                    continue
                try:
                    d = json.loads(line)
                except ValueError:
                    continue
                info = (d.get("payload") or {}).get("info") or {}
                last = info.get("last_token_usage") or {}
                ts = d.get("timestamp") or ""
                if last.get("total_tokens") and ts[:10]:
                    daily[ts[:10]] = daily.get(ts[:10], 0) + last["total_tokens"]
                if info.get("total_token_usage"):
                    total = info["total_token_usage"]
                rl = d.get("payload", {}).get("rate_limits") or info.get("rate_limits")
                if rl:
                    limits, limits_at = rl, ts
    except OSError:
        return None
    out = {"daily": daily, "total": total, "limits": limits,
           "limits_at": limits_at, "mtime": st.st_mtime}
    _CODEX_CACHE[path] = (key, out)
    return out


def read_codex():
    """Codex rollouts carry token_count events with running and per-turn use.

    ~/.codex/logs is diagnostics and has no counters, which is where an
    earlier look stopped. The sessions directory is the one that counts.
    """
    files = sorted(glob.glob(CODEX_SESSIONS, recursive=True),
                   key=os.path.getmtime)
    if not files:
        return {"ok": False, "why": "no session rollouts"}
    total = {"input_tokens": 0, "output_tokens": 0,
             "reasoning_output_tokens": 0, "total_tokens": 0,
             "cached_input_tokens": 0}
    counted = 0
    daily = {}
    limits, limits_at = None, ""
    for path in files:
        got = scan_rollout(path)
        if not got:
            continue
        if got["total"]:
            counted += 1
            for k in total:
                total[k] += got["total"].get(k, 0)
        for day, n in got["daily"].items():
            daily[day] = daily.get(day, 0) + n
        if got["limits"] and (got["limits_at"] or "") > limits_at:
            limits, limits_at = got["limits"], got["limits_at"]

    # per-turn rate, from the newest rollout only
    rates, prev = [], None
    for line in tail_lines(files[-1], 4 * 1024 * 1024):
        if '"token_count"' not in line:
            continue
        try:
            d = json.loads(line)
        except ValueError:
            continue
        u = ((d.get("payload") or {}).get("info") or {}).get(
            "last_token_usage") or {}
        out, ts = u.get("output_tokens") or 0, d.get("timestamp")
        if ts and prev and out:
            try:
                a = datetime.datetime.fromisoformat(prev.replace("Z", "+00:00"))
                b = datetime.datetime.fromisoformat(ts.replace("Z", "+00:00"))
            except ValueError:
                prev = ts
                continue
            gap = (b - a).total_seconds()
            if 0.5 < gap < 300:
                rates.append(out / gap)
        prev = ts or prev
    rates.sort()
    return {"ok": True, "sessions": counted, "files": len(files),
            "total": total, "rates": rates, "daily": daily,
            "limits": limits, "limits_at": limits_at, "live": cached("codex", codex_live),
            "last": os.path.getmtime(files[-1])}


def codex_plan_rows(state, w):
    """Plan type and a credit balance - all Codex publishes about the plan.

    Three lines rather than the section Copilot and Cursor get, because
    three lines is genuinely all there is. Credits belong here rather than
    under the quota bars: they are what the plan grants, not a window.
    """
    live = state.get("live") or {}
    plan = live.get("plan_type") or (state.get("limits") or {}).get("plan_type")
    credits = (live.get("credits")
               or (state.get("limits") or {}).get("credits") or {})
    pairs = []
    if credits:
        pairs.append(("credits", "unlimited" if credits.get("unlimited")
                      else str(credits.get("balance") or "0")))
    if (live.get("spend_control") or {}).get("individual_limit"):
        pairs.append(("spend limit",
                      str(live["spend_control"]["individual_limit"])))
    if not plan and not pairs:
        return []
    return plan_rows(plan, pairs, w)


def codex_tab(state, w, h):
    if not state.get("ok"):
        return [seg([(BAD, "  %s" % state.get("why"))], w - 1)]
    t = state["total"]
    rows = []
    # The one genuine quota figure any of these agents publishes: the server
    # sends it back with each response and the rollout records it. It is a
    # snapshot from whenever Codex last ran, not a live reading, so it is
    # dated.
    live = state.get("live") or {}
    lanes, source, plan = [], "", live.get("plan_type") or ""
    if live.get("rate_limit"):
        source = "live"
        for key in ("primary_window", "secondary_window"):
            win = (live["rate_limit"] or {}).get(key)
            if win and win.get("used_percent") is not None:
                lanes.append(("", win["used_percent"],
                              win.get("limit_window_seconds"),
                              win.get("reset_at")))
        # Some features meter separately from the account's general usage -
        # Spark is one - and each arrives named, with its own window and
        # reset. Rendering the list rather than the one name we know keeps
        # any future feature working without an edit.
        for extra in live.get("additional_rate_limits") or []:
            win = ((extra.get("rate_limit") or {}).get("primary_window") or {})
            if win.get("used_percent") is None:
                continue
            lanes.append((extra.get("limit_name") or "?", win["used_percent"],
                          win.get("limit_window_seconds"), win.get("reset_at")))
    elif (state.get("limits") or {}).get("primary"):
        source = "from the last session"
        win = state["limits"]["primary"]
        lanes.append(("", win.get("used_percent"),
                      (win.get("window_minutes") or 0) * 60,
                      win.get("resets_at")))
    if lanes:
        rows.append(seg([(LBL, " ── QUOTA ── "),
                         (OK if source == "live" else WARN, source),
                         (DIM, scope_phrase(w, 13 + len(source) + len(plan))),
                         (DIM, plan)], w - 1))
        prepared = []
        for name, pct, window, reset in lanes:
            secs = int(window or 0)
            wname = ("%dd" % (secs // 86400) if secs >= 86400
                     else "%dh" % (secs // 3600) if secs else "?")
            when = ""
            if reset:
                left = reset - time.time()
                when = ("resets in %dd %dh" % (left // 86400,
                                               (left % 86400) // 3600)
                        if left > 0 else "resetting")
            prepared.append((name, wname, pct, when))

        # Alone, the account-wide lanes are told apart by their window and a
        # bare "7d" is clear enough. Beside a named one it is not, so it says
        # what it covers only when there is something to confuse it with.
        named = any(name for name, _, _, _ in prepared)

        def labels(short):
            """Spell a feature out while there is room; below that the last
            segment carries it - GPT-5.3-Codex-Spark is Spark."""
            out = []
            for name, wname, _, _ in prepared:
                n = name.rsplit("-", 1)[-1] if (short and name) else name
                out.append(("%s %s" % (n or ("overall" if named else ""),
                                       wname)).strip())
            return out

        lab = labels(False)
        if w - 26 - max(len(x) for x in lab) < 20:
            lab = labels(True)
        label_w = max(9, max(len(x) for x in lab))
        for (_, _, pct, when), text in zip(prepared, lab):
            used = (pct or 0) / 100.0
            # heat(used), not heat(1 - used): red belongs at a quota nearly
            # spent, and the inverse painted a 26%-used week amber
            bar = meter(used, max(8, w - 26 - label_w))
            filled = bar.count("█")
            rows.append(seg([(DIM, " " + pad(text, label_w) + " "),
                             (heat(used), bar[:filled]), (GRID, bar[filled:]),
                             (heat(used), " %3.0f%%" % (pct or 0)),
                             (DIM, "  " + when)], w - 1))
        rows.append("")
        # Everything Codex publishes about the subscription itself: a plan
        # word and a credit balance, which is why this is three lines and
        # not the section Copilot and Cursor get. Credits live here rather
    rows.append(seg([(LBL, " ── TOTALS ── "),
                     (DIM, "%d sessions · newest %s ago"
                      % (state["sessions"], ago(state["last"])))], w - 1))
    cells = [("input tokens", big_num(t["input_tokens"]), TXT),
             ("output tokens", big_num(t["output_tokens"]), AGENT),
             ("reasoning tokens", big_num(t["reasoning_output_tokens"]), TXT),
             ("cached input", big_num(t["cached_input_tokens"]), DIM),
             ("all tokens", big_num(t["total_tokens"]), TXT),
             ("rollout files", "%d" % state["files"], DIM)]
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

    rates = state["rates"]
    rows.append("")
    if rates:
        med = rates[len(rates) // 2]
        p90 = rates[min(len(rates) - 1, int(len(rates) * 0.9))]
        rows.append(seg([(LBL, " ── OUTPUT RATE ── "),
                         (DIM, "newest session, %d turns" % len(rates))], w - 1))
        rows.append(seg([(DIM, "  median "), (AGENT, "%.0f" % med),
                         (DIM, " tok/s   p90 "), (TXT, "%.0f" % p90),
                         (DIM, "   max "), (TXT, "%.0f" % rates[-1])], w - 1))
        hi = rates[-1] or 1
        buckets = [0] * max(10, min(len(rates), w - 6))
        for r in rates:
            buckets[min(len(buckets) - 1,
                        int(r / hi * (len(buckets) - 1)))] += 1
        top = max(buckets) or 1
        for line in vbars([(b, AGENT) for b in buckets], 3):
            rows.append(seg([(RST, " ")] + line, w - 1))
        rows.append(seg([(RST, " "), (GRID, "─" * len(buckets))], w - 1))
        rows.append(seg([(DIM, " 0 tok/s"),
                         (DIM, " " * max(1, len(buckets) - 18)),
                         (DIM, "%.0f tok/s" % rates[-1])], w - 1))
    grid, peak, best, facts = day_calendar(state.get("daily") or {}, w,
                                           CODEX_STEPS)
    if grid:
        rows.append("")
        rows.append(seg([(LBL, " ── TOKENS / DAY ── "),
                         (DIM, "peak "), (AGENT, big_num(peak)),
                         (DIM, " on %s" % (best.strftime("%b %-d") if best
                                           else "--"))], w - 1))
        for line in grid:
            rows.append(seg(line, w - 1))
        rows.append(seg([(DIM, "  Less ")]
                        + [(rgb(*c), "█") for c in CODEX_STEPS]
                        + [(DIM, " More")], w - 1))
    rows.append("")
    rows.append(seg([(DIM, "  Tokens and rate are measured here, from the"
                           " rollouts. Quota is")], w - 1))
    rows.append(seg([(DIM, "  the account's, fetched from the same endpoint"
                           " the Codex CLI uses.")], w - 1))
    return rows


_GROK_CACHE = {}


def grok_quota():
    """Grok's weekly credit window, as its own CLI receives it.

    Not hidden and not inferred: the server sends it, and the CLI writes it
    into the session log under `.ctx.config`. An earlier pass here concluded
    no quota existed, having grepped for limit/quota/remaining/reset - the
    keys are `creditUsagePercent` and `currentPeriod`, so the search missed
    them and the tab said so in print for a day.
    """
    best = None
    for path in [GROK_LOG] if os.path.exists(GROK_LOG) else []:
        for line in tail_lines(path, 2 * 1024 * 1024):
            if "creditUsagePercent" not in line:
                continue
            try:
                d = json.loads(line)
            except ValueError:
                continue
            ctx = d.get("ctx") if isinstance(d, dict) else None
            cfg = (ctx or {}).get("config") if isinstance(ctx, dict) else None
            if not isinstance(cfg, dict) or "creditUsagePercent" not in cfg:
                continue
            period = cfg.get("currentPeriod") or {}
            when = period.get("start") or ""
            if best is None or when >= best["start"]:
                best = {"percent": cfg.get("creditUsagePercent"),
                        "kind": period.get("type") or "",
                        "start": when, "end": period.get("end") or "",
                        "tier": (ctx or {}).get("subscriptionTier") or "",
                        "on_demand_used": (cfg.get("onDemandUsed") or {}).get("val"),
                        "on_demand_cap": (cfg.get("onDemandCap") or {}).get("val"),
                        "prepaid": (cfg.get("prepaidBalance") or {}).get("val")}
        if best:
            break
    return best


def read_grok():
    """Grok logs a running totalTokens on each session event.

    Deltas between consecutive events, bucketed by the event's own timestamp,
    give per-day figures; the running total alone would credit an entire
    session to whichever day it happened to be read on. Cached per file on
    mtime and size, like the Codex rollouts.
    """
    files = glob.glob(GROK_SESSIONS, recursive=True)
    if not files:
        return {"ok": False, "why": "no sessions on disk"}
    total, daily, sessions, newest = 0, {}, 0, 0
    for path in files:
        try:
            st = os.stat(path)
        except OSError:
            continue
        key = (st.st_mtime, st.st_size)
        hit = _GROK_CACHE.get(path)
        if hit and hit[0] == key:
            got = hit[1]
        else:
            got, prev = {"total": 0, "daily": {}}, 0
            try:
                with open(path, errors="ignore") as f:
                    for line in f:
                        m = re.search(r'"totalTokens":(\d+)', line)
                        if not m:
                            continue
                        value = int(m.group(1))
                        step = value - prev
                        prev = max(prev, value)
                        if step <= 0:
                            continue
                        when = re.search(r'"agentTimestampMs":(\d+)', line)
                        if not when:
                            continue
                        day = datetime.datetime.fromtimestamp(
                            int(when.group(1)) / 1000.0,
                            datetime.timezone.utc).date().isoformat()
                        got["daily"][day] = got["daily"].get(day, 0) + step
                        got["total"] += step
            except OSError:
                continue
            _GROK_CACHE[path] = (key, got)
        if got["total"]:
            sessions += 1
            total += got["total"]
            for day, n in got["daily"].items():
                daily[day] = daily.get(day, 0) + n
        newest = max(newest, st.st_mtime)
    return {"ok": True, "sessions": sessions, "files": len(files),
            "total": total, "daily": daily, "last": newest,
            "quota": grok_quota()}


def grok_plan_rows(q, w):
    """Grok states its tier and the kind of period it bills in, and no more.

    Both arrive on the client log beside the credit percentage, so this
    costs nothing extra to show.
    """
    if not q:
        return []
    pairs = []
    kind = (q.get("kind") or "").replace("USAGE_PERIOD_TYPE_", "").lower()
    if kind:
        pairs.append(("billing period", kind))
    cap = q.get("on_demand_cap")
    if cap is not None:
        pairs.append(("on-demand", "%s of %s used"
                      % (q.get("on_demand_used") or 0, cap)))
    if q.get("prepaid") is not None:
        pairs.append(("prepaid balance", str(q["prepaid"])))
    return plan_rows(q.get("tier"), pairs, w)


def grok_tab(state, w, h):
    if not state.get("ok"):
        return [seg([(BAD, "  %s" % state.get("why"))], w - 1)]
    rows = []
    q = state.get("quota")
    if q and q.get("percent") is not None:
        # The one real remaining-quota figure in this widget: everything else
        # here counts what was spent. It leads the tab for that reason.
        used = float(q["percent"]) / 100.0
        left = None
        try:
            end = datetime.datetime.fromisoformat(q["end"])
            left = (end - datetime.datetime.now(
                datetime.timezone.utc)).total_seconds() / 86400.0
        except (ValueError, TypeError):
            pass
        period = ("weekly" if "WEEKLY" in (q.get("kind") or "")
                  else (q.get("kind") or "").replace(
                      "USAGE_PERIOD_TYPE_", "").lower() or "current")
        rows.append(seg([(LBL, " ── %s QUOTA ── " % period.upper()),
                         (DIM, "resets in %.1f days" % left
                          if left is not None and left >= 0 else "")], w - 1))
        rows.append(seg([(heat(used), " %-5s" % ("%.0f%%" % q["percent"])),
                         (heat(used), meter(used, max(10, w - 30))),
                         (DIM, "  credits used")], w - 1))
        span = ""
        for key, label in (("start", ""), ("end", " → ")):
            try:
                span += label + datetime.datetime.fromisoformat(
                    q[key]).strftime("%-d %b")
            except (ValueError, TypeError, KeyError):
                span = ""
                break
        extras = []
        if q.get("on_demand_cap"):
            extras.append("on-demand %s/%s" % (q.get("on_demand_used"),
                                               q["on_demand_cap"]))
        if q.get("prepaid"):
            extras.append("prepaid %s" % q["prepaid"])
        rows.append(seg([(DIM, "  window "), (TXT, span or "?"),
                         (DIM, "   " + " · ".join(extras) if extras else "")],
                        w - 1))
        rows.append("")
    rows.append(seg([(LBL, " ── TOTALS ── "),
                     (DIM, "%d sessions · newest %s ago"
                      % (state["sessions"], ago(state["last"])))], w - 1))
    rows.append(seg([(DIM, "  tokens "), (AGENT, big_num(state["total"])),
                     (DIM, "   across %d session files" % state["files"])],
                    w - 1))
    grid, peak, best, facts = day_calendar(state.get("daily") or {}, w,
                                           GROK_STEPS)
    if grid:
        rows.append("")
        rows.append(seg([(LBL, " ── TOKENS / DAY ── "),
                         (DIM, "peak "), (AGENT, big_num(peak)),
                         (DIM, " on %s" % (best.strftime("%b %-d") if best
                                           else "--"))], w - 1))
        for line in grid:
            rows.append(seg(line, w - 1))
        rows.append(seg([(DIM, "  Less ")]
                        + [(rgb(*c), "█") for c in GROK_STEPS]
                        + [(DIM, " More")], w - 1))
    rows.append("")
    rows.append(seg([(DIM, "  Totals are a running count per session, summed"
                           " as deltas so a")], w - 1))
    rows.append(seg([(DIM, "  session spanning days lands on the right one."
                           " The quota above is")], w - 1))
    rows.append(seg([(DIM, "  the server's own figure, read from the client"
                           " log - not inferred.")], w - 1))
    return rows


# Agents whose usage exists but is not readable from outside their session.
# Naming where the number actually lives beats showing an empty gauge.
ELSEWHERE = {}


def copilot_token():
    """The CLI keeps its OAuth token in ~/.copilot/config.json.

    That file is JSON with `//` comments on top, which json.load refuses, so
    the comments come off first. Keyed by host and login, because one machine
    can be signed in to github.com and an Enterprise host at once.
    """
    try:
        with open(COPILOT_CONFIG) as f:
            raw = re.sub(r"^\s*//.*$", "", f.read(), flags=re.M)
        toks = (json.loads(raw) or {}).get("copilotTokens") or {}
    except (OSError, ValueError):
        return None
    for host, tok in toks.items():
        if tok:
            return tok
    return None


def copilot_live():
    """Entitlement and quota, from the endpoint the Copilot CLI itself uses.

    This is where Copilot's remaining quota actually lives. The session store
    records what was spent per turn and is empty on plenty of machines; this
    is the account's standing, and it answers the only question a limit pane
    is really asked.
    """
    tok = copilot_token()
    if not tok:
        return {"why": "no token in ~/.copilot/config.json"}
    req = urllib.request.Request(COPILOT_USER_API, headers={
        "Authorization": "token " + tok, "User-Agent": "terminal-toys",
        "Accept": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return {"data": json.load(r)}
    except (urllib.error.URLError, ValueError, OSError) as e:
        # Which of the two went wrong matters: blaming the config file for a
        # dropped connection sends the reader to edit a file that is fine.
        return {"why": "quota request failed: %s" % str(e)[:40]}


def read_copilot():
    """Live quota, plus whatever the local session store has recorded.

    The two halves are independent: the quota is the account's and arrives
    over the network, while the per-turn detail is this machine's and is
    frequently empty. Either can be present without the other.
    """
    got = cached("copilot", copilot_live) or {}
    out = {"ok": True, "live": got.get("data"), "live_why": got.get("why"),
           "sessions": 0, "events": 0, "usage": None}
    if not os.path.exists(COPILOT_DB):
        out["why"] = "no session store"
        return out
    try:
        con = sqlite3.connect("file:%s?mode=ro" % COPILOT_DB, uri=True)
        out["sessions"] = con.execute(
            "select count(*) from sessions").fetchone()[0]
        row = con.execute(
            "select count(*), sum(input_tokens), sum(output_tokens),"
            " sum(cache_read_tokens), sum(reasoning_tokens),"
            " sum(total_nano_aiu), avg(time_to_first_token_ms),"
            " avg(inter_token_latency_ms), sum(duration_ms)"
            " from assistant_usage_events").fetchone()
        out["events"] = row[0] or 0
        if out["events"]:
            out["usage"] = {
                "input": row[1] or 0, "output": row[2] or 0,
                "cache": row[3] or 0, "reasoning": row[4] or 0,
                "nano_aiu": row[5] or 0, "ttft": row[6] or 0,
                "itl": row[7] or 0, "ms": row[8] or 0,
            }
            out["models"] = con.execute(
                "select model, count(*), sum(output_tokens)"
                " from assistant_usage_events group by model"
                " order by 3 desc limit 6").fetchall()
        con.close()
    except sqlite3.Error as e:
        out["why"] = str(e)[:40]
    return out


# The account's own description of itself. Names are what the API returns,
# shortened only where it repeats itself.
COPILOT_FEATURES = (("chat_enabled", "chat"),
                    ("cli_enabled", "cli"),
                    ("is_mcp_enabled", "mcp"),
                    ("cli_remote_control_enabled", "remote control"),
                    ("cloud_session_storage_enabled", "cloud sessions"),
                    ("copilot_app_enabled", "app"),
                    ("editor_preview_features_enabled", "editor previews"),
                    ("copilotignore_enabled", "copilotignore"))


def wrap_pair(key, value, label_w, w):
    """A labelled value flowed onto as many lines as it needs.

    Only text can be wrapped. A bar chart broken across two lines is not a
    bar chart, and a table row wrapped mid-row loses the columns that made
    it a table - which is why those still adapt to the width instead. A
    value like an enterprise sku is just words, so it wraps, and the
    continuation lines sit under the value rather than under the label.
    """
    budget = max(8, w - label_w - 5)
    words, lines, line = str(value).split(), [], ""
    for word in words:
        if line and len(line) + 1 + len(word) > budget:
            lines.append(line)
            line = word
        else:
            # A single word longer than the column is split rather than
            # allowed to run off; a sku is one word and still has to fit.
            while len(word) > budget:
                lines.append(word[:budget])
                word = word[budget:]
            line = (line + " " + word).strip() if line else word
    if line:
        lines.append(line)
    return [(key if not i else "", part) for i, part in enumerate(lines)]


def plan_rows(headline, pairs, w, note="", wrapped=None):
    """A subscription block: what the plan is, then the facts about it.

    Shared by four tabs so the same question is answered in the same shape
    wherever you are on the wall - a percentage means little without the
    subscription it is a percentage of.
    """
    rows = [seg([(LBL, " ── SUBSCRIPTION ── "), (TXT, headline or "unknown"),
                 (DIM, "   " + note if note else "")], w - 1)]
    labels = [k for k, _ in pairs] + ([wrapped[0]] if wrapped else [])
    label_w = max([len(x) for x in labels] or [0])
    for key, value in pairs:
        for lab, part in wrap_pair(key, value, label_w, w):
            rows.append(seg([(DIM, "  " + pad(lab, label_w) + "  "),
                             (TXT, part)], w - 1))
    if wrapped and wrapped[1]:
        # Wrapped rather than clipped: a truncated list reads as a shorter
        # one, and only the first line takes the label.
        budget = max(10, w - label_w - 6)
        lines, line = [], []
        for name in wrapped[1]:
            if line and len(" · ".join(line + [name])) > budget:
                lines.append(line)
                line = []
            line.append(name)
        if line:
            lines.append(line)
        for i, part in enumerate(lines):
            rows.append(seg([(DIM, "  " + pad(wrapped[0] if not i else "",
                                              label_w) + "  "),
                             (OK, " · ".join(part))], w - 1))
    return rows


def last_section(rows, block):
    """Put a section at the end of a tab with exactly one blank before it.

    Every tab ends with its subscription block, so the separator is owned in
    one place rather than by five callers who each end differently - some
    already finish on a blank line and would otherwise leave two.
    """
    if not block:
        return rows
    while rows and rows[-1] == "":
        rows.pop()
    return rows + [""] + block


def copilot_plan_rows(live, w):
    """Which subscription this quota belongs to, and since when.

    An enterprise seat is why two of the three pools come back unlimited,
    and assigned_date is the only field saying how long it has been so.
    """
    pairs = []
    since = iso_epoch(live.get("assigned_date") or "")
    day = iso_day(live.get("assigned_date") or "")
    if since and day:
        pairs.append(("seat since", "%s · %s ago" % (day, ago(since))))
    orgs = [o.get("name") or o.get("login")
            for o in (live.get("organization_list") or []) if o]
    if orgs:
        pairs.append(("organisation", ", ".join(orgs)))
    if live.get("login"):
        pairs.append(("account", live["login"]))
    if live.get("access_type_sku"):
        pairs.append(("sku", live["access_type_sku"]))
    if live.get("token_based_billing"):
        pairs.append(("billing", "token-based"))
    on = [name for flag, name in COPILOT_FEATURES if live.get(flag)]
    return plan_rows(live.get("copilot_plan"), pairs, w,
                     note="upgradeable" if live.get("can_upgrade_plan") else "",
                     wrapped=("enabled", on))


def copilot_tab(state, w, h):
    live = state.get("live") or {}
    rows = []
    snaps = live.get("quota_snapshots") or {}
    if snaps:
        # A monthly quota reset arrives as a bare date; days remaining is the
        # form every other tab here uses, and the one anyone reads.
        # quota_reset_date_utc, not quota_reset_date: the bare date carries
        # no zone, so it parses as local midnight and the countdown drifts by
        # the machine's UTC offset. Zero on this server, which is exactly why
        # it would have gone unnoticed here.
        stamp = iso_epoch(live.get("quota_reset_date_utc") or "")
        when = ""
        if stamp:
            days = int((stamp - time.time()) // 86400)
            when = "resets in %dd" % days if days > 0 else "resets today"
        rows.append(seg([(LBL, " ── QUOTA ── "), (OK, "live"),
                         (DIM, " · account-wide")], w - 1))
        # The reset sits with the window rather than on the header: they are
        # two halves of the same cycle, and the header had no room for it.
        span = quota_window(stamp)
        if span or when:
            rows.append(seg([(DIM, "  window "), (TXT, span or "—"),
                             (DIM, " · monthly · " if span else " "),
                             (DIM, when)], w - 1))
        label_w = max(9, max(len(k) for k in snaps))
        for key in sorted(snaps, key=lambda k: (snaps[k].get("unlimited"), k)):
            q = snaps[key] or {}
            name = pad(key.replace("_", " "), label_w)
            if q.get("unlimited"):
                # No denominator, so no bar. An unlimited pool drawn as an
                # empty gauge invents a limit that was explicitly denied.
                rows.append(seg([(DIM, " " + name + " "),
                                 (OK, "unlimited")], w - 1))
                continue
            ent = q.get("entitlement") or 0
            used_n = q.get("credits_used")
            # percent_remaining is what the API gives; every other tab here
            # shows what is spent, and red belongs at the empty end.
            pct = 100.0 - float(q.get("percent_remaining") or 0)
            used = max(0.0, min(1.0, pct / 100.0))
            bar = meter(used, max(8, w - 30 - label_w))
            filled = bar.count("█")
            rows.append(seg([(DIM, " " + name + " "),
                             (heat(used), bar[:filled]), (GRID, bar[filled:]),
                             (heat(used), " %3.0f%%" % pct)], w - 1))
            # A pool can carry its own reset, in which case it is not on the
            # account-wide cycle in the header and has to say so itself.
            own = q.get("quota_reset_at") or 0
            own_when = ""
            if own and abs(own - (stamp or 0)) > 3600:
                left_s = own - time.time()
                own_when = ("   resets in " + left_span(left_s)
                            if left_s > 0 else "   resetting")
            if ent:
                rows.append(seg([(DIM, " " + " " * label_w + "  "),
                                 (TXT, "{:,}".format(int(used_n or 0))),
                                 (DIM, " of "), (TXT, "{:,}".format(int(ent))),
                                 (DIM, " · "),
                                 (TXT, "{:,}".format(int(q.get("remaining")
                                                         or 0))),
                                 (DIM, " left"),
                                 (WARN, "   %d over" % q["overage_count"]
                                  if q.get("overage_count") else ""),
                                 (DIM, own_when)], w - 1))
        rows.append("")
    elif state.get("live_why"):
        rows.append(seg([(WARN, "  no quota: " + state["live_why"])], w - 1))
        rows.append("")

    use = state.get("usage")
    if use:
        rows.append(seg([(LBL, " ── SPENT ── "),
                         (DIM, "%d turns across %d sessions"
                          % (state["events"], state["sessions"]))], w - 1))
        cells = [("input tokens", big_num(use["input"]), TXT),
                 ("output tokens", big_num(use["output"]), AGENT),
                 ("cache read", big_num(use["cache"]), DIM),
                 ("reasoning tokens", big_num(use["reasoning"]), TXT),
                 # total_nano_aiu is billionths of an AI unit
                 ("AI units", "%.3f" % (use["nano_aiu"] / 1e9), TXT),
                 ("time generating", span_ms(use["ms"]), DIM)]
        label_w = max(len(c[0]) for c in cells)
        ncols = 2 if (w - 2) // 2 - label_w - 3 >= 8 else 1
        cw = (w - 2) // ncols
        val_w = max(5, cw - label_w - 3)
        for i in range(0, len(cells), ncols):
            line = [(RST, " ")]
            for lab, value, colour in cells[i:i + ncols]:
                line += [(DIM, " " + pad(lab, label_w) + " "),
                         (colour, pad(value, val_w))]
            rows.append(seg(line, w - 1))
        rows.append("")
        # The one agent that measures this rather than leaving it to be
        # inferred from timestamps, which is why it is stated flatly.
        rows.append(seg([(LBL, " ── LATENCY ── "),
                         (DIM, "measured by Copilot, not inferred")], w - 1))
        rows.append(seg([(DIM, "  first token "),
                         (TXT, "%.0f ms" % use["ttft"]),
                         (DIM, "   between tokens "),
                         (TXT, "%.1f ms" % use["itl"])], w - 1))
        rows.append("")
        for model, n, out_tok in (state.get("models") or []):
            rows.append(seg([(TXT, "  " + pad(str(model or "?"), 28)),
                             (DIM, "%5d turns  " % n),
                             (AGENT, big_num(out_tok))], w - 1))
    else:
        rows.append(seg([(LBL, " ── SPENT ── "), (DIM, "nothing recorded")],
                        w - 1))
        rows.append("")
        for line in (
                "The session store has the richest schema of any agent here:",
                "assistant_usage_events records per-turn input, output, cache",
                "and reasoning tokens, credits as total_nano_aiu, and - alone",
                "among these agents - duration, time to first token and",
                "inter-token latency.",
                "",
                "It has %d sessions and no turns on this machine, so there is"
                % state.get("sessions", 0),
                "nothing to total. The quota above is the account's and is",
                "real; this half fills in the moment a turn is recorded here."):
            rows.append(seg([(DIM if line else RST, "  " + line)], w - 1))
    return rows


class Store(object):
    def __init__(self):
        self.lock = threading.Lock()
        self.claude = {}
        self.cursor = {}
        self.codex = {}
        self.grok = {}
        self.copilot = {}
        self.installed = {}
        self.error = None
        self.fetched = 0
        self.wake = threading.Event()

    def snapshot(self):
        with self.lock:
            return (dict(self.claude), dict(self.cursor), dict(self.codex),
                    dict(self.grok), dict(self.copilot),
                    dict(self.installed), self.fetched, self.error)

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
        while True:
            claude, cursor, codex = read_claude(), read_cursor(), read_codex()
            grok, copilot = read_grok(), read_copilot()
            found = detect_agents()
            with self.lock:
                self.claude, self.cursor, self.codex = claude, cursor, codex
                self.grok, self.copilot = grok, copilot
                self.installed = found
                self.fetched = time.time()
            self.wake.wait(REFRESH)
            self.wake.clear()


# What we know how to read, and how to tell it is here. An agent counts as
# present if its CLI is on PATH *or* it has left state behind: an uninstalled
# agent whose history is still on disk is worth showing, and a CLI installed
# under a different name would otherwise vanish.
AGENTS = {
    "claude": {"label": "Claude Code", "bins": ("claude",),
               "paths": (CLAUDE_STATS,)},
    "codex": {"label": "OpenAI Codex", "bins": ("codex",),
              "paths": (os.path.expanduser("~/.codex/sessions"),)},
    "cursor": {"label": "Cursor", "bins": ("cursor-agent", "cursor"),
               "paths": (CURSOR_DB,)},
    "grok": {"label": "Grok", "bins": ("grok",),
             "paths": (os.path.expanduser("~/.grok"),)},
    "copilot": {"label": "GitHub Copilot", "bins": ("copilot",),
                "paths": (COPILOT_DB, COPILOT_CONFIG)},
}
ORDER = ("claude", "codex", "cursor", "grok", "copilot")


def detect_agents():
    """Which agents this machine has, by binary or by leftover state."""
    found = {}
    for name, spec in AGENTS.items():
        binary = next((b for b in spec["bins"] if shutil.which(b)), None)
        data = next((p for p in spec["paths"] if os.path.exists(p)), None)
        found[name] = {"bin": binary, "data": data,
                       "present": bool(binary or data)}
    return found


def visible_agents(found):
    """The tabs to draw.

    Empty `agents` discovers: every agent this machine actually has. Naming
    them instead fixes both the set and the order, whether or not they are
    installed - if you listed it, you want the tab. `exclude_agents` drops one
    either way.

    Falls back to everything known if the result would be empty, because a
    widget with no tabs teaches nothing and the likeliest cause is a typo in
    the config rather than a machine with no agents on it.
    """
    drop = set(_CFG["exclude_agents"] or [])
    named = [n for n in (_CFG["agents"] or []) if n in AGENTS]
    chosen = named or [n for n in ORDER if found.get(n, {}).get("present")]
    shown = tuple(n for n in chosen if n not in drop)
    return shown or ORDER


def config_complaints(found):
    """Names in the config that match no agent we know how to read."""
    known = set(AGENTS)
    bad = [n for n in (list(_CFG["agents"] or [])
                       + list(_CFG["exclude_agents"] or [])) if n not in known]
    return ("unknown agent in config: %s (known: %s)"
            % (", ".join(sorted(set(bad))), ", ".join(ORDER))) if bad else None


def tab_bar(active, installed, tabs, w):
    # brackets as well as the tint: which tab is open must not depend on a
    # background colour surviving. A dot marks an agent that is installed.
    parts = [(RST, " ")]
    for name in tabs:
        here = name == active
        have = (installed.get(name) or {}).get("present", False)
        parts.append((bg(38, 56, 76) + ACCENT if here else DIM,
                      ("[%s]" if here else " %s ") % name.upper()))
        parts.append((OK if have else GRID, "·" if have else " "))
    return seg(parts, w - 1)


WEEKDAYS = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")


MONTHS = ("Jan", "Feb", "Mar", "Apr", "May", "Jun",
          "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")


def token_heatmap(daily, w):
    """Claude's shape: a list of {date, tokensByModel} records."""
    return day_calendar(
        dict((e["date"], sum((e.get("tokensByModel") or {}).values()))
             for e in daily if e.get("date")), w)


def day_calendar(totals_by_date, w, steps=HEAT_STEPS):
    """Tokens per day, drawn the way Claude Code's own /stats draws it.

    Weekday rows with only Mon, Wed and Fri labelled; one cell per day; months
    named across the top; solid blocks in four steps of a single hue, and a dim
    dot for a day the file has no entry for.

    The grid spans the whole window even where there is no data, because the
    emptiness is information: this cache retains about four weeks, and a year
    of dots with a fortnight of colour at the right-hand end says that plainly.
    """
    totals = {}
    for key, value in (totals_by_date or {}).items():
        try:
            totals[datetime.date.fromisoformat(key)] = value
        except (ValueError, TypeError):
            continue
    if not totals:
        return [], 0, None, {}
    peak = max(totals.values()) or 1
    best = max(totals, key=totals.get)

    last = max(totals)
    weeks_fit = max(4, w - 7)
    end_week = last - datetime.timedelta(days=last.weekday())
    starts = [end_week - datetime.timedelta(days=7 * i)
              for i in range(weeks_fit - 1, -1, -1)]

    # month names sit over the week their month starts in, three characters
    # wide like /stats - a single initial is not a label, it is a hint
    # a month label needs three clear cells; without checking where the last
    # one ended, a short month writes over its neighbour and produces "JJul"
    strip = [" "] * len(starts)
    seen, wrote_to = None, -1
    for x, wk in enumerate(starts):
        if wk.month != seen and x > wrote_to and x + 3 <= len(strip):
            seen = wk.month
            for k, ch in enumerate(MONTHS[wk.month - 1]):
                strip[x + k] = ch
            wrote_to = x + 3
    rows = [[(DIM, "     " + "".join(strip))]]
    for i in range(7):
        label = {0: "Mon", 2: "Wed", 4: "Fri"}.get(i, "")
        line = [(DIM, " %-4s" % label)]
        for wk in starts:
            day = wk + datetime.timedelta(days=i)
            n = totals.get(day)
            if n is None:
                line.append((EMPTY_CELL, "·"))
            else:
                line.append((shade((n / float(peak)) ** 0.5, steps), "█"))
        rows.append(line)

    # active out of days in the range, not out of days the file happens to
    # list - otherwise every day is active by construction and the ratio says
    # nothing
    span = (max(totals) - min(totals)).days + 1
    active = sum(1 for v in totals.values() if v)
    run = best_run = 0
    for i in range(span):
        day = min(totals) + datetime.timedelta(days=i)
        run = run + 1 if totals.get(day) else 0
        best_run = max(best_run, run)
    current = 0
    for i in range(span):
        day = max(totals) - datetime.timedelta(days=i)
        if not totals.get(day):
            break
        current += 1
    facts = {"active": active, "span": span,
             "longest": best_run, "current": current}
    return rows, peak, best, facts


def claude_tab(state, w, h):
    rows = claude_quota(state.get("quota"), w)
    if not state.get("ok"):
        return rows + [seg([(BAD, "  no stats cache: %s"
                             % state.get("why"))], w - 1)]
    d = state["data"]
    mu = d.get("modelUsage") or {}
    daily = d.get("dailyActivity") or []

    out_tok = sum(v.get("outputTokens") or 0 for v in mu.values())
    in_tok = sum(v.get("inputTokens") or 0 for v in mu.values())
    cache_r = sum(v.get("cacheReadInputTokens") or 0 for v in mu.values())
    cache_w = sum(v.get("cacheCreationInputTokens") or 0 for v in mu.values())

    # the heatmap is computed first: its streaks and active-day counts belong
    # in the summary above it, not only beside the calendar
    grid, peak, best, facts = token_heatmap(d.get("dailyModelTokens") or [], w)

    ls = d.get("longestSession") or {}
    fav = max(mu, key=lambda k: mu[k].get("outputTokens") or 0) if mu else "—"
    all_tokens = in_tok + out_tok + cache_r + cache_w
    rows.append(seg([(LBL, " ── SUMMARY ── "),
                     (DIM, "all time · since %s"
                      % (d.get("firstSessionDate") or "")[:10])], w - 1))
    pairs = [
        ("Favorite model", fav.replace("claude-", ""), AGENT,
         "Total tokens", big_num(all_tokens), AGENT),
        ("Sessions", "%d" % (d.get("totalSessions") or 0), TXT,
         "Longest session", span_ms(ls.get("duration")), TXT),
        ("Active days", "%d/%d" % (facts.get("active", 0), facts.get("span", 0))
         if facts else "—", TXT,
         "Longest streak", "%d days" % facts.get("longest", 0) if facts else "—",
         TXT),
        ("Most active day", best.strftime("%b %-d") if best else "—", TXT,
         "Current streak", "%d days" % facts.get("current", 0) if facts else "—",
         OK if facts.get("current") else DIM),
    ]
    lw = max(max(len(a), len(c)) for a, _b, _bc, c, _d2, _dc in pairs)
    half = (w - 3) // 2
    vw = max(6, half - lw - 2)
    for a, b, bc, c, e, ec in pairs:
        rows.append(seg([(DIM, " " + pad(a, lw) + " "), (bc, pad(b, vw)),
                         (DIM, " " + pad(c, lw) + " "), (ec, pad(e, vw))],
                        w - 1))
    rows.append(seg([(DIM, "  Input "), (TXT, big_num(in_tok)),
                     (DIM, " · Output "), (TXT, big_num(out_tok)),
                     (DIM, " · Cache read "), (TXT, big_num(cache_r)),
                     (DIM, " · Cache written "), (TXT, big_num(cache_w))],
                    w - 1))
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
            share = tok / float(top)
            bar = meter(share, max(6, w - 34))
            filled = bar.count("█")
            rows.append(seg([(TXT, "  " + pad(name.replace("claude-", ""), 20)),
                             (AGENT, "%7s " % big_num(tok)),
                             (AGENT, bar[:filled]), (GRID, bar[filled:])],
                            w - 1))

    # 26 days of activity, straight from the file
    if daily:
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

    # ── how fast it generates ───────────────────────────────────────────
    rates = state.get("rates") or []
    if rates:
        med = rates[len(rates) // 2]
        p90 = rates[min(len(rates) - 1, int(len(rates) * 0.9))]
        rows.append("")
        rows.append(seg([(LBL, " ── OUTPUT RATE ── "),
                         (DIM, "%d turns across %d transcripts"
                          % (len(rates), state.get("sampled", 0)))], w - 1))
        rows.append(seg([(DIM, "  median "), (AGENT, "%.0f" % med),
                         (DIM, " tok/s   p90 "), (TXT, "%.0f" % p90),
                         (DIM, "   request to response, tools included")],
                        w - 1))

    # ── tokens per day, as a calendar ───────────────────────────────────
    if grid:
        rows.append("")
        rows.append(seg([(LBL, " ── TOKENS / DAY ── "),
                         (DIM, "peak "), (AGENT, big_num(peak)),
                         (DIM, " on %s" % (best.strftime("%b %-d") if best
                                           else "--"))], w - 1))
        for line in grid:
            rows.append(seg(line, w - 1))
        rows.append(seg([(DIM, "  Less ")]
                        + [(rgb(*c), "█") for c in HEAT_STEPS]
                        + [(DIM, " More")], w - 1))


    rows.append("")
    rows.append(seg([(DIM, "  This file records what was spent. It carries no"
                           " limit and no reset,")], w - 1))
    rows.append(seg([(DIM, "  so no percentage of a quota is shown -"
                           " there is none to read.")], w - 1))
    return rows


def cursor_quota(live, w):
    """The three lanes cursor-agent's Usage view shows, plus the cycle."""
    plan = (live or {}).get("planUsage") or {}
    if not plan:
        return []
    rows = []
    ends = live.get("billingCycleEnd")
    when = ""
    if ends:
        left = int(ends) / 1000.0 - time.time()
        when = ("resets in %dd" % (left // 86400)) if left > 0 else "resetting"
    rows.append(seg([(LBL, " ── QUOTA ── "), (OK, "live"),
                     (DIM, scope_phrase(w, 17 + len(when))),
                     (DIM, when)], w - 1))
    values = {"included": plan.get("totalPercentUsed"),
              "auto": plan.get("autoPercentUsed"),
              "api": plan.get("apiPercentUsed")}
    for name, colour in CURSOR_LANES:
        pct = values.get(name)
        if pct is None:
            continue
        used = max(0.0, min(1.0, pct / 100.0))
        bar = meter(used, max(8, w - 30))
        filled = bar.count("█")
        rows.append(seg([(DIM, " %-9s" % name),
                         (rgb(*colour), bar[:filled]),
                         (GRID, bar[filled:]),
                         (rgb(*colour), " %3.0f%%" % pct)], w - 1))
    limit, spend = plan.get("limit"), plan.get("totalSpend")
    if limit:
        # Deliberately dollars rather than a fourth bar. This is spend against
        # the plan limit - a different denominator from the three lanes above,
        # which are the server's own percentages - and drawing it as a bar
        # beside them would invite reading 12% and 2% as the same scale.
        left = plan.get("remaining")
        rows.append(seg([(DIM, "  spend "),
                         (TXT, "$%.2f" % ((spend or 0) / 100.0)),
                         (DIM, " of "), (TXT, "$%.2f" % (limit / 100.0)),
                         (DIM, "   $%.2f left" % (left / 100.0)
                          if left is not None else "")], w - 1))
    rows.append("")
    return rows


def cursor_spend_rows(spend, w):
    """Where the money went, per model, over the last 30 days."""
    if not spend or not spend.get("aggregations"):
        return []
    rows = [seg([(LBL, " ── SPEND ── "), (DIM, "last 30d · "),
                 (AGENT, "$%.2f" % (float(spend.get("totalCostCents") or 0) / 100)),
                 (DIM, "  in "), (TXT, big_num(int(spend.get("totalInputTokens") or 0))),
                 (DIM, " · out "), (TXT, big_num(int(spend.get("totalOutputTokens") or 0))),
                 (DIM, " · cache "),
                 (TXT, big_num(int(spend.get("totalCacheReadTokens") or 0)))],
                w - 1)]
    models = sorted(spend["aggregations"],
                    key=lambda a: -float(a.get("totalCents") or 0))
    top = float(models[0].get("totalCents") or 1)
    for a in models[:6]:
        cents = float(a.get("totalCents") or 0)
        bar = meter(cents / top if top else 0, max(6, w - 44))
        filled = bar.count("█")
        rows.append(seg([(TXT, "  " + pad(str(a.get("modelIntent") or "?"), 26)),
                         (AGENT, "%9s " % ("$%.2f" % (cents / 100))),
                         (AGENT, bar[:filled]), (GRID, bar[filled:])], w - 1))
    rows.append("")
    return rows


def cursor_tab(state, w, h):
    if not state.get("ok"):
        return (cursor_quota(state.get("live"), w)
                + cursor_spend_rows(state.get("spend"), w)
                or [seg([(BAD, "  %s" % state.get("why"))], w - 1)])
    rows = cursor_quota(state.get("live"), w)
    rows += cursor_spend_rows(state.get("spend"), w)
    rows.append(seg([(LBL, " ── AI-WRITTEN CODE ── "),
                 (DIM, "last seen %s ago"
                  % ago((state.get("last") or 0) / 1000.0
                        if state.get("last") else None))], w - 1))
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
            bar = meter(n / float(top), max(6, w - 36))
            filled = bar.count("█")
            rows.append(seg([(TXT, "  " + pad(str(name or "?"), 22)),
                             (AGENT, "%7s " % f"{n:,}"),
                             (AGENT, bar[:filled]), (GRID, bar[filled:])],
                            w - 1))
    rows.append("")
    rows.append(seg([(DIM, "  Authorship, not spend: this is how much code the"
                           " agent wrote,")], w - 1))
    rows.append(seg([(DIM, "  which is a different question from what it"
                           " cost.")], w - 1))
    return rows


def elsewhere_tab(name, installed, w, h):
    # Every agent in AGENTS now has a reader, so this is a backstop for one
    # added without one - it says so rather than raising in the draw loop.
    label, lines = ELSEWHERE.get(
        name, (name, ["No reader for this agent yet."]))
    have = (installed.get(name) or {}).get("present")
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
    # One offset per tab. Switching away and back keeps your place, which
    # matters when a tab is forty rows and you were reading the bottom of it.
    offsets = {}

    while True:
        # Scrolling is applied after the frame is built, not here: a page is
        # however many body rows this pane turned out to have, and that is
        # not known until the tab has been rendered and the footer packed.
        moves = []
        for key in keyboard.poll():
            if key in ("q", "Q"):
                raise SystemExit(0)
            if key in ("right", "tab", "l"):
                active += 1
            elif key in ("left", "h"):
                active -= 1
            elif key in ("up", "k"):
                moves.append(-1)
            elif key in ("down", "j"):
                moves.append(1)
            elif key == "pgup":
                moves.append("-page")
            elif key == "pgdn":
                moves.append("+page")
            elif key == "home":
                moves.append("top")
            elif key == "end":
                moves.append("bottom")
            elif key == "r":
                store.wake.set()

        w, h = size()
        (claude, cursor, codex, grok, copilot,
         installed, fetched, err) = store.snapshot()
        rows = [title("agent usage", w, AGENT)]
        tabs = visible_agents(installed)
        active %= len(tabs)          # wraps in both directions
        extra = [n for n in ORDER
                 if (installed.get(n) or {}).get("present") and n not in tabs]
        def status(where=""):
            # The scroll position goes last on this line but matters most,
            # so the legend stands down to make room rather than letting it
            # be clipped - which is what had been happening at 58 columns,
            # leaving the footer offering a scroll with nothing saying where
            # in the tab you were.
            base = " local state · live quota · read %s ago" % ago(fetched)
            hidden = "   %d hidden by config" % len(extra) if extra else ""
            legend = "   · = detected"
            if len(base) + len(hidden) + len(legend) + len(where) > w - 1:
                legend = ""
            return seg([(DIM, base), (DIM, legend), (DIM, hidden),
                        (ACCENT, where)], w - 1)

        status_at = len(rows)        # filled in once the scroll is resolved
        rows.append(status())
        gripe = err or config_complaints(installed)
        if gripe:
            rows.append(seg([(BAD, " ! " + gripe)], w - 1))
        rows.append(tab_bar(tabs[active], installed, tabs, w))
        rows.append("")

        # Every agent ends on the same section, in the same place. Which
        # subscription a quota belongs to is context for the whole tab, not
        # the headline, so it sits under the numbers it explains - and it is
        # appended here rather than by five tabs that each end differently.
        name = tabs[active]
        sub = []
        if name == "claude":
            body = claude_tab(claude, w, h)
            if claude.get("profile"):
                sub = claude_plan_rows(claude["profile"], w)
        elif name == "cursor":
            body = cursor_tab(cursor, w, h)
            sub = cursor_plan_rows(cursor.get("plan"), w)
        elif name == "codex":
            body = codex_tab(codex, w, h)
            sub = codex_plan_rows(codex, w)
        elif name == "grok":
            body = grok_tab(grok, w, h)
            sub = grok_plan_rows(grok.get("quota"), w)
        elif name == "copilot":
            body = copilot_tab(copilot, w, h)
            sub = copilot_plan_rows(copilot.get("live") or {}, w)
        else:
            body = elsewhere_tab(name, installed, w, h)
        body = last_section(body, sub)

        hints = [[(ACCENT, "←→"), (DIM, " agent")],
                 [(ACCENT, "↑↓"), (DIM, " scroll")],
                 [(DIM, "[r]efresh")], [(DIM, "[q]uit")]]
        # The footer is packed once, with the scroll hint always counted, so
        # the body's height does not change when scrolling becomes possible.
        # Sizing it against a shorter footer and then growing the footer would
        # move the fold under the reader's cursor.
        reserved = len(pack_hints(hints, w - 2))
        avail = max(1, h - len(rows) - reserved)
        top = max(0, len(body) - avail)
        off = min(offsets.get(name, 0), top)
        for move in moves:
            if move == "top":
                off = 0
            elif move == "bottom":
                off = top
            elif move == "-page":
                off -= max(1, avail - 1)
            elif move == "+page":
                off += max(1, avail - 1)
            else:
                off += move
        off = max(0, min(off, top))
        offsets[name] = off

        view = body[off:off + avail]
        if top:
            # Never let a partial view read as the whole tab, and say which
            # way there is more: an arrow that is simply absent at the top of
            # a long tab looks the same as a tab that ends there.
            rows[status_at] = status("   %d-%d of %d %s%s"
                                     % (off + 1, off + len(view), len(body),
                                        "▲" if off else " ",
                                        "▼" if off < top else " "))
        else:
            hints = [x for x in hints if x[0][1] != "↑↓"]
        footer = [" " + line for line in pack_hints(hints, w - 2)]
        # Padded back to the height already reserved, so dropping the scroll
        # hint does not lift the footer off the bottom of the pane.
        footer = [""] * (reserved - len(footer)) + footer
        rows += view
        while len(rows) < h - len(footer):
            rows.append("")
        rows.extend(footer)
        draw(rows[:h], w, h)
        time.sleep(0.3)


main()
