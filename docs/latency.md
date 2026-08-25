# `latency`

Continuous latency to a list of hosts, with the statistics that actually
explain a bad connection.

```
╺━ NETWORK LATENCY MONITOR ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 4 targets · 0.5s interval · 19:05:21 · 1 ping/column

 HOST                     NOW     AVG  MEDIAN     MIN     MAX  JITTER   LOSS
● studio               27.60ms 47.80ms 39.90ms 25.90ms 102.0ms 21.16ms   0.0%
   ▁▂▁█▂▁▂▅▁▆▂██▁▁▃▄▄▇▂▂▁▁▄▆▂▂▁▂▆▁▁▂▁▂▆▂▂▂▁▁▁▂▁
● build-mac            145.0ms 145.0ms 145.0ms 145.0ms 145.0ms     0µs   0.0%
   ▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁
● 1.1.1.1                3.98ms  3.96ms  4.01ms  3.69ms  4.28ms   173µs   0.0%
   ▅▂▂▅▄▂▅▆▇▄▆▃▆▃▆▁▅▃▆▆▅▂▅▁▆▃▇▅▂▆▆▅▅▁▃▂▅▆▅▃▂▂▄▅

 177.5ms│·           ·           ·           ·           ·
        │                                            ●●●●●●
  51.2ms│·           ·          ●●●●●●●●●●●●●●●●●●●●●●
   3.9ms│·  ●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●●
        └────────────────────────────────────────────────
         300s ago                                     now
```

## Why the shape of it

**The graph is log-scale**, because a useful target list spans three orders of
magnitude. On this network `8.8.8.8` answers in 232µs while a Tailscale peer
takes 145ms; on a linear axis the fast host is a flat line pinned to the floor
and you learn nothing from it.

**Median, not mean, when samples share a column.** Latency is right-skewed, so
one spike drags a mean well above what the connection actually felt like. `-g`
switches to mean, min, max or p95 if you want the other view.

**The spread is drawn behind the line.** Each column's min–max range appears as
a dimmed band, so aggregating never hides the outliers it aggregated away.

**One column per ping by default.** The plot advances exactly one column per
sample — the finest motion a character grid allows — and columns are anchored
to a fixed time grid, so a sample never migrates between columns as time slides.
`-c` trades that back for a longer visible history.

## What the numbers tell you

Low latency with high jitter is usually worse to work over than high latency
that is steady. Two of the hosts above make the point: `studio` is five times
closer than `build-mac` yet carries 21ms of jitter against 0µs — it will feel worse
in an interactive session despite the better average.

Jitter here is the mean gap between consecutive samples, so **it is not
comparable across different intervals**. Do not read a jitter change right after
pressing `i` as the network changing.

## Keys

| Key | Action |
|---|---|
| `i` | ping interval — 0.2 / 0.5 / 1 / 2 / 5s, applied to running pings immediately |
| `g` | how samples sharing a column combine — median / mean / min / max / p95 |
| `c` | seconds per graph column — 1 ping/col / 2 / 5 / 10s |
| `q` | quit |

Changing the interval kills and relaunches each `ping`, since `-i` is fixed at
launch. Those relaunches are marked internally so they are not logged as
outages.

## The event log

Beneath the graph is a running log of the things worth naming rather than
leaving you to spot in a line: a host going `DOWN` and coming back `UP` with
how long it was away, and a `SPIKE` when a single reading lands far above what
that host normally does.

"Far above" is `spike_factor`, three times the median by default, and it needs
at least ten samples before it will call anything — otherwise the first slow
reading on a fresh host is a spike against a median of itself. Raise it on a
link that is naturally jittery, lower it to catch smaller excursions.

## Traffic cost

One 98-byte frame each way per target per interval — about **1.6 KB/s for four
targets at 0.5s**, or 5.6 MB/hour. Negligible against an SSH session, which
measures around 250 KB/s. The reason to slow down is politeness to the far end,
not bandwidth.

Public anycast resolvers (`1.1.1.1`, `8.8.8.8`) are fine to probe at this rate,
but remember what they measure: you reach the nearest edge, not the internet. A
230µs reading to `8.8.8.8` from a GCP VM is a rack down the hall. They are a
baseline, not a path measurement.

## Configuration

```json
"latency": {
  "hosts": ["1.1.1.1", "8.8.8.8", "host.internal"],
  "interval": 0.5,
  "seconds_per_column": 0,
  "window": 600,
  "spike_factor": 3.0,
  "aggregate": "median",
  "strip_suffixes": [".internal"]
}
```

`strip_suffixes` shortens long FQDNs for display, so a narrow pane shows
`studio` rather than clipping the interesting half.

```sh
./target/release/latency                              # config targets, 0.5s
./target/release/latency -i 2 1.1.1.1 example.com     # override both
```

It measures *this host → each target*. Target-to-target legs need a probe on the
far end.
