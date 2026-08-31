# `months`

[← all widgets](../../../../docs/README.md)

A month grid you can page through, with today in context — and nothing on it
that came off a network.

```
╺━ MONTHS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 2026-08-29 SATURDAY · reckoned on this machine (UTC+0)
 weeks start Sunday · wk is the ISO 8601 week, Monday-reckoned

 AUGUST 2026                 SEPTEMBER 2026
  wk Su Mo Tu We Th Fr Sa     wk Su Mo Tu We Th Fr Sa
  31 26 27 28 29 30 31  1     36 30 31  1  2  3  4  5
  32  2  3  4  5  6  7  8     37  6  7  8  9 10 11 12
  33  9 10 11 12 13 14 15     38 13 14 15 16 17 18 19
  34 16 17 18 19 20 21 22     39 20 21 22 23 24 25 26
  35 23 24 25 26 27 28 29     40 27 28 29 30  1  2  3
  36 30 31  1  2  3  4  5
  37  6  7  8  9 10 11 12

 ←→ month  ↑↓ year  [t]oday  [q]uit
```

Today is the lit square; the week it is in is the lit number in the gutter.
Days belonging to the months either side are dimmed. `clocks` owns the time of
day — the server clock, the countdowns, the pomodoro, the world clock — and
this owns dates. They meet only at the header line `clocks` already prints.

## Why the month is taller than a month

At least two weeks of context either side of the current week is one of the
things this was built for, and a fixed month grid cannot promise it: today in
the top row has no weeks above it, and today in the bottom row none below.

So the grid grows. Leading or trailing weeks are added until there are two
either side of the row today is in — seven or eight rows instead of five or
six — and they need no new visual language, because days outside the month
were already drawn dimmed. In the picture above, August has grown by one week
at the bottom: today is 29 August, in the second-to-last row of an ordinary
August, and a week has been added under it.

Two decisions came with that, and both are load-bearing:

**Only the month today is in grows.** Once you have paged away, "two weeks
either side of the current week" means nothing, so every other month draws as
a plain grid with its ordinary spill. The rule is *today is on screen*, not
*this is today's month* — so a today that appears in a neighbour's trailing
spill, as 1 August does in July's last row, gets its context there too.

**The grid area keeps its height.** Eight rows, always: the tallest the
extension can ever need is a six-week month with today at one end of it, and a
month that uses five leaves the rest blank. A pane whose footer walks up and
down while you page is a pane you stop reading, and blank rows under a finished
month hide nothing — the month has ended, and there is nothing below it to see.

## The week number says what it counts

Week numbers are **ISO 8601**: weeks are reckoned from Monday, and week 1 is
the one holding the year's first Thursday. The pane says so, next to the
`wk` heading, on every frame that has a gutter to explain.

It has to, because the grid can start on Sunday — and then the number beside
a row belongs to a week that began a day earlier. Most of the year nobody
would notice. At the turn of a year the two genuinely disagree: the
Sunday-start row from 27 December 2026 to 2 January 2027 opens on a Sunday
that ISO counts in week 52 and spends its remaining six days in week 53.

Each row is numbered from the **Thursday** in it, which is the one day of the
row that sits in the same ISO week whichever day the grid begins on. Numbering
from the row's own first day would name the week before for six days out of
seven in exactly the case above. What is not on offer is an unlabelled number
that changes meaning when `week_start` changes.

## Which day is "today"

Dates are reckoned on this machine's zone unless `timezone` names another, and
the second line says which. That is not ceremony: for several hours of every
day it is a different date in two places, so a widget quietly using UTC on a
machine at UTC+8 would mark the wrong square every evening — which is the same
reason `clocks` exists.

A `timezone` the database does not know falls back to this machine's zone
**and says so on the pane**. Silently substituting a zone would leave every
square looking like an answer to a question nobody asked.

## Width

Extra width buys months rather than margins: two side by side at the sixty to
seventy columns these panes usually get, three from ninety, and up to a year
across on a wall. The first month is the one you have paged to and the rest
follow it.

Coming down, the week-number gutter goes before a day does — a week number is
worth less than the seventh day of the week — and below 23 columns there is no
honest grid to draw at all, so the pane says that instead of drawing six days
and a cut edge. Nothing is ever truncated: the reckoning lines wrap, and so
does the footer.

## Keys

| Key | Action |
|---|---|
| `←` `→` | page back and forward a month — `h` and `l` do the same |
| `↑` `↓` | a year back and forward — `k` and `j`, and `PgUp` / `PgDn` |
| `t` | back to the month it is now — `Home` does the same |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the view a line at a time, as in vim |
| `,` | open settings — the week start and the zone, on the shared screen |
| `q` | quit |

Paging has no ceiling of its own. It stops where the calendar does — some
262,000 years either way, which is the range of the date type underneath — and
there it holds still rather than wrapping into a year that would draw as a
perfectly ordinary month somewhere else.

## Configuration

Press `,` for the settings screen, or edit `config.json` yourself — the two
write the same file. Both fields are declared in
[`settings.json`](settings.json) beside this page, which is also where
`config.example.json` is generated from, so the example cannot drift from
what the widget reads.

```json
"months": {
  "week_start": "sunday",
  "timezone": ""
}
```

**`week_start`** — `sunday` or `monday`. Sunday by default, in the code and in
the generated example both. It moves where the row breaks and nothing else:
the week numbers stay ISO, and the weekend stays Saturday and Sunday, which is
what `clocks` reckons too. The two answers are declared as the field's
`choices`, so `↵` offers them rather than asking for one to be typed — and
anything unrecognised that reaches the file by hand is the default rather than
a stopped panel.

**`timezone`** — an IANA name such as `Asia/Tokyo`. Empty means this machine's
own zone, which is what `clocks` shows. A name the database does not know
falls back to this machine and says so on the pane.

Writing anything here restarts the widget on the way out of the screen, so a
new week start or zone is on the grid immediately: a running widget holds the
config it launched with, and the shared screen does the relaunch rather than
asking the reader to.

There is an AI-facing guide beside this page as well —
[`CONFIGURE.md`](CONFIGURE.md), also reachable as
`opscope months --configure-help` — which is mostly about what not to guess:
a week start is a preference to ask about, and a zone must never be inferred
from an IP address.

## What it does not do

No events, and no calendar feed. That keeps it credential-free, which is most
of why it is small — but the day cell is three columns wide and only two of
them hold the number, so the spare one is there for a marker if a feed is ever
wired in. That would be an addition rather than a relayout.

Nothing here writes anything, either. The widget that does is a separate one.
