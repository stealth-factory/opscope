# `start.py`

The front door: every widget, what it does, and whether it will work on this
machine.

```
╺━ TERMINAL TOYS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 13 widgets · 2 need something first   ↵ launches one, q leaves

 ▸ netwatch     Which processes are using the network, how much…   ss installed
   ports        What is listening on this machine, what started…   nothing to set up
   github       GitHub delivery metrics across every org…          set a GitHub token
   latency      Multi-target latency monitor.                      needs ping
   tailnet      Tailscale network: who is online…                  tailscale installed
   usage        How much the coding agents have been used…         reads what is logged in
   …

 ── NETWATCH ── python3 netwatch.py
   needs `ss`

 ↑↓ select  ↵ launch  [r]echeck  [q]uit
```

Thirteen scripts in a directory is a list you have to already know. Pick one
and it runs; quit it and you are back here.

## Nothing is described twice

The launcher holds no list of widgets, no descriptions, and no requirements
of its own. All three are read at startup from where they already live:

- **Which widgets exist** — every `.py` in the directory that is not
  `common.py`, `check.py`, or the launcher itself. The same rule `check.py`
  uses, so the two can never disagree about what a widget is.
- **What each one does** — its own first docstring line, the one you get from
  `python3 <widget> --help`.
- **What each one needs** — the Needs column of the README's widget table.

That last one is deliberate. It could have been restated here, and then there
would be two descriptions of every requirement, drifting apart quietly. The
README's version cannot rot: `check.py` fails any widget missing a row in
that table.

The practical effect is that adding a widget adds it here. There is no list
to remember to update, which is the only kind of list that stays correct.

## The preview

Under the description, in whatever height is left, a picture of the
highlighted widget — its doc page's own opening example, marked as one.

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

Every widget's doc page opens with a rendering of the widget it describes,
maintained by whoever wrote it, so there is no second copy of anything here
either — the same arrangement as the descriptions. `matrix.py` has no doc
page on purpose and so has no picture; it gets the description alone.

It says `example` on the frame because it is one. Static numbers in a live
layout would otherwise read as this machine's, and they are somebody's from
the day the page was written.

### Why not run the real thing

It used to. The launcher started the highlighted widget in a pseudo-terminal
and showed its actual frames, which was accurate by construction and cost
nothing to keep in step.

It also had side effects, and that is what settled it. Arrowing onto
`latency` spawns `ping` and puts packets on the wire. Onto `github` or `pr`,
GraphQL calls against a 5,000-an-hour quota. Onto `deployments` or `linear`,
their APIs. Onto `usage`, a walk of the entire agent transcript tree, which
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

`link.py`, `netwatch.py`, `latency.py` and `herdr-panes.py` all do this, via
`cannot_start` in `common.py`. The first two used to exit; the second two used
to run and quietly show nothing, which was worse.

## Launching## Launching

`↵` hands the terminal over: cursor restored, raw mode off, the widget gets a
normal terminal and this process waits. Quit the widget and the launcher
takes the terminal back and rechecks, so a token you set or a package you
installed while you were away is reflected without restarting.

Naming one skips the menu entirely, and anything after it is passed straight
through:

```sh
python3 terminal-toys         # the directory itself is runnable
./start.py                    # the menu, from inside it
./start.py netwatch           # straight into one
./start.py netwatch -i 2 -n 5 # arguments go to the widget
./start.py link --help        # including --help
```

The first form works because of `__main__.py`, which is Python's own
convention for an entry point: a directory containing one can be run by
name. It holds three lines and hands straight over to this script, so the
collection can be started without knowing which file inside it to name.

That form uses `exec`, so the launcher replaces itself rather than sitting in
the middle of a pipeline it adds nothing to.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | select a widget |
| `↵` | launch it, and come back here when it quits |
| `r` | recheck what is installed and configured |
| `q` | quit |

## Cost

One pass over the directory at startup and on `r`: a parse of each file for
its docstring, a read of the README, a `which` per required command. Nothing
runs in the background, and nothing is polled.
