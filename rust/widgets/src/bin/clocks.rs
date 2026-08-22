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
}

/// The three fixed countdowns: the hour, the working day, and midnight.
fn countdowns(now: chrono::DateTime<Local>, work_start: u32, work_end: u32) -> Vec<Countdown> {
    let mut out = Vec::new();

    let into_hour = now.minute() as i64 * 60 + now.second() as i64;
    out.push(Countdown {
        label: "Next Hour".into(),
        left: 3600 - into_hour,
        frac: into_hour as f64 / 3600.0,
    });

    // Office hours run to work_end today; past it, to work_start tomorrow.
    let today = now.date_naive();
    let start = today.and_time(NaiveTime::from_hms_opt(work_start, 0, 0).unwrap());
    let end = today.and_time(NaiveTime::from_hms_opt(work_end, 0, 0).unwrap());
    let naive = now.naive_local();
    let (label, target, from) = if naive < start {
        ("Start of Office Hour", start, start - chrono::Duration::hours(12))
    } else if naive < end {
        ("End of Office Hour", end, start)
    } else {
        let tomorrow = today + chrono::Duration::days(1);
        (
            "Start of Office Hour",
            tomorrow.and_time(NaiveTime::from_hms_opt(work_start, 0, 0).unwrap()),
            end,
        )
    };
    let span = (target - from).num_seconds().max(1);
    let left = (target - naive).num_seconds();
    out.push(Countdown {
        label: label.into(),
        left,
        frac: 1.0 - (left as f64 / span as f64),
    });

    let into_day = now.num_seconds_from_midnight() as i64;
    out.push(Countdown {
        label: "End of Day".into(),
        left: 86400 - into_day,
        frac: into_day as f64 / 86400.0,
    });
    out
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
            // On screen from the start, paused. Hidden and paused are
            // different things, and the Python shows it from the first
            // frame with "paused" against it.
            shown: true,
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
        };
        it.left = it.duration();
        it
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
    }

    fn start_stop(&mut self, now: f64) {
        if self.running {
            self.left = self.signed(now);
            self.running = false;
        } else {
            self.running = true;
            self.deadline = now + self.left;
        }
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
    }

    fn restart(&mut self, now: f64) {
        self.left = self.duration();
        self.deadline = now + self.left;
        self.rang_at = -1;
    }

    /// One tick: ring on elapse, and once a minute while overrunning.
    ///
    /// It does not advance on its own. A break that starts itself while you
    /// are mid-sentence is a break you ignore, and then the count is a lie.
    fn tick(&mut self, now: f64) {
        if !self.running || !self.shown {
            return;
        }
        let over = self.overtime(now);
        if over <= 0.0 {
            return;
        }
        let minute = (over / 60.0) as i64;
        if minute != self.rang_at {
            self.rang_at = minute;
            if self.bell {
                tc::out("\x07");
                tc::flush();
            }
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
    let work_start = tc::cfg_usize(&cfg, "work_start_hour", 9) as u32;
    let work_end = tc::cfg_usize(&cfg, "work_end_hour", 18) as u32;
    let cities = load_cities(&cfg);

    let p = palette();
    let mut pomo = Pomodoro::new(&cfg);
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
                "p" | "P" => pomo.toggle(seconds()),
                " " => pomo.start_stop(seconds()),
                "n" | "N" => pomo.advance(seconds()),
                "r" | "R" => pomo.restart(seconds()),
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let now = Local::now();
        let mut rows = vec![tc::title("clocks", w, &p.head)];
        rows.push(tc::seg(&[(p.lbl.as_str(), " ── SERVER TIME ── ".into())], w - 1));

        for line in render_big(&now.format("%H:%M:%S").to_string()) {
            rows.push(tc::seg(&[(p.big.as_str(), format!("  {}", line))], w - 1));
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

        rows.push(tc::seg(&[(p.lbl.as_str(), " ── COUNTDOWN ── ".into())], w - 1));
        let bar_w = w.saturating_sub(3).min(90);

        // The pomodoro leads the section, as it does in the Python.
        let stamp = seconds();
        pomo.tick(stamp);
        if pomo.shown {
            let over = pomo.overtime(stamp);
            let left = pomo.remaining(stamp);
            let frac = 1.0 - (left / pomo.duration().max(1.0));
            rows.push(tc::seg(
                &[
                    (p.txt.as_str(), " Pomodoro · ".into()),
                    (
                        if pomo.phase == Phase::Focus { &p.focus } else { &p.rest },
                        format!("{:<12}", pomo.phase.label()),
                    ),
                    (
                        if over > 0.0 { &p.focus } else { &p.accent },
                        if over > 0.0 {
                            format!("+{}", hms(over as i64))
                        } else {
                            hms(left as i64)
                        },
                    ),
                    (
                        p.dim.as_str(),
                        format!(
                            "  {}   {} done",
                            if pomo.running { "running" } else { "paused" },
                            pomo.done
                        ),
                    ),
                ],
                w - 1,
            ));
            rows.push(tc::seg(
                &[(
                    if pomo.phase == Phase::Focus { &p.focus } else { &p.rest },
                    format!(" {}", bar(frac, bar_w)),
                )],
                w - 1,
            ));
        }
        for item in countdowns(now, work_start, work_end) {
            rows.push(tc::seg(
                &[
                    (p.txt.as_str(), format!(" {:<21}", item.label)),
                    (p.accent.as_str(), hms(item.left)),
                ],
                w - 1,
            ));
            rows.push(tc::seg(
                &[(p.bar.as_str(), format!(" {}", bar(item.frac, bar_w)))],
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
                let awake = (7..19).contains(&there.hour());
                let day_shift = there.date_naive().signed_duration_since(now.date_naive()).num_days();
                rows.push(tc::seg(
                    &[
                        (
                            if awake { &p.sun } else { &p.moon },
                            format!(" {} ", if awake { "☀" } else { "☾" }),
                        ),
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

        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(
                p.dim.as_str(),
                format!("[p]{}", if pomo.shown { "off" } else { "omodoro" }),
            )],
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " cities".into())],
            vec![(p.dim.as_str(), "[q]uit".into())],
        ];
        let foot: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
        while rows.len() < h.saturating_sub(foot.len()) {
            rows.push(String::new());
        }
        rows.extend(foot);
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The configured cities, or the four the Python ships with.
///
/// A zone the database does not know is dropped rather than defaulted to
/// UTC: a clock quietly showing the wrong city is worse than one absent.
fn load_cities(cfg: &serde_json::Value) -> Vec<City> {
    let mut out = Vec::new();
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
    if out.is_empty() {
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
    dim: String,
    txt: String,
    lbl: String,
    accent: String,
    head: String,
    big: String,
    bar: String,
    sun: String,
    moon: String,
}

fn palette() -> Palette {
    Palette {
        focus: tc::rgb(255, 130, 120),
        rest: tc::rgb(120, 220, 170),
        dim: tc::rgb(127, 147, 172),
        txt: tc::rgb(225, 235, 245),
        lbl: tc::rgb(130, 165, 200),
        accent: tc::rgb(150, 210, 255),
        head: tc::rgb(0, 255, 170),
        big: tc::rgb(220, 255, 240),
        bar: tc::rgb(90, 200, 255),
        sun: tc::rgb(255, 210, 120),
        moon: tc::rgb(150, 170, 210),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let longest = countdowns(now, 9, 18)
            .into_iter()
            .map(|c| c.label.chars().count())
            .max()
            .unwrap();
        assert!(longest < 21, "a label of {} needs a wider field", longest);
    }

    #[test]
    fn a_focus_block_leads_to_a_break_and_back() {
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
        let items = countdowns(now, 9, 18);
        assert_eq!(items.len(), 3);
        for item in &items {
            assert!(item.left > 0, "{} had {}s left", item.label, item.left);
            assert!((0.0..=1.0).contains(&item.frac), "{} at {}", item.label, item.frac);
        }
        // Half past two leaves half an hour of this hour.
        assert_eq!(items[0].left, 1800);
    }

    #[test]
    fn after_hours_counts_to_tomorrow_morning() {
        let evening = Local.with_ymd_and_hms(2026, 8, 22, 21, 0, 0).unwrap();
        let items = countdowns(evening, 9, 18);
        assert_eq!(items[1].label, "Start of Office Hour");
        // Twelve hours to nine the next morning.
        assert_eq!(items[1].left, 12 * 3600);
    }
}
