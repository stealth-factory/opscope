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
"""Multi-target latency monitor.

Continuously pings every target, and shows per-target statistics, a per-target
sparkline, a shared log-scale time graph, and a log of loss/spike events.

    python3 latency.py [-i SECONDS] [-c SECONDS] [host ...]

Keys while running: i cycles the ping interval (0.2/0.5/1/2/5s, applied to
running pings immediately), g cycles the column aggregation, c cycles seconds
per graph column, q quits.

-i sets the ping interval. -g picks how samples sharing a column combine
(median, mean, min, max, p95; median by default, because latency is
right-skewed and a mean lets one spike misrepresent the whole block).
-c sets how many seconds each graph column covers;
the default of one column per ping gives the smoothest motion, while a larger
value trades that for a longer visible history.

Traffic cost: one 98-byte frame each way per target per interval. At the 1.0s
default with 4 targets that is ~0.8 KB/s (~2.8 MB/hour).

Measures THIS host -> each target. It cannot measure target-to-target legs;
that needs a probe running on the far end.
"""
import collections
import math
import os
import re
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, cycle, draw, load_config, maybe_help, pad,
                    rgb, seg, setup, size, title)

# Defaults are deliberately generic: personal targets belong in config.json,
# which is git-ignored, so this file stays publishable.
_CFG = load_config("latency", {
    "hosts": ["1.1.1.1", "8.8.8.8"],
    "interval": 0.5,
    "seconds_per_column": 0,
    "window": 600,
    "spike_factor": 3.0,
    "aggregate": "median",
    "strip_suffixes": [],
})

DEFAULT_HOSTS = list(_CFG["hosts"])
INTERVAL = float(_CFG["interval"])   # seconds between pings; -i overrides
SECONDS_PER_COLUMN = float(_CFG["seconds_per_column"])
WINDOW = int(_CFG["window"])         # samples retained per target
SPIKE_FACTOR = float(_CFG["spike_factor"])
STRIP_SUFFIXES = list(_CFG["strip_suffixes"])

AGGREGATE = _CFG["aggregate"]        # how samples sharing a graph column combine
AGGREGATORS = ("median", "mean", "min", "max", "p95")

# runtime key bindings cycle through these
INTERVAL_CHOICES = (0.2, 0.5, 1.0, 2.0, 5.0)
COLUMN_CHOICES = (0, 2.0, 5.0, 10.0)
RESTART = threading.Event()   # set when INTERVAL changes; readers relaunch ping

PALETTE = [(90, 220, 255), (255, 170, 80), (140, 255, 160),
           (230, 140, 255), (255, 110, 130), (255, 230, 110),
           (120, 160, 255), (255, 140, 200), (150, 255, 240)]
DIM = rgb(70, 100, 120)
GRID = rgb(38, 58, 74)
TXT = rgb(215, 235, 250)
LBL = rgb(120, 170, 200)
GOOD = rgb(110, 255, 170)
WARN = rgb(255, 200, 90)
BAD = rgb(255, 95, 105)
SPARK = "▁▂▃▄▅▆▇█"

TIME_RE = re.compile(r"time[=<]([\d.]+)\s*ms")
IP_RE = re.compile(r"^PING\s+\S+\s+\(([\d.a-fA-F:]+)\)")


def short(host):
    """Trim configured suffixes so long FQDNs stay readable in a narrow pane."""
    for suffix in STRIP_SUFFIXES:
        if host.endswith(suffix):
            return host[:-len(suffix)]
    return host


def fmt_ms(v):
    if v is None:
        return "   --  "
    if v < 1.0:
        return "%5.0fµs" % (v * 1000.0)
    if v < 100:
        return "%5.2fms" % v
    return "%5.1fms" % v


def pct(v):
    return "%5.1f%%" % v


def aggregate(values, how=None):
    """Combine samples that share one graph column.

    Median by default: latency is right-skewed, so a single spike inside a
    bucket would drag a mean well above the latency actually experienced most
    of the time.
    """
    how = how or AGGREGATE
    ordered = sorted(values)
    n = len(ordered)
    if n == 1:
        return ordered[0]
    if how == "mean":
        return sum(ordered) / n
    if how == "min":
        return ordered[0]
    if how == "max":
        return ordered[-1]
    if how == "p95":
        return ordered[min(n - 1, int(n * 0.95))]
    mid = n // 2
    return ordered[mid] if n % 2 else (ordered[mid - 1] + ordered[mid]) / 2.0


