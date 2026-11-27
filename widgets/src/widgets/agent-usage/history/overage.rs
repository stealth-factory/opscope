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

//! Over-utilisation: spend past an allowance (spec §6, OPS-307).
//!
//! An overage pool keeps its **own cycles on its own clock** - the fold
//! does that, from the pool's own samples: dated pools (SpaceXSI's usage
//! limit, Cursor's extra, Grok's on-demand, Copilot's overage requests) take
//! their boundaries from the server, and Claude's undated extra usage has
//! its boundary observed when the spend falls back to about zero
//! (`start_source = 'inferred'`). [`read::roll_day`] names the day such a pool
//! has been seen to roll on, for display only: nothing here projects a
//! countdown from it. A balance (Cursor's prepaid credit, Perplexity's
//! purchased credit, Codex credits, Devin's and Droid's extra-usage
//! balances) has no cycle at all: [`balance_moves`] reads a fall as spend,
//! a rise as a top-up, and a fall across a grant's stated expiry as expiry.
//!
//! What is written, and by whom:
//!
//! * [`spans_with`] is called by the store each time an `overage` or `balance`
//!   pool is re-folded, with that pool's samples and those of every
//!   included pool `pool_link` ties it to; its spans replace the pool's
//!   `overage_span` rows from the same point on. Each span is one spend
//!   delta between two readings, **in the pool's own unit** (money, or
//!   requests for Copilot) and never a percentage, with the full amount and
//!   the linked included pools exhausted at the time. The amount is never
//!   split between them: dividing it would be a number nobody measured.
//!   Spend while no linked pool was exhausted keeps an empty list and is
//!   read as unattributed.
//! * [`events_of`] is called by `observe` for every observation; its
//!   per-request charges (Cursor's `GetFilteredUsageEvents`, already
//!   fetched for METERED - no request is added) are written to
//!   `overage_event`, deduped by [`event_key`].
//! * [`links_of`] is called by `observe` for every observation; its pairs
//!   are written to `pool_link` (insert-or-ignore).
//!
//! The read side, for the charts (phase 3), is [`read`]: its `impl History`
//! block holds [`History::spend_in_cycle`] for the overage drawn while one
//! included cycle was exhausted, [`History::overage_cycles`] for an overage
//! pool's own cycles with their totals, [`History::balance_months`] for a
//! balance's spend per calendar month, [`History::charges`] for the exact
//! per-request charges and [`History::overage_roll_day`] for an undated
//! pool's observed roll day.

use rusqlite::{OptionalExtension, Transaction, params};

use super::Observation;
use super::fold::{EXHAUSTED_PCT, Role, Sample, Thresholds, tolerance};

use crate::shared::PoolRole;

/// Smaller moves than this, in the pool's unit, are float noise.
const EPS: f64 = 1e-9;

/// An included pool an overage pool serves, with its samples over the same
/// stretch - including the last one at or before it, so the reading in
/// force when the stretch opens is known.
#[derive(Clone, Debug)]
pub struct Included<'a> {
    pub pool_id: i64,
    pub samples: &'a [Sample],
}

/// A row of `overage_span`, less the overage pool's id.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    /// The spend happened between these two readings: a few minutes while
    /// watching, the whole gap after one.
    pub at_lo: f64,
    pub at_hi: f64,
    /// In the overage pool's unit and currency. The whole delta, never a
    /// share of it.
    pub amount: f64,
    /// Ids of the linked included pools at or above 99.5% then, sorted. May
    /// be empty: that spend is unattributed.
    pub exhausted: Vec<i64>,
}

/// What a balance did between two readings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveKind {
    /// It fell: spend.
    Spend,
    /// It rose: a purchase or a grant.
    TopUp,
    /// It fell across a grant's stated expiry, by no more than the grants
    /// that left the balance's total held (see [`balance_moves`]). Not spend
    /// - though spend in the same bracket, up to that amount, cannot be told
    /// apart from it; what falls past it is spend, a move of its own.
    Expiry,
}

/// One move of a balance between two readings.
#[derive(Clone, Debug, PartialEq)]
pub struct Move {
    pub at_lo: f64,
    pub at_hi: f64,
    /// Positive, in the balance's unit, whichever way it moved.
    pub amount: f64,
    pub kind: MoveKind,
}

/// The spend figure of an overage reading: `used`, in the pool's unit.
fn spend_of(s: &Sample) -> Option<f64> {
    s.used.filter(|u| u.is_finite())
}

/// What is left on a balance: the ceiling less what was used. A balance
/// with no ceiling (Codex credits, Devin, Droid) states what is left as its
/// `limit`, with no `used`.
fn level_of(s: &Sample) -> Option<f64> {
    let limit = s.limit.filter(|l| l.is_finite())?;
    let left = limit - s.used.filter(|u| u.is_finite()).unwrap_or(0.0);
    left.is_finite().then_some(left)
}

/// The figure as a percentage of the cap, where one can be read.
fn pct_of(s: &Sample) -> Option<f64> {
    s.pct.or_else(|| match (s.used, s.limit) {
        (Some(u), Some(l)) if l > 0.0 => Some(100.0 * u / l),
        _ => None,
    })
}

/// Whether the overage pool's own cycle rolled between `p` and `n`, and
/// where its new cycle began when that is known.
///
/// Dated: the stated reset was reached, or a later one appeared. Undated:
/// the spend fell back to about zero from above it - the same rule the fold
/// reads its inferred boundary off, so the two cannot disagree.
fn rolled(p: &Sample, n: &Sample, t: &Thresholds) -> Option<Option<f64>> {
    let window = n.window_secs.or(p.window_secs);
    let tol = tolerance(window);
    if let Some(r) = p.reset_at {
        let next = n.reset_at.filter(|nr| *nr > r + tol);
        // The earlier reading was itself already past the reset it states,
        // by the same rule that would roll at it, so it is in the cycle that
        // followed and a later reading still stating that reset is in it
        // too: a delta, not a second roll booking the whole spend again.
        if next.is_none() && p.at >= r + tol {
            return None;
        }
        // No tolerance before the end: a reading inside the last hours of a
        // month that still states the same end is that month's, and booking
        // it as a roll would count its whole cumulative spend again.
        if next.is_some() || super::fold::rolled_over(r, n, tol, 0.0) {
            // The new cycle's own start where the reading says it, else the
            // old cycle's end.
            let start = match (next, window) {
                (Some(nr), Some(w)) if w > 0.0 => nr - w,
                _ => r,
            };
            return Some(Some(start));
        }
        return None;
    }
    if n.reset_at.is_some() {
        return None;
    }
    let near = t.near_zero_pct;
    match (pct_of(p), pct_of(n)) {
        (Some(a), Some(b)) => (a > near && b <= near).then_some(None),
        _ => match (spend_of(p), spend_of(n)) {
            (Some(a), Some(b)) => (a > EPS && b <= EPS).then_some(None),
            _ => None,
        },
    }
}

