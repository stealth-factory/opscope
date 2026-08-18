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
"""Which processes are using the network, how much, and how fast.

`nettop` answers this on macOS and has no equivalent here. What Linux does
have is the kernel's own per-socket accounting: `ss -tine` reports bytes_sent
and bytes_received for every TCP socket along with its inode, and the inode
appears in /proc/<pid>/fd, which is what ties bytes to a process. No packet
capture, no kernel module, no root.

    python3 netwatch.py [-i SECONDS] [-n COUNT] [--sort total|live]
                        [--external] [--plain]

Only traffic that leaves the machine is counted. Loopback is excluded, and so
is any connection to one of this machine's own addresses - talking to your own
10.x or tailnet address never reaches a wire, however external it looks in the
socket table. --external is the narrower question of internet-only, and drops
the local network and the tailnet too.

Totals start at zero: the first sample is a baseline and only what happens
after it is counted. A process that exits keeps what it used, marked so, since
"what has been eating the connection" is usually asked after the thing has
stopped.

TCP only, which is the honest limit of this method - see docs/netwatch.md.

Keys: 1 sorts by total, 2 by current rate, r rezeroes, q quits.
"""
import collections
import json
import os
import re
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (Keyboard, bg, draw, load_config, maybe_help, pack_hints,
                    pad, rgb, seg, setup, size, title)

_CFG = load_config("netwatch", {
    "interval": 1.0,
    "limit": 0,          # 0 fills the pane
    "sort": "total",
    "external": False,
})

INTERVAL = max(0.2, float(_CFG["interval"]))
LIMIT = int(_CFG["limit"])
SORT = _CFG["sort"] if _CFG["sort"] in ("total", "live") else "total"
EXTERNAL = bool(_CFG["external"])
PLAIN = False

OK = rgb(90, 240, 160)
WARN = rgb(255, 200, 90)
BAD = rgb(255, 100, 110)
DIM = rgb(127, 147, 172)
GRID = rgb(60, 78, 98)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
ACCENT = rgb(150, 210, 255)
DOWN = rgb(120, 200, 255)
UP = rgb(255, 170, 120)

INO = re.compile(r"\bino:(\d+)")
SENT = re.compile(r"\bbytes_sent:(\d+)")
RECV = re.compile(r"\bbytes_received:(\d+)")
# 10/8, 172.16/12, 192.168/16, 169.254/16 and Tailscale's 100.64/10 are all
# somewhere other than the internet.
PRIVATE = re.compile(r"^(10\.|192\.168\.|169\.254\.|172\.(1[6-9]|2\d|3[01])\.|"
                     r"100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.)")
UNATTRIBUTED = "(unattributed)"


def run(args):
    try:
        out = subprocess.run(args, capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError):
        return ""
    return out.stdout if out.returncode == 0 else ""


def units(n):
    """Decimal units, as network equipment and ISPs quote them."""
    n = float(n)
    for suffix, scale in (("GB", 1e9), ("MB", 1e6), ("KB", 1e3)):
        if n >= scale:
            return "%.1f %s" % (n / scale, suffix)
    return "%d B" % n


def rate(n):
    return units(n) + "/s" if n else "-"


