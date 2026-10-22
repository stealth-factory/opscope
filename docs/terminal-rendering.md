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

Notifications and text sizing are negotiated in the same window, separately
from graphics. A terminal advertising OSC 99 title and body support gets one
notification per alert, with a stable replacement id. `report` is requested
only when the query listed it, and advertised `focus` is turned off with
`-focus`, so a click report does not also focus the pane. An action the
terminal did not list is left off, because an unknown `a` value can drop the
notification. Every other terminal, including one that only speaks OSC 9
(Ghostty among them), still gets the OSC 9 and OSC 777 bytes clocks has always
sent. A semicolon in that fallback is dropped, because OSC 777 uses it to
split the command, the title and the body. Text sizing is OSC 66, detected by
a cursor-position round trip: the width probe must move two columns and the
scale probe two more. `clocks` and
`months` use it only when a scale from 2× to 4× fits the pane. A block that
does not fit stays the block digits or the ordinary heading. `draw` keeps an
OSC 66 block, and that block occupies scale times the width of its text.
Core emits the sequences; widgets do not branch on the terminal.

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
library is needed. Line charts use RGBA (`f=32`); a filled plot uses RGB
(`f=24`, three quarters of the raw size) when every pixel in its visible crop
is opaque. Gaps, fractional edges and the meter inset retain alpha. The direct
path compresses pixels with zlib at level 6 in pure Rust, then base64-encodes
that payload and sends it in chunks of at most
4096 bytes (`t=d`). That path works over SSH.

On Linux and macOS the session also sends one 1×1 shared-memory probe (`t=s`,
`a=q`, reply requested) beside the graphics query. The object is created with
`shm_open`, `ftruncate` and `mmap` from libc, which is already linked. The
name is what the escape carries. The terminal reads the object and unlinks
it. `a=q` does not store the image, so the probe id cannot replace another
image. If the probe is acknowledged, later images of at least 16 KiB
are published as raw RGB or RGBA in shared memory, before any zlib or base64 work.
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
owned image IDs. Scrolling crops the chart instead of changing its axes.
Geometry is worked out on the full chart, then only the visible rows and the
columns that fit the pane are allocated and stroked, coverage buffers included.
Core cleans up on screen handoffs, suspend, exit and panic, and recovers on resize or
resume.

### One new sample

A live line chart that gains samples does not have to be sent again. When
every trace is the old one slid by the same N slots, the scale and the slot
count are unchanged, and that slide is a whole number of pixels, core shifts
the image the terminal already holds and uploads only the new columns. A
trace that matches several shifts counts only when N is one of them.

Kitty refuses to compose a frame onto itself when the rectangles overlap, and
a one-slot shift always overlaps, so the shift is taken from a one-pixel copy
of the root frame and written back onto the root (`a=c`, source offsets `X`/`Y`,
destination offsets `x`/`y`). The copy's replaced pixel sits in the strip the
shift discards. The new columns then replace the right-hand edge of the root
(`a=f` with `r=1`, `x`, `s` and `v`). The join pixel, one left of an N-slot
strip, is part of that upload: the new segment starts there. The oldest
column is uploaded the same way. The segment that just left the chart can
still cover it, and a pure shift would leave that tail behind.

Startup sends a 1×1 probe and a frame compose on it, ahead of the capability
tail. `OK` enables the path. `ENOENT` or `EINVAL`, or no answer, leaves it
off, and every tick stays a full `a=T` upload. A later frame error,
including `ENOSPC` when animation storage is exhausted, does the same and
asks the next frame to send the image again. Graphics stay on. An
`ENOENT`, `EINVAL`, or `ENOSPC` that also names a placement (`p=`) is the
image missing or moving, not a frame error, so animation stays on. A
successful placement acknowledgement carries `p=` too, and is not an error.

A rescale, a cell-size change, a move to another cell,
a slot-count change, a resize, a scroll that changes the crop, or a screen
loss is a full upload too. Changing only whether the image sits under the
text re-places it, and the pixels stay. Text and Braille are unchanged.