/// The linked included pools at or above [`EXHAUSTED_PCT`] at some reading
/// in force during `lo..hi`: the last one at or before `lo` (unless its own
/// window had already closed by then), and every one inside the bracket.
///
/// The samples are in time order, so both ends are found by bisection: a
/// full refold asks this once per spend delta, and scanning every reading
/// each time made it deltas times readings.
fn exhausted_during(included: &[Included], lo: f64, hi: f64) -> Vec<i64> {
    let mut out: Vec<i64> = included
        .iter()
        .filter(|inc| {
            let first_after = inc.samples.partition_point(|s| s.at <= lo);
            let past = inc.samples.partition_point(|s| s.at <= hi);
            let before = first_after
                .checked_sub(1)
                .map(|i| &inc.samples[i])
                .filter(|s| s.reset_at.is_none_or(|r| r > lo));
            let inside = inc.samples[first_after..past.max(first_after)].iter();
            before
                .into_iter()
                .chain(inside)
                .any(|s| s.pct.is_some_and(|p| p >= EXHAUSTED_PCT))
        })
        .map(|inc| inc.pool_id)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The spend spans in an overage or balance pool's samples, oldest first.
///
/// `samples` starts with the last sample already folded, where there is
/// one, so the first delta after a re-fold point is not lost. Only `used`
/// is read for an overage pool, and the level for a balance: a reading
/// with neither is skipped rather than guessed. A fall in an overage
/// pool's spend that is not its cycle rolling (a refund, a correction) is
/// no spend and makes no span. After a roll, what the new cycle already
/// shows was spent between the boundary (or the reading before, if later)
/// and the reading that showed it.
#[cfg(test)]
pub fn spans(role: Role, samples: &[Sample], included: &[Included], t: &Thresholds) -> Vec<Span> {
    spans_with(role, samples, included, t, &[])
}

/// The spend spans, as `spans` (above) describes them, with the grant
/// expiries readings before `samples` stated
/// (`stated`) - a balance's fall across one is expiry whichever reading
/// said so, exactly as the full replay ([`read`]'s `balance_months`) reads
/// it, so an incremental write and a full one cannot disagree about one move.
pub fn spans_with(role: Role, samples: &[Sample], included: &[Included], t: &Thresholds, stated: &[f64]) -> Vec<Span> {
    let mut out = Vec::new();
    match role {
        Role::Included => {}
        Role::Balance => {
            let mut known = expiries(samples);
            known.extend_from_slice(stated);
            known.sort_by(f64::total_cmp);
            known.dedup();
            for m in balance_moves(samples, &known) {
                if m.kind == MoveKind::Spend {
                    out.push(Span {
                        exhausted: exhausted_during(included, m.at_lo, m.at_hi),
                        at_lo: m.at_lo,
                        at_hi: m.at_hi,
                        amount: m.amount,
                    });
                }
            }
        }
        Role::Overage => {
            let mut prev: Option<(&Sample, f64)> = None;
            for n in samples {
                let Some(b) = spend_of(n) else { continue };
                if let Some((p, a)) = prev {
                    let span = match rolled(p, n, t) {
                        Some(boundary) => (b > EPS).then(|| {
                            let lo = boundary.map_or(p.at, |x| x.max(p.at)).min(n.at);
                            (lo, b)
                        }),
                        None => (b - a > EPS).then_some((p.at, b - a)),
                    };
                    if let Some((lo, amount)) = span {
                        out.push(Span {
                            at_lo: lo,
                            at_hi: n.at,
                            amount,
                            exhausted: exhausted_during(included, lo, n.at),
                        });
                    }
                }
                prev = Some((n, b));
            }
        }
    }
    out
}

/// The grant expiries a balance's readings stated: for a balance pool,
/// `reset_at` is the soonest grant's expiry where the provider sends one.
fn expiries(samples: &[Sample]) -> Vec<f64> {
    let mut out: Vec<f64> = samples.iter().filter_map(|s| s.reset_at).collect();
    out.sort_by(f64::total_cmp);
    out.dedup();
    out
}

/// Every move of a balance between consecutive readings with a level,
/// oldest first: a fall is spend, a rise a top-up, a fall across one of
/// `expiries` is expiry. A balance has no cycle, so nothing here rolls.
///
/// Expiry is capped at what the grants that expired held. A balance with a
/// ceiling and `used` (Perplexity's pools, Cursor's prepaid) states the
/// grants it holds as its `limit`, so a grant that lapsed takes its amount
/// out of the limit: the limit's fall is the most the expiry can be, and
/// the rest of the fall is spend, in a move of its own over the same
/// bracket. A limit that did not fall lost no grant, and its fall is all
/// spend. A balance with no ceiling (Codex credits, Devin, Droid) states
/// only what is left, so nothing bounds an expiry but the fall itself.
pub fn balance_moves(samples: &[Sample], expiries: &[f64]) -> Vec<Move> {
    let mut out = Vec::new();
    let mut prev: Option<(&Sample, f64)> = None;
    for s in samples {
        let Some(level) = level_of(s) else { continue };
        if let Some((p, before)) = prev {
            let (at, d) = (p.at, level - before);
            let mut push = |amount: f64, kind: MoveKind| {
                if amount > EPS {
                    out.push(Move {
                        at_lo: at,
                        at_hi: s.at,
                        amount,
                        kind,
                    });
                }
            };
            if d > EPS {
                push(d, MoveKind::TopUp);
            } else if d < -EPS {
                let fall = -d;
                let expired = if expiries.iter().any(|e| *e > at && *e <= s.at) {
                    match (p.used, s.used, p.limit, s.limit) {
                        (Some(_), Some(_), Some(lo), Some(hi)) => (lo - hi).clamp(0.0, fall),
                        _ => fall,
                    }
                } else {
                    0.0
                };
                push(expired, MoveKind::Expiry);
                push(fall - expired, MoveKind::Spend);
            }
        }
        prev = Some((s, level));
    }
    out
}

/// A per-request charge a provider published (Cursor's
/// `GetFilteredUsageEvents`, §6.3): a row of `overage_event`.
#[derive(Clone, Debug, PartialEq)]
pub struct Charge {
    /// The overage pool it belongs to, by the provider's pool key.
    pub pool_key: String,
    /// The provider's event id, or a hash of the event's fields
    /// ([`event_key`]).
    pub event_key: String,
    /// The event's own timestamp, epoch seconds.
    pub at: f64,
    /// Charged, in the pool's currency's major unit.
    pub amount: f64,
    pub model: Option<String>,
}

/// The key one Cursor usage event is deduped by. Cursor sends no event id,
/// so it is FNV-1a over the fields that make the event itself - timestamp,
/// model, charged cents and token usage - each as the server wrote it, so
/// the same event fetched twice, by two panes or two passes, gets one key.
///
/// Not keyed on the stable fields alone, deliberately. The events carry no
/// id, and timestamp and model are the only fields known not to move; two
/// requests on one model in the same millisecond would then be one charge,
/// understating spend. Whether Cursor revises an event's `chargedCents` or
/// `tokenUsage` after it is first listed is not known - nothing in the
/// fixtures or the payloads seen shows it - and if it did, the revised
/// event would be a second row. Kept as it is until a payload shows either.
pub fn event_key(e: &serde_json::Value) -> String {
    let field = |k: &str| match &e[k] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        v => v.to_string(),
    };
    let text = format!(
        "{}|{}|{}|{}",
        field("timestamp"),
        field("model"),
        field("chargedCents"),
        field("tokenUsage")
    );
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("ev:{h:016x}")
}

/// The charged events of one page of Cursor usage events, in the shape
/// Cursor's event report keeps them: `{key, at, cents, model}`. An event
/// with no charge, or one whose time or charge is not a usable number, is
/// left out - a charge placed at a guessed time is a guess.
pub fn parse_cursor_charge_rows(events: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let num = |v: &serde_json::Value| match v {
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        v => v.as_f64(),
    };
    events
        .iter()
        .filter_map(|e| {
            let ms = num(&e["timestamp"]).filter(|m| m.is_finite() && *m > 0.0)?;
            let cents = num(&e["chargedCents"]).filter(|c| c.is_finite() && *c > 0.0)?;
            let model = e["model"].as_str().filter(|m| !m.is_empty());
            Some(serde_json::json!({
                "key": event_key(e),
                "at": ms / 1000.0,
                "cents": cents,
                "model": model,
            }))
        })
        .collect()
}

/// The charges in a Cursor event report's `charges` rows, for `pool_key`.
pub fn parse_cursor_charges(report: &serde_json::Value, pool_key: &str) -> Vec<Charge> {
    report["charges"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let at = r["at"].as_f64().filter(|a| a.is_finite() && *a > 0.0)?;
            let cents = r["cents"].as_f64().filter(|c| c.is_finite() && *c > 0.0)?;
            let key = r["key"].as_str().filter(|k| !k.is_empty())?;
            Some(Charge {
                pool_key: pool_key.to_string(),
                event_key: key.to_string(),
                at,
                amount: cents / 100.0,
                model: r["model"].as_str().map(str::to_string),
            })
        })
        .collect()
}

/// The per-request charges carried by one observation, one per key.
pub fn events_of(obs: &Observation) -> Vec<Charge> {
    let mut out: Vec<Charge> = Vec::new();
    for c in &obs.charges {
        if !(c.amount.is_finite() && c.amount > 0.0 && c.at > 0.0) || c.event_key.is_empty() {
            continue;
        }
        if out.iter().any(|o| o.pool_key == c.pool_key && o.event_key == c.event_key) {
            continue;
        }
        out.push(c.clone());
    }
    out
}

