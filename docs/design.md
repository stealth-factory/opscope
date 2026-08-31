# Design conventions

[← all docs](README.md)

A few rules hold across all fifteen widgets. They are here rather than in
the README because they are for whoever changes one, not for whoever runs
one — and because each of them was paid for by something that shipped
wrong first.

- **Spend extra width on more content, not padding.** Widgets add columns as a
  pane grows and drop them as it shrinks, rather than truncating.
- **The mouse moves the view. Keys move the selection.** The wheel does a
  full-widget scroll and nothing else: the viewport slides, `selected` stays
  exactly where it is even when that takes it off screen, and a widget with
  sections does not change focus either. Scrolling to look at something must
  never change what `↵` opens. `Ctrl-Y` and `Ctrl-E` do the same from the
  keyboard, as in vim, so the feature is reachable without a mouse. Keys move
  the cursor and the window follows it — but only on the frame a key moved it,
  or the follow drags the view straight back from wherever the wheel put it.
  `every_widget_answers_the_wheel` in `check.rs` fails when a widget does not,
  and it reads match arms rather than the file, so a comment saying it scrolls
  will not satisfy it.
- **A pane too short is a pane you scroll, not a pane that hides things.**
  Every frame is a window onto a body built at whatever height it needs, with
  the title pinned above it. Nothing stands down for want of rows: a section
  that is not drawn looks exactly like a section with nothing in it, and those
  are opposite readings of the same screen — *nothing has gone wrong* against
  *you cannot see whether anything has*. Four widgets sized a chart to
  whatever the pane had left, which hid `latency`'s event log, `link`'s chart
  and four of `github`'s sections, and left the body exactly one pane tall so
  there was nothing for the wheel to reach.
- **Never truncate a key hint.** Footers wrap across as many lines as they need
  and never split a hint, because `[±]25` teaches a key that does not exist.
- **A directional glyph points the way the thing goes.** `▲`/`▼` mark which
  half of a diverging chart a series occupies — `▲ opened` above the baseline,
  `▼ merged` below it. `↑`/`↓` mean upload and download. Where both meanings
  meet, in `netwatch`'s chart, the halves are arranged so they agree: tx
  above and rx below, because a `↓` label over a line that climbs asks the
  reader to hold two directions at once, and they will believe the arrow.
- **Measure contrast, do not eyeball it.** Every colour that draws text clears
  WCAG AA against both the terminal background *and* the selected-row tint,
  with the measured ratios recorded beside the definitions.
- **Say what a number means when it is not obvious.** Counters that reset with
  a daemon, durations that predate the process, aggregates that hide their
  outliers — each is labelled rather than left to mislead.
- **Never show a stale figure under a fresh label.** Change a setting and the
  numbers it governs shimmer until real ones land, rather than sitting there
  looking current. The same rule kills silent truncation: a chart that cannot
  fit its window says `54d of 90d`, and a token missing a scope is named rather
  than left to quietly undercount.
- **Optional enhancements, never requirements.** The clipboard goes through
  OSC 52 so it survives SSH; Herdr toasts and `sudo`-gated data are added where
  available and skipped silently where not.
- **`cfg` decides where bytes come from; nothing else.** Parsers — pure
  functions from text or bytes to values — are always compiled and always
  tested, so a Linux `/proc` reader still runs on the macOS CI runners.
  `cfg(target_os)` is acquisition only: which file to open, which command
  to spawn. Anything that varies *within* a platform (a tool on `PATH`,
  whether `ping` accepts `-O`) is a runtime check, because a build-target
  test would be wrong on the machine that matters. A widget that cannot
  run here says `does not run on {os}` via `unsupported()`, drawn by
  `cannot_start_because()`, rather than an empty table that looks like a quiet
  source. Platform files live in the widget's own folder
  (`parse.rs`, `linux.rs`, `macos.rs` beside `main.rs`); there is no shared
  platform crate to import.
- **A widget owns its whole experience.** Code, help, README preview,
  and AI configuration guide live in one folder, plus a settings
  declaration when the widget is configurable. Configurable widgets all
  open the shared settings screen with `,`; the `opscope` launcher is
  not counted as a widget.

## Where these are enforced

These are not prose. `cargo test` runs
[`widgets/tests/check.rs`](../widgets/tests/check.rs), which reads the
sources and fails on a footer hint naming a key nothing answers, a hint
missing from the widget's README, a config key read but never declared,
an incomplete widget folder, a colour drawing text on the selected-row tint
below WCAG AA, a widget that does not answer the wheel, a parser or a test
gated by `cfg(target_os)`, and a widget that opens `/proc` with no macOS
path. See [internals](internals.md#the-checks).

The wheel rule is enforced the way it is because the obvious marker does not
work. A check that only looked at widgets calling `follow()` would have
passed six of the fourteen — `latency`, `netwatch`, `herdr-panes`, `clocks`,
`agent-usage` and `github-prs` all keep their offset by hand. So every widget
must answer it, and the one that genuinely has nothing to scroll — `matrix`,
which computes nothing on purpose — is named in the check rather than inferred.
