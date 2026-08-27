# `gha`

[← all docs](README.md)

GitHub Actions — what is running, what is failing, and how long it sat in
the queue — across the accounts already in config.

```
╺━ GITHUB ACTIONS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 last 2d · 12 of 21 recently pushed repos with workflows (3 accounts, pushed in 14d)   12s ago
 2 running  1 queued  4 failed  41 ok   48 in 2d

 ── STATE ── 48 runs in 2d
 ████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██████████████████████████████

 ── REPEATED FAILURES ──
  3×  CI   opscope
  2×  Release   web-app

 ── DURATION ── median 2m14s  p95 8m02s  queue median    4s
 ▂▂▃▁▁▁▇▇█▇▇▇█▇▇▇▇▁▇▇▇▇▇▇▇▇▇▇▇▇██▇▇▇▇▇▇▁▁▁▁▇█▇

 ── RUNS ── 1 of 48
▸✖ fail    opscope     CI             main     push    3m12s q4s   17m
 ● run     web-app     Preview        feat/x   pull_request 1m02s q12s   2m
 ↑↓ select  →/↵ jobs  [s]tate all  [/]filter  [w]indow 2d  [r]efresh  [q]uit
```

`github` counts PRs. `pr` rolls up one PR's checks as a letter. Neither
answers the questions a stalled release actually raises: what is running
right now and how long it has been queued, which workflow is failing
repeatedly rather than once, which job and step broke, and whether the
pipeline is getting slower. Those are all stamps and conclusions GitHub
already holds.

## What is on the board

**State** — in progress, queued, failed, succeeded, over the labelled
window. The bar is those four counts, not every conclusion (cancelled and
skipped sit in the list, not in the headline).

**Repeated failures** — a workflow that failed two or more times in the
window. A flake fails once; this is the other thing.

**Duration** — median and p95 of *run* time, plus the median queue time,
with a sparkline of recent durations so drift is visible. Queue time is
`run_started_at − created_at` when GitHub recorded a start. A run that is
still queued uses `now − created_at`, which is elapsed time, not a guess.
A finished run with no start stamp is `--`, because GitHub did not say.

**Runs** — one row per run: conclusion, repo, workflow, and as the pane
widens, branch, event, queued-for, the run number. Extra width buys those
columns; nothing is truncated to keep a layout.

`→` or `↵` opens the selected run. The list can only say `fail`; the
detail fetches the jobs and, for a failed job, the step that failed:

```
╺━ ACME/APP / CI ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
  #412  fail  push
  main  queued    4s  ran  3m12s
  abc1234  fix: the thing
  failed 3 times in this window

 ── JOBS ──
  ✖ test (linux)   2m58s
     step 4 · cargo test
  ● build            14s
 [c]opy · [r]efresh · ← or esc to close · [q]uit
```

Jobs are fetched on demand. Two hundred runs are not worth pre-fetching
for the one you open, so the view paints immediately and fills in.

`c` copies the run URL through OSC 52.

## What "all repos" means

Ten accounts is hundreds of repos, most of which have no workflows. The
board does not fan out over everything.

- An explicit `gha.repos` list, or `owner/repo` arguments, is the set.
- Otherwise: repos pushed in the last `pushed_days` that have
  `.github/workflows` files, newest first, capped at `max_repos` across
  every configured account. Empty `gha.accounts` inherits `github.accounts`.

The heading always says which of those it is. `12 of 21 recently pushed
repos with workflows` is a partial board, named as one. A repo whose
window holds more runs than one page (`30`) says so rather than presenting
the page as the total.

## Keys

| | |
|---|---|
| `↑` `↓` | select a run — on a run's own screen, scroll |
| `→` `↵` | open the selected run's jobs |
| `←` `esc` | back to the board |
| `c` | copy the open run's URL |
| `/` | filter the list by repo, workflow, branch, event or conclusion |
| `s` | cycle the state filter — all / failed / running |
| `w` | cycle the window — 12h / 1d / 2d / 7d |
| `PgUp` `PgDn` `Home` `End` | move the selection by the page, or to either end |
| `r` | refresh now |
| `q` | quit, from either screen |

`j` and `k` move the selection the same way `↓` and `↑` do, as they do on
`deployments`.

## Credentials

Reuses `github.token` in `config.json`, or `$GITHUB_TOKEN`. The same
classic token `github` and `pr` already want — `repo` and `read:org`. This
widget only reads: it does not re-run, cancel, or write anything.

A missing token is said on the widget's own screen. So is an account list
that yielded no repos with workflows.

## Settings

`gha` in `config.json`:

| Key | Default | |
|---|---|---|
| `accounts` | `github.accounts` | org or user logins. Empty inherits the github list, then discovers. |
| `repos` | `[]` | `owner/name` list. Empty means recently-pushed-with-workflows, capped. |
| `window_hours` | `48` | the labelled window the counts and list cover. `w` cycles it live. |
| `refresh` | `60` | seconds between polls, minimum 30. |
| `max_repos` | `16` | cap on discovered repos. Named on screen when it cuts. |
| `pushed_days` | `14` | how recently a repo must have been pushed to be considered. |

Discovery is one GraphQL query per account. Runs are REST,
`GET /repos/{owner}/{repo}/actions/runs`, one request per repo. Jobs are
REST too, and only for the run you open. GraphQL and REST have separate
rate-limit buckets, so the run fetches do not compete with `github` and
`pr`, which already spend the GraphQL budget all day.