The pitch is `(width in pixels − 1) / (slots − 1)`. It is a whole number of
pixels only for some shapes. A netwatch trace on an 80-column pane is 64
cells wide. Its steady window is 128 samples, and 511 is not divisible by
127, so that tick stays a full upload: 3294 bytes for the image command, on
a 4-row one-trace chart at level-6 RGBA. The same width with 74 samples has
pitch 7. There the image command is 2386 bytes and the frame edit is 451,
measured from `compose`. Both figures are the command that carries the
pixels. A full tick also deletes the previous image first; the frame edit
does not.

It is still not a general animation player. Nothing is drawn that the full
raster would not have drawn.

Level 6 is a measurement, not a guess. Release build, mean of 20 compressions,
on synthetic frames at the sizes the widgets use:

| frame | raw | level 3 | level 6 | level 9 |
| --- | --- | --- | --- | --- |
| 100×10 line, 12 traces | 512000 RGBA | 69770 B, 5.0 ms | 48569 B, 12.8 ms | 44863 B, 39.1 ms |
| same chart, 5 rows visible | 256000 RGBA | 35754 B, 2.5 ms | 25256 B, 7.1 ms | 23560 B, 18.7 ms |
| 80×5 line, 1 trace | 204800 RGBA | 1871 B, 0.49 ms | 1942 B, 0.65 ms | 1359 B, 1.5 ms |
| 40×4 bars | 81920 RGBA | 1366 B, 0.25 ms | 564 B, 0.30 ms | 457 B, 0.37 ms |
| 26×7 heatmap with gaps | 93184 RGBA | 1253 B, 0.24 ms | 470 B, 0.26 ms | 463 B, 0.47 ms |
| 26×7 solid heatmap | 69888 RGB | 95 B, 0.14 ms | 95 B, 0.14 ms | 95 B, 0.14 ms |
| 24-cell meter | 12288 RGBA | 141 B, 0.04 ms | 152 B, 0.08 ms | 117 B, 0.10 ms |

On the 12-trace chart, level 6 removes about 30% of the bytes versus level 3.
Level 9 saves little more and triples the time. A one-trace chart and the meter
are a few dozen bytes larger at level 6 than at level 3; the filled plots more
than make that up.

A captured latency chart from actual localhost ping measurements (568×64
RGBA pixels, 145,408 raw bytes) also checks the direct-path choice. In this
release build, 20 compressions averaged 0.680 ms / 896 bytes at level 3,
0.690 ms / 743 bytes at level 6, and 1.078 ms / 744 bytes at level 9. That
frame saves 17% at level 6 for similar time. The multi-trace table above
shows the larger CPU tradeoff; these results do not promise the same saving
for every chart. Shared-memory uploads bypass compression entirely.

Charts rasterise at the terminal's reported cell size. During the 800 ms
negotiation core sends `CSI 16 t` and reads `CSI 6 ; height ; width t`. A
resize (`SIGWINCH`, including a Kitty font change) only records that the
next frame should ask again. That frame sends `CSI 16 t` on the normal
output path, before its image bytes, and only while this process owns the
terminal, so the query cannot split a Kitty chunk or land in a child's
session. `restore_screen` drops a pending query before the handoff. Until
that reply, and when the numbers are missing, zero, or not a
character cell (outside 4×8..=48×96, or an aspect outside 5:4..=3:1), core
uses the pixel fields of `TIOCGWINSZ` divided by the column and row counts.
If those are missing, zero, or not a character cell either, the raster is
8×16 rather than the size from before the resize. That is what Braille's
2×4 dots are compared with and what every in-process test measures. A
reply that already matched this resize is left as reported. The terminal
still places the image in the same cell box; only the raster is finer, so
a HiDPI cell is not an 8×16 image scaled up after the stroke was drawn. A
reply answers the latest query only while it is the only one in flight.
If none arrives for a second and a half, that count is dropped so a lost
reply cannot reject the next one.