/// Which included pools each overage or balance pool of one observation
/// serves, as `(overage pool key, included pool key)` pairs.
///
/// Spend past the plan serves every allowance of the same account (Claude's
/// extra usage serves every lane), except where the overage is one
/// allowance's own: Copilot's `premium_interactions:overage` is the
/// requests past `premium_interactions` and serves only that.
pub fn links_of(obs: &Observation) -> Vec<(String, String)> {
    let keyed = |l: &&crate::shared::Lane| !l.pool_key.is_empty();
    let included: Vec<&str> = obs
        .lanes
        .iter()
        .filter(keyed)
        .filter(|l| l.role == PoolRole::Included)
        .map(|l| l.pool_key.as_str())
        .collect();
    let mut out = Vec::new();
    for over in obs.lanes.iter().filter(keyed).filter(|l| l.role != PoolRole::Included) {
        // One allowance's own overage serves that allowance only, and only
        // once it is in the reading: a pass that left it out links nothing,
        // rather than treating the overage as account-wide.
        let own = over.pool_key.strip_suffix(":overage");
        for inc in &included {
            if own.is_none_or(|k| k == *inc) && !out.iter().any(|(o, i)| o == &over.pool_key && i == inc) {
                out.push((over.pool_key.clone(), inc.to_string()));
            }
        }
    }
    out
}

