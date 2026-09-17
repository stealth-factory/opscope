# `github`

[← all widgets](../../../../docs/README.md)

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

 ── PR FLOW ── 7d · peak 11/day
         ▁▁▁▁▁▁▁         ███████                 ▅▅▅▅▅▅▅  ▀▀█ █▀█
         ███████ ▁▁▁▁▁▁▁ ███████ ▇▇▇▇▇▇▇         ███████    █ █ █
 ▂▂▂▂▂▂▂ ███████ ███████ ███████ ███████ ▆▆▆▆▆▆▆ ███████    ▀ ▀▀▀
 ───────────────────── ▲ 30 · ▼ 17 ─────────────────────  opened 24h
         ███████ ▀▀▀▀▀▀▀ ███████ ▀▀▀▀▀▀▀ ███████ ███████    █ █ █
         ▀▀▀▀▀▀▀         ▀▀▀▀▀▀▀         ███████            █ ▀▀█
                                                            ▀   ▀
 7d ago                                            today  merged 24h

 ── CONTRIBUTIONS ── yours, everywhere · 6024 in 52 weeks, peak 241/day
 Mon   ░▒░▒░ ░▒░           ░     ░   ░ ░ ░ ░ ░░░░░░░▒░ ░░▒
       ░▒░░░ ▒▓         ░        ░░ ░░░ ░░ ░▒░ ░░░░░░░░░░░
 Wed  ░░▒░░▒░░▒░      ░    ░     ░░  ▒░░░░ ░░░░░░░░░░▒░░░
 current streak 3 days                 longest streak 50 days
 today 18                              active days 213 of 366 (58%)
 busiest 2025-08-30 (241)              most on Tue (1031)

 ── BY ACCOUNT ──   1-6 of 9
 ACCOUNT              OPEN REVW  MRG7D  HELD   R24   T2D ISSUES  MERGED/DAY
▸example-corp          628  486     15   83%   71%   40%    162  ▇▂▂ ▃█
 wiiiimm (you)          34    0      2   67%   ···   ···     28       █
 example-labs           20    0      0    --    --    --      0
 example-tools           0    0      0    --    --    --      4
 example-web             0    0      0    --    --    --      0
 example-old             0    0      0    --    --    --      0


 ↑↓ account  [w]indow  [r]efresh  [q]uit
