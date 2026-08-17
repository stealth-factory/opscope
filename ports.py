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

Each row is one listening service: the port, what it is bound to, what kind of
server it is, the project directory it was started from - the label that
actually identifies a dev server - how long it has been up, and whether
anything outside this machine can reach it. A server listening on both IPv4
and IPv6 is one row, not two.

    python3 ports.py [-n SECONDS]

Read from /proc alone: the socket table for the ports, and each process's own
cmdline and cwd for the rest. Exposure comes from `tailscale serve status`
where Tailscale is installed. Another user's sockets cannot be tied to a
process without root, so those rows name the owner the socket table gives
and are hidden behind o along with the system ports.

k stops the selected server, after a confirmation and only for a process you
own: SIGTERM to its process group, which is what Ctrl-C in its own terminal
would have sent, then the offer of SIGKILL if it is still up three seconds
later.

Enter opens a second screen for one port, where there is more to show than the
table holds: the command behind it, every address it can actually be reached
at - bounded by what the socket is bound to - and c to copy one. From there s
and t publish it over Tailscale, to the tailnet or to the internet, and d
opens a Cloudflare quick tunnel. Each asks first.

Keys: up/down select, enter opens, esc goes back, c copies, s serves,
t funnels, d tunnels, k kills, o hides the machine's own ports, r refreshes,
q quits.
"""
import getpass
import json
import os
import pwd
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (Keyboard, bg, clipboard, draw, load_config, maybe_help,
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


def bind_class(bind):
    """Who can reach a socket bound to this address."""
    if bind in ("0.0.0.0", "::"):
        return "all"
    if bind.startswith("127.") or bind == "::1":
        return "local"
    if TAILNET_V4.match(bind) or bind.startswith("fd7a:115c:a1e0"):
        return "tailnet"
    return bind


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
            # The uid is in the table even where the process behind it is
            # not reachable, which is the difference between "somebody
            # else's" and "a mystery".
            out.append({"port": int(port, 16), "bind": hex_addr(addr, family),
                        "inode": cols[9], "uid": int(cols[7])})
    return out


def owner_name(uid):
    """Whose socket it is, by name where the machine has one."""
    if uid == os.getuid():
        return ""
    try:
        return pwd.getpwuid(uid).pw_name
    except KeyError:
        return "uid %d" % uid


def theirs(row):
    """Whether a row is part of the machine rather than something you started.

    Two kinds qualify and both belong behind the same key: a well-known
    system port, and any socket owned by another user - which in practice
    means root's daemons, the ones that cannot be signalled without root and
    are never the answer to "which port is my dev server on". A port served
    by Tailscale with nothing behind it is neither: nobody else configured
    that.
    """
    if row.get("orphan"):
        return False
    return row["port"] in SYSTEM_PORTS or bool(row.get("user"))


def socket_owners():
    """inode -> pid, for every process this user can read.

    Root's sockets are not readable, so sshd and the like arrive unowned.
    That is stated on screen rather than papered over: a widget claiming to
    know what is behind port 22 when it cannot is worse than one that says
    so.

    The pid is an int, not the string /proc was listed as: it is handed to
    os.kill when a row is stopped, and that takes no strings.
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
                owners[target[8:-1]] = int(pid)
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


def interfaces():
    """Every address this machine holds, by interface.

    Link-local is dropped: an fe80:: address needs a zone index to be usable
    and is never what somebody wants pasted into a browser.
    """
    found = []
    try:
        data = json.loads(run(["ip", "-j", "addr"]) or "[]")
    except ValueError:
        return found
    for link in data:
        for addr in link.get("addr_info") or []:
            ip = addr.get("local") or ""
            if not ip or ip.startswith("fe80:"):
                continue
            found.append((link.get("ifname") or "?", ip,
                          addr.get("family") == "inet6"))
    return found


def tailnet_self():
    """This node's tailnet name and addresses, and whether it may Funnel.

    Funnel is off unless the tailnet's policy grants the node the attribute,
    and the node knows: the capability is in the map the coordination server
    hands it. Asking here means the widget can say so instead of offering a
    key that only ever returns an error.
    """
    out = {"name": "", "ips": [], "funnel": False, "operator": False}
    try:
        data = json.loads(run(["tailscale", "status", "--json"]) or "null")
    except ValueError:
        return out
    self_node = (data or {}).get("Self") or {}
    out["name"] = (self_node.get("DNSName") or "").rstrip(".")
    out["ips"] = list(self_node.get("TailscaleIPs") or [])
    out["funnel"] = any("cap/funnel" in cap
                        for cap in (self_node.get("CapMap") or {}))
    # Changing the serve config is a root operation unless this user has been
    # named the operator. Worth knowing before the key is pressed, since the
    # fix is a one-off command rather than anything the widget can do.
    try:
        prefs = json.loads(run(["tailscale", "debug", "prefs"]) or "null")
    except ValueError:
        prefs = None
    who = (prefs or {}).get("OperatorUser")
    out["operator"] = os.getuid() == 0 or who == getpass.getuser()
    return out


