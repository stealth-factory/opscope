// opscope - small dependency-free terminal widgets
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

//! How much the coding agents on this machine have been used.
//!
//! A port of usage.py. One tab per agent, because they do not agree on what
//! usage even means: one counts tokens, another counts lines it wrote, and
//! several publish nothing at all outside their own session. An agent that
//! exposes nothing says so rather than showing a plausible zero.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Datelike, Duration as Days, NaiveDate, TimeZone, Utc};
use opscope_core as tc;

/// The priced kinds, in the order every rate card lists them.
const RATE_KINDS: &[&str] = &[
    "input",
    "output",
    "cache_read",
    "cache_write",
    "cache_write_1h",
];

const LIST_RATES_AS_OF: &str = "Aug 2026";
/// Models known to have no published price: prefix matching would otherwise
/// hand gpt-5.3-codex-spark its family's rate, and Spark is explicitly not
/// on the API. Naming them makes them report as unpriced rather than as a
/// number nobody published.
const NO_PUBLISHED_PRICE: &[&str] = &["gpt-5.3-codex-spark", "codex-auto-review"];

/// Published list prices, US$ per million tokens, from the vendors' own
/// pricing pages on the date above. They are shipped because they are
/// published facts with a citable source, not a guess - but they go stale
/// silently, so the date is carried onto the screen with them and config
/// overrides any line.
///
/// Cache writes come in two durations at different prices, and the
/// transcripts record which was taken per iteration, so both are carried
/// and neither is assumed. OpenAI does not charge for cache writes, so
/// those entries are absent rather than zero.
const LIST_RATES: &[(&str, &[(&str, f64)])] = &[
    ("gpt-5.6-sol", &[("input", 5.0), ("output", 30.0), ("cache_read", 0.50)]),
    ("gpt-5.6-terra", &[("input", 2.0), ("output", 12.0), ("cache_read", 0.20)]),
    ("gpt-5.6-luna", &[("input", 0.20), ("output", 1.20), ("cache_read", 0.02)]),
    ("gpt-5.5-pro", &[("input", 30.0), ("output", 180.0)]),
    ("gpt-5.5", &[("input", 5.0), ("output", 30.0), ("cache_read", 0.50)]),
    ("gpt-5.4-mini", &[("input", 0.75), ("output", 4.50), ("cache_read", 0.075)]),
    ("gpt-5.4-nano", &[("input", 0.20), ("output", 1.25), ("cache_read", 0.02)]),
    ("gpt-5.4-pro", &[("input", 30.0), ("output", 180.0)]),
    ("gpt-5.4", &[("input", 2.50), ("output", 15.0), ("cache_read", 0.25)]),
    ("gpt-5.3-codex", &[("input", 1.75), ("output", 14.0), ("cache_read", 0.175)]),
    ("gpt-5.2-pro", &[("input", 21.0), ("output", 168.0)]),
    ("gpt-5.2", &[("input", 1.75), ("output", 14.0), ("cache_read", 0.175)]),
    ("gpt-5.1", &[("input", 1.25), ("output", 10.0), ("cache_read", 0.125)]),
    ("gpt-5-mini", &[("input", 0.25), ("output", 2.0), ("cache_read", 0.025)]),
    ("gpt-5-nano", &[("input", 0.05), ("output", 0.40), ("cache_read", 0.005)]),
    ("gpt-5-pro", &[("input", 15.0), ("output", 120.0)]),
    ("gpt-5", &[("input", 1.25), ("output", 10.0), ("cache_read", 0.125)]),
    (
        "claude-fable-5",
        &[("input", 10.0), ("output", 50.0), ("cache_write", 12.50), ("cache_read", 1.0), ("cache_write_1h", 20.0)],
    ),
    (
        "claude-mythos-5",
        &[("input", 10.0), ("output", 50.0), ("cache_write", 12.50), ("cache_read", 1.0), ("cache_write_1h", 20.0)],
    ),
    (
        "claude-opus-5",
        &[("input", 5.0), ("output", 25.0), ("cache_write", 6.25), ("cache_read", 0.50), ("cache_write_1h", 10.0)],
    ),
    (
        "claude-opus-4-8",
        &[("input", 5.0), ("output", 25.0), ("cache_write", 6.25), ("cache_read", 0.50), ("cache_write_1h", 10.0)],
    ),
    (
        "claude-opus-4-7",
        &[("input", 5.0), ("output", 25.0), ("cache_write", 6.25), ("cache_read", 0.50), ("cache_write_1h", 10.0)],
    ),
    (
        "claude-opus-4-6",
        &[("input", 5.0), ("output", 25.0), ("cache_write", 6.25), ("cache_read", 0.50), ("cache_write_1h", 10.0)],
    ),
    (
        "claude-opus-4-5",
        &[("input", 5.0), ("output", 25.0), ("cache_write", 6.25), ("cache_read", 0.50), ("cache_write_1h", 10.0)],
    ),
    (
        "claude-opus-4-1",
        &[("input", 15.0), ("output", 75.0), ("cache_write", 18.75), ("cache_read", 1.50), ("cache_write_1h", 30.0)],
    ),
    (
        "claude-sonnet-5",
        &[("input", 2.0), ("output", 10.0), ("cache_write", 2.50), ("cache_read", 0.20), ("cache_write_1h", 4.0)],
    ),
    (
        "claude-sonnet-4-6",
        &[("input", 3.0), ("output", 15.0), ("cache_write", 3.75), ("cache_read", 0.30), ("cache_write_1h", 6.0)],
    ),
    (
        "claude-sonnet-4-5",
        &[("input", 3.0), ("output", 15.0), ("cache_write", 3.75), ("cache_read", 0.30), ("cache_write_1h", 6.0)],
    ),
    (
        "claude-haiku-4-5",
        &[("input", 1.0), ("output", 5.0), ("cache_write", 1.25), ("cache_read", 0.10), ("cache_write_1h", 2.0)],
    ),
    (
        "claude-haiku-3-5",
        &[("input", 0.80), ("output", 4.0), ("cache_write", 1.0), ("cache_read", 0.08), ("cache_write_1h", 1.6)],
    ),
];

/// One hue, four steps, the way /stats and the contribution calendar do it.
/// heat() runs green to amber to red, which reads as a change of *kind*
/// rather than of amount - wrong for "more of the same thing".
const HEAT_STEPS: [(u8, u8, u8); 4] = [(74, 52, 46), (140, 78, 58), (196, 100, 66), (240, 132, 84)];
// Carried with Claude's because they are the Python's own four-step ramps
// and the vendors that use them are the next thing to be ported. Kept here
// rather than reinvented later, where the numbers would drift.
#[allow(dead_code)]
const CODEX_STEPS: [(u8, u8, u8); 4] =
    [(66, 72, 82), (122, 130, 144), (182, 190, 202), (240, 244, 250)];
#[allow(dead_code)]
const GROK_STEPS: [(u8, u8, u8); 4] = [(44, 62, 88), (62, 104, 156), (86, 150, 210), (120, 196, 250)];
#[allow(dead_code)]
const CURSOR_STEPS: [(u8, u8, u8); 4] =
    [(48, 74, 66), (72, 124, 104), (100, 172, 142), (140, 220, 184)];

/// One hue per provider. Each is the colour that agent's own tab already
/// uses, so the same agent looks the same wherever you meet it. Copilot and
/// Antigravity have no calendar to borrow from and get their own, chosen to
/// sit clear of the amber and red this widget reserves for trouble.
fn agent_hue(name: &str) -> Option<(u8, u8, u8)> {
    Some(match name {
        "claude" => (240, 132, 84),
        "codex" => (206, 214, 228),
        "cursor" => (126, 208, 176),
        "grok" => (120, 196, 250),
        "copilot" => (186, 166, 255),
        "antigravity" => (232, 158, 200),
        _ => return None,
    })
}

