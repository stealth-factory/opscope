# terminal-toys

Small terminal widgets with no dependencies — pure Python 3 standard library, 24-bit
colour, and a full redraw each frame so everything reflows when you resize the pane.

Each widget is a single self-contained script that imports shared drawing helpers
from `common.py`. Run any of them directly; pass `-h` for its documentation.

## Widgets

### `latency.py` — multi-target latency monitor

Continuously pings a list of hosts and shows, for each: current / average / min /
max round-trip time, jitter, and packet loss, plus a per-target sparkline. Below
that, all targets share one **log-scale** time graph — so a 200µs host and a 140ms
host are both legible on the same plot, which a linear axis cannot do. Loss events
and latency spikes are timestamped in an event log.

```sh
./latency.py                              # built-in target list, 0.5s interval
./latency.py -i 2 1.1.1.1 example.com     # 2s interval, custom targets
```

Edit `DEFAULT_HOSTS` for your own targets. Traffic cost is one 98-byte frame each
way per target per interval — roughly 1.6 KB/s for four targets at 0.5s.

It measures *this host → each target*. It cannot measure target-to-target legs;
that needs a probe running on the far end.

### `deployments.py` — Vercel deployments, live

Deployment activity over time, build-duration trend, and the most recent
deployments with state, project, branch, commit and build time.

- **Activity** — deployments per hour over the last 48h, each bucket coloured by
  its worst outcome, so a failed run is visible in the timeline at a glance.
- **Build time** — median / p95 / max across the fetched window, plus a
  sparkline of recent build durations to show drift.
- **Recent** — one row per deployment with live elapsed time for in-flight
  builds, and the commit subject beneath it.

```sh
./deployments.py                    # every project you can see, 15s refresh
./deployments.py -n 60 ferry-hk     # one project, slower poll
```

Keys: `↑`/`↓` (also `PgUp`/`PgDn`, `Home`/`End`) move the selection, `c` or
`Enter` opens a copy sheet for the selected deployment, `r` refresh now, `f`
cycle filter (all / failed / production), `p` cycle project, `q` quit.

The copy sheet offers the four URLs worth having — deployment dashboard,
branch preview, commit preview and pull request — on keys `1`-`4`. Copying
uses OSC 52, so the terminal you are sitting at performs it and the text
reaches your local clipboard even though the widget runs on a remote host. If
your terminal or multiplexer blocks OSC 52, each URL is still shown in full
for mouse selection.

Credentials come from the Vercel CLI's existing login — if `vercel whoami`
works, so does this. `$VERCEL_TOKEN` is used first if set. The token is read
locally and never printed. `vercel ls --all --format json` returns similar
data, but spawns a Node process per refresh (~1.4s) against ~0.75s for a
direct API call returning 100 records, so this queries the REST API.

### `herdr-panes.py` — everything running in Herdr

Two sections. **AGENTS** lists every agent Herdr knows about, ordered so the
ones wanting attention come first: `blocked` (waiting on an approval right now), `done` (finished background
work you have not seen), then `working` and `idle`. On a server with a dozen
workspaces this is the difference between noticing an agent finished and finding
out an hour later.

**PROCESSES** lists every other pane that is actually running something — dev
servers, monitors, builds — with the command, CPU and memory. **IDLE** lists
panes sitting at a shell prompt, by directory, so they can still be jumped to;
toggle with `o`. The two are told apart exactly rather than by guessing at
process names: a busy pane's foreground pid differs from its own shell pid.

Each row carries the workspace, how long the state has held, and the real CPU
and RSS of the process. Durations show `≥` when the state predates the
widget starting, since then it is only a lower bound.

```sh
./herdr-panes.py       # 4s refresh
./herdr-panes.py -n 10
```

Keys: `↑`/`↓` select across both sections, **`Enter` jumps to whatever is
selected** — an agent's pane, or the tab holding that process, across
workspaces — `w` toggles workspace label vs pane id, `r` refresh, `q` quit.

Since the list is already ordered by who needs attention, "press Enter on the
top row" is the whole workflow: the blocked agent is at the top, and Enter puts
you in front of it.
A Herdr client: states, labels and pids all come from the `herdr` CLI, so
any agent kind Herdr recognises (claude, codex, copilot, cursor,
antigravity, grok and ~15 more) appears with no code change. Requires
`HERDR_ENV`.

### `tailnet.py` — Tailscale peers and how you reach them

Every peer with the column that actually matters: **PATH**. A peer is either
`DIRECT`, meaning NAT traversal succeeded and traffic flows peer-to-peer, or it
is relayed through a named DERP region, meaning every packet round-trips through
Tailscale's infrastructure. That difference can be tens to hundreds of
milliseconds and is easy to miss in plain `tailscale status`.

Peers advertising subnet routes are marked `⇄` — those routes only reach you if
this node runs with `--accept-routes`.

**RX/TX are traffic between this host and that peer**, counted by the local
WireGuard engine — not the peer's own totals. They live in tailscaled's memory
and reset when it restarts, so the panel states the window: a peer reading `0B`
may simply not have talked to you since the last restart.

```sh
./tailnet.py
```

Keys: `↑`/`↓` select a peer, `Enter` or `i` opens a full machine info view, `c`
opens a copy sheet, `g` toggles the throughput graph, `r` refresh, `o` hide
offline peers, `q` quit.

A **live throughput** section graphs peers currently moving data — `↓` receive
and `↑` transmit sparklines with current rates — scaled to a shared peak so
machines are directly comparable. Toggle it with `g`. The info view carries the
same graph for its own machine, scaled to that machine's own peak so its
variation is visible even when another peer dominates.

