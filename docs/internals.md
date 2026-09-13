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
invalid or unwired `dependencies.json`, a widget that does not answer the
wheel, a footer that is drawn and never registered so nobody can click it,
a widget with a cursor that ignores a click,
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
landed, `follow()` for a window that keeps a cursor in view with `item_at()`
and `rows_clicked()` for undoing it, non-blocking `Keyboard` input with
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
nothing in core can see it, so core provides only the arithmetic.

`item_at(y, head, scroll, placed)` reads a frame row back into an item, where charts,
section headings, blank spacers and multi-line rows sit between the items
so the nth row is not the nth item. The widget records where each item's
rows landed while building the body — which is the bookkeeping it already
does for its cursor, `cursor = Some(rows.len())`, extended to every item —
and one entry per row an item occupies, so a click on a sparkline picks the
row above it. Matched exactly rather than to the nearest, so a click on the
blank under a short list selects nothing. `luvus-panes` was already keeping
exactly this, as `spans.push(start..body.len())`, to bring its window to the
selected entry; read the other way round it is a click map.

A widget with sections whose cursors are separate — `linear` — records
`(row, slot)` and keeps the `(section, index)` pairs beside it, so
`item_at` stays as it is. Clicking into a section focuses it as well as
moving its cursor, because that is what walking into it with the arrows
does. Where the sections share one index — `herdr-panes`, `luvus-panes` —
there is nothing extra to do.

**The shared settings screen is clickable too.** Its three modes each hand
back what the frame offers a click — where each row landed, what the window
did, and the packed footer — and `run_settings` registers it after the draw
and resolves clicks before the next keys. Clicking a row picks it; clicking
it again is `enter`, which opens the editor, cycles a boolean or takes a
choice, exactly as the key does. It still clears the registration on the way
in and out, because it borrows its caller's `Keyboard` and polls before it
draws. It is core's screen rather than a widget's, so the two checks never
walk it — a unit test asserts one placement per field, read off the frame
rather than recomputed.

**A screen that draws without a list of its own clears the placements**, the
same way a screen with no packed footer calls `forget_footer()`. Leaving
them answers a click on a detail screen with whichever item happened to be
drawn on that row behind it.

**`rows_clicked` is the one a widget calls.** It resolves a frame's clicks
before the keys are matched: a click on another row moves the cursor there,
and a click on the row it is already on is rewritten to the literal key
`enter` — the key the footer already names for opening one.

```rust
let mut keys = keyboard.poll();
if let Some(at) = tc::rows_clicked(&mut keys, Some(selected), head, scroll, &placed) {
    selected = at;
    moved = true;
}
for key in keys { match key.as_str() { /* unchanged */ } }
```

Rewriting the key rather than acting on it is the whole point. The widget's
own `enter` arm does the opening, so the click cannot drift from what the
keyboard does — it *is* what the keyboard does. A widget with no `enter`
arm (`latency`, `clocks`, `matrix`) gets the right behaviour for free:
nothing.

It reads the running cursor rather than the one passed in, so two clicks on
the same row inside one poll behave like two clicks. A click that only moved the cursor is
replaced with an empty key, which nothing matches because every arm taking
an arbitrary key guards on `chars().count() == 1` first.

**`off_list` says what a click that misses every row becomes**, and it is a
decision each widget makes rather than a default. `latency` passes
`Some("esc")`: its selection can be empty, its footer hints
`[esc] clear focus`, and so clicking away from the host list is a second
route to a key already named. Every other widget passes `None`, because
their `esc` closes a detail screen or drops a filter — a click on a chart
that shut the screen would be a capability nobody asked for.

**Not a double-click.** An SGR report carries a button and a cell and never
a click count, so recognising one means holding the last press's cell and
timestamp and inventing a threshold — state on the input path, and a
tunable nobody can see. The affordance is better this way round too: the
selected row is tinted, so a reader can see that the next click will open
it. A double-click shows nothing before it fires.

Hit-test against the frame that was on screen when the click happened — the
one built on the previous pass — rather than recomputing the geometry.

The placements have to index the same vec the frame is built from. Splitting
one `rows` into a pinned head and a windowed rest satisfies that for free;
keeping the header in a separate vec and assembling `head ++ body[window]`
does not, and `luvus-panes` is the one widget shaped that way. Its spans are
`body`-relative, so they are shifted by `head.len()` when recorded — without
it every click landed a header's height further down and the first entries
could not be reached at all.

Tracking itself is asked for in `claim_screen()` unless
`"terminal": {"mouse": false}` says otherwise, and given back on all three
ways out: a normal quit, `SCREEN_RESTORE` in the signal handler, and
`Keyboard::restore()` on the unwind from a panic.

That key is the runtime toggle. The launcher has owned the shared
`terminal` section since the wheel landed, and `load` in `settings.rs` now
wraps it alongside whichever section the widget asked for — so every
settings screen offers it, and leaving the screen relaunches *that* widget
with the new setting. The schema is read out of the launcher's own
`settings.json` rather than copied, because two records of the same
defaults is one record and one thing that used to be true.

Two checks enforce all of it, and they are separate on purpose because a
widget can satisfy either without the other: `every_widget_registers_its_footer`
reads for the `footer_at` call — not for `pack_hints_placed`, since `months`
wraps prose through `pack_hints` too and a sentence is not a footer — and
`every_widget_with_a_cursor_answers_a_click` reads for `tc::rows_clicked(`,
which is the one call that does both halves of the gesture — and not
`row_at(`, because `github` keeps a local closure of that name.

The chart helpers are worth knowing before drawing anything new: `vbars()` and
its mirror `vbars_down()` (pair them on a shared scale for a diverging chart),
`stacked_bar()` for proportions, `meter()` for a gauge, and `skeleton()` for
the shimmer that stands in for a figure still being fetched.

Braille line charts are not among them. `latency` and `link` each keep their
own `braille_canvas`, and the two are not the same function: latency's series
carries the gaps a ping can leave, and link's is told how many slots the axis
holds, so that a session younger than the chart takes its own share of the
width rather than being stretched across all of it.
