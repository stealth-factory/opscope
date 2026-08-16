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
"""What is listening on this machine, what started it, and who can reach it.

On a box running several agents at once, "which port is that project on" and
"is this reachable from outside" are asked constantly and answered badly.
`lsof -i` gives a pid and a port and stops there.

Each row is one listening socket: the port, what it is bound to, what kind of
server it is, the project directory it was started from - the label that
actually identifies a dev server - how long it has been up, and whether
anything outside this machine can reach it.

    python3 ports.py [-n SECONDS]

Read from /proc alone: the socket table for the ports, and each process's own
cmdline and cwd for the rest. Exposure comes from `tailscale serve status`
where Tailscale is installed. Keys: up/down select, o hides system ports,
r refreshes, q quits.
"""
import json
import os
import re
import socket
import struct
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, bg, draw, load_config, maybe_help,
                    pack_hints, pad, rgb, seg, setup, size, title)

_CFG = load_config("ports", {
    # Ports that are part of the machine rather than something you started.
    # Hidden behind `o` by default: they are never the answer to "which port
    # is my dev server on".
    "system_ports": [22, 53, 123, 323, 631, 5353],
    "refresh": 4,
})

REFRESH = max(1.0, float(_CFG["refresh"]))
SYSTEM_PORTS = set(int(p) for p in (_CFG["system_ports"] or []))

OK = rgb(90, 240, 160)
WARN = rgb(255, 200, 90)
BAD = rgb(255, 100, 110)
DIM = rgb(127, 147, 172)
GRID = rgb(60, 78, 98)
TXT = rgb(225, 235, 245)
LBL = rgb(130, 165, 200)
ACCENT = rgb(150, 210, 255)
PORT = rgb(160, 220, 255)
OPEN = rgb(255, 170, 120)      # bound to every interface
LOCAL = rgb(120, 200, 160)     # loopback only

# Process titles worth recognising. Checked in order, first match wins, so
# the specific ones come before `node` and `python`.
KINDS = (
    (r"^next-server", "Next.js"),
    (r"node_modules/\.bin/vite|[/ ]vite(\s|$)", "Vite"),
    (r"node_modules/next/dist", "Next.js"),
    (r"react-scripts", "React"),
    (r"webpack", "webpack"),
    (r"nuxt", "Nuxt"),
    (r"astro", "Astro"),
    (r"remix", "Remix"),
    (r"turbo(\s|$)", "Turborepo"),
    (r"uvicorn", "uvicorn"),
    (r"gunicorn", "gunicorn"),
    (r"flask", "Flask"),
    (r"django|manage\.py", "Django"),
    (r"rails", "Rails"),
    (r"postgres", "Postgres"),
    (r"redis-server", "Redis"),
    (r"mysqld|mariadb", "MySQL"),
    (r"mongod", "MongoDB"),
    (r"docker-proxy", "Docker"),
    (r"ollama", "Ollama"),
    (r"code-server|vscode-server", "VS Code"),
    (r"^herdr|/herdr", "Herdr"),
    (r"tailscaled", "Tailscale"),
    (r"sshd", "SSH"),
    (r"systemd-resolve", "DNS"),
    (r"[/ ]python[0-9.]*(\s|$)", "Python"),
    (r"[/ ]node(\s|$)", "Node"),
)

# Ports whose owner is usually root, so /proc will not say what it is. Naming
# them by convention is a guess, and is marked as one.
BY_PORT = {22: "SSH", 53: "DNS", 80: "HTTP", 123: "NTP", 443: "HTTPS",
           631: "printing", 3306: "MySQL", 5432: "Postgres", 6379: "Redis",
           5353: "mDNS", 27017: "MongoDB"}