def host_part(ip, v6):
    """An address as it goes in a URL - IPv6 needs its brackets."""
    return "[%s]" % ip if v6 else ip


def url_for(host, v6, port):
    """A URL for a host and port, with the scheme the port implies."""
    scheme = "https" if port in (443, 8443) else "http"
    return "%s://%s:%d" % (scheme, host_part(host, v6), port)


def addresses(row, net, cfg):
    """Where this port can actually be reached, most local first.

    Bounded by what the socket is bound to, which is the part that gets got
    wrong: a server on 127.0.0.1 is not reachable at this machine's LAN
    address no matter how many addresses the machine has, and offering one
    to copy would hand somebody a URL that cannot work. Only a socket bound
    to every interface gets the full list.

    A served port is the exception worth keeping: Tailscale proxies to it
    over loopback, so its https URL works even for a loopback-only server.
    """
    port, reach = row["port"], bind_class(row["bind"])
    found = []
    url = served_url(cfg, port)
    if url:
        found.append((url, "tailnet · via serve"))
    # Nothing is listening, so every address below would refuse the
    # connection. The serve URL above is the only one that exists, and it
    # answers 502 - which is the whole reason this row is on screen.
    if row.get("orphan"):
        return found
    if reach == "local":
        found.append((url_for("127.0.0.1", False, port), "this machine only"))
        return found
    if reach == "tailnet":
        for ip in net["ips"]:
            found.append((url_for(ip, ":" in ip, port), "tailnet"))
        if net["name"]:
            found.append((url_for(net["name"], False, port), "tailnet · name"))
        return found
    if reach != "all":
        # Bound to one particular address, so that address is the answer.
        found.append((url_for(row["bind"], ":" in row["bind"], port),
                      "this interface"))
        return found
    found.append((url_for("127.0.0.1", False, port), "this machine"))
    tail = set(net["ips"])
    for name, ip, v6 in interfaces():
        if ip.startswith("127.") or ip == "::1":
            continue
        found.append((url_for(ip, v6, port), "tailnet" if ip in tail else name))
    if net["name"]:
        found.append((url_for(net["name"], False, port), "tailnet · name"))
    return found


def serve_config():
    """Tailscale's own serve configuration, as it reports it.

    The JSON form rather than the text: the detail view needs the URL a
    served port answers on, and putting a second port behind the same node
    needs to know which mounts are already taken. Both are structure the
    text output only implies.
    """
    try:
        return json.loads(run(["tailscale", "serve", "status", "--json"])
                          or "null") or {}
    except ValueError:
        return {}


def served_url(cfg, port):
    """The https URL a served port answers on, where one is configured."""
    for mount, web in (cfg.get("Web") or {}).items():
        for path, handler in (web.get("Handlers") or {}).items():
            proxy = handler.get("Proxy") or ""
            if re.search(r":%d(/|$)" % port, proxy):
                host, _, listen = mount.partition(":")
                return "https://%s%s%s" % (
                    host, "" if listen in ("", "443") else ":" + listen,
                    path if path != "/" else "/")
    return ""


