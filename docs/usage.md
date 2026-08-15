# `usage.py`

How much the coding agents on this machine have actually been used — one tab
per agent, from each agent's own local state, plus one live quota call.

```
╺━ AGENT USAGE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 local state only · read 16s ago   · = installed
 [CLAUDE]· CODEX · CURSOR · GROK · COPILOT ·

 ── SUMMARY ── all time · since 2026-07-16
 Favorite model  opus-5      Total tokens    20.6B
 Sessions        31          Longest session 4d 10h 52m
 Active days     28/30       Longest streak  21 days
 Most active day Aug 1       Current streak  4 days
  Input 891.2k · Output 53.6M · Cache read 20.1B · Cache written 501.6M

  Your input and output are ~75x the tokens in War and Peace

 ── BY MODEL ── output tokens
  opus-5                27.9M ████████████████████████████
  opus-4-8              15.0M ███████████████░░░░░░░░░░░░░
  fable-5                4.2M ████░░░░░░░░░░░░░░░░░░░░░░░░
  sonnet-5               3.0M ███░░░░░░░░░░░░░░░░░░░░░░░░░

 ── MESSAGES / DAY ── 26d · peak 6,216
                                ██
   ▃▃          ▁▁            ▇▇██▁▁▄▄
 ▃▃██▃▃▅▅▆▆▄▄▄▄██▂▂▂▂▆▆██████████████
 ─────────────────────────────────────
 07-16                           08-12

 ── TOKENS / DAY ── peak 3.3B on 08-01
      07-13 07-20 07-27 08-03 08-10
 Mon    ·     ░░    ▒▒    ▒▒    ·
 Tue    ·     ░░    ▒▒    ░░    ░░
 Wed    ·     ░░    ░░    ▒▒    ░░
 Thu    ░░    ░░    ░░    ░░    ·
 Fri    ▒▒    ▒▒    ▒▒    ░░    ·
 Sat    ░░    ░░    ██    ░░    ·
 Sun    ·     ░░    ▒▒    ░░    ·
  less ░░▒▒▓▓██ more
```

## Why tabs

Because the agents do not agree on what "usage" means. One counts tokens,
another counts the lines it wrote, and several publish nothing at all outside
their own session. A single table would need a shared schema that does not
exist; the columns would be mostly empty and the empties would look like zeros.

A tab per agent lets each show its own shape — and lets an agent that exposes
nothing **say so**, which is the honest answer and more useful than a blank
gauge.

`←` `→` or `tab` switch. The active tab is bracketed as well as tinted, so it
reads without colour. A `·` marks an agent that is installed.

## What each tab can actually show

**Claude Code** — the real one. `~/.claude/stats-cache.json` carries per-model
token counts (input, output, cache read, cache written), total sessions and
messages, and around four weeks of daily activity. All of it is spend.

The summary block mirrors Claude Code's own `/stats`, from the same file:
favourite model by output tokens, total across all four token kinds, sessions,
longest session, active days, both streaks and the most active day. Rendering
it against a `/stats` screenshot taken the same week agrees on every figure the
cache had settled — 31 sessions, a longest session of `4d 10h 52m`, a longest
streak of 21 days, Aug 1 as the busiest day.

The comparison line divides input plus output by **730,000 tokens**, a rough
count for *War and Peace*. The constant is named in the source, because a
comparison built on an unnamed number is just a number with a story attached.

The **tokens-per-day calendar** is laid out like the contribution calendar in
`github.py` — weekdays down the side, weeks across — so the two read the same
way on one wall.

The only difference is cell width. That pane spans a year, so its cells are one
character and its columns go unlabelled; there is no room for fifty-two dates.
Four weeks of retained history can afford wider cells and a date over each
column, and the widget picks the cell width from the pane. Intensity is carried
by the shading glyph as well as the colour, and a `·` marks a day the file has
no entry for — distinct from a day that recorded zero.

**Cursor** — `ai-tracking/ai-code-tracking.db` records *authorship*: how many
edits the agent made, across how many conversations, and how many lines in
scored commits came from the agent versus by hand. That is a different question
from cost, and the tab says so rather than letting it pass as usage.

**Codex** — real, and it took a second look to find. `~/.codex/logs_2.sqlite`
is diagnostics with no counters, which is where the first search stopped. The
answer is in `~/.codex/sessions/**/*.jsonl`: every rollout carries
`event_msg / token_count` events with both a running total and the last turn's
usage, timestamped.

