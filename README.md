# terminal-toys

Small terminal tools with no dependencies — pure Python 3 standard library, 24-bit
colour, and a full redraw each frame so everything reflows when you resize the pane.

Each tool is a single self-contained script that imports shared drawing helpers
from `common.py`. Run any of them directly; pass `-h` for its documentation.

## Tools

### `latency.py` — multi-target latency monitor

Continuously pings a list of hosts and shows, for each: current / average / min /
max round-trip time, jitter, and packet loss, plus a per-target sparkline. Below
that, all targets share one **log-scale** time graph — so a 200µs host and a 140ms
host are both legible on the same plot, which a linear axis cannot do. Loss events
and latency spikes are timestamped in an event log.

```sh
./latency.py                              # built-in target list, 0.5s interval
./latency.py -i 2 1.1.1.1 example.com     # 2s interval, custom targets
```

Edit `DEFAULT_HOSTS` for your own targets. Traffic cost is one 98-byte frame each
way per target per interval — roughly 1.6 KB/s for four targets at 0.5s.

It measures *this host → each target*. It cannot measure target-to-target legs;
that needs a probe running on the far end.

### `worldclock.py` — server time, office hours, world clock

Big-digit clock for the machine's own timezone, three live countdown bars, and a
world clock covering 19 tech hubs sorted west to east.

- **Countdowns** — time to the next hour, to the start/end of office hours, and to
  end of day. The office bar flips its own label: it counts down to 18:00 during
  office hours, and to the next opening outside them, correctly skipping weekends
  (Friday evening counts to Monday 09:00, not Saturday).
- **World clock** — each city in its own timezone, coloured by what people there
  are plausibly doing: green at work, amber evening, blue asleep, grey weekend.
  Handles half-hour offsets and `+1d` / `-1d` date rollovers.

```sh
./worldclock.py
```

Configure `CITIES`, and `WORK_START_H` / `WORK_END_H` (default Mon–Fri 09:00–18:00).
Set `TZ` to a fixed offset to pin the clock instead of following the system zone.

### `matrix.py` — digital rain

Falling glyphs with truecolor fade trails: a near-white head, bright green
shoulder, and smooth decay over each drop's length. Glyphs mutate in place
independently of the drops, and each column gets its own speed and trail length.

```sh
./matrix.py
```

## Requirements

- Python **3.9+** (`worldclock.py` uses `zoneinfo`); developed on 3.12
- A terminal with 24-bit colour support
- `ping` on `PATH` (`latency.py` only) — no root needed

## Shared helpers

`common.py` holds the pieces every tool uses: terminal sizing, a full-frame
`draw()`, 24-bit colour, progress bars, a green→amber→red `heat()` ramp, and
`seg()`, which joins coloured segments while clipping to a printable-cell budget
so coloured text never overflows a narrow pane.

## License

[GNU AGPL-3.0](LICENSE). You may use, modify and share these tools freely; if you
distribute a modified version — or run one as a network service — you must make
your source available under the same license.
