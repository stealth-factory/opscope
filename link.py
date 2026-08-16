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
"""How good the connection is between here and whoever is connected to it.

Every other network widget in this repo measures a path it chose - ping these
hosts, watch that tailnet. This one measures the path *you* are on: the TCP
socket carrying your session, as the kernel already sees it.

Nothing is sent. `ss -tin` reports what the kernel has measured for each
established socket - round-trip time and its variance, the best round trip it
has ever seen, retransmitted bytes, the delivery rate it actually achieved -
so this widget can describe the link without adding a single packet to it.

    python3 link.py [-n SECONDS]

Sessions are every established connection into a port this machine listens on,
which is SSH and anything else that accepts terminals. Keys: up/down select,
o toggles idle sessions, r refreshes, q quits.
"""
import collections
import os
import re
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, bg, draw, heat, load_config, maybe_help,
                    pack_hints, pad, rgb, seg, setup, size, title, vbars)

_CFG = load_config("link", {
    # Every established connection into a port we listen on. Naming ports
    # instead pins the set - useful if something else on this machine accepts
    # connections you would rather not watch.
    "ports": [],
    "refresh": 2,
    "history": 120,
})

REFRESH = max(0.5, float(_CFG["refresh"]))
HISTORY = int(_CFG["history"])
PORTS = [int(p) for p in (_CFG["ports"] or [])]

OK = rgb(90, 240, 160)
WARN = rgb(255, 200, 90)
BAD = rgb(255, 100, 110)
DIM = rgb(127, 147, 172)
GRID = rgb(60, 78, 98)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
ACCENT = rgb(150, 210, 255)
LINK = rgb(140, 200, 255)
# One hue per session, distinct from each other and clear of the amber and
# red this widget keeps for trouble.
SESSION_HUES = (rgb(120, 200, 255), rgb(150, 230, 180), rgb(220, 170, 255),
                rgb(160, 190, 240), rgb(200, 220, 150), rgb(240, 180, 210))

IDLE_AFTER = 300.0        # seconds without traffic before a session is idle
SPARK = "▁▂▃▄▅▆▇█"


def run(args):
    try:
        out = subprocess.run(args, capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError):
        return ""
    return out.stdout if out.returncode == 0 else ""


def listening_ports():
    """Ports this machine accepts connections on.

    Inbound is defined as "arrived at a port we listen on" rather than by a
    list of port numbers, so SSH, a terminal server and anything else that
    accepts sessions are all found without being named.
    """
    ports = set(PORTS)
    for line in run(["ss", "-tlnH"]).splitlines():
        cols = line.split()
        if len(cols) >= 4:
            try:
                ports.add(int(cols[3].rsplit(":", 1)[1]))
            except (ValueError, IndexError):
                continue
    return ports


def parse_metrics(text):
    """The kernel's own numbers for one socket.

    `ss` mixes two shapes on that line: `key:value` pairs and space-separated
    ones like `delivery_rate 45107960bps`. Both are read; anything unknown is
    left alone rather than guessed at.
    """
    out = {}
    for key in ("send", "pacing_rate", "delivery_rate"):
        found = re.search(r"\b%s (\d+)bps" % key, text)
        if found:
            out[key] = int(found.group(1))
    for token in text.split():
        if ":" not in token:
            continue
        key, _, value = token.partition(":")
        out[key] = value
    return out