The info view adds **ICMP latency over the tunnel** for the selected peer —
current, average, median, min, max, jitter, loss and a sparkline, the same
statistics the latency monitor reports — and names the peer's **home DERP
region** as a location hint (`hkg — Hong Kong`), read from the local DERP map
so no address is sent to a geolocation service. Only the selected peer is
probed, so this costs one ping process no matter how large the tailnet.

The info view shows everything known about one machine: its MagicDNS name, the
self-reported hostname when it differs, owner and tags, whether the path is
direct or relayed, handshake and enrolment times, **every address** — Tailscale
v4 and v6, public, and each private one with the LAN address marked as being
inside a subnet the peer advertises — plus its advertised routes and whether it
offers itself as an exit node.

The copy sheet offers the peer's **Tailscale IP**, **MagicDNS name**, **public
IP** and **LAN IP** on keys `1`-`4`, copied via OSC 52 so they reach the
clipboard of the machine you are typing at. Peer LAN addresses come from
`tailscale debug netmap`, which needs root — it is attempted with `sudo -n` and
silently omitted when that would prompt, so the widget never requires privilege.
Where a peer exposes several private addresses, one inside a subnet it
advertises wins over a docker or virtual bridge.

Footers wrap rather than truncate: key hints are packed across as many lines as
they need and never split, since a hint clipped to `[±]25` teaches the wrong
key. In `clocks.py`, `?` hides the pomodoro's controls and hands the space back
to the city list — the toggle itself and the panel keys always remain, since
hiding the way back would leave no way back.

### Configuration

Every widget reads optional settings from the first of
`$TERMINAL_TOYS_CONFIG`, `~/.config/terminal-toys/config.json`, or
`config.json` beside the scripts. Copy `config.example.json` to start. This
keeps hostnames, ping targets and city lists out of the source tree — the
repo ships generic defaults and `config.json` is git-ignored.

### `clocks.py` — server time, countdowns, pomodoro, world clock

Big-digit clock for the machine's own timezone, three live countdown bars, and a
world clock covering 19 tech hubs sorted west to east.

- **Countdowns** — time to the next hour, to the start/end of office hours, and to
  end of day. The office bar flips its own label: it counts down to 18:00 during
  office hours, and to the next opening outside them, correctly skipping weekends
  (Friday evening counts to Monday 09:00, not Saturday).
- **World clock** — each city in its own timezone, coloured by what people there
  are plausibly doing: green at work, amber evening, blue asleep, grey weekend.
  Handles half-hour offsets and `+1d` / `-1d` date rollovers.

```sh
./clocks.py
```

**Pomodoro** — off until you press `p`, which shows or hides it *and suspends
it with them*: a timer counting down where you cannot see it just means coming
back to a block that expired half an hour ago. Hiding freezes it exactly where
it stands, overtime included; showing resumes it only if it was running when it
went away. Standard 25/5 with a longer break every
fourth session. `space` pauses, `r` restarts the phase, `s` skips on, `+`/`-`
change the focus length.

A phase never ends itself. When time is up the counter keeps running and the
bar rescales so a growing red section shows the overrun — at 25 minutes over a
25-minute block, half the bar is red. The terminal is alerted when the phase elapses and again every minute it is
ignored: BEL, plus OSC 9 and OSC 777 desktop notifications. Escape sequences
are the only alerting channel that survives SSH — anything local to the machine
would fire where nobody is sitting. Under Herdr it *additionally* raises a
native toast with a sound; that is purely additive and skipped everywhere else,
so the widget never requires Herdr. The whole panel also flashes white twice, a second apart, on every alert —
visible with the sound muted. `pomodoro_flash_rgb` changes the colour, and the
text colour follows it automatically so a dark choice stays readable. State persists across restarts, so relaunching the panel does not cost you a
session. The completed tally is per day and clears when the date changes —
including while the panel is running — while preferences such as focus length
are not tied to the day. `0` zeroes the tally by hand.

Configure `CITIES`, and `WORK_START_H` / `WORK_END_H` (default Mon–Fri 09:00–18:00).
Set `TZ` to a fixed offset to pin the clock instead of following the system zone.

### `matrix.py` — digital rain

Falling glyphs with truecolor fade trails: a near-white head, bright green
shoulder, and smooth decay over each drop's length. Glyphs mutate in place
independently of the drops, and each column gets its own speed and trail length.

```sh
./matrix.py
```

## Bundled skill

[`skills/herdr/`](skills/herdr/) carries the Herdr control skill, so an agent
working in this repo can drive panes, tabs and workspaces without going looking
for it — copy it to `~/.claude/skills/herdr/` to install. It is Herdr's own
file, not covered by this repository's licence, and `herdr --skill` regenerates
it after an upgrade.

## Building your own panels

[`docs/building-herdr-panels.md`](docs/building-herdr-panels.md) collects what
was learned building these against Herdr — resize semantics, focus, detecting
what a pane is running, notification gating, and the layout mistakes worth
skipping. For the CLI reference itself run `herdr --skill`, which the installed
binary emits and therefore always matches your version.

## Requirements

- Python **3.9+** (`clocks.py` uses `zoneinfo`); developed on 3.12
- A terminal with 24-bit colour support
- `ping` on `PATH` (`latency.py` only) — no root needed
- A Vercel login (`deployments.py` only) — the CLI's token is reused

## Shared helpers

`common.py` holds the pieces every widget uses: terminal sizing, a full-frame
`draw()`, 24-bit colour, progress bars, a green→amber→red `heat()` ramp, and
`seg()`, which joins coloured segments while clipping to a printable-cell budget
so coloured text never overflows a narrow pane.

## License

[GNU AGPL-3.0](LICENSE). You may use, modify and share these widgets freely; if you
distribute a modified version — or run one as a network service — you must make
your source available under the same license.
