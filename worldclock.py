#!/usr/bin/env python3
# terminal-toys - small dependency-free terminal tools
# Copyright (C) 2026 William Li
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU Affero General Public License as published
# by the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU Affero General Public License for more details.
#
# You should have received a copy of the GNU Affero General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.
"""Server time, office-hours countdowns, a pomodoro timer, and a world clock.

The pomodoro is off until you press p. It runs the standard 25/5 with a longer
break every fourth session, all configurable, and persists across restarts so
relaunching the panel does not cost you a session.

A phase does not end itself. When the time is up the counter keeps going,
showing how far over you are, and the bar rescales so a growing red section
represents the overrun — the longer you ignore it, the more of the bar is red.
The terminal is alerted when the phase elapses and again every minute it keeps
running.

Keys: up/down (and PgUp/PgDn, Home/End) scroll the city list while the clock,
countdowns and footer stay pinned. p toggles the pomodoro, space pauses or
resumes, r restarts the phase, s skips to the next one, +/- change the focus
length, q quits.

Big digits show this server's system-timezone clock. Below it, each hub
is shown in its own timezone, sorted west to east, coloured by whether people
there are plausibly at work.
"""
import datetime
import json
import os
import sys
import time
from zoneinfo import ZoneInfo

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, Keyboard, bar, draw, flush, load_config, maybe_help,
                    out, pad, rgb, seg, setup, size, title)

# None = follow the system timezone for the big digits.
TZ = None

_CFG = load_config("worldclock", {
    "cities": [
        ["San Francisco", "America/Los_Angeles"],
        ["New York", "America/New_York"],
        ["London", "Europe/London"],
        ["Berlin", "Europe/Berlin"],
        ["Bengaluru", "Asia/Kolkata"],
        ["Singapore", "Asia/Singapore"],
        ["Tokyo", "Asia/Tokyo"],
        ["Sydney", "Australia/Sydney"],
    ],
    "work_start_hour": 9,
    "work_end_hour": 18,
    "pomodoro_enabled": False,
    "pomodoro_focus_minutes": 25,
    "pomodoro_short_break_minutes": 5,
    "pomodoro_long_break_minutes": 15,
    "pomodoro_sessions_before_long_break": 4,
    "pomodoro_bell": True,          # terminal bell on elapse and each minute over
    "pomodoro_notify": True,        # OSC 9 desktop notification, where supported
})

CITIES = [tuple(c) for c in _CFG["cities"]]

BIG = {
    "0": ["███", "█ █", "█ █", "█ █", "███"],
    "1": ["  █", "  █", "  █", "  █", "  █"],
    "2": ["███", "  █", "███", "█  ", "███"],
    "3": ["███", "  █", "███", "  █", "███"],
    "4": ["█ █", "█ █", "███", "  █", "  █"],
    "5": ["███", "█  ", "███", "  █", "███"],
    "6": ["███", "█  ", "███", "█ █", "███"],
    "7": ["███", "  █", "  █", "  █", "  █"],
    "8": ["███", "█ █", "███", "█ █", "███"],
    "9": ["███", "█ █", "███", "  █", "███"],
    ":": ["   ", " █ ", "   ", " █ ", "   "],
}

WORK_START_H = int(_CFG["work_start_hour"])
WORK_END_H = int(_CFG["work_end_hour"])

# Pomodoro: 25 minutes of focus, 5 off, a longer break every fourth session.
PHASES = ("focus", "short", "long")
PHASE_LABEL = {"focus": "FOCUS", "short": "SHORT BREAK", "long": "LONG BREAK"}
STATE_FILE = os.path.join(
    os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state"),
    "terminal-toys", "pomodoro.json")

C1 = rgb(120, 255, 200)
PURPLE = rgb(175, 130, 255)
HOUR = rgb(90, 220, 255)
FOCUS = rgb(255, 130, 120)
BREAK = rgb(120, 235, 170)
PAUSED = rgb(160, 172, 190)
OVER = rgb(255, 80, 90)
C2 = rgb(40, 150, 120)
DIM = rgb(70, 130, 110)
TXT = rgb(220, 255, 240)
WORK = rgb(130, 255, 180)      # inside working hours
EVE = rgb(255, 200, 90)        # evening
NIGHT = rgb(95, 130, 175)      # asleep
WEEKEND = rgb(150, 150, 170)
HERE = rgb(255, 170, 220)