A larger cell lowers the per-plot cell cap. At 8×16 a plot may be 20,000
cells (10,240,000 uncompressed RGBA bytes). At 20×40 the cap is 3,200 cells
and the byte product is the same. Thirty-two plots at that cap (327,680,000 raw RGBA bytes) is the frame
ceiling, unchanged from the fixed raster. Bigger or extra plots stay text.
Cell layout is unchanged. It is a raster chart renderer, not a general
replacement for terminal text or native scaled fonts. Frame edits apply only
to the sample slides described above, using the current reported cell size.

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
let plot = tc::Plot::new(chart_x, chart_y, chart_width, chart_height, chart).under_text();
let plots: Vec<_> = plot.in_viewport(scroll, pinned_rows, body_room)
    .into_iter().collect();
tc::draw_plots(&text_rows, width, height, &plots);
```

Inside tmux (`$TMUX` set and non-empty), core does not send XTVERSION.
tmux already asked, and a second answer through the passthrough is delivered
to whichever pane is focused, as keystrokes. Detection reads
`tmux display -p '#{client_termtype}'`, tmux's stored copy of the outer
terminal's XTVERSION. Placeholders are used when that string names Kitty
0.28.0 or newer, Ghostty, or another name on the allowlist, and
`allow-passthrough` is `on` or `all`. If either command fails, the name is
unknown, or passthrough is `off` or unreadable, charts stay on Braille. A
command that has not answered after 250 ms is killed and counts as
unreadable. Both reads run once, at startup. A later reattach from a
different outer terminal, or `allow-passthrough` changing after that, is
not noticed until the widget is started again. With several
clients attached, tmux picks one `client_termtype`. A graphics OK does not decide
this. `a=q` is try-to-load, and `U` has no meaning on that command, so an
old Kitty, WezTerm or Konsole can answer OK and then draw the placeholder
character as tofu. A cursor placement does not survive the multiplexer
either. Herdr (`HERDR_ENV=1`) is not wrapped. Nothing in Herdr's own
behaviour shows that it forwards a tmux DCS passthrough, so Herdr stays on
the direct path.

The upload uses `q=2,U=1`, the quiet mode Kitty specifies for placeholders,
and is written before the placeholder cells. There is no APC reply to read.
Each cell is U+10EEEE with the row and column diacritics and the image id in
the foreground colour. A non-zero high byte of the id, which these ids have,
is a third diacritic. Inside tmux there is no heal and no `a=p`
query. Stock tmux does not forward an APC reply, and its key parser would
type a `q=1` error into the focused pane. The periodic whole frame, every
5 seconds, sends the same `q=2,U=1` upload again. A static chart whose image was dropped
stays blank until that upload fires. A frame that is not a whole frame
leaves an unchanged chart alone. tmux has to allow the passthrough:

```
set -g allow-passthrough on
```

Use `all` instead of `on` for a pane that is not visible. Outside tmux, or
when `client_termtype` does not name a terminal with placeholders, behaviour
stays as it is today.
The cells are ordinary text, so a scroll or a pane move is expected to keep
the image. That was not checked by eye.

`Plot::under_text()` is only for line charts. On a direct Kitty session the
image is placed at `z=-1`: under the glyphs, and above cells that have a
background colour. A lower z, below `-1073741824`, would also sit under that
background, so an opaque selected-row tint would cover the stroke. The widget
draws labels and the current value in the chart cells. The row a label shares
with the stroke is `line_row`: the pixel y the chart rounds against, then the
cell that pixel falls in. Rounding against the cell count instead lands the
label on a different row. Pixel mode leaves the labels there. Inside tmux
those labels stay too. A placeholder cell cannot also hold a glyph, so the
blank cells of the rectangle become the placeholders and the reading is left
where the widget drew it. An uncoloured glyph after a placeholder is reset,
because the placeholder's foreground is the image id. There is still no
`a=p` inside tmux. Braille and text mode still replace those cells with the
chart, so the fallback layout does not move. `under_text()` does nothing for bars, heatmaps,
meters or stacked bars: text over a solid fill does not stay readable, and it
would fail the contrast rule.

The placement is a second graphics command (`a=p` with the same placement id)
after `image()` transmits the pixels at the default z. `image()` is unchanged.
Terminals without Kitty graphics never see the command, and the cells are
drawn as Braille exactly as before.

This was checked by the PTY fixture, which asserts the `z=-1` placement and
that the label cell is not blanked. Kitty, Ghostty and WezTerm were not
available in the build environment, so the z-index under a tinted cell was
not watched on a real terminal.

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
solid tiles fill each entire cell (8×16 pixels only when neither a cell-size
reply nor valid window pixel fields supply a size), matching the solid text blocks.
Measured zeroes use a dark fill;
missing days are blank. No outlines or dithering are used. Widgets own their colour ramps and activity scales;
core owns the shapes, spacing and both fallbacks. `Plot::heatmap` remains a
colour-only convenience wrapper using the same renderer with solid tiles.
Kitty meters fill seven eighths of the cell height (14 of 16 pixels at the
fallback size), with a gap above and below, so quota bars remain readable
alongside text. Meters clamp the drawn fill to 0–100%, while
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
| `latency` | Shared pixel/Braille traces; logarithmic scale, gaps and focus preserved. The focused target's last plotted bucket, or the only target's, sits at the end of its line in pixel mode | Measure real-terminal readability with many overlapping targets |
| `link` | Shared pixel/Braille charts in list and detail; young-session alignment preserved | Validate dense connections and selected-trace contrast in real Kitty |
| `netwatch` | Shared bidirectional pixel/Braille charts in overview and details, including disk I/O; idle runs stay blank. The newest sample's rate sits at the end of each line in pixel mode | Profile busy process views with several changing plots |
| `matrix` | Deadline-paced ~30 fps; elapsed-time falling speed and glyph mutation | Keep glyphs as text; rasterizing the entire rain would add bandwidth and lose font rendering |
| `github` | Pixel PR-flow bars on board and account details; contribution heatmap; elapsed-time loading motion | Keep discrete day buckets and side-by-side figures; validate dense account boards |
| `linear` | Pixel created/completed bars, cycle burnup, board/detail progress and milestone meters, and state distributions | Preserve exact scope/percentage labels; validate dense project details |
| `github-prs` | Pixel opened/merged daily charts and PR-state distribution; elapsed-time loading indicators | Keep review text, counts and tables selectable; unavailable series remain placeholders |
| `github-actions` | Pixel activity and recent-duration bars; elapsed-time running/loading indicators | Retain outcome colours, exact durations and status text |
| `vercel-deployments` | Pixel deployment activity and recent-build-duration bars; elapsed-time build indicators | Preserve scrolling and text logs; a spinner does not imply measured build progress |
| `agent-usage` | Pixel quota meters on summary and all vendor detail quota views; Claude/Codex/Grok gapless daily heatmaps | Preserve missing days, freshness, overages and pace labels; unmeasured allowances remain text |
| `herdr-panes` | Measured agent-state distribution bar; elapsed-time working indicators | Keep status, selection and navigation as text; no invented progress |
| `luvus-panes` | Measured agent-state distribution bar; elapsed-time working indicators | Retain coordination/evidence tables as text; verify pane protocol support end to end |
| `clocks` | Pixel countdown and pomodoro progress/overtime bars; OSC 66 clock face when the block fits | Block digits stay the fallback. A pane too narrow or too short for 2× keeps them, and the time is never cut to make a larger size fit |
| `months` | Synchronized text presentation; OSC 66 month headings when every heading in the band fits | Date cells stay text. One scale for the band, so a long name such as September keeps the row at ordinary size rather than overlapping the next month |
| `ports` | Pixel traffic bars in list and port detail, with independent up/down scales and held-history labels. An active list sparkline is a one-row line chart under the row text; quiet rows stay a dash and unmeasured cells stay dots | Keep the server table and unmeasured-history dots as text |
| `tailnet` | Shared pixel/Braille receive/send history in machine details. Board sparklines that are moving are one-row line charts under the row; a silent direction stays dotted, and the current rates stay beside the traces | Exact current rates stay beside traces |
| Launcher | Synchronized updates; session renegotiation after child return | Retain text previews and keyboard navigation |

## OSC 8 hyperlinks

`tc::link(url, text)` wraps a title when `url` is an `https://` target with
no spaces and no controls. Any other target leaves the sanitized title
unlinked.

