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

## Whether it will run

The right-hand column is this machine, not the general case, and it says
**what was actually checked** rather than a verdict. There is no "ready",
because "ready" would mean three different things:

| It says | It checked |
|---|---|
| `nothing to set up` | the widget declares no requirement at all |
| `ss installed` | every command it named is on `PATH` |
| `needs ping` | one is not, and that is its name |
| `token is set` | a token is present, in config or the named environment variable |
| `set a GitHub token` | none was found in either |
| `reads what is logged in` | nothing — see below |

The distinction matters most for tokens. **`token is set` is not `the token
works`.** Nothing here spends a credential to find out whether it has expired
or is missing a scope — a GitHub PAT without `read:org` will pass this check
and still show you half a board. The line says a token exists, because that
is the only thing that was established.

Nor is anything executed. A command being on `PATH` is not that command
working; `tailscale installed` says the binary is there, not that the daemon
is up or you are logged in.

`pr.py` has no token of its own by design, reusing GitHub's rather than
asking for a second, so it reports GitHub's.

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
