# `link`

[← all widgets](../../../../docs/README.md)

How good the connection is between this machine and whoever is connected to
it — measured, not probed.

```
╺━ CONNECTIONS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 4 inbound · measured by the kernel, nothing sent   every 2s

  PEER                        PORT    NOW   FLOOR  JITTER    LOSS  ACHIEVED   IDLE
▐ 198.51.100.7:62392          3003   45ms    30ms   8.1ms  0.00%  13.0Mbps    19h
▐ 198.51.100.114:56865        3333   76ms    52ms    34ms  0.00% 328.9kbps    14s
▐ 100.100.100.100:59448 will    22   58ms    20ms    20ms  0.13%  23.5Mbps     0s
▐ 198.51.100.220:57582        3010   36ms    28ms    13ms  0.00%   1.7Mbps     1m

   95ms│
       │
       │
       │⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤
       │
       │
       │                                          ⡀
       │                                         ⢰⢱
       │                                         ⡎⠈⡆   ⢀⣀⣀⣀                         ⡜
       │     ⢰⡀                           ⢠     ⢰⠁ ⢸  ⢀⠎   ⠉⠑⢆                     ⢰⠁
   50ms│     ⡇⢇                   ⡔⡄     ⢀⠏⡆    ⡇   ⢇ ⡜      ⠘⡄                   ⢀⠇
       │    ⢸ ⠸⡀          ⢰⡀    ⢠⠊ ⠈⢆    ⡜ ⢣   ⡸    ⠸⡰⠁       ⢱                   ⡜
       │⠒⠒⠒⠒⠒⠒⠒⢇⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡇⠒⠒⠒⠒⡰⠒⠒⠒⠒⠈⠒⠒⠒⢰⠒⠒⠘⠒⠒⢀⠒⠒⠒⠒⠒⠒⠁⠒⠒⠒⠒⠒⠒⠒⠒⠒⡆⠒⠒⠒⠒⠒⠒⠒⠒⠒⢀⠒⠢⠒⠒⠒⠒⠒⡰⠒⠒⠒
       │   ⢰⠁  ⠸⡀  ⢀⠎⢱   ⢰⠁⠈⡆ ⢰⠁      ⠑⣄⠇   ⢇ ⡜                ⠘⠢⢄⡠⠊ ⢇   ⡸   ⢣  ⢠⠃
       │   ⡜    ⠉⠑⠒⠁  ⢇  ⡜  ⢱⢠⠃        ⠈    ⢸⢰⠁                      ⠘⡄ ⢀⠇    ⢇⢀⠇
       │⣀⣀⢠⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⠘⣀⢠⣀⣀⣀⣀⠃⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⠸⣀⡜⣀⣀⣀⣀⣀⣀⠎⣀⣀⣀⣀⣀
       │⢀⠤⠊            ⢱⡸                                              ⢣⠃
       │⠁               ⠃
       │
       │
   27ms│
       └─────────────────────────────────────────────────────────────────────────────
        1m ago                                                            now

 ↑↓ select  →/[↵] open  [w] 1m  [o]hide idle  [r]efresh  [,] settings  [q]uit
```

## Why this is not the latency monitor

`latency` measures paths it was told to measure, by sending pings. This one
measures the path **you are on**, and sends nothing at all.

Every established TCP connection has a kernel that has been timing it since it
opened. `ss -tin` hands that over: smoothed round-trip time and its variance,
the best round trip the path has ever managed, retransmitted bytes, the
delivery rate actually achieved, the congestion window. All of it measured on
the packets your session was already sending.

So this widget adds no traffic to the link it is describing — which matters,
because a probe that competes with the session it measures is measuring itself.

## Which sessions

Every established connection arriving at a port this machine **listens on**.
That is SSH, and anything else accepting terminals, found without naming a
single port number: inbound is defined by "came to a port we answer on", so a
terminal server on some high port is included the day it starts listening.

Loopback is excluded. `::ffff:127.0.0.1` — an IPv4 address wearing an IPv6 hat
— slipped past the first version of that filter and put a 22-microsecond local
socket on the chart, which flattened every real session against the top of a
log axis.

**It cannot tell you which session is yours.** The widget runs in a pane owned
by the terminal server, not by your `sshd`, so there is no honest way to point
at one row and say "this is you". With one session connected, it is you. With
several, they are all listed and none is marked — which is the useful answer
anyway when the question is "how is everything reaching this box".

## The columns

