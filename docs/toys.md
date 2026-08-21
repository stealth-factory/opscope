# `toys.py`

The front door: every widget, what it does, and whether it will work on this
machine.

```
╺━ TERMINAL TOYS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 13 widgets · 11 ready here   ↵ launches one, q leaves

 ▸ netwatch     Which processes are using the network, how much…   ready
   ports        What is listening on this machine, what started…   ready
   github       GitHub delivery metrics across every org…          set a GitHub token
   latency      Multi-target latency monitor.                      needs ping
   tailnet      Tailscale network: who is online…                  ready
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

## Whether it will run

The right-hand column is this machine, not the general case.

A widget that needs a **command** either has it or does not, and that is
worth saying plainly: `needs ping` means exactly that. A widget that needs a
**token** is softer — it might be somewhere the launcher cannot see — so a
missing one reads `set a GitHub token`, a thing to do rather than a failure.
`pr.py` has no token of its own by design, reusing GitHub's rather than
asking for a second, so it reports GitHub's readiness.

`usage.py` says `reads what is logged in`, because its requirement is not one
credential but whichever agents happen to be signed in on this machine — and
the widget itself is the thing that knows. Claiming either way from out here
would be a guess.

**Nothing is hidden for failing a check.** A widget you cannot run yet is
still worth knowing exists, and the line says what is missing instead of
disappearing.

## Launching

`↵` hands the terminal over: cursor restored, raw mode off, the widget gets a
normal terminal and this process waits. Quit the widget and the launcher
takes the terminal back and rechecks, so a token you set or a package you
installed while you were away is reflected without restarting.

Naming one skips the menu entirely, and anything after it is passed straight
through:

```sh
./toys.py                    # the menu
./toys.py netwatch           # straight into one
./toys.py netwatch -i 2 -n 5 # arguments go to the widget
./toys.py link --help        # including --help
```

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