#[allow(dead_code)]
fn agent_steps(name: &str) -> [(u8, u8, u8); 4] {
    match name {
        "codex" => CODEX_STEPS,
        "grok" => GROK_STEPS,
        "cursor" => CURSOR_STEPS,
        _ => HEAT_STEPS,
    }
}

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SUMMARY_TAB: &str = "+";
const ORDER: &[&str] = &["claude", "codex", "cursor", "grok", "copilot", "antigravity"];
const MONTHS: &[&str] = &[
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Below this much of a window, a pace figure is noise.
const PACE_FLOOR: f64 = 3.0;
/// The five-hour session and the seven-day total, which the response names
/// in its own top-level keys rather than in limits[].
const CLAUDE_WINDOW_SECS: &[(&str, f64)] = &[("session", 5.0 * 3600.0), ("weekly", 7.0 * 86400.0)];

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn under_home(rest: &str) -> String {
    format!("{}/{}", home(), rest)
}

fn text(value: &serde_json::Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_string()
}

fn num(value: &serde_json::Value, key: &str) -> f64 {
    value[key].as_f64().unwrap_or(0.0)
}

/// A duration in milliseconds as days, hours and minutes.
fn span_ms(ms: f64) -> String {
    let s = (ms / 1000.0) as i64;
    let (d, rest) = (s / 86400, s % 86400);
    let (h, rest) = (rest / 3600, rest % 3600);
    let m = rest / 60;
    if d > 0 {
        format!("{}d {}h {}m", d, h, m)
    } else if h > 0 {
        format!("{}h {}m", h, m)
    } else if m > 0 {
        // A couple of seconds of generation is not "0m".
        format!("{}m", m)
    } else {
        format!("{:.1}s", ms / 1000.0)
    }
}

/// Token counts run to billions; nobody reads eleven digits.
fn big_num(n: f64) -> String {
    for (unit, size) in [("B", 1e9), ("M", 1e6), ("k", 1e3)] {
        if n.abs() >= size {
            return format!("{:.1}{}", n / size, unit);
        }
    }
    format!("{}", n as i64)
}

fn ago(when: f64) -> String {
    if when <= 0.0 {
        return "never".into();
    }
    let s = now() - when;
    if s < 60.0 {
        format!("{}s", s as i64)
    } else if s < 3600.0 {
        format!("{}m", (s / 60.0) as i64)
    } else if s < 86400.0 {
        format!("{}h", (s / 3600.0) as i64)
    } else if s < 365.0 * 86400.0 {
        format!("{}d", (s / 86400.0) as i64)
    } else {
        // A subscription can be years old, and "890d" is not a span anyone
        // reads.
        format!("{:.1}y", s / (365.0 * 86400.0))
    }
}

/// ISO-8601 to epoch seconds.
///
/// These APIs mix a trailing Z with +00:00 in the same response, and Go
/// writes nanoseconds where the parsers take three or six digits.
fn iso_epoch(s: &str) -> Option<f64> {
    if s.is_empty() {
        return None;
    }
    // Z and +00:00 are the same zone and these APIs mix them inside one
    // response. Spelling it one way means every zoned stamp is parsed by
    // the same branch - the two branches did not agree about whether a
    // fraction of a second survives.
    let s: String = match s.strip_suffix('Z') {
        Some(head) => format!("{}+00:00", head),
        None => s.to_string(),
    };
    let s = s.as_str();
    // Trim any sub-second field to microseconds, whatever it arrived as.
    let cleaned = match s.find('.') {
        Some(dot) => {
            let tail = &s[dot + 1..];
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            let rest = &tail[digits.len()..];
            format!("{}.{}{}", &s[..dot], &digits[..digits.len().min(6)], rest)
        }
        None => s.to_string(),
    };
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M:%S%:z",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(at) = chrono::DateTime::parse_from_str(&cleaned, fmt) {
            return Some(at.timestamp() as f64 + at.timestamp_subsec_micros() as f64 / 1e6);
        }
        if let Ok(at) = chrono::NaiveDateTime::parse_from_str(&cleaned, fmt) {
            // Unzoned, so read as UTC. usage.py reads these as local, but
            // its own docstring says the callers all pass zoned strings -
            // and every caller here does.
            let at = Utc.from_utc_datetime(&at);
            return Some(at.timestamp() as f64 + at.timestamp_subsec_micros() as f64 / 1e6);
        }
    }
    None
}

/// A date, kept in the zone it arrived in.
///
/// assigned_date carries the account's own offset. Converting it to this
/// machine's zone can move it a day - 3 Jun at 12:10 -07:00 is 4 Jun in UTC -
/// and then the pane disagrees with what the vendor shows for the same seat.
fn iso_day(s: &str) -> String {
    let s = s.trim_end_matches('Z');
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f%:z", "%Y-%m-%dT%H:%M:%S%:z"] {
        if let Ok(at) = chrono::DateTime::parse_from_str(s, fmt) {
            return format!("{} {} {}", at.day(), MONTHS[at.month0() as usize], at.year());
        }
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d"] {
        if let Ok(at) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return format!("{} {} {}", at.day(), MONTHS[at.month0() as usize], at.year());
        }
        if let Ok(at) = NaiveDate::parse_from_str(s, fmt) {
            return format!("{} {} {}", at.day(), MONTHS[at.month0() as usize], at.year());
        }
    }
    String::new()
}

fn left_span(secs: f64) -> String {
    let s = secs as i64;
    let (d, rest) = (s / 86400, s % 86400);
    let (h, rest) = (rest / 3600, rest % 3600);
    if d > 0 {
        format!("{}d {}h", d, h)
    } else if h > 0 {
        format!("{}h {}m", h, rest / 60)
    } else {
        format!("{}m", rest / 60)
    }
}

/// How far ahead of the clock a quota is, as a signed percentage.
///
/// The share of the window already gone minus the share of the allowance
/// already spent. Positive is headroom; negative means this runs out before
/// the window does. Below PACE_FLOOR of a window it is not shown at all,
/// because ten minutes into a week every number looks like a catastrophe or
/// a triumph.
fn lead(pct_used: f64, window_secs: Option<f64>, reset_ts: Option<f64>) -> Option<f64> {
    let (window, reset) = (window_secs?, reset_ts?);
    if window <= 0.0 {
        return None;
    }
    let gone = window - (reset - now());
    if gone <= 0.0 || gone > window {
        return None;
    }
    let elapsed = 100.0 * gone / window;
    if elapsed < PACE_FLOOR {
        return None;
    }
    Some(elapsed - pct_used)
}

/// A percentage with enough precision to prove it is not a placeholder.
///
/// Every Antigravity lane rounded to "0%" - which is what an empty section
/// looks like - while one was genuinely 0.4% spent and another 0.03%. A real
/// small number and no number at all have to be tellable apart.
fn pct_text(pct: f64) -> String {
    if pct <= 0.0 {
        "    0%".into()
    } else if pct < 1.0 {
        format!("{:5.2}%", pct)
    } else if pct < 10.0 {
        format!("{:5.1}%", pct)
    } else {
        format!("{:5.0}%", pct)
    }
}

/// How much of a window has gone, from its length and its reset.
fn elapsed_of(secs: Option<f64>, reset: Option<f64>) -> Option<f64> {
    let (secs, reset) = (secs?, reset?);
    if secs <= 0.0 {
        return None;
    }
    let left = reset - now();
    if left <= 0.0 || left > secs {
        return None;
    }
    Some((secs - left) / secs)
}

/// The dark end of every agent ramp. The two stops above it are measured,
/// not picked: 0.51 keeps the dimmest filled cell at 3:1 against the
/// background for the darkest agent hue, and 0.34 leaves the empty track at
/// least as visible as the flat grid it replaces.
const BAR_FLOOR: (u8, u8, u8) = (30, 38, 52);

fn blend(hue: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
    let step = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
    (
        step(BAR_FLOOR.0, hue.0),
        step(BAR_FLOOR.1, hue.1),
        step(BAR_FLOOR.2, hue.2),
    )
}

/// A tint of an agent's colour, t running dark to full.
///
/// Not `shade` - that name is taken by the calendars' four-step ramp, and
/// two functions of the same name meant the heatmaps drew with this one.
fn tint(hue: (u8, u8, u8), t: f64) -> String {
    let (r, g, b) = blend(hue, t);
    tc::rgb(r, g, b)
}

/// Which of the four steps a day falls in.
fn shade(frac: f64, steps: [(u8, u8, u8); 4]) -> String {
    let at = ((frac * 3.999) as usize).min(3);
    let (r, g, b) = steps[at];
    tc::rgb(r, g, b)
}

struct Palette {
    ok: String,
    warn: String,
    bad: String,
    dim: String,
    grid: String,
    txt: String,
    lbl: String,
    accent: String,
    agent: String,
    empty_cell: String,
    /// The notch is white on its own dark cell rather than a bare
    /// foreground colour: plain white manages 1.2:1 against a full bar, so
    /// it disappears exactly where it matters.
    pace_mark: String,
    /// Default background again, and only that.
    nobg: String,
}

fn palette() -> Palette {
    Palette {
        ok: tc::rgb(90, 240, 160),
        warn: tc::rgb(255, 200, 90),
        bad: tc::rgb(255, 100, 110),
        dim: tc::rgb(127, 147, 172),
        grid: tc::rgb(60, 78, 98),
        txt: tc::rgb(225, 235, 245),
        lbl: tc::rgb(130, 165, 200),
        accent: tc::rgb(150, 210, 255),
        agent: tc::rgb(180, 160, 255),
        empty_cell: tc::rgb(58, 66, 80),
        pace_mark: format!("{}{}", tc::bg(10, 12, 18), tc::rgb(238, 244, 252)),
        nobg: tc::NOBG.to_string(),
    }
}

/// What colour a quota's percentage is written in.
///
/// The agent's own colour, so the number matches the bar it sits beside.
/// Red is the one exception, at 90% spent, because nearly empty is trouble
/// whatever the pace. Behind-the-clock deliberately does not colour this:
/// the pace cell beside it is already amber for exactly that, and a number
/// and its own explanation both turning yellow reads as two problems.
fn pct_colour(pct: f64, hue: Option<(u8, u8, u8)>, p: &Palette) -> String {
    if pct >= 90.0 {
        return p.bad.clone();
    }
    match hue {
        Some((r, g, b)) => tc::rgb(r, g, b),
        None => tc::heat(pct / 100.0),
    }
}

