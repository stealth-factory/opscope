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

//! What a tab's HISTORY section draws (OPS-306, spec §7), read off the file
//! once a pass by the poller so the draw thread never touches SQLite.
//!
//! [`gather`] turns the stored cycles, resets, bank events and overage into
//! plain values; [`super::draw`] lays them out. Nothing here computes a
//! figure the fold did not store: the read side only groups and sums.

use std::collections::HashMap;

use rusqlite::params;

use super::Observation;
use super::fold::{Cycle, CycleEffect, Role, Sample, Thresholds, rolling_mask};
use super::overage::read::MonthTotal;
use super::store::History;

/// The most cycles a chart can show: wider than any pane is, so a chart
/// that grows with the pane is never short of cycles to grow into.
pub const MOST_CYCLES: usize = 400;
/// The most days of daily peaks a rolling pool keeps for its chart.
pub const MOST_DAYS: usize = 400;

/// How a reset was told apart (§5.6), as the chart marks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Banked,
    Issued,
    Unknown,
}

impl Mark {
    fn from_name(name: &str) -> Mark {
        match name {
            "banked" => Mark::Banked,
            "issued" => Mark::Issued,
            _ => Mark::Unknown,
        }
    }

    /// The letter under the bar: `B`, `I`, `?`.
    pub fn letter(self) -> char {
        match self {
            Mark::Banked => 'B',
            Mark::Issued => 'I',
            Mark::Unknown => '?',
        }
    }
}

/// One reset in a cycle.
#[derive(Clone, Debug, PartialEq)]
pub struct ResetMark {
    pub at_hi: f64,
    pub mark: Mark,
    /// The reset restarted the cycle rather than refilling it.
    pub restarted: bool,
}

/// Spend past the allowance attributed to one cycle (§6.2): whole spans,
/// in their own unit, never a percentage.
#[derive(Clone, Debug, PartialEq)]
pub struct Over {
    /// Which overage pool and which span: the same span shown against two
    /// cycles is one span, counted once.
    pub key: (String, u64),
    /// `money`, `requests`, ...; the currency for money.
    pub unit: String,
    pub currency: Option<String>,
    pub amount: f64,
    /// Its bracket crossed the cycle's boundary (§6.3).
    pub spans_reset: bool,
}

/// One included cycle and what happened in it.
#[derive(Clone, Debug, PartialEq)]
pub struct CycleBar {
    pub cycle: Cycle,
    pub resets: Vec<ResetMark>,
    pub over: Vec<Over>,
}

/// A rolling pool's highest reading of one local day.
#[derive(Clone, Debug, PartialEq)]
pub struct DayPeak {
    /// Local midnight the day starts at.
    pub day: f64,
    /// `None`: no reading that day.
    pub peak: Option<f64>,
}

/// An overage pool's own cycle with its total.
#[derive(Clone, Debug, PartialEq)]
pub struct SpendBar {
    pub starts_at: f64,
    pub spent: f64,
    pub unattributed: f64,
    /// Still running: the total so far.
    pub open: bool,
    /// Readings had gaps, so spend may be missing: "at least".
    pub lower_bound: bool,
}

/// What one pool's chart draws.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    /// Periodic cycles: one bar each.
    Cycles(Vec<CycleBar>),
    /// A rolling window: a daily-peak line.
    Rolling(Vec<DayPeak>),
    /// Spend past the plan on its own clock: one bar per cycle.
    Overage { bars: Vec<SpendBar>, roll_day: Option<(u32, usize)> },
    /// A balance: spend per calendar month.
    Balance(Vec<MonthTotal>),
}

/// One pool's chart.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolHistory {
    pub label: String,
    pub role: Role,
    /// `pct`, `money`, `requests`, `credits`, `tokens`.
    pub unit: String,
    pub currency: Option<String>,
    pub exponent: Option<i32>,
    /// Every cycle stored, of which `body` holds the newest.
    pub total: usize,
    /// When the first of them started, or the first reading.
    pub since: Option<f64>,
    pub body: Body,
}

