// terminal-toys - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Clocks: this server's, everyone else's, and the ones counting down.
//!
//! A port of clocks.py: the big clock, the countdown bars, the pomodoro,
//! and a world clock. The one widget here that needs a timezone database,
//! which is why the core carries one.

use std::time::Duration;

use chrono::{Datelike, Local, NaiveTime, Offset, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use toys_core as tc;

/// Five rows per digit, as clocks.py draws them.
const BIG: &[(char, [&str; 5])] = &[
    ('0', ["███", "█ █", "█ █", "█ █", "███"]),
    ('1', ["  █", "  █", "  █", "  █", "  █"]),
    ('2', ["███", "  █", "███", "█  ", "███"]),
    ('3', ["███", "  █", "███", "  █", "███"]),
    ('4', ["█ █", "█ █", "███", "  █", "  █"]),
    ('5', ["███", "█  ", "███", "  █", "███"]),
    ('6', ["███", "█  ", "███", "█ █", "███"]),
    ('7', ["███", "  █", "  █", "  █", "  █"]),
    ('8', ["███", "█ █", "███", "█ █", "███"]),
    ('9', ["███", "█ █", "███", "  █", "███"]),
    (':', ["   ", " █ ", "   ", " █ ", "   "]),
];

fn glyph(c: char) -> [&'static str; 5] {
    BIG.iter()
        .find(|(ch, _)| *ch == c)
        .map(|(_, rows)| *rows)
        .unwrap_or(["   ", "   ", "   ", "   ", "   "])
}

/// A time as five rows of blocks.
fn render_big(text: &str) -> Vec<String> {
    let mut rows = vec![String::new(); 5];
    for c in text.chars() {
        let art = glyph(c);
        for (i, row) in rows.iter_mut().enumerate() {
            row.push_str(art[i]);
            row.push(' ');
        }
    }
    rows
}

fn hms(seconds: i64) -> String {
    let s = seconds.max(0);
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// The offset from UTC, as a person writes it.
fn offset_str<T>(when: &chrono::DateTime<T>) -> String
where
    T: TimeZone,
    T::Offset: Offset,
{
    let seconds = when.offset().fix().local_minus_utc();
    let sign = if seconds < 0 { '-' } else { '+' };
    let total = seconds.abs();
    let (hours, minutes) = (total / 3600, (total % 3600) / 60);
    if minutes == 0 {
        format!("UTC{}{}", sign, hours)
    } else {
        format!("UTC{}{}:{:02}", sign, hours, minutes)
    }
}

/// A progress bar of the given width, filled to `frac`.
fn bar(frac: f64, width: usize) -> String {
    let filled = ((frac.clamp(0.0, 1.0)) * width as f64).round() as usize;
    let mut out = "█".repeat(filled);
    out.push_str(&"░".repeat(width.saturating_sub(filled)));
    out
}

struct Countdown {
    label: String,
    left: i64,
    frac: f64,
    /// Its own colour: the three are different clocks, not three readings
    /// of one, and a single hue makes them look like a stack of the same
    /// thing.
    ink: String,
}

/// Which days are working days, and between which hours.
#[derive(Clone)]
struct Office {
    /// Monday is 0. Defaults to Monday through Friday.
    days: Vec<u32>,
    start: u32,
    end: u32,
}

impl Office {
    fn from_config(cfg: &serde_json::Value) -> Office {
        let days = cfg
            .get("work_days")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| day_number(v))
                    .filter(|d| *d < 7)
                    .collect::<Vec<u32>>()
            })
            .filter(|d: &Vec<u32>| !d.is_empty())
            .unwrap_or_else(|| vec![0, 1, 2, 3, 4]);
        Office {
            days,
            // Hours are 0..=23. An out-of-range value used to reach
            // NaiveTime::from_hms_opt and unwrap, so a typo of 24 aborted
            // the widget on the first frame instead of rendering.
            start: (tc::cfg_usize(cfg, "work_start_hour", 9) as u32).min(23),
            end: (tc::cfg_usize(cfg, "work_end_hour", 18) as u32).min(23),
        }
    }

    fn is_working_day(&self, when: &chrono::DateTime<Local>) -> bool {
        self.days.contains(&when.weekday().num_days_from_monday())
    }

    fn is_open(&self, when: &chrono::DateTime<Local>) -> bool {
        self.is_working_day(when) && (self.start..self.end).contains(&when.hour())
    }

    /// The next opening after `now`, skipping days that are not worked.
    fn next_open(&self, now: &chrono::DateTime<Local>) -> chrono::DateTime<Local> {
        let mut day = now.date_naive();
        for _ in 0..14 {
            let candidate = day.and_time(NaiveTime::from_hms_opt(self.start, 0, 0).unwrap());
            let stamp = Local.from_local_datetime(&candidate).single();
            if let Some(stamp) = stamp {
                if stamp > *now && self.is_working_day(&stamp) {
                    return stamp;
                }
            }
            day += chrono::Duration::days(1);
        }
        *now
    }

    /// The last closing before `now`, for measuring how far into the gap
    /// we are - without it the bar has nothing to fill from.
    fn prev_close(&self, now: &chrono::DateTime<Local>) -> chrono::DateTime<Local> {
        let mut day = now.date_naive();
        for _ in 0..14 {
            let candidate = day.and_time(NaiveTime::from_hms_opt(self.end, 0, 0).unwrap());
            if let Some(stamp) = Local.from_local_datetime(&candidate).single() {
                if stamp <= *now && self.is_working_day(&stamp) {
                    return stamp;
                }
            }
            day -= chrono::Duration::days(1);
        }
        *now
    }
}

/// A weekday from a config entry, as a number or a name.
fn day_number(value: &serde_json::Value) -> Option<u32> {
    if let Some(n) = value.as_u64() {
        return Some(n as u32);
    }
    let name = value.as_str()?.to_lowercase();
    ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
        .iter()
        .position(|d| name.starts_with(d))
        .map(|i| i as u32)
}

/// The three fixed countdowns: the hour, the working day, and midnight.
fn countdowns(now: chrono::DateTime<Local>, office: &Office) -> Vec<Countdown> {
    let mut out = Vec::new();

    let into_hour = now.minute() as i64 * 60 + now.second() as i64;
    out.push(Countdown {
        label: "Next Hour".into(),
        left: 3600 - into_hour,
        frac: into_hour as f64 / 3600.0,
        ink: tc::rgb(90, 220, 255),
    });

    // Inside working hours this counts to the close; outside them, to the
    // next opening - which on a Friday evening is Monday morning, not
    // tomorrow. The bar fills from the last close, so the gap has a span.
    let today = now.date_naive();
    let (label, target, from) = if office.is_open(&now) {
        let start = Local
            .from_local_datetime(&today.and_time(NaiveTime::from_hms_opt(office.start, 0, 0).unwrap()))
            .single()
            .unwrap_or(now);
        let end = Local
            .from_local_datetime(&today.and_time(NaiveTime::from_hms_opt(office.end, 0, 0).unwrap()))
            .single()
            .unwrap_or(now);
        ("End of Office Hour", end, start)
    } else {
        ("Start of Office Hour", office.next_open(&now), office.prev_close(&now))
    };
    let span = (target - from).num_seconds().max(1);
    let left = (target - now).num_seconds();
    out.push(Countdown {
        label: label.into(),
        left,
        frac: 1.0 - (left as f64 / span as f64),
        ink: tc::rgb(255, 200, 90),
    });

    let (into_day, day_len) = day_bounds(&now);
    out.push(Countdown {
        label: "End of Day".into(),
        left: day_len - into_day,
        frac: into_day as f64 / day_len as f64,
        ink: tc::rgb(175, 130, 255),
    });
    out
}