class Target(object):
    def __init__(self, host, palette_rgb):
        self.host = host
        self.color = rgb(*palette_rgb)
        # dimmed variant, used for the min-max spread band behind the line
        self.band = rgb(*[int(c * 0.42) for c in palette_rgb])
        self.ip = None
        self.samples = collections.deque(maxlen=WINDOW)   # (t, rtt|None)
        self.lock = threading.Lock()
        self.alive = False
        self.proc = None          # live ping process, so the interval can change
        self.restarting = False   # True = deliberate relaunch, not an outage

    def add(self, rtt):
        with self.lock:
            self.samples.append((time.time(), rtt))
            self.alive = rtt is not None

    def snapshot(self):
        with self.lock:
            return list(self.samples)

    def stats(self):
        s = self.snapshot()
        got = [r for _, r in s if r is not None]
        lost = sum(1 for _, r in s if r is None)
        total = len(s)
        if not got:
            return {"now": None, "avg": None, "min": None, "max": None,
                    "jit": None, "p95": None, "loss": 100.0 if total else 0.0,
                    "n": total, "med": None}
        ordered = sorted(got)
        jit = 0.0
        if len(got) > 1:
            jit = sum(abs(got[i] - got[i - 1]) for i in range(1, len(got))) / (len(got) - 1)
        return {
            "now": s[-1][1],
            "avg": sum(got) / len(got),
            "min": ordered[0],
            "max": ordered[-1],
            "jit": jit,
            "p95": ordered[min(len(ordered) - 1, int(len(ordered) * 0.95))],
            "med": ordered[len(ordered) // 2],
            "loss": 100.0 * lost / total if total else 0.0,
            "n": total,
        }


EVENTS = collections.deque(maxlen=40)
EV_LOCK = threading.Lock()


def log_event(color, host, kind, detail):
    with EV_LOCK:
        EVENTS.append((time.strftime("%H:%M:%S"), color, host, kind, detail))


def reader(t):
    """Run ping forever, feeding samples into the target."""
    down_since = None
    while True:
        try:
            proc = subprocess.Popen(
                ["ping", "-n", "-O", "-i", str(INTERVAL), t.host],
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                text=True, bufsize=1)
            t.proc = proc
        except OSError:
            time.sleep(2)
            continue
        for line in proc.stdout:
            m = IP_RE.match(line)
            if m:
                t.ip = m.group(1)
                continue
            m = TIME_RE.search(line)
            if m:
                rtt = float(m.group(1))
                st = t.stats()
                if st["med"] and rtt > st["med"] * SPIKE_FACTOR and st["n"] > 10:
                    log_event(t.color, t.host, "SPIKE", "%s (median %s)" %
                              (fmt_ms(rtt), fmt_ms(st["med"])))
                if down_since is not None:
                    log_event(t.color, t.host, "UP", "recovered after %.0fs" %
                              (time.time() - down_since))
                    down_since = None
                t.add(rtt)
            elif "no answer yet" in line or "Unreachable" in line or "100% packet loss" in line:
                if down_since is None:
                    down_since = time.time()
                    log_event(t.color, t.host, "LOSS", "no reply")
                t.add(None)
        proc.wait()
        if t.restarting:
            # we killed it ourselves to apply a new interval; not an outage
            t.restarting = False
            continue
        # ping exited (bad name, network gone) - record loss and retry
        if down_since is None:
            down_since = time.time()
            log_event(t.color, t.host, "DOWN", "ping exited, retrying")
        t.add(None)
        time.sleep(2)


def apply_interval(targets):
    """Restart every ping so a new INTERVAL takes effect immediately."""
    for t in targets:
        t.restarting = True
        if t.proc and t.proc.poll() is None:
            try:
                t.proc.terminate()
            except OSError:
                t.restarting = False


def sparkline(samples, n):
    pts = samples[-n:]
    got = [r for _, r in pts if r is not None]
    if not got:
        return BAD + "×" * min(n, len(pts))
    lo, hi = min(got), max(got)
    span = (hi - lo) or 1.0
    out = []
    last = None
    for _, r in pts:
        if r is None:
            if last != BAD:
                out.append(BAD)
                last = BAD
            out.append("×")
            continue
        frac = (r - lo) / span
        col = GOOD if frac < 0.5 else (WARN if frac < 0.85 else BAD)
        if col != last:
            out.append(col)
            last = col
        out.append(SPARK[min(7, int(frac * 7.99))])
    return "".join(out)


def build_graph(targets, gw, gh, bucket):
    """Log-scale multi-series plot.

    Columns are anchored to a fixed time grid (floor(ts / bucket)) rather than
    measured backwards from `now`. A sample therefore never migrates between
    columns, so the plot steps left exactly once per bucket instead of
    jittering as `now` slides. With bucket == INTERVAL every ping advances the
    plot by one column, the finest motion a character grid allows.

    Consecutive samples are joined into a polyline, so each series reads as a
    continuous trace rather than scattered dots.

    Column gw-1 is 'now'. Returns (rows, span_seconds).
    """
    newest = int(math.floor(time.time() / bucket))
    series = {}
    lo = hi = None
    for t in targets:
        cols = [None] * gw
        for ts, r in t.snapshot():
            if r is None:
                continue
            idx = gw - 1 - (newest - int(math.floor(ts / bucket)))
            if 0 <= idx < gw:
                if cols[idx] is None:
                    cols[idx] = []
                cols[idx].append(r)
        # each column -> (central value, bucket min, bucket max)
        vals = [None if c is None else (aggregate(c), min(c), max(c)) for c in cols]
        series[t.host] = vals
        for v in vals:
            if v is None:
                continue
            lo = v[1] if lo is None else min(lo, v[1])
            hi = v[2] if hi is None else max(hi, v[2])
    if lo is None:
        return [DIM + " collecting…"], bucket * gw
    lo = max(0.05, lo * 0.8)
    hi = max(hi * 1.25, lo * 1.6)
    llo, lhi = math.log10(lo), math.log10(hi)

    grid = [[" "] * gw for _ in range(gh)]
    color = [[None] * gw for _ in range(gh)]

    def put(x, y, ch, col):
        if 0 <= x < gw and 0 <= y < gh:
            grid[y][x] = ch
            color[y][x] = col

    def row_of(v):
        frac = (math.log10(max(v, 1e-3)) - llo) / (lhi - llo)
        return int(round((1.0 - frac) * (gh - 1)))

    # pass 1: min-max spread inside each bucket, dimmed, behind everything
    for t in reversed(targets):
        for x, v in enumerate(series[t.host]):
            if v is None:
                continue
            top, bot = row_of(v[2]), row_of(v[1])
            if top == bot:
                continue                  # spread smaller than one row
            for y in range(min(top, bot), max(top, bot) + 1):
                put(x, y, "│", t.band)

    # pass 2: the central-value polyline, drawn over the bands
    for t in reversed(targets):           # first host in list drawn last (wins)
        pts = [(x, row_of(v[0])) for x, v in enumerate(series[t.host]) if v is not None]
        for i, (x, y) in enumerate(pts):
            if i:
                x0, y0 = pts[i - 1]
                prev = y0
                for xx in range(x0 + 1, x + 1):
                    f = (xx - x0) / float(x - x0)
                    yy = int(round(y0 + (y - y0) * f))
                    for k in range(min(prev, yy), max(prev, yy) + 1):
                        put(xx, k, "·" if k == yy else "│", t.color)
                    prev = yy
            put(x, y, "●", t.color)

    rows = []
    for y in range(gh):
        frac = 1.0 - y / float(gh - 1) if gh > 1 else 1.0
        tick = 10 ** (llo + frac * (lhi - llo))
        label = fmt_ms(tick) if y % 3 == 0 else "       "
        parts = [(LBL, label), (GRID, "│")]
        run_col, run = None, []
        for x in range(gw):
            c = color[y][x]
            ch = grid[y][x]
            if ch == " ":
                ch = "·" if (x % 12 == 0 and y % 3 == 0) else " "
                c = GRID if ch != " " else None
            if c != run_col:
                if run:
                    parts.append((run_col or RST, "".join(run)))
                run_col, run = c, []
            run.append(ch)
        if run:
            parts.append((run_col or RST, "".join(run)))
        rows.append(seg(parts, gw + 8))
    return rows, bucket * gw


def main():
    maybe_help(__doc__)
    global INTERVAL, SECONDS_PER_COLUMN, AGGREGATE
    args = sys.argv[1:]
    while args and args[0] in ("-i", "--interval", "-c", "--column-seconds",
                               "-g", "--group"):
        if args[0] in ("-i", "--interval"):
            INTERVAL = max(0.2, float(args[1]))
        elif args[0] in ("-c", "--column-seconds"):
            SECONDS_PER_COLUMN = max(0.0, float(args[1]))
        else:
            if args[1] not in AGGREGATORS:
                raise SystemExit("-g must be one of: " + ", ".join(AGGREGATORS))
            AGGREGATE = args[1]
        args = args[2:]
    hosts = args or DEFAULT_HOSTS
    targets = [Target(h, PALETTE[i % len(PALETTE)]) for i, h in enumerate(hosts)]
    setup()
    keyboard = Keyboard()
    for t in targets:
        th = threading.Thread(target=reader, args=(t,))
        th.daemon = True
        th.start()

    while True:
        for key in keyboard.poll():
            if key in ("q", "Q"):
                keyboard.restore()
                raise SystemExit(0)
            if key == "i":
                INTERVAL = cycle(INTERVAL_CHOICES, INTERVAL)
                apply_interval(targets)
            elif key == "g":
                AGGREGATE = cycle(AGGREGATORS, AGGREGATE)
            elif key == "c":
                SECONDS_PER_COLUMN = cycle(COLUMN_CHOICES, SECONDS_PER_COLUMN)

        w, h = size()
        bucket = SECONDS_PER_COLUMN or INTERVAL
        rows = [title("network latency monitor", w, rgb(90, 220, 255))]
        rows.append(seg([(DIM, " %d targets · %.1fs interval · " % (len(targets), INTERVAL)),
                         (TXT, time.strftime("%H:%M:%S")),
                         (DIM, " · " + ("1 ping/column" if bucket <= INTERVAL
                                        else "%s of %gs blocks" % (AGGREGATE, bucket))),
                         (GRID, "   [i]nterval [g]roup [c]olumns [q]uit"
                                if keyboard.fd is not None else "")],
                        w - 1))
        rows.append("")
        wide = w >= 72
        show_med = w >= 80
        head = " %-22s %7s %7s%s %7s %7s %7s %6s" % (
            "HOST", "NOW", "AVG", "  MEDIAN" if show_med else "",
            "MIN", "MAX", "JITTER", "LOSS")
        rows.append(LBL + pad(head, w - 1))
        for t in targets:
            st = t.stats()
            dot = GOOD + "●" if t.alive else BAD + "○"
            name = short(t.host)
            lossc = GOOD if st["loss"] == 0 else (WARN if st["loss"] < 5 else BAD)
            rows.append(seg([(dot, " "), (t.color, pad(name, 22)),
                             (TXT, " " + fmt_ms(st["now"])),
                             (TXT, " " + fmt_ms(st["avg"])),
                             (GOOD, (" " + fmt_ms(st["med"])) if show_med else ""),
                             (DIM, " " + fmt_ms(st["min"])),
                             (DIM, " " + fmt_ms(st["max"])),
                             (TXT, " " + fmt_ms(st["jit"])),
                             (lossc, " " + pct(st["loss"]))], w - 1))
            if wide:
                rows.append("   " + sparkline(t.snapshot(), w - 6))
        rows.append("")

        ev_h = 7 if h - len(rows) > 20 else 0
        gh = max(4, h - len(rows) - ev_h - 4)
        gw = max(10, w - 10)
        graph, gspan = build_graph(targets, gw, gh, bucket)
        rows.extend(graph)
        rows.append(LBL + "       └" + GRID + "─" * gw)
        # The plot occupies columns 8 .. 8+gw-1, so the axis labels must span
        # exactly gw cells: oldest flush left, "now" flush right under the
        # newest sample.
        left = "%ds ago" % int(gspan)
        if len(left) + 4 > gw:
            left = ""
        rows.append(DIM + " " * 8 + left + " " * (gw - len(left) - 3) + "now")
        rows.append("")

        if ev_h:
            rows.append(DIM + " ── EVENTS ──")
            with EV_LOCK:
                evs = list(EVENTS)[-(ev_h - 1):]
            if not evs:
                rows.append(DIM + "   (no loss or spikes recorded)")
            for ts, col, host, kind, detail in evs:
                kc = BAD if kind in ("LOSS", "DOWN") else (WARN if kind == "SPIKE" else GOOD)
                rows.append(seg([(DIM, " " + ts + " "), (kc, "%-6s" % kind),
                                 (col, pad(short(host), 22)),
                                 (DIM, detail)], w - 1))
        draw(rows, w, h)
        time.sleep(0.5)


main()
