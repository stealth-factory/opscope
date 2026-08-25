# `netwatch`

Which processes are using the network, how much they have used, and how fast
they are going right now.

```
╺━ NETWATCH ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 9 processes · 3 moving · 4m 12s · sorted by total   every 1s
 TCP only · ↓ 2.4 MB/s  ↑ 118 KB/s

  PROCESS            PID          TOTAL         NOW        DOWN          UP
  next-server        190856     1.4 GB    2.1 MB/s    2.0 MB/s   112 KB/s
  claude             6953       312 MB     94 KB/s     71 KB/s    23 KB/s
  (unattributed)     -          44 MB     1.2 KB/s    1.0 KB/s      200 B/s
  curl               1234875    8.2 MB           -           -           -

 [1] total  [2] live  [r]ezero  [q]uit
```

## The chart

Above the process list, and the same shape wherever it appears: **sent above
the zero line, received below it**, newest on the right.

That way round because of the labels. `↑` means upload and `↓` means
download, so upload has to be the half that goes up — a `↓ rx` label sitting
over a line that climbs asks you to hold two contradictory directions at
once, and you will believe the arrow. It is the same rule the PR and issue
flow charts follow with `▲`/`▼`.

```
 ── PROCESS WATCH ── ↑ tx above · ↓ rx below  · 2m 30s of history
 ↑ 90.0 KB/s ┌───────────────────────────────────────────────────────┐
           │   ⢀⣀⠤⠤⠒⠒⠊⠉⠉⠉⠉⠉⠉⠉⠒⠒⠒⠤⢄⣀⡀               ⢀⣀⠤⠤⠒⠒⠊⠉⠉⠉⠉⠉⠉⠉⠒⠒⠒⠤⠤⣀⡀  │
           │⠔⠒⠉⠁                   ⠈⠉⠒⠤⢄⡀     ⣀⡠⠔⠒⠉⠁                ⠈⠉⠒⠢⢄⣀│
         0 ├──────────────────────────────────────────────────────────┤
           │  ⡠⠔⠊⠉⠑⠢⡀             ⢀⠤⠊⠉⠉⠒⢄              ⡠⠒⠉⠉⠑⠢⡀        ⢀⠔⠊⠉│
           │⢀⠎      ⠈⠢⡀          ⡔⠁      ⠑⢄          ⡠⠊      ⠈⠢⡀      ⠔⠁ │
  ↓ 2.0 MB/s └───────────────────────────────────────────────────────┘
```

Drawn with **braille**, which is what makes it a line rather than a staircase.
A braille cell addresses eight points — two across, four down — so a character
row holds four vertical positions and a chart eight rows tall resolves
thirty-two. Consecutive samples are joined with a Bresenham segment, so a
steep climb draws as a line instead of a column of dots. Two samples share
each column.

**The two halves are scaled independently**, and each label names its own
direction and peak — `↑ 90.0 KB/s` at the top, `↓ 2.0 MB/s` at the bottom.
Neither is signed. Received traffic is not negative traffic; it is simply the
half drawn downward, and a minus sign on it would dress a fact about the
drawing up as a fact about the number. Sharing a scale is the obvious choice and the wrong one: a
download at two megabytes a second with acknowledgements going back at ninety
kilobytes would draw the upload as a flat line along the axis, and whether
the upload is flat is frequently the question. Read the labels, not the
heights.

An idle stretch draws **nothing** rather than a line pinned to the axis — a
flat line at zero reads as activity that happens to be zero, where blank
reads as what it is.

The chart is **PROCESS WATCH**: the sum of the rows beneath it, and nothing
else. It is deliberately not called "traffic" — the machine's traffic is the
`interfaces` line above, and the two disagreeing is the entire reason for
having both. Whether the rows are yours alone or everyone's is on that line
rather than in the chart's name, so the heading stays put when `o` is
pressed.

The same chart appears in three places: the process list's total above it,
the selected process at the top of its own screen, and the highlighted remote
host as a compact one under the endpoint list, a single row each way.

## Where the numbers come from

macOS has `nettop`, which reports per-process network use directly. Linux has
no such thing — but it does have the kernel's own per-socket accounting, which
gets to the same answer without packet capture, a kernel module, or root.

Two facts combine:

- `ss -tine` reports `bytes_sent` and `bytes_received` for every TCP socket,
  cumulative over that socket's life, along with its **inode**;