/// The signed cushion, coloured by whether it is one.
fn pace_cell(value: Option<f64>, p: &Palette) -> (String, String) {
    pace_cell_of(value, false, p)
}

/// The pace figure, with `~` when it rests on a cached percentage rather
/// than one fetched just now.
///
/// The tilde is doing real work here and it is worth being clear what it
/// covers. Where the cached window is still open the figure is a few
/// minutes stale and the mark is honest. Where that window has closed, the
/// percentage is the *previous* window's final one and the counter has since
/// reset - so the figure is carried forward rather than extrapolated, and
/// the `~` is the only thing saying so. The star beside the bar says the
/// reading is cached; the agent's own tab says how old.
fn pace_cell_of(value: Option<f64>, guessed: bool, p: &Palette) -> (String, String) {
    match value {
        None => (p.dim.clone(), String::new()),
        Some(v) => (
            if guessed {
                p.dim.clone()
            } else if v >= 0.0 {
                p.ok.clone()
            } else {
                p.warn.clone()
            },
            format!("  {}{:+.0}%", if guessed { "~" } else { "" }, v),
        ),
    }
}

/// A quota bar with a mark where an even burn would have reached by now.
///
/// The percentage alone cannot separate a lane 71% spent with three weeks
/// left from one 71% spent with three days left, and colour alone cannot
/// either - both are the same red. The mark is the window's own progress,
/// so a fill short of it is spending slower than the clock.
fn paced_bar(
    used: f64,
    elapsed: Option<f64>,
    room: usize,
    hue: Option<(u8, u8, u8)>,
    p: &Palette,
) -> Vec<(String, String)> {
    let bar = tc::meter(used, room);
    let filled = bar.chars().filter(|c| *c == '█').count();
    let at = elapsed.map(|e| ((e * room as f64).round() as usize).min(room.saturating_sub(1)));
    let mut parts: Vec<(String, String)> = Vec::new();
    for (i, ch) in bar.chars().enumerate() {
        let (colour, glyph) = if Some(i) == at {
            // One colour for the mark on every bar. It is a reference line -
            // where an even burn would have reached - and a line that
            // changes colour looks like it has a state of its own, when the
            // state being reported is the fill's position relative to it.
            (p.pace_mark.clone(), '┃')
        } else if i < filled {
            // Filled cells run dark to full across the fill, so the bar is
            // recognisably its agent's colour and still reads as a quantity
            // without counting cells.
            let t = 0.51 + 0.49 * (i as f64 / filled.saturating_sub(1).max(1) as f64);
            (
                format!(
                    "{}{}",
                    p.nobg,
                    match hue {
                        Some(hue) => tint(hue, t),
                        None => tc::heat(used),
                    }
                ),
                ch,
            )
        } else {
            (
                format!(
                    "{}{}",
                    p.nobg,
                    match hue {
                        Some(hue) => tint(hue, 0.34),
                        None => p.grid.clone(),
                    }
                ),
                ch,
            )
        };
        match parts.last_mut() {
            Some((had, run)) if *had == colour => run.push(glyph),
            _ => parts.push((colour, glyph.to_string())),
        }
    }
    // The mark is the one thing here that sets a background, so the run ends
    // by putting it back. Without this the dark cell bled through everything
    // drawn after it on the row.
    parts.push((p.nobg.clone(), String::new()));
    parts
}

/// Plain text flowed to a width. Clipping a sentence loses its end.
fn wrap_text(t: &str, budget: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in t.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > budget {
            lines.push(std::mem::take(&mut line));
            line.push_str(word);
        } else if line.is_empty() {
            line.push_str(word);
        } else {
            line.push(' ');
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// A labelled value flowed onto as many lines as it needs.
///
/// Only text can be wrapped. A bar chart broken across two lines is not a
/// bar chart, which is why those adapt to the width instead. The
/// continuation lines sit under the value rather than under the label.
fn wrap_pair(key: &str, value: &str, label_w: usize, w: usize) -> Vec<(String, String)> {
    let budget = (w.saturating_sub(label_w + 5)).max(8);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in value.split_whitespace() {
        let mut word = word.to_string();
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > budget {
            lines.push(std::mem::take(&mut line));
        }
        // A single word longer than the column is split rather than allowed
        // to run off; an enterprise sku is one word and still has to fit.
        while word.chars().count() > budget {
            lines.push(word.chars().take(budget).collect());
            word = word.chars().skip(budget).collect();
        }
        if line.is_empty() {
            line = word;
        } else {
            line.push(' ');
            line.push_str(&word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(i, part)| (if i == 0 { key.to_string() } else { String::new() }, part))
        .collect()
}

/// What to run to make each agent start recording. An empty tab that only
/// says "nothing here" leaves the reader to guess whether it is broken.
fn run_hint(name: &str) -> &'static str {
    match name {
        "claude" => "claude",
        "codex" => "codex",
        "cursor" => "cursor-agent",
        "grok" => "grok",
        "copilot" => "copilot",
        _ => "",
    }
}

/// The empty state: what is missing, and the one command that fixes it.
fn no_local(what: &str, run: &str, w: usize, p: &Palette) -> Vec<String> {
    let mut rows: Vec<String> = wrap_text(what, w.saturating_sub(4).max(20))
        .into_iter()
        .map(|line| tc::seg(&[(p.dim.as_str(), format!("  {}", line))], w - 1))
        .collect();
    if !run.is_empty() {
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "  run ".into()),
                (p.accent.as_str(), run.to_string()),
                (p.dim.as_str(), " here and this fills in".into()),
            ],
            w - 1,
        ));
    }
    rows
}

/// A rate card: US$ per million tokens, by priced kind.
type Rate = HashMap<String, f64>;

/// The rate for a model, and where it came from.
///
/// Config wins outright, then the published list prices. Keyed by model
/// rather than by agent, because a model has one list price wherever it
/// ran. Longest matching name wins, so claude-opus-4 does not shadow
/// claude-opus-4-8, and a "*" entry catches anything left over.
fn rate_for(model: &str, configured: &HashMap<String, Rate>) -> (Option<Rate>, &'static str) {
    if NO_PUBLISHED_PRICE.contains(&model) && !configured.contains_key(model) {
        return (None, "");
    }
    if let Some(rate) = configured.get(model) {
        return (Some(rate.clone()), "config");
    }
    let mut best: Option<(usize, Rate)> = None;
    for (key, rate) in configured {
        if key != "*" && model.contains(key.as_str()) {
            let len = key.chars().count();
            if best.as_ref().is_none_or(|(had, _)| len > *had) {
                best = Some((len, rate.clone()));
            }
        }
    }
    if let Some((_, rate)) = best {
        return (Some(rate), "config");
    }
    if let Some(rate) = configured.get("*") {
        return (Some(rate.clone()), "config");
    }
    let to_rate = |entries: &[(&str, f64)]| -> Rate {
        entries.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    };
    for (key, entries) in LIST_RATES {
        if *key == model {
            return (Some(to_rate(entries)), "list");
        }
    }
    let mut best: Option<(usize, Rate)> = None;
    for (key, entries) in LIST_RATES {
        if model.contains(key) {
            let len = key.chars().count();
            if best.as_ref().is_none_or(|(had, _)| len > *had) {
                best = Some((len, to_rate(entries)));
            }
        }
    }
    match best {
        Some((_, rate)) => (Some(rate), "list"),
        None => (None, ""),
    }
}

/// Token counts by priced kind.
type Tokens = HashMap<String, f64>;

fn cost_of(tokens: &Tokens, rate: &Rate) -> f64 {
    RATE_KINDS
        .iter()
        .map(|kind| {
            tokens.get(*kind).copied().unwrap_or(0.0) / 1e6
                * rate.get(*kind).copied().unwrap_or(0.0)
        })
        .sum()
}

fn empty_tokens() -> Tokens {
    RATE_KINDS.iter().map(|k| (k.to_string(), 0.0)).collect()
}

fn total_tokens(t: &Tokens) -> f64 {
    RATE_KINDS
        .iter()
        .map(|k| t.get(*k).copied().unwrap_or(0.0))
        .sum()
}

/// One costed window: its label, what it cost, how many tokens, and the
/// models under it.
type Window = (String, f64, f64, Vec<(String, f64)>);

