# `opscope`

[← all docs](../../../docs/README.md)

The front door: every widget, what it does, and a preview before it runs.

```
╺━ OPSCOPE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 14 widgets   ↵ or → starts one, q leaves

 ▸ agent-usage  How much the coding agents have been used…
   clocks       Server clock, countdowns, a pomodoro…
   vercel-deployments  Vercel deployments over time…
   github       GitHub delivery metrics across every org…
   latency      Multi-target latency monitor.
   months       A month grid you can page through…
   netwatch     Which processes are using the network…
   …

 ── AGENT-USAGE ──
   How much each coding agent on the machine has been used…

 ↑↓ select  ↵ launch  [,] settings  [q]uit
```

Fourteen widget binaries in a directory are a list you have to already know. Pick one
and it runs; quit it and you are back here.

## Nothing is described twice

The launcher writes none of this down twice. Every widget owns one folder
under `widgets/src/widgets/`; the launcher compiles its maintained files
rather than keeping another description:

- **What each one does** — the same `help.txt` the binary itself answers
  `--help` with, taken with `include_str!`.
- **What it looks like** — the opening preview in that folder's
  `README.md`, again embedded as the same bytes.
- **Which widgets exist** — a list in the launcher's own source, one entry
  per binary. It is the one thing that is written down, because a binary
  cannot enumerate its siblings the way a directory of scripts could.

`check.rs` enforces the folder contract: `main.rs`, `help.txt`, `README.md`,
`CONFIGURE.md`, and `settings.json` when the widget has settings. Adding a
widget still adds one name to the compiled registry and Cargo manifest, but
the contributor owns its full experience in that one folder.

The launcher has the same maintained-file shape for its own help and docs.
Its settings declaration and configuration guide cover only the shared
`terminal` section, not any widget's settings.

## The preview

Under the description, in whatever height is left, a picture of the
highlighted widget — its own README's opening example, marked as one.

```
 ── CLOCKS ──
  A big clock in the machine's own timezone, countdown bars for the next hour…
 ┌── example ──────────────────────────────────────────────────────────────┐
 │╺━ CLOCKS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 │ ── SERVER TIME ──
 │   ███ ███     ███ ███     █ █ ███
 │   █ █ █ █  █    █ █ █  █  █ █ █ █
 │ ── COUNTDOWN ──
 │ Pomodoro · FOCUS       00:23:32   3 done
```

Every widget README opens with a rendering maintained beside its code, so
there is no second copy here — the same arrangement as the descriptions.

It says `example` on the frame because it is one. Static numbers in a live
layout would otherwise read as this machine's, and they are somebody's from
the day the page was written.

### Why not run the real thing

It used to. The launcher started the highlighted widget in a pseudo-terminal
and showed its actual frames, which was accurate by construction and cost
nothing to keep in step.

It also had side effects, and that is what settled it. Arrowing onto
`latency` spawns `ping` and puts packets on the wire. Onto `github` or
`github-prs`, GraphQL calls against a 5,000-an-hour quota. Onto `vercel-deployments` or `linear`,
their APIs. Onto `agent-usage`, a walk of the entire agent transcript tree, which
on this machine is 541 MB. **Browsing a menu should cost nothing**, and a
menu that quietly spends your API budget as you scroll past a row is a menu
with a trap in it.

The live version also had to solve problems the static one does not have at
all: decoding partial characters across read boundaries, stripping cursor
control so a child could not move the real cursor, killing process groups on
every selection change, and a carriage-return translation that made previews
erase themselves. That is a hundred lines and three bugs bought with running
processes nobody asked to run.

What is lost is colour, and the certainty that the picture matches today's
build. The docs are checked by review rather than by machine, so a page that
falls behind its widget shows a stale picture here too.

## It says nothing about whether a widget will work

It used to. There was a column reporting whether each command was installed
and each token set, and it was the wrong place for all of it.

A widget that cannot run is the thing that knows why — which command, what it
is for, what to install. Saying it out here meant saying it twice, in less
detail, to somebody who has not yet asked. Now the launcher describes what
each widget *is*, and a widget that cannot start says so on its own screen:

```
╺━ NETWATCH ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 cannot start · needs ss

 ss reports the per-socket byte counters this is built on: how much each
 TCP connection has carried, and the inode that ties it to a process.
 Without it there is nothing to read.

 It ships in iproute2, which is on essentially every Linux system — its
 absence usually means a very small container image.

 try: apt install iproute2

 [q]uit
```

It **holds** there rather than exiting. A widget that dies on a missing
dependency is a pane that vanishes the moment you look at it, taking its
explanation with it — and in a tiled wall, or started from this menu, a line
on stderr has nowhere to go. So it draws the reason and waits, answering `q`
like everything else.

`link`, `netwatch`, `latency` and `herdr-panes` all do this, via
`cannot_start` in `opscope-core`. The first two used to exit; the second two used
to run and quietly show nothing, which was worse.

## Shared terminal settings

Mouse reporting belongs to the terminal experience rather than to any one
widget. Press `,` here to open the shared settings screen for
`terminal.mouse`; it shows the resolved config file, current value, default,
and field help. `opscope --configure-help` prints the launcher-owned guide.

The setting defaults to `true`, which enables wheel events but prevents
drag-to-select in the terminal. Turning it off restores drag selection;
`Ctrl-Y`, `Ctrl-E`, and the arrow keys still work.

## Launching

`↵` hands the terminal over: cursor restored, raw mode off, the widget gets a
normal terminal and this process waits. Quit the widget and the launcher
takes the terminal back.

Every widget is listed, whether or not this machine can run it. A widget that
is missing a tool or a token says so on its own screen, in its own words,
and `q` brings you back here - which is a better place to learn it than a
menu that has quietly hidden the row.

Naming one skips the menu entirely, and anything after it is passed straight
through:

```sh
opscope                    # the menu
opscope netwatch           # straight into one
opscope netwatch -i 2 -n 5 # arguments go to the widget
opscope link --help        # including --help
```

A widget is looked for beside the launcher's own binary, so a release
unpacked anywhere works without a path being configured.

`opscope netwatch.py` is still accepted, and only for that: every widget here
answered to that name for years and the muscle memory outlives the files.
The suffix is stripped and the binary of the same stem runs.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | select a widget |
| `↵` / `→` | launch it, and come back here when it quits |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the view a line at a time — the pane moves, the selection stays where it is |
| `,` | open shared terminal settings |
| `q` | quit |

## Cost

Descriptions and previews are compiled into the launcher. Browsing performs
no file discovery, starts no widget, calls no API, and polls nothing. It
touches the filesystem only when you launch the selected sibling binary or
open the settings screen, which reads the resolved config and writes only
after you confirm a change.
