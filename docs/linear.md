# `linear.py`

Linear across every team at once — what is outstanding, which cycles are
running, and whether issues are being closed faster than they arrive.

```
╺━ LINEAR OPS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 14 teams   updated 1m ago   2490 req left/hr
 ── HOW LONG ── all teams · median of 88 completed in 14d
  lead (created→completed)  10.9h              cycle (started→completed) 3.7h
  quickest                  APP-52 18m         slowest                   WEB-44 81.9d
  oldest open               STU-97 2.8y        oldest in progress        STU-112 1.9y

 ── OPEN ── 1221 issues open   (any age)
 █████████████████████████████████████████████████████████████████████████████████
 ▇ triage 704 (58%)   ▇ backlog 314 (26%)   ▇ todo 155 (13%)   ▇ in progress 48 (4%)

 ── ACTIVE CYCLES ── 11 running   ↑↓ 1-6 of 11
▸WEB Cycle 3       ████████████████████░░░░░░░░ 72%  205/284 pts  2d left  +175 added
 CLI Cycle 16      ████████████░░░░░░░░░░░░░░░░ 43%  3/7 pts  9d left  +3 added
 LAB Cycle 3       █████████░░░░░░░░░░░░░░░░░░░ 33%  1/3 pts  2d left
 SIT Cycle 3       ░░░░░░░░░░░░░░░░░░░░░░░░░░░░  0%  0/6 pts  2d left
 OPS Cycle 111     ░░░░░░░░░░░░░░░░░░░░░░░░░░░░  0%  0/8 pts  2d left
 STU Cycle 46      ░░░░░░░░░░░░░░░░░░░░░░░░░░░░  0%  0/63 pts  2d left

 ── ISSUE FLOW ── 14d · ▲ 297 created · ▼ 88 completed   peak 66/day
           ▁▁▁▁      ▇▇▇▇                ████
           ████ ▃▃▃▃ ████                ████
 ▄▄▄▄ ▅▅▅▅ ████ ████ ████ ▆▆▆▆ ▂▂▂▂ ▂▂▂▂ ████      ▁▁▁▁ ▄▄▄▄ ▄▄▄▄ ▁▁▁▁
 ─────────────────────────────────────────────────────────────────────
                ▀▀▀▀ ████


 14d ago                                                         today

 ── BY TEAM ──   1-8 of 14
 TEAM                    OPEN TRIAGE   DOING DONE14D
 OPS  Ops Intake          703    675       0       0
 STU  Studio              140     22      14       0
 WEB  Web App             125      1      20      75
 LIB  Libraries           105      6       3       0
 APP  Mobile App           59      0       8       9
```

## Triage is counted apart from the backlog

The one interpretation decision worth stating up front. An automated intake
queue and a groomed backlog are different populations, and a workspace with an
alerting integration will have far more of the former than the latter — 58% of
everything open, in the board above. Adding them together produces a number
that describes neither, so `triage` gets its own colour in the bar and its own
column in the table.

Issues in `completed`, `canceled` or `duplicate` states are not "open" and are
excluded at the query, so the total counts work that is genuinely outstanding.

## Sections

**How long** — median lead time (created → completed) and median cycle time
(started → completed) over everything finished in the window. It leads the
board because it is the one figure that says whether the machine is getting
faster or slower.

The heading says **all teams** deliberately: it is an aggregate over the whole
workspace, not the team highlighted in the table below, and without saying so it
would read as the latter. Medians rather than means, because one issue that sat
open for a month drags a mean somewhere unrepresentative.

Every figure in the section sits on one grid, two columns wide where the pane
allows and one where it does not, with each cell a fixed width so a long
identifier cannot push the next column out of line. The `created→completed`
and `started→completed` definitions ride with the labels they define rather
than trailing after the values.

Beneath the medians are the four extremes, each **naming the issue** rather than
just the number — a median describes the distribution, these tell you where to
go and look:

| | |
|---|---|
| `quickest` / `slowest` | fastest and slowest lead time among issues *completed* in the window. History: they say how wide the spread is behind that median. |
| `oldest open` | the issue that has been open longest, at any age. |
| `oldest in progress` | the issue that has been *started* longest without finishing. |