def exposure():
    """Ports Tailscale is serving, and whether the world can see them.

    `serve` is tailnet-only; `funnel` is public and limited to three ports.
    Both are reported by the same command, so the funnel list is what
    separates them.
    """
    served, public = {}, set()
    cfg = serve_config()
    for web in (cfg.get("Web") or {}).values():
        for handler in (web.get("Handlers") or {}).values():
            found = re.search(r"https?://(?:127\.0\.0\.1|localhost|\[::1\]"
                              r"|::1):(\d+)", handler.get("Proxy") or "")
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
    """One entry per listening service, plus anything served but not bound."""
    owners = socket_owners()
    served = exposure()
    rows, seen, services = [], set(), {}
    now = time.time()
    for sock in listening():
        pid = owners.get(sock["inode"])
        # A server that listens on both address families is two sockets in
        # the kernel table but one thing to know about: 0.0.0.0 alongside ::,
        # or a tailnet IPv4 address alongside its IPv6. Same port, same owner
        # and the same answer to "who can reach it" means one row. Any of the
        # three differing is a real second row - 127.0.0.1:8080 and
        # 100.x:8080 are not the same answer even from the same process.
        key = (sock["port"], pid, bind_class(sock["bind"]))
        if key in services:
            services[key]["families"] += 1
            continue
        cmdline, cwd, started = process_info(pid) if pid else ("", "", None)
        kind, guessed = kind_of(cmdline, sock["port"])
        row = {
            "port": sock["port"], "bind": sock["bind"], "families": 1,
            "pid": pid, "cmdline": cmdline, "cwd": cwd,
            "kind": kind, "guessed": guessed, "user": owner_name(sock["uid"]),
            "project": project_name(cwd),
            "gone": cwd.endswith("(deleted)") if cwd else False,
            "up": (now - started) if started else None,
            "exposed": served.get(sock["port"], ""),
        }
        services[key] = row
        rows.append(row)
        seen.add(sock["port"])
    # A port Tailscale forwards to with nothing behind it is worth its own
    # row: the URL exists, answers 502, and nothing in `lsof` explains why.
    for port, how in served.items():
        if port not in seen:
            rows.append({"port": port, "bind": "", "families": 0, "pid": None,
                         "cmdline": "", "cwd": "", "kind": "nothing listening",
                         "guessed": False, "project": "", "gone": False,
                         "user": "", "up": None, "exposed": how,
                         "orphan": True})
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


def tunnel_dir():
    """Where a launched quick tunnel's pid and URL are remembered.

    cloudflared holds no listening socket - it dials out - so nothing in
    /proc ties it to the port it serves. Without a note on disk the widget
    would lose a tunnel the moment it restarted, and leave it running with
    no way to find or stop it.
    """
    base = (os.environ.get("XDG_STATE_HOME")
            or os.path.expanduser("~/.local/state"))
    path = os.path.join(base, "terminal-toys", "tunnels")
    try:
        os.makedirs(path, exist_ok=True)
    except OSError:
        return ""
    return path


def tunnel_state(port):
    """The quick tunnel for a port: its pid, its URL, whether it still runs."""
    path = tunnel_dir()
    if not path:
        return None
    try:
        with open(os.path.join(path, "%d.json" % port)) as f:
            note = json.load(f)
    except (OSError, ValueError):
        return None
    if not alive(note.get("pid") or 0):
        forget_tunnel(port)
        return None
    return note


def forget_tunnel(port):
    try:
        os.remove(os.path.join(tunnel_dir(), "%d.json" % port))
    except OSError:
        pass


def start_tunnel(port, wait=25.0):
    """Run a cloudflared quick tunnel for a port and return its URL.

    A quick tunnel needs no account and no DNS: cloudflared picks a random
    trycloudflare.com name and prints it. A named tunnel on a domain of your
    own needs credentials and a DNS record, which is a setup task rather
    than a keypress, and is deliberately not attempted here.
    """
    path = tunnel_dir()
    if not path:
        return "", "no state directory to record the tunnel in"
    log = os.path.join(path, "%d.log" % port)
    try:
        handle = open(log, "w+b")
    except OSError as exc:
        return "", exc.strerror or "cannot write the log"
    try:
        proc = subprocess.Popen(
            ["cloudflared", "tunnel", "--no-autoupdate",
             "--url", "http://127.0.0.1:%d" % port],
            stdout=handle, stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL, start_new_session=True)
    except OSError as exc:
        handle.close()
        return "", exc.strerror or "cloudflared would not start"
    deadline = time.time() + wait
    found = ""
    while time.time() < deadline and not found:
        time.sleep(0.4)
        try:
            with open(log, "rb") as f:
                text = f.read().decode("utf8", "replace")
        except OSError:
            text = ""
        match = re.search(r"https://[a-z0-9-]+\.trycloudflare\.com", text)
        if match:
            found = match.group(0)
        elif not alive(proc.pid):
            break
    handle.close()
    if not found:
        end(proc.pid, signal.SIGTERM)
        return "", "no URL after %ds - see %s" % (int(wait), log)
    try:
        with open(os.path.join(path, "%d.json" % port), "w") as f:
            json.dump({"pid": proc.pid, "url": found, "port": port}, f)
    except OSError:
        pass
    return found, ""


