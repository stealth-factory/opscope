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

Enter opens one process: its command, every connection it holds separately,
and the files it currently has open with how fast each is growing - which is
the closest thing to "which file is it downloading" that exists outside the
encrypted stream. The URL and the remote filename are inside TLS and are not
readable from here by any means.

Keys: up/down select, enter opens one, esc goes back, 1 sorts by total,
2 by current rate, r rezeroes, q quits.
"""
import collections
import json
import os
import re
import socket
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (Keyboard, bg, clipboard, draw, load_config, maybe_help,
                    pack_hints, pad, rgb, seg, setup, size, title)

_CFG = load_config("netwatch", {
    "interval": 1.0,
    "limit": 0,          # 0 fills the pane
    "sort": "total",
    "external": True,
})

INTERVAL = max(0.2, float(_CFG["interval"]))
# Samples kept for the chart. Wider than any pane, so the graph is a window
# on to real history rather than exactly as much as happened to fit.
SERIES = 240
LIMIT = int(_CFG["limit"])
SORT = _CFG["sort"] if _CFG["sort"] in ("total", "live") else "total"
# Strict by default: a connection counts only when the other end is a
# globally routable address. The LAN, the tailnet and the machine's own
# addresses are all somewhere other than the internet, and "what is this box
# sending out" is almost always the question being asked.
EXTERNAL = bool(_CFG["external"])
VERSION = "1.0"
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


def port_of(addr):
    _, _, port = addr.rpartition(":")
    try:
        return int(port)
    except ValueError:
        return 0


def service(port):
    """What a port number is conventionally for, from /etc/services."""
    if not port:
        return ""
    try:
        return socket.getservbyport(port)
    except (OSError, TypeError):
        return ""


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
    inode, peer, port, header = None, "", 0, True
    for line in out.stdout.splitlines():
        if header:
            header = False
            continue
        # A socket is two lines: the addresses and inode, then the counters
        # on an indented continuation. Neither is usable without the other.
        if not line.startswith((" ", "\t")):
            cols = line.split()
            peer = host_of(cols[4]) if len(cols) > 4 else ""
            port = port_of(cols[4]) if len(cols) > 4 else 0
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
        found[inode] = {"sent": int(sent.group(1)) if sent else 0,
                        "recv": int(recv.group(1)) if recv else 0,
                        "peer": peer, "port": port}
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


_NAMES = {}
_WANTED = collections.deque()
_ASKED = set()


def resolver():
    """Reverse DNS, off the drawing thread.

    A PTR lookup takes half a second when it works and longer when it does
    not, which is several frames. The address is shown until a name arrives,
    and an address that has no name is remembered as having none so it is
    not asked about again every second.
    """
    while True:
        try:
            ip = _WANTED.popleft()
        except IndexError:
            time.sleep(0.3)
            continue
        try:
            _NAMES[ip] = socket.gethostbyaddr(ip)[0]
        except (OSError, socket.herror, socket.gaierror):
            _NAMES[ip] = ""


def hostname(ip):
    """A name for an address if one is known, queueing a lookup if not."""
    if ip in _NAMES:
        return _NAMES[ip]
    if ip not in _ASKED:
        _ASKED.add(ip)
        _WANTED.append(ip)
    return ""


def running(pid):
    """Whether the process still exists.

    Distinct from the `alive` flag on a row, which means "had a socket in
    the last sample". A long-running server sitting idle has neither
    traffic nor open connections and has certainly not exited, and saying
    it had would be worse than saying nothing.
    """
    return bool(pid) and os.path.isdir("/proc/%d" % pid)


def proc_io(pid):
    """Disk bytes this process has read and written, from /proc/<pid>/io."""
    out = {}
    try:
        with open("/proc/%d/io" % pid) as f:
            for line in f:
                key, _, value = line.partition(":")
                try:
                    out[key.strip()] = int(value)
                except ValueError:
                    continue
    except OSError:
        return {}
    return out


def open_files(pid):
    """Regular files this process has open, largest first.

    A download has to land somewhere, and where it lands is a file getting
    bigger. This is the closest thing to "which file" that exists outside
    the encrypted stream - the name of the thing being written, rather than
    the name of the thing being fetched.
    """
    found = []
    try:
        fds = os.listdir("/proc/%d/fd" % pid)
    except OSError:
        return found
    for fd in fds:
        try:
            path = os.readlink("/proc/%d/fd/%s" % (pid, fd))
        except OSError:
            continue
        if not path.startswith("/") or path.startswith(("/dev/", "/proc/",
                                                        "/sys/")):
            continue
        try:
            size = os.stat("/proc/%d/fd/%s" % (pid, fd)).st_size
        except OSError:
            continue
        writing = False
        try:
            with open("/proc/%d/fdinfo/%s" % (pid, fd)) as f:
                for line in f:
                    if line.startswith("flags:"):
                        writing = int(line.split()[1], 8) & 3 != 0
        except (OSError, ValueError, IndexError):
            pass
        found.append({"path": path, "size": size, "writing": writing})
    found.sort(key=lambda f: -f["size"])
    return found


def process_facts(pid):
    """Command, directory and age - what the table has no room for."""
    facts = {"cmdline": "", "cwd": "", "started": None}
    try:
        with open("/proc/%d/cmdline" % pid, "rb") as f:
            facts["cmdline"] = f.read().replace(b"\x00", b" ").decode(
                "utf8", "replace").strip()
    except OSError:
        pass
    try:
        facts["cwd"] = os.readlink("/proc/%d/cwd" % pid)
    except OSError:
        pass
    try:
        facts["started"] = os.stat("/proc/%d" % pid).st_ctime
    except OSError:
        pass
    return facts


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
        self.conns = collections.OrderedDict()
        self.series = collections.deque(maxlen=SERIES)
        self.spots = collections.OrderedDict()
        self.last = {}
        self.started = time.time()
        self.stamp = 0.0
        self.err = ""
        self.rezero = False

    def snapshot(self):
        with self.lock:
            return ([dict(v, key=k) for k, v in self.totals.items()],
                    self.started, self.err)

    def rates(self):
        """The machine's total down and up rate, one pair per sample."""
        with self.lock:
            return list(self.series)

    def endpoints(self, pid, name):
        """One process's remote hosts, aggregated, busiest first."""
        with self.lock:
            found = [dict(s, ports=sorted(s["ports"]),
                          hist=list(s["hist"]))
                     for s in self.spots.values()
                     if s["pid"] == pid and s["name"] == name]
        return sorted(found, key=lambda s: (-(s["up"] + s["down"]),
                                            s["peer"]))

    def history(self, pid, name):
        """One process's rate history, for its own chart."""
        with self.lock:
            row = self.totals.get((pid, name))
            return list(row["hist"]) if row else []

    def connections(self, pid, name):
        """One process's individual connections, busiest first."""
        with self.lock:
            found = [dict(c) for c in self.conns.values()
                     if c["pid"] == pid and c["name"] == name]
        return sorted(found, key=lambda c: (-(c["up"] + c["down"]),
                                            c["peer"]))

    def reset(self):
        """Make the current counters the new zero.

        Nothing from before the reset may reappear, so the last-seen map is
        kept and only the accumulated totals go: the next sample then
        differences against counters read before the reset and adds nothing
        for traffic that predates it.
        """
        with self.lock:
            self.totals.clear()
            self.conns.clear()
            self.spots.clear()
            self.series.clear()
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
                for conn in self.conns.values():
                    conn["up_rate"] = conn["down_rate"] = 0.0
                    conn["alive"] = False
                for spot in self.spots.values():
                    spot["up_rate"] = spot["down_rate"] = 0.0
                    spot["alive"] = False
                first = not self.stamp
                for inode, seen in found.items():
                    sent, recv = seen["sent"], seen["recv"]
                    was = self.last.get(inode)
                    was = was if was is None else (was["sent"], was["recv"])
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
                               "alive": True, "seen": now,
                               "hist": collections.deque(maxlen=SERIES)}
                        self.totals[key] = row
                    row["alive"] = True
                    row["seen"] = now
                    row["up"] += d_sent
                    row["down"] += d_recv
                    if gap:
                        row["up_rate"] += d_sent / gap
                        row["down_rate"] += d_recv / gap

                    # The same arithmetic per connection, so the detail
                    # screen can say which of a process's dozen sockets is
                    # the one actually moving.
                    conn = self.conns.get(inode)
                    if conn is None:
                        conn = {"pid": pid, "name": name, "peer": seen["peer"],
                                "port": seen["port"], "up": 0, "down": 0,
                                "up_rate": 0.0, "down_rate": 0.0,
                                "alive": True, "seen": now,
                                "opened": 0 if first else now}
                        self.conns[inode] = conn
                    conn["alive"] = True
                    conn["seen"] = now
                    conn["up"] += d_sent
                    conn["down"] += d_recv
                    if gap:
                        conn["up_rate"] += d_sent / gap
                        conn["down_rate"] += d_recv / gap

                    # Endpoints aggregate the sockets sharing a peer: a
                    # browser opening six connections to one host is one
                    # thing being talked to, not six.
                    spot = self.spots.setdefault(
                        (pid, name, seen["peer"]),
                        {"pid": pid, "name": name, "peer": seen["peer"],
                         "up": 0, "down": 0, "up_rate": 0.0,
                         "down_rate": 0.0, "alive": True, "seen": now,
                         "ports": set(),
                         "hist": collections.deque(maxlen=SERIES)})
                    spot["alive"] = True
                    spot["seen"] = now
                    spot["ports"].add(seen["port"])
                    spot["up"] += d_sent
                    spot["down"] += d_recv
                    if gap:
                        spot["up_rate"] += d_sent / gap
                        spot["down_rate"] += d_recv / gap
                # The whole machine's rate this sample, for the chart.
                # Summed from the same per-process figures the table shows,
                # so the two can never disagree.
                if gap:
                    self.series.append(
                        (sum(r["down_rate"] for r in self.totals.values()),
                         sum(r["up_rate"] for r in self.totals.values())))
                    for row in self.totals.values():
                        row["hist"].append((row["down_rate"], row["up_rate"]))
                    for spot in self.spots.values():
                        spot["hist"].append((spot["down_rate"],
                                             spot["up_rate"]))
                self.last = dict(found)
                self.stamp = now
                # A closed connection is worth keeping - it may be the one
                # that did the damage - but not forever. The quiet dead ones
                # go once there are enough of them to matter.
                if len(self.conns) > 400:
                    for inode, conn in sorted(
                            self.conns.items(),
                            key=lambda kv: kv[1]["seen"])[:100]:
                        if not conn["alive"]:
                            self.conns.pop(inode, None)
            self.wake.wait(INTERVAL)
            self.wake.clear()


