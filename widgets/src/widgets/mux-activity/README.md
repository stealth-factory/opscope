# `mux-activity`

[← all widgets](../../../../docs/README.md)

Every pane of the multiplexer it runs in — ordered by who needs you, with one
key to get there. Under [Rex](https://www.superlogical.com/rex/docs/), that is
everything running on the server, across every session.

It replaced `rex-panes`, `herdr-panes` and `luvus-panes`, and those names are
aliases of it: `opscope herdr-panes` starts `mux-activity`, which then reads
whichever multiplexer the pane is in.

```text
╺━ MUX ACTIVITY · REX ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 in Rex · REX_SESSION and REX_BLOCK are set
 3 agents · 2 sessions · 9 panes  1 blocked  1 done  1 unknown
 ███████████████████████▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░░░░░░░░░░░░░░
 ▸ 3 panes waiting for you

 ── AGENTS ── 3
 AGENT       STATE    FOR    CPU   MEM   SESSION
▸⚠ claude    BLOCKED  2m      1%  412M  api:1
   ~/src/api  ◑ api
   permission · approve: cargo test --workspace
 ✓ claude    done     16m     0%  388M  site:2
   ~/work/site  Ok
   replied 'ok' as requested
 ? claude    unknown  ≥12m    0%  193M  work:1
   ~  ◑ opscope
   no status reported

 ── PROCESSES ── 3 panes running something
 COMMAND                   STATE    CPU   MEM   SESSION
 ▪ sleep                   blocked     0%    1M api:1
   reported by terraform · permission · 40% · approve: cargo test · for 3m
   ├ build/test  done  3m
 ▪ cargo                             87%  412M site:2
 ▪ tail                               0%    2M site:2

 ── IDLE ── 1 pane at a prompt
 ▫ ~/work/site                       site:1

 [o]show Opscope  ↑↓ select  ↵ switch to this pane  tab section  [i]dle hide
 [z]oom  [e]xplain  [r]efresh  [,] settings  [q]uit
```

## Work an agent left running

An agent that hands a job to a background shell and goes back to its prompt
reports `idle` or `done` while the job is still going. Each board draws that
work as rows under the agent, from the best source it has:

1. **What the agent reports.** OSC 7501 child records, which Rex carries
   and the Rex board draws under the agent as `├ build/test`. A matching
   child ID replaces the corresponding log row. Report IDs are opaque, so
   numeric components never suppress process rows by coincidence.
2. **Session logs.** Claude Code subagent transcripts and parent launch/finish
   records; Codex child rollouts linked by `parent_thread_id`; OpenCode V2
   child sessions linked by `parent_id` and their persisted busy/idle times.
   Rows say `├ Explore  working · via log`. A Claude child waiting on its own
   background work stays visible until its final stop. Finished children
   leave the mux board; the shared reader retains their state for consumers
   that need it. Older OpenCode stores without persisted liveness say that
   the source cannot be read; a child-session relationship alone is not
   evidence that work is running.
3. **The process table.** A shell the agent started that is still running
   something, read from one `ps -axww -o pid=,ppid=,comm=,args=` per read on
   this machine, and drawn as `├ npm test  running · via process`. Only
   shells count: an agent's MCP servers and its own helpers are its children
   too, but they are started as themselves, and a command run for you goes
   through a shell. A shell that hands itself over to a lone command
   (`bash -c 'sleep 20'` can become `sleep`) leaves no shell to find, and
   that job looks like one of the agent's own helpers, so it is not shown
   rather than every MCP server being shown with it. Under Luvus the agent is found below the pane's root
   process, which is the shell the pane was opened with.

   A job is drawn by its command line, so anything on it that reads as a
   credential is drawn as `***` instead: the login in a URL, a query
   parameter, flag or variable named for a token, key, password or secret,
   curl's `-u`, and an `Authorization:` header. It goes by names, so it
   would rather hide a file after `--key` than show a token after a flag it
   does not know, but a secret passed under a name it does not recognise is
   still drawn. Check a board before screenshotting one with jobs on it.

Logs come from `~/.claude/projects` (`CLAUDE_CONFIG_DIR` when set),
`~/.codex/sessions` (`CODEX_HOME` when set), and
`$XDG_DATA_HOME/opencode/opencode.db` (default `~/.local/share`). No hooks
are installed. Log state is the last recorded lifecycle, not a heartbeat.
A crash that never writes a stop can leave work unfinished in the log.

A Claude agent is bound to its session by its own pid: Claude keeps
`sessions/<pid>.json` for every running process, naming the exact session,
so two Claude panes in one directory each show their own subagents. The
config dir is the agent's, not the widget's: its `CLAUDE_CONFIG_DIR` read from
`/proc/<pid>/environ` on Linux, and on a Mac whichever known config dir holds
a pid file for it. A pid file left by an earlier process with the same pid is
not believed where `/proc` gives a start time to check it against.

Without a pid file - Codex and OpenCode, or a Claude older than that file -
Rex and Herdr supply a cwd rather than an agent session ID, so the reader
uses the newest root session for that cwd. Those rows say `session guessed`;
Rex's explain screen also identifies the chosen session. Luvus uses a
session ID bound to the pane when supplied. Unbound entries from `agent
sessions` cannot establish the parent session; the reader chooses the newest
log for the cwd and labels that match as a guess. Luvus's explain screen gives
the chosen session too. A pid file that cannot be read says so under the
agent beside the guess.

A Rex pane on another machine shows only what its agent reports: local
processes and logs cannot describe it. An unreadable process table or log
source says `├ background work could not be read` and why under the agent.
A matching report ID is required to suppress a log fallback: matching the label
`Explore` would incorrectly hide a different child with the same name.

## Background agents with no pane

A Claude Code background agent (`claude --bg`, managed from `claude
agents`) runs on Claude's daemon, on a terminal no pane holds, so no
multiplexer can report it and no process tree under a pane contains it.
Every board reads it from the file Claude writes for its own dashboard,
`<config dir>/jobs/<id>/state.json`, and draws it the same way under Rex,
Herdr and Luvus: as an agent in a group of its own,
`background agents · via jobs`, sorted with the board's other agents. A
`blocked` one is with the agents that need you and counts in the state bar.

Each row has the state the agent reported (`working`, `blocked`, `done`;
never inferred), how long it has held it from the job's `timeline.jsonl`
(`≥` when that only gives a lower bound), its name, its one-line detail or
what it is waiting for, and the `inFlight` count. Its unfinished subagents
are drawn under it as `├ Explore  working · via jobs`, and the shells under
its process as `via process`. `e` explains it: every field, every entry in
its `fan` - subagents and shells, finished or not - with its age, and the
file it came from. Claude records a finish for a subagent but never for a
shell, so a shell is listed there and never claimed to be running.

A job whose process is gone while its file still says `working` or `blocked`
says so on the row. `done` jobs with no process holding them, and `stopped`
ones, are counted in the group's heading (`2 finished not shown`) rather than
drawn. A `done` job whose process could not be checked (no process table,
or a pid file that did not read) stays on the board and says why. A pid
counts as the job's process only while it is still a Claude, so a pid
reused by another program never lends the job its CPU or shells. A
`state.json` that cannot be read or names a state this widget does
not know is a row of its own with the reason, never an empty group. `Enter`
on one says that there is no pane and that `claude agents` opens it.

Under Herdr, its integration hook runs inside a background agent too and
reports it under whichever pane started the daemon. When that report carries
a session id one of the jobs has gone by, the job is not drawn again: its
subagents join that agent's row instead.

Only this machine's config dirs are read: the default (`CLAUDE_CONFIG_DIR`
or `~/.claude`) and any a running Claude agent was found using. The format
is undocumented and internal to Claude Code; see
[docs/agent-background-work.md](../../../../docs/agent-background-work.md)
for the version it was read from.