- that inode appears as a `socket:[N]` symlink in `/proc/<pid>/fd`.

So the inode is what ties bytes to a process. `ss -p` would name processes
directly, but it needs root to name anyone else's; `/proc/<pid>/fd` needs
nothing at all to name your own, which is the common case.

## What counts as leaving the machine

Only traffic that actually reaches an interface. Two kinds are excluded, and
the second is the one that is easy to get wrong:

- **Loopback** — `127.0.0.0/8` and `::1`. Obvious, and every tool does it.
- **This machine's own addresses.** A connection to `10.240.0.46` when that
  *is* this machine looks external in the socket table and is not: the kernel
  turns it around and sends it back up the stack without a packet ever
  reaching a wire. The same goes for connecting to your own tailnet address.
  These are excluded by matching the peer against every address the machine
  answers to, re-read every thirty seconds so an interface appearing later —
  a tailnet address when `tailscaled` starts, a bridge when a container does
  — is picked up.

Everything else is counted, because everything else genuinely leaves: a peer
on the same subnet, another node on the tailnet, and anything on the internet
are all traffic out of the network card.

If you want **internet only**, that is a narrower question and `--external`
answers it — it additionally drops the local network and the tailnet. On a
host running containers, note that traffic to a container's address leaves
this network namespace but not the physical box; `--external` drops it,
the default does not.

### Payload bytes, not wire bytes

The counters are TCP's own: `bytes_sent` includes retransmissions, but
neither figure includes TCP, IP or Ethernet headers. Real wire usage is a few
per cent higher than what is shown, and more than that for a connection made
of small packets.

## Totals start at zero

The first sample is a baseline, not a reading. Sockets already open when
netwatch starts are recorded and contribute nothing — otherwise an SSH session
open for a week would appear as a gigabyte of "current" use the moment you
looked.

A socket that **opens after** that baseline is different: it started at zero
when the kernel created it, so every byte on it happened while netwatch was
watching, and all of it counts. This matters more than it sounds. A `curl`
that starts and finishes inside one interval is only ever seen once, and
treating every unfamiliar socket as a baseline would silently drop it — short
connections would be invisible and the totals would read low forever.

`r` rezeroes. It clears the accumulated totals but **keeps** the per-socket
baseline, so the next sample differences against counters read before the
reset and adds only what has happened since. Traffic from before a reset
cannot reappear after it.

## Processes that go away

A process keeps its total after it exits, dimmed, with its rates blank. "What
has been eating the connection" is usually asked *after* the thing has
stopped, and a table that forgets on exit cannot answer it.

Rows are keyed by pid **and** name together, so a recycled pid running
something else starts its own row rather than inheriting a stranger's bytes.

## Naming

`/proc/<pid>/comm` is the kernel's own answer and is usually what you want.
When it carries no letters at all the enclosing path is walked back for
something that means something: a binary at `…/claude/versions/2.1.233`
reports itself as `2.1.233`, which is true and useless, and is shown as
`claude`.

## Routed traffic, and the `interfaces` line

**A machine that routes rather than terminates passes traffic this cannot
see at all.** An exit node, a subnet router, a container host: a packet
arrives on one interface and leaves by another, forwarded at the IP layer by
the kernel. It never terminates in a socket here, so `/proc/net/tcp` has no
entry for it, `ss` reports nothing, and no amount of filtering will reveal
it. It is not being hidden — it does not exist in the place this reads.

On a Tailscale exit node that is most of the traffic. Measured on one:
`ens4` moved 9.6 MB in twenty seconds while the sockets accounted for 2.0 MB
— **85% forwarded**, and invisible.

So the header carries a second line, read from `/proc/net/dev`, which counts
bytes per interface whatever produced them:

```
 TCP only · ↓ 6.3 KB/s  ↑ 72.2 KB/s  · internet only
 interfaces · ↓ 41.1 KB/s  ↑ 135.5 KB/s  · 44% of it has a socket  · ens4
```

The first line is what the process list adds up to. The second is what the
interfaces actually moved. When they agree, the list below is the whole
picture. When they do not, the difference is traffic passing through, and
the percentage says how much of the story the table is telling.

There is a second way the table can be showing less than everything, and it
says so too. When there are more processes than rows, the header adds
**`showing 1-14`** beside the count, and the table follows the cursor rather
than staying at the top of the list — so `↑` `↓` scroll it, and the selected
row is always on screen. Without the window the cursor walked off the bottom
of a short pane and disappeared, while `→` still opened whatever it was
invisibly sitting on. The label is absent when every process fits, because
then there is nothing to say.

