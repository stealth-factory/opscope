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
frames in the Kitty protocol. Pixel traces use a centered two-pixel stroke with
antialiased edges so steep segments retain an opaque core in narrow plots.
Missing measurements still break the trace; they are never interpolated away.

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

The widget owns its axis transform, sample aggregation, labels and colors.
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
| `github` | Elapsed-time loading shimmer and loading-chart motion | A future shared bar/heatmap API could improve activity charts; keep discrete day buckets |
| `linear` | Elapsed-time loading-chart motion | Shared bar and meter primitives could improve throughput/cycle views; never smooth counts into invented measurements |
| `github-prs` | Elapsed-time spinners and loading shimmer | Keep review text and tables selectable; enhance shared figures only if it improves legibility |
| `github-actions` | Elapsed-time running/loading indicators | Shared discrete activity bars and duration plots are candidates; retain status text and exact durations |
| `vercel-deployments` | Elapsed-time build indicators | Same shared activity-bar opportunity as Actions; do not imply measured build progress from a spinner |
| `agent-usage` | Elapsed-time loading indicators | Shared heatmap cells could improve daily usage calendars; preserve day boundaries, missing data and quota labels |
| `herdr-panes` | Elapsed-time working indicators | Keep status, selection and navigation as text; decorative motion must not imply extra agent activity |
| `luvus-panes` | Elapsed-time working indicators | Same principle for coordination/evidence tables; verify pane protocol support end to end |
| `clocks` | Synchronized text presentation | Native scaled text needs its own capability and fallback; keep current block digits until that exists |
| `months` | Synchronized text presentation | Date cells are discrete and benefit from stable text; no reason to animate static dates |
| `ports` | Synchronized text presentation | A text table is the useful view; avoid animation that makes stable rows look active |
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
cover shared geometry, gap/idle semantics, cropping, compression/chunking and
input isolation. These tests verify emitted bytes and state, not a terminal
emulator's implementation of them.

An outer Kitty terminal does not establish support inside Herdr, Luvus or tmux.
This implementation requires replies through the actual pane and does not force
passthrough wrappers. A multiplexer that drops queries uses the text path. A
positive reply still needs real visual checks for placement and pane clipping.
Direct Kitty, actual SSH, Herdr, Luvus and tmux visual validation remains a release
check; the managed test workspace has no attached graphical terminal.

The independent Kitty keyboard and text-sizing protocols are not enabled by this
graphics implementation. Legacy keyboard bindings remain the input contract.
