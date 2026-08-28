# `github`

[← all docs](README.md)

Pull requests across every org you work in — not what shipped, but whether work
is actually moving.

```
╺━ GITHUB OPS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 9 accounts   updated 41s ago   4100/5000 api
 ── OPEN PR STATE ── 682 PRs · 220 issues open   (any age)
 ███████████████████████████████████████████████████████████████████████████
 ▇ awaiting review 486 (71%)   ▇ ready to merge 180 (26%)   ▇ draft 16 (2%)

 ── MERGE RATE ── last 7 days
 81%  ████████████████████████████████████░░░░░░░░  17 merged / 4 dropped

 ── PR FLOW ── 7d · ▲ 30 opened · ▼ 17 merged   peak 19/day
           █████████
           █████████
 ▃▃▃▃▃▃▃▃▃ █████████ ▂▂▂▂▂▂▂▂▂           ▁▁▁▁▁▁▁▁▁ ▆▆▆▆▆▆▆▆▆
 ─────────────────────────────────────────────────────────────────────
 ▀▀▀▀▀▀▀▀▀                                         █████████
 7d ago                                                          today

 ── CONTRIBUTIONS ── 6024 in 52 weeks, peak 241/day
 Mon   ░▒░▒░ ░▒░           ░     ░   ░ ░ ░ ░ ░░░░░░░▒░ ░░▒
       ░▒░░░ ▒▓         ░        ░░ ░░░ ░░ ░▒░ ░░░░░░░░░░░
 Wed  ░░▒░░▒░░▒░      ░    ░     ░░  ▒░░░░ ░░░░░░░░░░▒░░░
 current streak 3 days                 longest streak 50 days
 today 18                              active days 213 of 366 (58%)
 busiest 2025-08-30 (241)              most on Tue (1031)

 ── BY ACCOUNT ──   1-6 of 9
 ACCOUNT              OPEN REVW  MRG7D  RATE ISSUES  MERGED/DAY
▸example-corp          628  486     15   83%    162  ▇▂▂ ▃█
 wiiiimm (you)          34    0      2   67%     28       █
 example-labs           20    0      0    --      0
 example-tools           0    0      0    --      4
 example-web             0    0      0    --      0
 example-old             0    0      0    --      0


 ↑↓ account  [w]indow  [r]efresh  [q]uit
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

**By account** — one row per org, **busiest first**: open PRs decide the order,
merged-in-window breaks ties so an idle backlog ranks below an account of the
same size that is actually moving, and the name settles the rest to keep the
order steady frame to frame.

`↑` `↓` select. Where the pane cannot show every account the table **scrolls**
rather than truncating — the selection stays on screen, centred where there is
room either side and pinned at the ends — and the heading counts what is shown,
`8-9 of 9`.

The sparkline on the right is that account's own merged-per-day across the
window. The columns carry totals but no shape, and a fortnight of nothing
ending in a spike reads very differently from a steady trickle.

**Each row is scaled to its own busiest day**, which is what the `SHAPE ONLY,
NOT TO SCALE` heading is warning about: on one board here a full block meant 31
merged in one org's row and 16 in another's. Read a row left-to-right for its
trend; do not read heights across rows. The comparable number is the `MRG`
column two to its left.

A blank row genuinely means nothing merged. Dots mean that account has not yet
reported for the selected window.

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
  together. Until then the heading reads `counting 90d…` and its bars bounce
  like a level meter, in pale versions of their own colours rather than grey,
  so the two halves stay legible while they wait. When the figures land the
  bars **settle** onto them over about two seconds and the colour comes up to
  full, rather than the chart cutting from placeholder to data.

  The placeholder is the same chart, not a stand-in for one: the day count and
  bar width follow from the window and the pane, neither of which needs any
  data, so the loader draws exactly the bars the finished chart will have and
  each one simply moves into place.

Both sides of the flow chart always draw three rows, even when the merged half
never uses its full height. Trimming the unused rows would make the chart change
height at the end of the settle — precisely the moment it should be still — and
a shorter axis on one side would mean the two halves no longer shared a scale.

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
private. `w` has no need to.

Accounts are fetched one at a time rather than batched: results appear as they
arrive, and one bad account cannot blank the whole board. Batching every account
into a single request returned HTTP 502 on the complexity limit.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` | select an account — on an account's own screen, move through its oldest open PRs |
| `→` `↵` | open the selected account |
| `←` `esc` | back to the board |
| `c` | copy the selected PR's URL |
| `PgUp` `PgDn` `Home` `End` | scroll an account's screen by the page, or to either end |
| `w` | cycle the window — 7 / 14 / 30 / 60 / 90 days |
| `r` | refresh now, ignoring the day cache |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the view a line at a time — the pane moves, the selection stays where it is |
| `q` | quit, from either screen |

