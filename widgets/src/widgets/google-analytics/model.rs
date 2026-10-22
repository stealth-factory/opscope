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

//! The arithmetic between GA's reports and the screen, kept apart from
//! both so every figure on screen can be tested from a fixture.
//!
//! Three rules run through it. An hour GA has not processed is *absent*,
//! never zero. A comparison is cumulative to the same hour on both sides,
//! so a half-processed day is never set against a whole one. And a
//! baseline is built only from weeks this widget was actually running in,
//! with the number of those weeks said beside it.
#![allow(dead_code)]

use chrono::{Datelike, NaiveDate, TimeZone};
use chrono_tz::Tz;

use crate::parse::{Report, Row, Sample};

/// The property's timezone, or UTC when GA has not said or named one
/// chrono-tz does not know.
pub fn tz_of(name: &str) -> Tz {
    name.parse::<Tz>().unwrap_or(chrono_tz::UTC)
}

pub fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

/// Percentage change from `before` to `now`, or `None` when `before` is
/// zero and there is no ratio to take.
pub fn pct(now: f64, before: f64) -> Option<f64> {
    (before > 0.0).then(|| (now - before) / before * 100.0)
}

/// `+48%`, `−24%` (a real minus sign), or `0%`.
pub fn pct_label(p: f64) -> String {
    let r = p.round();
    if r > 0.0 {
        format!("+{:.0}%", r)
    } else if r < 0.0 {
        format!("\u{2212}{:.0}%", -r)
    } else {
        "0%".into()
    }
}

/// `1,842`.
pub fn thousands(n: f64) -> String {
    let neg = n < 0.0;
    let digits = format!("{:.0}", n.abs());
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if neg {
        format!("\u{2212}{}", out)
    } else {
        out
    }
}

/// `1.2k`, `14k`, `200k` - the quota figures, which only need to be read
/// against each other.
pub fn short(n: f64) -> String {
    let n = n.max(0.0);
    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 10_000.0 {
        format!("{:.0}k", n / 1000.0)
    } else if n >= 1000.0 {
        format!("{:.1}k", n / 1000.0)
    } else {
        format!("{:.0}", n)
    }
}

/// The interval in force for realtime polling, and why it is not the one
/// chosen.
///
/// Two reasons can stretch it, and the title names whichever applies.
/// Under a tenth of a token bucket left, the widget slows to 120 s - at
/// the defaults it spends roughly 0.8-2.5k tokens an hour per property
/// against GA's 14k, so reading the quota and backing off is what keeps a
/// wall display from emptying it. After five minutes at zero active users
/// it backs off to 60 s; it comes back the first poll anybody is there.
pub fn realtime_interval(chosen: u64, idle_for: f64, quota_left: Option<f64>) -> (u64, String) {
    if quota_left.is_some_and(|f| f < 0.10) {
        return (chosen.max(120), "slowed: under 10% of quota left".into());
    }
    if idle_for >= 300.0 && chosen < 60 {
        return (60, "backed off: 0 active for 5m+".into());
    }
    (chosen, String::new())
}

/// The realtime interval config asks for: 15, 30 or 60 seconds, and 15
/// for anything else.
pub fn realtime_choice(asked: f64) -> u64 {
    match asked.round() as i64 {
        30 => 30,
        60 => 60,
        _ => 15,
    }
}

/// The realtime intervals `l` cycles through.
pub const LIVE_CHOICES: &[u64] = &[15, 30, 60];

/// The windows `w` cycles through, in days.
pub const WINDOW_CHOICES: &[usize] = &[7, 14, 28];

/// What the baseline row says.
#[derive(Clone, Debug, PartialEq)]
pub enum Baseline {
    /// No sample is a week old yet, so no past week can be compared.
    Collecting { since: f64 },
    /// Weeks exist, but none has a sample near this minute of the week:
    /// the widget was not running then.
    Uncovered { label: String },
    /// The median of `found` weeks out of the `of` asked for. `since` is
    /// set while the history is shorter than `of` weeks.
    Ready { median: f64, found: usize, of: usize, since: Option<f64> },
}

/// How close a past sample has to be to the same minute of the week.
pub const BASELINE_SLACK: f64 = 300.0;

/// The same local minute `k` weeks before `now`, as a Unix time.
///
/// Local, so a week that crossed a daylight-saving change still compares
/// 14:35 with 14:35 rather than with 13:35.
pub fn weeks_back(now: f64, k: i64, tz: Tz) -> f64 {
    let Some(at) = tz.timestamp_opt(now.floor() as i64, 0).single() else {
        return now - k as f64 * 604_800.0;
    };
    let naive = at.naive_local() - chrono::Duration::weeks(k);
    match tz.from_local_datetime(&naive).earliest().or_else(|| tz.from_local_datetime(&naive).latest()) {
        Some(t) => t.timestamp() as f64 + now.fract(),
        // A local time that does not exist (inside a spring-forward gap)
        // falls back to plain arithmetic, an hour off for that minute only.
        None => now - k as f64 * 604_800.0,
    }
}