Clickable titles have a dashed underline when styled underline support has
been negotiated, or a plain underline otherwise, in both text and Kitty
mode. The mark ends before alignment padding and adjacent columns. A clip
closes both the underline and hyperlink so neither reaches the next column.

External build output is not widget markup. Vercel passes log lines and log
errors through `tc::source_text()` before wrapping: all terminal escapes,
including source-provided OSC 8 targets, are removed. Tabs become spaces.
`link()` sanitizes its visible label the same way, then adds the widget-owned
target. The final row sanitizer also closes any dangling link before the next
row, since an SGR reset alone does not end OSC 8.

All JSON API responses in the five linked widgets use `parse_source_json()`
at acquisition. It removes terminal escapes recursively from string values
before either main or detail views receive them, including commit subjects,
PR titles, issue names and API error messages. Source line breaks and tabs
are retained so first-line selection and text parsing keep their meaning.
HTTP refusal text is sanitized too. This boundary covers raw fields rendered
without `link()`, rather than relying on the detail-label helper alone.

`link()` prefixes the opener with a private per-process capability marker
generated from 128 bits of operating-system entropy. `inert()` accepts an
HTTPS opener only immediately after that marker; source-provided OSC 8 is
stripped in every widget, including copied agent-pane output. A marker is
retained through internal row sanitization and clipping, then removed by
`put_line()` before any bytes reach the terminal. Captured terminal output
therefore has no capability to replay. If entropy acquisition fails, `link()`
returns plain text. The helper's result must be rendered through core, not
printed directly.

