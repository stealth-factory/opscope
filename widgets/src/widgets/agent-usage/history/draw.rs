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

//! The HISTORY section of a provider tab (OPS-306, spec §7): one chart per
//! pool, and under each the text lines that say everything the chart does.
//!
//! Pure: [`section`] takes what [`super::chart::gather`] read and a width,
//! and returns rows and the plots placed on them. Core draws the plots -
//! pixels where the terminal takes them, block characters otherwise - and
//! the text lines are written so that a reader with neither loses no fact:
//! every mark a chart carries (a reset letter, a hollow `·`, an open top
//! `┆`, a plan divider `│`, a second shade past one allowance, spend past
//! it) is counted or named in the lines below it.

use opscope_core as tc;

use super::chart::{Body, CycleBar, DayPeak, Mark, PoolHistory, SpendBar, TabHistory};
use super::fold::{Cycle, EndReason};
use super::overage::read::MonthTotal;
use crate::Palette;

const DAY: f64 = 86_400.0;

/// Rows of a percentage chart's scale between 0 and 100.
const PCT_STEP_ROWS: usize = 4;

/// The HISTORY section: a line saying why there is none, or a chart per
/// pool. `note` is the recorder's reason the file is not being written (a
/// newer file left alone, a damaged one set aside); `notes` are this tab's
/// own words about where its history came from (the Codex backfill).
pub fn section(
    tab: Option<&TabHistory>,
    note: Option<&str>,
    notes: Option<&str>,
    w: usize,
    p: &Palette,
    plots: &mut Vec<tc::Plot>,
) -> Vec<String> {
    let room = w.saturating_sub(1).max(1);
    let mut rows = Vec::new();
    let extra = |rows: &mut Vec<String>| {
        if let Some(n) = notes {
            wrapped(rows, n, room, &p.dim, 1);
        }
    };
    if let Some(n) = note {
        // One line instead of charts: a file this pane is not reading has
        // nothing in it to draw, and empty charts would read as idle pools.
        wrapped(&mut rows, n, room, &p.warn, 1);
        extra(&mut rows);
        return rows;
    }
    let pools = tab.map(|t| t.pools.as_slice()).unwrap_or_default();
    if pools.is_empty() {
        wrapped(
            &mut rows,
            "history: no chart for this account yet",
            room,
            &p.dim,
            1,
        );
        extra(&mut rows);
        return rows;
    }
    let expired = tab.map(|t| t.expired.as_slice()).unwrap_or_default();
    for (i, pool) in pools.iter().enumerate() {
        if i > 0 {
            rows.push(String::new());
        }
        tc::append_plot_rows(&mut rows, plots, |local| pool_rows(pool, expired, w, p, local));
    }
    extra(&mut rows);
    rows
}

fn pool_rows(pool: &PoolHistory, expired: &[(f64, u32)], w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    match &pool.body {
        Body::Cycles(bars) => cycles_rows(pool, bars, expired, w, p, plots),
        Body::Rolling(days) => rolling_rows(pool, days, w, p, plots),
        Body::Overage { bars, roll_day } => overage_rows(pool, bars, *roll_day, w, p, plots),
        Body::Balance(months) => balance_rows(pool, months, w, p, plots),
    }
}

// ----- formatting -----

fn local(at: f64) -> Option<chrono::DateTime<chrono::Local>> {
    use chrono::TimeZone;
    chrono::Local.timestamp_opt(at.trunc() as i64, 0).single()
}

/// `3 Sep`, or `3 Sep 2025` outside this year.
pub fn day_text(at: f64) -> String {
    use chrono::Datelike;
    match local(at) {
        Some(t) if t.year() == chrono::Local::now().year() => t.format("%-d %b").to_string(),
        Some(t) => t.format("%-d %b %Y").to_string(),
        None => "?".into(),
    }
}

/// `09-03`: the date under a chart's first and last bar.
fn short_date(at: f64) -> String {
    local(at).map_or_else(|| "?".into(), |t| t.format("%m-%d").to_string())
}

fn month_text(at: f64) -> String {
    local(at).map_or_else(|| "?".into(), |t| t.format("%b").to_string())
}

/// A span of time as one unit: `40m`, `5h`, `2d`.
pub fn span_text(secs: f64) -> String {
    let secs = secs.max(0.0);
    if secs >= 2.0 * DAY {
        format!("{:.0}d", secs / DAY)
    } else if secs >= 2.0 * 3600.0 {
        format!("{:.0}h", secs / 3600.0)
    } else {
        format!("{:.0}m", (secs / 60.0).max(1.0))
    }
}

fn pct_text(v: f64) -> String {
    format!("{:.0}%", v)
}

/// Nothing written as `-0`: a sum of float deltas that came back to zero
/// can land a hair under it.
fn no_negative_zero(v: f64) -> f64 {
    if v.abs() < 1e-9 { 0.0 } else { v }
}

/// An amount in its pool's unit: `A$12.40`, `14 requests`, `3.5 credits`.
pub fn amount_text(unit: &str, currency: Option<&str>, exponent: Option<i32>, v: f64) -> String {
    let v = no_negative_zero(v);
    match unit {
        "money" => {
            let places = exponent.unwrap_or(2).clamp(0, 4) as usize;
            format!("{}{:.p$}", crate::claude::cap_prefix(currency.unwrap_or("")), v, p = places)
        }
        "requests" => format!("{:.0} req", v),
        "tokens" => format!("{:.0} tokens", v.max(0.0)),
        _ => {
            if (v - v.round()).abs() < 0.05 {
                format!("{:.0} {unit}", v)
            } else {
                format!("{:.1} {unit}", v)
            }
        }
    }
}

