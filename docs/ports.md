# `ports`

[← all docs](README.md)

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

## Traffic

The **TRAFFIC** column, and the chart on a port's own screen, come from the
kernel's own per-socket byte counters — `bytes_sent` and `bytes_received` out
of `ss -tine`, the same two fields netwatch reads. Nothing here is derived
from a guess.

A *listening* socket carries no bytes. The traffic is on the connections
accepted from it, so a port's figure is the sum over every established socket
whose **local** port is that one. Only ports something is actually listening
on are tallied: most established sockets are outbound, and their local port is
an ephemeral number that belongs to nothing.

```
 ── TRAFFIC ── ↑ out above · ↓ in below  · 44s of history, sampled every 1s
                                             █                 ▁
                                             █▆ ▃     ▆▄▁▂    ▁█▁▁    ▁▂▇▄▁
↑ 4.7 MB/s ··································██▅█▂ ▄▂▇████▅ ▃▂████▄▂▂▄█████▇
           ─────────────────────────────────────────────────────────────────
           ··································
```

The chart is as wide as the pane. One column is one sample, newest at the
right, and each direction is scaled to its own peak and says what that peak
was in a gutter down the left. A shared scale would flatten the quieter of the
two into nothing, and nothing is what a source with no traffic looks like —
the one reading this widget must never produce by accident.

The dots are where there is no history yet. Left blank they would be
indistinguishable from a stretch of real zeroes, and a quiet port and an
unmeasured one are not the same thing. They fill in from the right as the
samples arrive, and once the history is longer than the pane is wide the
chart shows the most recent of it — which is what the heading's `44s of
history` counts, not everything kept.

Three things it is honest about:

**It counts what moved between two samples,** so a socket has to be seen twice
to say anything. A connection that opens and closes inside one interval is
never counted, and a long-lived one loses its last few bytes when it closes.
At the default four-second poll that is a real gap; `-n 1` narrows it.

**Rates divide by the gap that actually happened,** not by the interval that
was asked for. `[r]` polls early, and a rate measured against the nominal
interval would read high every time somebody pressed it.

**It is TCP.** A port serving anything else reads as quiet, because these
counters exist only for TCP sockets. Ports whose process belongs to another
user are counted the same as any other — the byte counters need no privilege,
even where naming the process does.

Unlike netwatch, nothing is filtered by peer. netwatch drops loopback because
it is about what leaves the machine; here loopback is the whole point, since a
browser hitting a dev server on `127.0.0.1` is the traffic being asked about.

The same chart is drawn once across the top of the main screen — everything
moving through every listening port — when there are rows to spare after the
table. The table is what this widget is for, so the chart yields to it.

Beside the rates, each row carries **the shape** of its own traffic:

```
  PORT  BIND    WHAT       PROJECT      TRAFFIC        LAST 17s        UP    EXPOSED
 38611 local   node       a-project                  ·····─────────  10s   -
 39311 all     Python     serve        ↑503K         ▆▃▁ ▂▂▄▇▅█▃▃▁▁  4m    -
