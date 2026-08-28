# `github-actions`

[← all docs](README.md)

GitHub Actions — what is running, what is failing, and how long it sat in
the queue — across the viewer's personal repos and every org they belong
to, drawn the way `deployments` draws Vercel.

```
╺━ GITHUB ACTIONS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 10 accounts   updated 20s ago   4840/5000 api
 48 runs · 12 repos  41 success  4 failed  2 running

 ── ACTIVITY ── runs/hour, last 48h
 ▂▂▃▁▁▁▇▇█▇▇▇█▇▇▇▇▁▇▇▇▇▇▇▇▇▇▇▇▇██▇▇▇▇▇▇▁▁▁▁▇█▇
 48h ago                                                    peak 6/h

 ── DURATION ── median 2m14s  p95 8m02s  max 11m40s  queue    4s
 ▂▂▃▁▁▁▇▇█▇▇▇█▇▇▇▇▁▇▇▇▇▇▇▇▇▇▇▇▇██▇▇▇▇▇▇▁▁▁▁▇█▇

 ── REPEATED FAILURES ──
  3×  CI   opscope

 ── RECENT ── 1 of 48
▸● success   3m12s   2m  acme/app
   CI · the build that came good
 ✖ failure   1m02s   9m  acme/deploy-tools
   Nightly release · the build that broke
 ● success      8s  17m  alice/toy
   CI · fix: the thing
 ↑↓ select  →/↵ details  [s]tate all  [/]filter  [w]indow 48h  [r]efresh  [q]uit
```

`github` counts PRs. `github-prs` rolls up one PR's checks as a letter. Neither
answers the questions a stalled release actually raises: what is running
right now and how long it has been queued, which workflow is failing
repeatedly rather than once, which job and step broke, and whether the
pipeline is getting slower. Those are all stamps and conclusions GitHub
already holds.

## What is on the board

**Headline** — first the github-ops meta line: how many accounts this look
covers, when the last poll finished, and remaining/limit on GitHub's REST
budget (`4840/5000 api`). GraphQL and REST are separate buckets; this
widget's run fetches spend REST, so that is the number. Then run count and
repo count (how many were asked for runs, including a quiet window), then
success / failed / running, the same placement as `deployments`' ready /
error / building. A missing poll stamp is `--`, not a fake now.

**Activity** — runs per hour over the last 48h (the default window; `w`
cycles 12h / 24h / 48h / 7d), coloured by the worst outcome in each
bucket. The axis says `48h ago` and names the peak, matching deployments.

**Duration** — median, p95 and max of *run* time, plus the median queue
time, with a sparkline of recent durations so drift is visible. Queue time
is `run_started_at − created_at` when GitHub recorded a start. A run that
is still queued uses `now − created_at`, which is elapsed time, not a
guess. A finished run with no start stamp is `--`, because GitHub did not
say.

**Repeated failures** — a workflow that failed two or more times in the
window. A flake fails once; this is the other thing. Omitted when nothing
repeats.

**Recent** — one list, newest first, whoever owns the repo. It was banded
under scope headings once; that reads well for a directory and badly for a
feed, because a failure a minute old sat screens down under an org whose
name starts with a later letter. The owner moved onto the row instead,
where it is read per run rather than inferred from which band the eye is
in.

Two lines per run when the pane is not wide enough for the commit title on
the metadata row. The first carries only fields whose own values cannot
outgrow them — conclusion, duration, age, and as the pane widens sha,
branch, event, queued-for — then `owner/repo`, last, taking whatever is
left. The second is the workflow and `display_title`. Nothing is padded
into a budget it can overflow, because a name cut to fit tells a reader
nothing they can act on. A missing `created_at` is `--`, not `0s`.

`→` or `↵` opens the selected run. The list can only say `failure`; the
detail fetches the jobs and, for a failed job, the step that failed:

```
╺━ ACME/APP / CI ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
  #412  failure  push
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
board does not fan out over everything, and it does not let one busy org
eat every slot.

- An explicit `github_actions.repos` list, or `owner/repo` arguments, is the set.
- Otherwise: empty `github_actions.accounts` discovers the viewer login and every
  org they belong to — the same empty-means-all that `deployments` uses
  for Vercel teams. Naming `github_actions.accounts` instead fixes the set.
- Repos pushed in the last `pushed_days` that have `.github/workflows`
  files, newest first, capped at `max_repos` *per owner*. Personal and
  each org keep their own newest sixteen (by default). Discovery pages
  each account forty repos at a time, newest first, until the page is
  older than `pushed_days` or it has looked at 200. A look that stops
  early is named on screen, so the eligible count is never presented as
  complete when it is not.

A cap that cuts is named under the headline (`16 of 40 org`), never drawn
as the set. The 48 in `last 48h` is the window, not a run limit: GitHub is
asked for runs `created` in that window, and the request pages until it
has them all. A hundred a repo was the ceiling once, and one repo here
reported `100 most recent of 430` on its own screen — honest, and still
not what somebody opened the pane for.

## Keys

| | |
|---|---|
| `↑` `↓` | select a run — on a run's own screen, scroll |
| `→` `↵` | open the selected run's jobs |
| `←` `esc` | back to the board |
| `c` | copy the open run's URL |
| `/` | filter the list by repo, workflow, branch, event, conclusion or title |
| `s` | cycle the state filter — all / failed / running |
| `w` | cycle the window — 12h / 24h / 48h / 7d |
| `PgUp` `PgDn` `Home` `End` | move the selection by the page, or to either end |
| `r` | refresh now |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the view a line at a time — the pane moves, the selection stays where it is |
| `q` | quit, from either screen |

`j` and `k` move the selection the same way `↓` and `↑` do, as they do on
`deployments`.

## Credentials

Reuses `github.token` in `config.json`, or `$GITHUB_TOKEN`. `github_actions.token`
and `github_actions.token_env` override that when set, then fall back to the github
section. The same classic token `github` and `github-prs` already want — `repo`
and `read:org`. This widget only reads: it does not re-run, cancel, or
write anything.

A missing token is said on the widget's own screen. So is an account list
that yielded no repos with workflows.

## Settings

`github_actions` in `config.json`:

| Key | Default | |
|---|---|---|
| `token` | `github.token` | classic PAT. Empty falls back to the github section. |
| `token_env` | `github.token_env` | environment variable to read when no `github_actions.token` is set. If that variable is set it overrides `github.token`. Empty or unset falls back to github, then `$GITHUB_TOKEN`. |
| `accounts` | discovered | org or user logins. Empty discovers the viewer and every org they belong to. |
| `repos` | `[]` | `owner/name` list. Empty means recently-pushed-with-workflows, capped per owner. |
| `window_hours` | `48` | the labelled window the counts and list cover. `w` cycles it live. |
| `refresh` | `60` | seconds between polls, minimum 30. |
| `max_repos` | `16` | cap on discovered repos *per owner*. Named on screen when it cuts. |
| `pushed_days` | `14` | how recently a repo must have been pushed to be considered. |

Discovery pages each account's recently-pushed repos. Runs are REST,
`GET /repos/{owner}/{repo}/actions/runs`, one request per repo. Jobs are
REST too, and only for the run you open. GraphQL and REST have separate
rate-limit buckets, so the run fetches do not compete with `github` and
`github-prs`, which already spend the GraphQL budget all day.
