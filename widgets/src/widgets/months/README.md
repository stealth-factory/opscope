# `months`

[← all widgets](../../../../docs/README.md)

A month grid you can page through, with today in context — and nothing on it
that came off a network.

```
╺━ MONTHS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 2026-08-31 MONDAY · reckoned on this machine (UTC+8)
 weeks start Sunday · wk is the ISO 8601 week, Monday-reckoned

 JULY 2026                   AUGUST 2026
  wk Su Mo Tu We Th Fr Sa     wk Su Mo Tu We Th Fr Sa
  27 28 29 30  1  2  3  4     31 26 27 28 29 30 31  1
  28  5  6  7  8  9 10 11     32  2  3  4  5  6  7  8
  29 12 13 14 15 16 17 18     33  9 10 11 12 13 14 15
  30 19 20 21 22 23 24 25     34 16 17 18 19 20 21 22
  31 26 27 28 29 30 31  1     35 23 24 25 26 27 28 29
                              36 30 31  1  2  3  4  5
                              37  6  7  8  9 10 11 12
                              38 13 14 15 16 17 18 19

 SEPTEMBER 2026
  wk Su Mo Tu We Th Fr Sa
  34 16 17 18 19 20 21 22
  35 23 24 25 26 27 28 29
  36 30 31  1  2  3  4  5
  37  6  7  8  9 10 11 12
  38 13 14 15 16 17 18 19
  39 20 21 22 23 24 25 26
  40 27 28 29 30  1  2  3

 ←→ month  ↑↓ year  [t]oday  [,] settings  [q]uit
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
were already drawn dimmed. The picture above shows it working in both
directions at once: today is 31 August, which falls in the *last* row of an
ordinary August, so August has grown two weeks downward — and it is the
*first* row of September's grid, so September has grown two weeks upward and
opens on week 34. July does not contain today at all and draws as a plain
five-row month.

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
across on a wall.

**The month you have paged to sits in the middle, with the one before it and
the one after either side.** A date is checked against the month just gone at
least as often as the month ahead — "was that the 3rd or the 10th" — and the
strip used to begin at the month in view and only go forward, so the one
behind was always a keypress away and never on screen. It is the same
argument `CONTEXT` makes a row at a time, made a month at a time.

Coming down, the week-number gutter goes before a day does — a week number is
worth less than the seventh day of the week — and below 23 columns there is no
honest grid to draw at all, so the pane says that instead of drawing six days
and a cut edge. Nothing is ever truncated: the reckoning lines wrap, and so
does the footer.

**Those three are a floor, not a cap.** A pane too narrow to hold them side by
side stacks them instead of dropping them, and the body scrolls to reach the
rest — `Ctrl-E` and `Ctrl-Y`, or the wheel. Dropping a month because the pane
is narrow is the one thing this collection does not do: a month that is not
drawn looks exactly like a month with nothing in it.

Each band keeps the same fixed height whether its months run to five weeks or
eight, so paging never shifts what is below it.

**`s` drops to this month alone**, and back. It stays one month however wide
the pane is — filling the width with neighbours would be the widget declining
to do the thing the key was pressed for. The footer names where the key goes
rather than what is on, `[s]ingle` against `[s]pread`, because a footer
reading "single" while a single month is showing says nothing about which way
it moves. It is a view and not a setting: something you want for a moment,
not something you configure.

## Keys

| Key | Action |
|---|---|
| `←` `→` | page back and forward a month — `h` and `l` do the same |
| `↑` `↓` | a year back and forward — `k` and `j`, and `PgUp` / `PgDn` |
| `t` | back to the month it is now — `Home` does the same |
| `s` | this month on its own, and back again — the footer says which way it goes |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the view a line at a time, as in vim |
| `,` | open settings — the week start, the week numbers, and the zone, on the shared screen |
| `q` | quit |

Paging has no ceiling of its own. It stops where the calendar does — some
262,000 years either way, which is the range of the date type underneath — and
there it holds still rather than wrapping into a year that would draw as a
perfectly ordinary month somewhere else.

## Configuration

Press `,` for the settings screen, or edit `config.json` yourself — the two
write the same file. All three fields are declared in
[`settings.json`](settings.json) beside this page, which is also where
`config.example.json` is generated from, so the example cannot drift from
what the widget reads.

```json
"months": {
  "week_start": "sunday",
  "week_numbers": true,
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

**`week_numbers`** — whether the ISO week gutter is drawn at all. On by
default. Turning it off is not only a column back: a narrower month means
**more months fit across the same pane**, which is why it is decided before
the width is divided up rather than blanked out afterwards. It is still
dropped on its own when the pane cannot hold it and seven days both — the
gutter goes before a day does — so this key says whether you want it, not
whether there is room.

It is config rather than a key because it is not a thing you flip while
reading. ISO week numbers are either part of how you work or they are noise.

**`timezone`** — an IANA name such as `Asia/Tokyo`. Empty means this machine's
own zone, which is what `clocks` shows. A name the database does not know
falls back to this machine and says so on the pane.

`↵` on that row opens **the same searchable list of zones `clocks` uses** —
all 597 of them, filtered as you type — rather than asking for a name to be
typed from memory beside a screen that already holds every one. The
difference from `clocks` is only what choosing does: `clocks` collects
cities, so a zone is added or removed; this holds one, so a zone replaces
whatever was there and the screen closes. Nothing declares that in
`settings.json` — the shape of the field decides it, so the two cannot
disagree.

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
