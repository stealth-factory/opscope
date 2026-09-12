# `github-prs`

[← all widgets](../../../../docs/README.md)

The pull requests you have to follow up on, and a dashboard for whichever one
you open.

```
╺━ GITHUB PRS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 4 accounts   updated 12s ago   4567/5000 api
 33 of 33 open
 ── STATE ── 34 open · 10 draft · 9 conflicting · 1 ready to merge
 ████████████████████████████████████████████████████████████████████████████████████████████████
 ▇ approved 2   ▇ CHANGES REQ 1   ▇ needs review 27   · checks pass 24   · checks FAIL 6

 ── OPENED / DAY ── last 30d · 243 opened · peak 37/day
                         ██                                                       ▀▀█ ▀▀█
                 ▂▂    ▁▁██▁▁                  ▁▁      ▂▂▆▆                       █▀▀   █
 ▁▁▆▆▃▃    ▃▃  ▂▂██▁▁▇▇██████▄▄▃▃▃▃▇▇▅▅▂▂▁▁▁▁▃▃██▄▄▁▁▄▄████                       ▀▀▀   ▀
 ────────────────────────────────────────────────────────────
 30d ago                                                today                     opened · last 24h

 ── MERGED / DAY ── last 30d · 207 merged · peak 33/day
                         ██                                                         █ █▀█
                   ▁▁    ██        ▂▂                  ▁▁▅▅                         █ ▀▀█
 ▂▂▂▂▅▅▁▁    ▁▁▆▆▃▃██▅▅▄▄████▂▂▁▁▂▂██▅▅▃▃▁▁  ▄▄▇▇▆▆▂▂▅▅████▁▁                       ▀ ▀▀▀
 ────────────────────────────────────────────────────────────
 30d ago                                                today                     merged · last 24h

 ── AGE OF OPEN PRs ── median 53d  p95 1.9y  max 3.9y   idle median 53d
 ▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▂▂▂▃▃▃▃▃▃▃▄▄▄▄▄██
 ────────────────────────────────────────────────────────────────────────────
 youngest 7h                      33 PRs                          oldest 3.9y
  oldest #712 3.9y     untouched longest #7 1.2y       biggest #26 +41296/-27883

 ── OPEN PRs ── by updated ↓
 PR     REPO              TITLE                                    REVIEW  CHECKS  IDLE        SIZE
▸#493   web-app           feat(analytics): stamp every hit        approved    pass    1m    +214/-18
 #538   web-app           draft · feat(seo): give each oper   needs review    pass    4d   +1514/-79
 #501   web-app           test(analytics): guard the GA4 r       approved    pass    4d     +48/-12
 #213   cms               Bump the npm_and_yarn group acro    needs review    FAIL   51d  +1504/-902
 #712   tsup              feat: add tsup.confg.* file supp               —       —  274d      +9/-1
 ↑↓ select  [↵] open  [/]filter  [s]ort created  [o]rder oldest  [r]efresh  [,] settings  [q]uit
```

Open one and it becomes a dashboard:

```
 #511 chore(data): verify Sun Ferry fares (XFY-321) · draft
  alice   xfy-321-verify-fares → main
  review             needs review                 merge              ready
  unresolved threads 0                            size               +2/-2 in 1 files
  commits            1                            opened / updated   4d ago / 4d ago

 ── REVIEWERS ── 1 approved · 2 awaiting
  approved           bob
  awaiting           carol, dave

 ── CHECKS ── pass   12 total
  Analyze (javascript-typescript)                                    pass     98s
  freshness                                                          pass     26s
  CodeQL                                                             pass      2s
```

## The stats

Four sections above the list. Two are computed from data already fetched and
cost nothing; the two day charts are the exception and cost one request each,
which is described below. `t` toggles them, and nothing else does: they used to
stand down on their own below thirty rows, which looked exactly like a board
with nothing to say about itself. The pane scrolls instead — the wheel moves
the stats off the top and gives the list the whole pane.

**They describe every open pull request, not the filtered list.** Typing in the
filter is a search of the board, not a redefinition of it: watching the age
median and the state bar lurch on every keystroke made them unreadable, and
worse, made them look like statements about the whole backlog when they
described the three rows that happened to match. The same goes for `f` — narrow
to one source and the stats still describe the lot.

The list header carries the other half of that: `1 of 54 shown` says what the
filter did, while the sections above say what the board is.

**State** — a bar over the review decisions, with drafts, conflicts, and
**ready to merge** called out. That last one is approved, green, unconflicted
and not a draft: everything else on the board describes work in flight, and
this is the one number that says something can be done right now. Which is
why nothing unread falls into it — a check rollup that was never fetched and
a trial merge that has not come back both hold a PR out of the count, and
the conflicting figure beside it reads `9 conflicting of 34 read` whenever
it is over less than the whole board.

