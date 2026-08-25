# `herdr-panes.py`

Everything running under [Herdr](https://herdr.dev), across every workspace —
and one keypress to get to any of it.

```
╺━ HERDR PANES ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
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
 ▪ tailnet.py           3%   16M  infra
 ▪ pnpm                 0%  116M  some-cli

 ── IDLE ── 13 panes at a prompt
 ▫ work/site                  site
 ▫ …/another-monorepo         monorepo

 ↑↓ select   ↵ switch to this pane   [i]dle [l]abels [r]efresh [q]uit
```

## Why it is ordered this way

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

## How it knows

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
`tailnet.py` rather than `python3`.

**Durations are marked `≥`** when the state was already in place before the
widget started — we did not see it begin, so it is only a lower bound. Herdr
does not timestamp state changes, so transitions are tracked here.

## When it does not all fit

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

## Keys

| Key | Action |
|---|---|
| `↑` `↓` `Home` `End` | select, across all three sections; the window follows |
| `Enter` / `f` | **go there** — the agent's pane, or the tab holding that process |
| `i` | show/hide the idle section — `o` in the Python, which is being retired |
| `l` | workspace labels vs pane ids |
| `r` | refresh now |
| `q` | quit |

Agents have a focus-by-id command; other panes do not, so those focus their
*tab* — which brings the pane into view, since a tab tiles its panes.

## Configuration

```json
"herdr_panes": { "refresh": 4 }
```

Polling every pane is cheap: `process-info` costs about 5ms, so 25 panes add
~125ms per refresh.

Requires `HERDR_ENV`; reports plainly when the CLI is unavailable.