The empty OSC 8 closer remains safe to retain. OSC 8 with another scheme,
and every other source OSC, is stripped with the rest of the non-colour
escapes. The sequence is zero width in `display_width`, `pad` and
`seg`. A clip that cuts the visible text appends the closer, so the next
column is not part of the target.

Widgets call `link` only in detail views where a key already copies that same
URL. Main dashboards and lists keep their titles as plain text. In Herdr,
Ctrl + left-click activates a link; regular clicks keep widget behaviour.
Widgets do not probe for OSC 8 and they do not branch on Kitty. A terminal that ignores
the sequence shows the same text; that is the defined fallback, so there is
nothing to negotiate first. Kitty, Ghostty, WezTerm, iTerm2, Konsole and
tmux 3.4+ are the protocol's audience. This workspace has no attached
graphical terminal. The PTY check reads the bytes on the label row, and
where a link sits in a real pane stays a release check, the same limit as
the graphics path above.

## Verification and compatibility limits

Run:

```sh
cargo test --workspace --all-targets
cargo build -p opscope-core --example terminal_probe
python3 core/tests/terminal_protocol.py
```

Set `OPSCOPE_PROBE_ARTIFACTS` to a directory to keep the decoded images the
PTY checks receive, for looking at by eye. RGBA is saved as `.rgba`. An
opaque `f=24` image is saved as `.rgb`, unchanged from the bytes that were
sent, and its `.size` line names that format after the width and height.

The protocol fixture is explicitly synthetic test data, not a widget. The PTY
tests emulate acknowledged/rejected graphics, synchronized-only and silent
terminals, fragmented/late replies, image errors, a shared-memory probe that
fails (the following image stays `t=d`) and one that succeeds (the following
image is `t=s`, and the object is gone after quit and panic; a fatal signal
does not unlink, because that call is not async-signal-safe), cached
frames, scrolling, resize, screen changes, suspend/resume, quit, panic and
signal cleanup. They also answer the keyboard-protocol query and check that
flag 1 is pushed on start, popped on exit, suspend, panic and a typed Ctrl-C,
Ctrl-Z or Ctrl-\, and pushed again after resume. They also emulate an OSC 99
answer and the three cursor reports of the text-sizing probe: a repeat
notification reuses one id, a terminal that did not answer still receives
OSC 9 and OSC 777, and a resize that makes the scaled sample wider than the
pane drops OSC 66. Cell-size checks verify reported dimensions,
window-pixel fallback, invalid/zero reports and a barren resize returning to
8×16; cell queries stay outside image chunks. They also check that HTTPS OSC 8 links are kept while HTTP OSC 8 and OSC 52 are stripped. Unit tests