/// The metered section: one row per window, each with its models under it.
///
/// Two windows - today and thirty days - because a month's total says what
/// an agent costs and today says whether that is still true. A single
/// all-time figure answered neither question.
#[allow(clippy::too_many_arguments)]
fn metered_block(
    where_: &str,
    windows: &[Window],
    w: usize,
    extras: &[(String, Option<f64>, String)],
    note: &str,
    scope: &str,
    caveat: &str,
    p: &Palette,
) -> Vec<String> {
    // Windows are kept even when empty, as long as something is. A zero
    // today against a busy month is the answer to "have I used this today".
    if !windows.iter().any(|x| x.1 > 0.0 || x.2 > 0.0) {
        return Vec::new();
    }
    // Scope first, because it is the thing most easily got wrong: this
    // section sits under a QUOTA labelled "account-wide", and a local figure
    // beside it reads as the same scope unless it says otherwise.
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── METERED ── ".into()),
            (
                p.txt.as_str(),
                if scope.is_empty() { String::new() } else { format!("{} · ", scope) },
            ),
            (p.dim.as_str(), format!("at {}", where_)),
            (
                p.dim.as_str(),
                if note.is_empty() { String::new() } else { format!("   {}", note) },
            ),
        ],
        w - 1,
    )];
    if !caveat.is_empty() {
        for line in wrap_text(caveat, w.saturating_sub(4).max(20)) {
            rows.push(tc::seg(&[(p.dim.as_str(), format!("  {}", line))], w - 1));
        }
    }
    let extras: Vec<&(String, Option<f64>, String)> =
        extras.iter().filter(|x| x.1.is_some()).collect();
    let label_w = windows
        .iter()
        .map(|x| x.0.chars().count())
        .chain(extras.iter().map(|x| x.0.chars().count()))
        .max()
        .unwrap_or(6);
    for (label, cost, tokens, models) in windows {
        rows.push(tc::seg(
            &[
                (p.txt.as_str(), format!("  {}  ", tc::pad(label, label_w))),
                (p.agent.as_str(), tc::pad(&format!("${:.2}", cost), 11)),
                (p.dim.as_str(), format!("{} tokens", big_num(*tokens))),
            ],
            w - 1,
        ));
        let top: Vec<&(String, f64)> = models.iter().take(5).collect();
        let name_w = top.iter().map(|(m, _)| m.chars().count()).max().unwrap_or(0);
        for (model, model_cost) in &top {
            rows.push(tc::seg(
                &[
                    (p.dim.as_str(), format!("  {}   ", " ".repeat(label_w))),
                    (p.dim.as_str(), format!("{}  ", tc::pad(model, name_w))),
                    (p.txt.as_str(), format!("${:.2}", model_cost)),
                ],
                w - 1,
            ));
        }
        if models.len() > top.len() {
            rows.push(tc::seg(
                &[
                    (p.dim.as_str(), format!("  {}   ", " ".repeat(label_w))),
                    (p.dim.as_str(), format!("+{} more", models.len() - top.len())),
                ],
                w - 1,
            ));
        }
    }
    // Summary rows below the windows rather than in the header, which had
    // grown long enough to clip the moment a scope word joined it.
    for (label, value, colour) in extras {
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!("  {}  ", tc::pad(label, label_w))),
                (colour.as_str(), format!("${:.2}", value.unwrap_or(0.0))),
            ],
            w - 1,
        ));
    }
    rows.push(String::new());
    rows
}

/// Cost a set of windows against the rate card.
///
/// Only models with a rate are counted and the unpriced ones are named, so
/// a half-filled card cannot read as a total.
#[allow(clippy::too_many_arguments)]
fn metered_rows(
    windows: &[(String, Vec<(String, Tokens)>)],
    w: usize,
    note: &str,
    agent: &str,
    scope: &str,
    caveat: &str,
    cfg: &Config,
    p: &Palette,
) -> Vec<String> {
    let mut origins: Vec<&'static str> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut built: Vec<Window> = Vec::new();
    for (label, entries) in windows {
        let (mut cost, mut tokens) = (0.0, 0.0);
        let mut models: Vec<(String, f64)> = Vec::new();
        for (model, counts) in entries {
            if total_tokens(counts) <= 0.0 {
                continue;
            }
            let (rate, origin) = rate_for(model, &cfg.rates);
            tokens += total_tokens(counts);
            let Some(rate) = rate else {
                if !missing.contains(model) {
                    missing.push(model.clone());
                }
                continue;
            };
            let this = cost_of(counts, &rate);
            cost += this;
            if !origins.contains(&origin) {
                origins.push(origin);
            }
            models.push((model.clone(), this));
        }
        models.sort_by(|a, b| b.1.total_cmp(&a.1));
        built.push((label.clone(), cost, tokens, models));
    }
    if !built.iter().any(|x| x.1 > 0.0) {
        if cfg.rates.is_empty() {
            let mut rows = vec![tc::seg(
                &[
                    (p.lbl.as_str(), " ── METERED ── ".into()),
                    (p.dim.as_str(), "no published rates for these models".into()),
                ],
                w - 1,
            )];
            rows.extend(no_local(
                "Set agent_usage.rates in config.json - US$ per million tokens, keyed by model.",
                "",
                w,
                p,
            ));
            rows.push(String::new());
            return rows;
        }
        return Vec::new();
    }
    // Where the prices came from belongs on screen: a list price is a dated
    // fact that goes stale in silence, and a configured one is the reader's
    // own assertion. Neither should be mistaken for the other.
    let where_ = if origins == ["config"] {
        "your configured rates".to_string()
    } else if origins == ["list"] {
        format!("list prices · {}", LIST_RATES_AS_OF)
    } else {
        format!("list prices · {}, some configured", LIST_RATES_AS_OF)
    };
    // A month's list cost against what the month actually cost you. Shown
    // only when the plan price is configured, because it is the one figure
    // in this section that no machine here knows.
    let month = built.iter().find(|x| x.0 == "30 days").map(|x| x.1);
    let saves = match (cfg.plan_cost.get(agent), month) {
        (Some(paid), Some(month)) => Some(month - paid),
        _ => None,
    };
    let mut rows = metered_block(
        &where_,
        &built,
        w,
        &[("the plan saves".to_string(), saves, p.ok.clone())],
        note,
        scope,
        caveat,
        p,
    );
    if !missing.is_empty() && !rows.is_empty() {
        missing.sort();
        let at = rows.len() - 1;
        rows.insert(
            at,
            tc::seg(
                &[
                    (
                        p.warn.as_str(),
                        format!(
                            "  {} model{} unpriced: ",
                            missing.len(),
                            if missing.len() == 1 { "" } else { "s" }
                        ),
                    ),
                    (
                        p.dim.as_str(),
                        missing.iter().take(3).cloned().collect::<Vec<_>>().join(", "),
                    ),
                ],
                w - 1,
            ),
        );
    }
    rows
}

/// A subscription block: what the plan is, then the facts about it.
///
/// Shared by four tabs so the same question is answered in the same shape
/// wherever you are on the wall - a percentage means little without the
/// subscription it is a percentage of.
fn plan_rows(
    headline: &str,
    pairs: &[(String, String)],
    w: usize,
    note: &str,
    wrapped: Option<(&str, &[String])>,
    caveat: &str,
    p: &Palette,
) -> Vec<String> {
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── SUBSCRIPTION ── ".into()),
            (
                p.txt.as_str(),
                if headline.is_empty() { "unknown".into() } else { headline.to_string() },
            ),
            (
                p.dim.as_str(),
                if note.is_empty() { String::new() } else { format!("   {}", note) },
            ),
        ],
        w - 1,
    )];
    if !caveat.is_empty() {
        for line in wrap_text(caveat, w.saturating_sub(4).max(20)) {
            rows.push(tc::seg(&[(p.dim.as_str(), format!("  {}", line))], w - 1));
        }
    }
    let label_w = pairs
        .iter()
        .map(|(k, _)| k.chars().count())
        .chain(wrapped.iter().map(|(k, _)| k.chars().count()))
        .max()
        .unwrap_or(0);
    for (key, value) in pairs {
        for (lab, part) in wrap_pair(key, value, label_w, w) {
            rows.push(tc::seg(
                &[
                    (p.dim.as_str(), format!("  {}  ", tc::pad(&lab, label_w))),
                    (p.txt.as_str(), part),
                ],
                w - 1,
            ));
        }
    }
    if let Some((label, names)) = wrapped {
        if !names.is_empty() {
            // Wrapped rather than clipped: a truncated list reads as a
            // shorter one, and only the first line takes the label.
            let budget = w.saturating_sub(label_w + 6).max(10);
            let mut lines: Vec<Vec<String>> = Vec::new();
            let mut line: Vec<String> = Vec::new();
            for name in names {
                let mut trial = line.clone();
                trial.push(name.clone());
                if !line.is_empty() && trial.join(" · ").chars().count() > budget {
                    lines.push(std::mem::take(&mut line));
                }
                line.push(name.clone());
            }
            if !line.is_empty() {
                lines.push(line);
            }
            for (i, part) in lines.iter().enumerate() {
                rows.push(tc::seg(
                    &[
                        (
                            p.dim.as_str(),
                            format!("  {}  ", tc::pad(if i == 0 { label } else { "" }, label_w)),
                        ),
                        (p.ok.as_str(), part.join(" · ")),
                    ],
                    w - 1,
                ));
            }
        }
    }
    rows
}