**Opened / day** — how many pull requests were **opened** on each of the last
30 days, whatever became of them since: merged, closed again or still sitting
there, they are all counted on the day they arrived. GitHub is asked for it,
one `created:YYYY-MM-DD` count per day, because the pool behind this board is
`is:open` throughout and bucketing it by `createdAt` counted only the arrivals
that are still open — 26 of 677 on the board this was measured against, about
4%, which drew a busy month as a quiet one. Drafts are counted, as they always
have been; nothing here filters them.

**Merged / day** — the same 30 days, the same bar height and the same
`30d ago … today` axis, directly underneath, so the two can be read against
one another: arrivals over departures. Each carries **one figure drawn large**
in its right half — `opened · last 24h` and `merged · last 24h` — which is the
only pair of numbers on the board that says whether the backlog is growing
today.

The window is **rolling, not a calendar day**, which is what the labels say. At
nine in the morning "today" is three hours of evidence and reads as a collapse
in throughput; 24 hours back from now does not.

**Both halves of each row count the same population**, which is what makes the
bars and the figure beside them readable as one thing. They did not always: the
bars used to be the open pool and the figure everything opened, two numbers side
by side, 2.5x apart, both correct, describing different populations. The one
difference left is the window — the bars are calendar days in UTC and the figure
is the last 24 hours rolling, so today's bar and the figure are not the same
number and are not meant to be.

The figure's half is as wide as its label when there is room for both. Narrower,
the label wraps onto two lines under the digits rather than being cut, and
narrower still the number sits on its own line under the caption, so the
heading and the peak cannot push it off the pane. A narrow pane loses the
size of the number, never the number. The digits themselves are drawn on
**half blocks, two pixel rows to a cell**, which is how three rows of text
carry a 3x5 glyph — core has no large-digit font.

When nothing is open, STATE and AGE have nothing to say and stand down. Both
day charts still draw: neither is counted from the pool, so an afternoon of
pull requests opened and merged again is a real reading of a board with nothing
open on it — and it is the only signal left.

**Age of open PRs** — median, p95 and max, then **one bar per open PR, youngest
on the left and oldest on the right**. The heading names the population because
this is the only section left that describes the open backlog rather than what
GitHub counted over the last thirty days. The x axis is *rank, not time*: neighbouring bars are
adjacent in the sorted order, not a day apart. The shape of the tail is the
point — a backlog ending in a wall of full blocks is a different problem from
one that slopes.

All three charts carry a **baseline rule and end labels**, and the age chart spreads
its bars to reach the right edge exactly. Without that a short chart simply
stopped mid-pane with no way to tell a finished chart from a truncated one.
When there are more PRs than columns the oldest are kept and the middle label
says so — `28 of 140 PRs` rather than silently dropping the rest.

Underneath, the three worth naming: oldest, untouched longest, and biggest by
diff.

Heights are **linear against the oldest PR**, which is worth knowing when the
spread is wide: with an outlier at 3.9 years, everything under a couple of
months lands on the same lowest block. `latency` solves the same problem
with a log scale; this chart has not adopted one yet.

## Where the day counts come from

Every search on this board is `is:open` throughout, so **nothing merged is ever
in hand** and an arrival that has since merged is gone from the pool. No amount
of reading it can produce either row. GitHub is asked instead, as counts rather
than records: one aliased
`search(query:"… is:pr is:merged merged:YYYY-MM-DD", type:ISSUE) { issueCount }`
per day for the merges, plus one for each rolling window, and one
`is:pr created:YYYY-MM-DD` per day for the arrivals. Measured against the live
API at HTTP 200, and a whole pass including the account walk cost 15 of 5000
rate-limit points. Paging the pull requests to count them would
have been a hundred round trips for two numbers.

**Two requests, not one.** The temptation is to put all sixty-two aliases in
one round trip, and it was measured: thirty-two merge aliases answer in
4.0–4.3s, thirty arrival aliases on their own in 4.1–4.4s, and the two
together in one request take 8.0–8.1s — sitting on the ~10s gateway cliff this
widget has already spent three issues climbing away from. Split, they cost
about 15 more rate-limit points a refresh against 5000 an hour, and neither
can take the other down.

The rolling windows are asked for as full datetimes — `merged:>=2026-09-10T09:00:00Z`
— which GitHub's search accepts. That was verified before it was relied on: the
same query at a one-hour cut returns a smaller count than at 24, so the time
part is read rather than ignored.