/// The sample nearest `target`, if one is within `BASELINE_SLACK`.
fn nearest(samples: &[Sample], target: f64) -> Option<&Sample> {
    let at = samples.partition_point(|s| s.t < target);
    let mut best: Option<&Sample> = None;
    for i in [at.checked_sub(1), Some(at)].into_iter().flatten() {
        if let Some(s) = samples.get(i) {
            if (s.t - target).abs() <= BASELINE_SLACK
                && best.is_none_or(|b| (s.t - target).abs() < (b.t - target).abs())
            {
                best = Some(s);
            }
        }
    }
    best
}

/// The baseline at `now`: the median of `pick` over the past `weeks` weeks
/// at this minute of the week, using only weeks with a sample within five
/// minutes of it. `samples` must be sorted by time.
pub fn baseline(
    samples: &[Sample],
    now: f64,
    weeks: usize,
    tz: Tz,
    pick: &dyn Fn(&Sample) -> f64,
) -> Baseline {
    let Some(first) = samples.first().map(|s| s.t) else {
        return Baseline::Collecting { since: now };
    };
    if first > weeks_back(now, 1, tz) + BASELINE_SLACK {
        return Baseline::Collecting { since: first };
    }
    let found: Vec<f64> = (1..=weeks as i64)
        .filter_map(|k| nearest(samples, weeks_back(now, k, tz)))
        .map(pick)
        .collect();
    if found.is_empty() {
        let label = tz
            .timestamp_opt(now.floor() as i64, 0)
            .single()
            .map(|t| t.format("%a %H:%M").to_string())
            .unwrap_or_default();
        return Baseline::Uncovered { label };
    }
    let short = first > weeks_back(now, weeks as i64, tz) + BASELINE_SLACK;
    Baseline::Ready {
        median: median(found.clone()).unwrap_or(0.0),
        found: found.len(),
        of: weeks,
        since: short.then_some(first),
    }
}

/// Samples older than the baseline can reach, plus a week's grace.
pub fn prune(samples: &mut Vec<Sample>, now: f64, weeks: usize) {
    let keep_from = now - (weeks as f64 + 1.0) * 604_800.0;
    samples.retain(|s| s.t >= keep_from);
}

/// One metric by hour: today's (absent past the last processed hour) and
/// the same weekday 1, 2 and 3 weeks ago.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hourly {
    /// 24 slots; `None` is an hour GA has not processed, which is not zero.
    pub today: Vec<Option<f64>>,
    /// The same weekday 1, 2 and 3 weeks back; `None` for a range GA
    /// returned nothing for at all (a property newer than that).
    pub weeks: Vec<Option<Vec<f64>>>,
    /// The last hour of today GA has processed.
    pub last: Option<usize>,
}

/// The date ranges the hourly report asks for, by name.
pub const HOURLY_RANGES: &[&str] = &["today", "w1", "w2", "w3"];

fn hour_of(s: &str) -> Option<usize> {
    s.parse::<usize>().ok().filter(|h| *h < 24)
}

/// Read the hourly report: `dims [hour, dateRange]`.
///
/// GA leaves out an hour with nothing in it, so a missing hour *before*
/// the last processed one is a real zero, and every hour *after* it is
/// unprocessed - drawn hatched, never as zero.
pub fn hourly(report: &Report, metric: &str) -> Hourly {
    let mut today = vec![0.0; 24];
    let mut seen_today = false;
    let mut last: Option<usize> = None;
    let mut weeks: Vec<Option<Vec<f64>>> = vec![None; 3];
    for row in &report.rows {
        let Some(h) = hour_of(report.d(row, "hour")) else { continue };
        let v = report.m(row, metric);
        match report.d(row, "dateRange") {
            "today" => {
                today[h] += v;
                seen_today = true;
                last = Some(last.map_or(h, |l| l.max(h)));
            }
            name => {
                if let Some(i) = HOURLY_RANGES.iter().position(|r| *r == name).filter(|i| *i > 0) {
                    weeks[i - 1].get_or_insert_with(|| vec![0.0; 24])[h] += v;
                }
            }
        }
    }
    let today = (0..24)
        .map(|h| match last {
            Some(l) if seen_today && h <= l => Some(today[h]),
            _ => None,
        })
        .collect();
    Hourly { today, weeks, last }
}

pub fn cumulative(v: &[f64], upto: usize) -> f64 {
    v.iter().take(upto + 1).sum()
}

/// Today against the same weekday, both cumulative to the last processed
/// hour.
#[derive(Clone, Debug, PartialEq)]
pub struct Versus {
    pub upto: usize,
    pub today: f64,
    pub last: Option<f64>,
    /// The median of the weeks GA returned, and how many that was.
    pub median: Option<(f64, usize)>,
}