cover shared geometry, gap/idle semantics, fractional bar heights, missing-day
markers, quota references, cropping, compression/chunking and input isolation,
plus the cell-size parser and the byte cap.
The fixture's `g` key shows synthetic bars, a calendar, a quota meter and a state distribution; the
PTY checks exercise both its pixel and text paths. These tests verify emitted
bytes and state, not a terminal
emulator's implementation of them.

An outer Kitty terminal does not establish support inside Herdr, Luvus or tmux.
Inside tmux, detection reads `client_termtype` once at startup rather than
sending XTVERSION. A later reattach or a later change to `allow-passthrough`
is not noticed until the widget starts again. A hung read is abandoned after
250 ms. Placeholders are used when that string names Kitty 0.28.0 or newer,
Ghostty, or another allowlisted name, and `allow-passthrough` is `on` or
`all`. If either command fails, the name is unknown, or passthrough is `off`
or unreadable, charts stay on Braille. With several clients attached, tmux picks one. Herdr
is not wrapped. tmux needs `allow-passthrough on`, or `all` for a pane that
is not visible. Outside tmux the graphics query is not wrapped. A positive
reply still needs real visual checks for placement and pane clipping. Direct
Kitty, actual SSH, Herdr, Luvus and tmux visual validation remains a release
check; the managed test workspace has no attached graphical terminal. A
scroll or a pane move is expected to keep a placeholder image, and that was
not checked by eye. A static chart whose image was dropped stays blank until
the whole-frame upload fires, every 5 seconds. That upload is not a heal.

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
Windows Terminal, which are the terminals that implement this flag. Text
sizing is negotiated in the same window and is covered below.

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

Text sizing is enabled only after the cursor-position probe, and only for the
clock face and month headings. Legacy keyboard bindings remain the input
contract. These PTY checks verify emitted bytes, not a live Kitty drawing of
the scaled glyphs.

The sizing probe begins with a carriage return: column reports must be
1, 3, 5, so a buffered report beginning elsewhere cannot enable it.
OSC 99 is enabled only when our query identifier is echoed and the terminal
advertises both `title` and `body` payloads. A title-only or incomplete reply
keeps OSC 9/777 delivery. `a=report,-focus` is sent only for the actions
that reply listed, so an unadvertised `report` is not requested and
advertised focus is turned off.

Core-generated OSC 66 blocks carry the same private provenance marker as
core links; the marker is stripped before output. Raw or replayed source
sizing is discarded. Clipping counts the entire scaled block atomically,
and a block that extends beyond the frame width or height falls back to plain
text so it cannot wrap or scroll the pinned title.

Cell size is the xterm `CSI 16 t` report. It is separate from the keyboard
protocol above and from font sizing. Legacy keyboard bindings remain the
input contract. A HiDPI Kitty display has not been checked by eye; the PTY
fixture checks the bytes, not placement on a real screen.

### Input responsiveness

Interactive widget loops use `Keyboard::wait(timeout)` between frames. It waits
on terminal input and wakes immediately without consuming keys or capability
replies. The timeout retains the idle refresh cadence, but input no longer waits
behind an unconditional 200–400 ms sleep. Settings uses the same helper; matrix
retains its animation pacing. Buffered partial escapes use a short bounded wait.
Unchanged Kitty plots remain cached when only the selected row changes.

Cell-size replies carry no request ID. Resume keeps unread queries accounted
for. A missing or ambiguous reply triggers a retry; after expired accounting,
the first apparent response is quarantined and a follow-up query confirms the
current dimensions. Until then, charts use the window pixel fields or the
8×16 fallback rather than accepting a delayed old report. Silent terminals
retry at the 1.5-second query timeout, without blocking input or drawing.