**The counts are their own requests on the list's own cadence, and never folded
into the list's paging.** A count GitHub refuses must not cost the list, which
has its own hard-won resilience to GitHub's slow spells: the reason lands beside
the figures rather than in the pane's error line, the last good counts stay on
screen, and that chart's own caption says the count failed and how old what you
are looking at is. **Each row answers for itself** — arrivals refused while the
merges land says so on the OPENED caption and leaves the rest of the board
exactly as it was, and the other way round. The one thing they share is the day
list, built once a pass, so the two rows always plot the same thirty days.

**A figure that has not arrived is not a figure of zero.** Nothing merged and
nothing opened in a day are both real and unremarkable readings, so an unfetched
count draws a shimmer and the word `loading` instead, and an alias GitHub leaves
out of the answer fails the whole read rather than landing on a zero the chart
would draw as a quiet day. A day missing from an answer takes the whole series
with it: the bars shimmer and the caption says `counting`, because one invented
zero in thirty bars is a claim about the day that may have been the busiest of
the month.

Both count over `@mine` — every org you belong to plus your own account, the
same expansion the list's own scope uses. That is deliberate and it is worth
knowing: if your configured `sources` do not use `@mine`, the two rows still
describe every account you can see, not the narrower ground the list searched.

## Which PRs, and why it takes three searches

GitHub search has **no `OR`**, so anything that is a union of conditions has to
be several searches pooled. Each entry in `github_prs.sources` is one search; results
are merged and de-duplicated by URL, and every PR remembers which sources found
it.

```json
"sources": {
  "orgs":     "is:open is:pr @mine",
  "authored": "is:open is:pr author:@me",
  "assigned": "is:open is:pr assignee:@me"
}
```

`@mine` expands to every org you belong to plus your own account, as owner
qualifiers — repeated qualifiers of the same kind *are* OR'd by GitHub, so one
search covers all of them. That gives everything in your orgs and your personal
repos. The other two reach outside those, for work that is yours wherever it
lives.

Measured on one account: `orgs` finds 50, `authored` 15, and the union is 55 —
so **five PRs the author filed outside their own organisations** would have been
missed by scoping alone, and are exactly what the extra searches are for.

The earlier default was a single `involves:@me`, which is the widest
relationship qualifier there is — author, assignee, mentioned, *or commented on
once*. That is how a pull request in a stranger's repository, commented on 274
days ago, ended up on the board. It has no scope attached, so nothing confined
it to code you have a stake in.

`f` cycles which source is shown — `all`, then each by name. It is instant and
costs no request, because the pooling already recorded the answer.

**Page size is 25 per source, and every source is paged to exhaustion.**
Three searches of 100 return HTTP 502; three of 50 do not, so more results
come from more rounds and never from a bigger page. The shipped default is
25 because a slow spell sheds larger pages first; a gateway 502/503/504
or a curl timeout asks again at half the size, down to ten. Each source
carries its own cursor and drops out of the round once GitHub says it has
no next page.
Rows are published as each round lands, so the board fills while it works
rather than staying empty until the last source is done, and the count in
the header is the count on screen throughout.

**The search carries plain fields only. Everything else arrives in two
passes afterwards, by node id, fifty at a time.** Both passes exist because
of what asking inside the search costs, but they are two rather than one
because the reasons are different and so are the failures.

*Stack and checks* — `stackEntry` and the check rollup — stop being served
after four pages: page five is a 502, whether it is one query over ten
owners or one query per owner, and splitting does not help. Without those
two subqueries the same search pages out in full, 665 of 665 in fourteen
rounds.

*The trial merge and the diff counts* — `mergeable`, `additions`,
`deletions`, `changedFiles` — are served at any depth, just slowly: GitHub
runs a trial merge and totals a diff to answer them, which about doubles
the request. Measured at 25 per page, three runs each: 2.2–2.8s without
them, 3.3–9.8s with — and the 9.8 is a slow minute landing on the request
that cannot fail without ending the pass. By node id the same four fields
cost 2.3–3.3s per fifty. The wall time is the same; the risk is not.

The two are kept apart because asking for both groups in one node query is
a 502 as readily as the search was, and because a stack lookup that failed
should not also cost the conflicting count.

A failed lookup never becomes an answer. The first pass marks the checks
unknown rather than reporting a state nobody read — a dash is honest, a
green tick would not be. The second needs no marker: the SIZE column stays
blank rather than drawing `+0/-0`, the STATE line says how many trial
merges its conflicting count is over, the reckoning line's *biggest* waits
for a figure to rank on, and nothing with an unread trial merge is counted
**ready to merge**. Which is also what the board looks like for the second
or so between the rows appearing and the figures landing: filling in, not
claiming.

