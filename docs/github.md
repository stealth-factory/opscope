# `github.py`

Pull requests across every org you work in — not what shipped, but whether work
is actually moving.

```
╺━ GITHUB OPS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 9 accounts   updated 0s ago   4634/5000 api

 ── MERGE RATE ── last 7 days
 83%  ██████████████████████████░░░░░░  18 merged / 3 dropped
 open right now:  682 PRs   217 issues   (any age)

 ── OPENED PRs / DAY ── 14d, 126 total, peak 26
   █ ▅
 ▃ █ █   ▇
 █▄█▅█   █
 █████▆▄▃█▂ ▁▆

 ── MERGED PRs / DAY ── 14d, 110 total, peak 27
     █
   ▆▂█
 ▆▆███       ▁
 █████▄▃▅▁▁ ▂█
 14d ago today

 ── OPEN PR STATE ── 682 total
 ████████████████████████████████████████████████████████░░░░░░░░
 ▇ awaiting review 486 (71%)   ▇ ready to merge 180 (26%)   ▇ draft 16 (2%)

 ── CONTRIBUTIONS ── 6008 in 52 weeks, peak 241/day
 Mon   ░▒░▒░ ░▒░           ░     ░   ░ ░ ░ ░ ░░░░░░░▒▓░
       ░▒░░░ ▒▓         ░        ░░ ░░░ ░░ ░▒░ ░░░░░▒▒░
 Wed  ░░▒░░▒░░▒░      ░    ░     ░░  ▒░░░░ ░░░░░ ░▒█▓▒░

 ── BY ACCOUNT ──
 ACCOUNT               OPEN  REVW  MRG7D   RATE ISSUES
▸stealth-factory        628   486     18    86%    159  ████████████
 hk2047                   0     0      0    --       0  ░░░░░░░░░░░░
```

## What is windowed and what is not

This is the distinction worth holding onto, because the two kinds of number
answer different questions:

- **Point-in-time**, at any age: open PRs, open issues, drafts, review backlog.
  "How much is outstanding right now."
- **Windowed** by the merge window: the merge rate, and the per-account `MRG7D`
  and `RATE` columns. "How did the last N days go."
- **Fixed** to `history_days`: the two per-day charts. They do not follow the
  merge window, so changing it leaves them alone.

## Sections

**Merge rate** — of the PRs that *closed* in the window, the share that merged.
The bar is coloured on the same green→amber→red ramp as everything else, so a
sinking rate reads at a glance. `dropped` means closed without merging; GitHub's
`is:closed` includes merged ones, which is why the two are counted separately
rather than subtracted.

**Opened / merged per day** — two bar charts over `history_days`. Read together
they show whether the queue is filling faster than it drains.

**Open PR state** — one bar over every open PR, split into awaiting review /
ready to merge / draft, with the counts beneath. The review backlog is usually
the number that explains a falling merge rate.

**Contributions** — the familiar GitHub calendar, a full 52 weeks, in braille
shading.

**By account** — one row per org, sorted by open volume, with a bar for relative
size. `↑` `↓` selects one; the selected row is tinted.

## How the per-day charts get exact numbers

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

Verified against the aggregate for one account over 14 days: 107 merged and 119
opened counted both ways, both already past the old cap.

## Loading state

Changing the merge window used to leave the previous window's figures on screen
under the new label, which is a quietly wrong answer. The window-dependent
figures instead show a grey shimmer until real numbers land — the merge-rate bar
and the `MRG7D` / `RATE` columns. The per-day charts keep their bars, because
they do not depend on the window.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` | select an account |
| `m` | cycle the merge window — 7 / 14 / 30 / 90 days |
| `r` | refresh now |
| `q` | quit |

## Credentials

`github.token` in `config.json`, or `$GITHUB_TOKEN`. A **classic personal access
token** with `repo` and `read:org` covers private repositories and org
discovery.

**The `gh` CLI is deliberately not used** — the REST/GraphQL API is called
directly so the widget carries no dependency on another tool being installed and
authenticated.

`config.json` holds a secret once you put a token in it; `chmod 600` it. The
file is git-ignored and the token is never printed.

## Cost

One request per account per refresh for the aggregates, plus one per 20 days of
history, plus one for the contribution calendar. Each costs a single point
against the 5000/hour GraphQL budget, so nine accounts at the default 120s
refresh sits around 550 points/hour. Remaining budget is shown in the header.

Accounts are fetched one at a time rather than batched: results appear as they
arrive, and one bad account cannot blank the whole board. Batching every account
into a single request returned HTTP 502 on the complexity limit.

## Configuration

```json
"github": {
  "token": "",
  "token_env": "GITHUB_TOKEN",
  "accounts": [],
  "window_days": 7,
  "history_days": 14,
  "refresh": 120
}
```

Empty `accounts` discovers every org you belong to plus your personal account;
otherwise list org logins, and `@me` for your own.

```sh
./github.py                        # discovered accounts, 120s
./github.py -n 300 acme @me        # two accounts, slower
```