def num(value):
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def sessions():
    """One entry per established inbound connection, with its metrics."""
    ports = listening_ports()
    if not ports:
        return []
    text = run(["ss", "-tinH", "state", "established"])
    if not text:
        return []
    found, head = [], None
    for line in text.splitlines():
        if not line.startswith(("\t", " ")):
            head = line.split()
            continue
        if head is None or len(head) < 4:
            continue
        local, peer = head[2], head[3]
        try:
            lport = int(local.rsplit(":", 1)[1])
        except (ValueError, IndexError):
            head = None
            continue
        peer_ip = peer.rsplit(":", 1)[0].strip("[]")
        # ::ffff:10.0.0.1 is an IPv4 address wearing an IPv6 hat - the same
        # machine, the same session - so it is unwrapped before anything
        # else looks at it. Left wrapped, ::ffff:127.0.0.1 walked straight
        # past the loopback filter and put a 22-microsecond local socket on
        # the chart, which flattened every real session against the ceiling.
        if peer_ip.startswith("::ffff:"):
            peer_ip = peer_ip[7:]
        if lport not in ports or peer_ip.startswith(("127.", "::1")):
            head = None
            continue
        m = parse_metrics(line)
        rtt = (m.get("rtt") or "").split("/")
        found.append({
            "peer": "%s:%s" % (peer_ip, peer.rsplit(":", 1)[1]),
            "ip": peer_ip, "port": lport,
            "rtt": num(rtt[0]) if rtt else None,
            "jitter": num(rtt[1]) if len(rtt) > 1 else None,
            "floor": num(m.get("minrtt")),
            "sent": num(m.get("bytes_sent")) or 0.0,
            "recv": num(m.get("bytes_received")) or 0.0,
            "retrans_bytes": num(m.get("bytes_retrans")) or 0.0,
            "delivery": num(m.get("delivery_rate")),
            "cwnd": num(m.get("cwnd")),
            "mss": num(m.get("mss")),
            "lastsnd": num(m.get("lastsnd")),
            "lastrcv": num(m.get("lastrcv")),
        })
        head = None
    return found


def who():
    """Who is logged in from where, to put a name against an address."""
    seen = {}
    for line in run(["who"]).splitlines():
        cols = line.split()
        if len(cols) < 2:
            continue
        host = cols[-1].strip("()") if cols[-1].startswith("(") else ""
        if host:
            seen.setdefault(host, []).append((cols[0], cols[1]))
    return seen


def rate(n):
    if n is None:
        return "--"
    for unit, size in (("Gbps", 1e9), ("Mbps", 1e6), ("kbps", 1e3)):
        if n >= size:
            return "%.1f%s" % (n / size, unit)
    return "%dbps" % n


def size_of(n):
    n = float(n or 0)
    for unit, step in (("G", 1e9), ("M", 1e6), ("k", 1e3)):
        if n >= step:
            return "%.1f%s" % (n / step, unit)
    return "%dB" % n


