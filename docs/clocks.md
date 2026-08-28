# `clocks`

[← all docs](README.md)

This server's clock, the clocks counting down, a pomodoro, and everyone else's
clock — the four things you need to know about time while working across
timezones.

```
╺━ CLOCKS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 ── SERVER TIME ──
   ███ ███     ███ ███     █ █ ███
   █ █ █ █  █    █ █ █  █  █ █ █ █
   ███ ███     ███ ███     ███ ███

 2026-08-13  THURSDAY   UTC+8

 ── COUNTDOWN ──
 Pomodoro · FOCUS       00:23:32   3 done
 ███░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░
 Next Hour              00:51:31
 ███████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░
 Start of Office Hour   11:51:31
 ██████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░
 End of Day             02:51:31
 ████████████████████████████████░░░░░░░░░░░░░░

 ── WORLD CLOCK ──  19 cities
 ☾ San Francisco   06:11  Thu  UTC-7
 ☀ London          11:11  Thu  UTC+1
 ☾ Taipei          18:11  Thu  UTC+8
```

## Sections

**Server time** — a big-digit clock in the machine's own timezone, with the
date and UTC offset beneath it. Set `TZ` in the source to pin it to a fixed
offset instead of following the system.

**Countdown** — three bars that are always running:

- **Next hour** — how much of the current hour is left.
- **Office hours** — counts down to 18:00 while you are inside them, and to the
  next opening outside them. It skips weekends properly: Friday evening counts
  to Monday 09:00, not Saturday.
- **End of day** — how much of today is gone.

**Pomodoro** — see below. Off until you press `p`.

**World clock** — each configured city in its own timezone, sorted west to
east, coloured by what people there are plausibly doing: green at work, amber
evening, blue asleep, grey weekend. Half-hour offsets (`UTC+5:30`) and `+1d` /
`-1d` date rollovers are handled. Scroll with the arrow keys when the list is
taller than the pane; everything else stays pinned.

## Pomodoro

Standard 25 / 5 / 15 — 25 minutes of focus, a 5-minute break, and a 15-minute
long break every fourth session.

**A phase never ends itself.** When the time is up the counter flips to `+MM:SS`
and keeps climbing, and the bar rescales to `duration + overtime` so a growing
red section shows the overrun: 25 minutes over a 25-minute block is half red, 50
over is two-thirds. Overrunning is information, not a failure to be reset away —
if you routinely run 40 minutes over a 25-minute block, that is telling you the
block is the wrong length.

**Alerts** fire when a phase elapses and again every minute it keeps running, so
ignoring one gets progressively harder. Four channels, only one of which needs
Herdr:

| Channel | Reaches |
|---|---|
| BEL | every terminal |
| OSC 9 | iTerm2, WezTerm, Windows Terminal, Ghostty |
| OSC 777 | urxvt and others |
| Herdr toast + sound | only under Herdr; additive, skipped elsewhere |

Escape sequences are the only channel that survives SSH — the widget runs on a
server, so anything local to it would fire where nobody is sitting. The panel
also **flashes white twice**, a second apart, which works with the sound muted.

**State persists** across restarts, so relaunching the panel does not cost you a
session. Time is tracked as an absolute deadline rather than a decrementing
counter, so a slow redraw cannot make it drift, and pausing preserves accrued
overtime.

**The completed tally is per day.** It clears when the date changes, including
while the panel is running. Preferences — focus length, whether the timer is
shown — are not tied to the day and survive.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` `PgUp` `PgDn` `Home` `End` | scroll the panel — the title stays pinned, everything under it moves |
| `Ctrl-Y` `Ctrl-E` `wheel` | the same scroll, a line at a time |
| `p` | show/hide the pomodoro — **and suspend it with them**, so a hidden timer never keeps counting |
| `space` | pause / resume |
| `s` `b` `e` | start a break during focus, end one during a break — the footer names whichever applies |
| `r` | restart the current phase |
| `+` `-` | one minute on or off **whichever block is running** — focus, short break or long break — clamped to 1–180. The finish line moves by however much the block actually changed, so a minute added twenty minutes into a twenty-five minute block leaves six, not twenty-six. `=` works as `+`, so it needs no shift, and the footer writes the pair as `[±]` |
| `0` `c` | zero today's completed tally |
| `?` `h` | hide/show the pomodoro controls |
| `q` | quit |

## Configuration

```json
"clocks": {
  "cities": [["San Francisco", "America/Los_Angeles"], ["Tokyo", "Asia/Tokyo"]],
  "work_start_hour": 9,
  "work_end_hour": 18,
  "pomodoro_enabled": false,
  "pomodoro_focus_minutes": 25,
  "pomodoro_short_break_minutes": 5,
  "pomodoro_long_break_minutes": 15,
  "pomodoro_sessions_before_long_break": 4,
  "pomodoro_bell": true,
  "pomodoro_notify": true,
  "pomodoro_flash": true,
  "pomodoro_flash_count": 2,
  "pomodoro_flash_gap": 1.0,
  "pomodoro_flash_rgb": [246, 248, 252],
  "show_hints": true
}
```

The flash colour's text contrast is derived from its luminance, so a dark
choice stays readable rather than turning the panel into a block.

State lives in `~/.local/state/opscope/pomodoro.json`.