```

## What is windowed and what is not

Worth holding onto, because the two kinds of number answer different questions:

- **Point-in-time**, at any age: open PRs, open issues, drafts, review backlog.
  "How much is outstanding right now." This is the top section, and it never
  changes when you change the window.
- **Windowed** — everything else: the merge rate, the PR flow chart, and the
  per-account `MRG*D`, `HELD`, `R24` and `T2D` columns. "How did the last N
  days go." `w` changes that sample. The R24 bar is still 24 hours and the
  T2D bar is still 2 days.

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
on the same green→amber→red ramp as everything else — read the other way up,
because here a high number is the healthy one: a rate near 100% draws green and
a rate near zero draws red. The per-account `HELD` column is this number. The
board section keeps the heading `── MERGE RATE ──` and still spells the
formula. `dropped` means closed without merging; GitHub's `is:closed` includes
merged ones, which is why the two are counted separately rather than subtracted.

**PR flow** — one diverging chart: PRs opened grow up in purple, PRs merged grow
down in green, from a shared baseline. Read together they answer whether the
queue is filling faster than it drains. **Both directions share one scale**, or
the comparison would lie, and the heading names the peak that scale represents.

The chart fills what is left of the pane: where there is room to spare a day
takes several columns with a gap between bars, and where there is not, the
oldest days are cropped and the heading says so — `54d of 90d` — because the
totals describe what is drawn, not the whole window.

To the right of it stand the two figures the window total cannot give you:
**opened 24h** above the axis and **merged 24h** below it, repeating the
chart's own grammar. A week of `▲ 30 · ▼ 17` says the queue grew; it cannot
say whether it grew *today*, which is the only version of the question you
can still do something about.

The **window totals sit on the axis**, which is the line that divides opened
above from merged below — so each total labels the half it divides rather
than being a fourth fact in a heading that overflowed first on a narrow pane.
Where the rule is too short to carry them and still read as one rule rather
than two stubs, they go back into the heading, worded where the words fit and
as bare numbers where only the numbers do; where the heading cannot hold them
either they take a row of their own. Nothing here is ever clipped mid-number:
the heading used to lose `▼ 147 merged` to `▼ 1` on a forty-column pane, which
is a wrong number rather than a missing one.

The figure column is **as wide as the wider of its label and three large
digits**, thirteen cells, and it stands down below a thirty-six column pane —
where the two figures take a row of text under the heading instead, because
a pane with no figure on it reads as an account with nothing opened. When
even the compact form would clip a count they take a row each: a
twenty-column pane has nineteen cells and `24h · ▲ 170 · ▼ 147` needs
twenty, which used to draw `▼ 14`. The
labels say `24h` rather than `opened · last 24h`: seven cells of wording that
named no window the short form leaves unnamed, spent out of a chart that had
eighteen columns to draw eighteen days in. Six of them come back to the
chart, which at fifty-six columns over an eighteen-day window is the
difference between one cell a day and two.

Three things about those figures:

- The window is a **rolling twenty-four hours**, not a calendar day. It is
  cut twenty-four hours back from now and carries the time of day, so it
  does not collapse every morning the way a midnight-to-now count does. The
  label says `24h` rather than `today` for that reason.
- They count **only the configured accounts**, which is what everything else
  on this board except the calendar counts. `github-prs` draws the same two
  figures over the same window from a different population: it pools `@mine`,
  authored and assigned and dedupes them, so it sees pull requests in
  repositories this board never asks about. **Two panes of the same wall
  will show different numbers under the same window**, and neither is wrong.
- A figure still being counted **shimmers rather than showing a zero**.
  Nothing opened in a day is a real and unremarkable reading, and it has to
  look different from a figure that has not arrived. The board's figures are
  the sum across accounts and appear only once every account has reported
  one, because a sum missing a member is a smaller number wearing the same
  label.

The column is reserved out of the chart's width *before* the days are spread,
so the bars narrow to make room rather than being drawn over. Where the chart
cannot pay for it — the days would be cropped, or fewer than twenty columns
would be left — **the figures stand down and the chart keeps the pane**. The
chart is the section; the figures are the addition. On a 90-day window that
means they appear only on a very wide pane.

**Contributions** — the familiar GitHub calendar, a full 52 weeks, in braille
shading, with the numbers underneath it that the squares cannot show: current
and longest streak, today's count, how many days of the year were active, the
single busiest day, and which weekday carries the most work. They lay out in
three columns, two, or one as the width allows.

Everything else on this board is scoped to the configured accounts. This section
is not, and the heading says so — `yours, everywhere`.
`contributionsCollection` is per-viewer
rather than per-org, so the calendar counts your own activity across all of
GitHub, including repositories in orgs this board does not list, and excluding
everyone else's work in the orgs it does. It is the calendar github.com draws on
your profile. The qualifier is the first thing to go when the pane narrows,
because the figures beside it matter more than the wording.

A streak counts consecutive days with at least one contribution, the way
github.com does it — a day that has scored nothing *so far* does not break the
current streak, because it is not over yet.

This is the one decorative section. It is drawn whenever the calendar has data; a
short pane scrolls to it rather than hiding it, because a grid that is not there
looks like an account with no contributions.

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
column to its left.

`HELD` is always on the row: of PRs that **closed** in the window, the share
that merged. `--` means nothing closed; `···` means that account has not yet
been refetched for the current window.

`R24` appears from 50 columns, `T2D` from 56. Both are % of PRs that
**merged** in the window — a dropped PR never lands, so it is not "slow to
merge." `R24` is the share whose first **human** review arrived within 24
hours of `createdAt` (bot reviews are skipped, or every CodeRabbit pass
looks instant). `T2D` is the share with `mergedAt − createdAt` at most two
days, including time spent in draft. `[w]` changes which PRs are in the
sample; it does not change those two bars.

Those two wait on a later paging pass. While that pass is short, or if
`o0_merged` is larger than the nodes fetched, the cells stay `···` — a
partial page is not a total. `--` when nothing merged.

ISSUES and the spark still appear at 62 columns. Extra width after that
buys more spark days, not another metric.

A blank row genuinely means nothing merged. Dots mean that account has not yet
reported for the selected window.

## How the per-day counts stay exact

Every day on the chart is **GitHub's own count of that day**, not a tally of
pull requests read back and bucketed. Reading them back would mean paging
through every one, and a pass that stopped short — which a busy account will
make it do — draws a busy month as a quiet one while looking entirely
plausible. A count GitHub computes is exact at any volume, and these are that,
so the chart can be trusted on a busy account as readily as a slow one.

## Loading state

**The headline figures land before the chart does** — the merge rate and open
state are live within seconds while the chart is still counting, because the
chart is ninety days of counting and they are not. R24 and T2D wait on a
third pass that pages the merged PRs; they stay `···` until that set is
complete.

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

A past day's counts cannot change — a PR merged on the 3rd stays merged on the
3rd — so days are held per account and only the trailing two are read again.
**Widening the window costs only the days it adds; narrowing costs nothing.**
Even a cold ninety-day window is a small fraction of an hour's GraphQL
allowance. The REST figure `github-actions` shows is a different one; this
allowance is shared with `github-prs` when they use the same token.

`r` re-reads every day from scratch, which is the escape hatch for the cases a
past day's immutability does not cover — a repo deleted, transferred or made
private. `w` has no need to.

Accounts are read one at a time rather than together: rows appear as they
arrive, and one bad account cannot blank the whole board.

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
| `,` | open settings |
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

**TO LAND** sits after those unlabeled fields and before OPEN PR STATE.
Held moves here (the standalone merge-rate field is gone). Then the two
landed-set percentages, with the median and the count the compact row
cannot hold:

- **first review ≤24h** — `12 of 15 · median 6h · bots skipped`
- **opened → merged ≤2d** — `6 of 15 · median 3.2d · includes draft`
- **no human review** — a count, not a third compact %. How many of the
  merged set never saw a human reviewer.

The heading says `last {N}d · merged in window, except held`. Incomplete
paging writes `···` and `incomplete · 100 of 247 paged` rather than a
sample percent.

The **OPEN PR STATE** bar and the **PR FLOW** chart are the two the board
draws for every account added together, drawn here for one. That is the
reason to open the screen: a queue growing in a single account is invisible
in a total six others are also feeding. The two **24h** figures come
with the chart and are scoped the same way — this account alone rather than
the board's sum.

**OLDEST OPEN** lists the ten longest-waiting PRs, newest information the
board cannot hold. Every other figure on this widget is a count, and a count
cannot name anything; this is the one section that names individual pull
requests, read when the screen is first opened and kept.

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
`read:user`**, and no scope changes its total — work in private repositories is
counted either way, so the calendar is the whole year's work however the token
was made.

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