def span(ms):
    if ms is None:
        return "--"
    s = ms / 1000.0
    if s < 60:
        return "%ds" % s
    if s < 3600:
        return "%dm" % (s // 60)
    if s < 86400:
        return "%dh" % (s // 3600)
    return "%dd" % (s // 86400)


def sparkline(values, n):
    """RTT over time, one cell per sample, newest at the right."""
    if not values:
        return ""
    tail = list(values)[-n:]
    hi = max(tail) or 1.0
    return "".join(SPARK[min(7, int(v / hi * 7.99))] for v in tail)


class Store(object):
    def __init__(self):
        self.lock = threading.Lock()
        self.rows = []
        self.names = {}
        self.error = None
        self.fetched = 0
        self.wake = threading.Event()
        self.history = collections.defaultdict(
            lambda: collections.deque(maxlen=HISTORY))
        self.last = {}

    def snapshot(self):
        with self.lock:
            return (list(self.rows), dict(self.names),
                    {k: list(v) for k, v in self.history.items()},
                    self.fetched, self.error)

    def run(self):
        # A daemon thread that raises just stops, and a dead poller looks
        # exactly like a machine with nobody connected to it.
        try:
            self.poll()
        except Exception as e:
            with self.lock:
                self.error = "poller stopped: %s: %s" % (type(e).__name__,
                                                         str(e)[:70])

    def poll(self):
        while True:
            rows = sessions()
            names = who()
            for row in rows:
                key = row["peer"]
                if row["rtt"] is not None:
                    self.history[key].append(row["rtt"])
                # Retransmits since the last look, rather than since the
                # connection opened: a session hours old has long since
                # forgiven whatever went wrong at breakfast.
                prev = self.last.get(key)
                if prev:
                    d_sent = row["sent"] - prev["sent"]
                    d_retrans = row["retrans_bytes"] - prev["retrans_bytes"]
                    row["recent_loss"] = (100.0 * d_retrans / d_sent
                                          if d_sent > 0 else 0.0)
                    row["moved"] = d_sent + (row["recv"] - prev["recv"])
                self.last[key] = dict(row)
                row["spark"] = sparkline(self.history[key], 0) or ""
            with self.lock:
                self.rows = rows
                self.names = names
                self.fetched = time.time()
                self.error = None if rows else self.error
            self.wake.wait(REFRESH)
            self.wake.clear()


def quality(row):
    """How much worse than this path's best the connection is right now.

    Compared against the socket's own minrtt rather than a fixed threshold:
    forty milliseconds is excellent from Hong Kong and poor from the next
    rack, and the kernel already knows which this is.
    """
    rtt, floor = row.get("rtt"), row.get("floor")
    if not rtt or not floor:
        return None
    return rtt / floor


def colour_for(ratio, loss):
    if loss is not None and loss >= 2.0:
        return BAD
    if ratio is None:
        return DIM
    if ratio >= 3.0 or (loss or 0) >= 0.5:
        return BAD
    if ratio >= 1.6:
        return WARN
    return OK


SERIES = "●▲■◆✚✦"          # one glyph per session, so the plot reads mono


def build_graph(rows, history, gw, gh):
    """Log-scale multi-series plot of round-trip time.

    Log because the sessions on one machine can differ by two orders of
    magnitude - a laptop on the same continent and a phone on the other side
    of it - and a linear axis renders the near one as a flat line at the
    bottom. One column per sample rather than per second: this widget's
    samples are whatever the kernel had at each poll, and pretending to a
    finer time grid would be inventing resolution.
    """
    lo = hi = None
    series = []
    for i, row in enumerate(rows):
        vals = list(history.get(row["peer"]) or [])[-gw:]
        if not vals:
            continue
        series.append((i, vals))
        lo = min(vals) if lo is None else min(lo, min(vals))
        hi = max(vals) if hi is None else max(hi, max(vals))
    if lo is None:
        return [seg([(DIM, "  collecting…")], gw)], None, None
    lo = max(0.05, lo * 0.8)
    hi = max(hi * 1.25, lo * 1.6)
    import math
    llo, lhi = math.log10(lo), math.log10(hi)

    grid = [[" "] * gw for _ in range(gh)]
    tone = [[None] * gw for _ in range(gh)]

    def row_of(v):
        frac = (math.log10(max(v, 1e-3)) - llo) / (lhi - llo)
        return int(round((1.0 - frac) * (gh - 1)))

    for idx, vals in series:
        glyph = SERIES[idx % len(SERIES)]
        colour = SESSION_HUES[idx % len(SESSION_HUES)]
        start = gw - len(vals)
        prev = None
        for x, v in enumerate(vals):
            y = row_of(v)
            col = start + x
            if prev is not None and abs(prev - y) > 1:
                # join consecutive samples so a series reads as a trace
                for fill in range(min(prev, y) + 1, max(prev, y)):
                    if grid[fill][col] == " ":
                        grid[fill][col] = "│"
                        tone[fill][col] = colour
            if 0 <= col < gw and 0 <= y < gh:
                grid[y][col] = glyph
                tone[y][col] = colour
            prev = y
    return grid, tone, (lo, hi)


def graph_rows(rows, history, w, h):
    gw = max(10, w - 9)
    gh = max(4, h)
    grid, tone, bounds = build_graph(rows, history, gw, gh)
    if bounds is None:
        return grid
    import math
    lo, hi = bounds
    llo, lhi = math.log10(lo), math.log10(hi)
    out = []
    for y, line in enumerate(grid):
        frac = 1.0 - (y / float(max(1, gh - 1)))
        value = 10 ** (llo + frac * (lhi - llo))
        # label only the top, middle and bottom: a number on every row is a
        # table pretending to be an axis
        label = ("%7s" % ms(value)) if y in (0, gh // 2, gh - 1) else " " * 7
        parts = [(DIM, label), (GRID, "│")]
        for x, ch in enumerate(line):
            parts.append((tone[y][x] or GRID, ch))
        out.append(seg(parts, w - 1))
    return out


def ms(v):
    if v >= 100:
        return "%dms" % round(v)
    if v >= 10:
        return "%.0fms" % v
    if v >= 1:
        return "%.1fms" % v
    return "%dµs" % round(v * 1000)


def table_rows(rows, names, history, w, selected):
    """One line per session: what it is, and how it is behaving."""
    wide = w >= 74
    out = [seg([(DIM, "  PEER"), (DIM, " " * 14),
                (DIM, "    NOW   FLOOR  JITTER    LOSS"),
                (DIM, "  ACHIEVED" if wide else ""),
                (DIM, "   IDLE" if wide else "")], w - 1)]
    for i, row in enumerate(rows):
        here = i == selected
        tint = bg(28, 44, 62) if here else ""
        ratio = quality(row)
        loss = row.get("recent_loss")
        tone = colour_for(ratio, loss)
        users = names.get(row["ip"]) or []
        who_txt = users[0][0] if users else ""
        idle = min([x for x in (row.get("lastsnd"), row.get("lastrcv"))
                    if x is not None] or [None])
        label = row["ip"] + (" %s" % who_txt if who_txt and wide else "")
        line = [(tint + SESSION_HUES[i % len(SESSION_HUES)],
                 SERIES[i % len(SERIES)] + " "),
                (tint + (TXT if here else DIM), pad(label, 18)),
                (tint + tone, "%7s" % ms(row["rtt"]) if row["rtt"] else "     --"),
                (tint + DIM, "%8s" % ms(row["floor"]) if row["floor"] else "      --"),
                (tint + DIM, "%8s" % ms(row["jitter"]) if row["jitter"] else "      --"),
                (tint + (BAD if (loss or 0) >= 0.5 else DIM),
                 "%7s" % ("%.2f%%" % loss if loss is not None else "--"))]
        if wide:
            line.append((tint + DIM, "%10s" % rate(row.get("delivery"))))
            line.append((tint + DIM, "%7s" % span(idle)))
        if here:
            line.append((tint, " " * w))
        out.append(seg(line, w - 1))
    return out


def main():
    maybe_help(__doc__)
    global REFRESH
    args = sys.argv[1:]
    while args and args[0] in ("-n", "--refresh"):
        REFRESH = max(0.5, float(args[1]))
        args = args[2:]

    if not run(["ss", "-V"]):
        sys.stderr.write("link.py needs `ss` (iproute2) to read socket "
                         "metrics; it is not on PATH.\n")
        raise SystemExit(1)

    setup()
    keyboard = Keyboard()
    store = Store()
    threading.Thread(target=store.run, daemon=True).start()
    selected, hide_idle, tick = 0, False, 0

    while True:
        tick += 1
        for key in keyboard.poll():
            if key in ("q", "Q"):
                raise SystemExit(0)
            if key in ("up", "k"):
                selected -= 1
            elif key in ("down", "j"):
                selected += 1
            elif key == "o":
                hide_idle = not hide_idle
            elif key == "r":
                store.wake.set()

        w, h = size()
        rows_all, names, history, fetched, err = store.snapshot()
        shown = [r for r in rows_all
                 if not (hide_idle and (r.get("lastrcv") or 0) > IDLE_AFTER * 1000)]
        selected = max(0, min(selected, len(shown) - 1)) if shown else 0

        rows = [title("connections", w, LINK)]
        rows.append(seg([(DIM, " %d inbound" % len(rows_all)),
                         (DIM, " · measured by the kernel, nothing sent"),
                         (DIM, "   every %gs" % REFRESH)], w - 1))
        if err:
            rows.append(seg([(BAD, " ! " + err)], w - 1))
        rows.append("")

        if not rows_all:
            rows.append(seg([(DIM, "  No inbound sessions on a listening"
                                   " port.")], w - 1))
            rows.append(seg([(DIM, "  Nothing is connected to this machine,"
                                   " or `ss` cannot see it.")], w - 1))
        else:
            rows.extend(table_rows(shown, names, history, w, selected))
            rows.append("")
            # The chart takes whatever the table left, so a machine with one
            # session gets a tall graph and one with six still gets a graph.
            room = h - len(rows) - 4
            if room >= 5:
                rows.extend(graph_rows(shown, history, w, room))
                rows.append(seg([(DIM, " " * 7), (GRID, "└" + "─" * max(10, w - 9))],
                                w - 1))
                oldest = REFRESH * max([len(history.get(r["peer"]) or [])
                                        for r in shown] or [0])
                rows.append(seg([(DIM, "        %s ago" % span(oldest * 1000)),
                                 (DIM, " " * max(1, w - 26)),
                                 (DIM, "now")], w - 1))

        while len(rows) < h - 2:
            rows.append("")
        hints = [[(ACCENT, "↑↓"), (DIM, " select")],
                 [(DIM, "[o]%s idle" % ("show" if hide_idle else "hide"))],
                 [(DIM, "[r]efresh")], [(DIM, "[q]uit")]]
        for line in pack_hints(hints, w - 2):
            rows.append(" " + line)
        draw(rows, w, h)
        time.sleep(0.3)


main()
