# Terminal rendering and Kitty graphics

Tracked in [OPS-32](https://linear.app/stealth-company/issue/OPS-32/kitty-terminal-support-across-the-widgets).

Opscope uses the same binaries in Kitty and ordinary terminals. Core negotiates
graphics and synchronized updates independently after entering cbreak mode.
Until a terminal acknowledges a capability, the existing text path runs. Startup
does not wait for replies. The negotiation window is 800 ms; late replies are
consumed without enabling a capability or becoming shortcut keys.

Synchronized updates bracket each changed frame, allowing a supporting terminal
to present it together. This benefits every widget and the launcher, including
text-only sessions. Unchanged frames write nothing. Existing changed-row drawing
and five-second recovery repaints remain in place.

`latency`, `link` (list and connection details), and `netwatch` (overview,
process, selected host/connection and disk charts) use the shared chart renderer.
It selects antialiased pixels for an acknowledged Kitty graphics session and
Braille otherwise. Samples, gaps, scales, time windows and selection stay the
same. Higher resolution does not imply more samples or faster polling.

`ports` adds pixel traffic bars; `github`, `github-actions`,
`vercel-deployments`, and `linear` add discrete pixel bar charts.
`github` and `agent-usage` use gapless calendar tiles, and `linear` and
all `agent-usage` vendor quota views use fractional progress/quota meters.
`github-prs` adds daily activity charts; `tailnet` adds detail traffic traces.
`herdr-panes` and `luvus-panes` show measured agent-state distributions,
and `clocks` adds countdown and pomodoro meters. These use shared text fallbacks:
block bars, Braille traces and reference marks.

## Choosing a renderer

```sh
OPSCOPE_GRAPHICS=auto opscope latency   # default: negotiate, fall back to text
OPSCOPE_GRAPHICS=text opscope latency   # no graphics query; keep synchronized updates
OPSCOPE_GRAPHICS=kitty opscope latency  # negotiate; explain an unavailable Kitty path
```

The variable is inherited by widgets opened through the launcher. `kitty` does
not bypass detection. Unsupported explicit requests show a notice in the title
row and use text. An image transmission error also disables pixels for the
session and shows why; re-entering the terminal session renegotiates support.

### Force text rendering on Kitty-capable terminals

Set `OPSCOPE_GRAPHICS=text` when starting Opscope to use Braille/text charts even
when the terminal supports Kitty graphics:

```sh
OPSCOPE_GRAPHICS=text opscope latency  # one widget
OPSCOPE_GRAPHICS=text opscope          # launcher and every widget it opens
```

If you start widgets through a local `preview` command, prefix that command too:

```sh
OPSCOPE_GRAPHICS=text preview
```

These prefixes apply only to that command and its children. To keep text mode
for the current shell session, run `export OPSCOPE_GRAPHICS=text` before launching
widgets. Run `unset OPSCOPE_GRAPHICS` and restart them to restore automatic
renderer selection. Text mode retains the same data and controls, and still
uses synchronized updates when supported.

## Image transport and lifecycle

No Kitty executable, image conversion command, shared file or system graphics
library is needed. Images are RGBA. The direct path compresses them with zlib
in pure Rust, then base64-encodes that payload and sends it in chunks of at most
4096 bytes (`t=d`). That path works over SSH.

On Linux and macOS the session also sends one 1×1 shared-memory probe (`t=s`,
`a=q`, reply requested) beside the graphics query. The object is created with
`shm_open`, `ftruncate` and `mmap` from libc, which is already linked. The
name is what the escape carries. The terminal reads the object and unlinks
it. `a=q` does not store the image, so the probe id cannot replace another
image. If the probe is acknowledged, later images of at least 16 KiB (32 cells)
are published as raw RGBA in shared memory, before any zlib or base64 work.
The terminal maps those pixels directly. Smaller images keep the compressed
direct path: for tiny bars, object creation can cost more than compression.
The probe-result parser is ordinary code, compiled and tested on every
target. Acquisition is limited to Linux and macOS. Anywhere else, and
whenever the probe is rejected, times out, or a later image reports an
error, the session stays on `t=d` and keeps drawing pixels. A shared-memory
failure does not turn graphics off. A cleanup table with no free slot sends
that one image directly and leaves shared memory on. An OS publication failure
or a shared-upload error reported by the terminal returns the session to `t=d`.

Owned image uploads request a reply (`q=0`) and carry a unique placement ID
(`p=`), which the terminal echoes. Replies match that ID rather than queue
position, so a missing, late or out-of-order reply cannot be attributed to a
newer upload. A table of eight owed uploads does not discard one to make
room: that frame is sent directly and shared memory stays available. The cap
removes a slot only when that upload is superseded. Expired keys, and a
reply that arrives after resume has cleared the table, are ignored. `EBADF`
still identifies a shared-memory read failure even when its upload is no
longer in the table, and it does not turn graphics off. Placements and
deletions stay quiet.

An object the terminal does not take is retired, then unlinked once a short
read window has passed, so a replacement that reuses the image id is not
collected by a reply still in flight for the previous object. Darwin reports
the object size rounded up to a page; the escape carries `S=` with the exact
payload length, and the bytes are read by mapping the object. `read` is not
used: Darwin shared-memory descriptors reject it.

Exit and panic unlink from ordinary cleanup, the same path that deletes
placements. A signal handler does not. `shm_unlink` is not async-signal-safe,
so SIGINT, SIGTERM and SIGTSTP only disarm the transport and restore the
terminal; a fatal signal can leave an object behind, as SIGKILL can. Suspend
unlinks on resume, after the handler has returned.

Stable charts reuse terminal images; changed regions replace their
owned image IDs. Scrolling crops the chart instead of changing its axes. Core
cleans up on screen handoffs, suspend, exit and panic, and recovers on resize or
resume.

The first implementation uses an 8×16 pixel raster per cell, scaled to the
placement's terminal cells, compared with Braille's 2×4 dots. It deliberately
does not depend on potentially unavailable cell-pixel-size reports. Up to 32
visible plots, each at most 20,000 cells, use images; additional or larger plots
use text to bound image memory/transport costs. It is a raster chart renderer,
not a general replacement for terminal text, native scaled fonts or animation
frames in the Kitty protocol.

## Widget API

Widget authors do not branch on Kitty. Build a `LineChart` with `Trace`s and
register its body rectangle as a `Plot`. Reserve that rectangle in the text
layout; core fills it in either mode. Labels, legends and axes remain text.

```rust,ignore
let chart = tc::LineChart {
    slots: samples.len(),
    traces: vec![tc::Trace {
        // Fractions measured from the bottom; None means a missing sample.
        values: samples.iter().map(|value| value.map(|v| v / axis_max)).collect(),
        positions: None, // evenly spaced samples; see timed traces below
        colour: tc::rgb(50, 220, 170),
        baseline: None,
    }],
    focus: None,
};
let plot = tc::Plot::new(chart_x, chart_y, chart_width, chart_height, chart);
let plots: Vec<_> = plot.in_viewport(scroll, pinned_rows, body_room)
    .into_iter().collect();
tc::draw_plots(&text_rows, width, height, &plots);
```

For discrete data, use the other `Plot` constructors with the same viewport and
drawing path:

```rust,ignore
// One column per bucket; an explicit peak keeps paired charts on one scale.
let upward = tc::Plot::bars(x, y, &columns, 3, peak, false);
let downward = tc::Plot::bars(x, y + 4, &other_columns, 3, peak, true);
// Each day is Some((truecolor, level)) or None. Levels: 0 zero, 1..=4 activity.
let calendar = tc::Plot::heatmap_levels(x, y, &days, &missing_colour);
// Fractions, with an optional elapsed-window marker. Keep exact values in text.
let quota = tc::Plot::meter(x, y, width, used, elapsed, &fill, &track, &marker);
// Measured category shares; keep their counts and legend in ordinary text.
let states = tc::Plot::stacked_bar(x, y, width, &shares);
// Smooth horizontal fills, keeping the same fractions and marker geometry.
let smooth_quota = tc::Plot::gradient_meter(x, y, width, used, elapsed, &fill, &track, &marker);
let smooth_states = tc::Plot::gradient_stacked_bar(x, y, width, &shares);
```

Bars do not interpolate between buckets. GitHub contributions and agent-usage's
Claude, Codex and Grok calendars all use the same gapless core heatmap renderer:
solid tiles fill each entire 8×16 pixel cell, matching the solid text blocks.
Measured zeroes use a dark fill;
missing days are blank. No outlines or dithering are used. Widgets own their colour ramps and activity scales;
core owns the shapes, spacing and both fallbacks. `Plot::heatmap` remains a
colour-only convenience wrapper using the same renderer with solid tiles.
Kitty meters fill 14 of each row's 16 pixels, with one pixel of space above
and below, so quota bars remain readable alongside text. Meters clamp the drawn fill to 0–100%, while
the widget's numeric label can still report an overage. Reference marks do not
change the measured fill. `Plot::ramped_meter` takes a colour per filled
cell, so a ramp the text bar draws is painted in pixels too rather than
flattened to one fill. `gradient_meter` and `gradient_stacked_bar` instead
let core own a smooth ramp: one colour per text cell, continuous interpolation
per Kitty pixel, with the same readable colour stops. Stacked gradients restart
inside each category and retain the measured boundaries. The original `meter`
and `stacked_bar` remain solid for widgets that need that treatment.
`Plot::text_row(0)` exposes the exact fallback cells for composing labels or
alert frames without duplicating gradient or marker logic. No primitive
animates a value between API polls.

Collect plots in the same coordinate system as the body, then call
`in_viewport` after the scroll offset is clamped. Clip to the body room above
the footer. If a guarded body fails, discard its pending plots together with
its rows, so graphics cannot cover the error message.
For nested sections, `append_plot_rows(&mut rows, &mut plots, |local_plots| ...)`
appends returned text and offsets section-local plots by the existing row count.
It publishes neither if the section panics. Apply the viewport transform once,
after all sections are assembled. Stacked bars take normalized fractions that
sum to one; pixel edges preserve subcell shares and text uses whole-cell shares.

The widget owns its axis transform, sample aggregation, labels and colors.
For timed traces, set `Trace::positions` to horizontal fractions in `0..=1`,
one per value. Positions with no observation are omitted; an explicit `None`
value is a discontinuity. The shared geometry applies this distinction in
both Kitty pixels and Braille. Equal positions can draw a vertical segment.

Latency uses sparse bucket positions: reply-time jitter can leave an arrival
bucket empty without losing a ping, so an empty bucket does not break the line.
Each populated bucket aggregates its successful replies, even if it also contains
losses. A bucket containing only losses leaves a gap. Loss counts and events still
include every recorded loss; the bucket width and numeric statistics are unchanged.

All widget chart paths were reviewed for this distinction. `link` and `netwatch`
use ordered sample traces; `ports` and `tailnet` use ordered traffic samples.
GitHub, Actions, PRs, Linear and Vercel activity buckets represent event counts,
where empty buckets are legitimate zeroes. Calendar missing-day markers remain
explicit, and the other widgets do not bucket measured line traces. No global
interpolation or fill-forward rule is applied to these different data types.
The chart owns right alignment, line interpolation between adjacent measurements,
gap preservation, focused-trace priority and both rasterizers. `baseline` omits
idle runs, while preserving transitions to/from activity; netwatch uses this for
zero traffic. Do not use interpolation to manufacture operational readings.

`Plot::in_viewport` takes the same scroll offset and pinned header height as the
text body; its `room` argument excludes the header and footer. It returns no plot
when the region is offscreen. Core rejects plots on the title row and still
sanitizes all widget text. Never embed graphics escapes in row strings or write
around `draw_plots`. `draw` remains the text-only API and clears former images
when a settings screen or other view takes over.

For animation, `animation_tick()` advances from monotonic elapsed time at 100 ms
intervals. Use it for spinners and loading decoration; source polling remains
independent. `FramePacer` provides deadline-based pacing for continuously animated
views. Matrix uses about 30 frames/second and elapsed-time movement; pauses do not
cause a burst of catch-up frames. API dashboards retain their existing loop and
poll rates, so this change does not promise 30 fps for those views.

## Widget review

All entries below receive capability-gated synchronized updates. Pixel charts
are implemented where useful below. The remaining column records validation
and intentional text presentation, not unimplemented widget migrations.

| Widget | Implemented in this change | Further useful visual work / reason to retain text |
| --- | --- | --- |
| `latency` | Shared pixel/Braille traces; logarithmic scale, gaps and focus preserved | Measure real-terminal readability with many overlapping targets |
| `link` | Shared pixel/Braille charts in list and detail; young-session alignment preserved | Validate dense connections and selected-trace contrast in real Kitty |
| `netwatch` | Shared bidirectional pixel/Braille charts in overview and details, including disk I/O; idle runs stay blank | Profile busy process views with several changing plots |
| `matrix` | Deadline-paced ~30 fps; elapsed-time falling speed and glyph mutation | Keep glyphs as text; rasterizing the entire rain would add bandwidth and lose font rendering |
| `github` | Pixel PR-flow bars on board and account details; contribution heatmap; elapsed-time loading motion | Keep discrete day buckets and side-by-side figures; validate dense account boards |
| `linear` | Pixel created/completed bars, cycle burnup, board/detail progress and milestone meters, and state distributions | Preserve exact scope/percentage labels; validate dense project details |
| `github-prs` | Pixel opened/merged daily charts and PR-state distribution; elapsed-time loading indicators | Keep review text, counts and tables selectable; unavailable series remain placeholders |
| `github-actions` | Pixel activity and recent-duration bars; elapsed-time running/loading indicators | Retain outcome colours, exact durations and status text |
| `vercel-deployments` | Pixel deployment activity and recent-build-duration bars; elapsed-time build indicators | Preserve scrolling and text logs; a spinner does not imply measured build progress |
| `agent-usage` | Pixel quota meters on summary and all vendor detail quota views; Claude/Codex/Grok gapless daily heatmaps | Preserve missing days, freshness, overages and pace labels; unmeasured allowances remain text |
| `herdr-panes` | Measured agent-state distribution bar; elapsed-time working indicators | Keep status, selection and navigation as text; no invented progress |
| `luvus-panes` | Measured agent-state distribution bar; elapsed-time working indicators | Retain coordination/evidence tables as text; verify pane protocol support end to end |
| `clocks` | Pixel countdown and pomodoro progress/overtime bars | Keep clock digits as text; native scaled text needs separate capability negotiation |
| `months` | Synchronized text presentation | Date cells are discrete and benefit from stable text; no reason to animate static dates |
| `ports` | Pixel traffic bars in list and port detail, with independent up/down scales and held-history labels | Keep the server table and unmeasured-history dots as text |
| `tailnet` | Shared pixel/Braille receive/send history in machine details | Compact board sparklines remain efficient text; exact current rates stay beside traces |
| Launcher | Synchronized updates; session renegotiation after child return | Retain text previews and keyboard navigation |

## Verification and compatibility limits

Run:

```sh
cargo test --workspace --all-targets
cargo build -p opscope-core --example terminal_probe
python3 core/tests/terminal_protocol.py
```

Set `OPSCOPE_PROBE_ARTIFACTS` to a directory to keep the decoded images the
PTY checks receive, for looking at by eye.

The protocol fixture is explicitly synthetic test data, not a widget. The PTY
tests emulate acknowledged/rejected graphics, synchronized-only and silent
terminals, fragmented/late replies, image errors, a shared-memory probe that
fails (the following image stays `t=d`) and one that succeeds (the following
image is `t=s`, and the object is gone after quit and panic; a fatal signal
does not unlink, because that call is not async-signal-safe), cached
frames, scrolling, resize, screen changes, suspend/resume, quit, panic and
signal cleanup. They also answer the keyboard-protocol query and check that
flag 1 is pushed on start, popped on exit, suspend, panic and a typed Ctrl-C,
Ctrl-Z or Ctrl-\, and pushed again after resume. Unit tests
cover shared geometry, gap/idle semantics, fractional bar heights, missing-day
markers, quota references, cropping, compression/chunking and input isolation.
The fixture's `g` key shows synthetic bars, a calendar, a quota meter and a state distribution; the
PTY checks exercise both its pixel and text paths. These tests verify emitted
bytes and state, not a terminal
emulator's implementation of them.

An outer Kitty terminal does not establish support inside Herdr, Luvus or tmux.
This implementation requires replies through the actual pane and does not force
passthrough wrappers. A multiplexer that drops queries uses the text path. A
positive reply still needs real visual checks for placement and pane clipping.
Direct Kitty, actual SSH, Herdr, Luvus and tmux visual validation remains a release
check; the managed test workspace has no attached graphical terminal.

## Keyboard protocol

Core asks `CSI ? u` in the same negotiation as graphics and synchronized
updates, before the primary device-attributes query, and it does this in text
mode too. A reply of `CSI ? <flags> u` inside the 800 ms window means the
terminal speaks the [Kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/).
Those flags are the mode currently in force, which is 0 until something
pushes, so any well-formed reply is enough. The two replies are independent:
device attributes do not answer the keyboard query, and a flags reply that
arrives after them, still inside the window, is support. Core then pushes
disambiguation only (`CSI > 1 u`, flag 1). A late reply, or a window that
ends with no keyboard reply, leaves the legacy decoder in place. Widgets
do not branch on this.

Flag 1 sends the Escape key as `CSI 27 u`. That sequence is finished, so the
key is delivered in the same poll instead of waiting to see whether a bare
ESC was the start of something else. A bare ESC is still held for one poll:
it is how every other sequence begins, and a torn arrow has to stay an arrow.
Ctrl, Alt and the editing keys are decoded from `CSI … u` into the names the
legacy bytes already produce — `esc`, `enter`, `tab`, `backspace`, `ctrl-y`,
`ctrl-e`, `ctrl-u`, and the character a legacy ESC prefix already yielded for
Alt. That includes a printable non-ASCII key: Alt+é is `é`, the same name as
ESC followed by é. Alt+`[` arrives as `[` instead of sitting in the buffer as
an unfinished control sequence. Keypad enter, digits and navigation use those
same legacy names (`enter`, `0`-`9`, the arrows, `home`, `end`, `pgup`,
`pgdn`). Release events are ignored, and so are functional keys this program
does not bind. Match arms in the widgets are unchanged, and so are text and
Braille.

Flag 1 reports Ctrl chords as `CSI … u` instead of the bytes the terminal
driver would have interpreted. Interrupt, quit and suspend follow the termios
saved when the keyboard was taken: they raise SIGINT, SIGQUIT and SIGTSTP
only when `ISIG` is set and the reconstructed byte is `VINTR`, `VQUIT` or
`VSUSP`. A disabled character (`_POSIX_VDISABLE`) or a remap such as
`intr = ^X` is honoured. With the usual defaults that is still Ctrl-C,
Ctrl-\ and Ctrl-Z (`CSI 99;5u`, `CSI 92;5u`, `CSI 122;5u`). SIGQUIT uses the
same restore path as SIGINT. The handler then calls `_exit(128 + SIGQUIT)`,
status 131. That is a normal exit, not a death by signal: a waiter does not
see `WIFSIGNALED`.

Software flow control follows that same saved termios. When `IXON` is set, a
report whose byte is `VSTOP` or `VSTART` is not handed to the widget as a key.
Core holds frames instead of stopping the tty: `draw` returns without writing,
and the same thread keeps reading. `VSTART` releases the hold and the next
draw paints the current frame. `VINTR` still raises SIGINT while output is
held. `tcflow` is not used. It would block the next redraw on this thread, and
on Linux a typed start character does not undo a programmatic stop. `IXON` is
checked before `ISIG`, which is the order the line discipline uses when one
byte is configured as both. With the usual defaults those keys are Ctrl-S and
Ctrl-Q. When `IXON` is clear, the same reports are ordinary keys.

`CSI < u` pops the mode on the same paths that already delete images: normal
exit, panic (dropping `Keyboard`), handing the terminal to another program,
and suspend (`SIGTSTP`). Resume queries again and pushes only if the terminal
answers again. A signal that exits pops as well, so the shell is not left
disambiguating. The push, the pop and the flag that records them are one
transition on the calling thread. `pthread_sigmask` blocks the signals that
pop until the flag and the bytes have both been updated. It does not block
those signals in any other thread.

The PTY fixture answers the query and checks the push and the pop. It does
not stand in for a real Kitty, Ghostty, WezTerm, iTerm2, foot, Alacritty or
Windows Terminal, which are the terminals that implement this flag. The
text-sizing protocol is not enabled.

## Underlines

Tracked in [OPS-151](https://linear.app/stealth-company/issue/OPS-151/use-styled-and-coloured-underlines-to-mark-stale-partial-and-over), part of [OPS-147](https://linear.app/stealth-company/issue/OPS-147/build-the-next-round-of-kitty-terminal-improvements).

A value can carry an underline as a second signal beside its colour. Widgets call `tc::marked` and do not write the SGR themselves.

| Mark | Meaning | When the terminal acknowledges it |
|---|---|---|
| curly | the last poll is older than its interval | `CSI 4:3` |
| dashed | a window that has not completed, or a figure carried forward | `CSI 4:5` |
| coloured | a threshold this value has crossed | `CSI 58;2;255;214;140` on top of the underline |

`(255, 214, 140)` clears WCAG AA 4.5 against black and against the selected-row tints. The digits and labels do not change.

Startup sends an XTGETTCAP query for `Smulx` and `Setulc` with the graphics and synchronized-update queries. The same 800 ms window applies, and a late reply does not enable the styles. Until the terminal answers, the text stays as it was. A refusal, or a window that closes with no answer, falls back to a plain underline (`CSI 4`), which is what a terminal without underline styles already draws and which stays legible. Underline colour is sent only after `Setulc` is acknowledged; otherwise the plain underline keeps the existing foreground.

`latency`, `link` and `github-actions` use the marks. Each widget's README says which of its numbers takes which one. Agent-usage keeps its colour and textual freshness signals and does not use underlines in text or Kitty mode. The reusable `marked()` / `Marks` API is opt-in for future widgets; the contributor guide shows how to use it.

`seg()` underlines the content of a marked segment, leaving its leading and trailing whitespace unmarked without removing any alignment cells. For ` -35% `, only `-35%` takes the mark. Internal spaces in a phrase remain underlined, and the following segment never inherits the mark. The core regression test checks the SGR state at each character, including clipped values, all-padding segments and Unicode whitespace.

The PTY fixture checks the query, a positive reply (curly, dashed, and the coloured underline around the same three words) and a negative reply (a plain underline, same words). That is a byte check, not a Kitty window. Visual confirmation in Kitty, SSH, Herdr, Luvus and tmux is still outstanding.

### Input responsiveness

Interactive widget loops use `Keyboard::wait(timeout)` between frames. It waits
on terminal input and wakes immediately without consuming keys or capability
replies. The timeout retains the idle refresh cadence, but input no longer waits
behind an unconditional 200–400 ms sleep. Settings uses the same helper; matrix
retains its animation pacing. Buffered partial escapes use a short bounded wait.
Unchanged Kitty plots remain cached when only the selected row changes.
