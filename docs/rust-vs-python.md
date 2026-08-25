# Where the Rust and the Python differ

Every widget here exists twice. The Rust is not a transliteration: some of it
answers differently on purpose, and the difference between *on purpose* and
*a defect the port introduced* is the whole point of the side-by-side review
([TOY-8](https://linear.app/stealth-company/issue/TOY-8)). This page is the
durable half of that review — the divergences that were meant.

Anything not listed here and not obviously a Rust-only feature should be
treated as a finding, not a decision.

**Reviewed against `ac02b90`.** Verified by reading both sources, not by
diffing them: three attempts at a mechanical key-differ each reported
something different and each was wrong in its own way — one missed
`if key == "f"` because it only read match arms, one counted `for key in
("send", ...)` as a keyboard key, and one missed `key in ("enter", "right",
"i")` because it only looked at the first element. That is the usual lesson
here: a grep that finds nothing is as often a wrong pattern as an absent
thing.

## One way in, and one way out

Seven widgets have a drill-in view and no two of them agreed on how to reach
it. `i` opened one in three of them, `c` opened another, `↵` alone opened
two, and coming back was `esc`, or `backspace`, or `q`, or `↵` again.

**The Rust: `→` or `↵` in, `←` or `esc` out, everywhere.** No letter is
spent on it.

Which means the Rust deliberately does *not* answer:

| Key | Still opens a detail view in | Replaced by |
|---|---|---|
| `i` | `ports.py`, `link.py`, `netwatch.py`, `start.py`, `tailnet.py` | `→` / `↵` |
| `backspace` | `ports.py`, `netwatch.py` | `←` / `esc` |

`i` was never in any footer in the Python either, in any of the five. It
worked and nothing said so.

`q` quits from inside a detail view rather than closing it. In the Python it
closes the overlay in four widgets while the footer beside it reads
`[q]uit` — the key disagreeing with its own hint, and quietly, because the
widget stays up.

**`usage` is untouched.** Its `←` and `→` move between vendor tabs, which is
lateral rather than into anything.

**`pr` is a special case.** Inside its detail `↵` walks the stack, so `→`
drills further rather than closing, and `esc` keeps its second job of
clearing the search, which `←` has no business doing.

## Sections are focused with tab, not with a letter each

`netwatch.py` gives each of its three lists a key: `e` focuses endpoints,
`f` focuses files, `tab` cycles. The Rust has **only `tab`**, plus the rule
every widget here with focusable sections now follows: the sections read as
one continuous list under the arrows, crossing at their ends, letting go at
exactly two places.

The heading of the section `tab` would focus *next* carries `[tab] to
focus`. The Python's headings carry the per-section letters instead.

## Keys renamed, and why

| Widget | Python | Rust | Why |
|---|---|---|---|
| `tailnet` | `n` cycles the interval | `i` cycles the interval | `i` is what `latency` calls the same thing, and `i` was free once the info screen moved to the arrows. `n` was this widget's own letter and meant nothing to a reader coming from the widget beside it. |
| `deployments` | `f` filters by project | `s` filters by state, `/` filters by text | Two different filters wanted the same letter. The one you type is `/`, as everywhere else that has one. |
| `herdr-panes` | `o` | `i` | Named after what it toggles. |

`clocks`, `latency` and `deployments` also answer `j` and `k` as aliases for
`↓` and `↑` in the Rust only.

## Charts

**`latency` and `link` draw on a braille canvas** in the Rust, the way
`netwatch` always has — two dots to a character across and four down, with
consecutive samples joined. The Pythons plot one glyph per sample and fill
`│` between the steps, so a value that moves quickly reads as a column of
marks rather than a line. The side effect worth having is resolution: a cell
that used to hold one sample holds two.

**A braille cell belongs to one trace.** Both Pythons merge every series' dot
masks into each cell and give the cell to whichever series comes later in the
table. A cell can hold two traces' dots but only one colour, so where two
hosts sit close together on the axis one is drawn end to end in the other's
colour and a third can vanish as a distinct line. No number is false; the
colour saying whose it is, is. **The Pythons still do this.**

**`netwatch` averages rates over about four seconds** in the Rust. Over one
sample interval the delta really is zero whenever a bursty process is between
bursts, so the column flickered between a figure and a dash — every reading
correct and the column unreadable. The header names the window. Totals are
untouched: smoothing a rate is honest, smoothing a total would not be.
`netwatch.py` still flickers.

## Columns

| Widget | Difference |
|---|---|
| `netwatch` | The connection list has a **LOCAL** column in the Rust — our end of the socket. Without it, several connections to one host and port are identical rows, and the only field telling them apart is not on screen. |
| `ports` | **WHAT** is sized to the widest name it has to show, in both, since the review; it was a flat eighteen cells with nothing after it and a name of exactly that length ran into the project name. |

## Rust-only features

Built after the port, on the Rust side only, because that is where the work
went once the ports were accepted:

| Widget | What |
|---|---|
| `linear` | A screen of its own for a cycle, a team, and a project; the board scrolls as a whole; a PROJECTS section; `[c]opy url` on a cycle's issues |
| `ports` | Per-port traffic — a column, a per-row sparkline, a chart on the port's screen and one across the top |
| `deployments` | Build logs, a scrolling detail screen, `[/]` filter, copy on its own page |
| `github` | A per-account screen with the oldest open PRs and `[c]opy` |
| `matrix` | Answers `q`. `matrix.py` has no keyboard at all and exits on Ctrl-C |

## Where the Rust deliberately agrees

Worth writing down because it was checked rather than assumed: subprocess
timeouts are the Pythons' own numbers, config defaults were compared key by
key against the Pythons', and `ports`' program-name table matches the
Python's patterns — including the two that have to be anchored, which the
port had flattened to substring tests until the review caught it.

## Still open

- `ports.py` answers `i` and the Rust does not. Dropping it from the Python
  is a decision rather than a fix.
- The braille colour bug and `netwatch`'s rate window are both worth
  backporting if the Python is staying.
