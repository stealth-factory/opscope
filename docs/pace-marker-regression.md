# Pace marker contrast regression (OPS-159)

The report compares v0.39.0 with v1.0.1. The supplied screenshots show
dark separation around the older pace marker, absent from the pixel bar
in v1.0.1.

The graphics migration in `2c9020e` introduced `paced_bar_with_plot`, which
passes a foreground-only marker to `Plot::meter`. The original text bar
uses `pace_mark`, including its dark background. The shared meter draws
the marker directly over its fill or track. Its text compositor also replaces
the widget's original bar with a foreground-only fallback. Thus both shared
renderers lose the separation, although the old `paced_bar` remains intact.
v0.40.0 is the last release before this migration; v1.0.0 introduced it.

Regression coverage inspects rendered pixels and text independently,
including a marker within the fill, beyond it, at the fill boundary, at
both ends, and on narrow bars. Preserve the measured fill and reference
position. OPS-158 / PR #370 changes Devin's pacing data and is separate
from this rendering correction. The fix passes the existing `pace_mark` style
through both overview and provider paths. Core splits its truecolour background
and foreground for pixel rectangles, adds a quarter-cell surround on each side
(clipped at the endpoints), and resets the background after the text marker.

Tracking: <https://linear.app/stealth-company/issue/OPS-159>

Validation:

- The new core pixel and text regression tests both fail on v1.0.1 and pass
  with the fix. An added historical text check on v0.40.0 confirms the dark
  marker background, unchanged cell count and background reset at all tested
  widths and positions. The pre-migration release has no Kitty meter path.
- `cargo test --workspace --all-targets`, `cargo build --workspace --bins`,
  and `node --test npm/test.js` pass on macOS.
- `cargo build -p opscope-core --example terminal_probe` and
  `python3 core/tests/terminal_protocol.py` pass, including decoded Kitty
  pixel checks and forced-text background/reset assertions. The decoded
  transmitted meter image was visually inspected.
- The PTY fixture emulates terminal replies; this is not a visual test in a
  physical Kitty/Herdr pane. No live quota credentials are used by the fixture.