def elapsed(seconds):
    seconds = int(seconds)
    if seconds < 60:
        return "%ds" % seconds
    if seconds < 3600:
        return "%dm %02ds" % (seconds // 60, seconds % 60)
    return "%dh %02dm" % (seconds // 3600, (seconds % 3600) // 60)


def host_of(addr):
    """The address out of ss's `addr:port`, brackets stripped from IPv6."""
    host, _, _ = addr.rpartition(":")
    return host.strip("[]")


_OWN = {"at": 0.0, "addrs": set()}


def own_addresses():
    """Every address this machine answers to, refreshed occasionally.

    A connection to one of our own addresses is turned around inside the
    kernel and never reaches a wire, so it is not traffic leaving the
    machine even though the address is not loopback. Interfaces come and go
    - a tailnet address arrives when tailscaled starts, a bridge when a
    container does - so this is re-read periodically rather than once.
    """
    now = time.time()
    if _OWN["addrs"] and now - _OWN["at"] < 30:
        return _OWN["addrs"]
    found = set()
    try:
        data = json.loads(run(["ip", "-j", "addr"]) or "[]")
    except ValueError:
        data = []
    for link in data:
        for addr in link.get("addr_info") or []:
            if addr.get("local"):
                found.add(addr["local"])
    if found:
        _OWN["addrs"] = found
        _OWN["at"] = now
    return _OWN["addrs"]


def local_peer(host):
    """Whether this traffic never leaves the machine.

    Loopback is the obvious half. The other is a connection to one of this
    machine's own addresses - 10.x to itself, or its own tailnet address -
    which looks external in the socket table and is not: the kernel routes
    it back up the stack without a packet ever reaching an interface.
    """
    if (host.startswith("127.") or host in ("::1", "*", "")
            or host.startswith("::ffff:127.")):
        return True
    bare = host[7:] if host.startswith("::ffff:") else host
    return bare in own_addresses()


def off_box(host):
    """Whether a peer is out on the internet rather than nearby."""
    if local_peer(host):
        return False
    if host.startswith("::ffff:"):
        host = host[7:]
    return not (PRIVATE.match(host) or host.startswith("fd7a:115c:a1e0")
                or host.startswith("fe80:") or host.startswith("fc")
                or host.startswith("fd"))


def sockets(external=False):
    """Every TCP socket's byte counters, keyed by inode.

    -i for the counters, -e for the inode. Without the inode there is no
    honest way to reach the process: `ss -p` needs root to name anybody
    else's, while /proc/<pid>/fd needs nothing to name our own.
    """
    try:
        out = subprocess.run(["ss", "-tine"], capture_output=True, text=True,
                             timeout=5)
    except (OSError, subprocess.SubprocessError):
        return {}, "ss would not run"
    found = {}
    inode, peer, header = None, "", True
    for line in out.stdout.splitlines():
        if header:
            header = False
            continue
        # A socket is two lines: the addresses and inode, then the counters
        # on an indented continuation. Neither is usable without the other.
        if not line.startswith((" ", "\t")):
            cols = line.split()
            peer = host_of(cols[4]) if len(cols) > 4 else ""
            seen = INO.search(line)
            inode = seen.group(1) if seen else None
            # ino:0 is a socket with no inode to own it - a TIME-WAIT
            # remnant, say. It cannot be attributed, and worse, every one of
            # them shares the key, so they would be merged into a single
            # entry whose counters jump about and manufacture deltas.
            if inode == "0":
                inode = None
            continue
        if inode is None:
            continue
        if local_peer(peer) or (external and not off_box(peer)):
            inode = None
            continue
        sent, recv = SENT.search(line), RECV.search(line)
        found[inode] = (int(sent.group(1)) if sent else 0,
                        int(recv.group(1)) if recv else 0)
        inode = None
    return found, ""


# Directory names that identify nothing: a binary living under one of these
# is named by whatever encloses it.
GENERIC = {"versions", "bin", "sbin", "libexec", "node_modules", "dist",
           "build", "lib", "share", "local", ".local", "current", "releases"}
HAS_LETTER = re.compile(r"[A-Za-z]")


def process_name(pid):
    """What to call a process, preferring something a person would recognise.

    /proc/<pid>/comm is the kernel's answer and usually right, but it is the
    executable's own name, and some are versioned - a binary at
    .../claude/versions/2.1.233 reports itself as "2.1.233", which is true
    and useless. When the name carries no letters at all, the enclosing path
    is walked back for one that does and means something.
    """
    try:
        with open("/proc/%d/comm" % pid) as f:
            name = f.read().strip()
    except OSError:
        name = ""
    if name and HAS_LETTER.search(name):
        return name
    try:
        with open("/proc/%d/cmdline" % pid, "rb") as f:
            argv0 = f.read().split(b"\x00")[0].decode("utf8", "replace")
    except OSError:
        argv0 = ""
    for part in reversed(argv0.split("/")):
        if part and HAS_LETTER.search(part) and part.lower() not in GENERIC:
            return part
    return name or "?"


def socket_owners():
    """inode -> (pid, name), for every process this user can read.

    Another user's /proc/<pid>/fd is unreadable, so their sockets arrive
    unowned. Their bytes are still counted, under one row that says so:
    dropping them would make the total quietly wrong.
    """
    owners = {}
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            fds = os.listdir("/proc/%s/fd" % pid)
        except OSError:
            continue
        name = ""
        for fd in fds:
            try:
                target = os.readlink("/proc/%s/fd/%s" % (pid, fd))
            except OSError:
                continue
            if not target.startswith("socket:["):
                continue
            if not name:
                name = process_name(int(pid))
            owners[target[8:-1]] = (int(pid), name or "?")
    return owners


class Store(object):
    """Per-process byte totals, accumulated from per-socket counters.

    The kernel counts per socket, not per process, and a socket's counters
    vanish with it. So each sample takes the difference against what that
    socket last read and adds it to whatever process owns it - which keeps
    the total intact when the socket closes, and when the process does.
    """

    def __init__(self):
        self.lock = threading.Lock()
        self.wake = threading.Event()
        self.totals = collections.OrderedDict()
        self.last = {}
        self.started = time.time()
        self.stamp = 0.0
        self.err = ""
        self.rezero = False

    def snapshot(self):
        with self.lock:
            return ([dict(v, key=k) for k, v in self.totals.items()],
                    self.started, self.err)

    def reset(self):
        """Make the current counters the new zero.

        Nothing from before the reset may reappear, so the last-seen map is
        kept and only the accumulated totals go: the next sample then
        differences against counters read before the reset and adds nothing
        for traffic that predates it.
        """
        with self.lock:
            self.totals.clear()
            self.started = time.time()

    def run(self):
        # A daemon thread that raises just stops, and a dead sampler looks
        # exactly like a machine using no network at all.
        try:
            self.poll()
        except Exception as exc:
            with self.lock:
                self.err = "sampler stopped: %s: %s" % (type(exc).__name__,
                                                        str(exc)[:70])

    def poll(self):
        while True:
            now = time.time()
            found, err = sockets(EXTERNAL)
            owners = socket_owners() if found else {}
            gap = max(1e-6, now - self.stamp) if self.stamp else 0.0
            with self.lock:
                self.err = err
                for row in self.totals.values():
                    row["up_rate"] = row["down_rate"] = 0.0
                    row["alive"] = False
                first = not self.stamp
                for inode, (sent, recv) in found.items():
                    was = self.last.get(inode)
                    # A socket opened since the last sample started at zero
                    # when it was created, so all of its counters are traffic
                    # that happened while we were watching. Only the sockets
                    # already open at the very first sample are zeroed - the
                    # difference is a connection that opens and closes inside
                    # one interval, whose bytes would otherwise never be
                    # counted at all.
                    if first:
                        d_sent = d_recv = 0
                    elif was is None:
                        d_sent, d_recv = sent, recv
                    # A reused inode reads lower than it did. Taking the
                    # difference would underflow, so the new socket's own
                    # figures are used instead.
                    elif sent < was[0] or recv < was[1]:
                        d_sent, d_recv = sent, recv
                    else:
                        d_sent, d_recv = sent - was[0], recv - was[1]
                    pid, name = owners.get(inode, (0, UNATTRIBUTED))
                    key = (pid, name)
                    row = self.totals.get(key)
                    if row is None:
                        row = {"pid": pid, "name": name, "up": 0, "down": 0,
                               "up_rate": 0.0, "down_rate": 0.0,
                               "alive": True, "seen": now}
                        self.totals[key] = row
                    row["alive"] = True
                    row["seen"] = now
                    row["up"] += d_sent
                    row["down"] += d_recv
                    if gap:
                        row["up_rate"] += d_sent / gap
                        row["down_rate"] += d_recv / gap
                self.last = dict(found)
                self.stamp = now
            self.wake.wait(INTERVAL)
            self.wake.clear()


def ordered(rows, mode):
    if mode == "live":
        return sorted(rows, key=lambda r: (-(r["up_rate"] + r["down_rate"]),
                                           -(r["up"] + r["down"])))
    return sorted(rows, key=lambda r: (-(r["up"] + r["down"]),
                                       -(r["up_rate"] + r["down_rate"])))


def table(rows, w, limit):
    """The process table, dropping columns rather than clipping them.

    Total is the one figure that cannot go: it is the whole question. Then
    the combined rate, then the split into down and up, which is a detail
    beside knowing something is moving at all. The name keeps a space of its
    own so it never runs into the pid.
    """
    avail = (w - 1) - 2 - 8 - 11
    wide = avail >= 10 + 11 + 22
    mid = avail >= 10 + 11
    name_w = max(8, min(26, avail - (33 if wide else 11 if mid else 0)))

    head = [(DIM, "  " + pad("PROCESS", name_w)), (DIM, "%-8s" % "PID"),
            (DIM, "%11s" % "TOTAL")]
    if mid:
        head.append((DIM, "%11s" % "NOW"))
    if wide:
        head.append((DIM, "%11s" % "DOWN"))
        head.append((DIM, "%11s" % "UP"))
    out = [seg(head, w - 1)]

    for row in rows[:limit]:
        live = row["up_rate"] + row["down_rate"]
        total = row["up"] + row["down"]
        gone = not row["alive"]
        line = [(DIM if gone else TXT,
                 "  " + pad(row["name"][:name_w - 1], name_w)),
                (DIM, "%-8s" % (row["pid"] or "-")),
                (TXT if total else DIM, "%11s" % units(total))]
        if mid:
            line.append((OK if live else DIM, "%11s" % rate(live)))
        if wide:
            line.append((DOWN if row["down_rate"] else DIM,
                         "%11s" % rate(row["down_rate"])))
            line.append((UP if row["up_rate"] else DIM,
                         "%11s" % rate(row["up_rate"])))
        out.append(seg(line, w - 1))
    return out


def plain_line(rows, started, mode, limit):
    """One block per interval, for a log or a pipe."""
    lines = ["--- %s elapsed · sorted by %s ---"
             % (elapsed(time.time() - started), mode)]
    for row in rows[:limit]:
        lines.append("%-22s %-8s %11s %11s %11s %11s"
                     % (row["name"], row["pid"] or "-",
                        units(row["up"] + row["down"]),
                        rate(row["up_rate"] + row["down_rate"]),
                        rate(row["down_rate"]), rate(row["up_rate"])))
    return "\n".join(lines)


def parse_args(argv):
    global INTERVAL, LIMIT, SORT, EXTERNAL, PLAIN
    rest = list(argv)
    while rest:
        arg = rest.pop(0)
        if arg in ("-i", "--interval") and rest:
            INTERVAL = max(0.2, float(rest.pop(0)))
        elif arg in ("-n", "--limit") and rest:
            LIMIT = max(0, int(rest.pop(0)))
        elif arg == "--sort" and rest:
            want = rest.pop(0)
            if want not in ("total", "live"):
                sys.stderr.write("--sort takes total or live\n")
                raise SystemExit(2)
            SORT = want
        elif arg == "--external":
            EXTERNAL = True
        elif arg == "--plain":
            PLAIN = True
        else:
            sys.stderr.write("unknown option %r - try --help\n" % arg)
            raise SystemExit(2)


def main():
    maybe_help(__doc__)
    parse_args(sys.argv[1:])
    if not any(os.access(os.path.join(p, "ss"), os.X_OK)
               for p in os.environ.get("PATH", "").split(os.pathsep)):
        sys.stderr.write("netwatch.py needs `ss` (iproute2); it is not on "
                         "PATH.\n")
        raise SystemExit(1)

    store = Store()
    threading.Thread(target=store.run, daemon=True).start()
    mode = SORT

    if PLAIN:
        while True:
            time.sleep(INTERVAL)
            rows, started, err = store.snapshot()
            if err:
                sys.stderr.write(err + "\n")
            print(plain_line(ordered(rows, mode), started, mode,
                             LIMIT or len(rows)))
            sys.stdout.flush()

    setup()
    keyboard = Keyboard()
    while True:
        for key in keyboard.poll():
            if key in ("q", "Q"):
                raise SystemExit(0)
            if key == "1":
                mode = "total"
            elif key == "2":
                mode = "live"
            elif key in ("r", "R"):
                store.reset()

        w, h = size()
        rows, started, err = store.snapshot()
        rows = ordered(rows, mode)
        moving = sum(1 for r in rows if r["up_rate"] + r["down_rate"])
        down = sum(r["down_rate"] for r in rows)
        up = sum(r["up_rate"] for r in rows)

        out = [title("netwatch", w, ACCENT)]
        out.append(seg([(DIM, " %d process%s" % (len(rows),
                                                 "" if len(rows) == 1
                                                 else "es")),
                        (DIM, " · %d moving" % moving),
                        (DIM, " · "), (ACCENT, elapsed(time.time() - started)),
                        (DIM, " · sorted by "), (ACCENT, mode),
                        (DIM, "   every %gs" % INTERVAL)], w - 1))
        out.append(seg([(DIM, " TCP only · "),
                        (DOWN, "↓ " + rate(down)), (DIM, "  "),
                        (UP, "↑ " + rate(up)),
                        (DIM, "  · off-box only" if EXTERNAL else "")], w - 1))
        if err:
            out.append(seg([(BAD, " ! " + err)], w - 1))
        out.append("")

        room = max(1, h - len(out) - 3)
        limit = min(LIMIT or room, room)
        if not rows:
            out.append(seg([(DIM, "  Nothing has moved yet. Totals start at "
                                  "zero, so this fills as traffic "
                                  "happens.")], w - 1))
        else:
            out.extend(table(rows, w, limit))

        while len(out) < h - 2:
            out.append("")
        hints = [[(ACCENT if mode == "total" else DIM, "[1] total")],
                 [(ACCENT if mode == "live" else DIM, "[2] live")],
                 [(DIM, "[r]ezero")], [(DIM, "[q]uit")]]
        for line in pack_hints(hints, w - 2):
            out.append(" " + line)
        draw(out, w, h)
        time.sleep(min(0.3, INTERVAL))


main()