def alive(pid):
    """Whether a pid is still running. Signal 0 checks without delivering.

    A zombie answers signal 0 and is not running: it is an exit status its
    parent has not collected yet. Reporting one as alive would leave the
    widget offering to SIGKILL something already dead, forever, since no
    signal moves a zombie. /proc knows the difference.
    """
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        pass
    try:
        with open("/proc/%d/stat" % pid) as f:
            return f.read().rsplit(")", 1)[1].split()[0] != "Z"
    except (OSError, IndexError):
        return True


def killable(row):
    """Whether this row can be signalled, and why not when it cannot.

    Returns (pid, reason). Only one of the two is ever set. The checks are
    all about not doing damage past what was asked for: a row whose owner
    /proc would not name is somebody else's process, and a process in this
    widget's own group cannot be group-killed without taking the widget
    down with it.
    """
    pid = row.get("pid")
    if not pid:
        return None, "not yours - no owner for this socket in /proc"
    if pid <= 1:
        return None, "refusing to signal pid %d" % pid
    try:
        if os.stat("/proc/%d" % pid).st_uid != os.getuid():
            return None, "pid %d is not yours" % pid
    except OSError:
        return None, "pid %d is already gone" % pid
    return pid, ""


def end(pid, sig):
    """Signal the process group, falling back to the process alone.

    A dev server is rarely one process: `npm run dev` is a shell, a package
    manager and the server itself, sharing a process group precisely so that
    Ctrl-C reaches all three. Signalling the group is what Ctrl-C does. The
    fallback covers a process whose group we cannot read, and the guard
    covers the case where the group is this widget's own.
    """
    try:
        group = os.getpgid(pid)
    except OSError:
        group = None
    try:
        if group is not None and group != os.getpgrp():
            os.killpg(group, sig)
        else:
            os.kill(pid, sig)
    except ProcessLookupError:
        return "already gone"
    except PermissionError:
        return "not permitted"
    except OSError as exc:
        return exc.strerror or "failed"
    return ""


def have(program):
    """Whether a command exists, so a key can say so instead of failing."""
    return bool(shutil.which(program))


def serve_port(port, public=False):
    """Put a local port behind this node's HTTPS name.

    Listening on the port's own number rather than 443, which is already
    taken by whatever was served first and would otherwise collide. Funnel
    has no such freedom - Tailscale allows it on 443, 8443 and 10000 only -
    so a funnel goes to 443 and the CLI's own complaint is passed on when
    that is occupied.
    """
    cmd = ["tailscale", "funnel" if public else "serve", "--bg"]
    cmd += ["--https=%d" % (443 if public else port), str(port)]
    try:
        done = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError) as exc:
        return str(exc) or "tailscale would not run"
    if done.returncode:
        text = (done.stderr or done.stdout or "").strip()
        return " ".join(text.split())[:200] or "tailscale refused"
    return ""


def unserve_port(port, public=False):
    """Take back one port, leaving every other mount as it was.

    Never `serve reset`: that clears the whole configuration, including
    whatever was already published before this widget was ever run.
    """
    cmd = ["tailscale", "funnel" if public else "serve",
           "--https=%d" % (443 if public else port), "off"]
    try:
        done = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError) as exc:
        return str(exc) or "tailscale would not run"
    if done.returncode:
        text = (done.stderr or done.stdout or "").strip()
        return " ".join(text.split())[:200] or "tailscale refused"
    return ""


def kill_label(row, room=999):
    """What a kill would take down, in whatever room the pane has.

    Everything here identifies the target, but not equally: the port is the
    one thing the person is looking at, and the framework name without the
    project it belongs to is no use on a machine running four of them. The
    pid is the first to go, then the kind.
    """
    what = row["kind"] or "unidentified"
    where = row["project"]
    full = "%s%s on :%d (pid %d)" % (what, " in " + where if where else "",
                                     row["port"], row["pid"])
    for text in (full,
                 "%s%s on :%d" % (what, " in " + where if where else "",
                                  row["port"]),
                 "%s on :%d" % (where or what, row["port"]),
                 ":%d" % row["port"]):
        if len(text) <= room:
            return text
    return ":%d" % row["port"]