VERSION = re.compile(r"\(v([0-9][0-9a-zA-Z.\-]*)\)")
# Tailscale hands out 100.64.0.0/10 and fd7a:115c:a1e0::/48. A socket bound
# to one is reachable by tailnet peers and nobody else, which is a different
# answer from either "all" or "local".
TAILNET_V4 = re.compile(r"^100\.(6[4-9]|[7-9]\d|1[0-1]\d|12[0-7])\.")


def run(args):
    try:
        out = subprocess.run(args, capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError):
        return ""
    return out.stdout if out.returncode == 0 else ""


def hex_addr(text, family):
    """The bind address from /proc's little-endian hex."""
    try:
        if family == socket.AF_INET:
            return socket.inet_ntoa(struct.pack("<I", int(text, 16)))
        raw = bytes.fromhex(text)
        return socket.inet_ntop(
            socket.AF_INET6,
            b"".join(raw[i:i + 4][::-1] for i in range(0, 16, 4)))
    except (ValueError, OSError, struct.error):
        return "?"


def listening():
    """Every listening TCP socket, from the kernel's own table."""
    out = []
    for path, family in (("/proc/net/tcp", socket.AF_INET),
                         ("/proc/net/tcp6", socket.AF_INET6)):
        try:
            with open(path) as f:
                lines = f.read().splitlines()[1:]
        except OSError:
            continue
        for line in lines:
            cols = line.split()
            if len(cols) < 10 or cols[3] != "0A":
                continue
            addr, port = cols[1].rsplit(":", 1)
            out.append({"port": int(port, 16), "bind": hex_addr(addr, family),
                        "inode": cols[9], "v6": family == socket.AF_INET6})
    return out


def socket_owners():
    """inode -> pid, for every process this user can read.

    Root's sockets are not readable, so sshd and the like arrive unowned.
    That is stated on screen rather than papered over: a widget claiming to
    know what is behind port 22 when it cannot is worse than one that says
    so.
    """
    owners = {}
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            fds = os.listdir("/proc/%s/fd" % pid)
        except OSError:
            continue
        for fd in fds:
            try:
                target = os.readlink("/proc/%s/fd/%s" % (pid, fd))
            except OSError:
                continue
            if target.startswith("socket:["):
                owners[target[8:-1]] = pid
    return owners


def project_name(cwd):
    """What a directory calls itself, for the label a person would use.

    A deleted directory still names the project it was - a dev server left
    running on a worktree that has since been removed is exactly the thing
    this widget should surface, and blanking the column hides it.
    """
    if cwd and cwd.endswith(" (deleted)"):
        return os.path.basename(cwd[:-10].rstrip("/")) + " ✗"
    if not cwd:
        return ""
    try:
        with open(os.path.join(cwd, "package.json")) as f:
            name = (json.load(f) or {}).get("name")
        if name:
            return str(name)
    except (OSError, ValueError):
        pass
    return os.path.basename(cwd.rstrip("/"))


def kind_of(cmdline, port):
    """What sort of server this is.

    From the process title where there is one - Next.js rewrites its own to
    `next-server (v16.3.0)`, which is the version as well as the name - and
    from the argv path when the title is just `node`, since a dev server
    launched through a package manager is several layers of wrapper deep.
    """
    if not cmdline:
        guess = BY_PORT.get(port)
        return ("%s?" % guess, True) if guess else ("", True)
    for pattern, name in KINDS:
        if re.search(pattern, cmdline):
            found = VERSION.search(cmdline)
            return ("%s %s" % (name, found.group(1)) if found else name), False
    return os.path.basename(cmdline.split()[0]), False


def process_info(pid):
    try:
        with open("/proc/%s/cmdline" % pid, "rb") as f:
            cmdline = f.read().replace(b"\x00", b" ").decode(
                "utf8", "replace").strip()
    except OSError:
        cmdline = ""
    try:
        cwd = os.readlink("/proc/%s/cwd" % pid)
    except OSError:
        cwd = ""
    try:
        started = os.stat("/proc/%s" % pid).st_ctime
    except OSError:
        started = None
    return cmdline, cwd, started


