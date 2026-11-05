# `rex-panes`

[← all widgets](../../../../docs/README.md)

Everything running under a [Rex](https://www.superlogical.com/rex/docs/)
server, across every session — ordered by who needs you, with one key to get
there.

```text
╺━ REX PANES ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
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

## Why it is ordered this way

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

## How it knows

It reads like `herdr-panes` on purpose — the same state bar, the same two-row
agent (state, then directory and title), the same footer — so the two sit
side by side. What only Rex knows goes on the rows under an agent: what it is
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
It never shows a socket path, an address or a login, because the pane gets
screenshotted.

## Keys

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

## Configuration

```json
"rex_panes": { "server": "", "refresh": 4, "agents": ["claude", "codex"] }
```

`server` picks a Rex server — a host label from `rex hosts`, or an address;
empty is the CLI's own default. A second `rex-panes` with `server` set is how
to watch another machine. `refresh` is the maximum idle delay between reads;
events wake one sooner. `agents` is the list of command names that count as
coding agents — a program reporting under any other app name still shows,
with its state, in PROCESSES.

Requires `rex` on `PATH`. Works on Linux and macOS.
