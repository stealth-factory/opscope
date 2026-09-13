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
test gated by `cfg(target_os)` (which would vanish from the macOS CI run), an
invalid or unwired `dependencies.json`,
and a widget that opens `/proc` with no macOS path and no explanation.

Every one of them exists because something shipped broken and looked, on
screen, exactly like "there is no data".

## `opscope-core`

`opscope-core` holds the shared pieces — terminal sizing, a full-frame `draw()`,
24-bit colour, a green→amber→red `heat()` / `health()` ramp and the
`heat_on()` / `health_on()` pair that lift its hot end for any composed
tint, `seg()` for clipping coloured
text to a cell budget, `pack_hints()` for wrapping footers and
`pack_hints_placed()` for wrapping them and remembering where each hint
landed, `follow()` for a window that keeps a cursor in view and `row_at()`
for undoing it, non-blocking `Keyboard` input with
arrow-key decoding, `clipboard()` over OSC 52, `unsupported()` /
`cannot_start_because()` when this kernel has no source, and
the dependency warning screen when a required tool is missing. It also owns
the two-tier dependency parser, semver probes, `os-release` distro detection,
native package catalogue, and `opscope doctor` report. Widgets provide the
command, range, platform, and optional reason in their owned
`dependencies.json`; they never choose or invoke a package manager. Core also
owns the shared per-widget settings screen and its order-preserving, private
atomic writer.

### The mouse

A click arrives as a key. That is the whole design: the mouse is a second
route to something a key already does, never a capability of its own, so
there is no second input type and no widget decoding escape sequences.

`Keyboard::poll()` turns an SGR report into `wheel-up`, `wheel-down`, or
`click:<col>,<row>` — zero-based and in the widget's own coordinates, where
`(0, 0)` is the first cell of `rows[0]` as handed to `draw()`. Only the left
button going down becomes a click. The release, a drag, and the middle and
right buttons are eaten rather than acted on: with reporting on, an ordinary
drag selects nothing — Shift-drag, or `"terminal": {"mouse": false}`, gives
the terminal its selection back. An action bound only to the right button
would have no hint, no `--help` line and no doc row, and is invisible to the
check that would have caught that.

A click key is deliberately longer than one character. Every place in this
tree that types an unrecognised key into a filter or a text field guards on
`chars().count() == 1` first, so a click cannot be typed into a search box by
a widget that has never heard of one.

What core can do for a widget divides in two, and unevenly.

**Footer hints cost nothing per hint.** Core packs the footer, so core knows
where every hint went. `pack_hints_placed()` returns the same lines
`pack_hints()` does — a test walks every width from 1 to 60 asserting it —
plus a `HintSpot` for each hint naming exactly one key. Hand the result to
`Keyboard::footer_at(&footer, top, indent)` beside the draw and a click
inside a hint arrives as the key that hint names, through the match arm the
widget already has. Hints added to the footer later are clickable the moment
they are added: the widget registers the footer, not the hints.

A hint is clickable when it names exactly one key, in one of the two forms
that are unambiguous about which characters are the key — `[q]uit`,
`[d] cloudflare`, `[↵] open`, or a bare `↵ → ← ↑ ↓`. The launcher's
`↑↓ select` names two and gets no spot; `[±]25` is one glyph standing for
`+` and `-` and gets none either. A key named only in prose (`esc closes`)
is left alone: recovering it from a sentence means guessing, and a click
that fires the wrong key is worse than one that fires none. The bracket
rules are the four `check.rs` uses, so the two readers agree about what a
footer teaches.

Register every frame. The footer moves when the pane resizes and wraps onto
a second line when it narrows, and a placement kept from an older frame
sends whatever key used to be under the pointer.

**Rows cost one hit-test.** Which rows are selectable is widget state and
nothing in core can see it, so core provides only the arithmetic:
`row_at(y, top, first, shown)` is the inverse of the window `follow()`
chose, and returns `None` outside those rows rather than clamping — a click
on the footer is not a click on the last item. The widget answers a click
that reached it unmatched:

```rust
other => {
    if let Some((_, row)) = tc::click_at(other) {
        if let Some(at) = tc::row_at(row, list_top, list_first, list_rows) {
            selected = at;
        }
    }
}
```

Hit-test against the frame that was on screen when the click happened — the
one built on the previous pass — rather than recomputing the geometry, which
is why the launcher keeps `list_top`, `list_first` and `list_rows` across
iterations.

Tracking itself is asked for in `claim_screen()` unless
`"terminal": {"mouse": false}` says otherwise, and given back on all three
ways out: a normal quit, `SCREEN_RESTORE` in the signal handler, and
`Keyboard::restore()` on the unwind from a panic.

The chart helpers are worth knowing before drawing anything new: `vbars()` and
its mirror `vbars_down()` (pair them on a shared scale for a diverging chart),
`stacked_bar()` for proportions, `meter()` for a gauge, and `skeleton()` for
the shimmer that stands in for a figure still being fetched.

Braille line charts are not among them. `latency` and `link` each keep their
own `braille_canvas`, and the two are not the same function: latency's series
carries the gaps a ping can leave, and link's is told how many slots the axis
holds, so that a session younger than the chart takes its own share of the
width rather than being stretched across all of it.