def prompt(ask, key, options, w):
    """A question and the key that answers it, fitted to the pane.

    On one line where both fit, on two where they do not, because the key is
    never the half that may be truncated: a prompt with its `[y]` pushed off
    the right edge is a prompt nobody can act on. The wording after the key
    has shorter forms for the same reason, longest first.
    """
    def answer(room):
        text = next((o for o in options if len(key[1]) + len(o) <= room), "")
        return [key, (DIM, text)]

    # The bare last form - "[y] yes", with no word on what anything else
    # does - is a fallback for a pane too narrow for two lines, not a thing
    # to choose while a second line is going spare.
    keep = options[-2] if len(options) > 1 else options[-1]
    room = (w - 1) - sum(len(t) for _, t in ask) - 2
    if room >= len(key[1]) + len(keep):
        return [seg(ask + [(DIM, "  ")] + answer(room), w - 1)]
    return [seg(ask, w - 1), seg([(DIM, " ")] + answer(w - 2), w - 1)]


def bind_note(row):
    """What the bound address means for who can reach it.

    The address itself is rarely the answer to that question and is always
    too wide for the column - a tailnet IPv6 address is 24 characters and
    pushed every field after it off the line. What matters is the class:
    every interface, loopback only, or bound to one particular network.
    """
    if row.get("orphan"):
        return "--", DIM
    reach = bind_class(row["bind"])
    if reach == "all":
        return "all", OPEN
    if reach == "local":
        return "local", LOCAL
    if reach == "tailnet":
        return "tailnet", ACCENT
    return reach[:7], TXT


ACTIONS = {
    "serve": ("publish", "tailnet only"),
    "funnel": ("publish publicly", "anyone with the URL"),
    "unserve": ("stop serving", ""),
    "unfunnel": ("stop the funnel", ""),
    "tunnel": ("open a cloudflare tunnel", "anyone with the URL"),
    "untunnel": ("close the cloudflare tunnel", ""),
}


def do_work(kind, row, done):
    """Carry out one exposure change and record how it went."""
    port = row["port"]
    if kind in ("serve", "funnel"):
        failed = serve_port(port, kind == "funnel")
        done.append(((failed, BAD, time.time() + 8) if failed
                     else ("%s now serves :%d" % (kind, port), OK,
                           time.time() + 6),))
    elif kind in ("unserve", "unfunnel"):
        failed = unserve_port(port, kind == "unfunnel")
        done.append(((failed, BAD, time.time() + 8) if failed
                     else ("stopped serving :%d" % port, OK,
                           time.time() + 5),))
    elif kind == "tunnel":
        url, failed = start_tunnel(port)
        done.append(((failed, BAD, time.time() + 10) if failed
                     else (url, OK, time.time() + 20),))
    elif kind == "untunnel":
        note = tunnel_state(port)
        if note:
            end(note["pid"], signal.SIGTERM)
            forget_tunnel(port)
        done.append((("closed the tunnel on :%d" % port, OK,
                      time.time() + 5),))


def start_work(kind, row):
    """Run an exposure change on a thread, so the frame keeps drawing."""
    work = {"kind": kind, "row": row, "done": []}
    threading.Thread(target=do_work, args=(kind, row, work["done"]),
                     daemon=True).start()
    return work


def footer(confirm, watch, working, notice, w, hints):
    """The bottom of either screen.

    One of five things, in the order they matter: a question that must be
    answered before anything happens, the wait after answering it, a slow
    action still running, the outcome of the last one, or the ordinary keys.
    """
    if confirm is not None:
        verb, cost = ACTIONS.get(confirm["kind"], ("kill", ""))
        ask = [(BAD, " " + verb + " "),
               (TXT, kill_label(confirm["row"], w - 34 - len(verb)))]
        if cost:
            ask.append((WARN, " - " + cost))
        return prompt(ask + [(DIM, "?")], (WARN, "[y]"),
                      [" yes  ·  any other key cancels",
                       " yes  ·  any key cancels", " yes"], w)
    if watch is not None and watch["asked"]:
        return prompt([(WARN, " still up: "),
                       (TXT, kill_label(watch["row"], w - 30))], (BAD, "[f]"),
                      [" force kill  ·  any other key leaves it",
                       " SIGKILL  ·  any key leaves it", " SIGKILL"], w)
    if watch is not None:
        return [seg([(DIM, " SIGTERM sent, waiting for "),
                     (TXT, kill_label(watch["row"], w - 29))], w - 1)]
    if working is not None:
        verb = ACTIONS.get(working["kind"], ("working", ""))[0]
        return [seg([(WARN, " %s :%d - this can take a moment"
                      % (verb, working["row"]["port"]))], w - 1)]
    if notice is not None:
        return [seg([(notice[1], " " + notice[0])], w - 1)]
    return [" " + line for line in pack_hints(hints, w - 2)]


