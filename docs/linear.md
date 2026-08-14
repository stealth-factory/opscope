# `linear.py`

Linear across every team at once — what is outstanding, which cycles are
running, and whether issues are being closed faster than they arrive.

```
╺━ LINEAR OPS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 14 teams   updated 1m ago   2490 req left/hr
 ── OPEN ── 1221 issues open   (any age)
 █████████████████████████████████████████████████████████████████████████████████
 ▇ triage 704 (58%)   ▇ backlog 314 (26%)   ▇ todo 155 (13%)   ▇ in progress 48 (4%)

 ── ACTIVE CYCLES ── 11 running, showing 6
 LAB Cycle 3       █████████░░░░░░░░░░░░░░░░░░░ 33%  1/3 pts  2d left
 WEB Cycle 3       ████████████████████░░░░░░░░ 72%  205/284 pts  2d left  +175 added
 SIT Cycle 3       ░░░░░░░░░░░░░░░░░░░░░░░░░░░░  0%  0/6 pts  2d left
 OPS Cycle 111     ░░░░░░░░░░░░░░░░░░░░░░░░░░░░  0%  0/8 pts  2d left
 STU Cycle 46      ░░░░░░░░░░░░░░░░░░░░░░░░░░░░  0%  0/63 pts  2d left
 MED Cycle 16      ░░░░░░░░░░░░░░░░░░░░░░░░░░░░  0%  0/1 pts  9d left

 ── ISSUE FLOW ── 14d · ▲ 297 created · ▼ 88 completed   peak 66/day
           ▁▁▁▁      ▇▇▇▇                ████
           ████ ▃▃▃▃ ████                ████
 ▄▄▄▄ ▅▅▅▅ ████ ████ ████ ▆▆▆▆ ▂▂▂▂ ▂▂▂▂ ████      ▁▁▁▁ ▄▄▄▄ ▄▄▄▄ ▁▁▁▁
 ─────────────────────────────────────────────────────────────────────
                ▀▀▀▀ ████


 14d ago                                                         today

 ── HOW LONG ── median over 88 completed
  lead time 10.9h   created → completed     cycle time 3.7h    started → completed

 ── BY TEAM ──   1-8 of 14
 TEAM                    OPEN TRIAGE   DOING DONE14D
▸OPS  Ops Intake          703    675       0       0
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

**Open** — one bar over everything outstanding at any age, split triage /
backlog / todo / in progress. Not windowed: it answers "how much is there right
now", and does not move when you change the window.

**Active cycles** — every team's running cycle, from a single query. Progress
bar, points completed against scope, and days remaining.

`+175 added` is the number to watch: scope added *after* the cycle opened. A
cycle can be worked hard and still slip, and this is the column that says which
of the two happened. Cycles are ordered by the soonest to end, with any that
have nothing scoped sorted last — an empty cycle is an open window, not work in
flight, and letting one sit above a cycle 72% done two days from its deadline
buries the row worth reading.

**Issue flow** — the diverging chart: created growing up, completed growing
down, one bar per day, both directions on a shared scale. Read together they
say whether the queue is filling faster than it drains. In the board above, 297
created against 88 completed.

**How long** — median lead time (created → completed) and median cycle time
(started → completed) over everything finished in the window. Medians, because
one issue that sat open for a month drags a mean somewhere unrepresentative.

**By team** — ranked by open volume, scrolling under `↑` `↓` when there are more
teams than rows. `DONE14D` follows the window.

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

| Key | Action |
|---|---|
| `↑` `↓` | select a team |
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