def ordered(rows, mode):
    if mode == "live":
        return sorted(rows, key=lambda r: (-(r["up_rate"] + r["down_rate"]),
                                           -(r["up"] + r["down"])))
    return sorted(rows, key=lambda r: (-(r["up"] + r["down"]),
                                       -(r["up_rate"] + r["down_rate"])))


def table(rows, w, limit, selected=-1):
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

    for i, row in enumerate(rows[:limit]):
        live = row["up_rate"] + row["down_rate"]
        total = row["up"] + row["down"]
        gone = not row["alive"]
        here = i == selected
        tint = bg(28, 44, 62) if here else ""
        line = [(tint + (ACCENT if here else DIM if gone else TXT),
                 ("▸" if here else " ") + " "
                 + pad(row["name"][:name_w - 2], name_w - 1)),
                (tint + DIM, "%-8s" % (row["pid"] or "-")),
                (tint + (TXT if total else DIM), "%11s" % units(total))]
        if mid:
            line.append((tint + (OK if live else DIM), "%11s" % rate(live)))
        if wide:
            line.append((tint + (DOWN if row["down_rate"] else DIM),
                         "%11s" % rate(row["down_rate"])))
            line.append((tint + (UP if row["up_rate"] else DIM),
                         "%11s" % rate(row["up_rate"])))
        if here:
            line.append((tint, " " * w))
        out.append(seg(line, w - 1))
    return out