/// How far into the local day `now` is, and how long that day runs.
///
/// Measured between two midnights rather than assumed to be 86,400
/// seconds: the day the clocks go forward is 23 hours long and the day
/// they go back is 25, and a fixed span puts both the time remaining and
/// the bar an hour out for the whole of a transition day. Generic over the
/// zone so the transition can be tested against a real one rather than
/// whichever zone this machine happens to be in.
fn day_bounds<T: TimeZone>(now: &chrono::DateTime<T>) -> (i64, i64) {
    let zone = now.timezone();
    // Midnight itself is sometimes the hour that does not exist - Chile and
    // Lebanon have moved their clocks at exactly that moment - so take the
    // first minute of the day that does.
    let midnight = |day: chrono::NaiveDate| {
        (0..180).find_map(|m| {
            zone.from_local_datetime(
                &(day.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap())
                    + chrono::Duration::minutes(m)),
            )
            .earliest()
        })
    };
    let today = now.date_naive();
    match (midnight(today), midnight(today + chrono::Duration::days(1))) {
        (Some(start), Some(end)) if end.timestamp() > start.timestamp() => (
            (now.timestamp() - start.timestamp()).max(0),
            end.timestamp() - start.timestamp(),
        ),
        // A day the zone cannot place at all: fall back to the wall clock,
        // which is what the day looks like on every day but the two it moves.
        _ => (now.num_seconds_from_midnight() as i64, 86_400),
    }
}

/// Seconds each flash stays lit.
const FLASH_ON: f64 = 0.35;

/// Is the panel lit right now?
///
/// Flashes are derived from one timestamp rather than driven by sleeps, so
/// the render loop keeps running - the clock stays live and keys stay
/// responsive while it blinks.
fn flash_window(started: Option<f64>, count: u32, gap: f64, now: f64) -> bool {
    let started = match started {
        Some(s) => s,
        None => return false,
    };
    let since = now - started;
    (0..count.max(1)).any(|n| {
        let edge = n as f64 * gap;
        since >= edge && since < edge + FLASH_ON
    })
}

/// The same frame, repainted solid: colours stripped, one loud background.
fn flash_frame(rows: &[String], w: usize, h: usize, bg: &str, fg: &str) -> Vec<String> {
    (0..h)
        .map(|i| {
            let plain = rows.get(i).map(|r| strip_ansi(r)).unwrap_or_default();
            format!("{}{}{}", bg, fg, tc::pad(&plain, w))
        })
        .collect()
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for n in chars.by_ref() {
                if n == 'm' || n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A readable foreground for whatever the flash colour is.
///
/// A near-white flash needs dark text on it and a dark one needs light;
/// picking by luminance means a configured colour cannot make the panel
/// unreadable at the moment it is trying to get your attention.
fn flash_ink(rgb: (u8, u8, u8)) -> String {
    let lum = (0.299 * rgb.0 as f64 + 0.587 * rgb.1 as f64 + 0.114 * rgb.2 as f64) / 255.0;
    if lum > 0.5 {
        tc::rgb(18, 20, 26)
    } else {
        tc::rgb(255, 240, 240)
    }
}

/// Herdr, when we happen to be inside it, can raise a real toast.
///
/// Purely additive: nothing here requires Herdr, and outside it this is a
/// no-op. The widget usually runs on a server, so anything local to that
/// machine - notify-send, a sound file - would fire where nobody is.
fn herdr_toast(title: &str, body: &str) {
    if std::env::var("HERDR_ENV").unwrap_or_default() != "1" {
        return;
    }
    let mut sent = SENT.lock().unwrap_or_else(|e| e.into_inner());
    reap(&mut sent);
    if let Ok(child) = std::process::Command::new("herdr")
        // --body, not a second positional: `herdr notification show` takes
        // one <TITLE> and the body is an option. Passing it positionally
        // fails with "unknown option" - silently, since stderr is nulled -
        // so this toast had never once been shown.
        .args(["notification", "show", title, "--body", body, "--sound", "done"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        sent.push(child);
    }
}

/// Toasts already sent, still holding an exit status nobody has read.
///
/// A `Child` that is never waited on stays a zombie for as long as the
/// widget runs, and an ignored pomodoro toasts once a minute - a panel
/// left alone for a day would leave hundreds of them behind. Each toast
/// clears the ones before it, which needs no thread of its own.
static SENT: std::sync::Mutex<Vec<std::process::Child>> = std::sync::Mutex::new(Vec::new());

/// Drop the handle of every toast that has finished.
///
/// `try_wait` does not block, so a `herdr` that hangs keeps its slot in
/// the list rather than holding up the render loop. A handle that cannot
/// be waited on at all is dropped too: keeping it reaps nothing.
fn reap(sent: &mut Vec<std::process::Child>) {
    sent.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
}

/// The pomodoro, and the state it keeps between runs.
///
/// Hidden means suspended, not merely invisible. A timer that keeps
/// counting while out of sight is worse than no timer: you come back to a
/// focus block that expired half an hour ago. Hiding freezes it where it
/// stands and showing resumes it only if it was running when it went away.
struct Pomodoro {
    phase: Phase,
    running: bool,
    /// pomodoro_enabled, then whatever the state file remembers: whether
    /// the timer is on screen at all. It is the visibility flag in
    /// clocks.py - and stored under the same "enabled" key - not a flag
    /// for whether it runs, which no setting starts.
    shown: bool,
    /// When the current phase ends, while running.
    deadline: f64,
    /// What was left when it stopped, while not.
    left: f64,
    done: u32,
    focus: f64,
    short: f64,
    long: f64,
    before_long: u32,
    bell: bool,
    rang_at: i64,
    /// The day the tally belongs to, as %Y-%m-%d. Kept so a panel left
    /// running over midnight zeroes rather than adding to yesterday.
    day: String,
    /// show_hints: whether the pomodoro's key hints are under the panel.
    /// A display preference rather than timer state, but it rides in the
    /// same file - under "hints" - so the panel comes back looking how it
    /// was left.
    hints: bool,
    /// pomodoro_notify: announce a phase change to the terminal and to
    /// Herdr. clocks.py reads this and the port did not, so setting it did
    /// nothing here.
    notify: bool,
}

/// Where the pomodoro's state lives, shared with clocks.py.
///
/// The same path and the same keys, so a session started under one
/// implementation is picked up by the other rather than each keeping a
/// private tally of the same afternoon.
fn state_file() -> String {
    let base = std::env::var("XDG_STATE_HOME").unwrap_or_else(|_| {
        format!(
            "{}/.local/state",
            std::env::var("HOME").unwrap_or_default()
        )
    });
    format!("{}/terminal-toys/pomodoro.json", base)
}

/// Today, in the form the state file stores.
fn today() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Focus,
    Short,
    Long,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Focus => "FOCUS",
            Phase::Short => "SHORT BREAK",
            Phase::Long => "LONG BREAK",
        }
    }
}