/// An amount short enough for a chart's scale: whole units.
fn scale_text(unit: &str, currency: Option<&str>, v: f64) -> String {
    let v = no_negative_zero(v.max(0.0)) + 0.0;
    match unit {
        "money" => format!("{}{:.0}", crate::claude::cap_prefix(currency.unwrap_or("")), v.ceil()),
        _ => format!("{:.0}", v.ceil()),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `text` wrapped on its words into `room` cells, indented.
fn wrapped(rows: &mut Vec<String>, text: &str, room: usize, colour: &str, indent: usize) {
    let indent = if room > indent + 4 { indent } else { 0 };
    for line in tc::wrap_words(text, room - indent) {
        rows.push(tc::seg(&[(colour, format!("{}{line}", " ".repeat(indent)))], room));
    }
}

/// The pool's title: `── HISTORY ── overall 7d · 14 cycles since 3 Sep`,
/// its rules dropped and its words wrapped where the pane is too narrow.
fn title_rows(rows: &mut Vec<String>, label: &str, what: &str, room: usize, p: &Palette) {
    let head = format!(" ── HISTORY ── {label} · {what}");
    if tc::display_width(&head) <= room {
        rows.push(tc::seg(
            &[(p.lbl.as_str(), " ── HISTORY ── ".into()), (p.txt.as_str(), format!("{label} · {what}"))],
            room,
        ));
    } else {
        wrapped(rows, &format!("HISTORY {label} · {what}"), room, &p.txt, 1);
    }
}

fn since_words(n: usize, one: &str, many: &str, since: Option<f64>) -> String {
    match since {
        Some(at) => format!("{} since {}", plural(n, one, many), day_text(at)),
        None => plural(n, one, many),
    }
}

/// A scale label column `lw` wide, then the axis glyph.
fn gutter(label: &str, lw: usize, axis: &str, p: &Palette) -> Vec<(String, String)> {
    vec![(p.dim.clone(), format!(" {label:>lw$}")), (p.grid.clone(), axis.to_string())]
}

fn join_row(parts: Vec<(String, String)>, room: usize) -> String {
    let parts: Vec<(&str, String)> = parts.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
    tc::seg(&parts, room)
}

/// The dates under a chart `cells` wide: the first column's at the left and
/// the last's at the right, the last alone when both do not fit, neither
/// when that does not either.
fn date_row(first: &str, last: &str, cells: usize, lw: usize, p: &Palette) -> Option<(String, String)> {
    let lead = " ".repeat(lw + 2);
    let (a, b) = (tc::display_width(first), tc::display_width(last));
    if first != last && a + 1 + b <= cells {
        Some((p.dim.clone(), format!("{lead}{first}{}{last}", " ".repeat(cells - a - b))))
    } else if b <= cells {
        Some((p.dim.clone(), format!("{lead}{}{last}", " ".repeat(cells - b))))
    } else {
        None
    }
}

// ----- included pools: one bar per cycle -----

/// What a column of the cycle chart is.
#[derive(Clone, Debug, PartialEq)]
pub enum Slot {
    /// `bars[i]`.
    Cycle(usize),
    /// The plan changed between the cycles either side.
    Divider,
}

/// The newest cycles that fit in `cells`, oldest first, with a divider
/// where a cycle closed on a plan change and the next one is shown too.
pub fn fit(bars: &[CycleBar], cells: usize) -> Vec<Slot> {
    let mut out = Vec::new();
    let mut used = 0;
    for i in (0..bars.len()).rev() {
        let divider = i + 1 < bars.len()
            && out.last() == Some(&Slot::Cycle(i + 1))
            && bars[i].cycle.end_reason == Some(EndReason::PlanChanged);
        let need = 1 + divider as usize;
        if used + need > cells {
            break;
        }
        if divider {
            out.push(Slot::Divider);
        }
        out.push(Slot::Cycle(i));
        used += need;
    }
    out.reverse();
    out
}

/// A cycle's figure as a share of one allowance (§5.7): the sum of its
/// segment peaks, which passes 100 when a reset refilled it.
pub fn consumed(c: &Cycle) -> Option<f64> {
    c.consumed_pct.or(c.peak_pct)
}

/// Whether the figure drawn for a cycle is "at least" (§5.7, §5.8).
pub fn lower_bound(c: &Cycle) -> bool {
    !c.is_hollow() && (c.end_is_lower_bound || c.consumed_is_lower_bound)
}

/// How long before its end a cycle was last read.
fn unseen_before_end(c: &Cycle) -> Option<f64> {
    let end = c.closed_at.or(c.ends_at)?;
    Some(end - c.last_obs_at?)
}

/// The spend past the allowance shown in a run of cycles, per unit: each
/// span once, however many cycles it is shown against.
#[derive(Clone, Debug, PartialEq)]
pub struct OverTotal {
    pub unit: String,
    pub currency: Option<String>,
    pub total: f64,
    /// Cycles it was drawn in.
    pub cycles: usize,
    /// Spans whose bracket crossed a cycle boundary, shown against both.
    pub spans_reset: usize,
}

/// Everything the lines under a cycle chart say, worked out from the
/// cycles shown. Pure, so the arithmetic is tested on its own.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub shown: usize,
    /// Mean share of one allowance over the closed, observed cycles that
    /// ran their full length, and whether any of those is "at least".
    pub avg: Option<(f64, bool)>,
    pub averaged: usize,
    /// Cut short by a restarted reset, left out of the average (§5.7).
    pub cut_short: usize,
    /// Closed by a plan, limit or window change before their scheduled
    /// end: part of a period, so left out of the average as a cut-short
    /// one is.
    pub ended_early: usize,
    /// Reached 100%, and the median time from the cycle's start to that.
    pub hit: usize,
    pub median_to_hit: Option<f64>,
    /// The cycles' window, for whether that time reads in hours or days.
    pub window: Option<f64>,
    /// The highest share: (figure, at least, allowances, its resets).
    pub peak: Option<(f64, bool, u32, Vec<Mark>)>,
    /// Resets: banked, issued, unknown.
    pub resets: [usize; 3],
    pub expired: u32,
    pub over: Vec<OverTotal>,
    /// Cycles nobody saw, drawn hollow.
    pub unseen: usize,
    /// Cycles past one allowance, drawn in the second shade above 100.
    pub past_one: usize,
    /// Lower-bound cycles, and the newest one's figure and how long before
    /// its end it was last seen.
    pub lower: usize,
    pub latest_lower: Option<(f64, f64, Option<f64>)>,
    /// The open cycle's figure so far, never a total.
    pub open: Option<f64>,
    /// Plan changes between shown cycles: (from, to, when).
    pub plans: Vec<(String, String, f64)>,
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

/// Closed by a plan, limit or window change before the end it was
/// scheduled for: a partial period, which is no more comparable with a
/// whole one than a cycle a restart cut short. One whose scheduled end is
/// unknown is not called partial, since nothing says it was. A change that
/// lands on the scheduled end ran the full length and stays in the average.
fn ended_early(c: &Cycle) -> bool {
    matches!(
        c.end_reason,
        Some(EndReason::PlanChanged | EndReason::LimitChanged | EndReason::WindowChanged)
    ) && match (c.closed_at, c.ends_at) {
        (Some(closed), Some(end)) => closed < end - super::fold::tolerance(c.window_secs),
        _ => false,
    }
}

/// The figures the text lines carry, over `bars` (the cycles shown).
pub fn summarise(bars: &[&CycleBar], expired: &[(f64, u32)]) -> Summary {
    let mut s = Summary {
        shown: bars.len(),
        ..Summary::default()
    };
    let seen: Vec<&&CycleBar> = bars.iter().filter(|b| !b.cycle.is_hollow()).collect();
    s.unseen = bars.len() - seen.len();
    let full: Vec<&&CycleBar> = seen
        .iter()
        .copied()
        .filter(|b| b.cycle.closed_at.is_some() && !b.cycle.cut_short() && !ended_early(&b.cycle))
        .filter(|b| consumed(&b.cycle).is_some())
        .collect();
    s.cut_short = seen.iter().filter(|b| b.cycle.cut_short()).count();
    s.ended_early = seen.iter().filter(|b| ended_early(&b.cycle)).count();
    s.averaged = full.len();
    if !full.is_empty() {
        let sum: f64 = full.iter().filter_map(|b| consumed(&b.cycle)).sum();
        s.avg = Some((sum / full.len() as f64, full.iter().any(|b| lower_bound(&b.cycle))));
    }
    let hits: Vec<f64> = seen
        .iter()
        .filter_map(|b| Some(b.cycle.exhausted_at? - b.cycle.starts_at))
        .collect();
    s.hit = hits.len();
    s.median_to_hit = median(hits);
    s.window = seen.iter().rev().find_map(|b| b.cycle.window_secs);
    for b in &seen {
        let Some(v) = consumed(&b.cycle) else { continue };
        if s.peak.as_ref().is_none_or(|(best, ..)| v > *best) {
            let marks = b.resets.iter().map(|r| r.mark).collect();
            s.peak = Some((v, lower_bound(&b.cycle), b.cycle.allowances, marks));
        }
        if v > 100.0 + 1e-9 {
            s.past_one += 1;
        }
    }
    for b in bars {
        for r in &b.resets {
            s.resets[match r.mark {
                Mark::Banked => 0,
                Mark::Issued => 1,
                Mark::Unknown => 2,
            }] += 1;
        }
    }
    let from = bars.first().map_or(f64::INFINITY, |b| b.cycle.starts_at);
    s.expired = expired.iter().filter(|(at, _)| *at >= from).map(|(_, n)| n).sum();
    let mut counted: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();
    for b in bars {
        let mut in_cycle: Vec<usize> = Vec::new();
        for o in &b.over {
            let i = match s.over.iter().position(|t| t.unit == o.unit && t.currency == o.currency) {
                Some(i) => i,
                None => {
                    s.over.push(OverTotal {
                        unit: o.unit.clone(),
                        currency: o.currency.clone(),
                        total: 0.0,
                        cycles: 0,
                        spans_reset: 0,
                    });
                    s.over.len() - 1
                }
            };
            if !in_cycle.contains(&i) {
                in_cycle.push(i);
                s.over[i].cycles += 1;
            }
            if counted.insert(o.key.clone()) {
                s.over[i].total += o.amount;
                s.over[i].spans_reset += o.spans_reset as usize;
            }
        }
    }
    let lowers: Vec<&&CycleBar> = seen.iter().copied().filter(|b| lower_bound(&b.cycle)).collect();
    s.lower = lowers.len();
    s.latest_lower = lowers
        .last()
        .and_then(|b| Some((consumed(&b.cycle)?, b.cycle.starts_at, unseen_before_end(&b.cycle))));
    s.open = bars
        .last()
        .filter(|b| b.cycle.closed_at.is_none() && !b.cycle.is_hollow())
        .and_then(|b| consumed(&b.cycle));
    for pair in bars.windows(2) {
        let (a, b) = (&pair[0].cycle, &pair[1].cycle);
        if a.end_reason == Some(EndReason::PlanChanged) {
            let from = a.plan.clone().unwrap_or_else(|| "?".into());
            let to = b.plan.clone().unwrap_or_else(|| "?".into());
            s.plans.push((from, to, b.starts_at));
        }
    }
    s
}

fn marks_text(marks: &[Mark]) -> String {
    let n = |m: Mark| marks.iter().filter(|x| **x == m).count();
    let parts: Vec<String> = [(Mark::Banked, "banked"), (Mark::Issued, "issued"), (Mark::Unknown, "unknown")]
        .iter()
        .filter(|(m, _)| n(*m) > 0)
        .map(|(m, name)| format!("{} {name}", n(*m)))
        .collect();
    parts.join(", ")
}

/// The sentences under a cycle chart, one per line before wrapping.
pub fn summary_lines(s: &Summary) -> Vec<String> {
    let mut lines = Vec::new();
    let mut first = vec![format!("last {}:", s.shown)];
    match s.avg {
        Some((avg, at_least)) => {
            let mut left = vec![plural(s.averaged, "closed", "closed")];
            if s.cut_short > 0 {
                left.push(format!("{} cut short left out", s.cut_short));
            }
            if s.ended_early > 0 {
                left.push(format!("{} ended early left out", s.ended_early));
            }
            first.push(format!(
                "avg {}{} of allowance ({})",
                if at_least { "at least " } else { "" },
                pct_text(avg),
                left.join("; ")
            ));
        }
        None => {
            let mut note = "no full cycle closed yet".to_string();
            if s.ended_early > 0 {
                note.push_str(&format!(" ({} ended early left out)", s.ended_early));
            }
            first.push(note);
        }
    }
    let mut clauses = vec![first.join(" ")];
    if s.hit > 0 {
        let when = s.median_to_hit.map(|t| {
            if s.window.is_some_and(|w| w >= 2.0 * DAY) {
                format!(" (median day {:.1})", 1.0 + t / DAY)
            } else {
                format!(" (median {:.1}h in)", t / 3600.0)
            }
        });
        clauses.push(format!("{} hit 100%{}", s.hit, when.unwrap_or_default()));
    } else {
        clauses.push("none hit 100%".into());
    }
    if let Some((peak, at_least, allowances, marks)) = &s.peak {
        let mut text = format!("{}{} peak", if *at_least { "at least " } else { "" }, pct_text(*peak));
        if !marks.is_empty() {
            let kinds = marks_text(marks);
            text.push_str(&if marks.len() == 1 {
                format!(" with {kinds} reset")
            } else {
                format!(" with {} ({kinds})", plural(marks.len(), "reset", "resets"))
            });
        }
        if *allowances > 1 {
            text.push_str(&format!(
                " ({} of {} allowances)",
                pct_text(peak / *allowances as f64),
                allowances
            ));
        }
        clauses.push(text);
    }
    lines.push(clauses.join(" · "));

    let total: usize = s.resets.iter().sum();
    let mut second = vec![if total == 0 {
        "resets: none seen".to_string()
    } else {
        let marks: Vec<Mark> = [(Mark::Banked, 0), (Mark::Issued, 1), (Mark::Unknown, 2)]
            .iter()
            .flat_map(|(m, i)| std::iter::repeat_n(*m, s.resets[*i]))
            .collect();
        format!("resets ↺: {}", marks_text(&marks))
    }];
    if s.expired > 0 {
        second.push(format!("banked: {} expired unused", s.expired));
    }
    for o in &s.over {
        let mut text = format!(
            "over: {} in {}",
            amount_text(&o.unit, o.currency.as_deref(), None, o.total),
            plural(o.cycles, "cycle", "cycles")
        );
        if o.spans_reset > 0 {
            text.push_str(&format!(" ({} across a reset)", o.spans_reset));
        }
        second.push(text);
    }
    lines.push(second.join(" · "));

    let mut third = Vec::new();
    if let Some(v) = s.open {
        third.push(format!("now: {} so far", pct_text(v)));
    }
    if s.past_one > 0 {
        third.push(format!("{} past one allowance (upper shade)", s.past_one));
    }
    if s.cut_short > 0 {
        third.push(format!("{} cut short by a restart", s.cut_short));
    }
    if s.unseen > 0 {
        third.push(format!("{} unseen (·)", s.unseen));
    }
    if let Some((v, start, gap)) = s.latest_lower {
        let mut text = format!(
            "{} (┆): {}{} at least {}",
            plural(s.lower, "lower bound", "lower bounds"),
            if s.lower > 1 { "latest " } else { "" },
            short_date(start),
            pct_text(v)
        );
        if let Some(g) = gap {
            text.push_str(&format!(", last seen {} before reset", span_text(g)));
        }
        third.push(text);
    }
    if !third.is_empty() {
        lines.push(third.join(" · "));
    }
    for (from, to, at) in &s.plans {
        lines.push(format!("plan │: {from} → {to} from {}", day_text(*at)));
    }
    lines
}

fn cycles_rows(
    pool: &PoolHistory,
    bars: &[CycleBar],
    expired: &[(f64, u32)],
    w: usize,
    p: &Palette,
    plots: &mut Vec<tc::Plot>,
) -> Vec<String> {
    let room = w.saturating_sub(1).max(1);
    let mut rows = Vec::new();
    title_rows(&mut rows, &pool.label, &since_words(pool.total, "cycle", "cycles", pool.since), room, p);

    // The scale's widest label decides where the bars start; the bars that
    // fit decide the scale. Measured over every candidate first, so the
    // gutter never has to grow after the bars are chosen.
    let widest_money = bars
        .iter()
        .map(|b| b.over.iter().map(|o| o.amount).sum::<f64>())
        .fold(0.0, f64::max);
    let money_unit = bars.iter().flat_map(|b| b.over.first()).next();
    let money_label = money_unit.map(|o| scale_text(&o.unit, o.currency.as_deref(), widest_money));
    let lw = money_label.as_deref().map_or(0, tc::display_width).max(3);
    let cells = room.saturating_sub(lw + 2);
    let slots = fit(bars, cells);
    let shown: Vec<&CycleBar> = slots
        .iter()
        .filter_map(|s| match s {
            Slot::Cycle(i) => Some(&bars[*i]),
            Slot::Divider => None,
        })
        .collect();

    if !slots.is_empty() {
        // 0-100 in four rows; more rows of the same step for consumption
        // past one allowance; one more row for spend past it, in money.
        let most = shown.iter().filter_map(|b| consumed(&b.cycle)).fold(100.0, f64::max).min(800.0);
        let step = if most <= 200.0 {
            25.0
        } else if most <= 400.0 {
            50.0
        } else {
            100.0
        };
        let pct_rows = (most / step).ceil().max(PCT_STEP_ROWS as f64) as usize;
        let pct_axis = pct_rows as f64 * step;
        let unit = shown.iter().flat_map(|b| b.over.first()).next().map(|o| (o.unit.clone(), o.currency.clone()));
        let spend = |b: &CycleBar| -> f64 {
            b.over
                .iter()
                .filter(|o| unit.as_ref().is_some_and(|(u, c)| *u == o.unit && *c == o.currency))
                .map(|o| o.amount)
                .sum()
        };
        let most_spend = shown.iter().map(|b| spend(b)).fold(0.0, f64::max);
        let band = most_spend > 0.0;
        // The money row is a scale of its own, from its own floor at the top
        // of the percentages to the amount its label names - the largest
        // spend rounded up, as the spend chart is - so equal amounts stand
        // equally tall over any cycle, a seen one or a hollow one.
        let money_top = most_spend.ceil().max(f64::MIN_POSITIVE);
        let height = pct_rows + band as usize;
        let peak = pct_axis + if band { step } else { 0.0 };
        let columns: Vec<tc::Column> = slots
            .iter()
            .map(|s| match s {
                Slot::Divider => tc::Column::Divider(p.grid.clone()),
                Slot::Cycle(i) => {
                    let b = &bars[*i];
                    let use_of = if b.cycle.is_hollow() {
                        tc::Column::Hollow(p.dim.clone())
                    } else {
                        let v = consumed(&b.cycle).unwrap_or(0.0);
                        let mut parts = vec![(v.min(100.0), tc::heat(v.min(100.0) / 100.0))];
                        if v > 100.0 {
                            parts.push((v - 100.0, p.warn.clone()));
                        }
                        tc::Column::Stack {
                            parts,
                            open: lower_bound(&b.cycle).then(|| p.dim.clone()),
                        }
                    };
                    // Spend known for a cycle whose use nobody read is still
                    // drawn: the `·` says the allowance went unread, not that
                    // nothing was spent.
                    let money = spend(b);
                    if money > 0.0 {
                        tc::Column::Band {
                            base: Box::new(use_of),
                            from: pct_axis,
                            height: money / money_top * step,
                            colour: p.agent.clone(),
                        }
                    } else {
                        use_of
                    }
                }
            })
            .collect();
        let x = 1 + lw + 1;
        let plot = tc::Plot::stacked_columns(x, rows.len(), &columns, height, peak);
        for r in 0..height {
            let value = peak - r as f64 * step;
            let label = if band && r == 0 {
                unit.as_ref()
                    .map(|(u, c)| scale_text(u, c.as_deref(), money_top))
                    .unwrap_or_default()
            } else if r == usize::from(band) || value == 100.0 || value == 50.0 {
                format!("{:.0}", value)
            } else {
                String::new()
            };
            let mut parts = gutter(&label, lw, if label.is_empty() { "│" } else { "┤" }, p);
            if band && r == 0 {
                parts[0].0 = p.agent.clone();
            }
            parts.extend(plot.text_row(r).iter().cloned());
            rows.push(join_row(parts, room));
        }
        plots.push(plot);
        let mut axis = gutter("0", lw, "└", p);
        axis.push((p.grid.clone(), "─".repeat(slots.len())));
        rows.push(join_row(axis, room));
        if shown.iter().any(|b| !b.resets.is_empty()) {
            let mut marks = gutter("↺", lw, " ", p);
            for s in &slots {
                // One letter a cycle: a cut-short cycle's is the restart
                // that ended it, which its flag promises; otherwise the
                // first. Every mark is still counted in the lines.
                let mark = match s {
                    Slot::Cycle(i) => {
                        let b = &bars[*i];
                        let restart = b.resets.iter().find(|r| r.restarted).filter(|_| b.cycle.cut_short());
                        restart.or(b.resets.first()).map(|r| r.mark)
                    }
                    Slot::Divider => None,
                };
                let ink = match mark {
                    Some(Mark::Banked) => p.ok.clone(),
                    Some(Mark::Issued) => p.accent.clone(),
                    Some(Mark::Unknown) => p.dim.clone(),
                    None => String::new(),
                };
                let ch = mark.map_or(' ', Mark::letter);
                marks.push((ink, ch.to_string()));
            }
            rows.push(join_row(marks, room));
        }
        let first = shown.first().map(|b| short_date(b.cycle.starts_at)).unwrap_or_default();
        let last = shown.last().map(|b| short_date(b.cycle.starts_at)).unwrap_or_default();
        if let Some(d) = date_row(&first, &last, slots.len(), lw, p) {
            rows.push(join_row(vec![d], room));
        }
    }
    let all: Vec<&CycleBar> = if shown.is_empty() { bars.iter().collect() } else { shown };
    for line in summary_lines(&summarise(&all, expired)) {
        wrapped(&mut rows, &line, room, &p.dim, 2);
    }
    rows
}

// ----- rolling pools: a daily-peak line -----

fn rolling_rows(pool: &PoolHistory, days: &[DayPeak], w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let room = w.saturating_sub(1).max(1);
    let mut rows = Vec::new();
    title_rows(&mut rows, &pool.label, &format!("rolling · daily peak since {}", pool.since.map_or("?".into(), day_text)), room, p);
    let lw = 3;
    let cells = room.saturating_sub(lw + 2).min(days.len());
    let shown = &days[days.len() - cells..];
    if cells > 0 {
        let height = PCT_STEP_ROWS;
        let y0 = rows.len();
        for r in 0..height {
            let value = 100.0 - r as f64 * 25.0;
            let label = if value == 100.0 || value == 50.0 { format!("{value:.0}") } else { String::new() };
            let mut parts = gutter(&label, lw, if label.is_empty() { "│" } else { "┤" }, p);
            parts.push((String::new(), " ".repeat(cells)));
            rows.push(join_row(parts, room));
        }
        let chart = tc::LineChart {
            slots: cells,
            traces: vec![tc::Trace {
                values: shown.iter().map(|d| d.peak.map(|v| (v / 100.0).clamp(0.0, 1.0))).collect(),
                positions: None,
                colour: p.accent.clone(),
                baseline: None,
            }],
            focus: None,
        };
        plots.push(tc::Plot::new(1 + lw + 1, y0, cells, height, chart).on_axis(0.0, 100.0));
        let mut axis = gutter("0", lw, "└", p);
        axis.push((p.grid.clone(), "─".repeat(cells)));
        rows.push(join_row(axis, room));
        let first = shown.first().map(|d| short_date(d.day)).unwrap_or_default();
        let last = shown.last().map(|d| short_date(d.day)).unwrap_or_default();
        if let Some(d) = date_row(&first, &last, cells, lw, p) {
            rows.push(join_row(vec![d], room));
        }
    }
    let shown = if cells > 0 { shown } else { days };
    for line in rolling_lines(shown) {
        wrapped(&mut rows, &line, room, &p.dim, 2);
    }
    rows
}

/// The sentence under a daily-peak line.
pub fn rolling_lines(days: &[DayPeak]) -> Vec<String> {
    let mut clauses = vec![format!("daily peak, last {}:", plural(days.len(), "day", "days"))];
    let read: Vec<&DayPeak> = days.iter().filter(|d| d.peak.is_some()).collect();
    if let Some(best) = read.iter().max_by(|a, b| a.peak.unwrap_or(0.0).total_cmp(&b.peak.unwrap_or(0.0))) {
        clauses[0].push_str(&format!(" highest {} on {}", pct_text(best.peak.unwrap_or(0.0)), short_date(best.day)));
    }
    if let Some(last) = read.last() {
        clauses.push(format!("latest {} on {}", pct_text(last.peak.unwrap_or(0.0)), short_date(last.day)));
    }
    let unread = days.len() - read.len();
    if unread > 0 {
        clauses.push(format!("{} with no rolling reading", plural(unread, "day", "days")));
    }
    vec![clauses.join(" · ")]
}

// ----- overage and balances: their own bars, in their own unit -----

/// Bars of amounts in the pool's own unit, the newest that fit, with the
/// scale's top in that unit and `names[i]` under the first and last bar.
/// Returns how many bars were drawn.
fn money_bars(
    rows: &mut Vec<String>,
    values: &[f64],
    names: &[String],
    pool: &PoolHistory,
    room: usize,
    p: &Palette,
    plots: &mut Vec<tc::Plot>,
) -> usize {
    let most = values.iter().copied().fold(0.0, f64::max);
    let top = scale_text(&pool.unit, pool.currency.as_deref(), most);
    // The bars are scaled to the figure the axis names - the largest spend
    // rounded up - so a 4.5 under a `5` stops short of the top.
    let limit = no_negative_zero(most.max(0.0)).ceil().max(f64::MIN_POSITIVE);
    let lw = tc::display_width(&top).max(1);
    let cells = room.saturating_sub(lw + 2).min(values.len());
    if cells == 0 {
        return 0;
    }
    let shown = &values[values.len() - cells..];
    let height = 3;
    let columns: Vec<(f64, String)> = shown.iter().map(|v| (*v, p.agent.clone())).collect();
    // The range the axis names, so a new scale replaces the pixels with
    // its label instead of waiting out the upload cap under it.
    let plot = tc::Plot::bars(1 + lw + 1, rows.len(), &columns, height, limit, false).on_axis(0.0, limit);
    for r in 0..height {
        let label = if r == 0 { top.clone() } else { String::new() };
        let mut parts = gutter(&label, lw, if r == 0 { "┤" } else { "│" }, p);
        parts.extend(plot.text_row(r).iter().cloned());
        rows.push(join_row(parts, room));
    }
    plots.push(plot);
    let mut axis = gutter("0", lw, "└", p);
    axis.push((p.grid.clone(), "─".repeat(cells)));
    rows.push(join_row(axis, room));
    let first = &names[values.len() - cells];
    let last = &names[values.len() - 1];
    if let Some(d) = date_row(first, last, cells, lw, p) {
        rows.push(join_row(vec![d], room));
    }
    cells
}

fn overage_rows(
    pool: &PoolHistory,
    bars: &[SpendBar],
    roll_day: Option<(u32, usize)>,
    w: usize,
    p: &Palette,
    plots: &mut Vec<tc::Plot>,
) -> Vec<String> {
    let room = w.saturating_sub(1).max(1);
    let mut rows = Vec::new();
    title_rows(&mut rows, &pool.label, &since_words(pool.total, "cycle", "cycles", pool.since), room, p);
    let values: Vec<f64> = bars.iter().map(|b| b.spent).collect();
    let names: Vec<String> = bars.iter().map(|b| month_text(b.starts_at)).collect();
    let cells = money_bars(&mut rows, &values, &names, pool, room, p, plots);
    let shown = if cells == 0 { bars } else { &bars[bars.len() - cells..] };
    for line in overage_lines(pool, shown, roll_day).into_iter().chain(local_lines(pool)) {
        wrapped(&mut rows, &line, room, &p.dim, 2);
    }
    rows
}

/// `Tue 18 Nov 14:00`, with the year outside this one.
fn when_text(at: f64) -> String {
    use chrono::Datelike;
    match local(at) {
        Some(t) if t.year() == chrono::Local::now().year() => t.format("%a %-d %b %H:%M").to_string(),
        Some(t) => t.format("%a %-d %b %Y %H:%M").to_string(),
        None => "?".into(),
    }
}

/// `Tue 18 Nov 14:00–16:10`, the day said once when both ends share it.
fn when_range(a: f64, b: f64) -> String {
    let (from, to) = (when_text(a), when_text(b));
    if from == to {
        return from;
    }
    match (local(a), local(b)) {
        (Some(x), Some(y)) if x.date_naive() == y.date_naive() => format!("{from}–{}", y.format("%H:%M")),
        _ => format!("{from}–{to}"),
    }
}

/// The lines about this machine's agent turns inside an overage or balance
/// pool's newest long spend brackets (§6.3, source 3). Activity only: the
/// amount is the span's own stored figure, and nothing divides it among the
/// turns. Every line says *local*, because spend from any other device is
/// invisible to it. A bracket with no local turns says nothing: a log
/// cleaned up or never written here reads exactly like an idle machine.
pub fn local_lines(pool: &PoolHistory) -> Vec<String> {
    use super::local::Local;
    let amount = |v: f64| amount_text(&pool.unit, pool.currency.as_deref(), pool.exponent, v);
    let mut lines: Vec<String> = pool
        .brackets
        .iter()
        .filter_map(|b| match &b.local {
            Local::Found(a) => Some(format!(
                "{} spent {}; local turns in this bracket: {} ({}, {})",
                amount(b.amount),
                when_range(b.at_lo, b.at_hi),
                when_range(a.first, a.last),
                a.agent.name(),
                plural(a.turns, "turn", "turns")
            )),
            _ => None,
        })
        .collect();
    let failed = pool.brackets.iter().find_map(|b| match &b.local {
        Local::Failed(why) => Some(why),
        _ => None,
    });
    if let Some(why) = failed {
        lines.push(format!("local turns not read: {why}"));
    }
    lines
}

/// The lines under an overage pool's chart: each bar's spend, in money or
/// requests, never as a percentage.
pub fn overage_lines(pool: &PoolHistory, bars: &[SpendBar], roll_day: Option<(u32, usize)>) -> Vec<String> {
    let amount = |v: f64| amount_text(&pool.unit, pool.currency.as_deref(), pool.exponent, v);
    let each: Vec<String> = bars
        .iter()
        .map(|b| {
            format!(
                "{} {}{}{}",
                month_text(b.starts_at),
                if b.lower_bound && !b.open { "at least " } else { "" },
                amount(b.spent),
                if b.open { " so far" } else { "" }
            )
        })
        .collect();
    let mut lines = vec![format!("spent per cycle: {}", each.join(" · "))];
    let loose: f64 = bars.iter().map(|b| b.unattributed).sum();
    if loose > 0.0 {
        // Spend while no linked allowance was read as used up: not proof
        // that none was, since a gap in its readings looks the same.
        lines.push(format!("{} of it not matched to a used-up allowance", amount(loose)));
    }
    if let Some((day, seen)) = roll_day {
        lines.push(format!("seen to roll on day {day} of the month ({} times)", seen));
    }
    lines
}

fn balance_rows(pool: &PoolHistory, months: &[MonthTotal], w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let room = w.saturating_sub(1).max(1);
    let mut rows = Vec::new();
    let since = months.first().map(month_name).unwrap_or_default();
    title_rows(&mut rows, &pool.label, &format!("spend per calendar month since {since}"), room, p);
    let values: Vec<f64> = months.iter().map(|m| m.spent).collect();
    let names: Vec<String> = months.iter().map(|m| m.label.split(' ').next().unwrap_or("?").to_string()).collect();
    let cells = money_bars(&mut rows, &values, &names, pool, room, p, plots);
    let shown = if cells == 0 { months } else { &months[months.len() - cells..] };
    for line in balance_lines(pool, shown).into_iter().chain(local_lines(pool)) {
        wrapped(&mut rows, &line, room, &p.dim, 2);
    }
    rows
}

/// `Nov 2026`: a month total's label without its `(calendar month)`, which
/// the lines say once.
fn month_name(m: &MonthTotal) -> String {
    m.label.trim_end_matches(" (calendar month)").to_string()
}

/// The lines under a balance's chart: spend, top-ups and expiries per
/// calendar month, labelled as calendar months because a balance has no
/// cycle of its own (§6.2).
pub fn balance_lines(pool: &PoolHistory, months: &[MonthTotal]) -> Vec<String> {
    use chrono::Datelike;
    let amount = |v: f64| amount_text(&pool.unit, pool.currency.as_deref(), pool.exponent, v);
    let now = chrono::Local::now();
    let current = |m: &MonthTotal| m.year == now.year() && m.month == now.month();
    let name = month_name;
    let list = |f: &dyn Fn(&MonthTotal) -> f64| -> Vec<String> {
        months
            .iter()
            .filter(|m| f(m) > 0.0)
            .map(|m| format!("{} {}{}", name(m), amount(f(m)), if current(m) { " so far" } else { "" }))
            .collect()
    };
    let spent: Vec<String> = months
        .iter()
        .map(|m| format!("{} {}{}", name(m), amount(m.spent), if current(m) { " so far" } else { "" }))
        .collect();
    let mut lines = vec![format!("spent per calendar month: {}", spent.join(" · "))];
    let topped = list(&|m| m.topped_up);
    if !topped.is_empty() {
        lines.push(format!("topped up: {}", topped.join(" · ")));
    }
    let expired = list(&|m| m.expired);
    if !expired.is_empty() {
        lines.push(format!("expired: {}", expired.join(" · ")));
    }
    let straddling: usize = months.iter().map(|m| m.straddling).sum();
    if straddling > 0 {
        lines.push(format!(
            "{} began in the month before the one it is counted in",
            plural(straddling, "move", "moves")
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::super::chart::{Over, ResetMark};
    use super::super::fold::{
        fold, CycleModel, FoldInput, Role, Sample, Source, StartSource, Thresholds,
    };
    use super::*;

    const H: f64 = 3600.0;
    const WEEK: f64 = 7.0 * DAY;
    /// Mid-2026: the dates are only read, never compared with now.
    const T0: f64 = 1_782_000_000.0;

    fn cycle(n: u32, consumed: Option<f64>) -> Cycle {
        let starts_at = T0 + (n as f64 - 1.0) * WEEK;
        Cycle {
            ordinal: n,
            starts_at,
            ends_at: Some(starts_at + WEEK),
            closed_at: Some(starts_at + WEEK),
            window_secs: Some(WEEK),
            model: CycleModel::Contiguous,
            start_source: StartSource::Server,
            end_reason: Some(EndReason::Scheduled),
            plan: Some("Pro".into()),
            peak_pct: consumed.map(|c| c.min(100.0)),
            consumed_pct: consumed,
            allowances: 1,
            available_pct: Some(100.0),
            end_is_lower_bound: false,
            used_amount: None,
            limit_amount: None,
            exhausted_at: None,
            first_obs_at: consumed.map(|_| starts_at + H),
            last_obs_at: consumed.map(|_| starts_at + WEEK - H),
            coverage: if consumed.is_some() { 1.0 } else { 0.0 },
            consumed_is_lower_bound: false,
            segments: Vec::new(),
        }
    }

    fn bar(c: Cycle) -> CycleBar {
        CycleBar { cycle: c, resets: Vec::new(), over: Vec::new() }
    }

    fn reset(at: f64, mark: Mark) -> ResetMark {
        ResetMark { at_hi: at, mark, restarted: false }
    }

    fn over(key: u64, amount: f64, spans_reset: bool) -> Over {
        Over {
            key: ("extra_usage".into(), key),
            unit: "money".into(),
            currency: Some("AUD".into()),
            amount,
            spans_reset,
        }
    }

    /// Twelve weekly cycles with every mark the chart can carry.
    fn weekly() -> Vec<CycleBar> {
        let mut bars: Vec<CycleBar> = (1..=12).map(|n| bar(cycle(n, Some(40.0 + n as f64 * 3.0)))).collect();
        // 2: hit 100% four days in (day 5), with A$4 over.
        bars[1].cycle.consumed_pct = Some(100.0);
        bars[1].cycle.exhausted_at = Some(bars[1].cycle.starts_at + 4.0 * DAY);
        bars[1].over = vec![over(1, 4.0, false)];
        // 4: 140% with one banked kept-boundary reset; hit 100 two days in.
        bars[3].cycle.consumed_pct = Some(140.0);
        bars[3].cycle.peak_pct = Some(100.0);
        bars[3].cycle.allowances = 2;
        bars[3].cycle.available_pct = Some(200.0);
        bars[3].cycle.exhausted_at = Some(bars[3].cycle.starts_at + 2.0 * DAY);
        bars[3].resets = vec![reset(bars[3].cycle.starts_at + 3.0 * DAY, Mark::Banked)];
        bars[3].over = vec![over(2, 8.40, true)];
        // 5: the same span shown again (it crossed the boundary).
        bars[4].over = vec![over(2, 8.40, true)];
        bars[4].cycle.exhausted_at = Some(bars[4].cycle.starts_at + 6.0 * DAY);
        // 6: nobody saw it.
        bars[5] = bar(cycle(6, None));
        // 7: cut short by a restarted, issued reset.
        bars[6].cycle.end_reason = Some(EndReason::Reset);
        bars[6].cycle.closed_at = Some(bars[6].cycle.starts_at + 2.0 * DAY);
        bars[6].cycle.consumed_pct = Some(10.0);
        bars[6].resets = vec![ResetMark { at_hi: bars[6].cycle.starts_at + 2.0 * DAY, mark: Mark::Issued, restarted: true }];
        // 9: last seen two days before its end.
        bars[8].cycle.end_is_lower_bound = true;
        bars[8].cycle.last_obs_at = Some(bars[8].cycle.starts_at + 5.0 * DAY);
        bars[8].cycle.consumed_pct = Some(63.0);
        // 10: closed on a plan change; 11 and 12 are on the new plan.
        bars[9].cycle.end_reason = Some(EndReason::PlanChanged);
        bars[10].cycle.plan = Some("Max".into());
        bars[11].cycle.plan = Some("Max".into());
        bars[10].resets = vec![reset(bars[10].cycle.starts_at + DAY, Mark::Unknown)];
        // 12: still open.
        bars[11].cycle.closed_at = None;
        bars[11].cycle.consumed_pct = Some(20.0);
        bars
    }

    fn pool(body: Body, total: usize) -> PoolHistory {
        PoolHistory {
            label: "overall 7d".into(),
            role: Role::Included,
            unit: "pct".into(),
            currency: None,
            exponent: None,
            total,
            since: Some(T0),
            body,
            brackets: Vec::new(),
        }
    }

    fn tab() -> TabHistory {
        let months = vec![
            MonthTotal { year: 2026, month: 9, label: "Sep 2026 (calendar month)".into(), spent: 12.0, topped_up: 50.0, expired: 0.0, straddling: 0 },
            MonthTotal { year: 2026, month: 10, label: "Oct 2026 (calendar month)".into(), spent: 4.5, topped_up: 0.0, expired: 2.0, straddling: 1 },
        ];
        let spend = vec![
            SpendBar { starts_at: T0, spent: 12.40, unattributed: 0.0, open: false, lower_bound: false },
            SpendBar { starts_at: T0 + 30.0 * DAY, spent: 3.10, unattributed: 1.0, open: true, lower_bound: true },
        ];
        let days = (0..30).map(|d| DayPeak { day: T0 + d as f64 * DAY, peak: (d % 7 != 3).then_some(d as f64 * 3.0) }).collect();
        TabHistory {
            pools: vec![
                pool(Body::Cycles(weekly()), 12),
                PoolHistory { label: "session 5h".into(), ..pool(Body::Rolling(days), 30) },
                PoolHistory {
                    label: "extra A$50".into(),
                    role: Role::Overage,
                    unit: "money".into(),
                    currency: Some("AUD".into()),
                    exponent: Some(2),
                    ..pool(Body::Overage { bars: spend, roll_day: Some((14, 2)) }, 2)
                },
                PoolHistory {
                    label: "credits".into(),
                    role: Role::Balance,
                    unit: "credits".into(),
                    ..pool(Body::Balance(months), 2)
                },
            ],
            expired: vec![(T0 + 2.0 * DAY, 1), (T0 - 30.0 * DAY, 5)],
        }
    }

    fn plain(row: &str) -> String {
        let mut out = String::new();
        let mut chars = row.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn draw(t: &TabHistory, w: usize) -> (Vec<String>, Vec<tc::Plot>) {
        let mut plots = Vec::new();
        let rows = section(Some(t), None, None, w, &crate::palette(), &mut plots);
        (rows.iter().map(|r| plain(r)).collect(), plots)
    }

    fn squeezed(rows: &[String]) -> String {
        rows.concat().chars().filter(|c| !c.is_whitespace()).collect()
    }

    #[test]
    fn no_row_outgrows_the_pane_and_no_word_is_lost_at_any_width() {
        let t = tab();
        let bars = weekly();
        let refs: Vec<&CycleBar> = bars.iter().collect();
        for w in 12..=160 {
            let (rows, plots) = draw(&t, w);
            for r in &rows {
                assert!(tc::display_width(r) < w, "width {w}: {r:?}");
            }
            for plot in &plots {
                assert!(plot.x + plot.width < w, "width {w}: a chart past the edge");
                assert!(plot.y + plot.height <= rows.len());
            }
            // Every line of text is on screen, wrapped, never cut.
            let all = squeezed(&rows);
            for needle in ["HISTORY", "overall7d", "12cyclessince", "spentpercycle", "spentpercalendarmonth", "dailypeak"] {
                assert!(all.contains(needle), "width {w}: {needle} missing from {all}");
            }
            // Where all twelve cycles fit, the lines are the whole run's.
            if plots[0].width >= 13 {
                for line in summary_lines(&summarise(&refs, &t.expired)) {
                    let words: String = line.chars().filter(|c| !c.is_whitespace()).collect();
                    assert!(all.contains(&words), "width {w}: {line:?} not whole in {all}");
                }
            }
        }
    }

    fn bracketed(t: &mut TabHistory) {
        use super::super::local::{Activity, Agent, Bracket, Local};
        let found = |agent, lo: f64, turns| Bracket {
            at_lo: lo,
            at_hi: lo + 20.0 * H,
            amount: 4.10,
            local: Local::Found(Activity { agent, first: lo + 2.0 * H, last: lo + 4.0 * H + 600.0, turns }),
        };
        t.pools[2].brackets = vec![
            found(Agent::ClaudeCode, T0 + 3.0 * DAY, 23),
            Bracket { local: Local::Nothing, ..found(Agent::ClaudeCode, T0 + 5.0 * DAY, 1) },
            Bracket { local: Local::Unread, ..found(Agent::ClaudeCode, T0 + 6.0 * DAY, 1) },
        ];
        t.pools[3].brackets = vec![
            found(Agent::Codex, T0 + 40.0 * DAY, 1),
            Bracket { local: Local::Failed("the local log reader panicked".into()), ..found(Agent::Codex, T0 + 41.0 * DAY, 1) },
        ];
    }

    #[test]
    fn local_turns_sit_beside_their_bracket_whole_at_every_width() {
        let mut t = tab();
        bracketed(&mut t);
        let extra = local_lines(&t.pools[2]);
        let credits = local_lines(&t.pools[3]);
        // Only the bracket with turns found gets a line; none found, or not
        // read yet, says nothing rather than "no local turns".
        assert_eq!(extra.len(), 1, "{extra:?}");
        assert!(extra[0].starts_with("A$4.10 spent "), "{}", extra[0]);
        assert!(extra[0].contains("; local turns in this bracket: "), "{}", extra[0]);
        assert!(extra[0].ends_with("(Claude Code, 23 turns)"), "{}", extra[0]);
        assert!(credits[0].ends_with("(Codex, 1 turn)"), "{}", credits[0]);
        assert_eq!(credits[1], "local turns not read: the local log reader panicked");
        for w in 12..=160 {
            let (rows, plots) = draw(&t, w);
            for r in &rows {
                assert!(tc::display_width(r) < w, "width {w}: {r:?}");
            }
            for plot in &plots {
                assert!(plot.x + plot.width < w, "width {w}: a chart past the edge");
            }
            let all = squeezed(&rows);
            for line in extra.iter().chain(&credits) {
                let words: String = line.chars().filter(|c| !c.is_whitespace()).collect();
                assert!(all.contains(&words), "width {w}: {line:?} not whole in {all}");
            }
        }
    }

    #[test]
    fn a_bracket_on_one_day_names_the_day_once() {
        // Ten in the morning local time, whatever zone the test runs in.
        let lo = {
            use chrono::TimeZone;
            let day = local(T0).unwrap().date_naive().and_hms_opt(10, 0, 0).unwrap();
            chrono::Local.from_local_datetime(&day).earliest().unwrap().timestamp() as f64
        };
        let same = when_range(lo, lo + 2.0 * H + 600.0);
        assert_eq!(same.matches(':').count(), 2, "{same}");
        assert!(!same.contains(" – "), "{same}");
        assert_eq!(when_range(lo, lo), when_text(lo));
        let across = when_range(lo, lo + DAY);
        assert_eq!(across.matches(':').count(), 2);
        assert!(across.len() > same.len(), "{across} vs {same}");
    }

    #[test]
    fn a_wider_pane_shows_more_cycles() {
        let bars: Vec<CycleBar> = (1..=200).map(|n| bar(cycle(n, Some(50.0)))).collect();
        let t = TabHistory { pools: vec![pool(Body::Cycles(bars), 200)], expired: Vec::new() };
        let width = |w| draw(&t, w).1[0].width;
        assert!(width(40) < width(100), "{} {}", width(40), width(100));
        assert!(width(100) < width(160));
        assert_eq!(width(12), 12 - 1 - 3 - 2);
        let (rows, _) = draw(&t, 40);
        assert!(rows.iter().any(|r| r.contains(&format!("last {}:", width(40)))), "{rows:#?}");
    }

    #[test]
    fn unseen_cycles_are_hollow_and_lower_bounds_have_open_tops() {
        let (rows, _) = draw(&tab(), 100);
        let text = rows.join("\n");
        assert!(text.contains('·'), "{text}");
        assert!(text.contains('┆'), "{text}");
        assert!(text.contains("1 unseen (·)"), "{text}");
        assert!(text.contains("at least 63%, last seen 2d before reset"), "{text}");
    }

    #[test]
    fn resets_are_marked_under_their_bars_by_kind() {
        let (rows, _) = draw(&tab(), 100);
        let marks = rows.iter().find(|r| r.trim_start().starts_with('↺')).expect("a marks row");
        let letters: String = marks.chars().filter(|c| "BI?".contains(*c)).collect();
        assert_eq!(letters, "BI?");
        let text = rows.join("\n");
        assert!(text.contains("resets ↺: 1 banked, 1 issued, 1 unknown"), "{text}");
        assert!(text.contains("banked: 1 expired unused"), "{text}");
        assert!(text.contains("1 cut short by a restart"), "{text}");
    }

    #[test]
    fn a_cut_short_cycle_shows_the_restart_that_ended_it_before_an_earlier_refill() {
        // CodeRabbit's case: a banked refill and later the issued restart in
        // one cycle. The mark row shows one letter per cycle, and the one a
        // cut-short cycle promises is its restart's.
        let mut t = tab();
        let Body::Cycles(bars) = &mut t.pools[0].body else { unreachable!() };
        let restart = bars[6].resets[0].clone();
        bars[6].resets = vec![reset(bars[6].cycle.starts_at + DAY, Mark::Banked), restart];
        let (rows, _) = draw(&t, 100);
        let marks = rows.iter().find(|r| r.trim_start().starts_with('↺')).expect("a marks row");
        let letters: String = marks.chars().filter(|c| "BI?".contains(*c)).collect();
        assert_eq!(letters, "BI?");
        // Every mark still counts in the totals.
        assert!(rows.join("\n").contains("resets ↺: 2 banked, 1 issued, 1 unknown"));
    }

    #[test]
    fn spend_bars_are_scaled_to_the_figure_on_their_axis() {
        // CodeRabbit's case: the top label reads 5 for a largest spend of
        // 4.5, so a 4.5 bar must stop short of the top, not reach it.
        let p = crate::palette();
        let pool = PoolHistory {
            label: "credits".into(),
            role: Role::Balance,
            unit: "credits".into(),
            ..pool(Body::Balance(Vec::new()), 2)
        };
        let mut rows = Vec::new();
        let mut plots = Vec::new();
        let names = vec!["Sep".to_string(), "Oct".to_string()];
        let cells = money_bars(&mut rows, &[4.5, 1.0], &names, &pool, 60, &p, &mut plots);
        assert_eq!(cells, 2);
        assert!(plain(&rows[0]).contains("5┤"), "{rows:?}");
        let top: String = plots[0].text_row(0).iter().map(|(_, g)| g.clone()).collect();
        assert!(!top.starts_with('█'), "a 4.5 bar under a 5 label filled the top row: {top:?}");
    }

    #[test]
    fn equal_spend_stands_equally_tall_in_its_own_row_and_over_a_hollow_cycle() {
        // CodeRabbit's two cases. The A$8.40 shown on the 140% cycle and on
        // the 55% one was stacked on each, so it stood at 165 on one and
        // below the 100 line on the other, read against an A$9 label. And a
        // cycle nobody saw drew only its `·`, though its spend was counted.
        let p = crate::palette();
        let mut t = tab();
        let Body::Cycles(bars) = &mut t.pools[0].body else { unreachable!() };
        bars[5].over = vec![over(3, 2.0, false)];
        let (_, plots) = draw(&t, 100);
        let chart = &plots[0];
        let cell = |r: usize, col: usize| -> (String, String) {
            let mut seen = 0;
            for (ink, text) in chart.text_row(r) {
                for ch in text.chars() {
                    if seen == col {
                        return (ink.clone(), ch.to_string());
                    }
                    seen += 1;
                }
            }
            panic!("no column {col} in row {r}");
        };
        // The money row is row 0: the same amount, the same glyph.
        assert_eq!(cell(0, 3), cell(0, 4));
        assert!(cell(0, 3).0.starts_with(&p.agent));
        for r in 1..chart.height {
            for col in [3, 4] {
                assert!(!cell(r, col).0.starts_with(&p.agent), "money below its own row at {r},{col}");
            }
        }
        // The hollow cycle keeps its dot and shows its spend.
        assert!(cell(0, 5).0.starts_with(&p.agent), "{:?}", cell(0, 5));
        assert_eq!(cell(chart.height - 1, 5).1, "·");
    }

    #[test]
    fn spend_past_the_allowance_is_stacked_in_money_and_counted_once() {
        let p = crate::palette();
        let (rows, plots) = draw(&tab(), 100);
        let text = rows.join("\n");
        // One span shown against two cycles is A$8.40 once: A$12.40 in all.
        assert!(squeezed(&rows).contains("over:A$12.40in3cycles(1acrossareset)"), "{text}");
        // The scale's top row names the money, and the chart is one row
        // taller for it.
        assert!(rows.iter().any(|r| r.contains("A$9┤")), "{text}");
        let chart = &plots[0];
        assert_eq!(chart.height, 6 + 1, "0-150 in 25s, then a money row");
        // The upper shade (past one allowance) and the money are drawn.
        let inks: String = (0..chart.height).flat_map(|r| chart.text_row(r).iter().map(|(c, _)| c.clone())).collect();
        assert!(inks.contains(&p.warn), "no second shade");
        assert!(inks.contains(&p.agent), "no money");
        assert!(text.contains("1 past one allowance (upper shade)"), "{text}");
    }

    #[test]
    fn a_plan_change_is_a_divider_named_in_the_lines() {
        let (rows, plots) = draw(&tab(), 100);
        let text = rows.join("\n");
        assert!(text.contains("plan │: Pro → Max from"), "{text}");
        let chart = &plots[0];
        // Eleven cycles and the divider before the eleventh.
        assert_eq!(chart.width, 13);
        for r in 0..chart.height {
            let cells: String = chart.text_row(r).iter().map(|(_, t)| t.as_str()).collect();
            assert_eq!(cells.chars().nth(10), Some('│'), "row {r}: {cells:?}");
        }
    }

    #[test]
    fn a_cycle_a_plan_change_closed_early_is_left_out_of_the_average() {
        // CodeRabbit's case: only a restart counted as cut short, so a
        // cycle a plan change closed three days in was averaged with whole
        // ones and pulled the figure down.
        let mut bars = weekly();
        bars[9].cycle.closed_at = Some(bars[9].cycle.starts_at + 3.0 * DAY);
        bars[9].cycle.consumed_pct = Some(5.0);
        let refs: Vec<&CycleBar> = bars.iter().collect();
        let s = summarise(&refs, &[]);
        assert_eq!((s.averaged, s.ended_early), (8, 1));
        let expect: f64 = [43.0, 100.0, 49.0, 140.0, 55.0, 64.0, 63.0, 73.0].iter().sum::<f64>() / 8.0;
        assert!((s.avg.unwrap().0 - expect).abs() < 1e-9);
        assert!(summary_lines(&s)[0].contains("(8 closed; 1 cut short left out; 1 ended early left out)"));
        // Closed by a plan change on its scheduled end, it ran its length.
        let bars = weekly();
        let refs: Vec<&CycleBar> = bars.iter().collect();
        assert_eq!(summarise(&refs, &[]).ended_early, 0);
    }

    #[test]
    fn a_window_change_that_closes_before_the_scheduled_end_is_left_out_of_the_average() {
        // The fold's own case: a weekly window becomes five hours after
        // 6,600 seconds, at 40%. That is a partial period. Averaging it
        // reports 40% over one closed cycle when no full cycle has run.
        let a = 1_000_000.0;
        let w5 = 5.0 * H;
        let samples = vec![
            sample(a + H, 40.0, Some(a + WEEK), Some(WEEK)),
            sample(a + 2.0 * H, 10.0, Some(a + 2.0 * H + w5 - 600.0), Some(w5)),
        ];
        let cycles = fold(&FoldInput {
            role: Role::Included,
            samples: &samples,
            plans: &[],
            resets: &[],
            prev: None,
            next_ordinal: 1,
            rolling_seed: None,
            resume: None,
            gone_at: None,
            thresholds: Thresholds::default(),
        })
        .cycles;
        let closed = &cycles[0];
        assert_eq!(closed.end_reason, Some(EndReason::WindowChanged));
        assert_eq!(closed.closed_at.unwrap() - closed.starts_at, 6_600.0);
        assert_eq!(closed.consumed_pct, Some(40.0));
        let bars: Vec<CycleBar> = cycles.into_iter().map(bar).collect();
        let refs: Vec<&CycleBar> = bars.iter().collect();
        let s = summarise(&refs, &[]);
        assert_eq!((s.averaged, s.ended_early, s.avg), (0, 1, None));
        let line = &summary_lines(&s)[0];
        assert!(line.contains("no full cycle closed yet (1 ended early left out)"), "{line}");
        // The 40% is still the peak of that partial cycle. It is not an average.
        assert!(!line.contains("avg"), "{line}");
        assert!(line.contains("40% peak"), "{line}");

        // A window change on the scheduled end ran the full length.
        let mut full = cycle(1, Some(80.0));
        full.end_reason = Some(EndReason::WindowChanged);
        let bars = [bar(full)];
        let refs: Vec<&CycleBar> = bars.iter().collect();
        let s = summarise(&refs, &[]);
        assert_eq!((s.averaged, s.ended_early), (1, 0));
        assert!((s.avg.unwrap().0 - 80.0).abs() < 1e-9);
    }

    fn sample(at: f64, pct: f64, reset: Option<f64>, window: Option<f64>) -> Sample {
        Sample {
            at,
            pct: Some(pct),
            used: None,
            limit: None,
            window_secs: window,
            reset_at: reset,
            source: Source::Live,
            spacexsi: None,
            grant_left: None,
        }
    }

    #[test]
    fn the_summary_arithmetic() {
        let bars = weekly();
        let refs: Vec<&CycleBar> = bars.iter().collect();
        let s = summarise(&refs, &[(T0 + 2.0 * DAY, 1), (T0 - 30.0 * DAY, 5)]);
        // Closed, seen, full length: 12 less the open one, the unseen one
        // and the cut-short one.
        assert_eq!(s.averaged, 9);
        assert_eq!(s.cut_short, 1);
        let expect: f64 = [43.0, 100.0, 49.0, 140.0, 55.0, 64.0, 63.0, 70.0, 73.0].iter().sum::<f64>() / 9.0;
        let (avg, at_least) = s.avg.unwrap();
        assert!((avg - expect).abs() < 1e-9, "{avg} {expect}");
        assert!(at_least, "a lower-bound cycle is in the average");
        assert_eq!(s.hit, 3);
        // 4, 2 and 6 days in: the median is 4 days in, which is day 5.
        assert_eq!(s.median_to_hit, Some(4.0 * DAY));
        let (peak, lower, allowances, marks) = s.peak.clone().unwrap();
        assert_eq!((peak, lower, allowances), (140.0, false, 2));
        assert_eq!(marks, vec![Mark::Banked]);
        assert_eq!(s.resets, [1, 1, 1]);
        assert_eq!(s.expired, 1, "an expiry before the first cycle shown is not counted");
        assert_eq!(s.unseen, 1);
        assert_eq!(s.past_one, 1);
        assert_eq!(s.lower, 1);
        assert_eq!(s.open, Some(20.0));
        let lines = summary_lines(&s);
        assert_eq!(
            lines[0],
            format!(
                "last 12: avg at least {:.0}% of allowance (9 closed; 1 cut short left out) · 3 hit 100% (median day 5.0) · 140% peak with 1 banked reset (70% of 2 allowances)",
                expect
            )
        );
        assert!(
            lines[1].starts_with("resets ↺: 1 banked, 1 issued, 1 unknown · banked: 1 expired unused · over: A$12.40"),
            "{}",
            lines[1]
        );
        assert!(lines[2].starts_with("now: 20% so far"), "{}", lines[2]);
    }

    #[test]
    fn a_short_window_counts_hours_to_100_and_an_even_median_is_the_middle_pair() {
        let mut bars: Vec<CycleBar> = (1..=4).map(|n| bar(cycle(n, Some(100.0)))).collect();
        for (b, hours) in bars.iter_mut().zip([1.0, 2.0, 3.0, 5.0]) {
            b.cycle.window_secs = Some(5.0 * H);
            b.cycle.exhausted_at = Some(b.cycle.starts_at + hours * H);
        }
        let refs: Vec<&CycleBar> = bars.iter().collect();
        let s = summarise(&refs, &[]);
        assert_eq!(s.median_to_hit, Some(2.5 * H));
        assert!(summary_lines(&s)[0].contains("4 hit 100% (median 2.5h in)"));
    }

    #[test]
    fn nothing_closed_says_so_rather_than_averaging_a_partial_cycle() {
        let mut c = cycle(1, Some(30.0));
        c.closed_at = None;
        let bars = [bar(c)];
        let refs: Vec<&CycleBar> = bars.iter().collect();
        let lines = summary_lines(&summarise(&refs, &[]));
        assert!(lines[0].contains("no full cycle closed yet"), "{lines:?}");
        assert!(!lines[0].contains("avg"), "{lines:?}");
        assert!(lines[2].contains("now: 30% so far"), "{lines:?}");
    }

    #[test]
    fn the_newest_cycles_fit_with_a_divider_between_plans() {
        let bars = weekly();
        assert_eq!(
            fit(&bars, 5),
            vec![Slot::Cycle(8), Slot::Cycle(9), Slot::Divider, Slot::Cycle(10), Slot::Cycle(11)]
        );
        // A divider is drawn only between two cycles that are both shown.
        assert_eq!(fit(&bars, 2), vec![Slot::Cycle(10), Slot::Cycle(11)]);
        assert_eq!(fit(&bars, 3), vec![Slot::Cycle(10), Slot::Cycle(11)]);
        assert_eq!(fit(&bars, 4), vec![Slot::Cycle(9), Slot::Divider, Slot::Cycle(10), Slot::Cycle(11)]);
    }

    #[test]
    fn overage_and_balance_lines_are_in_their_own_unit() {
        let t = tab();
        let Body::Overage { bars, roll_day } = &t.pools[2].body else { unreachable!() };
        let lines = overage_lines(&t.pools[2], bars, *roll_day);
        assert!(lines[0].contains("A$12.40 · "), "{lines:?}");
        assert!(lines[0].ends_with("A$3.10 so far"), "{lines:?}");
        assert_eq!(lines[1], "A$1.00 of it not matched to a used-up allowance", "{lines:?}");
        assert!(lines[2].contains("day 14"), "{lines:?}");
        let Body::Balance(months) = &t.pools[3].body else { unreachable!() };
        let lines = balance_lines(&t.pools[3], months);
        assert_eq!(lines[0], "spent per calendar month: Sep 2026 12 credits · Oct 2026 4.5 credits");
        assert_eq!(lines[1], "topped up: Sep 2026 50 credits");
        assert_eq!(lines[2], "expired: Oct 2026 2 credits");
        assert!(lines[3].starts_with("1 move began"));
    }

    #[test]
    fn spend_that_came_back_to_zero_is_never_minus_zero() {
        assert_eq!(amount_text("money", Some("USD"), Some(2), -0.0), "$0.00");
        assert_eq!(amount_text("money", Some("USD"), Some(2), -1e-12), "$0.00");
        assert_eq!(scale_text("money", Some("USD"), -1e-12), "$0");
        assert_eq!(scale_text("requests", None, 11.2), "12");
    }

    #[test]
    fn a_rolling_pool_draws_a_daily_peak_line() {
        let (rows, plots) = draw(&tab(), 100);
        assert!(plots.iter().any(|p| p.height == PCT_STEP_ROWS && p.chart.traces.len() == 1));
        let text = rows.join("\n");
        assert!(text.contains("rolling · daily peak since"), "{text}");
        assert!(text.contains("daily peak, last 30 days: highest 87% on"), "{text}");
        assert!(squeezed(&rows).contains("4dayswithnorollingreading"), "{text}");
    }
}
