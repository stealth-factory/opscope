# `github.py`

Pull requests across every org you work in — not what shipped, but whether work
is actually moving.

```
╺━ GITHUB OPS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 9 accounts   updated 0s ago   4891/5000 api
 ── OPEN PR STATE ── 682 PRs · 217 issues open   (any age)
 ████████████████████████████████████████████████████░░░░░░░░░░░░░░░░░
 ▇ awaiting review 486 (71%)   ▇ ready to merge 180 (26%)   ▇ draft 16 (2%)

 ── MERGE RATE ── last 7 days
 81%  ████████████████████████████████████████░░░░░░░░░  17 merged / 4 dropped

 ── PR FLOW ── 7d · ▲ 30 opened · ▼ 17 merged   peak 19/day
            ██████████
            ██████████
 ▃▃▃▃▃▃▃▃▃▃ ██████████ ▂▂▂▂▂▂▂▂▂▂            ▁▁▁▁▁▁▁▁▁▁ ▆▆▆▆▆▆▆▆▆▆
 ─────────────────────────────────────────────────────────────────────
 ▀▀▀▀▀▀▀▀▀▀                                             ██████████
 7d ago                                                          today

 ── CONTRIBUTIONS ── 6010 in 52 weeks, peak 241/day
 Mon   ░▒░▒░ ░▒░           ░     ░   ░ ░ ░ ░ ░░░░░░░▒▓░
       ░▒░░░ ▒▓         ░        ░░ ░░░ ░░ ░▒░ ░░░░░▒▒░
 Wed  ░░▒░░▒░░▒░      ░    ░     ░░  ▒░░░░ ░░░░░ ░▒█▓▒░

 ── BY ACCOUNT ──
 ACCOUNT               OPEN  REVW  MRG7D   RATE ISSUES
▸stealth-factory        628   486     15    83%    159  ████████████
 hk2047                   0     0      0    --       0  ░░░░░░░░░░░░
 wiiiimm (you)           34     0      2    67%     28  █░░░░░░░░░░░
```

## What is windowed and what is not

Worth holding onto, because the two kinds of number answer different questions:

- **Point-in-time**, at any age: open PRs, open issues, drafts, review backlog.
  "How much is outstanding right now." This is the top section, and it never
  changes when you change the window.
- **Windowed** — everything else: the merge rate, the PR flow chart, and the
  per-account `MRG*D` and `RATE` columns. "How did the last N days go."

The window is **N days ending today**, and both the aggregate and the chart use
exactly that span. They are drawn next to each other, so an off-by-one would be
plainly visible: the flow chart's `▼ merged` total equals the merge rate's
merged count, always.

## Sections

**Open PR state** — one bar over every open PR, split into awaiting review /
ready to merge / draft. The review backlog is usually the number that explains a
falling merge rate. It leads the board because it is the question asked most
often.

**Merge rate** — of the PRs that *closed* in the window, the share that merged,
on the same green→amber→red ramp as everything else. `dropped` means closed
without merging; GitHub's `is:closed` includes merged ones, which is why the two
are counted separately rather than subtracted.

**PR flow** — one diverging chart: PRs opened grow up in purple, PRs merged grow
down in green, from a shared baseline. Read together they answer whether the
queue is filling faster than it drains. **Both directions share one scale**, or
the comparison would lie, and the heading names the peak that scale represents.

The chart always fills the pane: where there is room to spare a day takes
several columns with a gap between bars, and where there is not, the oldest days
are cropped and the heading says so — `54d of 90d` — because the totals describe
what is drawn, not the whole window.

**Contributions** — the familiar GitHub calendar, a full 52 weeks, in braille
shading, with the numbers underneath it that the squares cannot show: current
and longest streak, today's count, how many days of the year were active, the
single busiest day, and which weekday carries the most work. They lay out in
three columns, two, or one as the width allows.

