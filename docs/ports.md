# `ports.py`

What is listening on this machine, what started it, and who can reach it.

```
╺━ DEV SERVERS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 16 listening · 8 yours · 1 reachable off-box   every 4s

  PORT  BIND    WHAT              PROJECT                  UP    EXPOSED
  3000  all     Next.js 16.3.1    fix-dependabot-alerts ✗  22h   -
  3001  all     Next.js 16.2.11   piaf-web                 8h    -
  3002  all     Next.js 16.3.0    my-project               1h    -
  4100  --      nothing listening                          --    tailnet
 25001  local   Next.js           my-project               1h    -
 42043  local   agy               wiiiimm-codes            9h    -

 ↑↓ select  [k]ill  [o]show system  [r]efresh  [q]uit
```

Seven of this machine's sixteen listening ports are hidden here — five of
root's, plus `22` and `53` from `system_ports` — and `o` shows them. The count
in the header is of all sixteen, so the seven it does not draw are never a
surprise.

## What identifies a dev server

Not the pid, and not the port. The **project directory** it was started from,
which is what a person actually calls it — so that column is the one given
whatever width the pane has left, and the fixed ones make way for it.

The name comes from `package.json` in the process's `cwd` where there is one,
and from the directory's own name otherwise. `piaf-web` above is the former;
`my-project` is a `package.json` that was never renamed, which is exactly as
true and rather more revealing.

## What kind of thing it is

Read from the process itself, in two steps, because a dev server launched
through a package manager is several layers of wrapper deep:

- **the process title**, where there is one. Next.js rewrites its own to
  `next-server (v16.3.0)`, which hands over the framework *and* the version;
- **the argv path**, when the title is just `node`. A path containing
  `node_modules/next/dist` is a Next.js server whatever the binary is called.

Vite, Nuxt, Astro, Remix, webpack, Django, Rails, uvicorn, Postgres, Redis and
a dozen others are recognised the same way. Anything unmatched shows the
basename of its command rather than a guess.

## The two columns that answer "who can reach this"

**BIND** is the class of the bound address, not the address: `all` for
`0.0.0.0`, `local` for loopback, `tailnet` for an address in Tailscale's
`100.64.0.0/10` or `fd7a:115c:a1e0::/48`. The literal address is both too wide
for a column — a tailnet IPv6 address is 24 characters — and rarely the answer
to the question being asked.

**EXPOSED** is what Tailscale is serving, from `tailscale serve status`:
`tailnet` where it is reachable by your own devices, `public` where a funnel
puts it on the open internet. Funnel is the one worth colouring red; it is
also limited to three ports (443, 8443, 10000), so most machines will never
show it.

Cloudflare Tunnel is **not** read. It would answer the same question for
arbitrary ports without a tailnet, and is a line of parsing away if it is ever
adopted here — but a column that silently covers one mechanism and not another
is worse than one that says which it knows.

## The two rows that only this widget can show you

**A server on a directory that no longer exists.** `fix-dependabot-alerts ✗`
above is a dev server still running, still holding port 3000, on a worktree
that has been deleted. `/proc` marks the cwd `(deleted)`, and the name is kept
rather than blanked, because a stale server holding a port you want is
precisely the thing worth finding.

**A port that is served but not listening.** Port 4100 has a Tailscale URL
pointing at it and nothing behind it — the URL exists, answers 502, and
nothing in `lsof` explains why. It gets its own row rather than being left out
for lacking a socket.

## What it cannot see

Sockets owned by another user, which on a normal machine means everything root
runs — `sshd`, the resolver, Tailscale, whatever your cloud provider installed
to collect metrics. Tying a socket to a process means reading
`/proc/<pid>/fd`, and that directory is unreadable for anyone else's process,
so the port is visible and the thing behind it is not.

What *is* readable is the uid, which the kernel puts in the socket table
beside the inode. So those rows name their owner — `root` — in the column
where yours name their project, and leave WHAT blank rather than guessing.
Where the port number is a well-known convention the service is named **with a
question mark**: `SSH?`, `HTTPS?`. The question mark is the point. It is a
guess from a number, not something read from the process, and the widget says
which it is.

`o` hides them, and does so by default, along with the system ports in
`system_ports`. Both are the machine rather than something you started, both
are unkillable without root, and neither is ever the answer to "which port is
my dev server on".

They are hidden rather than dropped because two questions this widget exists
for still involve them. A root process holding port 3000 is why *your* server
could not bind it, and a public Tailscale funnel lands on 443, which is root's.
The header keeps counting them for the same reason: `16 listening · 8 yours`
on a screen showing eight rows says plainly that eight more exist.

## One service, not one socket

A server that listens on both address families holds two sockets and appears
twice in the kernel's table — `0.0.0.0:3000` and `[::]:3000`, or a tailnet
IPv4 address alongside its IPv6. That is one thing to know about, so it is one
row.

Rows are merged only when the port, the owning pid **and** the reachability
class all match. Two sockets on the same port that differ on any of the three
stay two rows, because `127.0.0.1:8080` and `100.64.1.2:8080` are not the same
answer to "who can reach this" even when one process holds both.

## Stopping a server

`k` stops the selected one. It asks first, on the bottom line, naming what is
about to go:

```
 kill Next.js in my-project on :25001 (pid 84120)?  [y] yes  ·  any other key cancels
```

Only `y` proceeds. Every other key cancels, `q` included — quitting is not
consent.

What it sends is **SIGTERM to the process group**, which is what Ctrl-C in that
server's own terminal would have sent. A dev server is rarely one process:
`npm run dev` is a package manager, a supervisor and the server itself, sharing
a group for exactly this reason. Signalling the pid alone leaves the supervisor
to respawn it. Where the group cannot be read, or where it is this widget's
own, the pid alone is signalled instead.