pub fn versus(h: &Hourly) -> Option<Versus> {
    let upto = h.last?;
    let today = h.today.iter().take(upto + 1).map(|v| v.unwrap_or(0.0)).sum();
    let past: Vec<f64> = h.weeks.iter().flatten().map(|w| cumulative(w, upto)).collect();
    Some(Versus {
        upto,
        today,
        last: h.weeks.first().and_then(|w| w.as_ref()).map(|w| cumulative(w, upto)),
        median: median(past.clone()).map(|m| (m, past.len())),
    })
}

/// One name's total today and on the same weekday last week.
#[derive(Clone, Debug, PartialEq)]
pub struct Split {
    pub name: String,
    pub today: f64,
    pub last: f64,
}

/// A report with `dims [<dim>, …, hour, dateRange]` over `today` and `w1`,
/// summed per `dim` up to and including hour `upto`, largest today first.
pub fn split_to_hour(report: &Report, dim: &str, metric: &str, upto: usize) -> Vec<Split> {
    let mut found: Vec<Split> = Vec::new();
    for row in &report.rows {
        let Some(h) = hour_of(report.d(row, "hour")) else { continue };
        if h > upto {
            continue;
        }
        let name = report.d(row, dim).to_string();
        let v = report.m(row, metric);
        let at = match found.iter().position(|s| s.name == name) {
            Some(at) => at,
            None => {
                found.push(Split { name, today: 0.0, last: 0.0 });
                found.len() - 1
            }
        };
        match report.d(row, "dateRange") {
            "today" => found[at].today += v,
            "w1" => found[at].last += v,
            _ => {}
        }
    }
    found.sort_by(|a, b| b.today.total_cmp(&a.today).then(b.last.total_cmp(&a.last)).then(a.name.cmp(&b.name)));
    found
}

/// One key event: today to the last processed hour, the same weekday last
/// week to the same hour, and the last seven days.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyEventLine {
    pub name: String,
    pub today: f64,
    pub last: f64,
    /// Seven days ending today; today's is partial and drawn hatched.
    pub days: Vec<f64>,
}

/// Read the key event report: `dims [date, hour, eventName]` over the
/// eight days ending today, one row per (day, hour, event) with any.
///
/// Every name in `names` gets a row, so an event with none today reads
/// `0` rather than vanishing. `metric` is `keyEvents` for the events GA
/// flags and `eventCount` for a list named in config, which need not be
/// flagged.
pub fn key_events(
    report: &Report,
    names: &[String],
    today: NaiveDate,
    upto: Option<usize>,
    metric: &str,
) -> Vec<KeyEventLine> {
    let mut out: Vec<KeyEventLine> = names
        .iter()
        .map(|n| KeyEventLine { name: n.clone(), today: 0.0, last: 0.0, days: vec![0.0; 7] })
        .collect();
    let last_week = today - chrono::Duration::days(7);
    for row in &report.rows {
        let name = report.d(row, "eventName");
        if name.is_empty() {
            continue;
        }
        let Ok(date) = NaiveDate::parse_from_str(report.d(row, "date"), "%Y%m%d") else { continue };
        let Some(h) = hour_of(report.d(row, "hour")) else { continue };
        let v = report.m(row, metric);
        let at = match out.iter().position(|l| l.name == name) {
            Some(at) => at,
            None => {
                out.push(KeyEventLine { name: name.to_string(), today: 0.0, last: 0.0, days: vec![0.0; 7] });
                out.len() - 1
            }
        };
        let back = (today - date).num_days();
        if (0..7).contains(&back) {
            out[at].days[6 - back as usize] += v;
        }
        if let Some(upto) = upto {
            if h <= upto {
                if date == today {
                    out[at].today += v;
                } else if date == last_week {
                    out[at].last += v;
                }
            }
        }
    }
    out
}

/// One page, channel or source over the window and the window before it.
#[derive(Clone, Debug, PartialEq)]
pub struct Mover {
    pub name: String,
    /// One value per metric asked for, in that order.
    pub cur: Vec<f64>,
    pub prev: Vec<f64>,
    /// `(YYYYMMDD, values)` for a report that carried `date`, sorted.
    pub daily: Vec<(String, Vec<f64>)>,
}

/// Read a two-window report (`dateRange` `cur` and `prev`), grouping rows
/// by `key`. `metrics` names the values kept, in order.
pub fn movers(report: &Report, key: &dyn Fn(&Report, &Row) -> String, metrics: &[&str]) -> Vec<Mover> {
    let mut out: Vec<Mover> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for row in &report.rows {
        let name = key(report, row);
        let at = *index.entry(name.clone()).or_insert_with(|| {
            out.push(Mover { name, cur: vec![0.0; metrics.len()], prev: vec![0.0; metrics.len()], daily: Vec::new() });
            out.len() - 1
        });
        let vals: Vec<f64> = metrics.iter().map(|m| report.m(row, m)).collect();
        let target = match report.d(row, "dateRange") {
            "cur" => &mut out[at].cur,
            "prev" => &mut out[at].prev,
            _ => continue,
        };
        for (t, v) in target.iter_mut().zip(&vals) {
            *t += v;
        }
        let date = report.d(row, "date");
        if !date.is_empty() {
            let daily = &mut out[at].daily;
            match daily.iter_mut().find(|(d, _)| d == date) {
                Some((_, acc)) => acc.iter_mut().zip(&vals).for_each(|(a, v)| *a += v),
                None => daily.push((date.to_string(), vals)),
            }
        }
    }
    for m in &mut out {
        m.daily.sort_by(|a, b| a.0.cmp(&b.0));
    }
    out
}

