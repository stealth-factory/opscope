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
a widget that opens `/proc` with no macOS path and no explanation, a notice
id that is not a literal or a const or a Core-scope notice raised by a
widget (`every_notice_has_a_stable_id`), a widget that can hold a notice and
does not start the channel, draw its banner and counter, and answer `!` with
the screen (`a_widget_that_raises_a_notice_shows_it`), and a widget with a
notices type, module or function of its own
(`only_core_draws_the_notices_screen`). A widget that puts
`tc::notices::hint(` in its footer is read as hinting `!`, so the key and
README checks cover that key like any other.

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

`core/src/google.rs` is the Google sign-in every Google widget shares: a
service-account key signs an RS256 JWT locally (`rsa` and `sha2`, both pure
Rust), Google trades it for an hour-long token, and the token is cached per
process and re-minted five minutes before it runs out. A pasted token in
`token` / `token_env` is the testing fallback, and says on screen that it
lasts an hour. `Refusal` reads Google's error body into one wording for an
API that is not enabled, a 403 on the resource, a 401, and a 429 that names
its quota bucket. It goes through `post_answer()`, the sibling of
`get_answer()` that hands back a refusal's whole body rather than the
200-character line `post_json()` makes of it. The setup it needs on
Google's side is [Google setup](google-setup.md).

### Notices

`core/src/notices.rs`, used as `tc::notices`, is the one place a warning
that is not about a number goes. `start(widget)` names the widget once in
`main`; `raise(Notice)`, `clear(id)` and `when(cond, || Notice)` work from
any thread, against a process-global registry that recovers a poisoned lock
rather than propagating it, so nothing in the module panics or returns an
error. A notice is keyed by its scope and a stable id - a widget notice
sharing an id with a Core one is a second notice, not an overwrite: raising the same id again updates
it in place, and a change to its title or detail — hashed with FNV-1a, so the
number is the same in every build — banners it again and voids a dismissal.

The widget draws two things and answers one key. `hint(room)` is the footer
hint, `[!] 1 error, 2 warnings`, worst level first and coloured by it, or
`None` when nothing is outstanding; it shortens to `[!] 3 notices` rather
than overflowing a narrow footer, and `hint_key` reads its `[!]`, so it is
clickable through `footer_at` with nothing more done. `banner_rows(w, h)` is
up to two notices, newest first, for sixty seconds each - each one's title
alone, wrapped by `error_rows()`, the detail left to the `!` screen. It takes
at most a quarter of the pane's height `h`, since only the body scrolls and a
taller banner would push the footer's hints below the bottom edge; a title
that does not fit whole in what is left is not shown rather than cut, and
the `[!]` counter still counts it. It is drawn above the footer and counted in it, not injected by
`draw()`, which would move the footer out from under the placements
`footer_at` registered. `run(keyboard)` is the screen, on `!`.

Core raises its own: `config-world-readable` (Core scope, from
`load_config()`, and looked at again every five seconds from `hint` and
`banner_rows`, so a `chmod 600` clears it and a file rewritten wider raises
it without a restart), and one Info per recommended tool `dependencies_available()`
finds missing, and two about its own file below.

A dismissal is keyed `core/<id>` or `<widget>/<id>` with the hash it was
taken against. `s` dismisses for this session, in memory only, so a restart
forgets it. `d` and `m` dismiss a Warn or Info for 7 or 30 days, and on an
Error do nothing but say on the status line that errors can only be
dismissed for this session. Those two are written to
`tc::state_dir()/notices.json`, one file for every widget:

```json
{"version": 1, "dismissed": {"core/config-world-readable":
  {"hash": "566dda1943ee58c7", "at": 1800000000.0, "until": 1800604800.0}}}
```

