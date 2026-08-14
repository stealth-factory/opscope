# `pr.py`

The pull requests you have to follow up on, and a dashboard for whichever one
you open.

```
╺━ PR WATCH ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 33 of 33 open   updated 1m ago   4567 api
 ── OPEN PRs ── by updated ↓
 PR     REPO              TITLE                                    REVIEW  CHECKS   AGE        SIZE
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

## The list

Every PR matching `pr.query`, which defaults to `is:open is:pr involves:@me` —
everything you opened, were mentioned on, or were asked to review. That default
matters: the same account has **625 open PRs** across its organisations and 33
that involve this user. A list of 625 is a haystack, not a dashboard.

Columns are budgeted rather than guessed — the fixed ones are summed and the
title takes exactly what is left — so nothing runs off the right edge or into
its neighbour. The repo and size columns drop below 96 columns.

`⣿` before a title marks a PR that GitHub reports as part of a stack.

## Sorting and filtering

| Key | |
|---|---|
| `s` | sort by **updated** or **created** |
| `o` | reverse the order |
| `/` | filter |

Sorting is done locally on the fetched set, so both keys are instant and cost
no request.

`/` starts filtering and everything you type goes into the filter — including
`q`, which is why the other keys stop working until you leave. `↵` keeps the
filter and returns to navigating; `esc` clears it. The match is a substring
against number, title, author, repository and both branch names.

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

`▸` marks the PR you opened — a marker as well as the row tint, so which one
you are looking at does not depend on colour surviving a screenshot.

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
`github.py` uses. Set `pr.token` only to point this widget at a different
account.

## Configuration

```json
"pr": {
  "query": "is:open is:pr involves:@me",
  "limit": 100,
  "refresh": 60
}
```

Anything on the command line is appended to `query`, so `./pr.py org:acme`
narrows to one organisation and `./pr.py author:@me` to your own PRs, without
editing config.

```sh
./pr.py                          # everything you are involved in
./pr.py -n 120 review-requested:@me   # only what is waiting on your review
```