impl Pomodoro {
    fn new(cfg: &serde_json::Value) -> Pomodoro {
        let focus = tc::cfg_f64(cfg, "pomodoro_focus_minutes", 25.0);
        let mut it = Pomodoro {
            phase: Phase::Focus,
            running: false,
            // Off until [p], which is what clocks.py does and what the doc
            // promises: pomodoro_enabled is its visibility flag and it
            // defaults to false. Hardcoding this true put an optional
            // panel on screen for everyone who had turned it off.
            shown: cfg
                .get("pomodoro_enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            deadline: 0.0,
            left: focus * 60.0,
            done: 0,
            focus,
            short: tc::cfg_f64(cfg, "pomodoro_short_break_minutes", 5.0),
            long: tc::cfg_f64(cfg, "pomodoro_long_break_minutes", 15.0),
            before_long: tc::cfg_usize(cfg, "pomodoro_sessions_before_long_break", 4) as u32,
            bell: cfg
                .get("pomodoro_bell")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            rang_at: -1,
            day: today(),
            // Visible by default, as clocks.py has it: the hints are how
            // the keys are found in the first place, and starting hidden
            // means a reader has to already know the key that reveals them.
            hints: cfg
                .get("show_hints")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            // True, as clocks.py has it: a break ending is worth saying
            // out loud, and a machine with no config should behave the
            // same under either implementation.
            notify: cfg
                .get("pomodoro_notify")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        };
        it.left = it.duration();
        // Never running from config alone. Setting it running here left the
        // deadline at zero, and a deadline of zero falls back to a `left`
        // that nothing decrements: a timer frozen at 25:00 with no "paused"
        // beside it. Only a saved deadline resumes a block, in load().
        it.load();
        it
    }

    /// Read the state file, if there is one.
    ///
    /// Preferences outlive the day; only the tally and the block in
    /// progress belong to it, so a file from yesterday contributes the
    /// focus length and nothing else.
    fn load(&mut self) {
        let Ok(text) = std::fs::read_to_string(state_file()) else {
            return;
        };
        let Ok(d) = serde_json::from_str::<serde_json::Value>(&text) else {
            return;
        };
        if let Some(v) = d.get("focus").and_then(|v| v.as_f64()) {
            self.focus = v;
        }
        // "enabled" is visibility and "hints" is the hint preference - the
        // port had the second one holding the first, so hiding the panel
        // rewrote whether the keys are listed and hiding the keys moved
        // the panel.
        if let Some(v) = d.get("enabled").and_then(|v| v.as_bool()) {
            self.shown = v;
        }
        if let Some(v) = d.get("hints").and_then(|v| v.as_bool()) {
            self.hints = v;
        }
        if d.get("day").and_then(|v| v.as_str()) != Some(self.day.as_str()) {
            return; // a new day starts a fresh count
        }
        if let Some(v) = d.get("phase").and_then(|v| v.as_str()) {
            self.phase = match v {
                "short" => Phase::Short,
                "long" => Phase::Long,
                _ => Phase::Focus,
            };
        }
        self.done = d.get("completed").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        if let Some(v) = d.get("left").and_then(|v| v.as_f64()) {
            self.left = v;
        }
        // Resume mid-phase. If it elapsed while the panel was away the
        // timer simply shows how far over it has run, which is what the
        // Python does rather than pretending it ended on time.
        let deadline = d.get("deadline").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if d.get("running").and_then(|v| v.as_bool()).unwrap_or(false) && deadline > 0.0 {
            self.deadline = deadline;
            self.running = true;
        }
    }

    /// Write the state file, failing quietly.
    ///
    /// A panel that cannot save its tally should still show the clock.
    fn save(&self) {
        let path = state_file();
        if let Some(dir) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let body = serde_json::json!({
            "day": self.day,
            "phase": match self.phase {
                Phase::Focus => "focus",
                Phase::Short => "short",
                Phase::Long => "long",
            },
            "completed": self.done,
            "focus": self.focus,
            "enabled": self.shown,
            "running": self.running,
            "was_running": self.running,
            "hints": self.hints,
            "left": self.left,
            "deadline": self.deadline,
        });
        let _ = std::fs::write(&path, body.to_string());
    }

    /// Zero the tally when the date changes, even if nothing restarted.
    ///
    /// clocks.py's own docstring names this: the count previously reset
    /// only on load, so a panel left running over midnight kept adding to
    /// yesterday's total. The port had reintroduced exactly that.
    fn roll_day(&mut self) -> bool {
        let now = today();
        if now != self.day {
            self.day = now;
            self.done = 0;
            self.save();
            return true;
        }
        false
    }

    fn duration(&self) -> f64 {
        60.0 * match self.phase {
            Phase::Focus => self.focus,
            Phase::Short => self.short,
            Phase::Long => self.long,
        }
    }

    /// Seconds left, negative once the phase has been overrun.
    fn signed(&self, now: f64) -> f64 {
        if self.running && self.deadline > 0.0 {
            self.deadline - now
        } else {
            self.left
        }
    }

    fn remaining(&self, now: f64) -> f64 {
        self.signed(now).max(0.0)
    }

    fn overtime(&self, now: f64) -> f64 {
        (-self.signed(now)).max(0.0)
    }

    /// Show or hide, suspending with it.
    fn toggle(&mut self, now: f64) {
        if self.shown {
            self.left = self.signed(now);
            self.shown = false;
        } else {
            self.shown = true;
            if self.running {
                self.deadline = now + self.left;
            }
        }
        self.save();
    }

    fn start_stop(&mut self, now: f64) {
        if self.running {
            self.left = self.signed(now);
            self.running = false;
        } else {
            self.running = true;
            self.deadline = now + self.left;
        }
        self.save();
    }

    /// Move to whatever comes next, counting a finished focus block.
    fn advance(&mut self, now: f64) {
        if self.phase == Phase::Focus {
            self.done += 1;
            self.phase = if self.before_long > 0 && self.done % self.before_long == 0 {
                Phase::Long
            } else {
                Phase::Short
            };
        } else {
            self.phase = Phase::Focus;
        }
        self.left = self.duration();
        self.deadline = now + self.left;
        self.rang_at = -1;
        self.save();
    }

    /// What pressing the break key will do, right now.
    ///
    /// One key for both directions, and the label says which it is about to
    /// do rather than a fixed word like "skip" that describes neither. The
    /// long break is called out because it is the one worth waiting for.
    fn next_label(&self) -> String {
        if self.phase != Phase::Focus {
            return "[b]reak stop".into();
        }
        let long_next = self.before_long > 0 && (self.done + 1) % self.before_long == 0;
        if long_next {
            "[b]reak start (long)".into()
        } else {
            "[b]reak start".into()
        }
    }

    /// Zero the tally, once there is something to zero.
    fn reset_count(&mut self) {
        self.done = 0;
        self.save();
    }

    /// Lengthen or shorten the block you are actually in, in minutes.
    ///
    /// Whichever phase is running: focus during focus, and that break during
    /// a break. It used to write to `focus` whatever the phase, so pressing
    /// it during a break moved a number that was not on screen and left the
    /// countdown alone - and the hint beside it read "focus" while the line
    /// above it read BREAK. Both breaks keep their own length, so shortening
    /// a short break does not shorten the long one.
    ///
    /// It takes no clock: moving the finish line needs the size of the
    /// change, not the time of it.
    fn adjust(&mut self, delta: f64) {
        let before = self.duration();
        let slot = match self.phase {
            Phase::Focus => &mut self.focus,
            Phase::Short => &mut self.short,
            Phase::Long => &mut self.long,
        };
        *slot = (*slot + delta).clamp(1.0, 180.0);
        // Move the finish line by however much the block actually changed -
        // the clamp can make that less than was asked for - rather than
        // starting the block again. A minute added twenty minutes into a
        // twenty-five minute block leaves six, not twenty-six. Shortening
        // below what has already gone leaves the block over, which is
        // true, and the counter says so by climbing.
        let moved = self.duration() - before;
        self.left += moved;
        if self.running {
            self.deadline += moved;
        }
        self.save();
    }

    /// The length of the block in progress, in minutes.
    fn minutes(&self) -> i64 {
        (self.duration() / 60.0).round() as i64
    }

    fn restart(&mut self, now: f64) {
        self.left = self.duration();
        self.deadline = now + self.left;
        self.rang_at = -1;
        self.save();
    }

    /// One tick: ring on elapse, and once a minute while overrunning.
    ///
    /// It does not advance on its own. A break that starts itself while you
    /// are mid-sentence is a break you ignore, and then the count is a lie.
    /// Returns true on the tick an alert fires, so the panel can flash.
    fn tick(&mut self, now: f64) -> bool {
        if !self.running || !self.shown {
            return false;
        }
        let over = self.overtime(now);
        if over <= 0.0 {
            return false;
        }
        let minute = (over / 60.0) as i64;
        if minute == self.rang_at {
            return false;
        }
        self.rang_at = minute;
        // Every channel gets the same sentence, and how far over is part of
        // it: the render loop used to send a second, richer toast of its
        // own, which meant two notifications under Herdr and one even with
        // pomodoro_notify off.
        self.alert(&format!(
            "{} elapsed{}",
            self.phase.label(),
            if over >= 60.0 {
                format!(", {} over", hms(over as i64))
            } else {
                String::new()
            }
        ));
        true
    }

    /// Nudge whoever is in front of the terminal, over SSH if need be.
    ///
    /// BEL is universal. OSC 9 covers iTerm2, WezTerm, Windows Terminal
    /// and Ghostty; OSC 777 covers urxvt and several others. Terminals
    /// ignore the sequences they do not implement, so sending both costs
    /// nothing, and a multiplexer in between decides whether to forward
    /// them.
    fn alert(&self, text: &str) {
        if self.bell {
            tc::out("\x07");
        }
        if self.notify {
            tc::out(&format!("\x1b]9;{}\x07", text));
            tc::out(&format!("\x1b]777;notify;Pomodoro;{}\x07", text));
        }
        tc::flush();
        if self.notify {
            herdr_toast("Pomodoro", text);
        }
    }
}


struct City {
    name: String,
    zone: Tz,
}

fn main() {
    tc::maybe_help(include_str!("clocks_help.txt"));
    let cfg = tc::load_config("clocks");
    let office = Office::from_config(&cfg);
    let cities = load_cities(&cfg);

    let p = palette();
    let mut pomo = Pomodoro::new(&cfg);
    let flash_rgb = cfg
        .get("pomodoro_flash_rgb")
        .and_then(|v| v.as_array())
        .map(|a| {
            let n = |i: usize| a.get(i).and_then(|v| v.as_u64()).unwrap_or(250) as u8;
            (n(0), n(1), n(2))
        })
        .unwrap_or((246, 248, 252));
    let flash_bg = tc::bg(flash_rgb.0, flash_rgb.1, flash_rgb.2);
    let flash_fg = flash_ink(flash_rgb);
    let flash_count = tc::cfg_usize(&cfg, "pomodoro_flash_count", 2) as u32;
    let flash_gap = tc::cfg_f64(&cfg, "pomodoro_flash_gap", 1.0);
    let flash_on = cfg
        .get("pomodoro_flash")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let mut flash_started: Option<f64> = None;
    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut scroll = 0usize;

    loop {
        for key in keyboard.poll() {
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "up" | "k" | "K" => scroll = scroll.saturating_sub(1),
                "down" | "j" | "J" => scroll += 1,
                "pgup" => scroll = scroll.saturating_sub(8),
                "pgdn" => scroll += 8,
                "home" => scroll = 0,
                // Clamped to the last city further down, which is the only
                // place that knows how many there are.
                "end" => scroll = usize::MAX / 2,
                "p" | "P" => pomo.toggle(seconds()),
                // show_hints decides where this starts and [?] moves it,
                // but the answer outlives the session: it rides in the
                // pomodoro's state file, so hiding the hints once hides
                // them tomorrow too. It used to live in a local nothing
                // wrote down.
                "?" | "h" => {
                    pomo.hints = !pomo.hints;
                    pomo.save();
                }
                // Everything below moves a timer that is not running, so
                // it is ignored rather than silently acted on.
                _ if !pomo.shown => {}
                " " => pomo.start_stop(seconds()),
                // One action, three mnemonics: skip / break / end.
                "s" | "S" | "b" | "B" | "e" | "E" => pomo.advance(seconds()),
                "r" | "R" => pomo.restart(seconds()),
                "0" | "c" => pomo.reset_count(),
                "+" | "=" => pomo.adjust(1.0),
                "-" | "_" => pomo.adjust(-1.0),
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let now = Local::now();
        let mut rows = vec![tc::title("clocks", w, &p.head)];
        rows.push(tc::seg(&[(p.lbl.as_str(), " ── SERVER TIME ──".into())], w - 1));

        // Two colours down the digits, not one: the top three rows are
        // bright and the base is darker, which is what gives them weight.
        for (i, line) in render_big(&now.format("%H:%M:%S").to_string())
            .into_iter()
            .enumerate()
        {
            let ink = if i < 3 { &p.big_top } else { &p.big_base };
            rows.push(tc::seg(&[(ink.as_str(), format!(" {}", line))], w - 1));
        }
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.txt.as_str(), format!(" {}", now.format("%Y-%m-%d"))),
                (
                    p.dim.as_str(),
                    format!("  {}   {}", now.format("%A").to_string().to_uppercase(), offset_str(&now)),
                ),
            ],
            w - 1,
        ));
        rows.push(String::new());

        rows.push(tc::seg(&[(p.lbl.as_str(), " ── COUNTDOWN ──".into())], w - 1));
        let bar_w = w.saturating_sub(3).min(90);

        // The pomodoro leads the section, as it does in the Python.
        let stamp = seconds();
        // Before anything reads the tally: a panel left running over
        // midnight must zero rather than keep adding to yesterday.
        pomo.roll_day();
        if pomo.tick(stamp) {
            // The flash only. tick() has already alerted on every channel
            // the settings allow, the toast among them.
            flash_started = Some(stamp);
        }
        if pomo.shown {
            let over = pomo.overtime(stamp);
            let left = pomo.remaining(stamp);
            let frac = 1.0 - (left / pomo.duration().max(1.0));
            // Paused takes its own ink: a stopped timer showing focus red
            // reads as one that is running.
            let ink = if !pomo.running {
                &p.paused
            } else if pomo.phase == Phase::Focus {
                &p.focus
            } else {
                &p.rest
            };
            rows.push(tc::seg(
                &[
                    (
                        ink.as_str(),
                        format!(" {}", tc::pad(&format!("Pomodoro · {}", pomo.phase.label()), 23)),
                    ),
                    (
                        if over > 0.0 { &p.over } else { &p.txt },
                        if over > 0.0 {
                            format!("+{}", hms(over as i64))
                        } else {
                            hms(left as i64)
                        },
                    ),
                    (
                        p.paused.as_str(),
                        if pomo.running { String::new() } else { "  paused".into() },
                    ),
                    (
                        if over > 0.0 { &p.over } else { &p.dim },
                        if over > 0.0 { "  OVER".into() } else { String::new() },
                    ),
                    (p.dim.as_str(), format!("   {} done", pomo.done)),
                ],
                w - 1,
            ));
            if over > 0.0 {
                // The bar rescales to duration plus overtime, so the red
                // share grows the longer the phase is ignored - the point
                // being that it keeps getting harder to miss.
                let total = pomo.duration() + over;
                let base = ((bar_w as f64 * pomo.duration() / total).round() as usize).max(1);
                rows.push(tc::seg(
                    &[
                        (ink.as_str(), format!(" {}", "█".repeat(base))),
                        (p.over.as_str(), "█".repeat(bar_w.saturating_sub(base))),
                    ],
                    w - 1,
                ));
            } else {
                rows.push(tc::seg(
                    &[(ink.as_str(), format!(" {}", bar(frac, bar_w)))],
                    w - 1,
                ));
            }
        }
        for item in countdowns(now, &office) {
            rows.push(tc::seg(
                &[
                    (p.txt.as_str(), format!(" {:<21}", item.label)),
                    (p.accent.as_str(), hms(item.left)),
                ],
                w - 1,
            ));
            rows.push(tc::seg(
                &[(item.ink.as_str(), format!(" {}", bar(item.frac, bar_w)))],
                w - 1,
            ));
        }
        rows.push(String::new());

        // The world clock takes whatever is left, and says which slice of
        // the list it is showing rather than silently truncating.
        let room = h.saturating_sub(rows.len() + 3);
        if room >= 2 && !cities.is_empty() {
            let shown = room.saturating_sub(1).min(cities.len());
            if scroll + shown > cities.len() {
                scroll = cities.len().saturating_sub(shown);
            }
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── WORLD CLOCK ── ".into()),
                    (
                        p.dim.as_str(),
                        format!(" {}-{} of {}  ↑↓", scroll + 1, scroll + shown, cities.len()),
                    ),
                ],
                w - 1,
            ));
            for city in cities.iter().skip(scroll).take(shown) {
                let there = now.with_timezone(&city.zone);
                // Sun or moon by the local hour, which is the fastest way
                // to read "is it a reasonable time to message them".
                let (ink, glyph) = phase_of(&there, &p);
                let day_shift = there.date_naive().signed_duration_since(now.date_naive()).num_days();
                rows.push(tc::seg(
                    &[
                        (ink.as_str(), format!(" {} ", glyph)),
                        (p.txt.as_str(), tc::pad(&city.name, 16)),
                        (p.txt.as_str(), there.format("%H:%M").to_string()),
                        (
                            p.dim.as_str(),
                            format!("  {}  {}", there.format("%a"), offset_str(&there)),
                        ),
                        (
                            p.dim.as_str(),
                            match day_shift {
                                0 => String::new(),
                                d if d > 0 => format!("    +{}d", d),
                                d => format!("    {}d", d),
                            },
                        ),
                    ],
                    w - 1,
                ));
            }
        }

        // Navigation first, as every other widget in the collection has it,
        // then the pomodoro's own controls, then the panel keys. Only the
        // pomodoro controls hide behind ?: hiding the way back would leave
        // no way back.
        let mut hints: Vec<Vec<(&str, String)>> = vec![vec![
            (p.accent.as_str(), "↑↓".into()),
            (p.dim.as_str(), " cities".into()),
        ]];
        if pomo.shown && pomo.hints {
            hints.push(vec![
                (p.dim.as_str(), "[space] ".into()),
                (
                    p.txt.as_str(),
                    if pomo.running { "pause".into() } else { "start".into() },
                ),
            ]);
            hints.push(vec![(p.dim.as_str(), pomo.next_label())]);
            hints.push(vec![(p.dim.as_str(), "[r]estart".into())]);
            // Both halves, because each answers a question the other does
            // not. The step says what the key will do; the value is the
            // only place the block length appears while the timer runs,
            // since the countdown shows what is left rather than what it
            // started from.
            hints.push(vec![
                (p.dim.as_str(), "[±]1min ".to_string()),
                // "interval" rather than "focus": the key adjusts whichever
                // block is running, so naming one of the three would be
                // wrong in two phases out of three.
                (p.txt.as_str(), format!("(interval {}min)", pomo.minutes())),
            ]);
            if pomo.done > 0 {
                // Nothing to reset at zero, so it only appears once it counts.
                hints.push(vec![(
                    p.dim.as_str(),
                    format!("[0]reset {} done", pomo.done),
                )]);
            }
        }
        hints.push(vec![(
            p.dim.as_str(),
            if pomo.shown {
                "[p]omodoro off".into()
            } else {
                "[p]omodoro".into()
            },
        )]);
        if pomo.shown {
            // Shown even when the controls are hidden, so there is always a
            // way back. Names the action rather than the state.
            hints.push(vec![(
                p.dim.as_str(),
                format!(
                    "[?]{} pomodoro tips",
                    if pomo.hints { "hide" } else { "show" }
                ),
            )]);
        }
        hints.push(vec![(p.dim.as_str(), "[q]uit".into())]);
        let foot: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
        while rows.len() < h.saturating_sub(foot.len()) {
            rows.push(String::new());
        }
        rows.extend(foot);
        if flash_on && flash_window(flash_started, flash_count, flash_gap, seconds()) {
            tc::draw(&flash_frame(&rows, w, h, &flash_bg, &flash_fg), w, h);
        } else {
            tc::draw(&rows, w, h);
        }
        // Forget a flash once its last blink has passed, so the check stops
        // costing anything for the rest of the session.
        if let Some(started) = flash_started {
            if seconds() - started > flash_count as f64 * flash_gap + FLASH_ON {
                flash_started = None;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Colour and glyph for what people there are plausibly doing.
///
/// Four states rather than two: asleep, weekend, working, and the evening
/// either side of it. The glyph says light or dark and the colour says
/// whether anyone is likely to be at a desk, which is the actual question
/// being asked of a world clock.
fn phase_of(there: &chrono::DateTime<Tz>, p: &Palette) -> (String, &'static str) {
    let hour = there.hour() as f64 + there.minute() as f64 / 60.0;
    let weekend = there.weekday().num_days_from_monday() >= 5;
    if hour < 6.5 || hour >= 22.0 {
        return (p.night.clone(), "☾");
    }
    if weekend {
        return (p.weekend.clone(), "☀");
    }
    if (9.0..18.0).contains(&hour) {
        return (p.work.clone(), "☀");
    }
    if hour >= 18.0 {
        return (p.eve.clone(), "☾");
    }
    (p.eve.clone(), "☀")
}

/// The configured cities, or the four the Python ships with.
///
/// A zone the database does not know is dropped rather than defaulted to
/// UTC: a clock quietly showing the wrong city is worse than one absent.
fn load_cities(cfg: &serde_json::Value) -> Vec<City> {
    let mut out = Vec::new();
    // Whether the key is there at all, which is a different thing from it
    // being empty. Absent means nobody has said what they want and a default
    // is a kindness; present means they have, and substituting four cities
    // of our own for the answer they gave is the widget telling them about
    // somewhere they did not ask about.
    let asked = cfg.get("cities").and_then(|v| v.as_array()).is_some();
    if let Some(items) = cfg.get("cities").and_then(|v| v.as_array()) {
        for pair in items {
            let name = pair.get(0).and_then(|v| v.as_str()).unwrap_or("");
            let zone = pair.get(1).and_then(|v| v.as_str()).unwrap_or("");
            if let Ok(tz) = zone.parse::<Tz>() {
                out.push(City {
                    name: name.to_string(),
                    zone: tz,
                });
            }
        }
    }
    if !out.is_empty() {
        // West to east by current offset, so the row order is a map.
        let now = Utc::now();
        out.sort_by_key(|c| {
            (
                now.with_timezone(&c.zone).offset().fix().local_minus_utc(),
                c.name.clone(),
            )
        });
        return out;
    }
    // Only when nothing was configured. A list that was set and parsed to
    // nothing - empty, or every zone a typo - is left empty, so the row that
    // is missing is the question rather than a plausible answer to a
    // different one.
    if out.is_empty() && !asked {
        for (name, zone) in [
            ("San Francisco", "America/Los_Angeles"),
            ("London", "Europe/London"),
            ("Singapore", "Asia/Singapore"),
            ("Tokyo", "Asia/Tokyo"),
        ] {
            if let Ok(tz) = zone.parse::<Tz>() {
                out.push(City {
                    name: name.into(),
                    zone: tz,
                });
            }
        }
    }
    out
}

fn seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

struct Palette {
    focus: String,
    rest: String,
    paused: String,
    over: String,
    work: String,
    eve: String,
    night: String,
    weekend: String,
    dim: String,
    txt: String,
    lbl: String,
    accent: String,
    head: String,
    big_top: String,
    big_base: String,
    // Nothing draws these three. They are clocks.py's palette carried
    // over whole, and a palette is a set: taking the unused thirds out
    // would leave the rest looking chosen rather than inherited, and the
    // file they were inherited from is now deleted, so there would be
    // nothing left to check them against.
    #[allow(dead_code)]
    bar: String,
    #[allow(dead_code)]
    sun: String,
    #[allow(dead_code)]
    moon: String,
}

fn palette() -> Palette {
    // clocks.py's own palette, value for value. Its DIM is a green-grey
    // rather than the blue-grey the other widgets use, and the difference
    // is visible the moment the two sit side by side.
    Palette {
        focus: tc::rgb(255, 130, 120),
        rest: tc::rgb(120, 235, 170),
        paused: tc::rgb(160, 172, 190),
        over: tc::rgb(255, 80, 90),
        work: tc::rgb(130, 255, 180),
        eve: tc::rgb(255, 200, 90),
        night: tc::rgb(95, 130, 175),
        weekend: tc::rgb(150, 150, 170),
        dim: tc::rgb(70, 130, 110),
        txt: tc::rgb(220, 255, 240),
        lbl: tc::rgb(70, 130, 110),
        accent: tc::rgb(90, 220, 255),
        head: tc::rgb(0, 255, 170),
        big_top: tc::rgb(120, 255, 200),
        big_base: tc::rgb(40, 150, 120),
        bar: tc::rgb(90, 220, 255),
        sun: tc::rgb(255, 210, 120),
        moon: tc::rgb(150, 170, 210),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One lock for every test that touches the state file.
    ///
    /// XDG_STATE_HOME is process-global and cargo runs tests in parallel
    /// threads, so without this one test's sandbox becomes another's - and
    /// since Pomodoro::new loads, a test wanting a fresh 25-minute focus
    /// would read whatever a concurrent test had just written.
    static STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take the lock and point the state file at an empty directory.
    ///
    /// The real file holds a running pomodoro that clocks.py shares, so a
    /// careless test zeroes an actual afternoon.
    fn sandbox(name: &str) -> std::sync::MutexGuard<'static, ()> {
        let held = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = format!("/tmp/toys-clocks-test-{}-{}", std::process::id(), name);
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("XDG_STATE_HOME", &dir);
        held
    }

    #[test]
    fn a_panel_left_running_over_midnight_starts_a_fresh_count() {
        // clocks.py's roll_day docstring names this as a bug it already
        // fixed: the count used to reset only on load, so a panel running
        // overnight kept adding to yesterday's total. The port had
        // reintroduced it - there was no roll at all.
        let _held = sandbox("roll");
        let mut p = Pomodoro::new(&serde_json::json!({}));
        p.done = 5;
        p.day = "2020-01-01".into();
        assert!(p.roll_day(), "a changed date should roll");
        assert_eq!(p.done, 0, "yesterday's tally carried into today");
        assert_eq!(p.day, today());
        // And the same day twice does nothing.
        p.done = 2;
        assert!(!p.roll_day());
        assert_eq!(p.done, 2, "the tally was zeroed on an unchanged day");
    }

    #[test]
    fn the_tally_survives_a_restart_and_the_file_is_the_pythons() {
        // The help text promises the pomodoro persists across restarts,
        // and the port wrote nothing at all. The file is clocks.py's, key
        // for key, so a session started under one is picked up by the
        // other rather than each keeping a private tally.
        let _held = sandbox("save");
        let mut p = Pomodoro::new(&serde_json::json!({}));
        p.done = 3;
        p.phase = Phase::Short;
        p.save();

        let text = std::fs::read_to_string(state_file()).expect("a state file");
        let d: serde_json::Value = serde_json::from_str(&text).expect("json");
        for key in [
            "day", "phase", "completed", "focus", "enabled", "running",
            "was_running", "hints", "left", "deadline",
        ] {
            assert!(d.get(key).is_some(), "clocks.py reads {} and we omit it", key);
        }
        assert_eq!(d["completed"], 3);
        assert_eq!(d["phase"], "short");

        let back = Pomodoro::new(&serde_json::json!({}));
        assert_eq!(back.done, 3, "the tally did not survive");
        assert_eq!(back.phase, Phase::Short);
    }

    #[test]
    fn a_tally_from_another_day_is_not_carried_forward() {
        // Preferences outlive the day; the count does not.
        let _held = sandbox("stale");
        let mut p = Pomodoro::new(&serde_json::json!({}));
        p.done = 9;
        p.day = "2020-01-01".into();
        p.save();
        let back = Pomodoro::new(&serde_json::json!({}));
        assert_eq!(back.done, 0, "yesterday's count was loaded as today's");
    }

    #[test]
    fn each_block_keeps_its_own_length() {
        // The config numbers are starting values, not fixed ones: ± moves
        // whichever block is running and leaves the other two alone, so a
        // 25/5/15 day can become 30/5/15 without touching the breaks.
        // adjust() saves, and the real state file is a running pomodoro
        // that clocks.py shares - send the writes somewhere disposable.
        let _held = sandbox("adjust");
        let mut pomo = Pomodoro {
            phase: Phase::Focus,
            running: false,
            shown: true,
            deadline: 0.0,
            left: 25.0 * 60.0,
            done: 0,
            focus: 25.0,
            short: 5.0,
            long: 15.0,
            before_long: 4,
            bell: false,
            rang_at: 0,
            day: today(),
            hints: true,
            notify: false,
        };

        pomo.phase = Phase::Focus;
        pomo.adjust(5.0);
        assert_eq!((pomo.focus, pomo.short, pomo.long), (30.0, 5.0, 15.0));

        pomo.phase = Phase::Short;
        pomo.adjust(-2.0);
        assert_eq!((pomo.focus, pomo.short, pomo.long), (30.0, 3.0, 15.0));

        pomo.phase = Phase::Long;
        pomo.adjust(1.0);
        assert_eq!((pomo.focus, pomo.short, pomo.long), (30.0, 3.0, 16.0));

        // And the hint reports the block in progress, not one named block.
        assert_eq!(pomo.minutes(), 16);
        pomo.phase = Phase::Short;
        assert_eq!(pomo.minutes(), 3);

        // A block cannot be argued below a minute or above three hours.
        pomo.phase = Phase::Short;
        for _ in 0..10 {
            pomo.adjust(-1.0);
        }
        assert_eq!(pomo.short, 1.0);
        assert_eq!((pomo.focus, pomo.long), (30.0, 16.0), "the others are untouched");
    }

    #[test]
    fn the_advance_key_says_what_it_will_do() {
        let _held = sandbox("the_advance_key_says_what_it_will_do");
        let mut pomo = Pomodoro::new(&serde_json::json!({}));
        // Three blocks done, so the fourth leads to the long break and the
        // hint has to say so rather than promising an ordinary one.
        assert_eq!(pomo.next_label(), "[b]reak start");
        pomo.done = 3;
        assert_eq!(pomo.next_label(), "[b]reak start (long)");
        pomo.phase = Phase::Short;
        assert_eq!(pomo.next_label(), "[b]reak stop");
    }

    #[test]
    fn the_focus_length_can_be_nudged_and_stays_sane() {
        // new() loads and adjust() saves, so this needs its own state.
        let _held = sandbox("nudge");
        let mut pomo = Pomodoro::new(&serde_json::json!({}));
        pomo.adjust(5.0);
        assert_eq!(pomo.focus, 30.0);
        assert_eq!(pomo.duration(), 30.0 * 60.0);
        // It cannot be driven to zero or beyond a working day.
        for _ in 0..100 {
            pomo.adjust(-10.0);
        }
        assert!(pomo.focus >= 1.0, "focus fell to {}", pomo.focus);
    }

    #[test]
    fn the_tally_resets_only_when_there_is_one() {
        let _held = sandbox("the_tally_resets_only_when_there_is_one");
        let mut pomo = Pomodoro::new(&serde_json::json!({}));
        pomo.done = 4;
        pomo.reset_count();
        assert_eq!(pomo.done, 0);
    }

    #[test]
    fn the_office_countdown_across_a_whole_week() {
        let office = Office::from_config(&serde_json::json!({}));
        // 2026-08-17 is a Monday, so +n days walks the week.
        let at = |day: u32, hour: u32| {
            Local.with_ymd_and_hms(2026, 8, day, hour, 0, 0).unwrap()
        };
        let read = |when| {
            let items = countdowns(when, &office);
            (items[1].label.clone(), items[1].left)
        };

        // Inside hours on a working day: counting to the close.
        let (label, left) = read(at(18, 11));      // Tuesday 11:00
        assert_eq!(label, "End of Office Hour");
        assert_eq!(left, 7 * 3600, "seven hours until six");

        // Just before opening: counting to it, an hour off.
        let (label, left) = read(at(18, 8));
        assert_eq!(label, "Start of Office Hour");
        assert_eq!(left, 3600);

        // After the close on a weekday: tomorrow morning.
        let (label, left) = read(at(18, 19));
        assert_eq!(label, "Start of Office Hour");
        assert_eq!(left, 14 * 3600);

        // The last minute of the working day is still inside it.
        let (label, _) = read(at(18, 17));
        assert_eq!(label, "End of Office Hour");

        // Friday evening, Saturday, Sunday: all pointing at Monday.
        for (day, hour) in [(21, 19), (22, 11), (23, 11)] {
            let (label, left) = read(at(day, hour));
            assert_eq!(label, "Start of Office Hour", "on day {}", day);
            let opens = office.next_open(&at(day, hour));
            assert_eq!(opens.weekday(), chrono::Weekday::Mon, "on day {}", day);
            assert!(left > 0);
        }
    }

    #[test]
    fn friday_evening_counts_to_monday_not_saturday() {
        let office = Office::from_config(&serde_json::json!({}));
        // Friday 21 August 2026 at 19:00 - after the close, and the next
        // working day is three days off.
        let friday = Local.with_ymd_and_hms(2026, 8, 21, 19, 0, 0).unwrap();
        assert!(!office.is_open(&friday));
        let opens = office.next_open(&friday);
        assert_eq!(opens.weekday(), chrono::Weekday::Mon);
        assert_eq!(opens.hour(), 9);
        // Which is nearly three days, not the fourteen hours a naive
        // "tomorrow at nine" would give.
        let items = countdowns(friday, &office);
        assert!(items[1].left > 48 * 3600, "got {}s", items[1].left);
    }

    #[test]
    fn a_saturday_is_not_a_working_day() {
        let office = Office::from_config(&serde_json::json!({}));
        let saturday = Local.with_ymd_and_hms(2026, 8, 22, 11, 0, 0).unwrap();
        assert!(!office.is_open(&saturday), "eleven on a Saturday is not office hours");
        assert_eq!(countdowns(saturday, &office)[1].label, "Start of Office Hour");
    }

    #[test]
    fn the_working_week_can_be_configured() {
        // A Sunday-to-Thursday week, as much of the Gulf works.
        let office = Office::from_config(&serde_json::json!({
            "work_days": ["sun", "mon", "tue", "wed", "thu"],
            "work_start_hour": 8,
            "work_end_hour": 16
        }));
        let sunday = Local.with_ymd_and_hms(2026, 8, 23, 9, 0, 0).unwrap();
        assert!(office.is_open(&sunday), "Sunday is a working day there");
        let friday = Local.with_ymd_and_hms(2026, 8, 21, 9, 0, 0).unwrap();
        assert!(!office.is_open(&friday), "Friday is not");
        // Numbers work as well as names, Monday being zero.
        let by_number = Office::from_config(&serde_json::json!({"work_days": [0, 1, 2]}));
        assert_eq!(by_number.days, vec![0, 1, 2]);
        // And nonsense falls back rather than emptying the week.
        let junk = Office::from_config(&serde_json::json!({"work_days": []}));
        assert_eq!(junk.days, vec![0, 1, 2, 3, 4]);
        // An hour that is not an hour used to unwrap on the first frame.
        let wild = Office::from_config(&serde_json::json!({
            "work_start_hour": 24,
            "work_end_hour": 99
        }));
        assert!(wild.start <= 23 && wild.end <= 23);
        let noon = Local.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap();
        let _ = wild.next_open(&noon);
        let _ = wild.prev_close(&noon);
        let _ = countdowns(noon, &wild);
    }

    #[test]
    fn each_countdown_gets_its_own_colour() {
        let now = Local.with_ymd_and_hms(2026, 8, 22, 14, 30, 0).unwrap();
        let items = countdowns(now, &Office::from_config(&serde_json::json!({})));
        let inks: std::collections::HashSet<String> =
            items.iter().map(|c| c.ink.clone()).collect();
        assert_eq!(inks.len(), 3, "three clocks, three colours");
    }

    #[test]
    fn the_world_clock_reads_more_than_light_and_dark() {
        let p = palette();
        let tokyo: Tz = "Asia/Tokyo".parse().unwrap();
        let at = |h: u32, d: u32| {
            tokyo.with_ymd_and_hms(2026, 8, d, h, 0, 0).unwrap()
        };
        // Saturday the 22nd, Monday the 24th.
        assert_eq!(phase_of(&at(3, 24), &p).1, "☾", "the small hours");
        assert_eq!(phase_of(&at(11, 24), &p), (p.work.clone(), "☀"));
        assert_eq!(phase_of(&at(19, 24), &p), (p.eve.clone(), "☾"));
        assert_eq!(phase_of(&at(8, 24), &p), (p.eve.clone(), "☀"));
        // A weekday's working hours and a weekend's are different answers.
        assert_eq!(phase_of(&at(11, 22), &p), (p.weekend.clone(), "☀"));
    }

    #[test]
    fn the_clock_is_two_colours_down_its_height() {
        // The Python paints the top three rows bright and the base dark;
        // one flat colour loses the weight of the digits entirely.
        let p = palette();
        assert_ne!(p.big_top, p.big_base);
        assert_eq!(p.big_top, tc::rgb(120, 255, 200));
        assert_eq!(p.big_base, tc::rgb(40, 150, 120));
    }

    #[test]
    fn digits_are_five_rows_of_blocks() {
        let rows = render_big("12:34");
        assert_eq!(rows.len(), 5);
        // Every row is the same width, or the clock leans.
        let widths: Vec<usize> = rows.iter().map(|r| r.chars().count()).collect();
        assert!(widths.windows(2).all(|w| w[0] == w[1]), "{:?}", widths);
        assert!(rows[0].contains('█'));
    }

    #[test]
    fn an_unknown_character_leaves_a_gap_rather_than_panicking() {
        let rows = render_big("1?2");
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn the_offset_is_written_as_people_write_it() {
        let utc = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let india: Tz = "Asia/Kolkata".parse().unwrap();
        // Half-hour offsets have to keep their minutes.
        assert_eq!(offset_str(&utc.with_timezone(&india)), "UTC+5:30");
        let tokyo: Tz = "Asia/Tokyo".parse().unwrap();
        assert_eq!(offset_str(&utc.with_timezone(&tokyo)), "UTC+9");
        let la: Tz = "America/Los_Angeles".parse().unwrap();
        assert_eq!(offset_str(&utc.with_timezone(&la)), "UTC-7");
    }

    #[test]
    fn daylight_saving_actually_moves_the_clock() {
        // The whole reason for carrying a timezone database: London is one
        // hour off UTC in August and level with it in January.
        let london: Tz = "Europe/London".parse().unwrap();
        let summer = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let winter = Utc.with_ymd_and_hms(2026, 1, 22, 12, 0, 0).unwrap();
        assert_eq!(offset_str(&summer.with_timezone(&london)), "UTC+1");
        assert_eq!(offset_str(&winter.with_timezone(&london)), "UTC+0");
    }

    #[test]
    fn the_longest_countdown_label_still_leaves_a_gap() {
        // "Start of Office Hour" is exactly twenty characters, and a
        // twenty-wide field ran it straight into the time beside it.
        let now = Local.with_ymd_and_hms(2026, 8, 22, 7, 0, 0).unwrap();
        let longest = countdowns(now, &Office::from_config(&serde_json::json!({})))
            .into_iter()
            .map(|c| c.label.chars().count())
            .max()
            .unwrap();
        assert!(longest < 21, "a label of {} needs a wider field", longest);
    }

    #[test]
    fn the_flash_blinks_and_then_stops() {
        let started = Some(100.0);
        // Lit at the start of each window, dark between them.
        assert!(flash_window(started, 2, 1.0, 100.0));
        assert!(flash_window(started, 2, 1.0, 100.3));
        assert!(!flash_window(started, 2, 1.0, 100.5));
        assert!(flash_window(started, 2, 1.0, 101.1));
        // And over for good once the last blink has passed.
        assert!(!flash_window(started, 2, 1.0, 102.0));
        assert!(!flash_window(None, 2, 1.0, 100.0));
    }

    #[test]
    fn a_flashed_frame_is_solid_and_exactly_the_width() {
        let rows = vec![
            format!("{}hello{}", tc::rgb(1, 2, 3), tc::RST),
            "plain".to_string(),
        ];
        let lit = flash_frame(&rows, 20, 3, &tc::bg(250, 250, 250), &tc::rgb(0, 0, 0));
        assert_eq!(lit.len(), 3);
        for line in &lit {
            // The original colours are gone; only the flash pair remains.
            assert!(!line.contains("38;2;1;2;3"));
            assert_eq!(strip_ansi(line).chars().count(), 20);
        }
    }

    #[test]
    fn the_flash_picks_ink_it_can_be_read_through() {
        // Near-white wants dark text; a dark colour wants light.
        assert_eq!(flash_ink((246, 248, 252)), tc::rgb(18, 20, 26));
        assert_eq!(flash_ink((20, 20, 30)), tc::rgb(255, 240, 240));
    }

    #[test]
    fn a_focus_block_leads_to_a_break_and_back() {
        let _held = sandbox("a_focus_block_leads_to_a_break_and_back");
        let cfg = serde_json::json!({});
        let mut pomo = Pomodoro::new(&cfg);
        let now = 1000.0;
        assert_eq!(pomo.phase, Phase::Focus);
        pomo.advance(now);
        assert_eq!(pomo.phase, Phase::Short);
        assert_eq!(pomo.done, 1);
        pomo.advance(now);
        assert_eq!(pomo.phase, Phase::Focus, "a break returns to focus");
    }

    #[test]
    fn every_fourth_break_is_a_long_one() {
        let _held = sandbox("every_fourth_break_is_a_long_one");
        let mut pomo = Pomodoro::new(&serde_json::json!({}));
        let now = 0.0;
        for _ in 0..3 {
            pomo.advance(now); // focus -> short
            pomo.advance(now); // short -> focus
        }
        pomo.advance(now); // the fourth focus block
        assert_eq!(pomo.done, 4);
        assert_eq!(pomo.phase, Phase::Long);
    }

    #[test]
    fn hiding_freezes_it_rather_than_letting_it_run_away() {
        let _held = sandbox("hiding_freezes_it_rather_than_letting_it_run_away");
        let mut pomo = Pomodoro::new(&serde_json::json!({}));
        pomo.shown = true;
        pomo.start_stop(0.0);
        assert!(pomo.running);
        // Two minutes in, hide it, and leave it hidden for an hour.
        pomo.toggle(120.0);
        let frozen = pomo.left;
        pomo.toggle(3720.0);
        // What is left is what was left, not an hour less.
        assert!((pomo.remaining(3720.0) - frozen).abs() < 0.001,
                "left {} against frozen {}", pomo.remaining(3720.0), frozen);
        assert!(pomo.running, "it was running when it went away");
    }

    #[test]
    fn overrunning_counts_up_rather_than_stopping() {
        let _held = sandbox("overrunning_counts_up_rather_than_stopping");
        let mut pomo = Pomodoro::new(&serde_json::json!({}));
        pomo.shown = true;
        pomo.start_stop(0.0);
        let past = pomo.duration() + 90.0;
        assert_eq!(pomo.remaining(past), 0.0);
        assert!((pomo.overtime(past) - 90.0).abs() < 0.001);
        // And it does not advance itself: a break that starts while you are
        // mid-sentence is a break you ignore, and then the count is a lie.
        assert_eq!(pomo.phase, Phase::Focus);
    }

    #[test]
    fn hms_counts_down_and_never_below_zero() {
        assert_eq!(hms(3661), "01:01:01");
        assert_eq!(hms(59), "00:00:59");
        assert_eq!(hms(-5), "00:00:00");
    }

    #[test]
    fn a_bar_is_exactly_its_width() {
        for frac in [0.0, 0.25, 0.5, 1.0, 1.5, -0.2] {
            assert_eq!(bar(frac, 20).chars().count(), 20, "frac {}", frac);
        }
        assert!(bar(0.0, 10).starts_with('░'));
        assert!(bar(1.0, 10).starts_with('█'));
    }

    #[test]
    fn the_countdowns_stay_inside_their_spans() {
        let now = Local.with_ymd_and_hms(2026, 8, 22, 14, 30, 0).unwrap();
        let items = countdowns(now, &Office::from_config(&serde_json::json!({})));
        assert_eq!(items.len(), 3);
        for item in &items {
            assert!(item.left > 0, "{} had {}s left", item.label, item.left);
            assert!((0.0..=1.0).contains(&item.frac), "{} at {}", item.label, item.frac);
        }
        // Half past two leaves half an hour of this hour.
        assert_eq!(items[0].left, 1800);
    }

    #[test]
    fn a_weekday_evening_counts_to_the_next_morning() {
        // Thursday 20 August 2026 at nine in the evening: twelve hours to
        // Friday's opening. This test previously used a Saturday and
        // expected the same answer, which is how the missing weekend
        // handling went unnoticed - the assertion encoded the bug.
        let evening = Local.with_ymd_and_hms(2026, 8, 20, 21, 0, 0).unwrap();
        let items = countdowns(evening, &Office::from_config(&serde_json::json!({})));
        assert_eq!(items[1].label, "Start of Office Hour");
        assert_eq!(items[1].left, 12 * 3600);
    }

    #[test]
    fn a_day_the_clocks_move_is_not_twenty_four_hours() {
        // Britain moves at one in the morning on the last Sunday of March
        // and back on the last Sunday of October - in 2026, the 29th and
        // the 25th. Held against a fixed 86,400 seconds, both days put the
        // time remaining and the bar an hour out.
        let london: Tz = "Europe/London".parse().unwrap();

        let spring = london.with_ymd_and_hms(2026, 3, 29, 0, 30, 0).unwrap();
        let (into, len) = day_bounds(&spring);
        assert_eq!(len, 23 * 3600, "the day the clocks go forward is 23 hours");
        // Half past midnight, with the jump still ahead: twenty-two and a
        // half hours of the day left, not the twenty-three and a half a
        // fixed day gives.
        assert_eq!(len - into, 22 * 3600 + 1800);

        let autumn = london.with_ymd_and_hms(2026, 10, 25, 0, 30, 0).unwrap();
        let (into, len) = day_bounds(&autumn);
        assert_eq!(len, 25 * 3600, "the day they go back is 25 hours");
        assert_eq!(len - into, 24 * 3600 + 1800);

        // And a day nothing happens on is still a plain day.
        let plain = london.with_ymd_and_hms(2026, 8, 22, 6, 0, 0).unwrap();
        assert_eq!(day_bounds(&plain), (6 * 3600, 86_400));
    }

    #[test]
    fn a_finished_toast_is_reaped_rather_than_left_a_zombie() {
        // Every notification spawns a process, and one nobody waits on is a
        // zombie for the life of the widget - an ignored pomodoro sends one
        // a minute, so a panel left alone all day would leave hundreds.
        let mut sent = Vec::new();
        for _ in 0..3 {
            match std::process::Command::new("true")
                .stdout(std::process::Stdio::null())
                .spawn()
            {
                Ok(child) => sent.push(child),
                Err(_) => return, // nothing to spawn on this machine
            }
        }
        // They exit in milliseconds, but "immediately" is not a promise the
        // scheduler makes: allow a second before believing otherwise.
        for _ in 0..100 {
            reap(&mut sent);
            if sent.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(sent.is_empty(), "{} toasts left unreaped", sent.len());
    }

    #[test]
    fn the_timer_is_off_until_asked_for_and_never_starts_itself() {
        let _held = sandbox("enabled");
        // With nothing configured there is nothing on screen, which is what
        // the help text and the doc both promise. It was hardcoded shown.
        let off = Pomodoro::new(&serde_json::json!({}));
        assert!(!off.shown, "an optional panel was on screen unasked");
        assert!(!off.running);

        // pomodoro_enabled puts it there - paused. Setting `running` from
        // it left the deadline at zero, and with no deadline the countdown
        // reads a `left` that nothing decrements: a timer frozen at the
        // full block with nothing on the row saying it was not moving.
        let on = Pomodoro::new(&serde_json::json!({"pomodoro_enabled": true}));
        assert!(on.shown);
        assert!(!on.running, "a block nobody sat down for was being counted");
        let now = 1_000.0;
        assert_eq!(on.remaining(now), on.duration());
        assert_eq!(on.remaining(now + 600.0), on.duration(), "a paused timer moved");
    }

    #[test]
    fn the_hint_preference_is_not_the_pomodoros_visibility() {
        // One field held both: "hints" in the state file was written from
        // whether the panel was shown, so hiding the panel rewrote the [?]
        // preference - which was itself never saved, and came back every
        // restart.
        let _held = sandbox("hints");
        let shown = serde_json::json!({"pomodoro_enabled": true});

        let mut p = Pomodoro::new(&shown);
        assert!(p.hints, "show_hints defaults to on");
        p.hints = false; // what [?] does
        p.save();
        let back = Pomodoro::new(&shown);
        assert!(!back.hints, "the hint preference did not survive a restart");
        assert!(back.shown, "hiding the hints hid the panel with them");

        // And the other way about: hiding the panel leaves the hints alone.
        let mut p = Pomodoro::new(&shown);
        p.hints = true;
        p.save();
        p.toggle(0.0); // what [p] does
        assert!(!p.shown);
        let back = Pomodoro::new(&shown);
        assert!(!back.shown, "the panel came back after being hidden");
        assert!(back.hints, "hiding the panel rewrote the hint preference");
    }

    #[test]
    fn a_nudge_moves_the_finish_line_rather_than_starting_the_block_over() {
        // Twenty minutes into a twenty-five minute block, one more minute
        // means six left. It used to mean twenty-six: the new length was
        // assigned whole and the deadline rebuilt from now, so every nudge
        // threw away however far in you were.
        let _held = sandbox("nudge-keeps-progress");
        let mut pomo = Pomodoro::new(&serde_json::json!({}));
        pomo.shown = true;
        pomo.start_stop(0.0);
        let twenty = 20.0 * 60.0;
        pomo.adjust(1.0);
        assert_eq!(pomo.remaining(twenty), 6.0 * 60.0);

        // Paused, where `left` is the number that counts.
        pomo.start_stop(twenty);
        assert_eq!(pomo.left, 6.0 * 60.0);
        pomo.adjust(-2.0);
        assert_eq!(pomo.left, 4.0 * 60.0);

        // Cut shorter than the time already spent and the block is over,
        // which is true - the counter climbs rather than claiming minutes
        // that have gone.
        pomo.adjust(-20.0);
        assert!(pomo.signed(twenty) < 0.0, "{} left", pomo.left);
    }
}