def exposure():
    """Ports Tailscale is serving, and whether the world can see them.

    `serve` is tailnet-only; `funnel` is public and limited to three ports.
    Both are reported by the same command, so the funnel list is what
    separates them.
    """
    served, public = {}, set()
    text = run(["tailscale", "serve", "status"])
    for line in text.splitlines():
        found = re.search(r"proxy\s+https?://(?:127\.0\.0\.1|localhost):(\d+)",
                          line)
        if found:
            served[int(found.group(1))] = "tailnet"
    funnel = run(["tailscale", "funnel", "status"])
    if "tailnet only" not in funnel:
        for line in funnel.splitlines():
            found = re.search(r"proxy\s+https?://(?:127\.0\.0\.1|localhost)"
                              r":(\d+)", line)
            if found:
                public.add(int(found.group(1)))
    for port in public:
        served[port] = "public"
    return served


def span(seconds):
    if seconds is None:
        return "--"
    s = max(0, int(seconds))
    if s < 60:
        return "%ds" % s
    if s < 3600:
        return "%dm" % (s // 60)
    if s < 86400:
        return "%dh" % (s // 3600)
    return "%dd" % (s // 86400)


def scan():
    """One entry per listening socket, plus anything served but not bound."""
    owners = socket_owners()
    served = exposure()
    rows, seen = [], set()
    now = time.time()
    for sock in listening():
        pid = owners.get(sock["inode"])
        cmdline, cwd, started = process_info(pid) if pid else ("", "", None)
        kind, guessed = kind_of(cmdline, sock["port"])
        rows.append({
            "port": sock["port"], "bind": sock["bind"], "v6": sock["v6"],
            "pid": pid, "cmdline": cmdline, "cwd": cwd,
            "kind": kind, "guessed": guessed,
            "project": project_name(cwd),
            "gone": cwd.endswith("(deleted)") if cwd else False,
            "up": (now - started) if started else None,
            "exposed": served.get(sock["port"], ""),
        })
        seen.add(sock["port"])
    # A port Tailscale forwards to with nothing behind it is worth its own
    # row: the URL exists, answers 502, and nothing in `lsof` explains why.
    for port, how in served.items():
        if port not in seen:
            rows.append({"port": port, "bind": "", "v6": False, "pid": None,
                         "cmdline": "", "cwd": "", "kind": "nothing listening",
                         "guessed": False, "project": "", "gone": False,
                         "up": None, "exposed": how, "orphan": True})
    rows.sort(key=lambda r: (r["port"] in SYSTEM_PORTS, r["port"]))
    return rows


class Store(object):
    def __init__(self):
        self.lock = threading.Lock()
        self.rows = []
        self.error = None
        self.fetched = 0
        self.wake = threading.Event()

    def snapshot(self):
        with self.lock:
            return list(self.rows), self.fetched, self.error

    def run(self):
        # A dead poller looks exactly like a machine with nothing running on
        # it, which is a plausible enough state to be believed.
        try:
            self.poll()
        except Exception as e:
            with self.lock:
                self.error = "poller stopped: %s: %s" % (type(e).__name__,
                                                         str(e)[:70])

    def poll(self):
        while True:
            rows = scan()
            with self.lock:
                self.rows = rows
                self.fetched = time.time()
            self.wake.wait(REFRESH)
            self.wake.clear()


def bind_note(row):
    """What the bound address means for who can reach it.

    The address itself is rarely the answer to that question and is always
    too wide for the column - a tailnet IPv6 address is 24 characters and
    pushed every field after it off the line. What matters is the class:
    every interface, loopback only, or bound to one particular network.
    """
    if row.get("orphan"):
        return "--", DIM
    bind = row["bind"]
    if bind in ("0.0.0.0", "::"):
        return "all", OPEN
    if bind.startswith("127.") or bind == "::1":
        return "local", LOCAL
    if TAILNET_V4.match(bind) or bind.startswith("fd7a:115c:a1e0"):
        return "tailnet", ACCENT
    return bind[:7], TXT


def main():
    maybe_help(__doc__)
    global REFRESH
    args = sys.argv[1:]
    while args and args[0] in ("-n", "--refresh"):
        REFRESH = max(1.0, float(args[1]))
        args = args[2:]

    setup()
    keyboard = Keyboard()
    store = Store()
    threading.Thread(target=store.run, daemon=True).start()
    selected, hide_system, scroll = 0, True, 0

    while True:
        for key in keyboard.poll():
            if key in ("q", "Q"):
                raise SystemExit(0)
            if key in ("up", "k"):
                selected -= 1
            elif key in ("down", "j"):
                selected += 1
            elif key == "o":
                hide_system = not hide_system
            elif key == "r":
                store.wake.set()

        w, h = size()
        all_rows, fetched, err = store.snapshot()
        shown = [r for r in all_rows
                 if not (hide_system and r["port"] in SYSTEM_PORTS)]
        selected = max(0, min(selected, len(shown) - 1)) if shown else 0
        mine = sum(1 for r in all_rows if r["pid"])
        out = sum(1 for r in all_rows if r.get("exposed"))

        rows = [title("dev servers", w, PORT)]
        rows.append(seg([(DIM, " %d listening" % len(all_rows)),
                         (DIM, " · %d yours" % mine),
                         (DIM, " · "),
                         (OK if out else DIM, "%d reachable off-box" % out),
                         (DIM, "   every %gs" % REFRESH)], w - 1))
        if err:
            rows.append(seg([(BAD, " ! " + err)], w - 1))
        rows.append("")

        wide = w >= 78
        # The project column takes whatever the fixed ones leave, because it
        # is the one that identifies the server and the one whose contents
        # are a directory name of any length.
        fixed = 1 + 6 + 8 + 18 + (6 + 8 if wide else 0)
        name_w = max(8, (w - 1) - fixed)
        rows.append(seg([(DIM, "  PORT  BIND    WHAT              "),
                         (DIM, pad("PROJECT", name_w)),
                         (DIM, "UP    EXPOSED" if wide else "")], w - 1))
        visible = max(1, h - len(rows) - 3)
        if selected < scroll:
            scroll = selected
        elif selected >= scroll + visible:
            scroll = selected - visible + 1
        scroll = max(0, min(scroll, max(0, len(shown) - visible)))

        for i in range(scroll, min(len(shown), scroll + visible)):
            row = shown[i]
            here = i == selected
            tint = bg(28, 44, 62) if here else ""
            note, note_colour = bind_note(row)
            kind = row["kind"] or "(not ours)"
            line = [(tint + (ACCENT if here else PORT),
                     ("▸" if here else " ") + "%-6d" % row["port"]),
                    (tint + note_colour, "%-8s" % note),
                    (tint + (DIM if row["guessed"] or not row["kind"] else TXT),
                     pad(kind, 18)),
                    (tint + (WARN if row["gone"] else TXT),
                     pad(row["project"] or ("—" if row["pid"] else ""),
                         name_w))]
            if wide:
                line.append((tint + DIM, "%-6s" % span(row["up"])))
                line.append((tint + (OK if row["exposed"] == "tailnet"
                                     else BAD if row["exposed"] == "public"
                                     else GRID),
                             row["exposed"] or "-"))
            if here:
                line.append((tint, " " * w))
            rows.append(seg(line, w - 1))

        while len(rows) < h - 2:
            rows.append("")
        hints = [[(ACCENT, "↑↓"), (DIM, " select")],
                 [(DIM, "[o]%s system" % ("show" if hide_system else "hide"))],
                 [(DIM, "[r]efresh")], [(DIM, "[q]uit")]]
        for line in pack_hints(hints, w - 2):
            rows.append(" " + line)
        draw(rows, w, h)
        time.sleep(0.3)


main()
