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

    python3 tailnet.py [-n SECONDS]

Keys: up/down select a peer, c or Enter opens a copy sheet offering its
Tailscale IP, MagicDNS name, public IP and LAN IP, r refreshes now, o hides
offline peers, q quits. Copying uses OSC 52, so it reaches the clipboard of
the machine you are typing at even over SSH.

Needs the `tailscale` CLI. Peer LAN addresses come from `tailscale debug
netmap`, which needs root; it is attempted with `sudo -n` and simply omitted
when that would prompt, so nothing here requires privilege.
"""
import json
import os
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, bg, clipboard, draw, load_config, maybe_help,
                    pad, rgb, seg, setup, size, title)

_CFG = load_config("tailnet", {"refresh": 5.0})
REFRESH = float(_CFG["refresh"])

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


def ts_status():
    try:
        out = subprocess.run(["tailscale", "status", "--json"],
                             capture_output=True, text=True, timeout=15)
        return json.loads(out.stdout)
    except Exception:
        return None


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


class Store(object):
    def __init__(self):
        self.lock = threading.Lock()
        self.data = None
        self.endpoints = {}
        self.error = None
        self.wake = threading.Event()
        self._endpoints_at = 0

    def snapshot(self):
        with self.lock:
            return self.data, dict(self.endpoints), self.error

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

    hide_offline = False
    selected = 0
    scroll = 0
    overlay = False
    note = ""
    note_until = 0
    listed = []
    visible = 1
    while True:
        for key in keyboard.poll():
            if overlay:
                if key in ("esc", "c", "q", "Q", "enter"):
                    overlay = False
                elif key.isdigit() and listed:
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
            elif key in ("c", "enter"):
                if listed:
                    overlay = True
                    note = ""

        w, h = size()
        data, eps_now, err = store.snapshot()
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
                         (RELAY, "   %d relayed" % len(relayed))], w - 1))
        line = [(ROUTE, " %d advertising routes" % len(routers))]
        line.append((EXIT, "   exit node: " + (peer_name(exits[0]) if exits
                                               else "none")))
        rows.append(seg(line, w - 1))
        rows.append("")

        if overlay and listed:
            draw(copy_overlay(listed[min(selected, len(listed) - 1)], eps_now,
                              w, h, note), w, h)
            time.sleep(0.1)
            continue

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

        listed = [p for p in sorted(peers, key=order)
                  if not (hide_offline and not p.get("Online"))]
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
            up = bool(p.get("Online"))
            here = idx == selected
            tint = bg(28, 44, 62) if here else ""
            path_direct = bool(p.get("CurAddr"))
            path = "DIRECT" if path_direct else (p.get("Relay") or "?")
            name = peer_name(p)
            line = [(tint + (ONLINE if up else OFFLINE),
                     ("▸" if here else " ") + "%s " % ("●" if up else "○")),
                    (tint + (TXT if up else OFFLINE),
                     pad(name[:namew - 1], namew)),
                    (tint + DIM, "%-8s" % (p.get("OS") or "?")[:8]),
                    (tint + (DIRECT if path_direct else RELAY),
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
        rows.append(seg([(DIM, " ↑↓ select · [c]opy · [r]efresh [o]ffline [q]uit")], w - 1))
        draw(rows, w, h)
        time.sleep(0.3)


main()
