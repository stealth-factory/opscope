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
"""Server time, office-hours countdowns, and a world clock.

Big digits show this server's system-timezone clock. Below it, each hub
is shown in its own timezone, sorted west to east, coloured by whether people
there are plausibly at work.
"""
import datetime
import os
import sys
import time
from zoneinfo import ZoneInfo

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (RST, bar, draw, load_config, maybe_help, pad, rgb, seg,
                    setup, size, title)

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

C1 = rgb(120, 255, 200)
PURPLE = rgb(175, 130, 255)
HOUR = rgb(90, 220, 255)
C2 = rgb(40, 150, 120)
DIM = rgb(70, 130, 110)
TXT = rgb(220, 255, 240)
WORK = rgb(130, 255, 180)      # inside working hours
EVE = rgb(255, 200, 90)        # evening
NIGHT = rgb(95, 130, 175)      # asleep
WEEKEND = rgb(150, 150, 170)
HERE = rgb(255, 170, 220)


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
    zones = []
    for name, key in CITIES:
        try:
            zones.append((name, ZoneInfo(key)))
        except Exception:
            continue
    local_key = time.tzname[0]
    while True:
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

        rows.append(DIM + " ── WORLD CLOCK ──")
        namew = max(9, min(15, w - 26))
        for dt, name in entries:
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
        while len(rows) < h - 1:
            rows.append("")
        rows.append(C2 + " " + "▔" * max(1, w - 2))
        draw(rows, w, h)
        time.sleep(0.25)


main()