The last two are the actionable pair, and they are the reason this is worth
showing: an issue in progress for `1.9y` is not in progress. Durations roll over
to years because these figures reach them — `1021.6d` is arithmetic, `2.8y` is a
decision.

**Open** — one bar over everything outstanding at any age, split triage /
backlog / todo / in progress. Not windowed: it answers "how much is there right
now", and does not move when you change the window.

**Active cycles** — every team's running cycle, from a single query. Progress
bar, points completed against scope, and days remaining.

`+175 added` is the number to watch: scope added *after* the cycle opened. A
cycle can be worked hard and still slip, and this is the column that says which
of the two happened.

**Cycles are ordered by how much is actually moving in them**, not by deadline.
The burndown arrays already answer that: day-over-day movement in completed
scope and in scope itself, summed over the last six entries. A cycle nobody has
touched in a week is not where the action is however close its end date, and an
empty cycle scores zero and sinks without needing a special case. The deadline
breaks ties.

**Issue flow** — the diverging chart: created growing up, completed growing
down, one bar per day, both directions on a shared scale. Read together they
say whether the queue is filling faster than it drains. In the board above, 297
created against 88 completed.

**By team** — ranked by open volume, windowing around the cursor when focused
and there are more teams than rows. `DONE14D` follows the window.

## Cost

Linear allows **2,500 requests/hour** and 3,000,000 complexity points; a single
query may not exceed 10,000. Complexity is 0.1 per property and 1 per object,
multiplied by the page size, so the request count is the limit that binds and
the field count barely matters.

A full pass over a workspace of 14 teams and ~1,200 open issues costs about
**10 requests and 4 seconds**, so the default 120s refresh uses roughly 300
requests an hour — an eighth of the budget. Remaining quota is read from
`X-RateLimit-Requests-Remaining` and shown in the header.

Linear's connections expose no `totalCount`, so anything counted has to be
paged through at 250 records a time. Pagination is capped at 12 pages per query
and the header says `truncated` when the cap is reached, rather than quietly
reporting a smaller number.

## Keys

The board opens with no cursor anywhere — it is a thing to read before it is a
thing to work. `tab` focuses a pane, and the focused heading says so by
carrying the `↑↓` marker and its visible range; `↑` `↓` then move a cursor
through that pane, which windows itself around it.

Under the arrows the two panes read as **one continuous list**: `↓` off the
bottom of the cycles steps into the top of the teams, and `↑` off the top of
the teams steps into the *bottom* of the cycles. `tab` is the shortcut across
a whole pane rather than the only way between them.

You let go at exactly two places: `↑` from the first cycle, and `↓` from the
last team. `tab` from the last pane does the same. Panes with nothing in them
are stepped over in every direction.

**This is the same rule in every widget here that has focusable sections.**

| Key | Action |
|---|---|
| `tab` | focus the next pane, and from the last one back to no focus |
| `↑` `↓` | move the cursor, crossing between panes at their ends — or step into one when none is focused |
| `w` | cycle the window — 7 / 14 / 30 / 60 / 90 days |
| `r` | refresh now |
| `q` | quit |

## Credentials

`linear.token` in `config.json`, or `$LINEAR_API_KEY`. Create a **personal API
key** at Settings → Security & access → Personal API keys. It is sent in the
`Authorization` header directly — no CLI or SDK is involved.

`config.json` holds a secret once you put a key in it; `chmod 600` it. The file
is git-ignored, the key is never printed, and the widget warns if the file is
readable by others.

## Configuration

```json
"linear": {
  "token": "",
  "token_env": "LINEAR_API_KEY",
  "exclude_teams": [],
  "window_days": 14,
  "refresh": 120
}
```

Nothing needs choosing for the board to work: every team, cycle and issue is
queried workspace-wide, and where a "which one?" would arise the answer is
shown as rows rather than asked as a question.

`exclude_teams` exists for noise rather than function. A team fed by an
automated integration can hold more issues than every human team combined, and
dropping it is sometimes the difference between a readable board and one number
swamping the rest.

```sh
./linear.py                   # every team, 14-day window
./linear.py -n 300 WEB APP    # two teams by key, slower
```
