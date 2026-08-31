# `tailnet`

[← all widgets](../../../../docs/README.md)

Tailscale peers, and — the part plain `tailscale status` buries — *how* you are
reaching each one.

```
╺━ TAILNET ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 workstation-gcp-asia-east1-a-01-c  100.64.10.100  example-1.ts.net
 10 online / 23 peers   3 direct   7 relayed   every 2s
 3 advertising routes   exit node: none

 ── LIVE THROUGHPUT ── peers moving data
 tablet-lounge-ipad ↓▁▁▁▁▁▁ ↑▄█▂▃▁▁   5.3K/s  8.8K/s
 studio-mac-desk-1  ↓▁▂▁▁▁▁ ↑▁▂▂▂▁▁   1.5K/s  1.3K/s
 laptop-mac-1       ↓▁▁▁▁▁▁ ↑▁▁▁▁▁▁   1.2K/s  1.0K/s

 MACHINE                           OS       PATH       RX    TX  SEEN
 rx/tx = this host ↔ peer, since tailscaled started 26h ago
▸◆ workstation-gcp-asia-east1-a-01 linux   this       0B    0B   now ⇄
 ● laptop-mac-1                    macOS   DIRECT   305M  3.1G   now
 ● studio-mac-desk-1               macOS   DIRECT   567M  2.3G   now
 ● tablet-lounge-ipad-01           iOS     DIRECT    35M  616M   now
 ● nas-storage-1                   linux   hkg        0B    0B   now ⇄
 ● chat-bot                        linux   ord        0B    0B   now
 ● mini-mac-node-1                 macOS   syd        0B    0B   now
 ● pi-node-bedroom-2-au            linux   syd        0B    0B   now ⇄
 ● mini-mac-node-5                 macOS   sfo        0B    0B   now ⇄
 ● phone-personal-air-1            iOS     hkg        0B    0B   now
  ● ml                             linux   fra        0B    0B   now
 ○ workstation-gcp-us-west1-b-0002 linux   -          0B    0B    9d
 ○ vpn-exit-gcp-us-west1-c-nordvpn linux   -          0B    0B  301d
 ○ living-room-tv1                 tvOS    -          0B    0B   17d
 ○ ubuntu-test-vm-1                linux   -          0B    0B  254d
 ○ phone-spare-12-mini-0001        iOS     -          0B    0B   15d
 ⇄ NAS-storage routes 192.168.100.0/24
 ↑↓ select  →/↵ info  [c]opy  [g]raph  [o]ffline  [i]nterval 2s  [r]efresh  [,] settings
```


## This machine is in the list

It sits at the top, marked `◆` rather than `●`, with `this` where the other
rows say `DIRECT` or a relay name — the path column answers how traffic gets
there, and for this machine it does not go anywhere.

It is **not** in the counts above it. `12 online / 23 peers · 8 direct · 4
relayed` describes connections out of here, and there is no connection from
here to here; adding self would have quietly inflated the peer count and, since
`Self` carries no `CurAddr`, filed this machine under "relayed".

It is also never pinged. `tailscale status --json` reports it under `Self`
rather than `Peer`, which is why it was missing to begin with, and the latency
prober skips it — a round trip to ourselves would read as a suspiciously good
link.

## The column that matters

**PATH** is either `DIRECT` — NAT traversal succeeded, traffic flows
peer-to-peer — or a named DERP region, meaning every packet round-trips through
Tailscale's infrastructure. That difference can be tens to hundreds of
milliseconds, and it explains most tailnet latency surprises. A peer relayed
through `sfo` when you are in Hong Kong is going the long way.

**`⇄` marks peers advertising subnet routes.** Those routes are inert unless
this node runs with `--accept-routes`, which is worth knowing before you spend
an afternoon wondering why an address on the far LAN will not answer.

**RX/TX are this host's traffic with that peer**, counted by the local
WireGuard engine — not the peer's own totals. They live in tailscaled's memory
and reset with it, so the panel states the window: a peer reading `0B` may
simply not have talked to you since the last restart.

**Machine names come from MagicDNS, not `HostName`.** Devices self-report
uselessly — an iPad, a Chromecast and a Pixel all called themselves `localhost`,
and two Apple TVs both claimed `apple-tv`. The MagicDNS label is unique across
the tailnet and matches the admin console.

## Info view

`Enter` or `i` opens everything known about the selected machine: MagicDNS name,
the self-reported hostname when it differs, owner, tags, direct-or-relayed path,
handshake and enrolment times, its **home DERP region as a location hint**
(`hkg — Hong Kong`, read from the local DERP map so no address is sent to a
geolocation service), every address it has, its advertised routes, and whether
it offers itself as an exit node.

It also carries **live latency** for that peer — current, average, median, min,
max, jitter, loss and a sparkline, the same statistics `latency` reports,
measured by ICMP over the tunnel. Only the selected peer is probed, so this
costs one ping process no matter how large the tailnet, and history is kept per
peer so returning to one still shows its earlier samples.

## Copying addresses

`c` opens a copy sheet: Tailscale IP, MagicDNS name, public IP, LAN IP and
IPv6, on keys `1`–`5`, copied via OSC 52 so they reach your local clipboard.

Peer LAN addresses come from `tailscale debug netmap`, which needs root — it is
attempted with `sudo -n` so it fails instantly rather than prompting, and is
simply omitted when unavailable. **The widget never requires privilege.**

Where a peer exposes several private addresses, one inside a subnet it
advertises wins over a docker or virtual bridge: a NAS was otherwise reporting
`172.17.0.1` as its LAN address instead of `192.168.50.20`.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` | select a peer |
| `→` / `Enter` | machine info view — `i` in the Python, which is being retired |
| `←` / `esc` | back out of the info or copy view |
| `c` | copy addresses |
| `g` | show/hide the live throughput graphs |
| `o` | hide offline peers |
| `i` | poll interval — 1 / 2 / 5 / 10 / 30s |
| `r` | refresh now |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the view a line at a time — the pane moves, the selection stays where it is |
| `,` | open settings |
| `q` | quit |

## Throughput graphs

Byte counters are cumulative, so differencing them across polls gives rates.
Peers below 64 B/s are treated as keepalive noise and omitted, or the section
fills with idle machines drawing flat lines. A tailscaled restart zeroes the
counters, which would read as a large negative rate; those samples clamp to
zero.

The list and the info view scale differently on purpose: the list shares one
peak across peers so machines are comparable at a glance, while the info view
scales to that machine's own peak so its variation stays visible when another
peer dominates.

## Configuration

```json
"tailnet": { "refresh": 2, "history": 180 }
```

Graph resolution follows the poll interval, so `i` doubles as a zoom control.
Needs the `tailscale` CLI; no root required.