## Where it runs

`mux-activity` reads the multiplexer whose pane it was started in. Each one marks
its panes in their environment, and that is what decides. Only the detected
multiplexer's CLI runs, so a read costs what it did in the widget it replaced:

| Multiplexer | Marker | Board |
|---|---|---|
| Rex | `REX_SESSION` and `REX_BLOCK` set | [Under Rex](#under-rex) |
| Herdr | `HERDR_ENV=1` | [Under Herdr](#under-herdr) |
| Luvus | `LUVUS_ENV=1` | [Under Luvus](#under-luvus) |

A program started inside a pane inherits that pane's markers, so Herdr run
from a Rex pane has both. When several are set, the nearest multiplexer above
`mux-activity` in the process tree is the one whose pane it is, and the explain
screen (`e`) says which was picked and why. When the tree names none of them,
`mux-activity` takes Herdr, then Luvus, then Rex, and says to use `--mux`.

Outside all three, `mux-activity` prints which multiplexers it reads and how to
start each, and exits with status 2 before taking over the terminal:

```console
$ mux-activity
mux-activity runs inside Rex, Herdr or Luvus. Open it from a pane in one of those:
  Rex    start rex, then run mux-activity in one of its panes
  Herdr  start herdr, then run mux-activity in one of its panes
  Luvus  start luvus, then run mux-activity in one of its panes
To choose one regardless of where this runs: mux-activity --mux rex|herdr|luvus
```

`--mux rex|herdr|luvus` or the `backend` setting chooses regardless of the
markers, the flag first. The pane's own markers always win over settings
that name a server: `rex_server`, for watching another machine's Rex server,
is used only when `mux-activity` is in no multiplexer at all, as rex-panes always
allowed.

## Under Rex

The preview at the top is the Rex board. Every session on the server, read
with one `rex do` script and woken by `rex events`.

### Why it is ordered this way

On a server with a dozen sessions, an agent finishes or gets stuck where
nobody is looking. **Every section is sorted by who needs a human**, worst
first, and the one waiting longest first among equals:

| Sorts as | Meaning |
|---|---|
| `blocked` | waiting on a permission, a question or a sign-in, right now |
| `error` | failed and stopped |
| `done` | finished work you have not looked at yet |
| `attention` | not a reported state but a signal: rang the bell or sent a notification since you last looked |
| `working` | busy |
| `idle` | at rest, waiting for its next instruction |
| `unknown` | an agent is there and has **not reported** a state — not the same as idle |

The headline counts the panes waiting on you, so `Enter` on the top row is
the whole workflow.

**AGENTS** is every pane holding a coding agent. **PROCESSES** is every other
pane running something, plus any pane holding a status report — a `terraform`
waiting for approval belongs at the top whatever it is. **IDLE** is the panes
at a shell prompt, by directory, because the panel is also how you get around.

**Inside each section, panes are grouped by session** under a `╶ work · 3`
sub-heading, and the row's own column says only the window. The sessions are
ordered by their most urgent pane, so a blocked agent still brings its session
to the top of the section; within a session the order is the same worst-first
one. On a server with one session there is nothing to group, so there are no
sub-headings and the count line names the session instead.

A pane Rex could not be asked about is neither running nor resting: it sorts
into PROCESSES with `⚠ could not be read` and the server's own reason, and the
heading counts it apart. A pane whose process was read but whose status
was not keeps the failure on its own `⚠` row - an unread status is never
drawn as `no status reported`, and never lets a shell rest under IDLE.
`[i]` hides only panes known to be at a prompt.

### How it knows

It reads like the Herdr board on purpose — the same state bar, the same
two-row agent (state, then directory and title), the same footer — so a Rex
pane and a Herdr pane look alike side by side. What only Rex knows goes on the rows under an agent: what it is
blocked on, how far along it is, its message, and its child records. The
server, its version and how many Rex apps are attached are on the explain
screen.

Every fact on screen says where it came from.

- **State is what the program reported, and nothing else.** Rex implements
  OSC 7501, the [Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status):
  a program writes `idle`, `working`, `done`, `blocked` or `error` to its own
  terminal, with a kind (`permission`, `question`, `auth`), a progress, a
  message and child records such as `build/test`. Rex keeps the latest report
  per pane and `program_status` returns it. **Claude Code reports on its own** —
  under Rex it writes `app=claude-code` with every state change and a summary
  message when it finishes; no hook, plugin or setup is needed.
- A program that reports nothing is **`unknown`**, never `idle`. The widget
  does not read a state off a spinner in the title, a word on the screen or
  how busy the process is. The title is shown, labelled as a title, so you can
  read `◑` yourself. `claude agents` — the fleet dashboard — does not report,
  so a pane running it is an agent with an unknown state.
- **What a pane is, is the foreground process Rex names**: `invoked_path`,
  `argv0`, the script an interpreter was handed. That is why a pane reads
  `npm exec opscope@latest` rather than `node`, and why Claude Code is found
  even though its process is named after its version. An agent found that way
  is marked `id via process`; one that named itself in a report,
  `id via report`. Which commands count as agents is the `agents` setting.
- **At a prompt is exact**: the foreground pid is the shell Rex started.
- **Durations of reported states are the server's**: every record carries
  `updated_at`, so `FOR` is exact. A duration with `≥` is one the widget timed
  itself — how long a program with no report has been in front — and was
  already running when the widget started, so it is only a lower bound.
- **A finished agent stays on the board.** The protocol keeps `done` and
  `error` after the program exits, and so does this; a record whose reporter is
  no longer in the foreground says `reporter not in foreground`.
- **`attention`** is a bell or a desktop notification, timed when it arrived
  and cleared once the pane is focused in a session a Rex app is showing, or
  the program has reported again. A session nobody has attached keeps its
  last focused block, so that focus alone does not count as looking.
- **CPU and memory** come from the pids Rex reports, read on this machine —
  `/proc` on Linux, `ps` on macOS — and only when the widget talks to this
  machine's own server. A block on another host, or a server named in
  settings, shows `remote`, and the heading says the server is not here.

One `rex do` script reads the whole server on one connection — every
session, its windows, and each pane's process, title and status — in tens of
milliseconds, where a CLI call per pane would take most of a second. It is
read-only, and `every_rex_script_is_read_only` in `check.rs` fails the build if
it ever calls anything that changes a pane. `rex events --all --json` wakes a
read when a status, a process, a title or the layout changes; events never
patch rows, and when the stream drops the reason is drawn under the header
while the refresh timer keeps reading.

The widget **never starts a Rex server**: every call carries
`--autostart=false`, so with none running it says so instead of conjuring one.
Nothing it says about Rex itself shows a socket path, an address or a login,
because the pane gets screenshotted. A job row is the job's own command line,
which can name an address; a credential it recognises by name is drawn as
`***`, and one it does not is not (see
[Work an agent left running](#work-an-agent-left-running)).

### Keys

| Key | Action |
|---|---|
| `↑` `↓` `j` `k` `Home` `End` | select, across all three sections; the window follows |
| `Tab` | jump to the head of the next section, wrapping past the last |
| `Ctrl-Y` `Ctrl-E` `wheel` `PgUp` `PgDn` | scroll the window; the selection stays where it is |
| `Enter` / `f` | **switch to this pane** — its window becomes active in its session and the pane focused |
| `z` | zoom the pane to fill its window, or undo that |
| `e` | explain the pane: every status record with every field, the program's identity, and the last lines its screen shows; `esc` or `e` goes back |
| `o` | show/hide opscope's own panes (hidden by default) |
| `i` | show/hide the idle section |
| `r` | refresh now |
| `,` | open settings |
| `!` | notices: read warnings not tied to a number, and dismiss them for this session, for 7 days or for 30 |
| `q` | quit |

Focus and zoom are the **server's**: Rex makes the window active and the pane
focused in its session, and every app showing that session follows — so they
work from a headless Linux server. Making an app *switch* to another session
is a client action behind Rex's Remote Control, which this widget does not
attempt; when no app is attached to the pane's session, the note says so.
A detached block is in no window and cannot be focused, and the note says
that too.

## Under Herdr

Everything running under [Herdr](https://herdr.dev), across every workspace —
and one keypress to get to any of it.

```
╺━ MUX ACTIVITY · HERDR ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 in Herdr · HERDR_ENV=1 is set
 5 agents · 3 workspaces   1 done   1 working   3 idle
 ▸ 1 agent waiting for you

 ── AGENTS ── 5
 AGENT    STATE    FOR    CPU   MEM   WORKSPACE
 ✓ grok   DONE     ≥16s     6%  231M  some-cli
   you/some-cli    Investigate the failing test
▸◐ claude working  ≥16s    14%  763M  infra
   work            ◐ Refactor the schedule module
 · claude idle     ≥16s     1%  491M  site
   work/site       Plan the repository structure

 ── PROCESSES ── 7 panes running something
 COMMAND              CPU   MEM   WORKSPACE
 ▪ tailnet              3%   16M  infra
 ▪ pnpm                 0%  116M  some-cli

 ── IDLE ── 13 panes at a prompt
 ▫ work/site                  site
 ▫ …/another-monorepo         monorepo

 ↑↓ select   ↵ switch to this pane   [i]dle hide [l]abels [r]efresh [q]uit
```

### Why it is ordered this way

On a server with a dozen workspaces, agents finish or get stuck where nobody is
looking. **AGENTS is sorted by who needs a human**, not by name:

| State | Meaning |
|---|---|
| `blocked` | waiting on an approval or a question, right now |
| `done` | finished background work you have not looked at yet |
| `working` | busy |
| `idle` | ready for input |
| `unknown` | an agent is present but Herdr cannot classify it — **not** the same as idle |

A headline counts how many are waiting on you, so pressing `Enter` on the top
row is the whole workflow: the blocked agent surfaces, one key puts you in front
of it.

**PROCESSES** covers every other pane actually running something — dev servers,
monitors, builds — with the command, CPU and memory. **IDLE** lists panes at a
shell prompt by directory, because the panel is also how you navigate: a shell
sitting in a repo is somewhere you want to jump to even with nothing running.

There is a third answer, and PROCESSES carries it too. A pane the CLI could
not be asked about is neither running something nor resting: it sorts to the
top with `⚠ could not be read` and the reason on its own row, and the heading
counts it apart — `3 panes running something · 29 could not be read`. It is
kept visible on purpose, and `[i]` hides only panes known to be at a prompt,
because an unreadable pane filtered out of sight is exactly the shape this
widget exists to avoid: a failure that looks like an empty list.

### How it knows

Everything comes from the Herdr CLI, so this is a Herdr client rather than a
general agent monitor:

- `herdr agent list` — the agent inventory and lifecycle states
- `herdr workspace list` — labels, so you see `site` and not `w6`
- `herdr pane list` + `herdr pane process-info` — what every other pane runs,
  paired with `/proc` for real CPU and RSS

**Any agent kind Herdr recognises appears with no code change** — around twenty
of them, including claude, codex, copilot, cursor, antigravity, gemini and grok.
State quality depends on the per-agent integration hook being installed; check
with `herdr integration status`.

**Idle panes are detected exactly**, not guessed: a busy pane's foreground pid
differs from its own shell pid. Command names come from `argv`, so a pane shows
`tailnet` rather than `python3`.

**Durations are marked `≥`** when the state was already in place before the
widget started — we did not see it begin, so it is only a lower bound. Herdr
does not timestamp state changes, so transitions are tracked here.

### When it does not all fit

The three lists read as one under the arrows, and the pane is a window onto
that one list. The header above them is pinned — the counts and the
`▸ N agents waiting for you` line are the reason to have the widget open, and
they never scroll away.

The window follows the cursor: it holds still while the cursor moves inside
it, and moves by as little as it takes when the cursor would leave. It is
measured in **rows**, not entries, because an agent takes two rows — its
second carries the directory and the pane title — while a process takes one.
Counted in entries it admits more rows than the pane has, they are cut off the
bottom, and the cursor goes with them: it kept moving past the last drawn row
and disappeared, while `Enter` still switched to whatever it was invisibly
sitting on.

A heading whose section is not all on screen says so — `── IDLE ── 15 panes at
a prompt · showing 4-15` — and one the window has scrolled clean past says
`none on screen` rather than standing over nothing, which reads as a section
that has failed to load. A section entirely on screen says nothing: a range on
a list you can see all of is noise.

The idle heading is drawn whenever there are idle panes, at every height. It
used to be rationed — granted a heading only if the lists above had left room
— and dropping it silently left the footer offering `[i]dle` with nothing
behind it.

### Keys

| Key | Action |
|---|---|
| `↑` `↓` `Home` `End` | select, across all three sections; the window follows |
| `Tab` | jump to the head of the next section, wrapping past the last |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the window; the selection stays where it is |
| `Enter` / `f` | **go there** — the agent's pane, or the tab holding that process |
| `o` | show/hide Opscope processes (hidden by default; case-insensitive name and command matches) |
| `i` | show/hide the idle section |
| `l` | workspace labels vs pane ids |
| `e` | explain the selected background agent (the only rows with an explain screen here); `esc` or `e` goes back |
| `r` | refresh now |
| `,` | open settings |
| `!` | notices: read warnings not tied to a number, and dismiss them for this session, for 7 days or for 30 |
| `q` | quit |

Agents have a focus-by-id command; other panes do not, so those focus their
*tab* — which brings the pane into view, since a tab tiles its panes.

### Refresh

`refresh` is the maximum idle delay before another authoritative CLI read.
A Unix-domain socket subscription covers workspace lifecycle and rename
events, pane lifecycle events, and agent-status changes on every known pane,
including panes that are not agents. A new pane or a changed inventory
renews those filters; events wake the next full read but never update rows
directly. Acknowledgements, reconnects and finite-window renewal all request
a reconciliation read. CPU and memory sampling stays periodic, and `r`
wakes the CLI poller independently of any socket wait. Socket failures are
shown separately from CLI read failures.

## Under Luvus

Everything running under a [Luvus](https://luvus.dev) session — the agents, the
work they are coordinating, the paths they have reserved, and one keypress to
get to any of it.

```text
╺━ MUX ACTIVITY · LUVUS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 in Luvus · LUVUS_ENV=1 is set
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

 ↑↓ select  ↵ focus  tab section  [i]dle hide  [r]efresh  [,] settings  [q]uit
```

### Why it is ordered this way

On a machine hosting several workspaces, agents finish or get stuck where
nobody is looking. **AGENTS is sorted by who needs a human**, not by name:

| State | Meaning |
|---|---|
| `blocked` | waiting on an approval or a question, right now |
| `done` | finished work you have not looked at yet |
| `working` | busy |
| `idle` | ready for input |
| `unknown` | an agent is present and Luvus classified it as none of those — **not** the same as idle |

Those four are the states the protocol itself declares, so they are Luvus's
words rather than this widget's. `blocked` sits above `done` because blocked
is waiting on you *now* while done is waiting to be noticed.

A headline counts how many are waiting on you, so pressing `↵` on the top row
is the whole workflow: the blocked agent surfaces, one key puts you in front of
it.

### The claim behind each state

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

### What a blocked agent is waiting for

`blocked` is the row the widget sorts to the top and the one it used to say
least about. The row carries the state, how long it has held it, and how it
was decided — but never *what the agent is asking*, which is the question that
sends you out of the widget.

**`e`** opens the evidence behind the selected **agent**, from `luvus
agent explain`. A task, a lease or a pane that is not an agent is told
so rather than sent to a command that cannot explain it:

```text
 ── EXPLAIN ── codex · pane 4

 ⚠ waiting on  approve edit to src/main.rs?
   state       blocked · via integration_report · high confidence
   identity    codex · via integration_report · authoritative
   rule        matched in the body, priority 200
   authority   integration_report

 ── WHAT THE PANE SHOWS ──
 …the tail of the pane, from `luvus agent read`
```

The `authority` row is the one worth reading twice. An integration
*reporting* a state and a rule *guessing* at one from screen text are
different strengths of claim, and where nothing holds authority the row says
`none — the state was inferred, not reported` rather than leaving the field
looking answered.

An agent that is blocked with no hint to give says so too — drawing nothing
there would look like a panel that failed to load rather than a server with
nothing to add.

The panel is **read-only**. It shows you the prompt; it never answers it. `↵`
still focuses the pane this panel is about — not whichever row a hidden
selection would now pick — and nothing here prompts an agent, sends it keys,
claims a task or releases a lease. It fills in off the input loop, so `esc`
and `q` still answer while it is waiting. `esc` closes it, as does a second
`e`.

### Resumable sessions, and whose count it is

RESUMABLE answers for the **machine**, not for this session: a machine with
ten resumable sessions can easily have one workspace open that any of them
are in, the other nine being agents left in directories not covered by any
currently open workspace.

So the total and the in-a-workspace figure beside it count different
populations, and neither is wrong. RESUMABLE draws the full list, because an
agent you left somewhere is exactly the one you have forgotten, and its
heading says how many are in a workspace this session has open so the total
is never mistaken for the session's own. A session whose directory is under
an open workspace counts as inside it. If the snapshot did not come back,
membership is unknown rather than "not open here".

### The focused workspace's checkout

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

### Tasks and leases

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

### Three ways there is nothing to show, and it says which

An empty board is the same picture for three completely different situations,
so each one is named:

| What happened | What the pane says |
|---|---|
| no `luvus` on `PATH` | `cannot start · needs luvus`, with where to get it — the shared dependency screen, before the terminal is taken over |
| a binary, no server for this session | `⚠ no luvus server` / `nothing is serving the session named <name>` / `start one with luvus, or name another session with ,` |
| a server with nothing under it | `▪ session <name> is open and empty` / how many panes it does hold / `start one with luvus agent start` — only when there are no agents, tasks, leases, *or* busy non-agent panes; a running shell is listed under PANES, not called empty |

The second never shows the socket path Luvus names in its own error, because
this repository is public and screenshots of it are not.

### How it knows

Session data comes from its own Universal Harness Protocol 1.0 answers,
through the `luvus` CLI. Nothing here writes:

- `luvus uhp snapshot` — the whole session in one call: workspaces (name, cwd,
  branch) → tabs → panes (id, cwd, focused, what is in it, its state and the
  authority behind it). The pane inventory and the session's own identity.
- `luvus agent list` — **which panes hold an agent**, with the branch,
  project, worktree flag and `state_source` the snapshot does not carry.
- `luvus task list` — the tasks.
- `luvus lease list` — the leases.
- `luvus task next` — whether anything is ready to claim.
- `luvus agent sessions` — resumable sessions on the machine, not just this
  session.
- `luvus git status` — the focused workspace's checkout.
- `luvus worktree list` — every checkout of that repository.
- A bounded local `ps` scan joins process names and full commands to the
  snapshot's root PIDs, including descendants, to identify Opscope panes.
  Argument text is used only for matching; it is not displayed or saved.
  This requires the widget and Luvus session to run on the same machine.
  If the scan fails, a warning says command filtering is unavailable and
  only Luvus's reported command labels are matched.

The list commands print JSON on their own since Luvus 0.14, which refuses
`--json` on `agent list`; an older Luvus that prints a table is asked again
with `--json`.

**`agent list` is what decides who is an agent.** A plain shell sitting at a
prompt looks enough like an agent from the outside that taking the wider
reading would list every idle shell as an idle agent; PANES is what is left
once the real agents are taken out of it.

**Durations are marked `≥`** when the state was already in place before the
widget started. UHP does not timestamp a state change, so a duration is
measured here from the first poll that saw it, and one that began before we
were looking is only a lower bound.

### When it does not all fit

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

### Refresh

`refresh` is the maximum idle delay before another authoritative full read.
One bounded `events.wait` prioritizes `pane.agent_status_changed`, so blocked
or finished agents can appear sooner without spawning a waiter per pane.
Task, lease, pane, Git and worktree-only changes use the configured idle
ceiling instead. The event wait has a one-second minimum and a thirty-second
maximum; the separate poller honors the configured ceiling even below one
second. `r` wakes the poller independently of the event wait.

Events only wake a read: their payloads never patch the board. A lost,
malformed or disconnected event response is visible and requests a fresh
snapshot before its cursor is reset.

### Keys

| Key | Action |
|---|---|
| `↑` `↓` `j` `k` `Home` `End` | select, across the four sections and IDLE when it is shown; the window follows |
| `Tab` | jump to the head of the next section, wrapping past the last — IDLE is one of those sections only while it is on screen |
| `Ctrl-Y` `Ctrl-E` `PgUp` `PgDn` `wheel` | scroll the window; the selection stays where it is |
| `Enter` / `f` | **go there** — focus the selected pane |
| `o` | show/hide Opscope processes (hidden by default; case-insensitive name and command matches) |
| `i` | show/hide the idle section; hiding it also drops it from tab order |
| `r` | refresh now |
| `,` | open settings |
| `!` | notices: read warnings not tied to a number, and dismiss them for this session, for 7 days or for 30 |
| `q` | quit |

`luvus pane focus` jumps to the pane's workspace and tab as well, so one
command covers everything. A row that names no pane — an unclaimed task — says
so rather than focusing something arbitrary.

## Configuration

```json
"mux_activity": {
  "backend": "auto",
  "rex_server": "",
  "luvus_session": "default",
  "refresh": 4,
  "agents": ["claude", "codex"]
}
```

`backend` is `auto`, or `rex`, `herdr` or `luvus` to read that one wherever
`mux-activity` starts; `--mux` does the same for one run. `refresh` is the maximum
idle delay between reads under every multiplexer; events wake one sooner.

- **Rex:** `rex_server` picks a Rex server — a host label from `rex hosts`,
  or an address; empty is the CLI's own default. A second `mux-activity` with
  `rex_server` set is how to watch another machine, and it is only used when
  `mux-activity` is in no multiplexer at all. `agents` is the list of command names
  that count as coding agents — a program reporting under any other app name
  still shows, with its state, in PROCESSES.
- **Herdr:** nothing to set. Herdr names its agents itself.
- **Luvus:** `luvus_session` is which Luvus server session to read;
  `luvus session list` names them, and `default` is the one `luvus` starts on
  its own.

A `rex_panes`, `herdr_panes` or `luvus_panes` section from before `mux-activity` is
still read when there is no `mux-activity` section — `server` as `rex_server`,
`session` as `luvus_session` — and a notice asks for it to be renamed. Press
`,` for the settings screen, which writes the `mux-activity` section.

Requires `rex`, `herdr` or `luvus` on `PATH`: whichever multiplexer it runs
in. Works on Linux and macOS. Under Herdr, CPU and memory are read from
`/proc`, so on macOS those two columns stay empty, as they did in
herdr-panes.

Malformed Codex archive headers do not hide already identified children.
The pane says the session-log inventory may be incomplete. The archive index
is shared across panes for a short interval; selected child bodies remain
current on each read. Rex only attaches local logs to a live, associated
agent process, so an exited reporter cannot adopt a new foreground's logs.

Under Rex, session lookup uses the identified agent process's own working
directory, not the directory of a foreground build/test command. Linux
reads it through core's procfs helpers; macOS makes one `lsof` cwd read for
all identified agents per poll. An unavailable cwd is an explicit source
error. Herdr preserves a bounded source-failure reason even when task rows
overflow the pane.