Only real interfaces are counted — loopback, `tailscale0`, `docker0`, bridges
and veth pairs are skipped, because a forwarded packet leaves through a card
as well and counting both would count it twice. Which ones were counted is
named at the end of the line, where the pane is wide enough to say so; that
is the first thing dropped when it is not.

Two reasons the two lines never match exactly, even on a machine that routes
nothing. The interface counters include **every protocol** — UDP, ICMP, ARP,
QUIC, WireGuard — where the table is TCP only. And they count **framing**:
Ethernet, IP and TCP headers are on the wire, while `bytes_sent` and
`bytes_received` are payload. So the share is capped at 100%, and anything
above about 90% means the table is telling essentially the whole story.

On an exit node specifically, a peer's traffic crosses the same interface
**twice** — inbound wrapped in WireGuard, then outbound again in the clear to
wherever it was going. The count is a truthful account of what the card
moved; it is not the same number as what the peer transferred.

If you need per-process attribution of *forwarded* traffic, this method
cannot give it and neither can any other that reads sockets. That needs
conntrack (`conntrack -L`, which sees the NAT flows) or eBPF at the
forwarding hook — a different tool.

## What it cannot see

**UDP is invisible.** `ss` keeps no byte counters for UDP sockets — there is
nothing to read. That is a larger hole than it first appears:

- **QUIC**, which is HTTP/3, and therefore a growing share of ordinary web
  traffic;
- **DNS**, almost all of it;
- **WireGuard, and so Tailscale** — traffic to a tailnet peer is carried
  inside `tailscaled`'s UDP socket. It does not appear against the
  application, and it does not appear at all;
- **mosh**, and anything else with a UDP transport.

**Processes you do not own are excluded by default.** They are the machine's
own daemons — `tailscaled`, a cloud guest agent, `ssh.socket` — carrying
traffic you did not ask for and cannot do anything about, and they crowd out
the rows you can act on. `o` shows them, `--all-users` starts with them
shown, and `netwatch.mine` in config sets which way round it opens.

Both totals are recorded on every sample, so `o` flips instantly and the
chart redraws over history it already had rather than starting again. What
the header and the chart count follows the filter: with it on, the figures
are your processes, not the network card's.

**Another user's sockets have no pid here** — `/proc/<pid>/fd` is unreadable
for them, which on a normal machine means everything root runs. They are
still counted, and on a systemd machine they are still *named*: `ss` prints
the control group for every socket regardless of who owns it, and
`/system.slice/tailscaled.service` is `tailscaled` however closed its `/proc`
happens to be. So the busiest rows a laptop cannot explain — `tailscaled`,
`google-guest-agent`, `ssh.socket` — arrive with names and no pid, rather
than piled into one anonymous heap.

Where there is no cgroup to read, or nothing in it worth a name, the row is
`(unattributed)`. Those bytes are counted either way: dropping them would
make the totals quietly wrong.

Two of those names are worth knowing about on a machine like this one.
`tailscaled` appears because Tailscale falls back to relaying over TCP/443 to
a DERP server when it cannot get a direct path — the direct path is
WireGuard over UDP and stays invisible, but the relayed one does not, and it
is attributed to the daemon rather than to whichever application caused it.
And a cloud VM's guest and ops agents talk constantly to the metadata service
and to their telemetry endpoints, which is traffic you did not ask for and
would otherwise be unexplained.

**Traffic in the last fraction of a socket's life is missed.** If a connection
moves data and closes between two samples, what it moved after the final
sample went unread. Sampling cannot avoid this; a shorter `-i` narrows it.

**A VPN or proxy re-attributes traffic to itself.** If your connection leaves
through a local proxy, the bytes belong to the proxy's process, not to the
application that asked for them — the kernel is being told the truth about who
opened the socket, and that is the proxy. The same is true of system services
that fetch on another process's behalf.

## Relation to traffic-ctrl