/// Append a section with exactly one blank line before it.
///
/// The separator is owned here rather than by callers who each end
/// differently - some finish on a blank line and would otherwise leave two,
/// and plain concatenation leaves none at all.
fn add_section(mut rows: Vec<String>, block: Vec<String>) -> Vec<String> {
    if block.is_empty() {
        return rows;
    }
    while rows.last().is_some_and(|x| x.is_empty()) {
        rows.pop();
    }
    rows.push(String::new());
    rows.extend(block);
    rows
}

/// Tokens per day, drawn the way Claude Code's own /stats draws it.
///
/// Weekday rows with only Mon, Wed and Fri labelled; one cell per day;
/// months named across the top; solid blocks in four steps of a single hue,
/// and a dim dot for a day the file has no entry for.
struct Calendar {
    rows: Vec<Vec<(String, String)>>,
    best: Option<NaiveDate>,
    active: usize,
    span: usize,
    longest: usize,
    current: usize,
}

fn day_calendar(
    totals: &HashMap<NaiveDate, f64>,
    w: usize,
    steps: [(u8, u8, u8); 4],
    weeks: Option<usize>,
    p: &Palette,
) -> Option<Calendar> {
    if totals.is_empty() {
        return None;
    }
    let peak = totals.values().cloned().fold(0.0f64, f64::max).max(1.0);
    let best = totals
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(d, _)| *d);
    let last = *totals.keys().max()?;
    let first = *totals.keys().min()?;
    // A caller with a bounded window says so, rather than having its month
    // of data stretched across a year of empty dots.
    let fit = weeks.unwrap_or(w.saturating_sub(7)).clamp(4, w.saturating_sub(7).max(4));
    let end_week = last - Days::days(last.weekday().num_days_from_monday() as i64);
    let starts: Vec<NaiveDate> = (0..fit)
        .rev()
        .map(|i| end_week - Days::days(7 * i as i64))
        .collect();

    // Month names sit over the week their month starts in, three characters
    // wide like /stats - a single initial is not a label, it is a hint. A
    // month label needs three clear cells; without checking where the last
    // one ended, a short month writes over its neighbour.
    let mut strip = vec![' '; starts.len()];
    let (mut seen, mut wrote_to): (Option<u32>, i64) = (None, -1);
    for (x, wk) in starts.iter().enumerate() {
        if Some(wk.month()) != seen && x as i64 > wrote_to && x + 3 <= strip.len() {
            seen = Some(wk.month());
            for (k, ch) in MONTHS[wk.month0() as usize].chars().enumerate() {
                strip[x + k] = ch;
            }
            wrote_to = x as i64 + 3;
        }
    }
    let mut rows: Vec<Vec<(String, String)>> = vec![vec![(
        p.dim.clone(),
        format!("     {}", strip.iter().collect::<String>()),
    )]];
    for i in 0..7 {
        let label = match i {
            0 => "Mon",
            2 => "Wed",
            4 => "Fri",
            _ => "",
        };
        let mut line = vec![(p.dim.clone(), format!(" {:<4}", label))];
        for wk in &starts {
            let day = *wk + Days::days(i);
            match totals.get(&day) {
                None => line.push((p.empty_cell.clone(), "·".into())),
                Some(n) => line.push((shade((n / peak).sqrt(), steps), "█".into())),
            }
        }
        rows.push(line);
    }

    // Active out of days in the range, not out of days the file happens to
    // list - otherwise every day is active by construction.
    let span = (last - first).num_days() as usize + 1;
    let active = totals.values().filter(|v| **v > 0.0).count();
    let (mut run, mut longest) = (0usize, 0usize);
    for i in 0..span {
        let day = first + Days::days(i as i64);
        run = if totals.get(&day).is_some_and(|v| *v > 0.0) { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let mut current = 0usize;
    for i in 0..span {
        let day = last - Days::days(i as i64);
        if !totals.get(&day).is_some_and(|v| *v > 0.0) {
            break;
        }
        current += 1;
    }
    Some(Calendar {
        rows,
        best,
        active,
        span,
        longest,
        current,
    })
}

/// Settings, read once, so no widget-wide mutable globals are needed.
#[derive(Default, Clone)]
struct Config {
    agents: Vec<String>,
    exclude_agents: Vec<String>,
    rates: HashMap<String, Rate>,
    plan_cost: HashMap<String, f64>,
    refresh: f64,
    /// Grok is the only agent with no live quota unless it is asked for one.
    /// Off by default: asking means a request to x.ai carrying the token its
    /// CLI left on disk, and a widget that reads should not start talking to
    /// a vendor because it was launched.
    ///
    /// Turning it on also permits running the Grok CLI once after a session
    /// goes quiet, because that is what refreshes the token the request
    /// needs. The two were separate settings for one release and should not
    /// have been: asking without refreshing works until the token lapses and
    /// then stops, silently, which is the failure the refresh exists to
    /// prevent. Nobody wants the first without the second.
    grok_ping: bool,
    /// Whether Antigravity's quota may be asked of Google when nothing is
    /// serving it locally.
    ///
    /// On by default, unlike `grok_ping`, and the difference is what the
    /// request is. Grok's asks a vendor for a reading nothing on this
    /// machine has. This one asks for the same reading the app already
    /// serves over localhost, from the same host and with the same
    /// credential the tier is already fetched with. Turning it off costs
    /// the quota whenever Antigravity is closed and spares nothing that the
    /// tier request has not already spent.
    antigravity_remote: bool,
    /// Whether the widget may start the `agy` CLI to read the quota it
    /// serves, when nothing else has one.
    ///
    /// On by default, and the only thing here that runs somebody else's
    /// program. Three things make that defensible where Grok's equivalent
    /// is off: it is the reader's own CLI, already installed and signed in;
    /// it is started under a pseudo-terminal of ours, killed by pid the
    /// moment the reading is taken, and reaped, so nothing outlives the
    /// fetch; and a CLI the reader started themselves is found by the
    /// ordinary local probe long before this runs, so this can never shut
    /// one down. Turn it off and the quota lasts an hour past the last time
    /// Antigravity ran, which is the token's life.
    antigravity_start: bool,
    /// Minutes between those requests. Five, so the figure on screen is
    /// one a reader can act on: the window it reports moves over days, but
    /// the spend inside it moves while they work, and an hour-old reading
    /// of a live session is exactly the stale number this asks the server
    /// to avoid. One small GET twelve times an hour is not traffic.
    grok_ping_minutes: f64,
    /// Set when the settings came from a leftover `usage` section rather
    /// than `agent_usage`. The pane says so, because a silent fallback is
    /// how a rename looks like nothing changed.
    legacy_section: bool,
}

fn read_config() -> Config {
    let (raw, legacy_section) = load_agent_usage_config();
    let table = |key: &str| -> HashMap<String, Rate> {
        raw[key]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(model, entry)| {
                let rate: Rate = entry
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(k, v)| v.as_f64().map(|v| (k.clone(), v)))
                    .collect();
                (model.clone(), rate)
            })
            .collect()
    };
    Config {
        agents: tc::cfg_strings(&raw, "agents", &[]),
        exclude_agents: tc::cfg_strings(&raw, "exclude_agents", &[]),
        rates: table("rates"),
        plan_cost: raw["plan_cost"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| v.as_f64().map(|v| (k.clone(), v)))
            .collect(),
        refresh: tc::poll_secs(tc::cfg_f64(&raw, "refresh", 30.0), 30.0),
        grok_ping: raw
            .get("grok_ping")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        grok_ping_minutes: tc::cfg_f64(&raw, "grok_ping_minutes", 5.0),
        antigravity_remote: raw
            .get("antigravity_remote")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        antigravity_start: raw
            .get("antigravity_start")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        legacy_section,
    }
}

/// `agent_usage` if that section is present, otherwise a leftover `usage`.
///
/// `load_config` returns `{}` for a missing section, so emptiness alone
/// cannot tell "not set" from "set under the old name". Presence is what
/// decides, and a leftover section is reported so the pane does not look
/// like nothing changed.
fn load_agent_usage_config() -> (serde_json::Value, bool) {
    let parsed = first_readable_config().unwrap_or_else(|| serde_json::json!({}));
    let (section, legacy) = pick_config_section(&parsed);
    // Both names stay as string literals so check.rs sees the primary
    // section and the fallback, not a variable it cannot read.
    let raw = if section == "usage" {
        tc::load_config("usage")
    } else {
        tc::load_config("agent_usage")
    };
    (raw, legacy)
}

fn first_readable_config() -> Option<serde_json::Value> {
    for path in tc::config_paths() {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        match serde_json::from_str(&text) {
            Ok(v) => return Some(v),
            Err(_) => continue,
        }
    }
    None
}

/// Which section to read, and whether it is the pre-rename name.
fn pick_config_section(parsed: &serde_json::Value) -> (&'static str, bool) {
    if parsed.get("agent_usage").is_some() {
        ("agent_usage", false)
    } else if parsed.get("usage").is_some() {
        ("usage", true)
    } else {
        ("agent_usage", false)
    }
}

