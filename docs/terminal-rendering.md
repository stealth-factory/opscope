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
`github` and `agent-usage` use separated calendar cells, and `linear` and
`agent-usage` use fractional progress/quota meters. These fall back to ordinary
block characters, squares, dots and reference marks in text sessions.

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

No Kitty executable, image conversion command, shared file, shared memory or
system graphics library is needed. Images are RGBA, zlib compressed in pure Rust,
base64 encoded and sent in chunks of at most 4096 payload bytes. They can travel
over SSH. Stable charts reuse terminal images; changed regions replace their
owned image IDs. Scrolling crops the chart instead of changing its axes. Core
cleans up on screen handoffs, suspend, exit and panic, and recovers on resize or
resume. A process cannot catch SIGKILL.

The first implementation uses an 8×16 pixel raster per cell, scaled to the
placement's terminal cells, compared with Braille's 2×4 dots. It deliberately
does not depend on potentially unavailable cell-pixel-size reports. Up to 16
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
```

Bars do not interpolate between buckets. Calendar cells have transparent gutters
so adjacent days remain distinct. GitHub contributions and agent-usage's Claude,
Codex and Grok calendars all use the same core heatmap renderer: solid 7×14 pixel tiles within each 8×16 cell (one pixel between columns and
two between rows), and solid text blocks. Measured zeroes use a dark fill;
missing days are blank. No outlines or dithering are used. Widgets own their colour ramps and activity scales;
core owns the shapes, spacing and both fallbacks. `Plot::heatmap` remains a
colour-only convenience wrapper using the same renderer with solid tiles.
Meters clamp the drawn fill to 0–100%, while
the widget's numeric label can still report an overage. Reference marks do not
change the measured fill. No primitive animates a value between API polls.

Collect plots in the same coordinate system as the body, then call
`in_viewport` after the scroll offset is clamped. Clip to the body room above
the footer. If a guarded body fails, discard its pending plots together with
its rows, so graphics cannot cover the error message.

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
are implemented only where noted; the opportunities column is future work, not
a claim of shipped behavior.

| Widget | Implemented in this change | Further useful visual work / reason to retain text |
| --- | --- | --- |
| `latency` | Shared pixel/Braille traces; logarithmic scale, gaps and focus preserved | Measure real-terminal readability with many overlapping targets |
| `link` | Shared pixel/Braille charts in list and detail; young-session alignment preserved | Validate dense connections and selected-trace contrast in real Kitty |
| `netwatch` | Shared bidirectional pixel/Braille charts in overview and details, including disk I/O; idle runs stay blank | Profile busy process views with several changing plots |
| `matrix` | Deadline-paced ~30 fps; elapsed-time falling speed and glyph mutation | Keep glyphs as text; rasterizing the entire rain would add bandwidth and lose font rendering |
| `github` | Pixel PR-flow bars on board and account details; contribution heatmap; elapsed-time loading motion | Keep discrete day buckets and side-by-side figures; validate dense account boards |
| `linear` | Pixel created/completed bars and board cycle/project meters; elapsed-time loading motion | Detail-screen meters and state breakdowns remain text; preserve scope and exact percentage labels |
| `github-prs` | Elapsed-time spinners and loading shimmer | Keep review text and tables selectable; enhance shared figures only if it improves legibility |
| `github-actions` | Pixel activity and recent-duration bars; elapsed-time running/loading indicators | Retain outcome colours, exact durations and status text |
| `vercel-deployments` | Pixel deployment activity and recent-build-duration bars; elapsed-time build indicators | Preserve scrolling and text logs; a spinner does not imply measured build progress |
| `agent-usage` | Pixel quota meters on summary; Claude/Codex/Grok daily heatmaps; elapsed-time loading indicators | Individual vendor quota rows remain text; preserve missing days, freshness, overages and pace labels |
| `herdr-panes` | Elapsed-time working indicators | Keep status, selection and navigation as text; decorative motion must not imply extra agent activity |
| `luvus-panes` | Elapsed-time working indicators | Same principle for coordination/evidence tables; verify pane protocol support end to end |
| `clocks` | Synchronized text presentation | Native scaled text needs its own capability and fallback; keep current block digits until that exists |
| `months` | Synchronized text presentation | Date cells are discrete and benefit from stable text; no reason to animate static dates |
| `ports` | Pixel traffic bars in list and port detail, with independent up/down scales and held-history labels | Keep the server table and unmeasured-history dots as text |
| `tailnet` | Synchronized text presentation | Larger history plots could adopt `LineChart`; compact per-row sparklines remain efficient text |
| Launcher | Synchronized updates; session renegotiation after child return | Retain text previews and keyboard navigation |

## Verification and compatibility limits

Run:

```sh
cargo test --workspace --all-targets
cargo build -p opscope-core --example terminal_probe
python3 core/tests/terminal_protocol.py
```

The protocol fixture is explicitly synthetic test data, not a widget. The PTY
tests emulate acknowledged/rejected graphics, synchronized-only and silent
terminals, fragmented/late replies, image errors, cached frames, scrolling,
resize, screen changes, suspend/resume, quit, panic and signal cleanup. Unit tests
cover shared geometry, gap/idle semantics, fractional bar heights, missing-day
markers, quota references, cropping, compression/chunking and input isolation.
The fixture's `g` key shows synthetic bars, a calendar and a quota meter; the
PTY checks exercise both its pixel and text paths. These tests verify emitted
bytes and state, not a terminal
emulator's implementation of them.

An outer Kitty terminal does not establish support inside Herdr, Luvus or tmux.
This implementation requires replies through the actual pane and does not force
passthrough wrappers. A multiplexer that drops queries uses the text path. A
positive reply still needs real visual checks for placement and pane clipping.
Direct Kitty, actual SSH, Herdr, Luvus and tmux visual validation remains a release
check; the managed test workspace has no attached graphical terminal.

The independent Kitty keyboard and text-sizing protocols are not enabled by this
graphics implementation. Legacy keyboard bindings remain the input contract.

### Input responsiveness

Interactive widget loops use `Keyboard::wait(timeout)` between frames. It waits
on terminal input and wakes immediately without consuming keys or capability
replies. The timeout retains the idle refresh cadence, but input no longer waits
behind an unconditional 200–400 ms sleep. Settings uses the same helper; matrix
retains its animation pacing. Buffered partial escapes use a short bounded wait.
Unchanged Kitty plots remain cached when only the selected row changes.