This is the terminal-toys equivalent of
[`traffic-ctrl`](https://github.com/stealth-factory/traffic-ctrl), a Swift
tool that does the same job on macOS with `nettop`. The feature set is
matched; the interface is this repository's, not that one's, and the data
comes from `ss` and `/proc` rather than `nettop`.

**One feature is deliberately absent: pausing a process.** `traffic-ctrl` can
`SIGSTOP` a process for thirty seconds as a diagnostic. That is a reasonable
thing to do to an application on a laptop and an unreasonable thing to do to a
process on a server, where stopping the wrong one stops a service. This is a
watcher. It reads, and it signals nothing.

## Looking into one process

`↑` `↓` select a row and `↵` opens it. The question that sends you here is
usually "why is *that* using so much data", and the screen answers as much of
it as a machine honestly can:

```
╺━ CURL · PID 2754838 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 3.2 MB since it was first seen  ·  ↓ 411.2 KB/s  ↑ -

 ── PROCESS ──
  command   curl -s -o ~/tmp/big.bin --limit-rate 400k https://…/__down
  directory ~/projects/terminal-toys

 ── TALKING TO ── 1 endpoint
   HOST                              PORTS            RX         TX       RATE
   162.159.140.220                   https    ↓   3.2 MB ↑    722 B 411.2 KB/s

 ── CONNECTIONS ── 2 sockets
   SOCKET                                LOCAL STATE          RX         TX
   162.159.140.220:443                   50206 open   ↓   3.2 MB ↑    722 B
   162.159.140.220:443                   43738 open   ↓   1.1 MB ↑    310 B

 ── FILES ── 1 file
   PATH                                                  SIZE      GROWTH
   ~/tmp/big.bin                                       3.0 MB +425.7 KB/s

 ── DISK ── read 0 B · written 3.0 MB since it started
 HTTPS hides the URL and the filename. Who it talks to and what it writes
 are above.
```

Every list is drawn in full, always, and `↑` `↓` scroll the screen. `tab`
focuses a list instead: the focused one is marked `▏`, `↑` `↓` then move a
cursor `▸` inside it, and `c` copies whatever that cursor is on.

Exactly one *other* heading carries `[tab] to focus`, and it is the one tab
would actually focus next — not all of them, because tab cycles and a `[tab]`
on all three would promise three lists that one press reaches. This heading
used to name a key per list, `[e]` and `[f]`, pointing at keys that no longer
existed; tab was the only one that was ever true, so tab is what is left, and
only where it is true. A list with nothing in it never carries it, because tab
steps over those.

Under the arrows the three lists read as **one continuous list**: `↓` off the
bottom of a list steps into the top of the next, and `↑` off the top steps
into the *bottom* of the one above — the row you were about to reach if they
had never been separate. `tab` is the shortcut across a whole list rather
than the only way between them.

You let go of the lists at exactly two places: `↑` from the very first row,
and `↓` from the very last. Both put you back to scrolling the screen, as
does `tab` from the last list. Lists with nothing in them are stepped over in
every direction, since there would be nothing to put the cursor on.

**This is the same rule in every widget here that has focusable sections.**

**TALKING TO** ranks the remote hosts by what they have carried since launch.
Hosts, not sockets: a process opening six connections to one CDN is one thing
being talked to. Peers resolve to names in the background — the address shows
until the answer arrives, and a lookup is never made twice. The row the cursor
is on gets its own small rx/tx chart, drawn directly beneath it, which is the
quickest way to see whether that host is the one doing the work.

**CONNECTIONS** is the sockets themselves, open right now, which is a
different question: one host may hold six of them, and a socket that has
closed still shows what it carried. It charts the same way TALKING TO does —
the cursor's socket gets an rx/tx chart under its row.

**LOCAL** is the port at this end, and it is the only column that tells those
six apart. Five sockets to one CDN all read `1.2.3.4:443`, because the
address and port shown are the *peer's* and the peer's port is 443 on every
one of them; what differs is the port here. Without it the list is five
identical lines and "why are there so many of these" has no answer on screen.
The name is `ss`'s and `netstat`'s own — both label that column *Local
Address:Port*. A socket seen before the port could be read shows `-`.

A hostname is a best-effort label rather than the domain that was asked for.
CDNs, shared addresses, encrypted DNS and connection reuse all mean one
address can stand for many names, or for none.

**FILES** is the useful half of "which file". A download has to land
somewhere, and where it lands is a file getting bigger: these are the regular
files the process has open, their size, and how fast that size is growing
since you opened this screen. That is the name of the thing being *written*,
which is usually what you wanted when you asked which file was being fetched.

**DISK** is the process's own read and write totals from `/proc/<pid>/io`,
which is how you tell a download being written to disk from one being
streamed and discarded.

### What this cannot tell you, and why

**Not the URL, and not the remote filename.** Both are inside the TLS session.
There is no vantage point on this machine, short of terminating the connection
yourself with a proxy and its certificate installed, from which an HTTPS
request line is readable — that is the entire point of HTTPS, and no amount of
`/proc` gets around it. Packet capture would not help either; it would show
the same encrypted bytes.

What you get instead, and what usually answers the question:

- **Who** — the peer, by name where DNS has a PTR record. A process pulling
  from `storage.googleapis.com` is telling you a great deal.
- **What it landed as** — the growing file.
- **The command line**, which for anything launched from a shell frequently
  contains the URL outright, as `curl` does above.

If you truly need the request line, the tool for it is a local proxy you
trust — `mitmproxy` with its certificate installed — pointed at that one
process. That is a deliberate act of interception, which is why it is not
something this widget does quietly.

A row that has exited says so, and keeps its total. A row that is merely idle
says *that*, rather than claiming it has gone: `alive` in the table means
"moved bytes in the last sample", which a long-running server sitting quiet
does not.

## Units

Decimal — `KB` is 1,000 bytes, `MB` is 1,000,000 — which is how link rates and
data caps are quoted, and therefore what these numbers are usefully compared
against.

Every **rate** — `NOW`, `DOWN`, `UP`, the per-endpoint and per-socket figures,
and a file's growth — is an average over the last **ten seconds**, not over
the one-second sample. Almost all traffic is bursty, so an instantaneous rate
is honest and unreadable: a process that is steadily busy flickers between a
figure and a dash. An average over a stated window is just as true and can
actually be read. The header says the window (`every 1s · rates over 10s`) so
the number is never a mystery, and the **totals** are untouched by it.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | select a process in the list — on the detail screen, scroll it, or move the cursor inside a focused section |
| `PgUp` `PgDn` `Home` `End` | move the detail screen by a page, or to either end |
| `↵` / `→` | open the selected process |
| `esc` / `←` | back to the list |
| `tab` | focus the next section, and from the last one back to scrolling |
| `c` | copy the selected host, socket or path |
| `e` `f` | jump straight to the endpoints or the files — **`netwatch` only**; the Rust build reaches every section with `tab` alone |
| `s` | switch sort mode (`t` also works) |
| `o` | show or hide processes you do not own |
| `1` | sort by total data used |
| `2` | sort by current rate |
| `r` | rezero every total |
| `q` | quit |

## Options

```
netwatch [-i SECONDS] [-n COUNT] [--sort total|live] [--external] [--plain]
```

| Option | Meaning |
|---|---|
| `-i`, `--interval` | seconds between samples; default 1 |
| `-n`, `--limit` | how many processes to show; default fills the pane |
| `--sort` | `total` or `live`, the mode it opens in |
| `--external` | public internet only — **the default**, kept as an alias |
| `--all-external` | widen it: include the LAN, the tailnet, everything off-box |
| `--all-users` | include processes you do not own, off by default |
| `--plain` | print a block per interval, no clearing, for a pipe or a log |
| `-h`, `--help` | this |
| `-V`, `--version` | print the version |

The strict filter is **on by default**: a connection counts only when the far
end is a globally routable address. The local network (`10/8`, `172.16/12`,
`192.168/16`, `169.254/16`), the tailnet (`100.64/10`,
`fd7a:115c:a1e0::/48`), and this machine's own addresses are all somewhere
other than the internet, and "what is this box sending out" is nearly always
the question. `--all-external` widens it to everything that leaves the
machine. Loopback and self-addressed connections are excluded either way.

Note `-i` is the sampling interval here, where the other widgets in this
repository spell that `-n`. This one follows the interface it was asked for,
and `-n` is the row limit.

## Cost

One `ss -tine` and one walk of `/proc/*/fd` per interval. No network traffic
of its own, no root, no capture.

## Configuration

```json
"netwatch": {
  "interval": 1.0,
  "limit": 0,
  "sort": "total",
  "external": true,
  "mine": true
}
```

Every one of them is what the matching command-line option sets, and the
option wins where both are given. `limit` of 0 means fill the pane.

## Needs

`ss`, from iproute2, which is standard on Linux. The widget exits with a
message rather than drawing an empty frame if it is missing.