/// What we know how to read, and how to tell it is here.
///
/// An agent counts as present if its CLI is on PATH *or* it has left state
/// behind: an uninstalled agent whose history is still on disk is worth
/// showing, and a CLI installed under a different name would otherwise
/// vanish.
fn agent_spec(name: &str) -> (&'static str, Vec<&'static str>, Vec<String>) {
    match name {
        "claude" => (
            "Claude Code",
            vec!["claude"],
            vec![under_home(".claude/stats-cache.json")],
        ),
        "codex" => ("OpenAI Codex", vec!["codex"], vec![under_home(".codex/sessions")]),
        "cursor" => (
            "Cursor",
            vec!["cursor-agent", "cursor"],
            vec![under_home(".cursor/ai-tracking/ai-code-tracking.db")],
        ),
        "grok" => ("Grok", vec!["grok"], vec![under_home(".grok")]),
        "copilot" => (
            "GitHub Copilot",
            vec!["copilot"],
            vec![under_home(".copilot/session-store.db"), under_home(".copilot/config.json")],
        ),
        // No binary on PATH to look for: the CLI is launched by the IDE and
        // its server is fetched per run, so the state directory is the only
        // proof it is here - which is why detection takes paths as well.
        "antigravity" => (
            "Antigravity",
            vec!["antigravity"],
            vec![under_home(".gemini/antigravity-cli")],
        ),
        other => (Box::leak(other.to_string().into_boxed_str()), vec![], vec![]),
    }
}

#[derive(Clone, Default)]
struct Presence {
    present: bool,
}

fn detect_agents() -> HashMap<String, Presence> {
    ORDER
        .iter()
        .map(|name| {
            let (_, bins, paths) = agent_spec(name);
            let has_bin = bins.iter().any(|b| tc::missing(&[b]).is_empty());
            let has_data = paths.iter().any(|p| std::path::Path::new(p).exists());
            (
                name.to_string(),
                Presence {
                    present: has_bin || has_data,
                },
            )
        })
        .collect()
}

/// The tabs to draw.
///
/// Empty `agents` discovers every agent this machine actually has. Naming
/// them instead fixes both the set and the order, whether or not they are
/// installed - if you listed it, you want the tab. Falls back to everything
/// known if the result would be empty, because a widget with no tabs
/// teaches nothing and the likeliest cause is a typo.
fn visible_agents(found: &HashMap<String, Presence>, cfg: &Config) -> Vec<String> {
    let known: Vec<&str> = ORDER.to_vec();
    let named: Vec<String> = cfg
        .agents
        .iter()
        .filter(|n| known.contains(&n.as_str()))
        .cloned()
        .collect();
    let chosen: Vec<String> = if named.is_empty() {
        ORDER
            .iter()
            .filter(|n| found.get(**n).is_some_and(|x| x.present))
            .map(|n| n.to_string())
            .collect()
    } else {
        named
    };
    let shown: Vec<String> = chosen
        .into_iter()
        .filter(|n| !cfg.exclude_agents.contains(n))
        .collect();
    // The summary leads and is never discovered or excluded: it is not an
    // agent, it is the view across whichever agents there turn out to be.
    let mut out = vec![SUMMARY_TAB.to_string()];
    if shown.is_empty() {
        out.extend(ORDER.iter().map(|n| n.to_string()));
    } else {
        out.extend(shown);
    }
    out
}

/// Names in the config that match no agent we know how to read.
fn config_complaints(cfg: &Config) -> String {
    let mut parts: Vec<String> = Vec::new();
    if cfg.legacy_section {
        parts.push(LEGACY_SECTION_NOTE.to_string());
    }
    let mut bad: Vec<String> = cfg
        .agents
        .iter()
        .chain(cfg.exclude_agents.iter())
        .filter(|n| !ORDER.contains(&n.as_str()))
        .cloned()
        .collect();
    if !bad.is_empty() {
        bad.sort();
        bad.dedup();
        parts.push(format!(
            "unknown agent in config: {} (known: {})",
            bad.join(", "),
            ORDER.join(", ")
        ));
    }
    parts.join(" · ")
}

/// Shown when the settings came from a leftover `usage` section.
const LEGACY_SECTION_NOTE: &str =
    "config section is still called usage; rename it to agent_usage";

/// The gripe as rows, wrapped so a narrow pane keeps the words that matter.
///
/// `seg` clips. The 65-character rename note ends at `rename it to age` in
/// a 58-column pane, which is the documented width these are dragged to,
/// and hides the section name the reader has to type. Continuation lines
/// sit under the `!` rather than under the first word.
fn gripe_lines(gripe: &str, w: usize) -> Vec<String> {
    let budget = w.saturating_sub(4).max(8);
    wrap_text(gripe, budget)
        .into_iter()
        .enumerate()
        .map(|(i, part)| {
            if i == 0 {
                format!(" ! {}", part)
            } else {
                format!("   {}", part)
            }
        })
        .collect()
}

fn tab_bar(
    active: &str,
    installed: &HashMap<String, Presence>,
    tabs: &[String],
    w: usize,
    p: &Palette,
) -> String {
    // Brackets as well as the tint: which tab is open must not depend on a
    // background colour surviving. A dot marks an agent that is installed.
    let mut parts: Vec<(String, String)> = vec![(tc::RST.to_string(), " ".into())];
    for name in tabs {
        let here = name == active;
        let have = installed.get(name).is_some_and(|x| x.present);
        parts.push((
            if here {
                format!("{}{}", tc::bg(38, 56, 76), p.accent)
            } else {
                p.dim.clone()
            },
            if here {
                format!("[{}]", name.to_uppercase())
            } else {
                format!(" {} ", name.to_uppercase())
            },
        ));
        if name == SUMMARY_TAB {
            parts.push((p.grid.clone(), " ".into()));
            continue;
        }
        parts.push((
            if have { p.ok.clone() } else { p.grid.clone() },
            if have { "·".into() } else { " ".to_string() },
        ));
    }
    let refs: Vec<(&str, String)> = parts.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
    tc::seg(&refs, w - 1)
}

/// Where a tab cursor lands after moving `by` tabs among `count`.
///
/// rem_euclid rather than %, because the left key drives this negative
/// and Rust's % keeps the sign where Python's does not. Named rather than
/// inlined so a test can reach the arithmetic the loop actually runs.
fn step_tab(at: i64, by: i64, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    (at + by).rem_euclid(count as i64) as usize
}

/// What to show before the first poll lands.
///
/// Every tab's empty state is a statement of fact - no stats cache, no
/// rollouts, no agent publishing a quota - and each of them is false while
/// the first read is still running. The first read is also the slow one,
/// which is more than long enough for a wrong answer to be read and
/// believed.
fn loading_rows(w: usize, tick: usize, p: &Palette) -> Vec<String> {
    let mut rows = vec![
        tc::seg(
            &[
                (p.accent.as_str(), format!(" {}", SPINNER[tick % SPINNER.len()])),
                (p.txt.as_str(), "  reading local state and quotas".into()),
            ],
            w - 1,
        ),
        String::new(),
    ];
    for part in wrap_text(
        "The first pass is the slow one: Claude's transcripts run to hundreds \
         of megabytes and Cursor's usage events are paged a thousand at a \
         time. Both are cached afterwards.",
        w.saturating_sub(4).max(20),
    ) {
        rows.push(tc::seg(&[(p.dim.as_str(), format!("  {}", part))], w - 1));
    }
    rows
}

