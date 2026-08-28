# `github-prs`

[← all docs](README.md)

The pull requests you have to follow up on, and a dashboard for whichever one
you open.

```
╺━ GITHUB PRS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 4 accounts   updated 12s ago   4567/5000 api
 33 of 33 open
 ── STATE ── 34 open · 10 draft · 9 conflicting · 1 ready to merge
 ████████████████████████████████████████████████████████████████████████████████████████████████
 ▇ approved 2   ▇ CHANGES REQ 1   ▇ needs review 27   · checks pass 24   · checks FAIL 6

 ── OPENED / DAY ── last 30d · 12 of 34 still open · peak 11/day
                                                                        ██
                                                                        ██
                                                                        ██             ▂▂
 30d ago                                                                            today

 ── AGE ── median 53d  p95 1.9y  max 3.9y   idle median 53d
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
 ↑↓ select  [↵] open  [/]filter  [s]ort updated  [o]rder newest  [r]efresh  [q]uit
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

Three sections above the list, all computed from data already fetched, so they
cost nothing. `t` toggles them, and they stand down on their own below thirty
rows rather than leaving the list too short to be a list.

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
this is the one number that says something can be done right now.

**Opened / day** — when the *still-open* PRs arrived, over the last 30 days.
Not a throughput chart: it is the shape of the backlog's arrival, so a spike
means a batch landed and never left.

**Age** — median, p95 and max, then **one bar per open PR, youngest on the left
and oldest on the right**. The x axis is *rank, not time*: neighbouring bars are
adjacent in the sorted order, not a day apart. The shape of the tail is the
point — a backlog ending in a wall of full blocks is a different problem from
one that slopes.

Both charts carry a **baseline rule and end labels**, and the age chart spreads
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

**Page size is 50 per source, and every source is paged to exhaustion.**
Three searches of 100 return HTTP 502; three of 50 do not, so more results
come from more rounds and never from a bigger page. Each source carries its
own cursor and drops out of the round once GitHub says it has no next page.
Rows are published as each round lands, so the board fills while it works
rather than staying empty until the last source is done, and the count in
the header is the count on screen throughout.

**What a search will not serve at depth is fetched afterwards.** A search
carrying `stackEntry` and the check rollup stops being served after four
pages — measured: page five is a 502, whether it is one query over ten
owners or one query per owner, and splitting does not help. Without those
two subqueries the same search pages out in full: 665 of 665 in fourteen
rounds. So the search asks for plain fields only, and those two are fetched
by node id, fifty at a time, which answers every time. A lookup that fails
leaves the checks unknown rather than reporting a state nobody read — a
dash is honest, a green tick would not be.

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

One search per refresh for the list, one detail query when you open a PR, and
one more to reconstruct an inferred stack. The list query is the expensive one
at around 7 seconds for the search itself; the check rollup adds about a
second, which is why checks are worth carrying in the list rather than
deferring.

Detail is fetched only on demand — 33 PRs are not worth pre-fetching for the
one you open — so the view paints a loading shimmer and fills in.

## Credentials

**Reuses `github.token`** from `config.json`, or `$GITHUB_TOKEN`. No second
credential: it is the same classic token with `repo` and `read:org` that
`github` uses. Set `github_prs.token` only to point this widget at a different
account.

## Configuration

```json
"pr": {
  "sources": {
    "orgs":     "is:open is:pr @mine",
    "authored": "is:open is:pr author:@me",
    "assigned": "is:open is:pr assignee:@me"
  },
  "limit": 50,
  "refresh": 60
}
```

Add, remove or rename sources freely — `review-requested:@me` and
`is:open is:pr org:acme` are both reasonable entries, and the names are what
`f` cycles through. Anything on the command line is appended to *every* source,
so `./target/release/pr org:acme` narrows the lot without editing config.

```sh
./target/release/pr                          # everything you are involved in
./target/release/pr -n 120 review-requested:@me   # only what is waiting on your review
```