A streak counts consecutive days with at least one contribution, the way
github.com does it — a day that has scored nothing *so far* does not break the
current streak, because it is not over yet.

This is the one decorative section, so it is skipped entirely in a short pane to
leave the account table its rows.

**By account** — one row per org, with a bar for relative size. `↑` `↓` selects
one; the selected row is tinted.

## How the per-day counts stay exact

Worth recording, because the obvious implementation is wrong.

The natural approach is to fetch PRs and bucket their timestamps. But a GitHub
search connection returns **at most 100 nodes per page**, so any account merging
more than 100 PRs in the window loses everything past the hundredth — and since
the merged query sorted by update time, those hundred were not even the hundred
most recently merged. The chart looked plausible and undercounted.

Instead each day is asked for its own count:

```graphql
m0: search(query:"org:acme is:pr is:merged merged:2026-08-01", type:ISSUE) { issueCount }
c0: search(query:"org:acme is:pr created:2026-08-01",          type:ISSUE) { issueCount }
```

`issueCount` is a server-side aggregate — exact at any volume. Aliased searches
cost **one rate-limit point per request** no matter how many are packed into it,
so this is close to free; probing found the alias ceiling between 60 and 90, so
days go out in chunks of 20.

Verified across all nine accounts: 17 merged at 7d and 110 at 14d, counted both
by summing the days and by the aggregate, identical each way.

## Fetch order and loading state

A cold 90-day window is around fifty requests, while the headline figures are
one request per account. So **aggregates are fetched first, for every account,
before any per-day work** — the merge rate and open state are live within
seconds while the chart is still counting.

The two therefore go stale independently, and each says so rather than showing a
number it cannot justify:

- Changing the window leaves the previous window's figures wrong-but-plausible,
  so windowed figures show a grey shimmer until real numbers land.
- Rows carry the window they were fetched for, so an account already refetched
  shows real numbers while the ones behind it still shimmer.
- The flow chart's totals are only shown once **every** account has reported for
  the current window — summing a half-updated board would add two windows
  together. Until then the heading reads `counting 90d…`.

## Cost

Per refresh: one request per account for the aggregates, one per 20 days of
history *not already cached*, and one for the contribution calendar. Each costs
a single point against the 5000/hour GraphQL budget.

A past day's counts cannot change — a PR merged on the 3rd stays merged on the
3rd — so days are cached per account and only the trailing two are refetched.
**Widening the window costs only the days it adds; narrowing costs nothing.**
Steady state across nine accounts is ~18 points per refresh, or ~540/hour at the
default 120s. A cold 90d window is a one-time ~54.

`r` drops the day cache and re-reads everything, which is the escape hatch for
the cases immutability does not cover — a repo deleted, transferred or made
private. `m` has no need to.

Accounts are fetched one at a time rather than batched: results appear as they
arrive, and one bad account cannot blank the whole board. Batching every account
into a single request returned HTTP 502 on the complexity limit.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` | select an account |
| `m` | cycle the merge window — 7 / 14 / 30 / 90 days |
| `r` | refresh now, ignoring the day cache |
| `q` | quit |

## Credentials

`github.token` in `config.json`, or `$GITHUB_TOKEN`. A **classic personal access
token** with `repo` and `read:org` covers private repositories and org
discovery.

**The `gh` CLI is deliberately not used** — the API is called directly so the
widget carries no dependency on another tool being installed and authenticated.

`config.json` holds a secret once you put a token in it; `chmod 600` it. The
file is git-ignored and the token is never printed.

## Configuration

```json
"github": {
  "token": "",
  "token_env": "GITHUB_TOKEN",
  "accounts": [],
  "window_days": 7,
  "refresh": 120
}
```

Empty `accounts` discovers every org you belong to plus your personal account;
otherwise list org logins, and `@me` for your own. `window_days` sets the window
the board opens on; `m` cycles it from there.

```sh
./github.py                        # discovered accounts, 120s
./github.py -n 300 acme @me        # two accounts, slower
```