def has_detail(row):
    """Whether there is anything behind this row worth a second screen.

    A process of ours carries a command line, a directory and an age that
    the table has no room for, and any row at all can be given an address to
    copy or an exposure to set up. Another user's socket carries none of
    that: the four columns already say everything /proc will tell us, and
    opening a screen to repeat them would be a screen that wastes a press.
    """
    return bool(row.get("pid") or row.get("orphan") or row.get("exposed"))


def wrap(text, width):
    """Break a long value across lines at spaces, then anywhere."""
    lines, rest = [], text
    while rest and len(lines) < 4:
        if len(rest) <= width:
            lines.append(rest)
            break
        cut = rest.rfind(" ", 0, width + 1)
        cut = cut if cut > width // 2 else width
        lines.append(rest[:cut])
        rest = rest[cut:].lstrip()
    return lines or [""]


def field(label, value, w, colour=TXT, label_w=10):
    """One `label   value` line, wrapped under its own label."""
    rows = []
    for i, line in enumerate(wrap(value, max(8, (w - 3) - label_w))):
        rows.append(seg([(DIM, "  " + pad(label if not i else "", label_w)),
                         (colour, line)], w - 1))
    return rows


def expose_options(row, net, tunnel):
    """The ways this port could be published, and why one is unavailable.

    Each is a key, a name, and the state that decides whether pressing it
    does anything. An option that cannot work says so on the line rather
    than failing after the keypress - except Funnel, which is offered even
    when the capability is missing, because Tailscale's own error names the
    setting to change in the admin console better than this can.
    """
    how = row.get("exposed")
    # One blocker outranks the others: without the operator bit every serve
    # and funnel write is refused, whatever else is true of them.
    barred = "" if net.get("operator") else "needs: tailscale set --operator"
    return [
        ("s", "tailscale serve",
         "serving · tailnet only" if how == "tailnet"
         else barred or "tailnet only", how == "tailnet"),
        ("t", "tailscale funnel",
         "public · anyone with the URL" if how == "public"
         else barred or ("public" if net["funnel"]
                         else "not enabled for this node"),
         how == "public"),
        ("d", "cloudflare tunnel",
         "running · %s" % tunnel["url"] if tunnel
         else "quick tunnel, random domain" if have("cloudflared")
         else "cloudflared not installed",
         bool(tunnel)),
    ]