```

Each row is scaled to **its own peak**, not to the busiest port on screen. A
shared scale would flatten every quiet port to nothing, and nothing is what a
port with no traffic looks like. So the shape column says *shape* and the
rates beside it say *size*, and the two are read together — a row with a full
bar and `↑2K` is a port at its own busiest, which is not busy.

The three states are kept visibly apart, the same way the chart keeps them:
dots for cells with no sample behind them yet, a flat line for measured and
quiet, bars for traffic. `·····─────────` is a port that appeared ten seconds
ago and has done nothing since.

Both columns arrive when there is room for them *after* the names, rather than
past some width picked in advance: the project column is the one that gives,
and a project's name cut in half is a different project. Both are measured
against a row that already carries UP and EXPOSED whether or not the pane is
yet wide enough to show them, so that crossing that width cannot trade one
fact for another. The shapes are the more decorative of the two and so arrive
last and leave first. A port nothing is calling shows nothing in the rates
column rather than `0 B/s` — a column of zeroes down the table reads as a
measurement that has failed.

## When the scan itself breaks

If the poller stops, the header carries `! poller stopped - see the pane it
was started from` and the table holds whatever it last knew. It used to catch
the failure and return an empty list, so the pane read `0 listening` — a
machine with nothing running on it, which is a thing this widget is supposed
to be able to say truthfully.

The traffic sampler is separate: if it stops, the columns it feeds go quiet
and the line says so, while the table below carries on being found.

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

`↵` or `→` opens the selected port, and only when there is something behind it — a
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

Neither is limited to one port per device, though both default to 443, which
makes it look that way: publish a second port with no flags and it lands on
the mount the first one already owns.

- **`s` — `tailscale serve`.** Tailnet only: your own devices, nobody else.
  Mounted on the port's own number rather than 443, precisely to avoid that
  collision — serve accepts any tailnet-side port, so there is no reason to
  queue everything on one. Press it again to take that one mount back down;
  the mount is looked up rather than assumed. Never `serve reset`, which
  would clear configuration this widget did not create.
- **`t` — `tailscale funnel`.** Public, to anyone with the URL. Tailscale
  accepts funnel traffic on **443, 8443 and 10000** and nowhere else, so a
  node can hold three at once and this takes the first one free — defaulting
  them all to 443 would have allowed exactly one. When all three are taken it
  says so instead of colliding. Funnel
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
  written under `$XDG_STATE_HOME/opscope/tunnels` so a tunnel survives
  the widget restarting and can still be found and closed.

### Why only these two

Tailscale and Cloudflare, and deliberately nothing else.

**Tailscale** is already on this machine carrying the tailnet, so `serve`
publishes a port to your own devices with no third party involved and no
account beyond the one you have. It is the right first answer for "let me
reach this from my laptop", which is most of the question.

**Cloudflare** covers the rest: genuinely public, no account for a quick
tunnel, and nothing to configure.

**ngrok** is the original of the category and the obvious third candidate,
and is left out on purpose. It now requires an account and an authtoken —
anonymous tunnels were withdrawn — so it is no longer the low-friction option
it is remembered as, and its free tier serves browsers an interstitial
warning page that quietly breaks webhook and API testing. Everything it does
here, Cloudflare does without an account. Its genuinely better feature is the
request inspector on `127.0.0.1:4040`, which is a debugging tool rather than
an exposure mechanism, and not what this screen is for.

The same reasoning excludes localtunnel, bore, localhost.run and the rest: a
fourth key that publishes a port a fourth way is not worth the surface. If
you want one, `ssh -R` to a box you already own needs nothing installed at
all.

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
| `↵` `→` | open the selected port, where there is more to show |
| `esc` | back to the list |
| `c` | copy the highlighted address |
| `s` | `tailscale serve` this port, or stop serving it |
| `t` | `tailscale funnel` this port publicly, or stop |
| `d` | open or close a Cloudflare quick tunnel |
| `k` | stop the selected server, after confirming |
| `f` | escalate to SIGKILL, only when offered |
| `o` | show or hide what is the machine's rather than yours |
| `r` | rescan now |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the view a line at a time — the pane moves, the selection stays where it is |
| `q` | quit |

## Cost

Nothing measurable. `/proc/net/tcp` and a walk of `/proc/*/fd` every four
seconds, one `ss -tine` for the byte counters, plus one `tailscale serve
status` — no network, no root, no dependency beyond Tailscale for the exposure
column, which is simply blank without it.

The traffic sampling rides that same poll rather than a thread of its own. A
second thread would buy a finer chart and would also have to be watched: a
poller that dies is invisible, and its pane is indistinguishable from a source
with nothing to say.

## Configuration

```json
"ports": {
  "system_ports": [22, 53, 123, 323, 631, 5353],
  "refresh": 4
}
```

`system_ports` is what `o` hides. Add anything that is part of the machine
rather than something you started.