`until` is absolute unix seconds, `at + 7 * 86400` or `at + 30 * 86400`. The
hash is a 16-digit hex string rather than a number, because a u64 above
2^53 survives serde_json but not every editor that might open the file. A
write is a read-modify-write under `tc::statefile::with_lock` - `flock` on
`notices.json.lock` - then `write_atomic`, so two panes dismissing at once
merge; each write also drops what has expired. An entry taken against
content this pane's notice no longer has is kept, since another pane may
still show that content and have dismissed it; it hides nothing here and
goes when it expires. `start()` reads the file and `hint()`
and `banner_rows()` read it again at most once a minute, on the drawing
thread: it is a few hundred bytes, and a thread for it would be one more
poller to keep alive. `u` removes the entry from the file as well.

A file that does not parse, or names a version this build does not know,
is read as empty and **never overwritten**: a dismissal for days taken
meanwhile holds for this session only, and the Core Warn
`notices-file-unreadable` says the path cannot be read, with the error, until
it reads again; the same notice covers a file that cannot be opened at all.
A write that fails for another reason raises `notices-file-unwritable` and
keeps the dismissal for the session; a restore that cannot be written is not
made. Errors ignore dismissals saved in the file, since a notice's hash does
not include its level.

### State files

`core/src/statefile.rs`, used as `tc::statefile`, is where a widget keeps
what it learned between runs. `tc::state_dir()` is `$XDG_STATE_HOME/opscope`,
else `$HOME/.local/state/opscope`, else `./.opscope-state` when neither is
set - never a panic. `write_atomic(path, contents)` is the writer the
settings screen uses for `config.json`, lifted rather than rewritten: a
same-directory temporary created `O_EXCL | O_NOFOLLOW` at `0600`, re-read
and re-parsed as JSON before the rename, the directory synced after, a
symlink at the path kept. `write_atomic_if` adds settings' compare-and-swap,
refusing when the file no longer holds what was read. `with_lock(path, f)`
holds `flock(LOCK_EX)` on `<path>.lock`, created `0600` and never removed,
for the closure's duration. It waits about half a second for another holder
and then returns an `Err`, so a pane stopped mid-write cannot freeze the
rest; notices then keeps the dismissal for the session. clocks, agent-usage, ports and google-trends still compute
their state paths by hand; new code uses `state_dir()`.

### The mouse

A click arrives as a key. That is the whole design: the mouse is a second
route to something a key already does, never a capability of its own, so
there is no second input type and no widget decoding escape sequences.

`Keyboard::poll()` turns an SGR report into `wheel-up`, `wheel-down`, or
`click:<col>,<row>` — zero-based and in the widget's own coordinates, where
`(0, 0)` is the first cell of `rows[0]` as handed to `draw()`. Only the left
button going down becomes a click. The release, a drag, and the middle and
right buttons are eaten rather than acted on: with reporting on, an ordinary
drag selects nothing, so the drag belongs to the host and the way to copy a
value out of a widget is that widget's own copy key. An action bound only to
the right button
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

A hint is clickable when it names exactly one key. Two forms count first,
both unambiguous about which characters are the key — `[q]uit`,
`[d] cloudflare`, `[↵] open`, or a bare `↵ → ← ↑ ↓`. The launcher's
`↑↓ select` names two and gets no spot; `[±]25` is one glyph standing for
`+` and `-` and gets none either. The bracket rules are the four `check.rs`
uses, so the two readers agree about what a footer teaches.

A hint that names its key neither way falls back to its **leading token**,
when that token is a key with a name — `ctrl-u clear`, `esc cancel`,
`tab to add another`, which is how the settings screen writes half of its
footer. This is not the loose reading `check.rs` does. There a whole footer
is one string and the key could be anywhere in it, so recovering it means
guessing; here each hint arrives on its own and this tree writes the key
first, every time. Nothing after the first token is read, which is what
keeps `clear`, `cancel` and `done` from becoming keys.

A fallback, never a tie-breaker, and that ordering is load-bearing:
`esc / [,] back` names two keys for one action, so counting both would make
it ambiguous and take away a spot it already had. The bracket wins, the
synonym is ignored, the hint stays clickable.

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

