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
import glob
import shutil
import sqlite3
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, bg, draw, heat, load_config, maybe_help, mix,
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

# One hue, four steps, the way /stats and the contribution calendar do it.
# heat() runs green to amber to red, which reads as a change of *kind* rather
# than of amount - wrong for "more of the same thing".
HEAT_STEPS = ((74, 52, 46), (140, 78, 58), (196, 100, 66), (240, 132, 84))
EMPTY_CELL = rgb(58, 66, 80)


def shade(frac):
    """Which of the four steps a day falls in."""
    return rgb(*HEAT_STEPS[min(3, max(0, int(frac * 3.999)))])


CLAUDE_STATS = os.path.expanduser("~/.claude/stats-cache.json")
CODEX_SESSIONS = os.path.expanduser("~/.codex/sessions/**/*.jsonl")
CLAUDE_TRANSCRIPTS = os.path.expanduser("~/.claude/projects/*/*.jsonl")
RATE_FILES = 3           # newest transcripts to sample for a rate
MIN_GAP = 1.0            # seconds; below this the timestamps are not a turn
COPILOT_DB = os.path.expanduser("~/.copilot/session-store.db")
TAIL = 256 * 1024        # enough to reach the last token_count in a rollout
CURSOR_DB = os.path.expanduser("~/.cursor/ai-tracking/ai-code-tracking.db")


# For the comparison line. A rough token count for War and Peace - the book is
# about 587k words, which lands near this once tokenised. Stated here because a
# comparison built on an unnamed constant is just a number with a story.
WAR_AND_PEACE_TOKENS = 730_000


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
    rates, sampled = claude_rates()
    return {"ok": True, "mtime": stat.st_mtime, "data": d,
            "rates": rates, "sampled": sampled}


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
    for path in files:
        last = None
        for line in tail_lines(path):
            if '"token_count"' not in line:
                continue
            try:
                d = json.loads(line)
            except ValueError:
                continue
            u = ((d.get("payload") or {}).get("info") or {}).get(
                "total_token_usage")
            if u:
                last = u
        if last:
            counted += 1
            for k in total:
                total[k] += last.get(k, 0)

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
            "total": total, "rates": rates,
            "last": os.path.getmtime(files[-1])}


def codex_tab(state, w, h):
    if not state.get("ok"):
        return [seg([(BAD, "  %s" % state.get("why"))], w - 1)]
    t = state["total"]
    rows = [seg([(LBL, " ── TOTALS ── "),
                 (DIM, "%d sessions · newest %s ago"
                  % (state["sessions"], ago(state["last"])))], w - 1)]
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
    rows.append("")
    rows.append(seg([(DIM, "  Rate is wall-clock between turn boundaries, so"
                           " it includes")], w - 1))
    rows.append(seg([(DIM, "  tool calls and thinking - not raw decode"
                           " speed.")], w - 1))
    return rows


# Agents whose usage exists but is not readable from outside their session.
# Naming where the number actually lives beats showing an empty gauge.
ELSEWHERE = {
    "copilot": ("GitHub Copilot",
                ["It does keep usage locally, in the session store at",
                 "~/.copilot/session-store.db. The assistant_usage_events",
                 "table carries per-turn input, output, cache and reasoning",
                 "tokens, AI credits as total_nano_aiu, and - uniquely among",
                 "these agents - duration_ms, time_to_first_token_ms and",
                 "inter_token_latency_ms.",
                 "",
                 "It is empty on this machine, so there is nothing to draw.",
                 "The moment the CLI is used here, this tab has real numbers",
                 "and better ones than anywhere else."]),

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
        self.codex = {}
        self.installed = {}
        self.fetched = 0
        self.wake = threading.Event()

    def snapshot(self):
        with self.lock:
            return (dict(self.claude), dict(self.cursor), dict(self.codex),
                    dict(self.installed), self.fetched)

    def run(self):
        while True:
            claude, cursor, codex = read_claude(), read_cursor(), read_codex()
            found = {}
            for name in ("claude", "codex", "copilot", "cursor-agent", "grok"):
                found[name] = bool(shutil.which(name))
            with self.lock:
                self.claude, self.cursor, self.codex = claude, cursor, codex
                self.installed = found
                self.fetched = time.time()
            self.wake.wait(REFRESH)
            self.wake.clear()


TABS = ("claude", "codex", "cursor", "grok", "copilot")


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


MONTHS = ("Jan", "Feb", "Mar", "Apr", "May", "Jun",
          "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")


def token_heatmap(daily, w):
    """Tokens per day, drawn the way Claude Code's own /stats draws it.

    Weekday rows with only Mon, Wed and Fri labelled; one cell per day; months
    named across the top; solid blocks in four steps of a single hue, and a dim
    dot for a day the file has no entry for.

    The grid spans the whole window even where there is no data, because the
    emptiness is information: this cache retains about four weeks, and a year
    of dots with a fortnight of colour at the right-hand end says that plainly.
    """
    totals = {}
    for entry in daily:
        try:
            day = datetime.date.fromisoformat(entry["date"])
        except (ValueError, KeyError):
            continue
        totals[day] = sum((entry.get("tokensByModel") or {}).values())
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
    strip = [" "] * len(starts)
    seen = None
    for x, wk in enumerate(starts):
        if wk.month != seen and x + 3 <= len(strip):
            seen = wk.month
            for k, ch in enumerate(MONTHS[wk.month - 1]):
                strip[x + k] = ch
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
                line.append((shade((n / float(peak)) ** 0.5), "█"))
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
    spoken = in_tok + out_tok
    if spoken > WAR_AND_PEACE_TOKENS:
        rows.append("")
        rows.append(seg([(ACCENT, "  Your input and output are ~%dx the tokens"
                          " in War and Peace"
                          % round(spoken / float(WAR_AND_PEACE_TOKENS)))],
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
    if grid and h > 26:
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
        claude, cursor, codex, installed, fetched = store.snapshot()
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
        elif name == "codex":
            rows += codex_tab(codex, w, h)
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
