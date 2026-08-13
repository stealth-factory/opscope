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

The column that matters is PATH. A peer is either DIRECT, meaning NAT traversal
succeeded and traffic goes peer-to-peer, or it is relayed through a named DERP
region, meaning every packet round-trips through Tailscale's infrastructure.
Relayed peers can be dramatically slower and the difference is invisible in
`tailscale status` output unless you look for it.

Peers advertising subnet routes are flagged, since those only reach you if this
node runs with --accept-routes.

    python3 tailnet.py [-n SECONDS]

Keys: r refreshes now, o toggles hiding offline peers, q quits.
Needs the `tailscale` CLI; no root required.
"""
import json
import os
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, draw, maybe_help, pad, rgb, seg, setup, size,
                    title)

REFRESH = 5.0

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


def ts_status():
    try:
        out = subprocess.run(["tailscale", "status", "--json"],
                             capture_output=True, text=True, timeout=15)
        return json.loads(out.stdout)
    except Exception:
        return None


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
        self.error = None
        self.wake = threading.Event()

    def snapshot(self):
        with self.lock:
            return self.data, self.error

    def run(self):
        while True:
            d = ts_status()
            with self.lock:
                if d is None:
                    self.error = "tailscale CLI unavailable or not logged in"
                else:
                    self.data, self.error = d, None
            self.wake.wait(REFRESH)
            self.wake.clear()


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
    while True:
        for key in keyboard.poll():
            if key in ("q", "Q"):
                keyboard.restore()
                raise SystemExit(0)
            if key == "r":
                store.wake.set()
            elif key == "o":
                hide_offline = not hide_offline

        w, h = size()
        data, err = store.snapshot()
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
        line.append((EXIT, "   exit node: " + (exits[0].get("HostName") if exits
                                               else "none")))
        rows.append(seg(line, w - 1))
        rows.append("")

        wide = w >= 62
        head = " %-24s %-8s %-7s" % ("PEER", "OS", "PATH")
        if wide:
            head += " %5s %5s %5s" % ("RX", "TX", "SEEN")
        rows.append(LBL + pad(head, w - 1))

        def order(p):
            return (not p.get("Online"), not p.get("CurAddr"),
                    -(p.get("RxBytes", 0) + p.get("TxBytes", 0)))

        for p in sorted(peers, key=order):
            if len(rows) >= h - 1:
                break
            up = bool(p.get("Online"))
            if hide_offline and not up:
                continue
            path_direct = bool(p.get("CurAddr"))
            path = "DIRECT" if path_direct else (p.get("Relay") or "?")
            name = (p.get("HostName") or "?")
            line = [(ONLINE if up else OFFLINE, " %s " % ("●" if up else "○")),
                    (TXT if up else OFFLINE, pad(name[:22], 23)),
                    (DIM, "%-8s" % (p.get("OS") or "?")[:8]),
                    (DIRECT if path_direct else RELAY,
                     "%-7s" % (path if up else "-"))]
            if wide:
                line.append((DIM, " %5s %5s" % (human(p.get("RxBytes")),
                                                human(p.get("TxBytes")))))
                line.append((DIM, " %5s" % (seen(p.get("LastSeen")) if not up else "now")))
            if p.get("PrimaryRoutes"):
                line.append((ROUTE, " ⇄"))
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
        rows.append(seg([(DIM, " [r]efresh  [o]ffline  [q]uit")], w - 1))
        draw(rows, w, h)
        time.sleep(0.3)


main()