| | |
|---|---|
| **PEER** | who is connected, and the port *they* dialled from. That port is the only thing telling four browser tabs against one dev server apart — same address, same service, four sockets. A login name follows it where the pane is wide enough and `who` knows one |
| **PORT** | the port on **this** machine they reached, which is what they are connected *to*. Every row here is inbound, so this is always one of your own services — SSH, a dev server, whatever `ports` is watching. Read it against PEER: the address answers *who*, this answers *what for* |
| **NOW** | the kernel's smoothed round-trip time, this instant |
| **FLOOR** | `minrtt` — the best this path has ever done. The *gap* between it and NOW is the congestion, and it is why NOW alone means little |
| **JITTER** | RTT variance. A steady 90ms link types better than one flapping between 20 and 90 |
| **LOSS** | retransmitted bytes **since the last poll**, not since the connection opened. A session running for a day has long forgiven whatever went wrong at breakfast |
| **ACHIEVED** | `delivery_rate` — what the connection *has* delivered. Not capacity: measuring that means flooding the link, which this repo will not do to a link someone is typing on |

Colour is judged against the socket's own floor rather than a fixed threshold:
40ms is excellent from another continent and poor from the next rack, and the
kernel already knows which this is. Amber past 1.6× the floor, red past 3× or
above half a percent of loss.

## The chart

Log scale, newest on the right, one glyph and hue per session so the trace
reads without colour. Log because the sessions on one machine can differ by
two orders of magnitude — a laptop across town and a phone across an ocean —
and a linear axis draws the near one as a flat line along the bottom.

Only the top, middle and bottom rows carry an axis label: a number on every
row is a table pretending to be an axis.

### How much time is on screen

`w` cycles the span: **1m, 5m, 15m, 1h**, shown in the footer and marked under
the chart's left corner. The retained history follows the longest of them
automatically, so the last window on the list is always one the samples can
actually fill.

Up to a minute or so there is one column per sample, as there always was.
Beyond that there are more readings than the pane has columns — fifteen
minutes at a two-second poll is 450 of them — so each column becomes the
**median** of its slice. The median is the typical round-trip over that
slice, which is what a line should show; a mean would let one 900ms stall
drag a whole column, and a max would draw a chart made entirely of worst
cases.

The consequence is worth stating: **at longer windows a brief spike is
smoothed away.** The peak is not lost — the table's `worst` column and the
detail screen both report it — but if you are hunting for a stall, look at it
on `1m`, where nothing is being averaged.

A window longer than the session has existed for simply draws what there is,
left-padded, and the corner label says how far back the columns really go
rather than how far back they could.

## What it cannot see

**mosh is invisible.** It is UDP, and none of this exists for a UDP socket —
there is no retransmission, no congestion window, no kernel RTT to read. A
mosh session simply does not appear, which is the honest outcome, and worth
knowing before concluding nobody is connected.

The same is true of anything else that carries a session over UDP, including
some VPN-tunnelled setups where the TCP socket the kernel sees is the tunnel's
rather than the session's — in that case the numbers describe the tunnel, and
are still true about the path, just not about the terminal.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` | select a session |
| `↵` / `i` | open that connection on its own screen |
| `n` / `p` | on the detail screen, step to the next or previous connection |
| `esc` | back to the list |
| `w` | cycle the chart's span: 1m, 5m, 15m, 1h |
| `o` | hide or show sessions idle over five minutes |
| `r` | re-read now |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the view a line at a time — the pane moves, the selection stays where it is |
| `,` | open settings |
| `q` | quit |

## Two screens

The list is for noticing; the second screen is for looking into. Opening a
session gives it the whole pane: every figure the kernel keeps for that socket,
labelled in words rather than in `ss` abbreviations, and a chart of nothing but
that connection.

Some of it only makes sense with room to explain, which is why it lives here
rather than in the table — `timeout` is how long before a lost packet is
resent, `in flight` is how much the kernel will leave unacknowledged at once
and collapses before your typing starts to feel bad, `pacing at` is the rate it
is willing to send at as distinct from what it achieved.

A session keeps its glyph and colour across both screens. Opening the `▲` row
and finding a `●` chart reads as a different connection.

## Cost

One `ss` invocation per refresh, default every two seconds, and one `who`.
**No network traffic whatsoever** — every number is read from the kernel's
existing accounting for sockets that already exist.

## Configuration

```json
"link": {
  "ports": [],
  "refresh": 2,
  "history": 120,
  "windows": [60, 300, 900, 3600]
}
```

`ports` empty means every port this machine listens on, which is the useful
default. Naming ports instead pins the set — worth doing if something else
here accepts connections you would rather not watch.

`windows` is what `w` cycles through, in seconds, and the first one is what
opens. `history` is a floor on how many samples are kept: the actual figure is
whatever covers the longest window, so adding a six-hour entry to `windows`
does not also need `history` raised to match. At the default two-second poll,
the hour window means about 1,800 readings per session — a few tens of
kilobytes.

## Needs

`ss`, from iproute2, and `who`. Both are standard on Linux. The widget exits
with a message rather than drawing an empty frame if `ss` is missing.