fn main() {
    tc::maybe_help(include_str!("agent-usage_help.txt"));
    let cfg = read_config();
    let mut refresh = cfg.refresh;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() >= 2 && (args[0] == "-n" || args[0] == "--refresh") {
        refresh = tc::poll_secs(args[1].parse().unwrap_or(refresh), refresh);
    }

    let p = palette();
    let state = Arc::new(Mutex::new(vendors::State::default()));
    let wake = Arc::new((Mutex::new(false), Condvar::new()));
    let poller = Arc::clone(&state);
    let poller_wake = Arc::clone(&wake);
    let poller_cfg = cfg.clone();
    std::thread::spawn(move || {
        let mut caches = shared::Caches::default();
        loop {
            // A poller that dies takes its explanation with it, and an empty
            // board looks exactly like a machine with no agents on it.
            let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                vendors::read_all(&mut caches, &poller_cfg)
            }));
            match read {
                Ok(found) => {
                    if let Ok(mut g) = poller.lock() {
                        *g = found;
                        g.fetched = now();
                    }
                }
                Err(_) => {
                    if let Ok(mut g) = poller.lock() {
                        g.err = "poller stopped - see the pane it was started from".into();
                    }
                    return;
                }
            }
            let (lock, cond) = &*poller_wake;
            let mut asked = match lock.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            if !*asked {
                asked = match cond.wait_timeout(asked, Duration::from_secs_f64(refresh)) {
                    Ok((g, _)) => g,
                    Err(_) => return,
                };
            }
            *asked = false;
        }
    });

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    // Signed, because the left key has to be able to go below zero and
    // wrap; rem_euclid then brings it back into range the way Python's
    // % does for a negative index.
    let (mut active, mut tick) = (0i64, 0usize);
    // Switching tabs lands at the top of the new one.
    //
    // This used to be one offset per tab, kept so that switching away and
    // back returned you to where you were reading. In use that is the wrong
    // trade: the tabs are different lengths and different shapes, so a
    // remembered offset from a forty-row tab opens the next one part-way
    // down with its heading scrolled off, and the first thing a reader does
    // on arriving somewhere new is look at the top of it. Only one offset is
    // needed now, carried across frames of the same tab and dropped the
    // moment the tab changes.
    let (mut carried, mut shown) = (0usize, String::new());

    loop {
        tick += 1;
        // Scrolling is applied after the frame is built, not here: a page is
        // however many body rows this pane turned out to have, and that is
        // not known until the tab has been rendered and the footer packed.
        let mut moves: Vec<i64> = Vec::new();
        let (mut to_top, mut to_bottom) = (false, false);
        let mut pages: Vec<i64> = Vec::new();
        for key in keyboard.poll() {
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "right" | "tab" | "l" => active += 1,
                "left" | "h" => active -= 1,
                "up" | "k" => moves.push(-1),
                "down" | "j" => moves.push(1),
                "pgup" => pages.push(-1),
                "pgdn" => pages.push(1),
                "home" => to_top = true,
                "end" => to_bottom = true,
                "r" | "R" => {
                    let (lock, cond) = &*wake;
                    if let Ok(mut asked) = lock.lock() {
                        *asked = true;
                        cond.notify_all();
                    }
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let snapshot = match state.lock() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        let mut rows = vec![tc::title("agent usage", w, &p.agent)];
        let tabs = visible_agents(&snapshot.installed, &cfg);
        let at = step_tab(active, 0, tabs.len());
        active = at as i64;
        let name = tabs[at].clone();
        let hidden = ORDER
            .iter()
            .filter(|n| {
                snapshot.installed.get(**n).is_some_and(|x| x.present)
                    && !tabs.contains(&n.to_string())
            })
            .count();

        let status_at = rows.len();
        rows.push(String::new()); // filled in once the scroll is resolved
        let gripe = if snapshot.err.is_empty() {
            config_complaints(&cfg)
        } else {
            snapshot.err.clone()
        };
        if !gripe.is_empty() {
            for line in gripe_lines(&gripe, w) {
                rows.push(tc::seg(&[(p.bad.as_str(), line)], w - 1));
            }
        }
        rows.push(tab_bar(&name, &snapshot.installed, &tabs, w, &p));
        rows.push(String::new());

        let body = if snapshot.fetched <= 0.0 {
            loading_rows(w, tick, &p)
        } else {
            vendors::tab_body(&name, &snapshot, w, h, &cfg, &p)
        };

        let mut hints: Vec<Vec<(&str, String)>> = vec![
            vec![(p.accent.as_str(), "←→".into()), (p.dim.as_str(), " agent".into())],
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " scroll".into())],
            vec![(p.dim.as_str(), "[r]efresh".into())],
            vec![(p.dim.as_str(), "[q]uit".into())],
        ];
        // The footer is packed once, with the scroll hint always counted, so
        // the body's height does not change when scrolling becomes possible.
        let reserved = tc::pack_hints(&hints, w - 2, "  ").len();
        let avail = h.saturating_sub(rows.len() + reserved).max(1);
        let top = body.len().saturating_sub(avail);
        let mut off = if name == shown { carried.min(top) } else { 0 };
        if to_top {
            off = 0;
        }
        if to_bottom {
            off = top;
        }
        for page in &pages {
            let step = avail.saturating_sub(1).max(1) as i64;
            off = (off as i64 + page * step).clamp(0, top as i64) as usize;
        }
        for move_ in &moves {
            off = (off as i64 + move_).clamp(0, top as i64) as usize;
        }
        off = off.min(top);
        carried = off;
        shown = name.clone();

        let view: Vec<String> = body.iter().skip(off).take(avail).cloned().collect();
        let where_ = if top > 0 {
            // Never let a partial view read as the whole tab, and say which
            // way there is more: an arrow simply absent at the top of a long
            // tab looks the same as a tab that ends there.
            format!(
                "   {}-{} of {} {}{}",
                off + 1,
                off + view.len(),
                body.len(),
                if off > 0 { "▲" } else { " " },
                if off < top { "▼" } else { " " }
            )
        } else {
            hints.retain(|x| x[0].1 != "↑↓");
            String::new()
        };
        // The scroll position goes last on this line but matters most, so
        // the legend stands down to make room rather than being clipped.
        let base = format!(" local state · live quota · read {} ago", ago(snapshot.fetched));
        let hidden_txt = if hidden > 0 {
            format!("   {} hidden by config", hidden)
        } else {
            String::new()
        };
        let mut legend = "   · = detected".to_string();
        if base.len() + hidden_txt.len() + legend.len() + where_.len() > w - 1 {
            legend.clear();
        }
        rows[status_at] = tc::seg(
            &[
                (p.dim.as_str(), base),
                (p.dim.as_str(), legend),
                (p.dim.as_str(), hidden_txt),
                (p.accent.as_str(), where_),
            ],
            w - 1,
        );

        let mut footer: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
        // Padded back to the height already reserved, so dropping the scroll
        // hint does not lift the footer off the bottom of the pane.
        while footer.len() < reserved {
            footer.insert(0, String::new());
        }
        rows.extend(view);
        while rows.len() < h.saturating_sub(footer.len()) {
            rows.push(String::new());
        }
        rows.extend(footer);
        rows.truncate(h);
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(300));
    }
}

// Kept in a directory of its own rather than beside this file: anything
// dropped straight into src/bin/ risks being taken for another binary. One
// module per agent, because they share only the shape the summary screen
// compares them in - and because five readers being written at once should
// not be five edits to the same file.
#[path = "agent-usage/shared.rs"]
mod shared;
#[path = "agent-usage/antigravity.rs"]
mod antigravity;
#[path = "agent-usage/claude.rs"]
mod claude;
#[path = "agent-usage/codex.rs"]
mod codex;
#[path = "agent-usage/copilot.rs"]
mod copilot;
#[path = "agent-usage/cursor.rs"]
mod cursor;
#[path = "agent-usage/grok.rs"]
mod grok;
#[path = "agent-usage/vendors.rs"]
mod vendors;

#[cfg(test)]
mod tests {

    /// Under a day, say the hours. The quota headings used to render this
    /// span three different ways - "~0.9 days", a truncated "0d", and
    /// "resets today" - and all three round away the part a reader is
    /// asking for when the reset is close.
    #[test]
    fn a_span_under_a_day_is_hours_and_minutes() {
        assert_eq!(left_span(22.0 * 3600.0 + 6.0 * 60.0), "22h 6m");
        assert_eq!(left_span(45.0 * 60.0), "45m");
        assert_eq!(left_span(3600.0), "1h 0m");
        // A day or more keeps days, which is what that range wants.
        assert_eq!(left_span(5.0 * 86400.0 + 12.0 * 3600.0), "5d 12h");
        assert_eq!(left_span(86400.0), "1d 0h");
        // The boundary the old code fell off: just under a day is not "0d".
        let almost = 86400.0 - 60.0;
        assert_eq!(left_span(almost), "23h 59m");
        assert!(!left_span(almost).starts_with('0'));
    }
    use super::*;

    #[test]
    fn a_model_takes_the_longest_matching_rate() {
        let none: HashMap<String, Rate> = HashMap::new();
        // claude-opus-4 must not shadow claude-opus-4-8.
        let (rate, origin) = rate_for("claude-opus-4-8", &none);
        assert_eq!(origin, "list");
        assert_eq!(rate.unwrap().get("input"), Some(&5.0));
        // A model name with a suffix still matches its family.
        let (rate, _) = rate_for("claude-sonnet-5-20260101", &none);
        assert_eq!(rate.unwrap().get("output"), Some(&10.0));
        // Config wins outright over the list.
        let mut mine: HashMap<String, Rate> = HashMap::new();
        mine.insert(
            "claude-opus-5".into(),
            [("input".to_string(), 99.0)].into_iter().collect(),
        );
        let (rate, origin) = rate_for("claude-opus-5", &mine);
        assert_eq!(origin, "config");
        assert_eq!(rate.unwrap().get("input"), Some(&99.0));
    }

    #[test]
    fn a_model_with_no_published_price_reports_as_unpriced() {
        // Spark is explicitly not on the API, so prefix matching must not
        // hand it its family's rate - that would be a number nobody
        // published, which is the one thing this widget must never show.
        let none: HashMap<String, Rate> = HashMap::new();
        assert!(rate_for("gpt-5.3-codex-spark", &none).0.is_none());
        // Unless the reader asserts one themselves.
        let mut mine: HashMap<String, Rate> = HashMap::new();
        mine.insert(
            "gpt-5.3-codex-spark".into(),
            [("input".to_string(), 1.0)].into_iter().collect(),
        );
        assert!(rate_for("gpt-5.3-codex-spark", &mine).0.is_some());
    }

