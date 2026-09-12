# `luvus-panes`

[← all widgets](../../../../docs/README.md)

Everything running under a [Luvus](https://luvus.dev) session — the agents, the
work they are coordinating, the paths they have reserved, and one keypress to
get to any of it.

```text
╺━ LUVUS PANES ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 4 agents · 9 panes   1 blocked   1 done   1 working   1 idle
 session default · uhp 1.0 · 3 workspaces · seq 4186 · focused on pane 2
 ▸ 2 agents waiting for you

 ── AGENTS ── 4
 AGENT    STATE    FOR    WORKSPACE      BRANCH
▸⚠ claude   BLOCKED  4m     some-cli       feature/port-the-parser
   some-cli  state via integration_report · id via process_tree
 ✓ codex    DONE     ≥16m   site           main
   work/site  state via manifest_rule · id via osc_title
 ⠙ claude   working  2m     infra          feature/schedule-module
   work/infra  state via screen_text · id via process_tree
 · gemini   idle     ≥16m   docs           main
   work/docs  state via no_positive_state_evidence · id via command_fallback

 ── TASKS ── 2
 ◆ t-014      claimed     Port the parser to the new envelope  (claude)
 ◆ t-015      ready       Rewrite the lease conflict message

 ── LEASES ── 1
 ⛒ t-014      widgets/src/widgets/ports/**  (claude)

 ── PANES ── 2 panes not at a prompt
 IN THE PANE  STATE    WORKSPACE      DIRECTORY
 ▪ pnpm         working  site           work/site
 ▪ bash         done     infra          work/infra

 ── IDLE ── 3 panes at a prompt
 ▫ work/docs                      docs
 ▫ ~                              home

 ↑↓ select  ↵ focus  tab section  [i]dle  [r]efresh  [,] settings  [q]uit
```

## Why it is ordered this way

On a machine hosting several workspaces, agents finish or get stuck where
nobody is looking. **AGENTS is sorted by who needs a human**, not by name:

| State | Meaning |
|---|---|
| `blocked` | waiting on an approval or a question, right now |
| `done` | finished work you have not looked at yet |
| `working` | busy |
| `idle` | ready for input |
| `unknown` | an agent is present and Luvus classified it as none of those — **not** the same as idle |

Those four are the states the protocol itself declares — they are defined
on the UHP capabilities object, not fetched by a fifth widget call. The
refresh still makes only the four calls above. `blocked` sits above `done`
because blocked is waiting on you *now* while done is waiting to be noticed.

A headline counts how many are waiting on you, so pressing `↵` on the top row
is the whole workflow: the blocked agent surfaces, one key puts you in front of
it.

## The claim behind each state

Every agent row carries a second line saying **how** the state was decided, and
that is not decoration. Luvus declares seven authorities, and they are not
equally strong: `integration_report` is the agent itself saying what it is
doing, `process_tree` is an inference from what is running, `screen_text` is a
guess read off the terminal, and `command_fallback` is little more than the
name of the command in the pane. A screen already flattens four states into one
word; flattening the evidence behind them too would make a guess and a report
look identical.

So the row says `state via manifest_rule · id via process_tree` — the left half
is how the *state* was decided (`state_source`), the right half is how the
*identity* was (`agent_authority`). The right half appears once the pane is
wide enough for it.

## Resumable sessions, and whose count it is

`luvus agent sessions` answers for the **machine**, not for this session. On
the box this was written against it named ten sessions while the session had
one workspace open that any of them were in — the other nine were agents left
in directories nothing currently has open.

`mission.snapshot` reports only the ones inside an open workspace, which is
why it said one where `agent sessions` said ten. The two do not disagree;
they answer different questions. RESUMABLE draws the full list, because an
agent you left somewhere is exactly the one you have forgotten, and its
heading says how many are in a workspace this session has open so the total
is never mistaken for the session's own. A session whose directory is under
an open workspace counts as inside it. If the snapshot did not come back,
membership is unknown rather than "not open here".

## The focused workspace's checkout

The line under the session says what the checkout looks like: branch, how far
from upstream, how many entries are changed, how many stashes, and how many
worktrees the repository has.

It covers **one** workspace and names it. `luvus git status` and
`luvus worktree list` take no workspace argument — only the UHP methods
behind them do — so they answer for whichever workspace the session is
focused on. Drawing those figures without naming the workspace would make
them read as a claim about the whole session that none of them support.

*Entries*, not *files*: git reports a wholly untracked directory as a single
entry, so the count is of what git listed rather than of what is on disk. A
source that did not come back says so on this line, and a directory that is
not a repository is a dash rather than a failure.

## Tasks and leases

Luvus coordinates work across agents in a way Herdr has no equivalent for, and
these are the two sections that show it:

- **TASKS** — claimable units of work (`luvus task add/claim/next/start/done`),
  with whoever has claimed one.
- **LEASES** — the file paths an unfinished task has reserved. This is what you
  want on screen when two agents are about to collide.

**Both are empty on most sessions, and empty is a reading.** `no tasks —
nothing is being coordinated` is an answer; a source that did not come back
says `⚠ could not be read` and the reason instead, on its own row. Those are
opposite readings and the widget will not draw one as the other.

## Three ways there is nothing to show, and it says which

An empty board is the same picture for three completely different situations,
so each one is named:

| What happened | What the pane says |
|---|---|
| no `luvus` on `PATH` | `cannot start · needs luvus`, with where to get it — the shared dependency screen, before the terminal is taken over |
| a binary, no server for this session | `⚠ no luvus server` / `nothing is serving the session named <name>` / `start one with luvus, or name another session with ,` |
| a server with nothing under it | `▪ session <name> is open and empty` / how many panes it does hold / `start one with luvus agent start` — only when there are no agents, tasks, leases, *or* busy non-agent panes; a running shell is listed under PANES, not called empty |

The second never shows the socket path Luvus names in its own error, because
this repository is public and screenshots of it are not.

## How it knows

Everything comes from the session's own Universal Harness Protocol 1.0
answers, through the `luvus` CLI. Eight read-only calls per refresh:

- `luvus uhp snapshot` — the whole session in one call: workspaces (name, cwd,
  branch) → tabs → panes (id, cwd, focused, what is in it, its state and the
  authority behind it). The pane inventory and the session's own identity.
- `luvus agent list --json` — **which panes hold an agent**, with the branch,
  project, worktree flag and `state_source` the snapshot does not carry.
- `luvus task list --json` — the tasks.
- `luvus lease list --json` — the leases.
- `luvus task next` — whether anything is ready to claim.
- `luvus agent sessions` — resumable sessions on the machine, not just this
  session.
- `luvus git status` — the focused workspace's checkout.
- `luvus worktree list` — every checkout of that repository.

**`agent list` is what decides who is an agent, and the snapshot is not.** Every
pane in a snapshot carries an `agent` field, and a plain shell at a prompt
arrives as `agent: "bash"` with `agent_authority: "command_fallback"` — so
reading the snapshot for agents would turn every idle shell into an idle agent.
PANES is what is left once the agent panes are joined out by pane id.

**Durations are marked `≥`** when the state was already in place before the
widget started. UHP does not timestamp a state change, so a duration is
measured here from the first poll that saw it, and one that began before we
were looking is only a lower bound.

Subscribing to the UHP event stream instead of polling is a follow-up, not this
widget: polling the snapshot matches every other widget here and is right for a
first cut.

## When it does not all fit

The four lists read as one under the arrows, and the pane is a window onto the
body built at whatever height it needs. The header is pinned — the counts and
the `▸ N agents waiting for you` line are the reason to have the widget open.
A note line under the body says `rows 12-30 of 46` whenever there is more than
one pane's worth, so a section you have scrolled past never reads as a section
that failed to load.

On a pane too short for both, the header gives its last lines back from the
bottom rather than leaving the body with no rows: a body with nothing in it
looks exactly like a session with nothing in it, and those are opposite
readings of the same screen. The title and the counts never go.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` `j` `k` `Home` `End` | select, across the four sections and IDLE when it is shown; the window follows |
| `Tab` | jump to the head of the next section, wrapping past the last — IDLE is one of those sections only while it is on screen |
| `Ctrl-Y` `Ctrl-E` `PgUp` `PgDn` `wheel` | scroll the window; the selection stays where it is |
| `Enter` / `f` | **go there** — focus the selected pane |
| `i` | show/hide the idle section; hiding it also drops it from tab order |
| `r` | refresh now |
| `,` | open settings |
| `q` | quit |

`luvus pane focus` jumps to the pane's workspace and tab as well, so one
command covers everything. A row that names no pane — an unclaimed task — says
so rather than focusing something arbitrary.

## Configuration

```json
"luvus_panes": { "session": "default", "refresh": 4 }
```

`session` is only worth changing on a machine running more than one Luvus
server; `luvus session list` names them. A session name with no server behind
it draws the "no luvus server" screen rather than an empty board.

Requires the `luvus` command, declared in this folder's `dependencies.json`;
reports plainly when it is unavailable.
