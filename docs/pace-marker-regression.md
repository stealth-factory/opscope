# Pace marker contrast regression (OPS-159)

The report compares v0.39.0 with v1.0.1. The supplied screenshots show
dark separation around the older pace marker, absent from the pixel bar
in v1.0.1.

The graphics migration in `2c9020e` introduced `paced_bar_with_plot`, which
passes a foreground-only marker to `Plot::meter`. The original text bar
uses `pace_mark`, including its dark background. The shared meter draws
the marker directly over its fill or track.

Regression coverage should inspect rendered pixels and text independently,
including a marker within the fill, beyond it, at the fill boundary, at
both ends, and on narrow bars. Preserve the measured fill and reference
position. OPS-158 / PR #370 changes Devin's pacing data and is separate
from this rendering correction.

Tracking: <https://linear.app/stealth-company/issue/OPS-159>