That gives totals *and* an **output rate** — output tokens divided by the
wall-clock gap between turn boundaries. On this machine: 659.9M input, 1.3M
output across 28 sessions, and a median of 30 tok/s in the newest one.

The running total is repeated on every event, so only the tail of each rollout
is read — some are 30MB and re-reading them every refresh to learn a number
printed at the end would be daft.

**Copilot** — it *does* keep usage locally, in `~/.copilot/session-store.db`.
The `assistant_usage_events` table carries per-turn input, output, cache and
reasoning tokens, AI credits as `total_nano_aiu`, a `request_multiplier`, and —
uniquely among these agents — `duration_ms`, `time_to_first_token_ms` and
`inter_token_latency_ms`. It is the best-shaped usage data of the lot.

It is simply **empty** on this machine, so the tab says so rather than drawing
an empty chart. The moment the CLI is used here it has real numbers, and better
ones than anywhere else.

**Grok** — nothing found. `grok du` reports **disk** use, not quota; the name
is a coincidence worth not falling for.

## On tokens per second

Two agents can answer it, and they answer different questions.

**Codex** is computed here: output tokens over the wall-clock gap between turn
boundaries. That includes tool calls and thinking, so it is a *throughput*
figure and not raw decode speed — the pane says so under the chart rather than
letting the number imply more precision than it has.

**Copilot** would be exact, once there is data: `inter_token_latency_ms` and
`time_to_first_token_ms` are recorded per turn, so no inference is needed.

**Claude Code** is computed too, and getting it right took two attempts. A turn
is a `user` record followed by an `assistant` one, and the rate is that
assistant's output tokens over the gap between them. Measuring from *any*
previous record instead inflates it wildly — two assistant records can be
milliseconds apart while the second reports a whole turn's output, which
produced a maximum of 21,183 tokens/second.

Even with the right boundary a few gaps remain impossible — 1,073 tokens in
0.07 seconds among them — where the timestamps plainly do not bracket
generation. So **only the median and p90 are shown, never a maximum**: the
median sits at 74–75 however the outliers are trimmed, which is the reason to
trust it, while the maximum moves from 15,328 to 800 on the same data, which is
the reason not to publish one.

Transcripts run to tens of megabytes, so it samples the tail of the three most
recently touched.

**Cursor and Grok** record no tokens at all, so there is nothing to divide.

## Codex quota is the exception: it is real, live and account-wide

Every other number here is local consumption. Codex publishes an actual
**remaining quota**, and two ways to get it.

The rollouts record a `rate_limits` snapshot the server sends back with each
response — `used_percent`, the window length, and `resets_at`. That is real but
only as fresh as the last time Codex ran.

Better, and what the widget prefers: the same endpoint the Codex CLI itself
uses. Read the OAuth token from `~/.codex/auth.json` and

```
GET https://chatgpt.com/backend-api/wham/usage
Authorization: Bearer <token>
```

which returns `plan_type`, `credits`, and a `rate_limit` with primary and
secondary windows. The header says `live` or `from the last session` so it is
never ambiguous which you are looking at.

This method came from reading how [CodexBar](https://github.com/steipete/CodexBar)
does it — a menu-bar app that does this for twenty-odd providers, and documents
the endpoint. Its `codexbar-cli` would cover far more of them; it is not used
here because the widget stays dependency-free, but it is the obvious thing to
reach for if this ever needs to cover providers whose numbers are not on disk.

**This is the one place the widget touches the network or a credential.** The
token goes only to the host Codex itself talks to, is never printed, and any
failure — expired token, no network — falls back to the rollout snapshot
rather than showing nothing.

## The thing this widget does not do

**It shows no remaining quota for Claude Code, Cursor or Grok**, because none
of them publish one. Claude Code's file records what was spent and carries no
limit and no reset. (Codex is the exception, above.)

Showing "73% of your limit" would mean inventing the denominator. The pane says
what was spent and stops there — which is the whole point of the repo, and the
reason the empty tabs are empty rather than full of plausible zeros.

If a future release of any of these writes a limit to disk, or exposes one over
an API, the tab has somewhere obvious to put it.

## Keys

| Key | Action |
|---|---|
| `←` `→` / `tab` | switch agent |
| `r` | re-read the files now |
| `q` | quit |

## Cost

Small. Local files are read every 30 seconds; Codex rollouts are parsed once
each and cached on mtime and size, because one is 29MB and a finished rollout
never changes. The only network call is the Codex quota one.

## Configuration

```json
"usage": {
  "refresh": 30
}
```