def detail_rows(row, net, cfg, tunnel, links, sel, w):
    """The second screen: everything known about one port, and what to do."""
    rows = [title(":%d" % row["port"], w, PORT)]
    head = row["kind"] or ("%s's" % row["user"] if row.get("user") else "")
    if row["project"]:
        head += " in " + row["project"]
    rows.append(seg([(TXT, " " + head.strip()),
                     (DIM, "  ·  up " + span(row["up"]) if row["up"]
                      else "")], w - 1))
    rows.append("")

    if row.get("pid"):
        rows.append(seg([(LBL, " ── PROCESS ── ")], w - 1))
        rows += field("command", row["cmdline"] or "?", w)
        rows += field("directory", row["cwd"] or "?", w,
                      WARN if row["gone"] else TXT)
        group = ""
        try:
            group = " · group %d" % os.getpgid(row["pid"])
        except OSError:
            pass
        rows += field("pid", "%d%s" % (row["pid"], group), w)
        rows.append("")

    # A lone :: is not an IPv6-only server: Linux maps IPv4 onto it unless
    # the process asked for IPV6_V6ONLY, and /proc cannot say which it did.
    # Claiming "IPv6 only" here would be a guess dressed as a fact.
    note = ("two sockets, IPv4 and IPv6" if row.get("families", 0) > 1
            else "IPv4 too, unless the server turned that off"
            if row["bind"] == "::" else "one socket")
    rows.append(seg([(LBL, " ── LISTENING ON ── "),
                     (TXT, row["bind"] or "nothing"),
                     (DIM, "  " + note)], w - 1))
    rows.append("")
    rows.append(seg([(LBL, " ── REACHABLE AT ── "),
                     (DIM, "↑↓ to pick, c copies")], w - 1))
    if not links:
        rows.append(seg([(DIM, "   nothing is listening to reach")], w - 1))
    for i, (url, note) in enumerate(links):
        here = i == sel
        rows.append(seg([(ACCENT if here else DIM, " ▸ " if here else "   "),
                         (TXT if here else DIM, url), (DIM, "  " + note)],
                        w - 1))
    rows.append("")
    rows.append(seg([(LBL, " ── EXPOSE ── ")], w - 1))
    for key, name, state, on in expose_options(row, net, tunnel):
        rows.append(seg([(ACCENT, "  [%s] " % key), (TXT, pad(name, 18)),
                         (OK if on else DIM, state)], w - 1))
    return rows


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
    confirm = None    # an action awaiting an explicit yes before it happens
    watch = None      # what SIGTERM was sent to, and whether it has died
    notice = None     # (text, colour, expires) - the result of the last one
    detail = None     # the port whose second screen is open, and its state
    working = None    # an action too slow to block the frame on
    net = cfg = None  # tailnet identity and serve config, read on demand

    while True:
        w, h = size()
        all_rows, fetched, err = store.snapshot()
        shown = [r for r in all_rows if not (hide_system and theirs(r))]
        selected = max(0, min(selected, len(shown) - 1)) if shown else 0

        # A process that dies on its own between the prompt and the deadline
        # is the normal case, and wants no further questions.
        if watch is not None and not watch["asked"]:
            if not alive(watch["pid"]):
                notice = ("stopped " + kill_label(watch["row"], w - 11),
                          OK, time.time() + 5)
                watch = None
                store.wake.set()
            elif time.time() >= watch["deadline"]:
                watch["asked"] = True

        # An action that talks to tailscaled or cloudflared takes seconds,
        # which is far too long to hold a frame for, so it runs on a thread
        # and its answer is collected here.
        if working is not None and working["done"]:
            notice = working["done"][0]
            working = None
            net = cfg = None
            store.wake.set()

        for key in keyboard.poll():
            if confirm is not None:
                # Only an explicit yes acts. Every other key cancels,
                # deliberately including q: quitting must never double as
                # consent to signal something or publish it.
                act, confirm = confirm, None
                if key not in ("y", "Y"):
                    notice = ("cancelled", DIM, time.time() + 2)
                    continue
                row, kind = act["row"], act["kind"]
                if kind == "kill":
                    pid, why = killable(row)
                    if why:
                        notice = (why, BAD, time.time() + 5)
                        continue
                    failed = end(pid, signal.SIGTERM)
                    if failed:
                        notice = ("%s: %s"
                                  % (kill_label(row, w - 4 - len(failed)),
                                     failed), BAD, time.time() + 5)
                        store.wake.set()
                    else:
                        watch = {"pid": pid, "row": row, "asked": False,
                                 "deadline": time.time() + 3.0}
                else:
                    working = start_work(kind, row)
                continue
            if watch is not None and watch["asked"]:
                pid, row, watch = watch["pid"], watch["row"], None
                if key in ("f", "F"):
                    failed = end(pid, signal.SIGKILL)
                    notice = ("%s: %s" % (kill_label(row, w - 4 - len(failed)),
                                          failed) if failed
                              else "SIGKILL sent to "
                                   + kill_label(row, w - 19),
                              BAD if failed else WARN, time.time() + 5)
                else:
                    notice = ("left running: " + kill_label(row, w - 17),
                              DIM, time.time() + 3)
                store.wake.set()
                continue
            if detail is not None:
                # The second screen keeps its own selection - of addresses
                # rather than rows - and hands every other key back.
                if key in ("esc", "left", "q", "Q", "backspace"):
                    detail = None
                    continue
                if key == "up":
                    detail["at"] -= 1
                elif key == "down":
                    detail["at"] += 1
                elif key in ("c", "C"):
                    links = detail["links"]
                    if links:
                        url = links[max(0, min(detail["at"],
                                               len(links) - 1))][0]
                        # The address goes in the notice either way: OSC 52
                        # is refused by some terminals and swallowed by some
                        # multiplexers, and a copy that silently did nothing
                        # would leave nothing on screen to read instead.
                        ok = clipboard(url)
                        notice = (("copied  " if ok else "no clipboard  ")
                                  + url, OK if ok else WARN, time.time() + 8)
                elif key in ("s", "S", "t", "T", "d", "D"):
                    kind = {"s": "serve", "t": "funnel",
                            "d": "tunnel"}[key.lower()]
                    how = detail["row"].get("exposed")
                    if kind == "serve" and how == "tailnet":
                        kind = "unserve"
                    elif kind == "funnel" and how == "public":
                        kind = "unfunnel"
                    elif kind == "tunnel" and detail["tunnel"]:
                        kind = "untunnel"
                    elif kind == "tunnel" and not have("cloudflared"):
                        notice = ("cloudflared is not installed - see "
                                  "the docs for the one-line install",
                                  WARN, time.time() + 8)
                        continue
                    if working is None:
                        confirm = {"kind": kind, "row": detail["row"]}
                elif key == "r":
                    net = cfg = None
                    store.wake.set()
                continue
            if key in ("q", "Q"):
                raise SystemExit(0)
            if key == "up":
                selected -= 1
            elif key == "down":
                selected += 1
            elif key == "o":
                hide_system = not hide_system
            elif key == "r":
                store.wake.set()
            elif key in ("enter", "right", "i") and shown:
                row = shown[max(0, min(selected, len(shown) - 1))]
                if has_detail(row):
                    detail = {"port": row["port"], "row": row, "at": 0,
                              "links": [], "tunnel": None}
                    net = cfg = None
                else:
                    notice = ("nothing more to show - /proc will not name "
                              "another user's process", DIM, time.time() + 5)
            elif key in ("k", "K") and shown and watch is None:
                row = shown[max(0, min(selected, len(shown) - 1))]
                pid, why = killable(row)
                if why:
                    notice = (why, BAD, time.time() + 5)
                else:
                    confirm = {"kind": "kill", "row": row}

        # Rebuilt after the keys rather than before them, so that a press of
        # o is answered in the frame it was made in and not the next one.
        shown = [r for r in all_rows if not (hide_system and theirs(r))]
        selected = max(0, min(selected, len(shown) - 1)) if shown else 0
        if notice and time.time() >= notice[2]:
            notice = None

        if detail is not None:
            # Tailscale is asked once per visit rather than once per frame:
            # two subprocesses at 3Hz would cost more than the whole rest of
            # the widget. Any change made here clears them.
            if net is None:
                net, cfg = tailnet_self(), serve_config()
            live = next((r for r in all_rows if r["port"] == detail["port"]),
                        None)
            detail["row"] = live or dict(detail["row"], pid=None, gone=True)
            detail["tunnel"] = tunnel_state(detail["port"])
            detail["links"] = addresses(detail["row"], net, cfg)
            if detail["tunnel"]:
                detail["links"] = detail["links"] + [
                    (detail["tunnel"]["url"], "public · cloudflare")]
            detail["at"] = max(0, min(detail["at"],
                                      len(detail["links"]) - 1))
            rows = detail_rows(detail["row"], net, cfg, detail["tunnel"],
                               detail["links"], detail["at"], w)
            foot = footer(confirm, watch, working, notice, w,
                          [[(ACCENT, "↑↓"), (DIM, " address")],
                           [(DIM, "[c]opy")], [(DIM, "[s]erve")],
                           [(DIM, "[t]unnel")], [(DIM, "[d] cloudflare")],
                           [(ACCENT, "esc"), (DIM, " back")]])
            while len(rows) < h - len(foot) - 1:
                rows.append("")
            draw(rows[:h - len(foot) - 1] + foot, w, h)
            time.sleep(0.3)
            continue

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
            # Another user's row names its owner rather than its project,
            # which it has none of that we can read. That is the whole of
            # what is knowable about it, and it is more use than the
            # "(not ours)" this used to say in the column beside it.
            who = row["project"] or row.get("user") or ("—" if row["pid"]
                                                        else "")
            line = [(tint + (ACCENT if here else PORT),
                     ("▸" if here else " ") + "%-6d" % row["port"]),
                    (tint + note_colour, "%-8s" % note),
                    (tint + (DIM if row["guessed"] or not row["kind"] else TXT),
                     pad(row["kind"], 18)),
                    (tint + (WARN if row["gone"] else
                             DIM if row.get("user") else TXT),
                     pad(who, name_w))]
            if wide:
                line.append((tint + DIM, "%-6s" % span(row["up"])))
                line.append((tint + (OK if row["exposed"] == "tailnet"
                                     else BAD if row["exposed"] == "public"
                                     else GRID),
                             row["exposed"] or "-"))
            if here:
                line.append((tint, " " * w))
            rows.append(seg(line, w - 1))

        foot = footer(confirm, watch, working, notice, w,
                      [[(ACCENT, "↑↓"), (DIM, " select")],
                       [(ACCENT, "↵"), (DIM, " details")], [(DIM, "[k]ill")],
                       [(DIM, "[o]%s system"
                         % ("show" if hide_system else "hide"))],
                       [(DIM, "[r]efresh")], [(DIM, "[q]uit")]])

        while len(rows) < h - len(foot) - 1:
            rows.append("")
        rows.extend(foot)
        draw(rows, w, h)
        time.sleep(0.3)


main()