## The list

Columns are budgeted rather than guessed — the fixed ones are summed and the
title takes exactly what is left — so nothing runs off the right edge or into
its neighbour. The repo and size columns drop below 96 columns.

`⣿` before a title marks a PR that GitHub reports as part of a stack.

The time column **follows the sort**: sorting by `created` shows `AGE`, sorting
by `updated` shows `IDLE`. Labelling both of them "AGE" had the column
reporting time-since-update while the stats above reported true age, and the
two disagreed by years on the same PR.

## Sorting and filtering

| Key | |
|---|---|
| `s` | sort by **updated** or **created** |
| `o` | reverse the order |
| `/` | filter by text |
| `f` | show one source, or all |
| `t` | show or hide the stats |
| `↑` `↓` | in a PR view, move through its stack |
| `↵` | in a PR view, open the stack row under the cursor |
| `c` | copy the PR's URL |
| `r` | refetch now |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the whole widget a line at a time — the pane moves, the selection stays where it is |
| `,` | open settings |
| `q` | quit |

Sorting is done locally on the fetched set, so both keys are instant and cost
no request.

`/` starts filtering and everything you type goes into the filter — including
`q`, which is why the other keys stop working until you leave. `↵` keeps the
filter and returns to navigating; `esc` clears it. The match is a substring
against number, title, author, repository and both branch names.

## Copying

`c` copies a PR's URL — the highlighted row in the list, the open PR in the
dashboard — through **OSC 52**, so the terminal emulator performs the copy and
the text lands on the clipboard of the machine you are sitting at, not the
server the widget runs on. That is the only mechanism that works over SSH.

The header confirms with `copied <url>` for a few seconds. If stdout is not a
terminal, or the multiplexer refuses to forward the escape, the confirmation
says `no clipboard:` and shows the URL instead of pretending it worked.

In a stack, `c` copies the PR **on screen** (the `●` row). To copy a different
one, move the cursor to it and press `↵` first — then it is the one on screen.

## Opening a PR

Detail is fetched on demand, and the wait shows the work rather than a
placeholder:

```
 ── OPENING ── acme/web-app #536

   ✓  pull request, checks, reviews                    1.4s
   ⠼  stack, from open branches
```

A braille spinner sits on the stage in flight; finished stages get a tick and
their actual duration. The stages are the real requests — the pull request
query, then the repository sweep that reconstructs a stack — so a PR whose
stack GitHub already knows shows `stack, from GitHub` and no second wait.

This replaced a block of shimmering bars. A shimmer says "wait" and nothing
else; a trace says what is being waited on, which is both more useful and more
like a machine doing something. One thin sweeping line is kept underneath for
motion.

## The dashboard

**Status grid** — review decision, merge state, unresolved threads, size,
commits, and how long since it was opened and last touched. Merge state is
GitHub's `mergeStateStatus` rendered as words: `ready`, `CONFLICT`, `blocked`,
`behind`, `checks red`.

**Reviewers** — grouped by what they did, with the *last* state per person
winning. Someone who requested changes and later approved has approved, and
showing both would misreport the gate. `awaiting` is people asked who have not
answered.

**Checks** — every context on the last commit with its conclusion and duration,
**failures first**. A green wall of passing checks is not why anyone opens this
view.

## The stack

When the PR belongs to a stack, the dashboard grows a stack map and states the
merge order. There are two sources, and the heading says which was used.