fn sql(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Write one observation's `pool_link` pairs and `overage_event` rows under
/// `account`. A link not there before puts its overage pool on `refold` from
/// the start, so spans folded before the link was known are attributed.
pub fn write_links_and_charges(
    tx: &Transaction,
    account: i64,
    obs: &Observation,
    refold: &mut Vec<(i64, f64)>,
) -> Result<(), String> {
    let id = |key: &str| -> Result<Option<i64>, String> {
        tx.query_row(
            "SELECT id FROM pool WHERE account_id = ?1 AND pool_key = ?2",
            params![account, key],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)
    };
    for (over, inc) in links_of(obs) {
        let (Some(oi), Some(ii)) = (id(&over)?, id(&inc)?) else {
            continue;
        };
        let added = tx
            .execute(
                "INSERT OR IGNORE INTO pool_link(overage_id, included_id) VALUES (?1, ?2)",
                params![oi, ii],
            )
            .map_err(sql)?;
        if added > 0 {
            refold.push((oi, f64::NEG_INFINITY));
        }
    }
    for c in events_of(obs) {
        if let Some(pid) = id(&c.pool_key)? {
            tx.execute(
                "INSERT OR IGNORE INTO overage_event(overage_id, event_key, at, amount, model) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![pid, c.event_key, c.at, c.amount, c.model],
            )
            .map_err(sql)?;
        }
    }
    Ok(())
}

/// For every included pool on `refold`, its overage and balance pools too,
/// from the same point - but only where a span already stored reaches that
/// point: a reading later than every span changes none of them, and the
/// next span is folded against it anyway.
pub fn refold_linked(tx: &Transaction, refold: &mut Vec<(i64, f64)>) -> Result<(), String> {
    let mut more = Vec::new();
    let mut st = tx
        .prepare(
            "SELECT l.overage_id FROM pool_link l
              WHERE l.included_id = ?1
                AND EXISTS (SELECT 1 FROM overage_span s WHERE s.overage_id = l.overage_id AND s.at_hi >= ?2)",
        )
        .map_err(sql)?;
    for &(pool, from) in refold.iter() {
        let rows = st.query_map(params![pool, from], |r| r.get::<_, i64>(0)).map_err(sql)?;
        for over in rows {
            more.push((over.map_err(sql)?, from));
        }
    }
    refold.extend(more);
    Ok(())
}

/// The read side, for the charts (phase 3), which is its only reader: the
/// allowance below goes when the charts call it.
#[allow(dead_code)]
pub mod read {
    use rusqlite::{OptionalExtension, params};

    use super::super::fold::{Cycle, CycleModel, EndReason, StartSource};
    use super::super::store::History;
    use super::{Charge, Move, MoveKind, Span, balance_moves, expiries, sql};

    /// An undated pool's boundary counts towards [`roll_day`] only when it was
    /// pinned to within this long: a roll seen after a three-day gap names no
    /// day.
    const ROLL_BRACKET: f64 = 36.0 * 3600.0;

    /// The day an undated overage pool has been seen to roll on (Claude's extra
    /// usage, which states no reset).
    #[derive(Clone, Debug, PartialEq)]
    pub struct RollDay {
        /// Day of the month, in the calendar `day_of` read it in.
        pub day: u32,
        /// How many observed boundaries agree on it: at least two.
        pub seen: usize,
    }

    /// The day of the month an undated pool rolls on, once at least two
    /// observed boundaries - each pinned to within [`ROLL_BRACKET`] - fall on
    /// the same day (or the next, a bracket can straddle midnight). For
    /// display only: it is what was seen, and nothing projects a countdown from
    /// it. `None` before then, or when the boundaries disagree.
    pub fn roll_day(cycles: &[Cycle], day_of: impl Fn(f64) -> u32) -> Option<RollDay> {
        let mut days = Vec::new();
        for pair in cycles.windows(2) {
            let (p, c) = (&pair[0], &pair[1]);
            let observed = c.model == CycleModel::Undated
                && c.start_source == StartSource::Inferred
                && p.end_reason == Some(EndReason::Scheduled);
            let pinned = p.last_obs_at.is_some_and(|lo| c.starts_at - lo <= ROLL_BRACKET);
            if observed && pinned {
                days.push(day_of(c.starts_at));
            }
        }
        let last = *days.last()?;
        let agree = days.iter().all(|d| d.abs_diff(last) <= 1);
        (days.len() >= 2 && agree).then_some(RollDay {
            day: last,
            seen: days.len(),
        })
    }

    /// A span as an included cycle sees it.
    #[derive(Clone, Debug, PartialEq)]
    pub struct AttributedSpan {
        /// The overage or balance pool it was drawn from.
        pub overage_pool_key: String,
        /// `money`, `requests`, `credits`... and the currency for money.
        pub unit: String,
        pub currency: Option<String>,
        pub span: Span,
        /// The bracket began before the included cycle did, or ended after it:
        /// the span is shown against both cycles as "spans the reset", never
        /// split between them by time.
        pub spans_reset: bool,
        /// The other included pools exhausted at the time, which this span is
        /// also shown against. Reports count it once.
        pub shared_with: Vec<i64>,
    }

    /// An overage pool's own cycle with what was spent in it.
    #[derive(Clone, Debug, PartialEq)]
    pub struct OverageCycle {
        pub cycle: Cycle,
        /// The sum of the spans whose later reading falls in the cycle, in the
        /// pool's unit. A lower bound when coverage has gaps: spend before the
        /// first reading after a roll is in the next reading's span.
        pub spent: f64,
        /// The part of `spent` drawn while no linked included pool was
        /// exhausted.
        pub unattributed: f64,
        pub spans: Vec<Span>,
    }

    /// A balance's moves over one calendar month (local time). A move is
    /// placed in the month of its later reading; one whose bracket began in an
    /// earlier month is counted there and in `straddling`, never split.
    #[derive(Clone, Debug, PartialEq)]
    pub struct MonthTotal {
        pub year: i32,
        pub month: u32,
        /// `Nov 2026 (calendar month)`: a balance has no cycle, so a month on a
        /// chart is the calendar's and says so.
        pub label: String,
        pub spent: f64,
        pub topped_up: f64,
        pub expired: f64,
        pub straddling: usize,
    }

    /// The moves of a balance totalled per month, oldest first. `month_of`
    /// gives the (year, month) an instant falls in.
    pub fn month_totals(moves: &[Move], month_of: impl Fn(f64) -> (i32, u32)) -> Vec<MonthTotal> {
        let mut out: Vec<MonthTotal> = Vec::new();
        let mut last: Option<(f64, f64)> = None;
        for m in moves {
            let (year, month) = month_of(m.at_hi);
            if out.last().is_none_or(|t| (t.year, t.month) != (year, month)) {
                out.push(MonthTotal {
                    year,
                    month,
                    label: format!("{} {year} (calendar month)", month_name(month)),
                    spent: 0.0,
                    topped_up: 0.0,
                    expired: 0.0,
                    straddling: 0,
                });
            }
            let t = out.last_mut().expect("pushed above");
            match m.kind {
                MoveKind::Spend => t.spent += m.amount,
                MoveKind::TopUp => t.topped_up += m.amount,
                MoveKind::Expiry => t.expired += m.amount,
            }
            // One bracket split into expiry and spend straddles once.
            if month_of(m.at_lo) != (year, month) && last != Some((m.at_lo, m.at_hi)) {
                t.straddling += 1;
            }
            last = Some((m.at_lo, m.at_hi));
        }
        out
    }

    fn month_name(m: u32) -> &'static str {
        const NAMES: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        NAMES.get(m.saturating_sub(1) as usize).copied().unwrap_or("?")
    }

    fn local_month(at: f64) -> (i32, u32) {
        use chrono::{Datelike, Local, TimeZone};
        match Local.timestamp_opt(at.trunc() as i64, 0).single() {
            Some(t) => (t.year(), t.month()),
            None => (0, 0),
        }
    }

    fn local_day(at: f64) -> u32 {
        use chrono::{Datelike, Local, TimeZone};
        Local
            .timestamp_opt(at.trunc() as i64, 0)
            .single()
            .map_or(0, |t| t.day())
    }

    /// The read side, for the charts (phase 3). Every figure is in the pool's
    /// own unit; nothing here is a percentage of an allowance it is not part of.
    impl History {
        /// A pool's id by provider, account key and the provider's pool key.
        fn overage_pool(&self, provider: &str, account_key: &str, pool_key: &str) -> Result<Option<i64>, String> {
            self.conn
                .query_row(
                    "SELECT p.id FROM pool p JOIN account a ON a.id = p.account_id
                      WHERE a.provider = ?1 AND a.account_key = ?2 AND p.pool_key = ?3",
                    params![provider, account_key, pool_key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql)
        }

        fn spans_of(&self, overage_id: i64) -> Result<Vec<Span>, String> {
            self.spans_between(overage_id, f64::NEG_INFINITY, f64::INFINITY)
        }

        /// The spans whose bracket overlaps `start..end`, each whole, oldest
        /// first: filtered by the query, so a long history's old spans are
        /// never loaded and decoded for a recent cycle.
        fn spans_between(&self, overage_id: i64, start: f64, end: f64) -> Result<Vec<Span>, String> {
            let mut st = self
                .conn
                .prepare(
                    "SELECT at_lo, at_hi, amount, exhausted FROM overage_span
                      WHERE overage_id = ?1 AND at_hi > ?2 AND at_lo < ?3 ORDER BY at_hi",
                )
                .map_err(sql)?;
            let rows = st
                .query_map(params![overage_id, start, end], |r| {
                    let exhausted: String = r.get(3)?;
                    Ok(Span {
                        at_lo: r.get(0)?,
                        at_hi: r.get(1)?,
                        amount: r.get(2)?,
                        exhausted: serde_json::from_str(&exhausted).unwrap_or_default(),
                    })
                })
                .map_err(sql)?;
            rows.collect::<Result<_, _>>().map_err(sql)
        }

        /// **The spend attributed to one included cycle**: every span from an
        /// overage or balance pool linked to `included_pool_key` that was drawn
        /// while that pool was exhausted (its id in the span's `exhausted`) and
        /// whose bracket overlaps `cycle` - from its start to its close, its
        /// scheduled end, or for ever while open. Each span is whole: one that
        /// crosses the cycle's boundary is returned for both cycles with
        /// `spans_reset` set. Oldest first. Draw it stacked above the 100% line
        /// in its own unit; never sum different pools' units or currencies.
        pub fn spend_in_cycle(
            &self,
            provider: &str,
            account_key: &str,
            included_pool_key: &str,
            cycle: &Cycle,
        ) -> Result<Vec<AttributedSpan>, String> {
            let start = cycle.starts_at;
            let end = cycle.closed_at.or(cycle.ends_at).unwrap_or(f64::INFINITY);
            self.spend_overlapping(provider, account_key, included_pool_key, start, end)
        }

        /// [`History::spend_in_cycle`] for any stretch `start..end`: every
        /// span drawn while `included_pool_key` was exhausted whose bracket
        /// overlaps it, whole, with `spans_reset` set when it reaches past
        /// either end. The charts read a whole displayed range with one call
        /// and hand each cycle the spans that overlap it.
        pub fn spend_overlapping(
            &self,
            provider: &str,
            account_key: &str,
            included_pool_key: &str,
            start: f64,
            end: f64,
        ) -> Result<Vec<AttributedSpan>, String> {
            let Some(inc) = self.overage_pool(provider, account_key, included_pool_key)? else {
                return Ok(Vec::new());
            };
            let overs: Vec<(i64, String, String, Option<String>)> = {
                let mut st = self
                    .conn
                    .prepare(
                        "SELECT p.id, p.pool_key, p.unit, p.currency FROM pool_link l JOIN pool p ON p.id = l.overage_id
                          WHERE l.included_id = ?1 ORDER BY p.id",
                    )
                    .map_err(sql)?;
                let rows = st
                    .query_map([inc], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                    .map_err(sql)?;
                rows.collect::<Result<_, _>>().map_err(sql)?
            };
            let mut out = Vec::new();
            for (id, key, unit, currency) in overs {
                for span in self.spans_between(id, start, end)? {
                    if !span.exhausted.contains(&inc) {
                        continue;
                    }
                    out.push(AttributedSpan {
                        overage_pool_key: key.clone(),
                        unit: unit.clone(),
                        currency: currency.clone(),
                        spans_reset: span.at_lo < start || span.at_hi > end,
                        shared_with: span.exhausted.iter().copied().filter(|e| *e != inc).collect(),
                        span,
                    });
                }
            }
            out.sort_by(|a, b| a.span.at_hi.total_cmp(&b.span.at_hi));
            Ok(out)
        }

        /// **An overage pool's own cycles with their totals**, oldest first, on
        /// the pool's own clock (monthly for every provider today; Claude's
        /// inferred from the spend returning to zero). Each span is counted once,
        /// in the cycle its later reading falls in. Empty for a balance, which
        /// has no cycles: see [`History::balance_months`].
        pub fn overage_cycles(&self, provider: &str, account_key: &str, pool_key: &str) -> Result<Vec<OverageCycle>, String> {
            let Some(id) = self.overage_pool(provider, account_key, pool_key)? else {
                return Ok(Vec::new());
            };
            let spans = self.spans_of(id)?;
            let cycles = self.cycles(provider, account_key, pool_key)?;
            let mut out = Vec::new();
            for (i, c) in cycles.iter().enumerate() {
                let end = c
                    .closed_at
                    .or_else(|| cycles.get(i + 1).map(|n| n.starts_at))
                    .unwrap_or(f64::INFINITY);
                let mine: Vec<Span> = spans
                    .iter()
                    .filter(|s| s.at_hi >= c.starts_at && s.at_hi < end)
                    .cloned()
                    .collect();
                out.push(OverageCycle {
                    spent: mine.iter().map(|s| s.amount).sum(),
                    unattributed: mine.iter().filter(|s| s.exhausted.is_empty()).map(|s| s.amount).sum(),
                    cycle: c.clone(),
                    spans: mine,
                });
            }
            Ok(out)
        }

        /// **A balance's spend, top-ups and expiries per calendar month**
        /// (local time), oldest first, labelled as calendar months because a
        /// balance has no cycle of its own.
        pub fn balance_months(&self, provider: &str, account_key: &str, pool_key: &str) -> Result<Vec<MonthTotal>, String> {
            let samples = self.samples(provider, account_key, pool_key)?;
            Ok(month_totals(&balance_moves(&samples, &expiries(&samples)), local_month))
        }

        /// **The exact per-request charges** of an overage pool between `from`
        /// and `to` (epoch seconds, inclusive), oldest first: Cursor's and
        /// SpaceXSI's, each at its own instant, so it lands in exactly one
        /// included cycle. Compare with the cycle's `exhausted_at` for how long
        /// after exhaustion the spend began.
        pub fn charges(&self, provider: &str, account_key: &str, pool_key: &str, from: f64, to: f64) -> Result<Vec<Charge>, String> {
            let Some(id) = self.overage_pool(provider, account_key, pool_key)? else {
                return Ok(Vec::new());
            };
            let mut st = self
                .conn
                .prepare(
                    "SELECT event_key, at, amount, model FROM overage_event
                      WHERE overage_id = ?1 AND at >= ?2 AND at <= ?3 ORDER BY at, event_key",
                )
                .map_err(sql)?;
            let rows = st
                .query_map(params![id, from, to], |r| {
                    Ok(Charge {
                        pool_key: pool_key.to_string(),
                        event_key: r.get(0)?,
                        at: r.get(1)?,
                        amount: r.get(2)?,
                        model: r.get(3)?,
                    })
                })
                .map_err(sql)?;
            rows.collect::<Result<_, _>>().map_err(sql)
        }

        /// **The day an undated overage pool rolls on**, in the local calendar,
        /// once two observed boundaries agree ([`roll_day`]). Display only.
        pub fn overage_roll_day(&self, provider: &str, account_key: &str, pool_key: &str) -> Result<Option<RollDay>, String> {
            Ok(roll_day(&self.cycles(provider, account_key, pool_key)?, local_day))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::read::*;
    use super::*;
    use crate::history::fold::{self, CycleModel, FoldInput, Source, StartSource};
    use crate::history::store::History;
    use crate::shared::{Lane, Unit};

    const H: f64 = 3600.0;
    const DAY: f64 = 24.0 * H;
    const WEEK: f64 = 7.0 * DAY;
    const MONTH: f64 = 30.0 * DAY;

    fn money(at: f64, used: f64, limit: f64, reset: Option<f64>, window: Option<f64>) -> Sample {
        Sample {
            at,
            pct: Some(100.0 * used / limit),
            used: Some(used),
            limit: Some(limit),
            window_secs: window,
            reset_at: reset,
            source: Source::Live,
            spacexsi: None,
        }
    }

    fn pct(at: f64, p: f64, reset: f64) -> Sample {
        Sample {
            at,
            pct: Some(p),
            used: None,
            limit: None,
            window_secs: Some(WEEK),
            reset_at: Some(reset),
            source: Source::Live,
            spacexsi: None,
        }
    }

    fn t() -> Thresholds {
        Thresholds::default()
    }

    #[test]
    fn a_reset_still_stated_after_it_passed_rolls_once() {
        // CodeRabbit's case: 40 before the stated reset T, then 41 and 42
        // after it, both still stating T. Each pair read as a roll and
        // booked its whole cumulative spend, 41 + 42 = 83, where the new
        // cycle has spent 42.
        let a = 1_000_000.0;
        let r = a + MONTH;
        let over = vec![
            money(r - DAY, 40.0, 50.0, Some(r), Some(MONTH)),
            money(r + DAY, 41.0, 50.0, Some(r), Some(MONTH)),
            money(r + 2.0 * DAY, 42.0, 50.0, Some(r), Some(MONTH)),
        ];
        let out = spans(Role::Overage, &over, &[], &t());
        let total: f64 = out.iter().map(|s| s.amount).sum();
        assert!((total - 42.0).abs() < 1e-9, "{out:?}");
        // A roll to a later stated reset still books the new cycle whole.
        let over = vec![
            money(r + DAY, 41.0, 50.0, Some(r), Some(MONTH)),
            money(r + 2.0 * DAY, 3.0, 50.0, Some(r + MONTH), Some(MONTH)),
        ];
        let out = spans(Role::Overage, &over, &[], &t());
        assert_eq!(out.iter().map(|s| s.amount).sum::<f64>(), 3.0);
    }

    #[test]
    fn overage_with_one_exhausted_pool_carries_the_whole_amount_and_that_pool() {
        let a = 1_000_000.0;
        let r = a + MONTH;
        let over = vec![
            money(a, 0.0, 50.0, Some(r), Some(MONTH)),
            money(a + H, 4.10, 50.0, Some(r), Some(MONTH)),
        ];
        let weekly = vec![pct(a, 100.0, a + 3.0 * DAY), pct(a + H, 100.0, a + 3.0 * DAY)];
        let session = vec![pct(a, 40.0, a + 2.0 * H), pct(a + H, 60.0, a + 2.0 * H)];
        let inc = [
            Included { pool_id: 7, samples: &weekly },
            Included { pool_id: 8, samples: &session },
        ];
        let out = spans(Role::Overage, &over, &inc, &t());
        assert_eq!(out.len(), 1);
        assert!((out[0].amount - 4.10).abs() < 1e-9);
        assert_eq!((out[0].at_lo, out[0].at_hi), (a, a + H));
        assert_eq!(out[0].exhausted, vec![7]);
    }

    #[test]
    fn overage_with_two_exhausted_pools_is_listed_against_both_and_not_split() {
        let a = 1_000_000.0;
        let over = vec![money(a, 10.0, 50.0, None, None), money(a + H, 16.0, 50.0, None, None)];
        let weekly = vec![pct(a, 100.0, a + 3.0 * DAY)];
        let model = vec![pct(a + 30.0 * 60.0, 99.6, a + 3.0 * DAY)];
        let inc = [
            Included { pool_id: 3, samples: &model },
            Included { pool_id: 2, samples: &weekly },
        ];
        let out = spans(Role::Overage, &over, &inc, &t());
        assert_eq!(out.len(), 1);
        // The full delta, once, with both pools - not 3.0 apiece.
        assert!((out[0].amount - 6.0).abs() < 1e-9);
        assert_eq!(out[0].exhausted, vec![2, 3]);
    }

    #[test]
    fn spend_while_nothing_was_exhausted_stays_unattributed() {
        let a = 1_000_000.0;
        let over = vec![money(a, 1.0, 50.0, None, None), money(a + H, 3.0, 50.0, None, None)];
        let weekly = vec![pct(a, 80.0, a + 3.0 * DAY)];
        // A reading at 100% whose own week had closed before the span.
        let old = vec![pct(a - 2.0 * DAY, 100.0, a - DAY)];
        let inc = [
            Included { pool_id: 1, samples: &weekly },
            Included { pool_id: 2, samples: &old },
        ];
        let out = spans(Role::Overage, &over, &inc, &t());
        assert_eq!(out.len(), 1);
        assert!(out[0].exhausted.is_empty());
        // And with no linked pool at all.
        assert!(spans(Role::Overage, &over, &[], &t())[0].exhausted.is_empty());
    }

    #[test]
    fn a_fall_that_is_not_a_roll_is_no_spend_and_a_percentage_only_reading_makes_none() {
        let a = 1_000_000.0;
        let over = vec![money(a, 10.0, 50.0, None, None), money(a + H, 8.0, 50.0, None, None)];
        assert!(spans(Role::Overage, &over, &[], &t()).is_empty());
        let mut only_pct = money(a + 2.0 * H, 0.0, 50.0, None, None);
        only_pct.used = None;
        only_pct.limit = None;
        only_pct.pct = Some(40.0);
        let out = spans(Role::Overage, &[over[1].clone(), only_pct], &[], &t());
        assert!(out.is_empty(), "never a percentage: {out:?}");
    }

    #[test]
    fn claude_extra_usage_rolls_where_its_spend_returns_to_zero() {
        let a = 1_000_000.0;
        let day = |n: f64| a + n * DAY;
        // Undated: no reset instant anywhere.
        let samples = vec![
            money(day(0.0), 5.0, 50.0, None, None),
            money(day(10.0), 30.0, 50.0, None, None),
            // The month rolled between these two: back to zero.
            money(day(14.0), 30.0, 50.0, None, None),
            money(day(14.0) + H, 0.0, 50.0, None, None),
            money(day(20.0), 7.0, 50.0, None, None),
            money(day(44.0), 20.0, 50.0, None, None),
            money(day(44.0) + H, 0.2, 50.0, None, None),
        ];
        let sp = spans(Role::Overage, &samples, &[], &t());
        let amounts: Vec<f64> = sp.iter().map(|s| (s.amount * 100.0).round() / 100.0).collect();
        // 25 spent, the roll (nothing), 7, 13, then the 0.20 after the roll.
        assert_eq!(amounts, vec![25.0, 7.0, 13.0, 0.2]);
        let out = fold::fold(&FoldInput {
            role: Role::Overage,
            samples: &samples,
            plans: &[],
            resets: &[],
            prev: None,
            next_ordinal: 1,
            rolling_seed: None,
            resume: None,
            gone_at: None,
            thresholds: t(),
        });
        assert_eq!(out.cycles.len(), 3);
        for c in &out.cycles {
            assert_eq!(c.model, CycleModel::Undated);
            assert_eq!(c.start_source, StartSource::Inferred);
            // No countdown nobody sent.
            assert_eq!(c.ends_at, None);
        }
        assert_eq!(out.cycles[1].starts_at, day(14.0) + H);
        // Two boundaries, a month apart: a roll day, for display only.
        let rd = roll_day(&out.cycles, |at| (((at - a) / DAY) as u32 % 30) + 1).unwrap();
        assert_eq!(rd.seen, 2);
        assert_eq!(rd.day, 15);
        // One boundary is not enough.
        assert_eq!(roll_day(&out.cycles[..2], |_| 15), None);
    }

    #[test]
    fn a_roll_seen_only_after_a_long_gap_names_no_day() {
        let a = 1_000_000.0;
        let samples = vec![
            money(a, 10.0, 50.0, None, None),
            money(a + 5.0 * DAY, 0.0, 50.0, None, None),
            money(a + 20.0 * DAY, 9.0, 50.0, None, None),
            money(a + 20.0 * DAY + H, 0.0, 50.0, None, None),
        ];
        let out = fold::fold(&FoldInput {
            role: Role::Overage,
            samples: &samples,
            plans: &[],
            resets: &[],
            prev: None,
            next_ordinal: 1,
            rolling_seed: None,
            resume: None,
            gone_at: None,
            thresholds: t(),
        });
        assert_eq!(out.cycles.len(), 3);
        assert_eq!(roll_day(&out.cycles, |_| 1), None);
    }

    #[test]
    fn a_dated_overage_rolls_on_the_server_boundary_and_the_new_months_spend_starts_there() {
        let a = 1_000_000.0;
        let r1 = a + DAY;
        let r2 = r1 + MONTH;
        let samples = vec![
            money(a, 40.0, 50.0, Some(r1), Some(MONTH)),
            // Next month, after a gap across the boundary.
            money(r1 + 2.0 * DAY, 3.0, 50.0, Some(r2), Some(MONTH)),
        ];
        let out = spans(Role::Overage, &samples, &[], &t());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].at_lo, r1);
        assert!((out[0].amount - 3.0).abs() < 1e-9);
    }

    #[test]
    fn spend_read_in_the_last_hours_before_a_monthly_end_is_not_booked_again() {
        // A monthly tolerance is 7.2 hours: readings inside it still state
        // the same end, so they are the same month, not a roll each.
        let e = 10_000_000.0;
        let reading = |at: f64, used: f64| money(at, used, 100.0, Some(e), Some(MONTH));
        let samples = vec![
            reading(e - 20.0 * H, 0.0),
            reading(e - 10.0 * H, 40.0),
            reading(e - 3.0 * H, 41.0),
            reading(e - 2.0 * H, 42.0),
            reading(e - H, 43.0),
        ];
        let out = spans(Role::Overage, &samples, &[], &t());
        let total: f64 = out.iter().map(|s| s.amount).sum();
        assert!((total - 43.0).abs() < 1e-9, "{out:?}");
        // The month that follows starts its own spend at the boundary.
        let mut next = samples.clone();
        next.push(money(e + H, 2.0, 100.0, Some(e + MONTH), Some(MONTH)));
        let out = spans(Role::Overage, &next, &[], &t());
        let last = out.last().unwrap();
        assert_eq!((last.at_lo, last.amount), (e, 2.0));
    }

    #[test]
    fn copilot_overage_is_counted_in_requests() {
        let a = 1_000_000.0;
        let r = a + MONTH;
        let req = |at: f64, n: f64| Sample {
            at,
            pct: None,
            used: Some(n),
            limit: None,
            window_secs: Some(MONTH),
            reset_at: Some(r),
            source: Source::Live,
            spacexsi: None,
        };
        let premium = vec![pct(a, 100.0, r)];
        let inc = [Included { pool_id: 4, samples: &premium }];
        let out = spans(Role::Overage, &[req(a, 0.0), req(a + H, 12.0)], &inc, &t());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].amount, 12.0);
        assert_eq!(out[0].exhausted, vec![4]);
    }

    #[test]
    fn a_balance_reads_spend_top_up_and_expiry_apart() {
        let a = 1_000_000.0;
        // A balance with no ceiling states what is left as its limit.
        let left = |at: f64, n: f64, expiry: Option<f64>| Sample {
            at,
            pct: None,
            used: None,
            limit: Some(n),
            window_secs: None,
            reset_at: expiry,
            source: Source::Live,
            spacexsi: None,
        };
        let expiry = a + 3.0 * H + 60.0;
        let samples = vec![
            left(a, 40.0, Some(expiry)),
            left(a + H, 35.0, Some(expiry)),       // spend 5
            left(a + 2.0 * H, 60.0, Some(expiry)), // top-up 25
            left(a + 3.0 * H, 60.0, Some(expiry)),
            left(a + 4.0 * H, 50.0, None), // the grant expired: 10
        ];
        let moves = balance_moves(&samples, &expiries(&samples));
        let got: Vec<(MoveKind, f64)> = moves.iter().map(|m| (m.kind, m.amount)).collect();
        assert_eq!(
            got,
            vec![(MoveKind::Spend, 5.0), (MoveKind::TopUp, 25.0), (MoveKind::Expiry, 10.0)]
        );
        // Only spend becomes a span.
        let sp = spans(Role::Balance, &samples, &[], &t());
        assert_eq!(sp.len(), 1);
        assert_eq!(sp[0].amount, 5.0);
        // A ceiling-and-used balance (Cursor's prepaid): paid 100, spent 30.
        let prepaid = |at: f64, used: f64, paid: f64| Sample {
            used: Some(used),
            limit: Some(paid),
            ..left(at, paid, None)
        };
        let m = balance_moves(&[prepaid(a, 20.0, 100.0), prepaid(a + H, 30.0, 100.0), prepaid(a + 2.0 * H, 30.0, 150.0)], &[]);
        assert_eq!(m.iter().map(|m| (m.kind, m.amount)).collect::<Vec<_>>(), vec![(MoveKind::Spend, 10.0), (MoveKind::TopUp, 50.0)]);
        // Across an expiry, a ceiling balance's expiry is at most what left
        // its limit: 300 -> 200 with 30 more used is 100 expired, 30 spent;
        // a limit that held is no grant gone, so its fall is all spend.
        let across = [a + 30.0 * 60.0];
        let m = balance_moves(&[prepaid(a, 0.0, 300.0), prepaid(a + H, 30.0, 200.0)], &across);
        assert_eq!(m.iter().map(|m| (m.kind, m.amount)).collect::<Vec<_>>(), vec![(MoveKind::Expiry, 100.0), (MoveKind::Spend, 30.0)]);
        let m = balance_moves(&[prepaid(a, 0.0, 300.0), prepaid(a + H, 30.0, 300.0)], &across);
        assert_eq!(m.iter().map(|m| (m.kind, m.amount)).collect::<Vec<_>>(), vec![(MoveKind::Spend, 30.0)]);
        let split = month_totals(
            &balance_moves(&[prepaid(a, 0.0, 300.0), prepaid(a + H, 30.0, 200.0)], &across),
            |at| if at < a + 0.5 * H { (2026, 10) } else { (2026, 11) },
        );
        assert_eq!(split[0].straddling, 1, "one bracket, split in two, straddles once");
        // Totalled per calendar month, labelled as such.
        let totals = month_totals(&moves, |at| if at < a + 1.5 * H { (2026, 10) } else { (2026, 11) });
        assert_eq!(totals.len(), 2);
        assert_eq!(totals[0].label, "Oct 2026 (calendar month)");
        assert_eq!(totals[0].spent, 5.0);
        assert_eq!((totals[1].topped_up, totals[1].expired, totals[1].straddling), (25.0, 10.0, 1));
    }

    #[test]
    fn a_span_crossing_the_weekly_reset_keeps_its_bracket_and_lists_the_pool() {
        let a = 1_000_000.0;
        let week_end = a + 2.0 * H;
        // The weekly pool at 100% until its reset, then a new week at 0%.
        let weekly = vec![pct(a, 100.0, week_end), pct(a + 4.0 * H, 0.0, week_end + WEEK)];
        let over = vec![money(a, 10.0, 50.0, None, None), money(a + 4.0 * H, 14.0, 50.0, None, None)];
        let inc = [Included { pool_id: 9, samples: &weekly }];
        let out = spans(Role::Overage, &over, &inc, &t());
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].at_lo, out[0].at_hi, out[0].amount), (a, a + 4.0 * H, 4.0));
        assert_eq!(out[0].exhausted, vec![9]);
    }

    #[test]
    fn cursor_charges_are_kept_once_each_with_a_stable_key() {
        let e = serde_json::json!({"timestamp": "1763510400000", "model": "m", "chargedCents": "410",
            "tokenUsage": {"inputTokens": 5}});
        let free = serde_json::json!({"timestamp": "1763510500000", "model": "m", "chargedCents": 0});
        let undated = serde_json::json!({"model": "m", "chargedCents": 9});
        let rows = parse_cursor_charge_rows(&[e.clone(), free, undated, e.clone()]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["key"], rows[1]["key"]);
        let report = serde_json::json!({"charges": rows});
        let charges = parse_cursor_charges(&report, "usage_limit");
        assert_eq!(charges[0].amount, 4.10);
        assert_eq!(charges[0].at, 1_763_510_400.0);
        assert_eq!(charges[0].pool_key, "usage_limit");
        let obs = Observation {
            charges: charges.clone(),
            ..Observation::default()
        };
        // The same event twice in one pass is one row.
        assert_eq!(events_of(&obs).len(), 1);
        // A different event is a different key.
        let other = serde_json::json!({"timestamp": "1763510400001", "model": "m", "chargedCents": "410",
            "tokenUsage": {"inputTokens": 5}});
        assert_ne!(event_key(&e), event_key(&other));
    }

    #[test]
    fn extra_usage_serves_every_lane_and_copilot_overage_only_its_own() {
        let lane = |key: &str, role: PoolRole| Lane {
            pool_key: key.into(),
            role,
            ..Lane::default()
        };
        let claude = Observation {
            lanes: vec![
                lane("five_hour", PoolRole::Included),
                lane("seven_day", PoolRole::Included),
                lane("extra_usage", PoolRole::Overage),
            ],
            ..Observation::default()
        };
        assert_eq!(
            links_of(&claude),
            vec![
                ("extra_usage".to_string(), "five_hour".to_string()),
                ("extra_usage".to_string(), "seven_day".to_string())
            ]
        );
        let copilot = Observation {
            lanes: vec![
                lane("chat", PoolRole::Included),
                lane("premium_interactions", PoolRole::Included),
                Lane {
                    unit: Unit::Requests,
                    ..lane("premium_interactions:overage", PoolRole::Overage)
                },
            ],
            ..Observation::default()
        };
        assert_eq!(
            links_of(&copilot),
            vec![("premium_interactions:overage".to_string(), "premium_interactions".to_string())]
        );
        assert!(links_of(&Observation::default()).is_empty());
    }

    // ===== Through the file: written by `observe`, re-folded, read back =====

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "opscope-overage-{name}-{}-{}",
            std::process::id(),
            opscope_core::now().to_bits()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(super::super::store::FILE)
    }

    fn weekly(p: f64, at: f64, reset: f64) -> Lane {
        Lane {
            label: "overall 7d".into(),
            pct: p,
            window_secs: Some(WEEK),
            reset: Some(reset),
            pool_key: "seven_day".into(),
            fetched_at: at,
            ..Lane::default()
        }
    }

    fn extra(used: f64, at: f64) -> Lane {
        Lane {
            label: "extra A$50".into(),
            pct: 100.0 * used / 50.0,
            pool_key: "extra_usage".into(),
            unit: Unit::Money {
                currency: "AUD".into(),
                exponent: 2,
            },
            role: PoolRole::Overage,
            used: Some(used),
            limit: Some(50.0),
            fetched_at: at,
            apart: true,
            ..Lane::default()
        }
    }

    fn claude(lanes: Vec<Lane>) -> Observation {
        Observation {
            provider: "claude".into(),
            account_id: "acct-overage".into(),
            heading: "CLAUDE".into(),
            tab: "claude".into(),
            lanes,
            ..Observation::default()
        }
    }

    #[test]
    fn a_span_across_the_weekly_reset_is_shown_against_both_weeks_and_counted_once() {
        let p = temp("cross");
        let mut h = History::open(&p).unwrap();
        let a = 1_000_000.0;
        let reset = a + 2.0 * H;
        h.record(&[claude(vec![weekly(100.0, a, reset), extra(10.0, a)])], &t(), a)
            .unwrap();
        // Read again after the reset: a new week, and 4.00 more spent.
        let b = a + 4.0 * H;
        h.record(&[claude(vec![weekly(0.0, b, reset + WEEK), extra(14.0, b)])], &t(), b)
            .unwrap();
        let key = claude(vec![]).account_key();
        let weeks = h.cycles("claude", &key, "seven_day").unwrap();
        assert_eq!(weeks.len(), 2);
        for w in &weeks {
            let got = h.spend_in_cycle("claude", &key, "seven_day", w).unwrap();
            assert_eq!(got.len(), 1, "{w:?}");
            assert!((got[0].span.amount - 4.0).abs() < 1e-9);
            assert!(got[0].spans_reset);
            assert_eq!(got[0].overage_pool_key, "extra_usage");
            assert_eq!(got[0].currency.as_deref(), Some("AUD"));
        }
        // The overage pool's own cycle holds it once.
        let own = h.overage_cycles("claude", &key, "extra_usage").unwrap();
        assert_eq!(own.len(), 1);
        assert!((own[0].spent - 4.0).abs() < 1e-9);
        assert_eq!(own[0].unattributed, 0.0);
        assert_eq!(own[0].cycle.start_source, StartSource::Inferred);
        assert_eq!(h.count("overage_span"), 1);
        assert_eq!(h.count("pool_link"), 1);
    }

    #[test]
    fn an_allowances_own_overage_is_never_linked_to_another_allowance() {
        // CodeRabbit's case: Copilot reports `premium_interactions:overage`
        // and `chat`, and omits `premium_interactions` this pass. The
        // overage is that allowance's own; it is not account-wide, and it
        // waits for its own allowance rather than landing on chat.
        let lane = |key: &str, role: PoolRole| Lane {
            pool_key: key.into(),
            role,
            ..Lane::default()
        };
        let mut copilot = Observation {
            provider: "copilot".into(),
            lanes: vec![lane("chat", PoolRole::Included), lane("premium_interactions:overage", PoolRole::Overage)],
            ..Observation::default()
        };
        assert!(links_of(&copilot).is_empty(), "{:?}", links_of(&copilot));
        copilot.lanes.push(lane("premium_interactions", PoolRole::Included));
        assert_eq!(
            links_of(&copilot),
            vec![("premium_interactions:overage".to_string(), "premium_interactions".to_string())]
        );
    }

    fn credits(left: f64, at: f64, expiry: Option<f64>) -> Lane {
        Lane {
            label: "credits".into(),
            pct: f64::NAN,
            pool_key: "credits".into(),
            unit: Unit::Credits,
            role: PoolRole::Balance,
            limit: Some(left),
            reset: expiry,
            fetched_at: at,
            ..Lane::default()
        }
    }

    #[test]
    fn a_fall_across_an_expiry_stated_before_the_refold_point_is_expiry_not_spend() {
        // CodeRabbit's case: the expiry is stated by a reading before the
        // one an incremental write folds on from, and that one omits it. The
        // write saw only its own window's expiries and booked the fall as
        // spend; the full replay calls it expiry.
        let p = temp("expiry");
        let mut h = History::open(&p).unwrap();
        let a = 1_000_000.0;
        let expiry = a + 3.0 * H;
        let codex = |lane: Lane| Observation {
            provider: "codex".into(),
            account_id: "acct-expiry".into(),
            tab: "codex".into(),
            lanes: vec![lane],
            ..Observation::default()
        };
        h.record(&[codex(credits(100.0, a, Some(expiry)))], &t(), a).unwrap();
        h.record(&[codex(credits(100.0, a + 2.0 * H, None))], &t(), a + 2.0 * H).unwrap();
        h.record(&[codex(credits(80.0, a + 4.0 * H, None))], &t(), a + 4.0 * H).unwrap();
        assert_eq!(h.count("overage_span"), 0, "the fall crossed a stated expiry");
        let key = codex(credits(0.0, 0.0, None)).account_key();
        let months = h.balance_months("codex", &key, "credits").unwrap();
        assert_eq!(months.iter().map(|m| m.spent).sum::<f64>(), 0.0);
        assert_eq!(months.iter().map(|m| m.expired).sum::<f64>(), 20.0);
    }

    #[test]
    fn a_long_span_is_shown_on_every_cycle_it_overlaps_in_the_chart() {
        // CodeRabbit's case: the week was exhausted, then nothing was read
        // for two weeks while extra usage was spent. The span overlaps the
        // exhausted week, an unseen week and the current one; the chart
        // skipped the current one because neither it nor the week before it
        // reached 100%.
        let p = temp("chart-overlap");
        let mut h = History::open(&p).unwrap();
        let a = 1_000_000.0;
        let end = a + 2.0 * H;
        h.record(&[claude(vec![weekly(100.0, a, end), extra(10.0, a)])], &t(), a).unwrap();
        let b = end + WEEK + 2.0 * DAY;
        h.record(&[claude(vec![weekly(5.0, b, end + 2.0 * WEEK), extra(14.0, b)])], &t(), b)
            .unwrap();
        let pass = [claude(vec![weekly(5.0, b, end + 2.0 * WEEK), extra(14.0, b)])];
        let got = crate::history::chart::gather(&h, &pass, &t()).unwrap();
        let pool = got["claude"].pools.iter().find(|p| p.label == "overall 7d").unwrap();
        let crate::history::chart::Body::Cycles(bars) = &pool.body else { panic!("{:?}", pool.body) };
        assert_eq!(bars.len(), 3, "{bars:#?}");
        for bar in bars {
            assert_eq!(bar.over.len(), 1, "{:?}", bar.cycle);
            assert!(bar.over[0].spans_reset);
            assert!((bar.over[0].amount - 4.0).abs() < 1e-9);
        }
    }

    #[test]
    fn a_link_learned_later_and_a_new_detector_both_refold_the_spans() {
        let p = temp("refold");
        let mut h = History::open(&p).unwrap();
        let a = 1_000_000.0;
        // Claude leaves an idle limit out: only extra usage at first, so
        // the spend has nothing to be attributed to.
        h.record(&[claude(vec![extra(10.0, a)])], &t(), a).unwrap();
        h.record(&[claude(vec![extra(14.0, a + H)])], &t(), a + H).unwrap();
        let key = claude(vec![]).account_key();
        let own = h.overage_cycles("claude", &key, "extra_usage").unwrap();
        assert_eq!(own[0].unattributed, 4.0);
        // Another pane's earlier reading surfaces: the week was at 100%
        // inside the span. The link is new, so the spans are folded again.
        let late = a + 30.0 * 60.0;
        h.record(
            &[claude(vec![weekly(100.0, late, a + DAY), extra(14.0, a + H)])],
            &t(),
            a + 2.0 * H,
        )
        .unwrap();
        let own = h.overage_cycles("claude", &key, "extra_usage").unwrap();
        assert_eq!(own[0].unattributed, 0.0);
        assert_eq!(own[0].spans[0].exhausted.len(), 1);
        // A file folded by another detector is folded again on open, spans
        // included: nothing stale survives it.
        h.conn
            .execute("UPDATE overage_span SET amount = 999, exhausted = '[]'", [])
            .unwrap();
        h.conn
            .execute("UPDATE meta SET v = '0' WHERE k = 'detector_version'", [])
            .unwrap();
        drop(h);
        let h = History::open(&p).unwrap();
        let own = h.overage_cycles("claude", &key, "extra_usage").unwrap();
        assert!((own[0].spent - 4.0).abs() < 1e-9);
        assert_eq!(own[0].spans[0].exhausted.len(), 1);
    }

    #[test]
    fn a_charge_seen_on_two_passes_is_one_event_under_the_vendor_and_the_unified_plan() {
        let p = temp("events");
        let mut h = History::open(&p).unwrap();
        let a = 1_763_510_000.0;
        let limit = |used: f64, at: f64| Lane {
            label: "usage limit".into(),
            pct: used,
            window_secs: Some(MONTH),
            reset: Some(a + 10.0 * DAY),
            pool_key: "usage_limit".into(),
            unit: Unit::Money {
                currency: "USD".into(),
                exponent: 2,
            },
            role: PoolRole::Overage,
            used: Some(used),
            limit: Some(100.0),
            fetched_at: at,
            ..Lane::default()
        };
        let e = serde_json::json!({"timestamp": "1763510400000", "model": "m", "chargedCents": "410"});
        let report = serde_json::json!({"charges": parse_cursor_charge_rows(&[e])});
        let obs = |used: f64, at: f64| Observation {
            provider: "cursor".into(),
            account_id: "cursor-acct".into(),
            heading: "SpaceXSI".into(),
            tab: "cursor".into(),
            lanes: vec![limit(used, at)],
            spacexsi_account: Some("sx".into()),
            charges: parse_cursor_charges(&report, "usage_limit"),
            ..Observation::default()
        };
        h.record(&[obs(4.10, a)], &t(), a).unwrap();
        h.record(&[obs(4.10, a + H)], &t(), a + H).unwrap();
        // Once under Cursor, once under SpaceXSI - never twice under either.
        assert_eq!(h.count("overage_event"), 2);
        let got = h
            .charges("spacexsi", "sx", "usage_limit", 0.0, f64::INFINITY)
            .unwrap();
        assert_eq!(got.len(), 1);
        assert!((got[0].amount - 4.10).abs() < 1e-9);
    }

    #[test]
    fn a_balance_lane_records_spend_and_reads_back_per_calendar_month() {
        let p = temp("balance");
        let mut h = History::open(&p).unwrap();
        let a = 1_000_000.0;
        let credits = |left: f64, at: f64| Lane {
            label: "credits".into(),
            pct: f64::NAN,
            pool_key: "credits".into(),
            unit: Unit::Credits,
            role: PoolRole::Balance,
            limit: Some(left),
            fetched_at: at,
            ..Lane::default()
        };
        let five = Lane {
            label: "5h".into(),
            pct: 100.0,
            window_secs: Some(5.0 * H),
            reset: Some(a + 4.0 * H),
            pool_key: "primary_window".into(),
            fetched_at: a,
            ..Lane::default()
        };
        let codex = |lanes: Vec<Lane>| Observation {
            provider: "codex".into(),
            account_id: "codex-acct".into(),
            lanes,
            ..Observation::default()
        };
        h.record(&[codex(vec![five.clone(), credits(40.0, a)])], &t(), a).unwrap();
        h.record(&[codex(vec![credits(30.0, a + H)])], &t(), a + H).unwrap();
        h.record(&[codex(vec![credits(80.0, a + 2.0 * H)])], &t(), a + 2.0 * H)
            .unwrap();
        let key = codex(vec![]).account_key();
        // No cycles for a balance; one span, the spend, against the 5h pool.
        assert!(h.cycles("codex", &key, "credits").unwrap().is_empty());
        assert_eq!(h.count("overage_span"), 1);
        let months = h.balance_months("codex", &key, "credits").unwrap();
        let spent: f64 = months.iter().map(|m| m.spent).sum();
        let topped: f64 = months.iter().map(|m| m.topped_up).sum();
        assert_eq!((spent, topped), (10.0, 50.0));
        assert!(months[0].label.ends_with("(calendar month)"));
    }
}