    #[test]
    fn a_cost_is_the_sum_of_its_priced_kinds() {
        let rate: Rate = [
            ("input".to_string(), 5.0),
            ("output".to_string(), 25.0),
            ("cache_read".to_string(), 0.5),
        ]
        .into_iter()
        .collect();
        let mut tokens = empty_tokens();
        tokens.insert("input".into(), 1_000_000.0);
        tokens.insert("output".into(), 2_000_000.0);
        tokens.insert("cache_read".into(), 10_000_000.0);
        // 5 + 50 + 5
        assert!((cost_of(&tokens, &rate) - 60.0).abs() < 1e-9);
        // A kind the card does not price contributes nothing rather than
        // falling back to another kind's rate.
        tokens.insert("cache_write".into(), 9_999_999.0);
        assert!((cost_of(&tokens, &rate) - 60.0).abs() < 1e-9);
    }

    #[test]
    fn a_small_percentage_is_not_rounded_into_nothing() {
        // A real 0.03% and an empty section have to be tellable apart.
        assert_eq!(pct_text(0.0), "    0%");
        assert_eq!(pct_text(0.03), " 0.03%");
        assert_eq!(pct_text(0.4), " 0.40%");
        assert_eq!(pct_text(4.2), "  4.2%");
        assert_eq!(pct_text(71.0), "   71%");
        // Always six cells, whatever the value.
        for pct in [0.0, 0.03, 4.2, 71.0, 100.0] {
            assert_eq!(pct_text(pct).chars().count(), 6, "{}", pct);
        }
    }

    #[test]
    fn a_pace_figure_waits_until_the_window_means_something() {
        let window = Some(7.0 * 86400.0);
        // Ten minutes into a week, every number looks like a catastrophe.
        let just_started = Some(now() + 7.0 * 86400.0 - 600.0);
        assert_eq!(lead(1.0, window, just_started), None);
        // Half way through, spending a third is a real cushion.
        let half = Some(now() + 3.5 * 86400.0);
        let got = lead(33.0, window, half).expect("a pace");
        assert!((got - 17.0).abs() < 1.0, "got {}", got);
        // And spending two thirds is not.
        assert!(lead(67.0, window, half).expect("a pace") < 0.0);
        // Nothing to say without both halves.
        assert_eq!(lead(50.0, None, half), None);
        assert_eq!(lead(50.0, window, None), None);
    }

    #[test]
    fn a_span_says_what_it_is_rather_than_zero() {
        assert_eq!(span_ms(90_000.0), "1m");
        assert_eq!(span_ms(3_600_000.0), "1h 0m");
        assert_eq!(span_ms(90_000_000.0), "1d 1h 0m");
        // A couple of seconds of generation is not "0m".
        assert_eq!(span_ms(2_400.0), "2.4s");
    }

    #[test]
    fn big_numbers_stay_readable() {
        assert_eq!(big_num(999.0), "999");
        assert_eq!(big_num(1_500.0), "1.5k");
        assert_eq!(big_num(2_400_000.0), "2.4M");
        assert_eq!(big_num(7_100_000_000.0), "7.1B");
    }

    #[test]
    fn a_timestamp_is_read_whichever_shape_it_arrives_in() {
        // These APIs mix Z with +00:00 in the same response, and Go writes
        // nanoseconds where the parser takes six digits.
        let want = iso_epoch("2026-08-23T04:15:00+00:00").expect("offset form");
        assert_eq!(iso_epoch("2026-08-23T04:15:00Z"), Some(want));
        // The two spellings of the same zone must give the same answer to
        // the same precision. This asserted that the nanosecond form
        // equalled the whole second - which is the fraction being thrown
        // away, and adjacent records then look 0 or 1 second apart when a
        // rate is computed by dividing tokens by that gap.
        let fine = iso_epoch("2026-08-23T04:15:00.123456789Z").expect("nanoseconds");
        assert!((fine - (want + 0.123456)).abs() < 1e-6, "got {}", fine - want);
        assert_eq!(iso_epoch("2026-08-23T04:15:00.123456+00:00"), Some(fine));
        assert!(iso_epoch("").is_none());
        assert!(iso_epoch("not a date").is_none());
    }

    #[test]
    fn the_left_key_wraps_to_the_last_tab_at_every_tab_count() {
        // How the loop moves between tabs: a signed cursor brought back
        // into range with rem_euclid. Checked across counts because the
        // fault this replaced was right for powers of two and wrong for
        // everything else - a test at one width would have been a coin
        // toss.
        for count in 2usize..10 {
            let n = count as i64;
            assert_eq!(step_tab(0, -1, count), count - 1, "left from the first of {}", count);
            assert_eq!(step_tab(n - 1, 1, count), 0, "right from the last of {}", count);
            for at in 0..n {
                let back = step_tab(at, -1, count) as i64;
                assert_eq!(step_tab(back, 1, count) as i64, at, "there and back from {}", at);
            }
        }
        // An empty tab list must not index anything.
        assert_eq!(step_tab(0, -1, 0), 0);
    }

    #[test]
    fn a_bar_marks_where_an_even_burn_would_be() {
        let p = palette();
        let plain = |parts: &[(String, String)]| -> String {
            parts.iter().map(|(_, t)| t.clone()).collect()
        };
        // Half spent, half the window gone: the mark sits at the boundary.
        let bar = paced_bar(0.5, Some(0.5), 10, Some((240, 132, 84)), &p);
        let drawn = plain(&bar);
        assert_eq!(drawn.chars().count(), 10);
        assert!(drawn.contains('┃'));
        // No window information means no mark, not a mark at zero.
        let bare = plain(&paced_bar(0.5, None, 10, None, &p));
        assert!(!bare.contains('┃'));
        assert_eq!(bare.chars().count(), 10);
    }

    #[test]
    fn a_calendar_counts_streaks_over_the_range_not_the_entries() {
        let p = palette();
        let day = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let totals: HashMap<NaiveDate, f64> = [
            (day("2026-08-01"), 10.0),
            (day("2026-08-02"), 5.0),
            // 3 August is absent, which is a gap rather than a zero.
            (day("2026-08-04"), 7.0),
            (day("2026-08-05"), 3.0),
            (day("2026-08-06"), 1.0),
        ]
        .into_iter()
        .collect();
        let cal = day_calendar(&totals, 60, HEAT_STEPS, None, &p).expect("a calendar");
        assert_eq!(cal.span, 6);
        assert_eq!(cal.active, 5);
        assert_eq!(cal.longest, 3);
        assert_eq!(cal.current, 3);
        assert_eq!(cal.best, Some(day("2026-08-01")));
        // Seven weekday rows plus the month strip.
        assert_eq!(cal.rows.len(), 8);
    }

    #[test]
    fn the_new_config_section_wins_over_the_old_name() {
        let both = serde_json::json!({"agent_usage": {}, "usage": {"grok_ping": true}});
        assert_eq!(pick_config_section(&both), ("agent_usage", false));
        let leftover = serde_json::json!({"usage": {"grok_ping": true}});
        assert_eq!(pick_config_section(&leftover), ("usage", true));
        let empty = serde_json::json!({});
        assert_eq!(pick_config_section(&empty), ("agent_usage", false));
        // Present-but-empty is still a hit: they created the new section.
        let blank = serde_json::json!({"agent_usage": {}});
        assert_eq!(pick_config_section(&blank), ("agent_usage", false));
    }

    #[test]
    fn a_legacy_section_note_keeps_the_new_name_at_a_narrow_pane() {
        // 58 is the documented width a pane is dragged to. The note is 65
        // cells with its prefix; clipping there hid `agent_usage`.
        let lines = gripe_lines(LEGACY_SECTION_NOTE, 58);
        assert!(
            lines.iter().any(|l| l.contains("agent_usage")),
            "the section name was lost: {:?}",
            lines
        );
        assert!(
            lines.iter().all(|l| l.chars().count() <= 57),
            "a wrapped line still overflowed: {:?}",
            lines
        );
        let tight = gripe_lines(LEGACY_SECTION_NOTE, 30);
        assert!(
            tight.iter().any(|l| l.contains("agent_usage")),
            "the section name was lost at 30: {:?}",
            tight
        );
        assert!(tight.len() > 1, "should have wrapped: {:?}", tight);
    }

    #[test]
    fn a_leftover_section_is_named_on_screen() {
        let mut cfg = Config::default();
        assert!(config_complaints(&cfg).is_empty());
        cfg.legacy_section = true;
        assert_eq!(config_complaints(&cfg), LEGACY_SECTION_NOTE);
        cfg.agents = vec!["nonsence".into()];
        let got = config_complaints(&cfg);
        assert!(got.contains(LEGACY_SECTION_NOTE), "{got}");
        assert!(got.contains("unknown agent in config: nonsence"), "{got}");
    }

    #[test]
    fn a_sentence_wraps_rather_than_losing_its_end() {
        assert_eq!(wrap_text("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap_text("", 10), vec![""]);
        // A labelled value's continuation lines sit under the value.
        let got = wrap_pair("plan", "a rather long enterprise sku here", 6, 30);
        assert_eq!(got[0].0, "plan");
        assert_eq!(got[1].0, "");
        assert!(got.len() > 1);
    }
}