Tracking itself is asked for in `claim_screen()`, unconditionally, and given
back on all three ways out: a normal quit, `SCREEN_RESTORE` in the signal
handler, and `Keyboard::restore()` on the unwind from a panic. The asking is
what has no condition on it; the three ways out are not up for negotiation,
because a terminal left reporting outlives the process that asked for it.

It was a config key once, `terminal.mouse`, spliced into every widget's
settings screen out of a `terminal` section the launcher owned. It is gone.
The key was on by default, on is what everybody got, and the only thing
turning it off ever did was leave somebody with a pane whose wheel had
stopped working — a way to break your own pane rather than a way to make
anything easier. `the_wheel_is_asked_for_unconditionally` in `check.rs`
reads `claim_screen`'s body for MOUSE_ON and for any config read, so
putting the condition back fails the build; `the_wheel_is_turned_off_on_every_way_out`
is the other half and covers the exits.

## A panic while drawing is a row, not the end of the pane

`Drop` putting the terminal back on an unwind is the *last* thing that
happens, and until now it was also the only thing: one bad value in one
section took the whole pane with it, the process exited, and the reader got
a shell prompt where a widget had been. Everything else the widget knew went
with it — the other sections, the quota that was fine, the keys.

`guard_rows(what, w, build)` wraps the part that reads the data, and
`guard_frame(widget, w, h, build)` wraps a whole frame for a widget with no
natural section to guard. Reach for the first: a widget's chrome is cheap and
almost never the thing that fails, so leaving the title, the tab strip and
the footer drawn leaves a pane the reader can still steer — the other tabs
still open, `q` still quits, and `r` gets a fresh attempt.

**The frame is the right thing to guard, and nearly the only thing.** It is
rebuilt from scratch every tick from whatever the pollers left behind, so
nothing carries over from the attempt that failed and there is no
half-written state to reason about. That is what makes catching a panic here
honest rather than reckless. A poller keeps its own `catch_unwind`, because
it owns state that *does* carry over — and it records its reason for the same
purpose this does.

The failure is drawn, never swallowed. An empty section and a section that
could not be built look identical on screen, which is the founding hazard of
the collection wearing another hat, so the rows say what broke and where:

```
 codex could not draw this frame.

  no rate for gpt-5.6-sol (widgets/src/widgets/agent-usage/vendors.rs:462)
```

The location comes from a panic hook, because `catch_unwind` hands back the
payload — the message — and not where it came from, and a panic nobody can
locate is a bug report nobody can act on. The hook also stops the default
one writing to stderr, which on a full-screen widget lands on top of the
frame in the wrong colours and survives the redraw. `RUST_BACKTRACE` puts
the default behaviour back, because somebody who asked for a backtrace wants
Rust's, not a one-line summary of it.

What it records is **per-thread**, which is the scope a panic actually has:
it unwinds on the thread that panicked and is caught on the same one. Shared,
a poller dying in the background would overwrite what the frame was about to
say about itself — two failures, one row, and the wrong one drawn.

Two checks enforce all of it, and they are separate on purpose because a
widget can satisfy either without the other: `every_widget_registers_its_footer`
reads for the `footer_at` call — not for `pack_hints_placed`, since `months`
wraps prose through `pack_hints` too and a sentence is not a footer — and
`every_widget_with_a_cursor_answers_a_click` reads for `tc::rows_clicked(`,
which is the one call that does both halves of the gesture. Not `item_at(`,
which a widget could call without ever opening anything, and not `row_at(`,
which is not even core API any more — `github` keeps a local closure of that
name, and a reader looking for it would have been satisfied by a widget that
answered nothing.

The chart helpers are worth knowing before drawing anything new: `vbars()` and
its mirror `vbars_down()` (pair them on a shared scale for a diverging chart),
`stacked_bar()` for proportions, `meter()` for a gauge, and `skeleton()` for
the shimmer that stands in for a figure still being fetched.

Braille line charts are not among them. `latency` and `link` each keep their
own `braille_canvas`, and the two are not the same function: latency's series
carries the gaps a ping can leave, and link's is told how many slots the axis
holds, so that a session younger than the chart takes its own share of the
width rather than being stretched across all of it.
