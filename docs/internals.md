# Internals

[← all docs](README.md)

What is shared between the widgets, and what the test suite checks that a
compiler cannot. For the rules these implement, see
[design conventions](design.md).

## The checks

`cargo test` from the root runs each widget's tests plus
`widgets/tests/check.rs`, which reads the sources and fails on a poller that
dies without saying why, a footer or `--help` line naming a key nothing
answers, a hint missing from the widget's README, a config key read but never
declared in its `settings.json` — or declared there and never read, or read
with no fallback behind it — a stale generated `config.example.json`, a
colour that draws text on the selected-row tint below WCAG AA, an incomplete
widget folder, a widget missing from the README table or docs index, a
name in the launcher's sample listing that is not a widget, a parser or a
test gated by `cfg(target_os)` (which would vanish from the macOS CI run),
and a widget that opens `/proc` with no macOS path and no explanation.

Every one of them exists because something shipped broken and looked, on
screen, exactly like "there is no data".

## `opscope-core`

`opscope-core` holds the shared pieces — terminal sizing, a full-frame `draw()`,
24-bit colour, a green→amber→red `heat()` ramp, `seg()` for clipping coloured
text to a cell budget, `pack_hints()` for wrapping footers, `follow()` for a
window that keeps a cursor in view, non-blocking `Keyboard` input with
arrow-key decoding, `clipboard()` over OSC 52, `unsupported()` /
`cannot_start_because()` when this kernel has no source, and
`cannot_start()` when a required tool is missing. It also owns the shared
per-widget settings screen and its order-preserving, private atomic writer;
widgets provide only their section name, optional legacy alias, and owned
`settings.json`.

The chart helpers are worth knowing before drawing anything new: `vbars()` and
its mirror `vbars_down()` (pair them on a shared scale for a diverging chart),
`stacked_bar()` for proportions, `meter()` for a gauge, and `skeleton()` for
the shimmer that stands in for a figure still being fetched.

Braille line charts are not among them. `latency` and `link` each keep their
own `braille_canvas`, and the two are not the same function: latency's series
carries the gaps a ping can leave, and link's is told how many slots the axis
holds, so that a session younger than the chart takes its own share of the
width rather than being stretched across all of it.