/// How the movers are ordered; `s` cycles them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    /// Largest absolute change first.
    Change,
    /// Largest in the current window first.
    Size,
    /// Largest percentage change first, `new` and `gone` at the top.
    Percent,
}

impl Sort {
    pub fn next(self) -> Sort {
        match self {
            Sort::Change => Sort::Size,
            Sort::Size => Sort::Percent,
            Sort::Percent => Sort::Change,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Sort::Change => "change",
            Sort::Size => "size",
            Sort::Percent => "percent",
        }
    }
}

/// Order `v` by metric `m` and drop names with nothing in either window.
pub fn sort_movers(v: &mut Vec<Mover>, m: usize, sort: Sort) {
    v.retain(|x| x.cur.get(m).copied().unwrap_or(0.0) > 0.0 || x.prev.get(m).copied().unwrap_or(0.0) > 0.0);
    let get = |x: &Mover| (x.cur.get(m).copied().unwrap_or(0.0), x.prev.get(m).copied().unwrap_or(0.0));
    v.sort_by(|a, b| {
        let (ac, ap) = get(a);
        let (bc, bp) = get(b);
        let order = match sort {
            Sort::Change => (bc - bp).abs().total_cmp(&(ac - ap).abs()),
            Sort::Size => bc.total_cmp(&ac),
            Sort::Percent => {
                let key = |c: f64, p: f64| pct(c, p).map(f64::abs).unwrap_or(f64::INFINITY);
                key(bc, bp).total_cmp(&key(ac, ap))
            }
        };
        order.then(bc.total_cmp(&ac)).then(a.name.cmp(&b.name))
    });
}

/// What the change column says: `new`, `gone`, or a percentage.
pub fn change_label(cur: f64, prev: f64) -> String {
    if prev <= 0.0 && cur > 0.0 {
        "new".into()
    } else if cur <= 0.0 && prev > 0.0 {
        "gone".into()
    } else {
        pct(cur, prev).map(pct_label).unwrap_or_else(|| "0%".into())
    }
}

/// The realtime totals: active users, views and key events, last 30 min.
pub fn realtime_totals(report: &Report) -> (f64, f64, f64) {
    match report.rows.first() {
        Some(row) => (
            report.m(row, "activeUsers"),
            report.m(row, "screenPageViews"),
            report.m(row, "keyEvents"),
        ),
        // Realtime leaves out an empty window entirely: nobody is there.
        None => (0.0, 0.0, 0.0),
    }
}

/// Per-minute series from `dims [minutesAgo]`, oldest first: 30 slots,
/// slot 29 being the minute in progress. Realtime is complete within
/// seconds, so a minute it leaves out is a real zero.
pub fn per_minute(report: &Report, metric: &str) -> Vec<f64> {
    let mut out = vec![0.0; 30];
    for row in &report.rows {
        if let Ok(ago) = report.d(row, "minutesAgo").parse::<usize>() {
            if ago < 30 {
                out[29 - ago] += report.m(row, metric);
            }
        }
    }
    out
}

/// The top `n` of a one-dimension realtime breakdown.
pub fn top(report: &Report, dim: &str, metric: &str, n: usize) -> Vec<(String, f64)> {
    let mut v: Vec<(String, f64)> = report
        .rows
        .iter()
        .map(|r| (report.d(r, dim).to_string(), report.m(r, metric)))
        .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(n);
    v
}

/// Key events in the realtime window, from `dims [eventName]` with
/// `eventCount` and `keyEvents`.
///
/// With `names` configured, those events' counts whatever GA flags; with
/// none, every event GA counts as a key event.
pub fn realtime_key_events(report: &Report, names: &[String]) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = Vec::new();
    if names.is_empty() {
        for row in &report.rows {
            let n = report.m(row, "keyEvents");
            if n > 0.0 {
                out.push((report.d(row, "eventName").to_string(), n));
            }
        }
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    } else {
        for name in names {
            let n = report
                .rows
                .iter()
                .filter(|r| report.d(r, "eventName") == name)
                .map(|r| report.m(r, "eventCount"))
                .sum();
            out.push((name.clone(), n));
        }
    }
    out
}