**`from GitHub`** — the API's own `PullRequestStack`, populated by
[`gh stack`](https://github.com/github/gh-stack). `PullRequestStackEntry.position`
is documented as "1 is the closest to the base", so the order is authoritative
and needs no reconstruction. A native stack is a *line*, so it draws flat with
its position numbers; eleven levels of indentation would be unreadable and
would imply a branching that is not there.

**`inferred from branches`** — for stacks made any other way. A PR whose base
branch is another open PR's head branch is sitting on top of it. This costs one
extra request, scoped to the PR's own repository.

An inferred stack is a **tree**, not a line — one PR can have several branched
off it — so it draws with real connectors:

```
 ── STACK ── 3 pull requests · inferred from branches
  merge bottom-up: the one nearest the base branch first
  main
▸└─ #6     CLR-37 Listing all Articles                   needs review  CONFLICT
     ├─ #7     Update all dependencies                              —  CONFLICT
     └─ #8     Upgrade dependencies                                 —  CONFLICT
```

Two gutter marks, because they answer different questions: **`▸` is the
cursor**, **`●` is the PR currently on screen**. One symbol plus a colour could
not say both, and after walking a few steps up a stack they are rarely the same
row.

`↑` `↓` move the cursor through the stack and **`↵` opens whatever it lands
on**, so a stack can be walked from inside itself without going back to the
list. The stack scrolls when it is taller than the space left after the checks
— eleven-deep stacks exist — and the heading counts what is shown.

**Merge bottom-up**: the PR nearest the base branch first, then rebase or
retarget what sat on it. A child merged before its parent drags in commits
nobody reviewed. The pane *shows* the order; it does not merge anything, which
is deliberate — a wall display that can merge is a wall display that can merge
by accident.

## Cost

Each paging round is one search, then two enrichment passes over that
round's new rows, fifty ids at a time. Opening a PR costs one detail
query. Reconstructing an inferred stack pages the repository, a hundred
open pull requests at a time.

The round costs what it always did, around eight seconds per fifty pull
requests. What changed is where: the search is 2.2–2.8s of it instead of
3.3–9.8s, and the two passes, whose failure is non-fatal by design, carry
the rest. No single request sits near GitHub's ten-second gateway budget,
which is the one that used to come back as a raw 502 page.

Detail is fetched only on demand — 33 PRs are not worth pre-fetching for the
one you open — so the view paints a loading shimmer and fills in.

## Credentials

**Its own `github_prs.token`** in `config.json`, or the variable
`token_env` names (`$GITHUB_TOKEN` when that key is unset). It wants
the same classic token with `repo` and `read:org` that `github` wants, and one
variable still serves all three — but it reads only its own section. It used
to borrow `github.token`, which was fine while both held the same string and
wrong as soon as they did not: a settings screen showing `unset` while the
widget quietly ran on somebody else's credential.

## Configuration

```json
"github_prs": {
  "token": "",
  "token_env": "GITHUB_TOKEN",
  "sources": {
    "orgs":     "is:open is:pr @mine",
    "authored": "is:open is:pr author:@me",
    "assigned": "is:open is:pr assignee:@me"
  },
  "limit": 25,
  "refresh": 60
}
```

`limit` is the page size a search asks GitHub for, not a cap on what the
pane shows — paging runs until every source is exhausted either way. It
matters because GitHub's search backend goes through slow spells and sheds
the heaviest requests first: measured during one, every size from 25 up
returned 502 at about 10.7s while 20 and below answered in three, and an
hour later 50 answered in five with nothing changed at this end. So a round
that is refused — a gateway 502, 503 or 504, or curl running out of its
45 seconds — asks again at half the size, down to a floor of ten. A round
still refused at the floor is asked once more after a three-second pause,
because a slow spell is usually one bad request rather than a bad minute;
only then does the pass stop paging and report the list as a floor.

None of that is an error, and it is not drawn as one. A pass that fell back
or stopped short leaves a dim line under the count — `GitHub is slow ·
served 10/page · 302 of at least 686` — naming the page size it was served
and how far it got, with `next pass in 34s` on the end when the pane is wide
enough to hold it. The `!` banner stays for a pass that produced nothing and
for a refusal that really is one.

Leave `token` empty and the variable `token_env` names is read instead,
defaulting to `GITHUB_TOKEN`. Its value is the variable's name, not a
credential. Nothing here reaches into another widget's section.

A leftover `"pr"` section is still read when `"github_prs"` is absent, and
the pane says so. Rename it when you next edit the file.

Add, remove or rename sources freely — `review-requested:@me` and
`is:open is:pr org:acme` are both reasonable entries, and the names are what
`f` cycles through. Anything on the command line is appended to *every* source,
so `./target/release/github-prs org:acme` narrows the lot without editing config.

**`sources` replaces the shipped three wholesale — it is not merged with
them.** Naming one search gives you one search. Leaving the key out
altogether is what gives you the defaults, so deleting your `sources` block
is how you get them back.

An **empty** `"sources": {}` therefore lands on those same three, exactly as
leaving the key out does. The two are deliberately not told apart: with no
sources no request is sent at all, so an empty map bought a board that could
not say anything about anything — and it used to print "no open PRs" and
`0 of 0 open` from it, which are totals about a search that never ran.

That state is one keypress away, because the settings screen will delete the
last entry, so the defaults stand in rather than leaving you with a board
that cannot work. The pane says when that happens — *`sources` is empty —
using the three shipped searches* — because a config file reading `{}` beside
a board searching three things is a disagreement worth hearing about.

The cost, stated plainly: **emptying the map is not a way to search
nothing.** There is no use for that state, so nothing offers it.

```sh
./target/release/github-prs                          # everything you are involved in
./target/release/github-prs -n 120 review-requested:@me   # only what is waiting on your review
```
