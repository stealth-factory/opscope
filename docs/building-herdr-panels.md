# Building panels for Herdr

[← all docs](README.md)

Notes from building the widgets in this repo against [Herdr](https://herdr.dev),
a terminal multiplexer for coding agents. None of this is in `herdr --skill`,
which documents the CLI surface; this is what the surface does not tell you.

Run `herdr --skill` for the authoritative command reference — it is emitted by
the installed binary, so it always matches the version you have. Do not copy it
into a repository: it belongs to Herdr's authors and it goes stale.

## Layout

**`pane resize` only ever grows the target.** `--direction` names the side to
expand into; the pane never shrinks. To shrink a pane, grow its neighbour.

**Ratio nudging ripples.** Iteratively resizing to hit target widths does not
converge: adjustments propagate up to the root split and distort unrelated
panes, including ones in other columns. Several attempts to "just nudge it"
left the layout worse than it started.

**Rebuilding a row is reliable; resizing it is not.** Close the row's panes and
re-split from the survivor with explicit ratios. `--ratio` is the fraction kept
by the *original* pane, so a row of widths `[a, b, c]` out of `total` is:

```sh
herdr pane split $first --direction right --ratio $(a/total)   # -> a, rest
herdr pane split $rest  --direction right --ratio $(b/(b+c))   # -> b, c
```

Splitting a pane does not disturb the process running in it, so only the panes
you close need restarting.

**Panes report roughly one column less than their allocated width.** A pane
listed as 45 wide behaves as ~43–44 for layout purposes. Budget accordingly, or
tools with hard minimums will refuse to start for no visible reason.

## Reading panes

**`pane read` returns plain text, not JSON**, unlike almost every other
subcommand. Parsing it as JSON fails confusingly.

**`--lines N` returns the *last* N lines**, not the first. To inspect the top of
a panel, request the full height and slice.

**Alternate-screen programs cannot be scrolled back.** Rows that leave the
alternate screen never enter Herdr's scrollback, so a larger `--lines` will not
recover them.

## The mouse

**Wheel events reach a pane.** A panel that turns on SGR tracking
(`ESC [ ? 1000 h` then `ESC [ ? 1006 h`) gets wheel-up and wheel-down inside a
Herdr pane, in the same `ESC [ < b ; x ; y M` form a bare terminal sends. This
was worth checking before building on it, and it is the reason every widget
here scrolls under the mouse.

**Turn tracking off on every way out, the panic included.** A pane left
reporting outlives the process that asked for it: every later click spits
escape bytes at the shell prompt, from something that has already exited, with
nothing on screen to explain it. That is three exits, not one — the normal
quit, the signal handler, and the `Drop` that runs while a panic unwinds. The
signal handler's copy has to be a pre-built constant, because a handler that
formats, allocates or takes the stdout lock can deadlock against a `draw`
already in flight.

**Tracking costs the reader drag-to-select.** While a program is reporting,
dragging in its pane selects nothing, so copying a line off a panel with the
mouse stops working. Worth a config key so it can be turned off, rather than a
trade made on the reader's behalf.

**`pane send-text` will not deliver an arrow; `pane send-keys` will.** Testing
a panel's key handling from the CLI, `herdr pane send-keys <pane> Down` works
and passing a raw `ESC [ B` through `send-text` does not. Plain characters go
through `send-text` fine.

## Focus

**Only agent panes can be focused by id.** `herdr agent focus <pane>` exists;
there is no equivalent for an arbitrary pane. For a non-agent pane, focus its
tab — a tab tiles its panes, so the pane comes into view.

## Detecting what a pane is doing

**`pane process-info` costs about 5ms**, so polling every pane is cheap: 25
panes cost ~125ms, comfortably inside a multi-second refresh.

**An idle pane is one whose foreground pid equals its own `shell_pid`.** That is
exact, unlike guessing from process names, and it correctly treats a pane
running `bash script.sh` as busy.

**Name processes from `argv`, not the executable.** Otherwise every panel in a
Python-based toolkit reports itself as `python3`.

## Agents

**`herdr agent list` reports 20+ agent kinds** (claude, codex, copilot, cursor,
antigravity, grok, gemini, devin and more). A panel that renders whatever the
CLI reports supports new kinds with no code change — do not hard-code a list.

**Lifecycle states are the useful signal**: `blocked` (waiting on a human right
now) and `done` (finished background work nobody has looked at) are what a
person needs surfaced. `unknown` means an agent is present but unclassified; it
does not mean idle.

**State quality depends on integrations.** `herdr integration status` shows
which agents have their hooks installed; without one, an agent may sit in
`unknown` forever.

**Herdr does not timestamp state changes.** To show how long an agent has been
blocked, track transitions yourself — and mark durations that predate your
process as lower bounds rather than claiming precision you do not have.

## Notifications and clipboard

**`herdr notification show` is gated by config.** It returns
`{"shown": false, "reason": "disabled"}` unless `[ui.toast] delivery` is set to
something other than `"off"` in `~/.config/herdr/config.toml`. Options are
`off`, `herdr` (in-app toast), `terminal` (ask the outer terminal), and `system`
(the OS notification service). On a headless server, `system` fires where nobody
is sitting; `herdr` renders in whichever client the user is attached from.

**Prefer escape sequences, and treat Herdr as an enhancement.** OSC 52 for
clipboard and OSC 9/777 for notifications reach the machine the user is actually
typing at, across SSH, with no dependency on Herdr. Detect Herdr via
`HERDR_ENV=1` and add its native toast on top — never require it.

**Shell out to `herdr` without blocking.** A notification is not worth stalling
a render loop for; fire and forget.

## Writing the panel itself

- Re-read the terminal size every frame. Panes are resized constantly.
- Spend extra width on more content, not padding, and degrade by dropping
  columns rather than truncating them.
- Reserve footer rows structurally. Budgeting space per section drifts, and the
  footer ends up written past the last row where it is never drawn.
- Measure contrast against the selected-row background as well as the terminal
  background; the tint is the harder case and the easier one to forget.
- Restart the pane after editing a script, and confirm the process start time
  against the file mtime before believing a fix is live.
