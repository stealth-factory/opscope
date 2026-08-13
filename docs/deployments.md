# `deployments.py`

Vercel deployments — how they are going over time, not just what shipped last.

```
╺━ VERCEL DEPLOYMENTS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 200 deploys · 20 proj  179 ready  21 error   0s ago
 ↑↓ select · [c]opy · [r]efresh [f]ilter [p]roject [q]uit

 ── ACTIVITY ── deploys/hour, last 48h
 ····▂▂·▃▃▄▄···················▂▄▆▄▃▃▃▄▂▃▃············▂▃·▃▂▂▄·▂▄█▃▂▃
 48h ago                                                    peak 6/h

 ── BUILD TIME ── median 3m27s  p95 3m56s  max 4m08s
 ▂▂▄▁▁▁▁▇▇█▇▇▇█▇▇▇▇▁▇▇▇▇▇▇▇▇▇▇▇▇██▇▇▇▇▇▇▁▁▁▁▇█▇▁▇▇█▇▇▇▇▇▇▇█▇▇▇▇

 ── RECENT ── 1 of 200
▸● Ready    site        3m18s  17m  prev  bedec8d feature/new-timetable
   fix(a11y): announce the answer when it loads
 ● Ready    site        2m52s  49m  prev  cb4d954 feature/new-timetable
   docs: record why the default-pin guard needs no pair tracking
 ✖ Error    marketing     18s   2h  prev  0c72b3d main
   chore(deps): bump the build toolchain
```

## Sections

**Activity** — deployments per hour over the last 48h, each bucket coloured by
its worst outcome, so a failure is visible in the timeline rather than needing
to be hunted for.

**Build time** — median, p95 and max across the fetched window, with a sparkline
of recent durations so drift is visible.

**Recent** — one row per deployment: state, project, build duration, age,
preview/production, commit SHA and branch, with the commit subject beneath. Live
builds get a spinner and a running elapsed time.

## Copying links

`c` or `Enter` opens a copy sheet for the selected deployment with the four URLs
worth having:

```
 [1] Deployment dashboard   vercel.com/org/project/A51x3neL…
 [2] Branch preview         project-git-branch-org.vercel.app
 [3] Commit preview         project-hunohka96-org.vercel.app
 [4] Pull request           github.com/org/project/pull/535
```

Copying uses **OSC 52**, so your local terminal performs it and the text lands
on the clipboard of the machine you are typing at — the only mechanism that
works across SSH. Links a deployment does not have (a push with no PR) are
omitted, and the sheet says how many are missing rather than silently dropping
them. Every URL is shown wrapped in full, so mouse selection still works where
OSC 52 is blocked.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` `PgUp` `PgDn` `Home` `End` | move the selection |
| `c` / `Enter` | copy sheet for the selected deployment |
| `1`–`4` | inside the sheet, copy that URL |
| `f` | filter — all / failed / production |
| `p` | cycle which project is shown |
| `r` | refresh now |
| `q` | quit |

## Layout

The list is progressive rather than fixed: under 66 columns it shows the
essentials, above that the commit SHA and branch appear, and from 110 columns
the metadata and commit subject share one line — twice as many deployments in
the same height.

## Credentials

Reuses the Vercel CLI's own login: if `vercel whoami` works, this does.
`$VERCEL_TOKEN` is checked first, then the CLI's `auth.json`. **The token is
read locally and never printed.**

`vercel ls --all --format json` returns comparable data, but spawns a Node
process per refresh — measured at 1433ms for 3 records against 756ms for 100
over the REST API, since Node startup dominates. Hence the API.

Fetching runs on a background thread, so a slow or failed poll keeps the last
good data on screen behind an error banner rather than freezing the panel.

## Configuration

```json
"deployments": {
  "refresh": 15,
  "limit": 100,
  "teams": [],
  "projects": []
}
```

Empty `teams` discovers every team you can see; empty `projects` shows all.
Polling every 15s is 4 requests/min per team.

```sh
./deployments.py                    # every project, 15s
./deployments.py -n 60 my-project   # one project, slower
```
