# `usage.py`

How much the coding agents on this machine have actually been used — one tab
per agent, read entirely from local state. No network, no credentials.

```
╺━ AGENT USAGE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 local state only · read 16s ago   · = installed
 [CLAUDE]· CODEX · CURSOR · GROK · COPILOT ·

 ── TOTALS ── since 2026-07-16 · cache written 1d ago
  sessions      24           messages      44,417
  output tokens 50.1M        input tokens  863.3k
  cache read    18.7B        cache written 501.6M

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

**Claude Code** carries per-message `usage` and timestamps in its transcripts,
so a rate is derivable — but naively differencing consecutive records gives
nonsense: a quick pass produced a maximum of 21,183 tokens/second, because two
assistant records can be milliseconds apart while one of them reports a whole
turn's output. Doing it properly means reconstructing turn boundaries, and
until that is done the Claude tab shows no rate rather than a wrong one.

**Cursor and Grok** record no tokens at all, so there is nothing to divide.

## The thing this widget does not do

**It does not show remaining quota, for any agent, because none of them publish
one to disk.** Claude Code's file records what was spent and carries no limit
and no reset; the others record nothing at all.

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

None worth measuring. It reads two local files — a small JSON and a read-only
SQLite query — every 30 seconds, and shells out to nothing. It needs no
credentials, so it is the one widget here that cannot leak anything.

## Configuration

```json
"usage": {
  "refresh": 30
}
```
