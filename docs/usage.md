# `usage.py`

How much the coding agents on this machine have actually been used — one tab
per agent, read entirely from local state. No network, no credentials.

```
╺━ AGENT USAGE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 local state only · read 16s ago   · = installed
 [CLAUDE]· CURSOR · COPILOT · CODEX · GROK ·

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

**Cursor** — `ai-tracking/ai-code-tracking.db` records *authorship*: how many
edits the agent made, across how many conversations, and how many lines in
scored commits came from the agent versus by hand. That is a different question
from cost, and the tab says so rather than letting it pass as usage.

**Copilot, Codex, Grok** — nothing readable. Each tab names where the number
really lives instead:

- Copilot shows AI credits in its session footer and via `/usage`; there is no
  CLI subcommand, and the REST endpoints for a personal plan return 404.
  Organisation-level Copilot metrics *do* have an API, but that is a widget
  about a team rather than about you.
- Codex keeps sessions, history and a logs database, but the logs are
  diagnostics — level, target, module — with no usage counters.
- Grok keeps sessions and config. `grok du` reports **disk** use, not quota;
  the name is a coincidence worth not falling for.

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