/// One tab's history.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabHistory {
    /// In the tab's own order: the order its lanes are drawn in, then any
    /// pool the file holds that this pass did not report (an idle Claude
    /// limit is left out of a reading, and its history is still history).
    pub pools: Vec<PoolHistory>,
    /// When banked resets expired unused (§5.6 `expired`), with how many.
    pub expired: Vec<(f64, u32)>,
}

struct PoolRow {
    id: i64,
    key: String,
    label: String,
    role: String,
    unit: String,
    currency: Option<String>,
    exponent: Option<i32>,
}

fn sql(e: rusqlite::Error) -> String {
    e.to_string()
}

impl History {
    fn pool_rows(&self, provider: &str, account_key: &str) -> Result<Vec<PoolRow>, String> {
        let mut st = self
            .conn
            .prepare(
                "SELECT p.id, p.pool_key, p.label, p.role, p.unit, p.currency, p.exponent
                   FROM pool p JOIN account a ON a.id = p.account_id
                  WHERE a.provider = ?1 AND a.account_key = ?2 ORDER BY p.id",
            )
            .map_err(sql)?;
        let rows = st
            .query_map(params![provider, account_key], |r| {
                Ok(PoolRow {
                    id: r.get(0)?,
                    key: r.get(1)?,
                    label: r.get(2)?,
                    role: r.get(3)?,
                    unit: r.get(4)?,
                    currency: r.get(5)?,
                    exponent: r.get(6)?,
                })
            })
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// A pool's resets, oldest first, with the ordinal of the cycle the
    /// fold put each in.
    fn reset_rows(&self, pool: i64) -> Result<Vec<(Option<u32>, ResetMark)>, String> {
        let mut st = self
            .conn
            .prepare(
                "SELECT c.ordinal, e.at_hi, e.kind, e.cycle_effect FROM reset_event e
                   LEFT JOIN cycle c ON c.id = e.cycle_id
                  WHERE e.pool_id = ?1 ORDER BY e.at_hi",
            )
            .map_err(sql)?;
        let rows = st
            .query_map([pool], |r| {
                let kind: String = r.get(2)?;
                let effect: String = r.get(3)?;
                Ok((
                    r.get::<_, Option<i64>>(0)?.map(|o| o as u32),
                    ResetMark {
                        at_hi: r.get(1)?,
                        mark: Mark::from_name(&kind),
                        restarted: effect == CycleEffect::Restarted { new_start: 0.0 }.name(),
                    },
                ))
            })
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    fn has_links(&self, pool: i64) -> Result<bool, String> {
        self.conn
            .query_row("SELECT EXISTS (SELECT 1 FROM pool_link WHERE included_id = ?1)", [pool], |r| r.get(0))
            .map_err(sql)
    }

    /// Banked resets that expired unused, for one account (§5.6).
    fn expired_banks(&self, provider: &str, account_key: &str) -> Result<Vec<(f64, u32)>, String> {
        let mut st = self
            .conn
            .prepare(
                "SELECT e.at_hi, e.count FROM bank_event e JOIN account a ON a.id = e.account_id
                  WHERE a.provider = ?1 AND a.account_key = ?2 AND e.kind = 'expired' ORDER BY e.at_hi",
            )
            .map_err(sql)?;
        let rows = st
            .query_map(params![provider, account_key], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u32)))
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// One pool's chart, or `None` when it has nothing to draw yet.
    /// One pool's charts: none when it has nothing to draw yet, and two for
    /// an allowance that has both cycles and rolling stretches between them
    /// - the bars, then the stretches' daily-peak line.
    fn pool_history(&self, provider: &str, account_key: &str, row: &PoolRow, t: &Thresholds) -> Result<Vec<PoolHistory>, String> {
        let role = Role::from_name(&row.role);
        let mut out = PoolHistory {
            label: row.label.clone(),
            role,
            unit: row.unit.clone(),
            currency: row.currency.clone(),
            exponent: row.exponent,
            total: 0,
            since: None,
            body: Body::Cycles(Vec::new()),
        };
        match role {
            Role::Balance => {
                let months = self.balance_months(provider, account_key, &row.key)?;
                if months.is_empty() {
                    return Ok(Vec::new());
                }
                out.total = months.len();
                out.body = Body::Balance(months);
                return Ok(vec![out]);
            }
            Role::Overage => {
                let cycles = self.overage_cycles(provider, account_key, &row.key)?;
                if cycles.is_empty() {
                    return Ok(Vec::new());
                }
                out.total = cycles.len();
                out.since = cycles.first().map(|c| c.cycle.starts_at);
                let from = cycles.len().saturating_sub(MOST_CYCLES);
                let bars = cycles[from..]
                    .iter()
                    .map(|c| SpendBar {
                        starts_at: c.cycle.starts_at,
                        spent: c.spent,
                        unattributed: c.unattributed,
                        open: c.cycle.closed_at.is_none(),
                        lower_bound: c.cycle.coverage < 0.999 || c.cycle.end_is_lower_bound,
                    })
                    .collect();
                let roll_day = self
                    .overage_roll_day(provider, account_key, &row.key)?
                    .map(|d| (d.day, d.seen));
                out.body = Body::Overage { bars, roll_day };
                return Ok(vec![out]);
            }
            Role::Included => {}
        }
        // A rolling stretch keeps no cycles (§5.1); it sits between them, so
        // a pool can have both. Its readings are drawn as a daily-peak line.
        let samples = self.samples(provider, account_key, &row.key)?;
        let mask = rolling_mask(&samples, t.near_zero_pct, None);
        let rolling: Vec<Sample> = samples.iter().zip(&mask).filter(|(_, m)| **m).map(|(s, _)| s.clone()).collect();
        let rolling_chart = |out: &PoolHistory| -> Option<PoolHistory> {
            let first = rolling.first()?;
            let days = daily_peaks(&rolling, local_midnight);
            let from = days.len().saturating_sub(MOST_DAYS);
            Some(PoolHistory {
                since: Some(first.at),
                total: days.len(),
                body: Body::Rolling(days[from..].to_vec()),
                ..out.clone()
            })
        };
        let cycles = self.cycles(provider, account_key, &row.key)?;
        if cycles.is_empty() {
            return Ok(rolling_chart(&out).into_iter().collect());
        }
        out.total = cycles.len();
        out.since = cycles.first().map(|c| c.starts_at);
        let from = cycles.len().saturating_sub(MOST_CYCLES);
        let resets = self.reset_rows(row.id)?;
        let linked = self.has_links(row.id)?;
        let mut bars: Vec<CycleBar> = cycles[from..]
            .iter()
            .map(|c| CycleBar {
                cycle: c.clone(),
                resets: Vec::new(),
                over: Vec::new(),
            })
            .collect();
        place_resets(&mut bars, resets);
        if linked {
            for i in 0..bars.len() {
                // A span is attributed only while the pool was exhausted, so
                // only a cycle that was, or the one after it (a span across
                // the boundary), can hold one.
                let exhausted = |j: usize| bars[j].cycle.exhausted_at.is_some();
                if !(exhausted(i) || (i > 0 && exhausted(i - 1))) {
                    continue;
                }
                let spans = self.spend_in_cycle(provider, account_key, &row.key, &bars[i].cycle)?;
                bars[i].over = spans
                    .into_iter()
                    .map(|s| Over {
                        key: (s.overage_pool_key, s.span.at_hi.to_bits()),
                        unit: s.unit,
                        currency: s.currency,
                        amount: s.span.amount,
                        spans_reset: s.spans_reset,
                    })
                    .collect();
            }
        }
        out.body = Body::Cycles(bars);
        let line = rolling_chart(&out);
        Ok(std::iter::once(out).chain(line).collect())
    }
}

/// Put each reset under the cycle the fold filed it in, or the cycle its
/// later reading falls in when it was filed nowhere. A restarted reset is
/// read at the first reading of the cycle it opened, so it is moved back to
/// the cycle it cut short: that is the one it ended, and the one the mark
/// has to flag.
pub fn place_resets(bars: &mut [CycleBar], resets: Vec<(Option<u32>, ResetMark)>) {
    for (ordinal, r) in resets {
        let by_ordinal = ordinal.and_then(|o| bars.iter().position(|b| b.cycle.ordinal == o));
        let by_time = || {
            bars.iter().rposition(|b| b.cycle.starts_at <= r.at_hi)
        };
        let Some(mut i) = by_ordinal.or_else(by_time) else { continue };
        if r.restarted && i > 0 && bars[i - 1].cycle.cut_short() && !bars[i - 1].resets.iter().any(|x| x.restarted) {
            i -= 1;
        }
        bars[i].resets.push(r);
    }
}

/// The local midnight `at` falls after.
fn local_midnight(at: f64) -> f64 {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(at.trunc() as i64, 0)
        .single()
        .and_then(|t| t.date_naive().and_hms_opt(0, 0, 0))
        .and_then(|d| Local.from_local_datetime(&d).earliest())
        .map_or(at, |d| d.timestamp() as f64)
}

/// The highest reading of each day from the first sample's to the last's,
/// a day with no reading kept as `None` rather than joined across.
/// `midnight` gives the start of the day an instant is in.
pub fn daily_peaks(samples: &[Sample], midnight: impl Fn(f64) -> f64) -> Vec<DayPeak> {
    let mut out: Vec<DayPeak> = Vec::new();
    for s in samples {
        let Some(pct) = s.pct.filter(|p| p.is_finite()) else { continue };
        let day = midnight(s.at);
        if let Some(last) = out.last().map(|d| d.day) {
            // Days nobody read: one empty slot each, so the line breaks.
            let mut next = midnight(last + 36.0 * 3600.0);
            while next < day && out.len() < 100_000 {
                out.push(DayPeak { day: next, peak: None });
                next = midnight(next + 36.0 * 3600.0);
            }
        }
        match out.last_mut() {
            Some(d) if d.day == day => d.peak = Some(d.peak.map_or(pct, |p| p.max(pct))),
            _ => out.push(DayPeak { day, peak: Some(pct) }),
        }
    }
    out
}

/// Every tab's history, read once a pass. A tab's pools are those of the
/// account it draws - the unified plan's own for a SpaceXSI tab.
pub fn gather(h: &History, obs: &[Observation], t: &Thresholds) -> Result<HashMap<String, TabHistory>, String> {
    let mut out: HashMap<String, TabHistory> = HashMap::new();
    let mut done: Vec<(String, String, String)> = Vec::new();
    for o in obs {
        // The keys the recorder wrote under: an outage reading with no id
        // maps to the account last seen, and a unified plan's copy to its own.
        let own = h.account_key_of(o);
        let (provider, key) = match h.spacexsi_key_of(o) {
            Some(k) => ("spacexsi".to_string(), k),
            None => (o.provider.clone(), own.clone()),
        };
        let marker = (o.tab.clone(), provider.clone(), key.clone());
        if done.contains(&marker) {
            continue;
        }
        done.push(marker);
        let rows = h.pool_rows(&provider, &key)?;
        // The lanes' order first; then what the file has that this pass
        // did not report, oldest first.
        let mut ordered: Vec<&PoolRow> = Vec::new();
        for lane in obs.iter().filter(|x| x.tab == o.tab).flat_map(|x| x.lanes.iter()) {
            if let Some(r) = rows.iter().find(|r| r.key == lane.pool_key) {
                if !ordered.iter().any(|x| x.id == r.id) {
                    ordered.push(r);
                }
            }
        }
        for r in &rows {
            if !ordered.iter().any(|x| x.id == r.id) {
                ordered.push(r);
            }
        }
        let tab = out.entry(o.tab.clone()).or_default();
        for r in ordered {
            tab.pools.extend(h.pool_history(&provider, &key, r, t)?);
        }
        tab.expired.extend(h.expired_banks(&o.provider, &own)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::fold::Source;

    fn s(at: f64, pct: f64) -> Sample {
        Sample {
            at,
            pct: Some(pct),
            used: None,
            limit: None,
            window_secs: None,
            reset_at: None,
            source: Source::Live,
            spacexsi: None,
        }
    }

    fn lane(key: &str, pct: f64, at: f64, reset: f64, window: f64) -> crate::shared::Lane {
        crate::shared::Lane {
            label: key.into(),
            pct,
            window_secs: Some(window),
            reset: Some(reset),
            pool_key: key.into(),
            fetched_at: at,
            ..crate::shared::Lane::default()
        }
    }

    #[test]
    fn a_tab_gets_its_pools_in_lane_order_and_keeps_one_this_pass_left_out() {
        const WEEK: f64 = 7.0 * 86_400.0;
        const FIVE: f64 = 5.0 * 3600.0;
        let dir = std::env::temp_dir().join(format!("opscope-chart-{}-{}", std::process::id(), opscope_core::now().to_bits()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = History::open(&dir.join(super::super::store::FILE)).unwrap();
        let t = super::super::Thresholds::default();
        let obs = |lanes| Observation {
            provider: "codex".into(),
            account_id: "acct-1".into(),
            heading: "CODEX".into(),
            tab: "codex".into(),
            lanes,
            ..Observation::default()
        };
        let a = 1_000_000.0;
        // Three weeks of the weekly pool, and a session pool read once.
        for (i, at) in [a + 2.0 * 86_400.0, a + WEEK + 2.0 * 86_400.0, a + 2.0 * WEEK + 2.0 * 86_400.0].iter().enumerate() {
            let week_end = a + (i as f64 + 1.0) * WEEK;
            let mut lanes = vec![lane("secondary_window", 30.0 + i as f64, *at, week_end, WEEK)];
            if i == 0 {
                lanes.insert(0, lane("primary_window", 10.0, *at, at + FIVE, FIVE));
            }
            h.record(&[obs(lanes.clone())], &t, *at).unwrap();
        }
        // This pass reports only the weekly pool, after a reading of it.
        let pass = [obs(vec![lane("secondary_window", 40.0, a + 2.0 * WEEK + 3.0 * 86_400.0, a + 3.0 * WEEK, WEEK)])];
        // A 5h window whose reset slides with the clock: a rolling stretch,
        // drawn as a line rather than dropped.
        for k in 0..4 {
            let at = a + 3.0 * WEEK + k as f64 * 3600.0;
            h.record(&[obs(vec![lane("primary_window", 20.0 + k as f64, at, at + FIVE, FIVE)])], &t, at).unwrap();
        }
        let got = gather(&h, &pass, &t).unwrap();
        let tab = &got["codex"];
        let labels: Vec<&str> = tab.pools.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, vec!["secondary_window", "primary_window"]);
        let Body::Cycles(bars) = &tab.pools[0].body else { panic!("{:?}", tab.pools[0].body) };
        assert_eq!(bars.len(), 3);
        assert_eq!(tab.pools[0].total, 3);
        assert_eq!(tab.pools[0].since, Some(bars[0].cycle.starts_at));
        assert!(
            tab.pools.iter().any(|p| p.label == "primary_window" && matches!(p.body, Body::Rolling(_))),
            "{:?}",
            tab.pools.iter().map(|p| (&p.label, &p.body)).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daily_peaks_keep_the_highest_reading_and_leave_unread_days_empty() {
        let day = 86_400.0;
        let midnight = |at: f64| (at / day).floor() * day;
        let got = daily_peaks(&[s(10.0, 20.0), s(500.0, 35.0), s(3.0 * day + 5.0, 12.0)], midnight);
        assert_eq!(
            got,
            vec![
                DayPeak { day: 0.0, peak: Some(35.0) },
                DayPeak { day, peak: None },
                DayPeak { day: 2.0 * day, peak: None },
                DayPeak { day: 3.0 * day, peak: Some(12.0) },
            ]
        );
    }
}
