# `agent-activity`

[← all widgets](../../../../docs/README.md)

Local coding-agent processes and the child work below them, without a multiplexer.

```text
╺━ AGENT ACTIVITY ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 2 named agent processes · 1 open log rows · 1 observed shell jobs
 Claude Code · Codex · OpenCode — local processes
 Log sessions are newest-cwd guesses; states are last recorded.

▸ claude · pid 420 · process present
  cwd /srv/project
   ├ Explore  working · via log · session guessed
   ├ cargo test  running · via process
   latest finished: 1 of 1 in matched session
   └ Review  done · via log · 2026-11-11T12:00:00Z

  codex · pid 560 · process present
  cwd /srv/another-project
   no matching session log or observed shell jobs

↑↓ select  [↵]sources  [r]refresh  [,]settings  [q]quit
```

The preview is illustrative. Running the widget shows only measured local
processes and recorded lifecycle states. A named process is not proof of an
active turn. The board supports Claude Code, Codex and OpenCode; it does not
include cloud agents or remote machines.

## Sources and limits

`tc::agent_work` owns discovery, interpreter-wrapper collapse, MCP filtering and
the child lifecycle readers shared with mux-activity. One `ps` read finds
agent processes and child shells. Linux gets each agent's own working
directory through `tc::procfs`; macOS makes one `lsof` cwd read for all
identified processes. Foreground jobs' working directories are not used.

Subagents come from Claude parent/child JSONL transcripts, Codex dated
rollouts, and OpenCode's read-only V2 SQLite session store. Rows say
`via log` or `via process`. A standalone process has no multiplexer OSC
feed; this view says so. Session matching uses the newest root for the
agent cwd and is explicitly a guess. Concurrent sessions in one directory
may be misattributed, and a shared guessed session can repeat under multiple
processes; the header counts observed log rows, not unique tasks. Enter shows the session ID and association explanation.

Source details mask credential-shaped command arguments with the shared
shell-job labeler. Local paths and session text can still be private.

Log states are the **last recorded lifecycle**, not a heartbeat. Unknown
states remain unknown. OpenCode V1 cannot establish persisted liveness and
shows that limitation. A shell that execs directly into a lone command can
be indistinguishable from an MCP helper and is left out by the shared
reader. Counts cover observed rows, not all work the agent could be doing.

Source failures remain beside the affected process and never erase
process-backed shells. A failed process poll retains the previous snapshot
with a visible stale warning. No matching session, no processes, initial
loading, and a failed read have different messages.

Finished work is bounded: up to `finished_per_agent` most recent completed
log children in each matched session of a **live parent process**. Older
sessions and exited parents are excluded. This is not total history; there
is no finished-process-shell history. Times are source update timestamps;
unrecorded times sort last and say so, rather than claiming a completion time. Set the limit to zero to hide finished
rows. [Shared reader research](../../../../docs/agent-background-work.md).

## Keys

| Key | Action |
| --- | --- |
| `↑` / `↓`, `k` / `j` | Select an agent; scroll in source details. |
| `↵` / `enter` | Open the selected agent's source details; no external action. |
| `esc` | Return from details. |
| `r` | Refresh now. |
| `,` | Open shared settings. |
| `q` | Quit. |
| Wheel, `ctrl-y` / `ctrl-e` | Scroll the whole body without moving selection. |
| `pgup` / `pgdn` | Scroll a page. |
| `home` / `end` | Scroll to either end. |
| `ctrl-k` | Open commands. |
| `ctrl-t` | Open the theme picker. |
| `!` | Open notices. |

A click selects an agent; clicking the selected agent opens the same details
as Enter. Selection survives refreshed ordering. Only selection keys/clicks
follow the cursor; wheel scrolling leaves it where it was.

## Configuration

Section `agent_activity`: `refresh` defaults to 2 seconds;
`finished_per_agent` defaults to 3 (0–100). Press `,` to edit. `-n SECONDS`
or `--refresh SECONDS` overrides the poll interval for one launch. No tokens
or account lists are required. Paths follow the shared reader's standard
environment variables; see [CONFIGURE.md](CONFIGURE.md).

Text uses core's theme roles and their lifted forms on the selected tint;
core measures AA contrast for both. Long content wraps, and a short pane
scrolls the full body with its title pinned.