Then it waits. Most servers are gone within a second and the row disappears. If
one is still up after three seconds, the offer is `[f]` for SIGKILL, which it
does not get automatically — a server ignoring SIGTERM is usually mid-write,
and that is worth a deliberate second keystroke.

It refuses outright on anything that is not yours: a row with no pid is a
socket `/proc` would not name, which means another user's process, and pid 1 is
never a dev server. Neither case is offered a prompt.

## The second screen

`↵` opens the selected port, and only when there is something behind it — a
process of yours, or a port Tailscale is already serving. Another user's
socket does not get a screen, because the four columns already carry
everything `/proc` will say about it, and a press that opens a repeat of the
row is a press wasted.

```
╺━ :3001 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 Next.js in piaf-web  ·  up 8h

 ── PROCESS ──
  command   next-server (v16.2.11)
  directory ~/projects/piaf-web
  pid       41220 · group 41198

 ── LISTENING ON ── 0.0.0.0, IPv4 and IPv6

 ── REACHABLE AT ── ↑↓ to pick, c copies
 ▸ http://127.0.0.1:3001                this machine
   http://10.0.0.4:3001                 ens4
   http://100.x.x.x:3001                tailnet
   http://this-machine.tail____.ts.net:3001   tailnet · name

 ── EXPOSE ──
  [s] tailscale serve     tailnet only
  [t] tailscale funnel    not enabled for this node
  [d] cloudflare tunnel   quick tunnel, random domain
```

### The addresses are bounded by the bind

This is the part that is easy to get wrong and worth being exact about. A
server bound to `127.0.0.1` is **not** reachable at this machine's LAN or
tailnet address, however many addresses the machine has, so those are not
offered — a URL that cannot work is worse than no URL. Only a socket bound to
every interface gets the full list; one bound to a single address offers that
address; a `local` one offers loopback and nothing else.

The exception is a port Tailscale already serves. Tailscale proxies to it over
loopback, so its `https://` name works even for a loopback-only server, and it
is listed first.

`c` copies the highlighted one via OSC 52, which asks *your terminal* to do
the copying — so it reaches the laptop you are sitting at, not the server. Some
terminals refuse and some multiplexers swallow it, so the address is printed in
the confirmation either way and can be read off the screen if the copy is
silently dropped.

### Publishing a port

Three ways, each behind a confirmation that names what it is about to do,
because two of them put a local server on the public internet.

Both Tailscale actions write to the serve configuration, which is a root
operation unless this user has been made the operator. The widget checks and
says `needs: tailscale set --operator` on the line rather than letting you
find out by pressing it. The one-off fix is:

```
sudo tailscale set --operator=$USER
```

- **`s` — `tailscale serve`.** Tailnet only: your own devices, nobody else. It
  mounts on the port's own number rather than 443, since 443 belongs to
  whatever was served first and a second mount there would collide. Press it
  again to take that one mount back down — never `serve reset`, which would
  clear configuration this widget did not create.
- **`t` — `tailscale funnel`.** Public, on 443, to anyone with the URL. Funnel
  only works if the tailnet's policy grants this node the attribute, and the
  node knows whether it has it, so the line says `not enabled for this node`
  rather than offering a key that only ever errors. Press it anyway and
  Tailscale's own message is shown — it names the admin-console setting better
  than this can.
- **`d` — a Cloudflare quick tunnel.** Needs `cloudflared`, which the line says
  plainly when it is missing. On Debian or Ubuntu:

  ```
  curl -fsSL -o /tmp/cloudflared.deb https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64.deb
  sudo dpkg -i /tmp/cloudflared.deb
  ```

  A random `*.trycloudflare.com` name, no
  account and no DNS needed. A *named* tunnel on a domain of your own needs
  credentials and a DNS record, which is a setup task rather than a keypress,
  and is not attempted. `cloudflared` holds no listening socket — it dials out
  — so nothing in `/proc` ties it to the port it serves; its pid and URL are
  written under `$XDG_STATE_HOME/terminal-toys/tunnels` so a tunnel survives
  the widget restarting and can still be found and closed.

All three run on a thread. `tailscaled` and `cloudflared` take seconds to
answer, which is far too long to hold a frame for.

### What about a public IP?

There usually is not one. A cloud VM holds a private address and reaches the
internet through NAT, so no interface here has an address the world can route
to, and the widget will not invent one. On a machine like that the public
address of a port *is* the funnel URL or the tunnel URL — which is why both
are on this screen rather than an IP.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` | select a row — or an address, on the second screen |
| `↵` | open the selected port, where there is more to show |
| `esc` | back to the list |
| `c` | copy the highlighted address |
| `s` | `tailscale serve` this port, or stop serving it |
| `t` | `tailscale funnel` this port publicly, or stop |
| `d` | open or close a Cloudflare quick tunnel |
| `k` | stop the selected server, after confirming |
| `f` | escalate to SIGKILL, only when offered |
| `o` | show or hide what is the machine's rather than yours |
| `r` | rescan now |
| `q` | quit |

## Cost

Nothing measurable. `/proc/net/tcp` and a walk of `/proc/*/fd` every four
seconds, plus one `tailscale serve status` — no network, no root, no
dependency beyond Tailscale for the exposure column, which is simply blank
without it.

## Configuration

```json
"ports": {
  "system_ports": [22, 53, 123, 323, 631, 5353],
  "refresh": 4
}
```

`system_ports` is what `o` hides. Add anything that is part of the machine
rather than something you started.