def alert(text, bell=True, notify=True):
    """Nudge the user through the terminal.

    BEL is universal; OSC 9 raises a desktop notification on terminals that
    implement it and is ignored by those that do not, so both are sent.
    """
    if bell:
        out("\a")
    if notify:
        out("\x1b]9;%s\x07" % text)
    flush()


class Pomodoro(object):
    """A pomodoro that survives the panel being restarted.

    Panels get relaunched often, and losing a 20-minute session to that would
    make the timer useless, so phase and elapsed time are persisted and
    reloaded. Time is tracked as an absolute deadline rather than by counting
    down, so a stalled or slow redraw cannot make the timer drift.
    """

    def __init__(self):
        self.focus = int(_CFG["pomodoro_focus_minutes"])
        self.short = int(_CFG["pomodoro_short_break_minutes"])
        self.long = int(_CFG["pomodoro_long_break_minutes"])
        self.cycle = int(_CFG["pomodoro_sessions_before_long_break"])
        self.enabled = bool(_CFG["pomodoro_enabled"])
        self.phase = "focus"
        self.completed = 0
        self.running = False
        self.left = self.focus * 60.0     # seconds remaining when paused
        self.deadline = None              # wall-clock end when running
        self.rang = False
        self.nagged = 0            # whole minutes of overtime already alerted
        self.bell = bool(_CFG["pomodoro_bell"])
        self.notify = bool(_CFG["pomodoro_notify"])
        self._load()

    # ---- persistence -------------------------------------------------
    def _load(self):
        try:
            with open(STATE_FILE) as f:
                d = json.load(f)
        except (OSError, ValueError):
            return
        if d.get("day") != time.strftime("%Y-%m-%d"):
            return                         # a new day starts a fresh count
        self.phase = d.get("phase", self.phase)
        self.completed = int(d.get("completed", 0))
        self.focus = int(d.get("focus", self.focus))
        self.enabled = bool(d.get("enabled", self.enabled))
        self.left = float(d.get("left", self.left))
        if d.get("running") and d.get("deadline"):
            # resume mid-phase; if it elapsed while we were away the timer
            # simply shows how far over it has run
            self.deadline = float(d["deadline"])
            self.running = True
            self.rang = self.signed() <= 0

    def save(self):
        try:
            os.makedirs(os.path.dirname(STATE_FILE), exist_ok=True)
            with open(STATE_FILE, "w") as f:
                json.dump({"day": time.strftime("%Y-%m-%d"), "phase": self.phase,
                           "completed": self.completed, "focus": self.focus,
                           "enabled": self.enabled, "running": self.running,
                           "left": self.left, "deadline": self.deadline}, f)
        except OSError:
            pass

    # ---- state -------------------------------------------------------
    def duration(self):
        return {"focus": self.focus, "short": self.short,
                "long": self.long}[self.phase] * 60.0

    def signed(self):
        """Seconds left; negative once the phase has been overrun."""
        if self.running and self.deadline:
            return self.deadline - time.time()
        return self.left

    def remaining(self):
        return max(0.0, self.signed())

    def overtime(self):
        return max(0.0, -self.signed())

    def toggle(self):
        self.enabled = not self.enabled
        self.save()

    def pause(self):
        if self.running:
            self.left = self.signed()          # keeps any overtime accrued
            self.running, self.deadline = False, None
        else:
            self.deadline = time.time() + self.signed()
            self.running = True
        self.save()

    def restart(self):
        self.left = self.duration()
        self.deadline = time.time() + self.left if self.running else None
        self.rang = False
        self.nagged = 0
        self.save()

    def advance(self):
        """Move to the next phase; a long break every `cycle` focus sessions.

        Keeps whatever run state it had: skipping out of a running focus block
        starts the break immediately, which is what "I am done, move on" means.
        """
        if self.phase == "focus":
            self.completed += 1
            self.phase = ("long" if self.cycle and self.completed % self.cycle == 0
                          else "short")
        else:
            self.phase = "focus"
        self.left = self.duration()
        self.deadline = time.time() + self.left if self.running else None
        self.rang = False
        self.nagged = 0
        self.save()

    def adjust(self, delta):
        """Change the focus length; applies now when focus is not running."""
        self.focus = max(1, min(120, self.focus + delta))
        if self.phase == "focus" and not self.running:
            self.left = self.duration()
        self.save()

    def tick(self):
        """True exactly once, when a phase elapses.

        The phase is not advanced automatically: overrunning a focus block is
        worth seeing rather than silently resetting, so the timer keeps
        counting upward until `s` moves it on.
        """
        if not self.running:
            return False
        over = self.overtime()
        if over <= 0:
            return False
        if not self.rang:
            self.rang = True
            self.nagged = 0
            return True
        # keep nagging once per minute for as long as it is ignored
        minutes = int(over // 60)
        if minutes > self.nagged:
            self.nagged = minutes
            return True
        return False


def render_big(s, w):
    lines = ["", "", "", "", ""]
    for ch in s:
        g = BIG.get(ch, ["   "] * 5)
        for i in range(5):
            lines[i] += g[i] + " "
    return [ln[:w] for ln in lines]


def offset_str(dt):
    off = dt.utcoffset()
    total = int(off.total_seconds()) // 60
    sign = "+" if total >= 0 else "-"
    total = abs(total)
    if total % 60:
        return "UTC%s%d:%02d" % (sign, total // 60, total % 60)
    return "UTC%s%d" % (sign, total // 60)


def hms(seconds):
    seconds = max(0, int(seconds))
    return "%02d:%02d:%02d" % (seconds // 3600, seconds % 3600 // 60, seconds % 60)


def at(now, day, hour):
    """Wall-clock `hour` on `day`, in the same timezone as `now`."""
    return datetime.datetime.combine(day, datetime.time(hour, 0), tzinfo=now.tzinfo)


def is_office(dt):
    return dt.weekday() < 5 and WORK_START_H <= dt.hour < WORK_END_H


def next_open(now):
    day = now.date()
    cand = at(now, day, WORK_START_H)
    while cand <= now or cand.weekday() >= 5:
        day += datetime.timedelta(days=1)
        cand = at(now, day, WORK_START_H)
    return cand


def prev_close(now):
    day = now.date()
    cand = at(now, day, WORK_END_H)
    while cand > now or cand.weekday() >= 5:
        day -= datetime.timedelta(days=1)
        cand = at(now, day, WORK_END_H)
    return cand


def office_countdown(now):
    """Office hours are Mon-Fri 09:00-18:00 local."""
    if is_office(now):
        start = at(now, now.date(), WORK_START_H)
        end = at(now, now.date(), WORK_END_H)
        span = (end - start).total_seconds()
        return ("End of Office Hour", hms((end - now).total_seconds()),
                (now - start).total_seconds() / span)
    opens = next_open(now)
    closed = prev_close(now)
    span = (opens - closed).total_seconds()
    return ("Start of Office Hour", hms((opens - now).total_seconds()),
            (now - closed).total_seconds() / span if span > 0 else 0.0)


def countdowns(now):
    """Real countdowns for the current local day. Returns rows of
    (label, text, elapsed_fraction, colour), stacked top to bottom."""
    midnight = now.replace(hour=0, minute=0, second=0, microsecond=0)
    day_end = midnight + datetime.timedelta(days=1)
    hour_start = now.replace(minute=0, second=0, microsecond=0)
    hour_end = hour_start + datetime.timedelta(hours=1)
    label, text, frac = office_countdown(now)

    return [
        ("Next Hour", hms((hour_end - now).total_seconds()),
         (now - hour_start).total_seconds() / 3600.0, HOUR),
        (label, text, frac, EVE),
        ("End of Day", hms((day_end - now).total_seconds()),
         (now - midnight).total_seconds() / 86400.0, PURPLE),
    ]


def phase(dt):
    """Colour + glyph for what people there are plausibly doing."""
    hour = dt.hour + dt.minute / 60.0
    weekend = dt.weekday() >= 5
    if hour < 6.5 or hour >= 22:
        return NIGHT, "☾"
    if weekend:
        return WEEKEND, "☀"
    if 9 <= hour < 18:
        return WORK, "☀"
    if hour >= 18:
        return EVE, "☾"
    return EVE, "☀"


def main():
    maybe_help(__doc__)
    setup()
    keyboard = Keyboard()
    pomo = Pomodoro()
    scroll = 0
    zones = []
    for name, key in CITIES:
        try:
            zones.append((name, ZoneInfo(key)))
        except Exception:
            continue
    local_key = time.tzname[0]
    while True:
        for key in keyboard.poll():
            if key in ("q", "Q"):
                keyboard.restore()
                raise SystemExit(0)
            if key == "up":
                scroll -= 1
            elif key == "down":
                scroll += 1
            elif key == "pgup":
                scroll -= 8
            elif key == "pgdn":
                scroll += 8
            elif key == "home":
                scroll = 0
            elif key == "end":
                scroll = 10 ** 6           # clamped to the end below
            elif key == "p":
                pomo.toggle()
            elif not pomo.enabled:
                continue
            elif key == " ":
                pomo.pause()
            elif key == "r":
                pomo.restart()
            elif key == "s":
                pomo.advance()
            elif key in ("+", "="):
                pomo.adjust(5)
            elif key == "-":
                pomo.adjust(-5)
        if pomo.tick():
            over = pomo.overtime()
            alert("%s %s" % (PHASE_LABEL[pomo.phase],
                             "finished" if over < 60
                             else "running %dm over" % (over // 60)),
                  pomo.bell, pomo.notify)

        w, h = size()
        stamp = datetime.datetime.now(TZ) if TZ else datetime.datetime.now().astimezone()

        rows = [title("server time", w)]
        rows.append("")
        for i, ln in enumerate(render_big(stamp.strftime("%H:%M:%S"), w - 2)):
            rows.append(" " + (C1 if i < 3 else C2) + ln)
        rows.append("")
        rows.append(seg([(DIM, " "), (TXT, stamp.strftime("%Y-%m-%d  %A").upper()),
                         (DIM, "   " + offset_str(stamp))], w - 1))
        rows.append("")
        rows.append(DIM + " ── COUNTDOWN ──")
        if pomo.enabled:
            over = pomo.overtime()
            col = FOCUS if pomo.phase == "focus" else BREAK
            if not pomo.running:
                col = PAUSED
            rows.append(seg([(col, " " + pad("Pomodoro · " + PHASE_LABEL[pomo.phase],
                                             21)),
                             (OVER if over else TXT,
                              ("+" + hms(over)) if over else hms(pomo.remaining())),
                             (PAUSED, "  paused" if not pomo.running else ""),
                             (OVER if over else DIM,
                              "  OVER" if over else ""),
                             (DIM, "   %d done" % pomo.completed)], w - 1))
            n = max(4, w - 3)
            if over:
                # the bar rescales to duration+overtime, so the red share grows
                # the longer the phase is ignored
                total = pomo.duration() + over
                base = max(1, int(round(n * pomo.duration() / total)))
                rows.append(" " + col + "█" * base + OVER + "█" * (n - base))
            else:
                done = 1.0 - (pomo.remaining() / pomo.duration()
                              if pomo.duration() else 0)
                rows.append(" " + col + bar(done, n))
        for label, text, frac, col in countdowns(stamp):
            rows.append(seg([(DIM, " " + pad(label, 21)), (TXT, text)], w - 1))
            rows.append(" " + col + bar(frac, max(4, w - 3)))

        rows.append("")

        # sort west -> east by current UTC offset
        now_utc = datetime.datetime.now(datetime.timezone.utc)
        entries = []
        for name, tz in zones:
            entries.append((now_utc.astimezone(tz), name))
        entries.sort(key=lambda e: (e[0].utcoffset(), e[1]))

        # The clock, countdowns and footer stay pinned; only this list scrolls.
        room = max(1, h - len(rows) - 2)
        scroll = max(0, min(scroll, max(0, len(entries) - room)))
        window = entries[scroll:scroll + room]
        more = len(entries) > room
        rows.append(DIM + " ── WORLD CLOCK ──" +
                    ("  %d-%d of %d  ↑↓" % (scroll + 1, scroll + len(window),
                                            len(entries)) if more else ""))
        namew = max(9, min(15, w - 26))
        for dt, name in window:
            if len(rows) >= h - 1:
                break
            col, glyph = phase(dt)
            same_zone = dt.strftime("%Z") == stamp.strftime("%Z")
            daydiff = (dt.date() - stamp.date()).days
            tag = ""
            if daydiff > 0:
                tag = " +1d"
            elif daydiff < 0:
                tag = " -1d"
            rows.append(seg([(col, " " + glyph + " "),
                             (HERE if same_zone else TXT, pad(name, namew)),
                             (col, dt.strftime(" %H:%M")),
                             (DIM, dt.strftime("  %a")),
                             (DIM, "  " + pad(offset_str(dt), 8)),
                             (EVE, tag)], w - 1))
        rows = rows[:h - 1]
        while len(rows) < h - 1:
            rows.append("")
        if pomo.enabled:
            rows.append(seg([(DIM, " [space]"), (TXT, "pause" if pomo.running
                                                 else "start"),
                             (DIM, " [r]estart [s]kip [±]%dmin [p]off" % pomo.focus)],
                            w - 1))
        else:
            rows.append(seg([(DIM, " [p] pomodoro")], w - 1))
        draw(rows, w, h)
        time.sleep(0.25)


main()