## One account on its own screen

`→` or `↵` opens the highlighted account. Most of what is there the row
already carried and had no room to spell out — open split into what waits on
a reviewer and what is still a draft, merged split into what landed and what
was closed unmerged — plus a few figures worth deriving:

- **net** — opened minus merged over the window. A queue of six hundred is a
  different thing depending on whether it grew by forty this week or held
  level.
- **merged/day**, with the open queue restated as time at that rate. *"110d
  of open PRs"* is the number people estimate and get wrong.
- **busiest day** and **days with none** — the shape of the window.

The **OPEN PR STATE** bar and the **PR FLOW** chart are the two the board
draws for every account added together, drawn here for one. That is the
reason to open the screen: a queue growing in a single account is invisible
in a total six others are also feeding.

**OLDEST OPEN** lists the ten longest-waiting PRs, newest information the
board cannot hold. Everything else on this widget is built from `issueCount`
aggregates — exact at any volume, one rate-limit point per request rather
than per alias, and unable to name anything at all. So this one asks for
nodes, once per account when its screen is first opened, and keeps the
answer.

`↑` `↓` move through that list and `c` copies the URL of the row under the
cursor, the same key `pr` uses for the same job. The page scrolls to follow
the cursor; `PgUp` `PgDn` move it freely for the sections above and below.

## Credentials

`github.token` in `config.json`, or `$GITHUB_TOKEN`.

**Use a classic token.** The deciding factor is how many accounts you point the
widget at: a fine-grained token is *"limited to access resources owned by a
single user or organization"*, and GitHub lists *"using a fine-grained personal
access token to access multiple organizations at once"* among the feature's
current gaps. This board exists to compare orgs side by side, so one
fine-grained token could cover exactly one of them — you would need a token per
org, and there is one `github.token` field to put them in. (The limit is one
*resource owner*, not one permission; fine-grained tokens can carry plenty of
permissions, just never across two owners.)

Create it at Settings → Developer settings → Personal access tokens → Tokens
(classic), with exactly two scopes:

| Scope | Why | Without it |
|---|---|---|
| `repo` | search sees private repositories | **every figure silently undercounts** — public results only, no error |
| `read:org` | enumerate the orgs you belong to | the account list comes back short, or empty |

Nothing else is needed. In particular the **contribution calendar needs no
`read:user`**, and no scope changes its total: the same account over the same
52 weeks reported 6024 contributions through a token with `user` and through
one without it. Work in private repositories is counted either way.

(GraphQL's `restrictedContributionsCount` is tempting to read as "how many were
private", and it is not — it counts contributions whose *details* the token
cannot see, so it moved from 4722 to 2 between those two tokens while the total
did not budge. It measures the token, not the work, so this widget does not
show it.)
`repo` is coarse (it grants write as well as read), but GitHub offers no
read-only equivalent for classic tokens.

Both failure modes are silent rather than loud, which is worse than an error,
so the widget reads the `X-OAuth-Scopes` header GitHub returns and says which
scope is missing instead of quietly showing smaller numbers.

Fine-grained tokens also return no `X-OAuth-Scopes` header, so the check above
cannot run against them. Only the classic path is tested.

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
  "window_days": 14,
  "refresh": 120
}
```

Empty `accounts` discovers every org you belong to plus your personal account;
otherwise list org logins, and `@me` for your own. `window_days` sets the window
the board opens on — **14 days by default** — and `w` cycles it from there
through 7 / 14 / 30 / 60 / 90.

Fourteen rather than seven because a week is short enough that one quiet
Friday moves every figure on the board: a merge rate, a per-day average and
a queue trend all read as noise when a single day is a seventh of the sample.

```sh
./target/release/github                        # discovered accounts, 120s
./target/release/github -n 300 acme @me        # two accounts, slower
```
