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
"""Tailscale network: who is online, and how you are reaching them.

RX and TX are traffic between *this* host and that peer, counted by the local
WireGuard engine — not the peer's own totals. They reset when tailscaled
restarts, so they cover that window rather than all time.

The column that matters is PATH. A peer is either DIRECT, meaning NAT traversal
succeeded and traffic goes peer-to-peer, or it is relayed through a named DERP
region, meaning every packet round-trips through Tailscale's infrastructure.
Relayed peers can be dramatically slower and the difference is invisible in
`tailscale status` output unless you look for it.

Peers advertising subnet routes are flagged, since those only reach you if this
node runs with --accept-routes.

The info view names each peer's home DERP region — the Tailscale POP nearest to
it — as a location hint. That comes from the local DERP map, so no address is
ever sent to a geolocation service.

    python3 tailnet.py [-n SECONDS]

A live throughput section graphs peers currently moving data (toggle with g),
and the info view carries the same graph for the selected machine plus ICMP
latency over the tunnel — current, average, median, min, max, jitter, loss and
a sparkline — measured the same way the latency monitor does. Only the selected
peer is probed, so this costs one ping process regardless of tailnet size.

n cycles the poll interval while running (1/2/5/10/30s), the same way the
latency monitor's i key does; the graph resolution follows it. -n sets the
starting value, and `tailnet.refresh` in config.json sets the default.

Keys: up/down select a peer, Enter or i opens a full machine info view (every address,
routes, tags, owner, handshake times), c or Enter opens a copy sheet offering its
Tailscale IP, MagicDNS name, public IP and LAN IP, r refreshes now, o hides
offline peers, q quits. Copying uses OSC 52, so it reaches the clipboard of
the machine you are typing at even over SSH.

Needs the `tailscale` CLI. Peer LAN addresses come from `tailscale debug
netmap`, which needs root; it is attempted with `sudo -n` and simply omitted
when that would prompt, so nothing here requires privilege.
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
from common import (RST, Keyboard, bg, clipboard, cycle, draw, load_config,
                    maybe_help, pack_hints, pad, rgb, seg, setup, size, title)

_CFG = load_config("tailnet", {"refresh": 2.0, "history": 180})
REFRESH = float(_CFG["refresh"])
HISTORY = int(_CFG["history"])   # rate samples kept per peer
REFRESH_CHOICES = (1.0, 2.0, 5.0, 10.0, 30.0)   # cycled at runtime with n
SPARK = "▁▂▃▄▅▆▇█"

ONLINE = rgb(90, 240, 160)
OFFLINE = rgb(120, 130, 150)
DIRECT = rgb(90, 240, 160)
RELAY = rgb(255, 190, 90)
DIM = rgb(127, 147, 172)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
ACCENT = rgb(120, 200, 255)
ROUTE = rgb(200, 160, 255)
EXIT = rgb(255, 140, 200)


def daemon_uptime():
    """Seconds since tailscaled started.

    Its byte counters live in memory and reset with it, so RX/TX cover this
    window rather than all time — a peer reading 0B may just predate a restart.
    """
    try:
        for pid in os.listdir("/proc"):
            if not pid.isdigit():
                continue
            try:
                with open("/proc/%s/comm" % pid) as f:
                    if f.read().strip() != "tailscaled":
                        continue
                with open("/proc/%s/stat" % pid) as f:
                    started = int(f.read().rpartition(")")[2].split()[19])
                with open("/proc/uptime") as f:
                    up = float(f.read().split()[0])
            except (OSError, IndexError, ValueError):
                continue
            return up - started / float(os.sysconf("SC_CLK_TCK"))
    except OSError:
        pass
    return None


def peer_name(peer):
    """Tailnet-unique display name.

    HostName is whatever the device calls itself and is frequently useless:
    iPads, Chromecasts and Pixels all report "localhost", and two Apple TVs
    report the same "apple-tv". The first label of the MagicDNS name is unique
    across the tailnet and matches what the admin console shows.
    """
    dns = (peer.get("DNSName") or "").rstrip(".")
    if dns:
        return dns.split(".")[0]
    return peer.get("HostName") or "?"


def classify(ip):
    """public | private | tailscale | other, from the address alone."""
    ip = ip.split("%")[0]
    if ":" in ip:
        return "tailscale" if ip.lower().startswith("fd7a:") else "other"
    try:
        a, b = (int(x) for x in ip.split(".")[:2])
    except ValueError:
        return "other"
    if a == 100 and 64 <= b <= 127:
        return "tailscale"          # CGNAT range Tailscale itself uses
    if a == 10 or (a == 172 and 16 <= b <= 31) or (a == 192 and b == 168):
        return "private"
    if a == 169 and b == 254:
        return "other"              # link-local
    if a == 127:
        return "other"
    return "public"


def in_network(ip, cidr):
    """Is an IPv4 address inside a CIDR block?"""
    net, _, bits = cidr.partition("/")
    if ":" in net or not bits:
        return False
    try:
        bits = int(bits)
        to_int = lambda a: sum(int(o) << (24 - 8 * i)
                               for i, o in enumerate(a.split(".")))
        mask = (0xffffffff << (32 - bits)) & 0xffffffff
        return (to_int(ip) & mask) == (to_int(net) & mask)
    except (ValueError, IndexError):
        return False


def lan_rank(ip, routes):
    """Lower is better. Prefers a real LAN address over a virtual bridge.

    A peer often exposes several private endpoints, and docker0 (172.17.0.1)
    or a k8s bridge is not the address anyone wants to copy. An address inside
    a subnet the peer advertises is almost certainly its real LAN address.
    """
    if any(in_network(ip, r) for r in routes):
        return 0
    first, second = (int(x) for x in ip.split(".")[:2])
    if first == 192 and second == 168:
        return 1
    if first == 10:
        return 2
    return 3                      # 172.16-31: usually docker/virtual


def endpoints_by_peer():
    """Peer LAN/public endpoints from the netmap, which needs root.

    Optional enrichment: `tailscale status` does not carry peer endpoints, so
    without this the panel simply offers fewer addresses to copy. Uses sudo -n
    so it fails instantly rather than prompting when sudo needs a password.
    """
    try:
        out = subprocess.run(["sudo", "-n", "tailscale", "debug", "netmap"],
                             capture_output=True, text=True, timeout=25)
        data = json.loads(out.stdout)
    except Exception:
        return {}
    found = {}
    for peer in (data.get("Peers") or []):
        name = (peer.get("Name") or "").rstrip(".")
        if name:
            found[name] = [e.split(":")[0] for e in (peer.get("Endpoints") or [])]
    return found


_DERP = {}


def derp_regions():
    """DERP region code -> city, from the local map.

    A peer's home region is the Tailscale POP nearest to it, so this gives a
    location hint without sending anyone's IP to a geolocation service. Cached:
    the map changes rarely and is the same for every peer.
    """
    if _DERP:
        return _DERP
    try:
        out = subprocess.run(["tailscale", "debug", "derp-map"],
                             capture_output=True, text=True, timeout=20)
        data = json.loads(out.stdout)
    except Exception:
        return _DERP
    for region in (data.get("Regions") or {}).values():
        code = region.get("RegionCode")
        if code:
            _DERP[code] = region.get("RegionName") or code
    return _DERP


def ts_status():
    try:
        out = subprocess.run(["tailscale", "status", "--json"],
                             capture_output=True, text=True, timeout=15)
        return json.loads(out.stdout)
    except Exception:
        return None


def _sample_rates(store, data):
    """Turn cumulative byte counters into per-second rates."""
    now = time.time()
    for peer in (data.get("Peer") or {}).values():
        key = peer_name(peer)
        rx, tx = peer.get("RxBytes", 0), peer.get("TxBytes", 0)
        prev = store._counters.get(key)
        store._counters[key] = (rx, tx, now)
        if not prev:
            continue
        dt = now - prev[2]
        if dt <= 0:
            continue
        # a tailscaled restart zeroes the counters; report no traffic rather
        # than a large negative spike
        drx = max(0, rx - prev[0]) / dt
        dtx = max(0, tx - prev[1]) / dt
        hist = store.rates.setdefault(key, collections.deque(maxlen=HISTORY))
        hist.append((drx, dtx))


def spark(values, n, peak=None):
    """Sparkline of the last n values, scaled to `peak` or their own max."""
    vals = list(values)[-n:]
    if not vals:
        return ""
    hi = peak if peak else max(vals)
    if not hi:
        return "·" * len(vals)
    return "".join(SPARK[min(7, int(v / hi * 7.99))] for v in vals)


def ago(s):
    s = int(max(0, s))
    if s < 3600:
        return "%dm ago" % (s / 60)
    if s < 172800:
        return "%dh ago" % (s / 3600)
    return "%dd ago" % (s / 86400)


def human(n):
    """Byte count in exactly five cells, so columns stay aligned."""
    n = float(n or 0)
    for u in ("B", "K", "M", "G", "T"):
        if n < 1024:
            body = "%.0f%s" % (n, u) if (n >= 10 or u == "B") else "%.1f%s" % (n, u)
            return "%5s" % body
        n /= 1024.0
    return "%5s" % ("%.0fP" % n)


def seen(iso):
    """Age of an ISO-8601 timestamp, coarsely."""
    if not iso:
        return "  -"
    try:
        t = time.mktime(time.strptime(iso[:19], "%Y-%m-%dT%H:%M:%S"))
    except ValueError:
        return "  -"
    s = max(0, time.time() - t - time.timezone)
    if s < 90:
        return "now"
    if s < 5400:
        return "%dm" % (s / 60)
    if s < 172800:
        return "%dh" % (s / 3600)
    return "%dd" % (s / 86400)


TIME_RE = re.compile(r"time[=<]([\d.]+)\s*ms")


class Prober(object):
    """Pings whichever peer is selected, keeping per-peer history.

    Probing every peer continuously would mean two dozen ping processes for
    data nobody is looking at, so exactly one runs at a time and follows the
    selection. History is kept per peer, so returning to one still shows its
    earlier samples.
    """

    def __init__(self):
        self.lock = threading.Lock()
        self.samples = {}          # machine -> deque of rtt|None
        self.want = None           # (machine, ip)
        self.proc = None

    def watch(self, machine, ip):
        with self.lock:
            if self.want and self.want[0] == machine:
                return
            self.want = (machine, ip) if ip else None
        if self.proc and self.proc.poll() is None:
            try:
                self.proc.terminate()
            except OSError:
                pass

    def history(self, machine):
        with self.lock:
            return list(self.samples.get(machine) or [])

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
            with self.lock:
                target = self.want
            if not target:
                time.sleep(0.4)
                continue
            machine, ip = target
            try:
                self.proc = subprocess.Popen(
                    ["ping", "-n", "-O", "-i", "1", ip],
                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                    text=True, bufsize=1)
            except OSError:
                time.sleep(2)
                continue
            for line in self.proc.stdout:
                with self.lock:
                    if not self.want or self.want[0] != machine:
                        break
                    hist = self.samples.setdefault(
                        machine, collections.deque(maxlen=120))
                m = TIME_RE.search(line)
                if m:
                    hist.append(float(m.group(1)))
                elif "no answer yet" in line or "Unreachable" in line:
                    hist.append(None)
            try:
                self.proc.terminate()
            except OSError:
                pass


class Store(object):
    def __init__(self):
        self.lock = threading.Lock()
        self.data = None
        self.endpoints = {}
        self.error = None
        self.wake = threading.Event()
        self._endpoints_at = 0
        self.rates = {}        # machine -> deque of (rx_per_s, tx_per_s)
        self._counters = {}    # machine -> (rx, tx, when)

    def snapshot(self):
        with self.lock:
            return (self.data, dict(self.endpoints), self.error,
                    {k: list(v) for k, v in self.rates.items()})

    def _sample(self, data):
        _sample_rates(self, data)

    def run(self):
        while True:
            d = ts_status()
            eps = None
            if time.time() - self._endpoints_at > 60:
                eps = endpoints_by_peer()
                self._endpoints_at = time.time()
            with self.lock:
                if d is None:
                    self.error = "tailscale CLI unavailable or not logged in"
                else:
                    self.data, self.error = d, None
                if eps:
                    self.endpoints = eps
                if d:
                    self._sample(d)
            self.wake.wait(REFRESH)
            self.wake.clear()


def addresses(peer, eps):
    """The addresses worth copying for a peer, as (label, value) pairs."""
    out = []
    ips = peer.get("TailscaleIPs") or []
    v4 = [i for i in ips if ":" not in i]
    v6 = [i for i in ips if ":" in i]
    if v4:
        out.append(("Tailscale IP", v4[0]))
    dns = (peer.get("DNSName") or "").rstrip(".")
    if dns:
        out.append(("MagicDNS name", dns))

    seen_pub, seen_priv = [], []
    cur = (peer.get("CurAddr") or "").rsplit(":", 1)[0]
    if cur and classify(cur) == "public":
        seen_pub.append(cur)
    for ip in eps.get(dns, []):
        kind = classify(ip)
        if kind == "public" and ip not in seen_pub:
            seen_pub.append(ip)
        elif kind == "private" and ip not in seen_priv:
            seen_priv.append(ip)
    # PrimaryRoutes only: AllowedIPs also carries 0.0.0.0/0 for exit nodes,
    # which would match every address and defeat the ranking entirely.
    routes = [r for r in (peer.get("PrimaryRoutes") or [])
              if r not in ("0.0.0.0/0", "::/0")]
    seen_priv.sort(key=lambda i: lan_rank(i, routes))
    if seen_pub:
        out.append(("Public IP", seen_pub[0]))
    if seen_priv:
        out.append(("Private IP (LAN)", seen_priv[0]))
        if len(seen_priv) > 1:
            out.append(("Other private IP", seen_priv[1]))
    if v6:
        out.append(("Tailscale IPv6", v6[0]))
    return out


def stamp(iso):
    """ISO-8601 to a readable local time, or None when unset."""
    if not iso or iso.startswith("0001-01-01"):
        return None
    try:
        t = time.mktime(time.strptime(iso[:19], "%Y-%m-%dT%H:%M:%S")) - time.timezone
    except ValueError:
        return None
    return time.strftime("%Y-%m-%d %H:%M", time.localtime(t))


def rate(v):
    """Per-second byte rate in six cells."""
    return "%s/s" % human(v).strip().rjust(5)


def activity_rows(rates, w, limit):
    """Live throughput for peers that are actually moving data."""
    active = []
    for name, hist in rates.items():
        if not hist:
            continue
        recent = hist[-30:]
        if max((r + t for r, t in recent), default=0) < 64:   # ignore keepalives
            continue
        active.append((max(r + t for r, t in recent), name, hist))
    active.sort(reverse=True)
    if not active:
        return [DIM + " no peer traffic in the last few minutes"]

    sw = max(8, min(28, w - 42))
    peak = max(a[0] for a in active)
    out = []
    for _, name, hist in active[:limit]:
        rx = [r for r, _ in hist]
        tx = [t for _, t in hist]
        out.append(seg([(TXT, " " + pad(name[:18], 19)),
                        (ONLINE, "↓" + spark(rx, sw, peak)),
                        (RELAY, " ↑" + spark(tx, sw, peak)),
                        (DIM, "  " + rate(rx[-1]) + " " + rate(tx[-1]))], w - 1))
    return out


def info_overlay(peer, eps, users, w, h, rates=None, latency=None):
    """Everything known about one machine, including every address."""
    rows = [title("machine info", w, ACCENT)]
    dns = (peer.get("DNSName") or "").rstrip(".")
    ips = peer.get("TailscaleIPs") or []
    owner = (users.get(str(peer.get("UserID"))) or {}).get("LoginName", "")

    def field(label, value, color=TXT):
        if value in (None, "", []):
            return
        rows.append(seg([(DIM, " %-13s" % label), (color, str(value))], w - 1))

    field("machine", peer_name(peer), ACCENT)
    field("dns name", dns)
    if (peer.get("HostName") or "") != peer_name(peer):
        field("hostname", peer.get("HostName") + "   (self-reported)", DIM)
    field("os", peer.get("OS"))
    region = peer.get("Relay") or ""
    if region:
        city = derp_regions().get(region)
        field("region", "%s — %s" % (region, city) if city else region, ROUTE)
    field("owner", owner)
    field("tags", ", ".join(peer.get("Tags") or []), ROUTE)
    rows.append("")

    up = bool(peer.get("Online"))
    field("status", "online" if up else "offline", ONLINE if up else OFFLINE)
    if peer.get("CurAddr"):
        field("path", "DIRECT via " + peer["CurAddr"], DIRECT)
    else:
        relay = peer.get("Relay") or "?"
        city = derp_regions().get(relay, "")
        field("path", "relayed through DERP %s%s" % (relay,
                                                     " (%s)" % city if city else ""),
              RELAY)
    field("rx / tx", "%s  /  %s" % (human(peer.get("RxBytes")).strip(),
                                    human(peer.get("TxBytes")).strip()))
    field("handshake", stamp(peer.get("LastHandshake")))
    field("last seen", stamp(peer.get("LastSeen")) or ("connected" if up else "-"))
    field("added", stamp(peer.get("Created")))
    rows.append("")

    rows.append(LBL + " addresses")
    for ip in ips:
        field("  tailscale", ip, ACCENT)
    pub, priv, other = [], [], []
    cur = (peer.get("CurAddr") or "").rsplit(":", 1)[0]
    if cur:
        pub.append(cur)
    for ip in eps.get(dns, []):
        kind = classify(ip)
        bucket = {"public": pub, "private": priv}.get(kind, other)
        if ip not in bucket:
            bucket.append(ip)
    routes = [r for r in (peer.get("PrimaryRoutes") or [])
              if r not in ("0.0.0.0/0", "::/0")]
    priv.sort(key=lambda i: lan_rank(i, routes))
    for ip in pub:
        field("  public", ip)
    for ip in priv:
        tag = "  (in advertised subnet)" if any(in_network(ip, r) for r in routes) else ""
        field("  private", ip + tag)
    for ip in other:
        field("  other", ip, DIM)
    if not eps:
        rows.append(DIM + "   endpoints need sudo; only the current path is shown")

    if routes:
        rows.append("")
        rows.append(LBL + " advertises")
        for r in routes[:6]:
            rows.append(seg([(ROUTE, "   " + r)], w - 1))
        if len(routes) > 6:
            rows.append(DIM + "   +%d more" % (len(routes) - 6))
    pings = [x for x in (latency or []) if x is not None]
    if latency:
        rows.append("")
        loss = 100.0 * (len(latency) - len(pings)) / len(latency)
        if pings:
            ordered = sorted(pings)
            jit = (sum(abs(pings[i] - pings[i - 1]) for i in range(1, len(pings)))
                   / (len(pings) - 1)) if len(pings) > 1 else 0.0
            rows.append(LBL + " latency  " + DIM + "(icmp over tailscale, %d samples)"
                        % len(latency))
            rows.append(seg([(DIM, "   now "), (TXT, "%7.2fms" % pings[-1]),
                             (DIM, "  avg "), (TXT, "%7.2fms" % (sum(pings) / len(pings))),
                             (DIM, "  med "), (TXT, "%7.2fms" % ordered[len(ordered) // 2])],
                            w - 1))
            rows.append(seg([(DIM, "   min "), (TXT, "%7.2fms" % ordered[0]),
                             (DIM, "  max "), (TXT, "%7.2fms" % ordered[-1]),
                             (DIM, "  jit "), (TXT, "%7.2fms" % jit)], w - 1))
            rows.append(seg([(DIM, "   loss"), (ONLINE if loss == 0 else RELAY,
                                                "%7.1f%%" % loss)], w - 1))
            lo, hi = ordered[0], ordered[-1]
            span = (hi - lo) or 1.0
            sw = max(10, w - 8)
            marks = []
            for v in latency[-sw:]:
                if v is None:
                    marks.append((RELAY, "×"))
                else:
                    marks.append((ONLINE, SPARK[min(7, int((v - lo) / span * 7.99))]))
            rows.append("   " + "".join(c + ch for c, ch in marks))
        else:
            rows.append(LBL + " latency  " + RELAY + "no replies")
    elif peer.get("Online"):
        rows.append("")
        rows.append(LBL + " latency  " + DIM + "probing…")

    hist = (rates or {}).get(peer_name(peer)) or []
    if hist:
        rows.append("")
        rows.append(LBL + " throughput  " + DIM + "(last %d samples)" % len(hist[-60:]))
        rx = [r for r, _ in hist]
        tx = [t for _, t in hist]
        peak = max(max(rx), max(tx), 1)
        sw = max(10, w - 22)
        rows.append(seg([(DIM, "   down "), (ONLINE, spark(rx, sw, peak)),
                         (DIM, " " + rate(rx[-1]))], w - 1))
        rows.append(seg([(DIM, "   up   "), (RELAY, spark(tx, sw, peak)),
                         (DIM, " " + rate(tx[-1]))], w - 1))
        rows.append(seg([(DIM, "   peak  %s" % rate(peak))], w - 1))

    if peer.get("ExitNodeOption"):
        rows.append("")
        rows.append(seg([(EXIT, " offers itself as an exit node")], w - 1))

    while len(rows) < h - 1:
        rows.append("")
    rows.append(seg([(DIM, " [c]opy addresses · esc, ↵ or i to close")], w - 1))
    return rows


def wrap(text, width):
    return [text[i:i + width] for i in range(0, len(text), width)] or [""]


def copy_overlay(peer, eps, w, h, note):
    rows = [title("copy address", w, ROUTE)]
    rows.append("")
    rows.append(seg([(TXT, " " + peer_name(peer)),
                     (DIM, "  " + (peer.get("OS") or "")),
                     (DIRECT if peer.get("CurAddr") else RELAY,
                      "  " + ("DIRECT" if peer.get("CurAddr")
                              else "relay " + str(peer.get("Relay") or "?")))], w - 1))
    rows.append("")
    pairs = addresses(peer, eps)
    for i, (label, value) in enumerate(pairs, 1):
        rows.append(seg([(ONLINE, " [%d] " % i), (TXT, label)], w - 1))
        for line in wrap(value, max(10, w - 6)):
            rows.append(ACCENT + "     " + line)
        rows.append("")
    if not pairs:
        rows.append(DIM + "  (no addresses available for this peer)")
    if not eps:
        rows.append(DIM + "  LAN addresses need `sudo tailscale debug netmap`;")
        rows.append(DIM + "  passwordless sudo is unavailable, so they are omitted.")
    while len(rows) < h - 2:
        rows.append("")
    rows.append(seg([(DIM, " press 1-%d to copy · esc or c to close" % max(1, len(pairs)))],
                    w - 1))
    rows.append(seg([(ONLINE, " " + note) if note else (DIM, "")], w - 1))
    return rows


def main():
    maybe_help(__doc__)
    global REFRESH
    args = sys.argv[1:]
    if args and args[0] in ("-n", "--refresh"):
        REFRESH = max(1.0, float(args[1]))

    setup()
    keyboard = Keyboard()
    store = Store()
    th = threading.Thread(target=store.run)
    th.daemon = True
    th.start()
    prober = Prober()
    pt = threading.Thread(target=prober.run)
    pt.daemon = True
    pt.start()

    hide_offline = False
    show_graph = True
    selected = 0
    scroll = 0
    view = None          # None | "copy" | "info"
    note = ""
    note_until = 0
    listed = []
    visible = 1
    while True:
        for key in keyboard.poll():
            if view:
                if key == "esc" or key in ("q", "Q"):
                    view = None
                elif key in ("i", "enter"):
                    view = "info" if view != "info" else None
                elif key == "c":
                    view = "copy" if view != "copy" else None
                elif view == "copy" and key.isdigit() and listed:
                    pairs = addresses(listed[min(selected, len(listed) - 1)], eps_now)
                    idx = int(key) - 1
                    if 0 <= idx < len(pairs):
                        label, value = pairs[idx]
                        note = ("✓ copied %s" % label.lower()) if clipboard(value) \
                            else "! no clipboard; select the text with the mouse"
                        note_until = time.time() + 3
                continue
            if key in ("q", "Q"):
                keyboard.restore()
                raise SystemExit(0)
            if key == "r":
                store.wake.set()
            elif key == "o":
                hide_offline = not hide_offline
                selected = 0
            elif key == "g":
                show_graph = not show_graph
            elif key == "n":
                REFRESH = cycle(REFRESH_CHOICES, REFRESH)
                store.wake.set()      # apply the new interval immediately
            elif key == "up":
                selected = max(0, selected - 1)
            elif key == "down":
                selected += 1
            elif key == "pgup":
                selected = max(0, selected - visible)
            elif key == "pgdn":
                selected += visible
            elif key == "home":
                selected = 0
            elif key == "end":
                selected = max(0, len(listed) - 1)
            elif key == "c":
                if listed:
                    view = "copy"
                    note = ""
            elif key in ("i", "enter"):
                if listed:
                    view = "info"

        w, h = size()
        data, eps_now, err, rates = store.snapshot()
        if note and time.time() > note_until:
            note = ""
        rows = [title("tailnet", w, ACCENT)]

        if not data:
            rows.append(seg([(RELAY, " " + (err or "connecting…"))], w - 1))
            draw(rows, w, h)
            time.sleep(0.4)
            continue

        me = data.get("Self") or {}
        peers = list((data.get("Peer") or {}).values())
        online = [p for p in peers if p.get("Online")]
        direct = [p for p in online if p.get("CurAddr")]
        relayed = [p for p in online if not p.get("CurAddr")]
        routers = [p for p in peers if p.get("PrimaryRoutes")]
        exits = [p for p in peers if p.get("ExitNode")]

        rows.append(seg([(TXT, " " + (me.get("DNSName") or "").rstrip(".").split(".")[0]),
                         (DIM, "  " + (me.get("TailscaleIPs") or ["?"])[0]),
                         (DIM, "  " + (data.get("MagicDNSSuffix") or ""))], w - 1))
        rows.append(seg([(ONLINE, " %d online" % len(online)),
                         (DIM, " / %d peers" % len(peers)),
                         (DIRECT, "   %d direct" % len(direct)),
                         (RELAY, "   %d relayed" % len(relayed)),
                         (DIM, "   every %gs" % REFRESH)], w - 1))
        line = [(ROUTE, " %d advertising routes" % len(routers))]
        line.append((EXIT, "   exit node: " + (peer_name(exits[0]) if exits
                                               else "none")))
        rows.append(seg(line, w - 1))
        rows.append("")

        if view and listed:
            chosen = listed[min(selected, len(listed) - 1)]
            if view == "info":
                users = {str(k): v for k, v in (data.get("User") or {}).items()}
                draw(info_overlay(chosen, eps_now, users, w, h, rates,
                                  prober.history(peer_name(chosen))), w, h)
            else:
                draw(copy_overlay(chosen, eps_now, w, h, note), w, h)
            time.sleep(0.1)
            continue

        if show_graph:
            rows.append(LBL + " ── LIVE THROUGHPUT ── " + DIM + "peers moving data")
            rows.extend(activity_rows(rates, w, 4))
            rows.append("")

        wide = w >= 62
        # machine names are long; spend spare width on them rather than padding
        namew = max(16, min(32, w - 45)) if wide else max(12, w - 22)
        head = " %s %-8s %-7s" % (pad("MACHINE", namew + 1), "OS", "PATH")
        if wide:
            head += " %5s %5s %5s" % ("RX", "TX", "SEEN")
        rows.append(LBL + pad(head, w - 1))
        if wide:
            span = daemon_uptime()
            rows.append(seg([(DIM, " rx/tx = this host ↔ peer, since tailscaled "
                                   "started" + ((" " + ago(span)) if span else ""))],
                            w - 1))

        def order(p):
            return (not p.get("Online"), not p.get("CurAddr"),
                    -(p.get("RxBytes", 0) + p.get("TxBytes", 0)))

        if listed:
            sel_peer = listed[min(selected, len(listed) - 1)]
            ip4 = [i for i in (sel_peer.get("TailscaleIPs") or []) if ":" not in i]
            # Nothing is learned by pinging ourselves, and the round trip
            # would read as a suspiciously good link.
            prober.watch(peer_name(sel_peer),
                         ip4[0] if (ip4 and sel_peer.get("Online")
                                    and not sel_peer.get("_self")) else None)

        listed = [p for p in sorted(peers, key=order)
                  if not (hide_offline and not p.get("Online"))]
        # This machine belongs in the list of machines - it was only ever in
        # the header - but never in the counts above it: "3 direct, 2
        # relayed" describes connections out of here, and there is no
        # connection from here to here. It pins to the top rather than
        # sorting by traffic, because where you are is not a ranking.
        if me:
            listed.insert(0, dict(me, _self=True))
        selected = max(0, min(selected, len(listed) - 1)) if listed else 0
        visible = max(1, h - len(rows) - 2)
        if selected < scroll:
            scroll = selected
        elif selected >= scroll + visible:
            scroll = selected - visible + 1
        scroll = max(0, min(scroll, max(0, len(listed) - visible)))

        for idx in range(scroll, min(len(listed), scroll + visible)):
            p = listed[idx]
            if len(rows) >= h - 2:
                break
            mine = bool(p.get("_self"))
            up = True if mine else bool(p.get("Online"))
            here = idx == selected
            tint = bg(28, 44, 62) if here else ""
            path_direct = bool(p.get("CurAddr"))
            # "this" rather than DIRECT or a relay name: the path column
            # answers how the traffic gets there, and for this machine it
            # does not go anywhere.
            path = "this" if mine else (
                "DIRECT" if path_direct else (p.get("Relay") or "?"))
            name = peer_name(p)
            line = [(tint + (ACCENT if mine else (ONLINE if up else OFFLINE)),
                     ("▸" if here else " ")
                     + "%s " % ("◆" if mine else ("●" if up else "○"))),
                    (tint + (TXT if up else OFFLINE),
                     pad(name[:namew - 1], namew)),
                    (tint + DIM, "%-8s" % (p.get("OS") or "?")[:8]),
                    (tint + (ACCENT if mine else
                             (DIRECT if path_direct else RELAY)),
                     "%-7s" % (path if up else "-"))]
            if wide:
                line.append((tint + DIM, " %5s %5s" % (human(p.get("RxBytes")),
                                                       human(p.get("TxBytes")))))
                line.append((tint + DIM, " %5s" % (seen(p.get("LastSeen"))
                                                   if not up else "now")))
            if p.get("PrimaryRoutes"):
                line.append((tint + ROUTE, " ⇄"))
            if here:
                line.append((tint, " " * w))
            rows.append(seg(line, w - 1))

        while len(rows) < h - 2:
            rows.append("")
        if routers:
            first = routers[0]
            rts = first.get("PrimaryRoutes") or []
            rows.append(seg([(ROUTE, " ⇄ "), (DIM, "%s routes " % first.get("HostName")),
                             (TXT, ", ".join(rts[:2])),
                             (DIM, (" +%d more" % (len(rts) - 2)) if len(rts) > 2 else "")],
                            w - 1))
        hints = [[(ACCENT, "↑↓"), (DIM, " select")],
                 [(DIM, "↵/[i]nfo")], [(DIM, "[c]opy")], [(DIM, "[g]raph")],
                 [(DIM, "[o]ffline")], [(DIM, "[n]=%gs" % REFRESH)],
                 [(DIM, "[r]efresh")], [(DIM, "[q]uit")]]
        for line in pack_hints(hints, w - 2):
            rows.append(" " + line)
        draw(rows, w, h)
        time.sleep(0.3)


main()