/// The property's local date and hour now.
pub fn local_now(now: f64, tz: Tz) -> (NaiveDate, usize, String) {
    match tz.timestamp_opt(now.floor() as i64, 0).single() {
        Some(t) => (t.date_naive(), chrono::Timelike::hour(&t) as usize, t.format("%a").to_string()),
        None => (NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_default(), 0, String::new()),
    }
}

/// The weekday's short name for a date.
pub fn weekday(d: NaiveDate) -> String {
    d.weekday().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse_batch, parse_report, Sample};

    fn sample(t: f64, active: f64) -> Sample {
        Sample { t, active, views: active * 2.0, events: vec![("purchase".into(), active / 10.0)] }
    }

    const WEEK: f64 = 604_800.0;
    // 2026-10-20 14:35:00 UTC, a Tuesday.
    const NOW: f64 = 1_792_506_900.0;

    #[test]
    fn the_baseline_is_the_median_of_the_weeks_that_were_recorded() {
        let samples = vec![
            sample(NOW - 4.0 * WEEK + 60.0, 40.0),
            sample(NOW - 3.0 * WEEK - 120.0, 50.0),
            sample(NOW - 2.0 * WEEK, 48.0),
            sample(NOW - WEEK + 299.0, 52.0),
        ];
        let b = baseline(&samples, NOW, 4, chrono_tz::UTC, &|s| s.active);
        assert_eq!(b, Baseline::Ready { median: 49.0, found: 4, of: 4, since: None });
    }

    #[test]
    fn a_gap_lowers_the_coverage_rather_than_inventing_a_week() {
        // Running in weeks 1 and 3 only; week 2 has a sample, but 6
        // minutes off, and week 4 has none near this minute at all.
        let samples = vec![
            sample(NOW - 4.0 * WEEK - 3600.0, 999.0),
            sample(NOW - 3.0 * WEEK, 30.0),
            sample(NOW - 2.0 * WEEK + 360.0, 999.0),
            sample(NOW - WEEK - 10.0, 50.0),
        ];
        let b = baseline(&samples, NOW, 4, chrono_tz::UTC, &|s| s.active);
        assert_eq!(b, Baseline::Ready { median: 40.0, found: 2, of: 4, since: None });
    }

    #[test]
    fn no_week_covering_this_minute_says_so() {
        let samples = vec![sample(NOW - 2.0 * WEEK - 7200.0, 10.0), sample(NOW - WEEK + 3600.0, 10.0)];
        let b = baseline(&samples, NOW, 4, chrono_tz::UTC, &|s| s.active);
        assert_eq!(b, Baseline::Uncovered { label: "Tue 14:35".into() });
    }

    #[test]
    fn under_a_week_of_samples_is_still_collecting() {
        let since = NOW - 3.0 * 86_400.0;
        let b = baseline(&[sample(since, 10.0), sample(NOW - 60.0, 12.0)], NOW, 4, chrono_tz::UTC, &|s| s.active);
        assert_eq!(b, Baseline::Collecting { since });
        assert_eq!(baseline(&[], NOW, 4, chrono_tz::UTC, &|s| s.active), Baseline::Collecting { since: NOW });
    }

    #[test]
    fn a_short_history_says_since_when() {
        let first = NOW - WEEK - 60.0;
        let b = baseline(&[sample(first, 20.0)], NOW, 4, chrono_tz::UTC, &|s| s.active);
        assert_eq!(b, Baseline::Ready { median: 20.0, found: 1, of: 4, since: Some(first) });
    }

    #[test]
    fn a_week_across_a_clock_change_compares_the_same_local_minute() {
        // 2026-11-03 14:35 in New York is after the clocks went back on
        // 2026-11-01, so a week earlier is 25 hours further than 7 * 24.
        let tz: Tz = "America/New_York".parse().unwrap();
        let now = tz.with_ymd_and_hms(2026, 11, 3, 14, 35, 0).unwrap().timestamp() as f64;
        let then = tz.with_ymd_and_hms(2026, 10, 27, 14, 35, 0).unwrap().timestamp() as f64;
        assert_eq!(weeks_back(now, 1, tz), then);
        assert_eq!(now - then, WEEK + 3600.0);
    }

    #[test]
    fn key_event_baselines_read_zero_for_a_week_without_that_event() {
        let mut quiet = sample(NOW - WEEK, 10.0);
        quiet.events.clear();
        let b = baseline(&[quiet], NOW, 1, chrono_tz::UTC, &|s| {
            s.events.iter().find(|(n, _)| n == "purchase").map(|e| e.1).unwrap_or(0.0)
        });
        assert_eq!(b, Baseline::Ready { median: 0.0, found: 1, of: 1, since: None });
    }

    #[test]
    fn pruning_keeps_one_week_past_the_baseline() {
        let mut v = vec![sample(NOW - 6.0 * WEEK, 1.0), sample(NOW - 5.0 * WEEK + 1.0, 1.0), sample(NOW, 1.0)];
        prune(&mut v, NOW, 4);
        assert_eq!(v.len(), 2);
    }

    fn hourly_fixture() -> Report {
        // Today processed to 02:00 with a genuine zero at 01:00; last week
        // complete; two weeks ago only hour 0; three weeks ago absent.
        parse_report(r#"{"dimensionHeaders":[{"name":"hour"},{"name":"dateRange"}],
          "metricHeaders":[{"name":"sessions"}],
          "rows":[
            {"dimensionValues":[{"value":"00"},{"value":"today"}],"metricValues":[{"value":"10"}]},
            {"dimensionValues":[{"value":"02"},{"value":"today"}],"metricValues":[{"value":"30"}]},
            {"dimensionValues":[{"value":"00"},{"value":"w1"}],"metricValues":[{"value":"8"}]},
            {"dimensionValues":[{"value":"01"},{"value":"w1"}],"metricValues":[{"value":"4"}]},
            {"dimensionValues":[{"value":"02"},{"value":"w1"}],"metricValues":[{"value":"20"}]},
            {"dimensionValues":[{"value":"03"},{"value":"w1"}],"metricValues":[{"value":"99"}]},
            {"dimensionValues":[{"value":"00"},{"value":"w2"}],"metricValues":[{"value":"16"}]}
          ]}"#)
        .unwrap()
    }

    #[test]
    fn an_unprocessed_hour_is_absent_and_a_quiet_hour_is_zero() {
        let h = hourly(&hourly_fixture(), "sessions");
        assert_eq!(h.last, Some(2));
        assert_eq!(h.today[0], Some(10.0));
        assert_eq!(h.today[1], Some(0.0), "an hour before the last processed one is a real zero");
        assert_eq!(h.today[2], Some(30.0));
        assert!(h.today[3..].iter().all(Option::is_none), "every hour after it is unprocessed, not zero");
        assert!(h.weeks[0].is_some() && h.weeks[1].is_some());
        assert!(h.weeks[2].is_none(), "a week GA returned nothing for is absent");
    }

    #[test]
    fn nothing_processed_today_is_not_a_day_of_zeros() {
        let r = parse_report(r#"{"dimensionHeaders":[{"name":"hour"},{"name":"dateRange"}],"metricHeaders":[{"name":"sessions"}],
          "rows":[{"dimensionValues":[{"value":"05"},{"value":"w1"}],"metricValues":[{"value":"3"}]}]}"#).unwrap();
        let h = hourly(&r, "sessions");
        assert_eq!(h.last, None);
        assert!(h.today.iter().all(Option::is_none));
        assert_eq!(versus(&h), None);
    }

    #[test]
    fn the_comparison_stops_at_the_same_hour_on_both_sides() {
        let v = versus(&hourly(&hourly_fixture(), "sessions")).unwrap();
        assert_eq!(v.upto, 2);
        assert_eq!(v.today, 40.0);
        assert_eq!(v.last, Some(32.0), "last week's 03:00 is left out");
        // w1 to 02:00 is 32, w2 is 16: the median of two weeks.
        assert_eq!(v.median, Some((24.0, 2)));
        assert_eq!(pct(v.today, v.last.unwrap()).map(pct_label).as_deref(), Some("+25%"));
    }

    #[test]
    fn sources_are_split_to_the_last_processed_hour() {
        let r = parse_report(r#"{"dimensionHeaders":[{"name":"sessionDefaultChannelGroup"},{"name":"sessionSourceMedium"},{"name":"hour"},{"name":"dateRange"}],
          "metricHeaders":[{"name":"sessions"}],
          "rows":[
            {"dimensionValues":[{"value":"Organic Search"},{"value":"google / organic"},{"value":"01"},{"value":"today"}],"metricValues":[{"value":"5"}]},
            {"dimensionValues":[{"value":"Organic Search"},{"value":"google / organic"},{"value":"09"},{"value":"today"}],"metricValues":[{"value":"500"}]},
            {"dimensionValues":[{"value":"Direct"},{"value":"(direct) / (none)"},{"value":"02"},{"value":"today"}],"metricValues":[{"value":"7"}]},
            {"dimensionValues":[{"value":"Organic Search"},{"value":"google / organic"},{"value":"02"},{"value":"w1"}],"metricValues":[{"value":"4"}]},
            {"dimensionValues":[{"value":"Organic Search"},{"value":"bing / organic"},{"value":"02"},{"value":"w1"}],"metricValues":[{"value":"2"}]}
          ]}"#).unwrap();
        let sources = split_to_hour(&r, "sessionSourceMedium", "sessions", 2);
        assert_eq!(sources[0], Split { name: "(direct) / (none)".into(), today: 7.0, last: 0.0 });
        assert_eq!(sources[1], Split { name: "google / organic".into(), today: 5.0, last: 4.0 });
        assert_eq!(sources[2], Split { name: "bing / organic".into(), today: 0.0, last: 2.0 });
        let channels = split_to_hour(&r, "sessionDefaultChannelGroup", "sessions", 2);
        assert_eq!(channels[0], Split { name: "Direct".into(), today: 7.0, last: 0.0 });
        assert_eq!(channels[1], Split { name: "Organic Search".into(), today: 5.0, last: 6.0 });
    }

    #[test]
    fn key_events_compare_to_the_hour_and_keep_a_configured_zero() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let r = parse_report(r#"{"dimensionHeaders":[{"name":"date"},{"name":"hour"},{"name":"eventName"}],
          "metricHeaders":[{"name":"keyEvents"}],
          "rows":[
            {"dimensionValues":[{"value":"20261020"},{"value":"09"},{"value":"purchase"}],"metricValues":[{"value":"3"}]},
            {"dimensionValues":[{"value":"20261013"},{"value":"09"},{"value":"purchase"}],"metricValues":[{"value":"4"}]},
            {"dimensionValues":[{"value":"20261013"},{"value":"15"},{"value":"purchase"}],"metricValues":[{"value":"40"}]},
            {"dimensionValues":[{"value":"20261019"},{"value":"12"},{"value":"purchase"}],"metricValues":[{"value":"6"}]},
            {"dimensionValues":[{"value":"20261013"},{"value":"10"},{"value":"sign_up"}],"metricValues":[{"value":"31"}]}
          ]}"#).unwrap();
        let lines = key_events(&r, &["purchase".into(), "sign_up".into(), "refund".into()], today, Some(10), "keyEvents");
        assert_eq!(lines[0].name, "purchase");
        assert_eq!((lines[0].today, lines[0].last), (3.0, 4.0), "last week's 15:00 is past today's last hour");
        assert_eq!(lines[0].days, vec![0.0, 0.0, 0.0, 0.0, 0.0, 6.0, 3.0]);
        assert_eq!((lines[1].today, lines[1].last), (0.0, 31.0), "0 today, 31 last week - not a missing row");
        assert_eq!(lines[2].name, "refund");
        assert_eq!((lines[2].today, lines[2].last), (0.0, 0.0));
        let unprocessed = key_events(&r, &["purchase".into()], today, None, "keyEvents");
        assert_eq!((unprocessed[0].today, unprocessed[0].last), (0.0, 0.0));
    }

    fn movers_fixture() -> Report {
        parse_batch(r#"{"reports":[{"dimensionHeaders":[{"name":"pagePath"},{"name":"date"},{"name":"dateRange"}],
          "metricHeaders":[{"name":"screenPageViews"},{"name":"totalUsers"},{"name":"sessions"}],
          "rows":[
            {"dimensionValues":[{"value":"/pricing"},{"value":"20261019"},{"value":"cur"}],"metricValues":[{"value":"9000"},{"value":"10"},{"value":"5"}]},
            {"dimensionValues":[{"value":"/pricing"},{"value":"20261018"},{"value":"cur"}],"metricValues":[{"value":"120"},{"value":"1"},{"value":"1"}]},
            {"dimensionValues":[{"value":"/pricing"},{"value":"20261010"},{"value":"prev"}],"metricValues":[{"value":"6480"},{"value":"9"},{"value":"4"}]},
            {"dimensionValues":[{"value":"/docs/getting-started"},{"value":"20261019"},{"value":"cur"}],"metricValues":[{"value":"2210"},{"value":"300"},{"value":"200"}]},
            {"dimensionValues":[{"value":"/docs/getting-started"},{"value":"20261011"},{"value":"prev"}],"metricValues":[{"value":"2890"},{"value":"350"},{"value":"260"}]},
            {"dimensionValues":[{"value":"/blog/launch"},{"value":"20261019"},{"value":"cur"}],"metricValues":[{"value":"890"},{"value":"80"},{"value":"60"}]},
            {"dimensionValues":[{"value":"/old"},{"value":"20261011"},{"value":"prev"}],"metricValues":[{"value":"50"},{"value":"5"},{"value":"5"}]}
          ]}]}"#).unwrap().remove(0)
    }

    #[test]
    fn movers_sum_each_window_and_name_new_and_gone() {
        let mut v = movers(&movers_fixture(), &|r, row| r.d(row, "pagePath").to_string(), &["screenPageViews", "totalUsers", "sessions"]);
        sort_movers(&mut v, 0, Sort::Change);
        let names: Vec<&str> = v.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["/pricing", "/blog/launch", "/docs/getting-started", "/old"]);
        assert_eq!(v[0].cur[0], 9120.0);
        assert_eq!(v[0].daily.len(), 3);
        assert_eq!(v[0].daily[0].0, "20261010", "days sorted oldest first");
        assert_eq!(change_label(v[0].cur[0], v[0].prev[0]), "+41%");
        assert_eq!(change_label(v[1].cur[0], v[1].prev[0]), "new");
        assert_eq!(change_label(v[2].cur[0], v[2].prev[0]), "\u{2212}24%");
        assert_eq!(change_label(v[3].cur[0], v[3].prev[0]), "gone");
        sort_movers(&mut v, 0, Sort::Size);
        assert_eq!(v[0].name, "/pricing");
        assert_eq!(v[3].name, "/old");
        sort_movers(&mut v, 0, Sort::Percent);
        assert!(v[0].name == "/blog/launch", "new outranks any percentage: {:?}", v[0].name);
        // By users, the docs page is now the largest mover.
        sort_movers(&mut v, 1, Sort::Size);
        assert_eq!(v[0].name, "/docs/getting-started");
    }

    #[test]
    fn realtime_reads_totals_minutes_and_breakdowns() {
        let totals = parse_report(r#"{"dimensionHeaders":[],"metricHeaders":[{"name":"activeUsers"},{"name":"screenPageViews"},{"name":"keyEvents"}],
          "rows":[{"metricValues":[{"value":"71"},{"value":"540"},{"value":"4"}]}]}"#).unwrap();
        assert_eq!(realtime_totals(&totals), (71.0, 540.0, 4.0));
        let empty = parse_report(r#"{"metricHeaders":[{"name":"activeUsers"}]}"#).unwrap();
        assert_eq!(realtime_totals(&empty), (0.0, 0.0, 0.0), "an empty realtime window is nobody there");
        let minutes = parse_report(r#"{"dimensionHeaders":[{"name":"minutesAgo"}],"metricHeaders":[{"name":"activeUsers"}],
          "rows":[{"dimensionValues":[{"value":"00"}],"metricValues":[{"value":"5"}]},{"dimensionValues":[{"value":"29"}],"metricValues":[{"value":"2"}]},{"dimensionValues":[{"value":"45"}],"metricValues":[{"value":"9"}]}]}"#).unwrap();
        let series = per_minute(&minutes, "activeUsers");
        assert_eq!(series.len(), 30);
        assert_eq!((series[0], series[29], series[15]), (2.0, 5.0, 0.0));
        let events = parse_report(r#"{"dimensionHeaders":[{"name":"eventName"}],"metricHeaders":[{"name":"eventCount"},{"name":"keyEvents"}],
          "rows":[{"dimensionValues":[{"value":"page_view"}],"metricValues":[{"value":"500"},{"value":"0"}]},
                  {"dimensionValues":[{"value":"purchase"}],"metricValues":[{"value":"3"},{"value":"3"}]},
                  {"dimensionValues":[{"value":"sign_up"}],"metricValues":[{"value":"1"},{"value":"1"}]}]}"#).unwrap();
        assert_eq!(realtime_key_events(&events, &[]), vec![("purchase".to_string(), 3.0), ("sign_up".to_string(), 1.0)]);
        assert_eq!(
            realtime_key_events(&events, &["page_view".into(), "refund".into()]),
            vec![("page_view".to_string(), 500.0), ("refund".to_string(), 0.0)]
        );
        assert_eq!(top(&events, "eventName", "eventCount", 1), vec![("page_view".to_string(), 500.0)]);
    }

    #[test]
    fn the_interval_backs_off_when_idle_and_slows_on_quota() {
        assert_eq!(realtime_interval(15, 0.0, Some(0.5)), (15, String::new()));
        assert_eq!(realtime_interval(15, 299.0, None), (15, String::new()));
        assert_eq!(realtime_interval(15, 300.0, None).0, 60);
        assert!(realtime_interval(30, 900.0, None).1.contains("0 active for 5m+"));
        assert_eq!(realtime_interval(60, 900.0, None), (60, String::new()), "already at 60, nothing to say");
        let (secs, why) = realtime_interval(15, 0.0, Some(0.09));
        assert_eq!(secs, 120);
        assert!(why.contains("under 10% of quota"));
        assert_eq!(realtime_interval(15, 900.0, Some(0.05)).0, 120, "quota wins over idle");
    }

    #[test]
    fn the_realtime_choice_is_fifteen_thirty_or_sixty() {
        assert_eq!(realtime_choice(30.0), 30);
        assert_eq!(realtime_choice(60.0), 60);
        assert_eq!(realtime_choice(15.0), 15);
        assert_eq!(realtime_choice(45.0), 15);
        assert_eq!(realtime_choice(0.0), 15);
        assert_eq!(realtime_choice(-30.0), 15);
    }

    #[test]
    fn numbers_read_the_way_the_screen_writes_them() {
        assert_eq!(thousands(1842.0), "1,842");
        assert_eq!(thousands(212.0), "212");
        assert_eq!(thousands(1_000_000.0), "1,000,000");
        assert_eq!(short(1200.0), "1.2k");
        assert_eq!(short(14000.0), "14k");
        assert_eq!(short(200000.0), "200k");
        assert_eq!(short(31.0), "31");
        assert_eq!(pct_label(-40.4), "\u{2212}40%");
        assert_eq!(median(vec![]), None);
        assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
    }
}