# A braille cell is two dots wide and four tall, so one character holds
# eight addressable points. The bit for each is fixed by the encoding: the
# glyph is U+2800 plus the mask of the dots that are lit.
BRAILLE = ((0x01, 0x08), (0x02, 0x10), (0x04, 0x20), (0x40, 0x80))


def braille_canvas(values, peak, cols, rows, inverted=False):
    """Plot a series on a dot canvas eight times finer than the cells.

    Two dots per column and four per row, which is the difference between a
    line that steps between character rows and one that reads as a curve.
    Consecutive samples are joined with Bresenham rather than left as
    points, so a steep climb is a line and not a column of freckles.
    """
    px_w, px_h = cols * 2, rows * 4
    grid = [[0] * cols for _ in range(rows)]
    vals = list(values)[-px_w:]
    if not vals:
        return grid

    def point(i):
        x = (px_w - 1 if len(vals) == 1
             else int(round(i * (px_w - 1) / float(len(vals) - 1))))
        scaled = min(1.0, max(0.0, vals[i] / peak)) if peak else 0.0
        magnitude = int(round(scaled * (px_h - 1)))
        return x, (magnitude if inverted else px_h - 1 - magnitude)

    def dot(x, y):
        if 0 <= x < px_w and 0 <= y < px_h:
            grid[y // 4][x // 2] |= BRAILLE[y % 4][x % 2]

    if len(vals) == 1:
        if vals[0] > 0:
            dot(*point(0))
        return grid
    for i in range(1, len(vals)):
        # An idle stretch draws nothing at all rather than a flat line
        # pinned to the axis, which would read as activity at zero.
        if vals[i - 1] == 0 and vals[i] == 0:
            continue
        x0, y0 = point(i - 1)
        x1, y1 = point(i)
        dx, dy = abs(x1 - x0), -abs(y1 - y0)
        sx = 1 if x0 < x1 else -1
        sy = 1 if y0 < y1 else -1
        err = dx + dy
        while True:
            dot(x0, y0)
            if x0 == x1 and y0 == y1:
                break
            twice = 2 * err
            if twice >= dy:
                err += dy
                x0 += sx
            if twice <= dx:
                err += dx
                y0 += sy
    return grid


def braille_row(masks, colour):
    return [(colour, chr(0x2800 + m) if m else " ") for m in masks]


def chart(series, w, h, label="", tint=None):
    """Received above the line, sent below it, newest on the right.

    The two halves are scaled independently and each says what its own peak
    is. A shared scale is the obvious choice and the wrong one here: a
    download running at ten megabits with acknowledgements going back at
    fifty kilobits would draw the upload as a flat nothing, and whether the
    upload is flat is often the question. The lower label carries a minus
    sign, since down the page is the negative half of the axis.
    """
    lab = 10
    plot = max(12, w - lab - 4)
    canvas = max(2, h - 3)
    up_h = max(1, canvas // 2)
    down_h = max(1, canvas - up_h)
    window = list(series)[-plot * 2:]
    rx = [v[0] for v in window]
    tx = [v[1] for v in window]
    rx_peak = max(rx or [0]) or 1.0
    tx_peak = max(tx or [0]) or 1.0
    rx_hue, tx_hue = tint or (DOWN, UP)

    out = [seg([(DIM, "%*s " % (lab, rate(rx_peak))),
                (GRID, "┌" + "─" * plot + "┐")], w - 1)]
    for masks in braille_canvas(rx, rx_peak, plot, up_h):
        out.append(seg([(DIM, " " * (lab + 1)), (GRID, "│")]
                       + braille_row(masks, rx_hue) + [(GRID, "│")], w - 1))
    out.append(seg([(DIM, "%*s " % (lab, "0")),
                    (GRID, "├" + "─" * plot + "┤")], w - 1))
    for masks in braille_canvas(tx, tx_peak, plot, down_h, inverted=True):
        out.append(seg([(DIM, " " * (lab + 1)), (GRID, "│")]
                       + braille_row(masks, tx_hue) + [(GRID, "│")], w - 1))
    out.append(seg([(DIM, "%*s " % (lab, "−" + rate(tx_peak))),
                    (GRID, "└" + "─" * plot + "┘")], w - 1))
    return out


def chart_head(series, w, label):
    """The line above a chart: what it is, and how far back it reaches."""
    window = list(series)
    span = elapsed(len(window) * INTERVAL) if window else "nothing yet"
    return seg([(LBL, " ── %s ── " % label),
                (DOWN, "↓ rx above"), (DIM, " · "), (UP, "↑ tx below"),
                (DIM, "  · %s of history" % span)], w - 1)


def short(path, room):
    """A path that fits, keeping the end - which is the filename."""
    home = os.path.expanduser("~")
    if path.startswith(home):
        path = "~" + path[len(home):]
    if len(path) <= room:
        return path
    return "…" + path[-(room - 1):]


def wrap(text, width):
    lines, rest = [], text
    while rest and len(lines) < 3:
        if len(rest) <= width:
            lines.append(rest)
            break
        cut = rest.rfind(" ", 0, width + 1)
        cut = cut if cut > width // 2 else width
        lines.append(rest[:cut])
        rest = rest[cut:].lstrip()
    return lines or [""]


def field(label, value, w, colour=TXT):
    out = []
    for i, line in enumerate(wrap(value, max(8, (w - 3) - 10))):
        out.append(seg([(DIM, "  " + pad(label if not i else "", 10)),
                        (colour, line)], w - 1))
    return out


SECTIONS = ("endpoints", "connections", "files")
# What `c` would put on the clipboard, set while drawing and read when the
# key is pressed - the selection is known in one place and used in another.
pending_copy = [""]


def section_head(name, count, note, focused, key, w):
    """A section header that says whether it is the one taking the keys."""
    return seg([(ACCENT if focused else LBL,
                 (" ▏" if focused else " ") + "── %s ── " % name),
                (DIM, "%d %s" % (count, note)),
                (ACCENT if focused else GRID, "   [%s]" % key)], w - 1)


def endpoint_rows(spots, at, focused, room, w):
    """Remote hosts, ranked by what they have carried since launch."""
    out = []
    host_w = max(14, min(34, (w - 1) - 42))
    for i, spot in enumerate(spots[:max(1, room)]):
        here = focused and i == at
        name = hostname(spot["peer"]) or spot["peer"]
        ports = "/".join(service(p) or str(p) for p in spot["ports"][:2])
        tint = bg(28, 44, 62) if here else ""
        out.append(seg([
            (tint + (ACCENT if here else DIM), " ▸ " if here else "   "),
            (tint + (TXT if spot["alive"] else DIM), pad(name[:host_w - 1],
                                                        host_w)),
            (tint + DIM, "%-9s" % ports[:9]),
            (tint + DOWN, "↓%9s" % units(spot["down"])),
            (tint + UP, " ↑%9s" % units(spot["up"])),
            (tint + (OK if spot["down_rate"] + spot["up_rate"] else DIM),
             "%11s" % rate(spot["down_rate"] + spot["up_rate"])),
            (tint, " " * w if here else ""),
        ], w - 1))
    return out


def connection_rows(conns, at, focused, room, w):
    """The sockets open right now, which is a different list from the hosts."""
    out = []
    host_w = max(14, min(38, (w - 1) - 34))
    for i, conn in enumerate(conns[:max(1, room)]):
        here = focused and i == at
        tint = bg(28, 44, 62) if here else ""
        where = "%s:%d" % (conn["peer"], conn["port"])
        out.append(seg([
            (tint + (ACCENT if here else DIM), " ▸ " if here else "   "),
            (tint + (TXT if conn["alive"] else DIM), pad(where[:host_w - 1],
                                                         host_w)),
            (tint + (OK if conn["alive"] else DIM),
             "%-7s" % ("open" if conn["alive"] else "closed")),
            (tint + DOWN, "↓%9s" % units(conn["down"])),
            (tint + UP, " ↑%9s" % units(conn["up"])),
            (tint, " " * w if here else ""),
        ], w - 1))
    return out


def file_rows(files, sizes, at, focused, room, w):
    """Open files, with how fast each is growing since this screen opened."""
    out = []
    path_w = max(18, (w - 1) - 30)
    for i, item in enumerate(files[:max(1, room)]):
        here = focused and i == at
        tint = bg(28, 44, 62) if here else ""
        was = sizes.get(item["path"])
        grew = item["size"] - was[0] if was else 0
        span = time.time() - was[1] if was else 0
        out.append(seg([
            (tint + (ACCENT if here else DIM), " ▸ " if here else "   "),
            (tint + TXT, pad(short(item["path"], path_w), path_w)),
            (tint + DIM, "%10s" % units(item["size"])),
            (tint + (OK if grew > 0 else DIM),
             "%12s" % (("+" + rate(grew / span)) if grew > 0 and span else "")),
            (tint, " " * w if here else ""),
        ], w - 1))
    return out


def detail_rows(row, hist, spots, conns, files, sizes, focus, at, w, h):
    """One process in full: what it is, who it talks to, what it writes."""
    facts = process_facts(row["pid"])
    here_now = running(row["pid"])
    total = row["up"] + row["down"]
    out = [title("%s · pid %d" % (row["name"], row["pid"]), w, ACCENT)]
    out.append(seg([(TXT, " " + units(total)),
                    (DIM, " since first seen  ·  "),
                    (DOWN, "↓ " + rate(row["down_rate"])), (DIM, "  "),
                    (UP, "↑ " + rate(row["up_rate"]))], w - 1))
    if not row["pid"]:
        out.append(seg([(DIM, " sockets /proc would not name an owner for - "
                              "another user's, which means root's")], w - 1))
    elif not here_now:
        out.append(seg([(WARN, " this process has exited - its total is kept,"
                               " and nothing below is live")], w - 1))
    elif not row["alive"]:
        out.append(seg([(DIM, " no connection open at the moment - what is "
                              "below is the last that was seen")], w - 1))
    out.append("")

    # This process's own traffic, on the same chart as the machine's.
    spare = h - len(out)
    graph_h = 7 if spare >= 30 else 5 if spare >= 24 else 0
    if graph_h and hist:
        out.append(chart_head(hist, w, "THIS PROCESS"))
        out.extend(chart(hist, w, graph_h))
        out.append("")

    if h - len(out) >= 12:
        out.append(seg([(LBL, " ── PROCESS ── ")], w - 1))
        out += field("command", facts["cmdline"] or "?", w)
        if facts["cwd"]:
            out += field("directory", short(facts["cwd"], w - 14), w)
        out.append("")

    # What is left is split between the three lists, with the focused one
    # given the room: it is the one being read, and the others still say
    # how much they are holding in their headers.
    left = max(3, h - len(out) - 4)
    shares = {name: 1 for name in SECTIONS}
    shares[focus] = max(1, left - 2 - 3 * 2)
    counts = {"endpoints": len(spots), "connections": len(conns),
              "files": len(files)}

    for name, key, note in (("TALKING TO", "e", "endpoint"),
                            ("CONNECTIONS", "tab", "socket"),
                            ("FILES", "f", "file")):
        which = SECTIONS[("TALKING TO", "CONNECTIONS",
                          "FILES").index(name)]
        n = counts[which]
        focused = focus == which
        out.append(section_head(name, n, note + ("" if n == 1 else "s"),
                                focused, key, w))
        room = min(shares[which], max(1, h - len(out) - 3))
        if not n:
            out.append(seg([(DIM, "   none")], w - 1))
        elif which == "endpoints":
            out.extend(endpoint_rows(spots, at[which], focused, room, w))
            # The highlighted host gets its own small chart, which is the
            # quickest way to see whether it is the one doing the work.
            pick = spots[min(at[which], len(spots) - 1)] if spots else None
            if focused and pick and pick["hist"] and h - len(out) >= 7:
                out.append(seg([(DIM, "     ── "),
                                (ACCENT, hostname(pick["peer"])
                                 or pick["peer"]),
                                (DIM, " alone ──")], w - 1))
                out.extend(chart(pick["hist"], w, 4))
        elif which == "connections":
            out.extend(connection_rows(conns, at[which], focused, room, w))
        else:
            out.extend(file_rows(files, sizes, at[which], focused, room, w))
        out.append("")

    io = proc_io(row["pid"]) if here_now else {}
    if io and h - len(out) >= 2:
        out.append(seg([(LBL, " ── DISK ── "),
                        (DIM, "read %s · written %s since it started"
                         % (units(io.get("read_bytes", 0)),
                            units(io.get("write_bytes", 0))))], w - 1))
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
        elif arg == "--all-external":
            EXTERNAL = False
        elif arg in ("-V", "--version"):
            print("netwatch.py %s" % VERSION)
            raise SystemExit(0)
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
    threading.Thread(target=resolver, daemon=True).start()
    selected, detail, sizes = 0, None, {}
    focus, at = "endpoints", {k: 0 for k in SECTIONS}
    notice = None
    while True:
        w, h = size()
        rows, started, err = store.snapshot()
        rows = ordered(rows, mode)

        for key in keyboard.poll():
            if detail is not None:
                if key in ("esc", "left", "q", "Q", "backspace"):
                    detail, sizes = None, {}
                elif key in ("r", "R"):
                    store.reset()
                    detail, sizes = None, {}
                elif key in ("up", "k", "K"):
                    at[focus] -= 1
                elif key in ("down", "j", "J"):
                    at[focus] += 1
                elif key == "tab":
                    focus = SECTIONS[(SECTIONS.index(focus) + 1)
                                     % len(SECTIONS)]
                elif key in ("e", "E"):
                    focus = "endpoints"
                elif key in ("f", "F"):
                    focus = "files"
                elif key in ("c", "C"):
                    text = pending_copy[0]
                    if text:
                        ok = clipboard(text)
                        # The value goes in the message either way: OSC 52
                        # is refused by some terminals and swallowed by some
                        # multiplexers, and a copy that quietly did nothing
                        # would leave nothing on screen to read instead.
                        notice = (("copied  " if ok else "no clipboard  ")
                                  + text, OK if ok else WARN, time.time() + 8)
                continue
            if key in ("q", "Q"):
                raise SystemExit(0)
            if key == "1":
                mode = "total"
            elif key == "2":
                mode = "live"
            elif key in ("up", "k", "K"):
                selected -= 1
            elif key in ("down", "j", "J"):
                selected += 1
            elif key in ("s", "S", "t", "T"):
                mode = "live" if mode == "total" else "total"
            elif key in ("enter", "right", "i") and rows:
                pick = rows[max(0, min(selected, len(rows) - 1))]
                detail, sizes = (pick["pid"], pick["name"]), {}
                focus, at = "endpoints", {k: 0 for k in SECTIONS}
            elif key in ("r", "R"):
                store.reset()
                selected = 0

        selected = max(0, min(selected, len(rows) - 1)) if rows else 0

        # One process in full. Its row is looked up fresh each frame so the
        # figures keep moving while the screen is open, and it survives the
        # process exiting - which is often when it is being looked at.
        if detail is not None:
            pid, name = detail
            row = next((r for r in rows
                        if r["pid"] == pid and r["name"] == name), None)
            if row is None:
                detail, sizes = None, {}
            else:
                spots = store.endpoints(pid, name)
                conns = store.connections(pid, name)
                files = open_files(pid) if running(pid) else []
                now = time.time()
                for item in files:
                    if item["path"] not in sizes:
                        sizes[item["path"]] = (item["size"], now)
                sizes_of = {"endpoints": len(spots),
                            "connections": len(conns), "files": len(files)}
                for which in SECTIONS:
                    at[which] = max(0, min(at[which],
                                           sizes_of[which] - 1)) \
                        if sizes_of[which] else 0
                # What c would copy, decided where the selection is rather
                # than at the keypress, so the footer can name it.
                if focus == "endpoints" and spots:
                    pick = spots[at["endpoints"]]
                    pending_copy[0] = hostname(pick["peer"]) or pick["peer"]
                elif focus == "connections" and conns:
                    pick = conns[at["connections"]]
                    pending_copy[0] = "%s:%d" % (pick["peer"], pick["port"])
                elif focus == "files" and files:
                    pending_copy[0] = files[at["files"]]["path"]
                else:
                    pending_copy[0] = ""

                foot = [" " + line for line in pack_hints(
                    [[(ACCENT, "↑↓"), (DIM, " in section")],
                     [(ACCENT, "tab"), (DIM, " section")],
                     [(DIM, "[e]ndpoints")], [(DIM, "[f]iles")],
                     [(DIM, "[c]opy")], [(DIM, "[esc] back")],
                     [(DIM, "[q]uit")]], w - 2)]
                if notice and time.time() >= notice[2]:
                    notice = None
                if notice:
                    foot = [seg([(notice[1], " " + notice[0])], w - 1)]
                room = max(1, h - len(foot) - 1)
                body = detail_rows(row, store.history(pid, name), spots,
                                   conns, files, sizes, focus, at, w, room)
                while len(body) < room:
                    body.append("")
                draw(body[:room] + foot, w, h)
                time.sleep(min(0.5, INTERVAL))
                continue

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
                        (DIM, "  · internet only" if EXTERNAL
                         else "  · everything off-box")], w - 1))
        if err:
            out.append(seg([(BAD, " ! " + err)], w - 1))
        out.append("")

        # The chart takes a share of the pane and the list takes the rest,
        # but the list is the point: below a certain height there is no
        # chart at all rather than two rows of neither.
        spare = h - len(out) - 4
        graph_h = 9 if spare >= 20 else 7 if spare >= 15 else 5 if spare >= 11 else 0
        if graph_h and store.rates():
            out.append(chart_head(store.rates(), w, "TRAFFIC"))
            out.extend(chart(store.rates(), w, graph_h))
            out.append("")

        room = max(1, h - len(out) - 3)
        limit = min(LIMIT or room, room)
        if selected >= limit:
            rows = rows[selected - limit + 1:]
        if not rows:
            out.append(seg([(DIM, "  Nothing has moved yet. Totals start at "
                                  "zero, so this fills as traffic "
                                  "happens.")], w - 1))
        else:
            out.extend(table(rows, w, limit, selected))

        while len(out) < h - 2:
            out.append("")
        hints = [[(ACCENT, "↑↓"), (DIM, " select")],
                 [(ACCENT, "↵"), (DIM, " details")],
                 [(ACCENT if mode == "total" else DIM, "[1] total")],
                 [(ACCENT if mode == "live" else DIM, "[2] live")],
                 [(DIM, "[r]ezero")], [(DIM, "[q]uit")]]
        for line in pack_hints(hints, w - 2):
            out.append(" " + line)
        draw(out, w, h)
        time.sleep(min(0.3, INTERVAL))


main()
