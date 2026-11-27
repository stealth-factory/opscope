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

//! Reset detection and bank events (OPS-305, spec §5.2 to §5.7).
//!
//! Pure and ungated: the store hands [`detect`] one pool's samples with its
//! account's bank readings and plan changes, writes what comes back to
//! `reset_event` and hands it to [`super::fold::fold`], which applies each
//! event (a kept boundary starts a segment, a restart closes the cycle).
//! [`bank_events`] turns an account's bank readings into `bank_event` rows.
//! Both run again whenever the pool or the bank is re-folded, so a bank read
//! after a reset fills in its kind on the next fold (§3.5, §5.6).
//!
//! # How a pair of readings is judged ([`judge`])
//!
//! Consecutive readings `P`, `N` of one pool, `Cached` ones skipped (a
//! cached figure below a live one is age, never evidence, §4). The pair is
//! `watching` when `N.at - P.at <= watch_gap_secs`, else `offline` (§5.2).
//! Then, in the order of §5.4:
//!
//! 1. **Scheduled end**: `N` at or past the cycle's end less [`tolerance`]
//!    is a rollover, whatever the figure did - unless `N` still states
//!    that same end ([`fold::rolled_over`]). If the gap also holds a
//!    discontinuity - a credit spent, or a new window that began before the
//!    old one was due to end - a reset is recorded too, with effect
//!    `unknown`: the two cannot be separated.
//! 2. **Plan or limit change**, then 3. **window change**: not resets.
//! 4. **Rolling pool**: no resets at all.
//! 5. **Boundary moved**: the instant jumped with no fall and no credit
//!    spent while the pool was idle (at about zero), or while watching
//!    (data was flowing, so a real reset would have read as a fall to zero).
//!
//! A reset is then established by any of (§5.3):
//!
//! * `pct_fall` - watching: back to `near_zero_pct` or under from above it;
//!   offline: a fall of at least `max(offline_fall_pts, offline_fall_share%
//!   of P)`. A smaller fall is an `adjustment` and establishes nothing.
//! * `bank_fall` - a credit spent, or gone before its expiry, in a bank
//!   interval overlapping the pair (§5.6). While watching it counts alone
//!   only for a pool at about zero: anything higher would have shown a fall.
//! * `instant_jump` alone, offline, on a pool that was not idle: a reset
//!   inside a gap that usage then climbed back through (§5.3).
//!
//! On a pool that never states an instant, a fall to about zero is how its
//! cycles are inferred (§5.1 undated), so only `bank_fall` makes a reset
//! there. Overage and balance pools have no resets: spend falling back is
//! their own month rolling (§6.2).
//!
//! # What it did, and which kind
//!
//! The effect is read off the pair's own instants (§5.5): the same instant
//! is `kept_boundary`; a moved one is `restarted` at `N.reset_at -
//! N.window_secs`; none to read is `unknown`, or the configured
//! `history_reset_effect`, marked `source: config`. The kind (§5.6) is
//! `banked` when a credit went, `issued` only on positive evidence - a bank
//! read with its credit list on both sides, nothing spent, and the instant
//! moved or the figure went to about zero - and `unknown` otherwise.
//!
//! `group_id` is the instant the interval closed, in milliseconds: the bank
//! reading that showed the credit gone for a banked reset, else `N.at`.
//! Pools of one account read in one response share `N.at`, so one
//! redemption that reset several of them gives them one group.
//!
//! Every event's `evidence` records the mode, the thresholds, which signals
//! fired, where the effect and the kind came from, and `before_known`: false
//! when the figure just before the reset is not known (found offline, or by
//! the bank or the instant alone), which makes the cycle's `consumed_pct` a
//! lower bound (§5.7).

use std::collections::HashMap;

use super::fold::{self, CycleEffect, PlanChange, ResetEvent, ResetKind, Role, Sample, Source, Thresholds, tolerance};
use super::{BankReading, RawCredit};

/// What [`detect`] reads for one pool.
#[derive(Clone, Debug)]
pub struct DetectInput<'a> {
    /// The last sample before `samples`, when the fold starts from a kept
    /// cycle, so the first pair can be compared. `None` from the start.
    pub before: Option<&'a Sample>,
    /// The samples being folded, oldest first.
    pub samples: &'a [Sample],
    /// Every bank reading of the pool's account, oldest first.
    pub banks: &'a [BankReading],
    /// Every plan change of the pool's account, oldest first.
    pub plans: &'a [PlanChange],
    pub thresholds: Thresholds,
    /// The pool's role: overage and balance pools have no resets.
    pub role: Role,
    /// `agent_usage.history_reset_effect` for this provider, when set: the
    /// effect to record for an event with no instant to read one from
    /// (§5.5). Written to the event's evidence as `source: config`.
    pub effect_override: Option<&'a str>,
    /// As [`fold::FoldInput::rolling_seed`]: the reading before `samples`,
    /// known to be in a rolling stretch. Never compared with them.
    pub rolling_seed: Option<&'a Sample>,
    /// The bank intervals (`at_lo`, `at_hi`) whose spent credit a reset
    /// kept from an earlier fold already holds: never given to a pair of
    /// this one too.
    pub claimed: &'a [(f64, f64)],
}

/// The effect `history_reset_effect` may name for a provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectOverride {
    KeptBoundary,
    Restarted,
}

impl EffectOverride {
    pub fn name(self) -> &'static str {
        match self {
            EffectOverride::KeptBoundary => "kept_boundary",
            EffectOverride::Restarted => "restarted",
        }
    }

    pub fn from_name(name: &str) -> Option<EffectOverride> {
        match name.trim() {
            "kept_boundary" => Some(EffectOverride::KeptBoundary),
            "restarted" => Some(EffectOverride::Restarted),
            _ => None,
        }
    }
}

/// The providers `history_reset_effect` can name: the family ids the
/// history keys accounts by, the unified SpaceXSI plan included.
pub const PROVIDERS: [&str; 11] = [
    "claude",
    "codex",
    "cursor",
    "grok",
    "copilot",
    "antigravity",
    "notion",
    "perplexity",
    "devin",
    "droid",
    "spacexsi",
];

/// `history_reset_effect`, per provider. A fixed table rather than a map so
/// [`Thresholds`] stays `Copy`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResetEffects([Option<EffectOverride>; PROVIDERS.len()]);

impl ResetEffects {
    /// From the config value, `{"droid": "restarted"}`. A provider or an
    /// effect it does not know is left out.
    pub fn from_config(v: &serde_json::Value) -> ResetEffects {
        let mut out = ResetEffects::default();
        for (provider, effect) in v.as_object().into_iter().flatten() {
            if let Some(i) = PROVIDERS.iter().position(|p| p == provider) {
                out.0[i] = effect.as_str().and_then(EffectOverride::from_name);
            }
        }
        out
    }

    pub fn get(&self, provider: &str) -> Option<EffectOverride> {
        PROVIDERS.iter().position(|p| *p == provider).and_then(|i| self.0[i])
    }
}

/// What a pair of consecutive readings was found to be.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// A reset, as written to `reset_event`.
    Reset(ResetEvent),
    /// The scheduled end passed between them, and nothing else is known.
    Rollover,
    PlanChanged,
    LimitChanged,
    WindowChanged,
    /// The instant moved with no fall and no credit spent (§5.4 rule 5).
    BoundaryMoved,
    /// A fall too small to count (§5.3).
    Adjustment,
    /// Nothing happened worth a name.
    Steady,
}

/// One pair's verdict: about the readings at `at_lo` and `at_hi`.
#[derive(Clone, Debug, PartialEq)]
pub struct Judged {
    pub at_lo: f64,
    pub at_hi: f64,
    pub verdict: Verdict,
}

/// The resets established between consecutive samples of one pool, oldest
/// first. Each `at_hi` is the time of a sample in `input.samples`.
pub fn detect(input: &DetectInput) -> Vec<ResetEvent> {
    judge(input)
        .into_iter()
        .filter_map(|j| match j.verdict {
            Verdict::Reset(e) => Some(e),
            _ => None,
        })
        .collect()
}

/// A pair of readings and what each signal said about it.
struct Pair<'a> {
    p: &'a Sample,
    n: &'a Sample,
    watching: bool,
    tol: f64,
    /// The cycle's scheduled end, when `N` reached it.
    due_end: Option<f64>,
    plan_changed: bool,
    limit_changed: bool,
    window_changed: bool,
    jump: bool,
    pct_fall: bool,
    adjustment: bool,
    /// `P` at about zero, or with no figure: a window not yet used.
    idle: bool,
    /// The redeemed bank event attributed to this pair, by index.
    bank: Option<usize>,
}

impl Pair<'_> {
    fn changed(&self) -> bool {
        self.plan_changed || self.limit_changed || self.window_changed
    }

    /// A signal of its own, bank aside. A jump in the instant counts as
    /// `verdict` counts it: offline only, since while watching a reset
    /// would have come with a fall, and a date moving on its own is the
    /// boundary moving, not evidence to hand a spent credit to.
    fn signal(&self, dated: bool) -> bool {
        self.due_end.is_some() || (dated && (self.pct_fall || (self.jump && !self.idle && !self.watching)))
    }

    /// Whether this pair may take a spent credit as its own reset. While
    /// watching, with nothing else showing, only a pool at about zero could
    /// have been reset without a visible fall.
    fn accepts_bank(&self, dated: bool) -> bool {
        !self.changed() && (self.signal(dated) || !self.watching || self.idle)
    }
}

fn differs(a: Option<f64>, b: Option<f64>, by: f64) -> bool {
    matches!((a, b), (Some(a), Some(b)) if (a - b).abs() > by)
}

/// Every pair of consecutive evidence readings of one pool that ends in
/// `input.samples`, judged.
pub fn judge(input: &DetectInput) -> Vec<Judged> {
    if matches!(input.role, Role::Overage | Role::Balance) {
        return Vec::new();
    }
    let t = &input.thresholds;
    // Rule 4, decided exactly as the fold decides it: the readings of a
    // rolling stretch are no evidence of anything, and are left out.
    let mask = fold::rolling_mask(input.samples, t.near_zero_pct, input.rolling_seed);
    let all: Vec<&Sample> = input
        .before
        .into_iter()
        .chain(input.samples.iter().zip(&mask).filter(|(_, m)| !**m).map(|(s, _)| s))
        .collect();
    let dated = all.iter().any(|s| s.reset_at.is_some());
    let near = t.near_zero_pct;

    let mut pairs: Vec<Pair> = Vec::new();
    let mut prev: Option<&Sample> = None;
    // The cycle's end as the fold sees it: the latest stated instant, from
    // any reading - a cached one can close a cycle on schedule.
    let mut end: Option<f64> = None;
    // The end as it stood at the last live reading. A cached reading after
    // it can state the next window's end, and judged against that alone
    // the old end was never reached, so the first live reading of the new
    // window read as a fall - a restart at the start of the cycle the fold
    // had already opened on schedule.
    let mut end_at_prev: Option<f64> = None;
    // Whether a cached reading since the last live one reached the end in
    // force, so closed the cycle as the fold does. One that only stated a
    // new end before reaching the old one moved the end, and the old end
    // is then no boundary the fold rolled at.
    let mut crossed = false;
    for &n in &all {
        if n.source == Source::Cached {
            let tol = tolerance(n.window_secs);
            crossed |= end.is_some_and(|e| fold::rolled_over(e, n, tol, tol));
        }
        if n.source != Source::Cached {
            if let Some(p) = prev {
                let tol = tolerance(n.window_secs.or(p.window_secs));
                let watching = n.at - p.at <= t.watch_gap_secs;
                let fall = match (p.pct, n.pct) {
                    (Some(a), Some(b)) => a - b,
                    _ => 0.0,
                };
                let need = t
                    .offline_fall_pts
                    .max(t.offline_fall_share / 100.0 * p.pct.unwrap_or(0.0));
                let pct_fall = match (p.pct, n.pct) {
                    (Some(a), Some(b)) if watching => a > near && b <= near,
                    (Some(_), Some(_)) => fall > 0.0 && fall >= need,
                    _ => false,
                };
                pairs.push(Pair {
                    p,
                    n,
                    watching,
                    tol,
                    due_end: end
                        .filter(|e| fold::rolled_over(*e, n, tol, tol))
                        .or_else(|| end_at_prev.filter(|e| crossed && fold::rolled_over(*e, n, tol, tol))),
                    plan_changed: fold::plan_change_between(input.plans, p.at, n.at).is_some(),
                    limit_changed: differs(p.limit, n.limit, 1e-9),
                    window_changed: differs(p.window_secs, n.window_secs, 1.0),
                    jump: differs(p.reset_at, n.reset_at, tol),
                    pct_fall,
                    adjustment: fall > 1e-9 && !pct_fall,
                    idle: p.pct.is_none_or(|x| x <= near),
                    bank: None,
                });
            }
            prev = Some(n);
            crossed = false;
        }
        if n.reset_at.is_some() {
            end = n.reset_at;
        }
        if n.source != Source::Cached {
            end_at_prev = end;
        }
    }

    let banks = bank_events(input.banks);
    attribute_bank(&mut pairs, &banks, dated, input.claimed);

    let first = input.samples.first().map_or(f64::INFINITY, |s| s.at);
    pairs
        .iter()
        .filter(|x| x.n.at >= first)
        .map(|x| Judged {
            at_lo: x.p.at,
            at_hi: x.n.at,
            verdict: verdict(x, input, &banks, dated),
        })
        .collect()
}

/// Give each redeemed credit to at most one pair of this pool: the first it
/// overlaps that shows a signal of its own, else the one holding the bank
/// reading that first showed it gone. Never counted twice in one pool.
fn attribute_bank(pairs: &mut [Pair], banks: &[BankEvent], dated: bool, claimed: &[(f64, f64)]) {
    let same = |a: f64, b: f64| (a - b).abs() <= 1e-6;
    for (i, e) in banks.iter().enumerate() {
        if e.kind != BankEventKind::Redeemed || claimed.iter().any(|(lo, hi)| same(*lo, e.at_lo) && same(*hi, e.at_hi)) {
            continue;
        }
        let free = |x: &Pair| x.bank.is_none() && e.at_lo < x.n.at && x.p.at < e.at_hi && x.accepts_bank(dated);
        let pick = pairs
            .iter()
            .position(|x| free(x) && x.signal(dated))
            .or_else(|| pairs.iter().position(|x| free(x) && x.p.at < e.at_hi && e.at_hi <= x.n.at));
        if let Some(k) = pick {
            pairs[k].bank = Some(i);
        }
    }
}

/// The last bank reading at or before `lo`, and the first at or after `hi`.
fn bank_around(banks: &[BankReading], lo: f64, hi: f64) -> Option<(&BankReading, &BankReading)> {
    let before = banks.iter().rev().find(|b| b.at <= lo)?;
    let after = banks.iter().find(|b| b.at >= hi)?;
    Some((before, after))
}

fn millis(at: f64) -> i64 {
    (at * 1000.0).round() as i64
}

fn verdict(x: &Pair, input: &DetectInput, banks: &[BankEvent], dated: bool) -> Verdict {
    let (p, n) = (x.p, x.n);
    let bank_fall = x.bank.is_some();

    // Rule 1: the scheduled end, and whether anything else crossed with it.
    // Across a rollover the figure falls and the instant moves on their
    // own, so only a spent credit or a window that began before the old one
    // was due is a sign of anything more.
    let (mut pct_fall, mut jump) = (x.pct_fall, x.jump);
    let rollover = match x.due_end {
        Some(end) => {
            let began_early = match (n.reset_at, n.window_secs) {
                (Some(r), Some(w)) if w > 0.0 && r > end => r - w < end - x.tol,
                _ => false,
            };
            if x.changed() || !(bank_fall || began_early) {
                return Verdict::Rollover;
            }
            pct_fall = false;
            jump = began_early;
            true
        }
        // Rules 2 and 3.
        None if x.plan_changed => return Verdict::PlanChanged,
        None if x.limit_changed => return Verdict::LimitChanged,
        None if x.window_changed => return Verdict::WindowChanged,
        None => false,
    };

    // On a pool with no instant, a fall to zero is its own rollover (§5.1);
    // an instant that jumped while watching would have come with one.
    let pct_reset = dated && x.pct_fall;
    let instant_reset = dated && x.jump && !x.idle && !x.watching;
    if !rollover && !(pct_reset || bank_fall || instant_reset) {
        // Rule 5, and the falls too small to count.
        return if x.jump {
            Verdict::BoundaryMoved
        } else if x.adjustment {
            Verdict::Adjustment
        } else {
            Verdict::Steady
        };
    }

    // §5.5: the effect, off this event's own instants.
    let mut clamped = false;
    let (effect, effect_source) = if rollover {
        (CycleEffect::Unknown, "rollover")
    } else {
        match (p.reset_at, n.reset_at, n.window_secs) {
            (Some(a), Some(b), _) if (a - b).abs() <= x.tol => (CycleEffect::KeptBoundary, "instant"),
            (Some(_), Some(b), Some(w)) if w > 0.0 => {
                let start = b - w;
                clamped = start < p.at || start > n.at;
                (
                    CycleEffect::Restarted {
                        new_start: start.clamp(p.at, n.at),
                    },
                    "instant",
                )
            }
            // A date on one side only, or two that differ with no window to
            // place a restart by: there are dates, and they do not say what
            // happened, so the setting - which is for resets with none - is
            // not reached for. The help promises exactly this.
            (Some(_), _, _) | (_, Some(_), _) => (CycleEffect::Unknown, "none"),
            _ => match input.effect_override.and_then(EffectOverride::from_name) {
                Some(EffectOverride::KeptBoundary) => (CycleEffect::KeptBoundary, "config"),
                // With no instant to start it at: the reading that showed it.
                Some(EffectOverride::Restarted) => (CycleEffect::Restarted { new_start: n.at }, "config"),
                None => (CycleEffect::Unknown, "none"),
            },
        }
    };

    // §5.6: the kind, from the bank. "Issued" needs positive evidence.
    let near = input.thresholds.near_zero_pct;
    let (kind, kind_rule) = if bank_fall {
        (ResetKind::Banked, "credit_spent")
    } else {
        // Every reading from one side to the other a full list: a count
        // alone in between cannot tell a credit spent from one granted
        // beside it, so it is no evidence that nothing was spent.
        let clean = bank_around(input.banks, p.at, n.at).is_some_and(|(b1, b2)| {
            input
                .banks
                .iter()
                .filter(|b| b.at >= b1.at && b.at <= b2.at)
                .all(|b| b.credits.is_some())
                && !banks.iter().any(|e| {
                    matches!(e.kind, BankEventKind::Redeemed | BankEventKind::Unexplained)
                        && e.at_lo >= b1.at
                        && e.at_hi <= b2.at
                })
        });
        let moved = jump || n.pct.is_some_and(|v| v <= near);
        if clean && moved {
            (ResetKind::Issued, "bank_read_both_sides_nothing_spent")
        } else {
            (ResetKind::Unknown, "no_positive_evidence")
        }
    };

    let mut fired = Vec::new();
    if pct_fall {
        fired.push("pct_fall");
    }
    if jump {
        fired.push("instant_jump");
    }
    if bank_fall {
        fired.push("bank_fall");
    }
    // Known only when a watched fall shows the figure just before the reset;
    // a reset found by the bank or the instant alone has none (§5.7).
    let before_known = x.watching && !rollover && pct_fall;
    let bank = x.bank.map(|i| &banks[i]);
    let t = &input.thresholds;
    let mut evidence = serde_json::json!({
        "rule": if rollover { "rollover_and_reset" } else { "reset" },
        "mode": if x.watching { "watching" } else { "offline" },
        "gap_secs": n.at - p.at,
        "thresholds": {
            "watch_gap_secs": t.watch_gap_secs,
            "near_zero_pct": t.near_zero_pct,
            "offline_fall_pts": t.offline_fall_pts,
            "offline_fall_share": t.offline_fall_share,
            "tolerance_secs": x.tol,
        },
        "signals": {
            "pct_fall": pct_fall,
            "instant_jump": jump,
            "bank_fall": bank_fall,
        },
        "pct": if pct_fall { "fall" } else if x.adjustment { "adjustment" } else { "no_fall" },
        "idle": x.idle,
        "sources": [p.source.name(), n.source.name()],
        "effect": { "source": effect_source },
        "kind_rule": kind_rule,
        "before_known": before_known,
    });
    if effect_source == "config" {
        evidence["effect"]["value"] = serde_json::json!(effect.name());
    }
    if clamped {
        evidence["effect"]["new_start_clamped"] = serde_json::json!(true);
    }
    if let Some(b) = bank {
        evidence["bank"] = serde_json::json!({"at_lo": b.at_lo, "at_hi": b.at_hi, "count": b.count});
    }

    Verdict::Reset(ResetEvent {
        at_lo: p.at,
        at_hi: n.at,
        kind,
        effect,
        signals: fired.join(","),
        pct_before: p.pct,
        pct_after: n.pct,
        reset_at_before: p.reset_at,
        reset_at_after: n.reset_at,
        group_id: Some(millis(bank.map_or(n.at, |b| b.at_hi))),
        evidence: evidence.to_string(),
        before_known,
    })
}

/// What happened to a bank between two readings (§5.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BankEventKind {
    Granted,
    Redeemed,
    Expired,
    Unexplained,
}

impl BankEventKind {
    pub fn name(self) -> &'static str {
        match self {
            BankEventKind::Granted => "granted",
            BankEventKind::Redeemed => "redeemed",
            BankEventKind::Expired => "expired",
            BankEventKind::Unexplained => "unexplained",
        }
    }
}

/// A row of `bank_event`, less the account id the store adds.
#[derive(Clone, Debug, PartialEq)]
pub struct BankEvent {
    pub at_lo: f64,
    pub at_hi: f64,
    pub kind: BankEventKind,
    pub count: u32,
    pub expires_at: Option<f64>,
}

/// A status the server gives a credit that has been used.
fn is_spent(status: &str) -> bool {
    matches!(status, "spent" | "redeemed" | "used" | "consumed")
}

/// A credit still held: neither used nor expired.
fn is_held(status: &str) -> bool {
    !is_spent(status) && status != "expired"
}

/// A credit's identity: the server's id, else its expiry, matched as a
/// multiset so two credits nothing tells apart are still two.
fn identity(c: &RawCredit) -> String {
    match &c.id {
        Some(id) => format!("id:{id}"),
        None => format!("exp:{:?}", c.expires_at.map(f64::to_bits)),
    }
}

/// Every bank event in one account's bank readings, oldest first, from the
/// whole list - the store rebuilds the account's `bank_event` rows from it
/// whenever a bank reading is written.
///
/// Two readings with credit lists are diffed credit by credit, never by
/// count, so a grant and a redemption in one gap are both seen. A credit
/// held at the first and used at the second is `redeemed`; one gone from
/// the list before its expiry is `redeemed`, at or after it `expired`, with
/// no expiry to judge by `unexplained`. A new credit is `granted`. Where
/// either side is a count alone, a rise is `granted` and a fall
/// `unexplained`: a count cannot tell a redemption from an expiry.
pub fn bank_events(banks: &[BankReading]) -> Vec<BankEvent> {
    let mut out = Vec::new();
    for pair in banks.windows(2) {
        let (b1, b2) = (&pair[0], &pair[1]);
        let mut found: Vec<(BankEventKind, u32, Option<f64>)> = Vec::new();
        let mut add_n = |kind: BankEventKind, n: u32, exp: Option<f64>| match found.iter_mut().find(|f| f.0 == kind) {
            Some(f) => {
                f.1 = f.1.saturating_add(n);
                f.2 = match (f.2, exp) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            }
            None => found.push((kind, n, exp)),
        };
        match (&b1.credits, &b2.credits) {
            (Some(l1), Some(l2)) => {
                let mut add = |kind: BankEventKind, exp: Option<f64>| add_n(kind, 1, exp);
                // The second list by identity, as a multiset: each credit of
                // the first claims one of the second with its identity -
                // the one with its own status where there is one, so a list
                // the provider reordered reads the same - and what nothing
                // claims is new.
                let mut by_id: HashMap<String, Vec<usize>> = HashMap::new();
                for (j, c) in l2.iter().enumerate() {
                    by_id.entry(identity(c)).or_default().push(j);
                }
                let mut claimed = vec![false; l2.len()];
                // Same status first, across the whole identity group, and
                // only then the rest: claimed one credit at a time, the
                // first held credit could take a credit another of the
                // group still stood as, and `[available, spent]` against
                // `[spent, expired]` read as a redemption or an expiry by
                // the order the provider listed them in.
                let mut now_of: Vec<Option<usize>> = vec![None; l1.len()];
                for (i, c) in l1.iter().enumerate() {
                    if let Some(v) = by_id.get_mut(&identity(c)) {
                        if let Some(k) = v.iter().position(|&j| l2[j].status == c.status) {
                            let j = v.remove(k);
                            claimed[j] = true;
                            now_of[i] = Some(j);
                        }
                    }
                }
                for (i, c) in l1.iter().enumerate() {
                    if now_of[i].is_some() {
                        continue;
                    }
                    if let Some(v) = by_id.get_mut(&identity(c)).filter(|v| !v.is_empty()) {
                        let j = v.remove(0);
                        claimed[j] = true;
                        now_of[i] = Some(j);
                    }
                }
                for (i, c) in l1.iter().enumerate() {
                    let now = now_of[i].map(|j| &l2[j]);
                    if !is_held(&c.status) {
                        continue;
                    }
                    match now {
                        Some(l) if is_spent(&l.status) => add(BankEventKind::Redeemed, c.expires_at),
                        Some(l) if l.status == "expired" => add(BankEventKind::Expired, c.expires_at),
                        Some(_) => {}
                        None => match c.expires_at {
                            Some(e) if e <= b2.at => add(BankEventKind::Expired, Some(e)),
                            Some(e) => add(BankEventKind::Redeemed, Some(e)),
                            None => add(BankEventKind::Unexplained, None),
                        },
                    }
                }
                for (j, c) in l2.iter().enumerate() {
                    if claimed[j] {
                        continue;
                    }
                    add(BankEventKind::Granted, c.expires_at);
                    if is_spent(&c.status) {
                        add(BankEventKind::Redeemed, c.expires_at);
                    } else if c.status == "expired" {
                        add(BankEventKind::Expired, c.expires_at);
                    }
                }
            }
            _ => {
                // One row for the whole move, however large.
                let n = |d: u64| u32::try_from(d).unwrap_or(u32::MAX);
                if b2.left > b1.left {
                    add_n(BankEventKind::Granted, n(b2.left - b1.left), None);
                } else if b1.left > b2.left {
                    add_n(BankEventKind::Unexplained, n(b1.left - b2.left), None);
                }
            }
        }
        found.sort_by_key(|f| f.0 as u8);
        out.extend(found.into_iter().map(|(kind, count, expires_at)| BankEvent {
            at_lo: b1.at,
            at_hi: b2.at,
            kind,
            count,
            expires_at,
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::fold::{Cycle, EndReason, FoldInput, FoldOutput, StartSource};
    use super::*;

    const M: f64 = 60.0;
    const H: f64 = 3600.0;
    const W5: f64 = 5.0 * H;
    const WEEK: f64 = 7.0 * 24.0 * H;
    const A: f64 = 1_000_000.0;
    const FAR: f64 = 9e9;

    fn s(at: f64, pct: f64, reset: Option<f64>, window: Option<f64>) -> Sample {
        Sample {
            at,
            pct: Some(pct),
            used: None,
            limit: None,
            window_secs: window,
            reset_at: reset,
            source: Source::Live,
            spacexsi: None,
        }
    }

    /// A weekly reading ending at `A + WEEK`.
    fn wk(at: f64, pct: f64) -> Sample {
        s(at, pct, Some(A + WEEK), Some(WEEK))
    }

    fn credit(id: &str, status: &str, exp: Option<f64>) -> RawCredit {
        RawCredit {
            id: Some(id.into()),
            status: status.into(),
            expires_at: exp,
        }
    }

    fn bank(at: f64, credits: Vec<RawCredit>) -> BankReading {
        let left = credits.iter().filter(|c| is_held(&c.status)).count() as u64;
        BankReading {
            at,
            left,
            credits: Some(credits),
        }
    }

    fn count_only(at: f64, left: u64) -> BankReading {
        BankReading { at, left, credits: None }
    }

    fn input<'a>(samples: &'a [Sample], banks: &'a [BankReading], plans: &'a [PlanChange]) -> DetectInput<'a> {
        DetectInput {
            before: None,
            samples,
            banks,
            plans,
            thresholds: Thresholds::default(),
            role: Role::Included,
            effect_override: None,
            rolling_seed: None,
            claimed: &[],
        }
    }

    fn verdicts(i: &DetectInput) -> Vec<Verdict> {
        judge(i).into_iter().map(|j| j.verdict).collect()
    }

    fn one(i: &DetectInput) -> ResetEvent {
        let found = detect(i);
        assert_eq!(found.len(), 1, "{:#?}", judge(i));
        found.into_iter().next().unwrap()
    }

    fn evidence(e: &ResetEvent) -> serde_json::Value {
        serde_json::from_str(&e.evidence).unwrap()
    }

    fn fold_with(samples: &[Sample], resets: &[ResetEvent]) -> FoldOutput {
        fold::fold(&FoldInput {
            role: Role::Included,
            samples,
            plans: &[],
            resets,
            prev: None,
            next_ordinal: 1,
            rolling_seed: None,
            resume: None,
            gone_at: None,
            thresholds: Thresholds::default(),
        })
    }

    #[test]
    fn a_reading_inside_the_tolerance_that_states_the_same_end_is_not_a_rollover() {
        let e = A + WEEK;
        let samples = vec![wk(A + H, 10.0), wk(e - 2.0 * H, 50.0), wk(e - H, 95.0), {
            s(e + H, 1.0, Some(e + WEEK), Some(WEEK))
        }];
        let i = input(&samples, &[], &[]);
        assert_eq!(
            verdicts(&i),
            vec![Verdict::Steady, Verdict::Steady, Verdict::Rollover],
            "{:#?}",
            judge(&i)
        );
    }

    #[test]
    fn a_kept_boundary_reset_while_watching_adds_a_segment_and_an_allowance() {
        // Used to 100%, a banked reset, then back up to 40%: 140% of one
        // allowance, 70% of the two that were available (§5.7).
        let samples = vec![wk(A + H, 100.0), wk(A + H + 10.0 * M, 0.0), wk(A + 3.0 * H, 40.0)];
        let banks = vec![
            bank(A + H - M, vec![credit("c1", "available", Some(FAR))]),
            bank(A + H + 11.0 * M, vec![]),
        ];
        let e = one(&input(&samples, &banks, &[]));
        assert_eq!(e.effect, CycleEffect::KeptBoundary);
        assert_eq!(e.kind, ResetKind::Banked);
        assert_eq!(e.signals, "pct_fall,bank_fall");
        assert_eq!((e.at_lo, e.at_hi), (A + H, A + H + 10.0 * M));
        assert!(e.before_known);
        let ev = evidence(&e);
        assert_eq!(ev["mode"], "watching");
        assert_eq!(ev["thresholds"]["near_zero_pct"], 1.0);
        assert_eq!(ev["effect"]["source"], "instant");
        let c = &fold_with(&samples, &[e]).cycles[0];
        assert_eq!(c.allowances, 2);
        assert_eq!(c.consumed_pct, Some(140.0));
        assert_eq!(c.available_pct, Some(200.0));
        assert!(!c.consumed_is_lower_bound);
    }

    #[test]
    fn a_restarted_reset_closes_at_the_new_instant_less_its_window_and_is_issued_on_a_clean_bank() {
        let at = A + 2.0 * H;
        let new_start = at - 3.0 * M;
        let samples = vec![wk(A + H, 90.0), wk(at - 10.0 * M, 91.0), s(at, 0.0, Some(new_start + WEEK), Some(WEEK))];
        // The bank was read either side and nothing went from it.
        let held = || vec![credit("c1", "available", Some(FAR))];
        let banks = vec![bank(A + H, held()), bank(at + M, held())];
        let e = one(&input(&samples, &banks, &[]));
        assert_eq!(e.effect, CycleEffect::Restarted { new_start });
        assert_eq!(e.kind, ResetKind::Issued);
        assert_eq!(e.signals, "pct_fall,instant_jump");
        let out = fold_with(&samples, &[e]);
        assert_eq!(out.cycles.len(), 2);
        assert_eq!(out.cycles[0].closed_at, Some(new_start));
        assert!(out.cycles[0].cut_short());
        assert_eq!(out.cycles[1].start_source, StartSource::Reset);
        assert!(!out.cycles[1].cut_short());
    }

    #[test]
    fn issued_needs_positive_evidence_and_an_unread_bank_is_unknown() {
        let samples = vec![wk(A + H, 60.0), wk(A + H + 10.0 * M, 0.0)];
        assert_eq!(one(&input(&samples, &[], &[])).kind, ResetKind::Unknown);
        // A bank read only before the reset is not "both sides".
        let banks = vec![bank(A + H, vec![credit("c1", "available", Some(FAR))])];
        assert_eq!(one(&input(&samples, &banks, &[])).kind, ResetKind::Unknown);
        // A count-only bank cannot show that nothing was spent.
        let banks = vec![count_only(A + H, 1), count_only(A + 2.0 * H, 1)];
        assert_eq!(one(&input(&samples, &banks, &[])).kind, ResetKind::Unknown);
    }

    #[test]
    fn a_reset_at_three_percent_shows_only_in_the_bank_and_its_figure_is_a_lower_bound() {
        // An hour apart (offline): 3% to 4%, no fall, but a credit went.
        let samples = vec![wk(A + H, 3.0), wk(A + 2.0 * H, 4.0)];
        let banks = vec![
            bank(A + H, vec![credit("c1", "available", Some(FAR))]),
            bank(A + 2.0 * H, vec![]),
        ];
        let e = one(&input(&samples, &banks, &[]));
        assert_eq!(e.signals, "bank_fall");
        assert_eq!(e.kind, ResetKind::Banked);
        assert_eq!(e.effect, CycleEffect::KeptBoundary);
        assert!(!e.before_known);
        let c = &fold_with(&samples, &[e]).cycles[0];
        assert_eq!(c.allowances, 2);
        assert!(c.consumed_is_lower_bound);
    }

    #[test]
    fn a_bank_only_reset_while_watching_an_idle_pool_has_no_known_figure_before_it() {
        // CodeRabbit's case: 0% twice ten minutes apart, a credit redeemed
        // between. Watching, but no reading shows the usage just before the
        // reset, so what the cycle used is "at least" (§5.7).
        let samples = vec![wk(A + H, 0.0), wk(A + H + 10.0 * M, 0.0)];
        let banks = vec![
            bank(A + H, vec![credit("c1", "available", Some(FAR))]),
            bank(A + H + 10.0 * M, vec![credit("c1", "spent", Some(FAR))]),
        ];
        let e = one(&input(&samples, &banks, &[]));
        assert_eq!(e.signals, "bank_fall");
        assert!(!e.before_known);
        assert_eq!(evidence(&e)["before_known"], false);
        // A watched fall to zero still knows its figure.
        let fell = vec![wk(A + H, 70.0), wk(A + H + 10.0 * M, 0.0)];
        assert!(one(&input(&fell, &banks, &[])).before_known);
    }

    #[test]
    fn a_count_only_reading_inside_the_bracket_leaves_the_reset_unknown() {
        // CodeRabbit's case: credit c1 listed, then a count of one with no
        // list, then credit c2 listed. Both count comparisons find nothing,
        // but c1 may have been redeemed and c2 granted: not positive evidence
        // that nothing was spent.
        let samples = vec![wk(A + H, 60.0), wk(A + H + 10.0 * M, 0.0)];
        let banks = vec![
            bank(A + H, vec![credit("c1", "available", Some(FAR))]),
            count_only(A + H + 5.0 * M, 1),
            bank(A + H + 10.0 * M, vec![credit("c2", "available", Some(FAR))]),
        ];
        assert_eq!(one(&input(&samples, &banks, &[])).kind, ResetKind::Unknown);
        // Lists all the way through, nothing spent: issued.
        let clean = vec![
            bank(A + H, vec![credit("c1", "available", Some(FAR))]),
            bank(A + H + 5.0 * M, vec![credit("c1", "available", Some(FAR))]),
            bank(A + H + 10.0 * M, vec![credit("c1", "available", Some(FAR))]),
        ];
        assert_eq!(one(&input(&samples, &clean, &[])).kind, ResetKind::Issued);
    }

    #[test]
    fn anonymous_credits_are_matched_as_a_multiset_and_the_unmatched_ones_classified() {
        // CodeRabbit's case: one held credit with no id becomes two with the
        // same expiry, held and spent. One was granted and one redeemed,
        // whichever order the provider lists them in.
        let anon = |status: &str| RawCredit {
            id: None,
            status: status.into(),
            expires_at: Some(FAR),
        };
        let before = bank(A + H, vec![anon("available")]);
        for after in [vec![anon("available"), anon("spent")], vec![anon("spent"), anon("available")]] {
            let ev = bank_events(&[before.clone(), bank(A + 2.0 * H, after)]);
            let kinds: Vec<_> = ev.iter().map(|e| (e.kind, e.count)).collect();
            assert_eq!(kinds, vec![(BankEventKind::Granted, 1), (BankEventKind::Redeemed, 1)]);
        }
        // A count that leaps is one row with the whole difference.
        let up = bank_events(&[count_only(1.0, 0), count_only(2.0, 100_000)]);
        assert_eq!((up[0].kind, up[0].count), (BankEventKind::Granted, 100_000));
        // CodeRabbit's later case: [available, spent] against [spent,
        // expired]. The spent credit is still the spent one, so the held one
        // expired, in either order - it read as a redemption when the
        // second list began with `spent`, and a redemption can bank a reset.
        let before = bank(A + H, vec![anon("available"), anon("spent")]);
        for after in [vec![anon("spent"), anon("expired")], vec![anon("expired"), anon("spent")]] {
            let ev = bank_events(&[before.clone(), bank(A + 2.0 * H, after)]);
            let kinds: Vec<_> = ev.iter().map(|e| (e.kind, e.count)).collect();
            assert_eq!(kinds, vec![(BankEventKind::Expired, 1)]);
        }
    }

    #[test]
    fn a_spent_credit_while_watching_is_not_this_pools_reset_unless_it_fell() {
        // Data flowing, 30% to 31%: a reset here would have read as zero,
        // so the credit reset some other pool.
        let samples = vec![wk(A + H, 30.0), wk(A + H + 10.0 * M, 31.0)];
        let banks = vec![
            bank(A + H, vec![credit("c1", "available", Some(FAR))]),
            bank(A + H + 10.0 * M, vec![]),
        ];
        assert!(detect(&input(&samples, &banks, &[])).is_empty());
    }

    #[test]
    fn a_reset_inside_a_gap_that_climbed_back_is_read_off_the_instant() {
        // Offline for two days: 60% before, 58% after (a fall too small to
        // count), but the week now ends three hours after the new reading's
        // start - the cycle restarted in the gap and usage climbed again.
        let at = A + 2.0 * 24.0 * H;
        let new_start = at - 3.0 * H;
        let samples = vec![wk(A + H, 60.0), s(at, 58.0, Some(new_start + WEEK), Some(WEEK))];
        let e = one(&input(&samples, &[], &[]));
        assert_eq!(e.signals, "instant_jump");
        assert_eq!(e.effect, CycleEffect::Restarted { new_start });
        assert_eq!(e.kind, ResetKind::Unknown);
        assert!(!e.before_known);
        assert_eq!(evidence(&e)["pct"], "adjustment");
        let out = fold_with(&samples, &[e]);
        assert!(out.cycles[0].consumed_is_lower_bound);
        assert_eq!(out.cycles[0].end_reason, Some(EndReason::Reset));
    }

    #[test]
    fn a_plan_upgrade_dropping_the_figure_is_not_a_reset() {
        let samples = vec![wk(A + H, 80.0), wk(A + H + 10.0 * M, 20.0)];
        let plans = vec![
            PlanChange { at: A, plan: "5x".into() },
            PlanChange {
                at: A + H + 5.0 * M,
                plan: "20x".into(),
            },
        ];
        let i = input(&samples, &[], &plans);
        assert_eq!(verdicts(&i), vec![Verdict::PlanChanged]);
        assert!(detect(&i).is_empty());
    }

    #[test]
    fn a_limit_change_is_not_a_reset() {
        let mut x = wk(A + H, 50.0);
        x.limit = Some(50.0);
        let mut y = wk(A + H + 10.0 * M, 0.0);
        y.limit = Some(100.0);
        let samples = [x, y];
        assert_eq!(verdicts(&input(&samples, &[], &[])), vec![Verdict::LimitChanged]);
    }

    #[test]
    fn a_window_change_is_not_a_reset() {
        let samples = vec![wk(A + H, 40.0), s(A + H + 10.0 * M, 0.0, Some(A + H + W5), Some(W5))];
        assert_eq!(verdicts(&input(&samples, &[], &[])), vec![Verdict::WindowChanged]);
    }

    #[test]
    fn a_small_adjustment_is_logged_as_one_and_changes_nothing() {
        // Offline: 40 to 38.5 is under max(3 points, 5% of 40).
        let offline = vec![wk(A + H, 40.0), wk(A + 3.0 * H, 38.5)];
        assert_eq!(verdicts(&input(&offline, &[], &[])), vec![Verdict::Adjustment]);
        // Watching: 40 to 39 is not back to about zero.
        let watching = vec![wk(A + H, 40.0), wk(A + H + 10.0 * M, 39.0)];
        assert_eq!(verdicts(&input(&watching, &[], &[])), vec![Verdict::Adjustment]);
        // Offline, a fall of 3 points and more does count.
        let fell = vec![wk(A + H, 40.0), wk(A + 3.0 * H, 36.0)];
        let e = one(&input(&fell, &[], &[]));
        assert_eq!(e.signals, "pct_fall");
        assert_eq!(evidence(&e)["mode"], "offline");
        // So does 5% of a high figure, and not less.
        let t = Thresholds {
            offline_fall_pts: 1.0,
            ..Thresholds::default()
        };
        let high = vec![wk(A + H, 90.0), wk(A + 3.0 * H, 86.0)];
        let mut i = input(&high, &[], &[]);
        i.thresholds = t;
        assert_eq!(verdicts(&i), vec![Verdict::Adjustment]);
        let high = vec![wk(A + H, 90.0), wk(A + 3.0 * H, 85.5)];
        let mut i = input(&high, &[], &[]);
        i.thresholds = t;
        assert_eq!(detect(&i).len(), 1);
    }

    #[test]
    fn a_cached_reading_below_a_live_one_is_never_evidence() {
        let mut cached = wk(A + H + 5.0 * M, 0.0);
        cached.source = Source::Cached;
        let samples = vec![wk(A + H, 30.0), cached, wk(A + H + 10.0 * M, 32.0)];
        let j = judge(&input(&samples, &[], &[]));
        // One pair, live to live, which skips the cached reading.
        assert_eq!(j.len(), 1);
        assert_eq!((j[0].at_lo, j[0].at_hi), (A + H, A + H + 10.0 * M));
        assert_eq!(j[0].verdict, Verdict::Steady);
    }

    #[test]
    fn a_grant_and_a_redemption_in_one_gap_leave_the_count_alone_and_are_still_banked() {
        let banks = vec![
            bank(A + H, vec![credit("c1", "available", Some(FAR))]),
            bank(A + 2.0 * H, vec![credit("c2", "available", Some(FAR + 1.0))]),
        ];
        assert_eq!(banks[0].left, banks[1].left);
        let ev = bank_events(&banks);
        let kinds: Vec<_> = ev.iter().map(|e| (e.kind, e.count)).collect();
        assert_eq!(
            kinds,
            vec![(BankEventKind::Granted, 1), (BankEventKind::Redeemed, 1)]
        );
        assert_eq!(ev[0].expires_at, Some(FAR + 1.0));
        let samples = vec![wk(A + H + 30.0 * M, 70.0), wk(A + H + 40.0 * M, 0.0)];
        assert_eq!(one(&input(&samples, &banks, &[])).kind, ResetKind::Banked);
    }

    #[test]
    fn a_count_only_bank_across_an_expiry_is_unexplained_and_the_reset_unknown() {
        let banks = vec![count_only(A + H, 2), count_only(A + 2.0 * H, 1)];
        let ev = bank_events(&banks);
        assert_eq!(ev.len(), 1);
        assert_eq!((ev[0].kind, ev[0].count), (BankEventKind::Unexplained, 1));
        let samples = vec![wk(A + H + 30.0 * M, 70.0), wk(A + H + 40.0 * M, 0.0)];
        let e = one(&input(&samples, &banks, &[]));
        assert_eq!(e.kind, ResetKind::Unknown);
        assert_eq!(e.signals, "pct_fall");
        // A count that rises is a grant.
        let up = bank_events(&[count_only(1.0, 1), count_only(2.0, 3)]);
        assert_eq!((up[0].kind, up[0].count), (BankEventKind::Granted, 2));
    }

    #[test]
    fn expiry_is_judged_against_the_credits_own_date() {
        let at2 = A + 2.0 * H;
        let banks = vec![
            bank(
                A + H,
                vec![
                    // Gone, and its date had passed by the second reading.
                    credit("old", "available", Some(A + 1.5 * H)),
                    // Gone with its date still ahead: used.
                    credit("used", "available", Some(FAR)),
                    // Listed as expired by the server.
                    credit("said", "available", Some(FAR)),
                    // Gone with no date to judge by.
                    credit("nodate", "available", None),
                    // Still there, still held.
                    credit("kept", "available", Some(FAR)),
                ],
            ),
            bank(
                at2,
                vec![
                    credit("said", "expired", Some(FAR)),
                    credit("kept", "available", Some(FAR)),
                ],
            ),
        ];
        let got: Vec<_> = bank_events(&banks).iter().map(|e| (e.kind, e.count)).collect();
        assert_eq!(
            got,
            vec![
                (BankEventKind::Redeemed, 1),
                (BankEventKind::Expired, 2),
                (BankEventKind::Unexplained, 1),
            ]
        );
        // An expired credit is no reset: a fall-free pool is left alone.
        let only_expiry = vec![
            bank(A + H, vec![credit("old", "available", Some(A + 1.5 * H))]),
            bank(at2, vec![]),
        ];
        let samples = vec![wk(A + H, 3.0), wk(at2, 4.0)];
        assert!(detect(&input(&samples, &only_expiry, &[])).is_empty());
    }

    #[test]
    fn a_credit_marked_spent_rather_than_dropped_is_a_redemption() {
        let banks = vec![
            bank(A, vec![credit("c1", "available", Some(FAR))]),
            bank(A + H, vec![credit("c1", "spent", Some(FAR))]),
        ];
        let ev = bank_events(&banks);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, BankEventKind::Redeemed);
    }

    #[test]
    fn one_redemption_resetting_two_pools_puts_them_in_one_group() {
        let five = |at: f64, pct: f64| s(at, pct, Some(A + W5), Some(W5));
        let t0 = A + H;
        let t1 = A + H + 10.0 * M;
        let banks = vec![
            bank(t0 - M, vec![credit("c1", "available", Some(FAR))]),
            bank(t1 + 30.0, vec![]),
        ];
        let short = vec![five(t0, 80.0), five(t1, 0.0)];
        let long = vec![wk(t0, 55.0), wk(t1, 0.0)];
        let a = one(&input(&short, &banks, &[]));
        let b = one(&input(&long, &banks, &[]));
        assert_eq!(a.kind, ResetKind::Banked);
        assert_eq!(b.kind, ResetKind::Banked);
        assert!(a.group_id.is_some());
        assert_eq!(a.group_id, b.group_id);
        // Two separate issued resets an hour apart are two groups.
        let x = one(&input(&[wk(t0, 50.0), wk(t1, 0.0)], &[], &[]));
        let y = one(&input(&[wk(t0 + H, 50.0), wk(t1 + H, 0.0)], &[], &[]));
        assert_ne!(x.group_id, y.group_id);
    }

    #[test]
    fn a_credit_is_counted_once_in_a_pool_even_when_its_bracket_spans_two_pairs() {
        // The bank was read before and long after; the pool fell to zero in
        // the second of two pairs inside that bracket.
        let samples = vec![wk(A + H, 40.0), wk(A + H + 10.0 * M, 41.0), wk(A + H + 20.0 * M, 0.0)];
        let banks = vec![
            bank(A, vec![credit("c1", "available", Some(FAR))]),
            bank(A + 5.0 * H, vec![]),
        ];
        let found = detect(&input(&samples, &banks, &[]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].at_hi, A + H + 20.0 * M);
        assert_eq!(found[0].kind, ResetKind::Banked);
    }

    #[test]
    fn a_gap_spanning_a_rollover_and_a_reset_records_both() {
        // Last seen two days before the weekly end, next a day after it,
        // and a credit went in between: the rollover stands, and a reset of
        // unknown effect is recorded beside it.
        let before = A + WEEK - 48.0 * H;
        let after = A + WEEK + 24.0 * H;
        let samples = vec![wk(before, 50.0), s(after, 10.0, Some(A + 2.0 * WEEK), Some(WEEK))];
        let banks = vec![
            bank(before, vec![credit("c1", "available", Some(FAR))]),
            bank(after, vec![]),
        ];
        let e = one(&input(&samples, &banks, &[]));
        assert_eq!(e.effect, CycleEffect::Unknown);
        assert_eq!(e.kind, ResetKind::Banked);
        // Only what the rollover does not explain: its own fall and its own
        // new instant are not signals of the reset.
        assert_eq!(e.signals, "bank_fall");
        assert!(!e.before_known);
        assert_eq!(evidence(&e)["rule"], "rollover_and_reset");
        let out = fold_with(&samples, &[e]);
        assert_eq!(out.cycles.len(), 2);
        assert_eq!(out.cycles[0].end_reason, Some(EndReason::Scheduled));
        assert!(out.cycles[0].consumed_is_lower_bound);
        // With nothing spent it is a rollover and nothing more.
        assert_eq!(verdicts(&input(&samples, &[], &[])), vec![Verdict::Rollover]);
        // A new window that began before the old one was due is a reset too.
        let early = vec![wk(before, 50.0), s(after, 10.0, Some(A + WEEK - 12.0 * H + WEEK), Some(WEEK))];
        let e = one(&input(&early, &[], &[]));
        assert_eq!(e.effect, CycleEffect::Unknown);
        assert_eq!(e.signals, "instant_jump");
        assert_eq!(evidence(&e)["signals"]["instant_jump"], true);
    }

    #[test]
    fn a_cached_reading_of_the_next_window_does_not_turn_the_rollover_into_a_restart() {
        // CodeRabbit's case: 60% live eight minutes before E, the next
        // window in a cached reading just after E, then 0% live in it. The
        // live pair was checked only against the cached reading's new end,
        // never reached, and the fall read as a restart at E.
        let e = A + W5;
        let mut cached = s(e + M, 2.0, Some(e + W5), Some(W5));
        cached.source = Source::Cached;
        let samples = vec![s(e - 8.0 * M, 60.0, Some(e), Some(W5)), cached, s(e + 5.0 * M, 0.0, Some(e + W5), Some(W5))];
        let i = input(&samples, &[], &[]);
        assert_eq!(verdicts(&i), vec![Verdict::Rollover], "{:#?}", judge(&i));
        assert!(detect(&i).is_empty());
        // CodeRabbit's case: a cached reading too early to close the cycle
        // only moves its end. The fold does not roll at E, so the fall in
        // the live pair across it is a reset, not a rollover.
        let mut early = s(e - 500.0, 60.0, Some(e + W5), Some(W5));
        early.source = Source::Cached;
        let samples = vec![s(e - 700.0, 60.0, Some(e), Some(W5)), early, s(e + 60.0, 0.0, Some(e + W5), Some(W5))];
        let i = input(&samples, &[], &[]);
        assert_ne!(verdicts(&i), vec![Verdict::Rollover], "{:#?}", judge(&i));
        assert_eq!(detect(&i).len(), 1, "{:#?}", judge(&i));
    }

    #[test]
    fn a_use_anchored_instant_sliding_while_idle_is_not_a_reset() {
        let samples = vec![
            s(A, 0.0, Some(A + W5), Some(W5)),
            s(A + H, 0.0, Some(A + W5 + H), Some(W5)),
            s(A + 2.0 * H, 0.0, Some(A + W5 + 1.7 * H), Some(W5)),
        ];
        let i = input(&samples, &[], &[]);
        assert_eq!(verdicts(&i), vec![Verdict::BoundaryMoved, Verdict::BoundaryMoved]);
        assert!(detect(&i).is_empty());
        // Nor while watching, when a reset would have read as a fall.
        let watching = vec![wk(A + H, 30.0), s(A + H + 10.0 * M, 31.0, Some(A + WEEK + 24.0 * H), Some(WEEK))];
        assert_eq!(verdicts(&input(&watching, &[], &[])), vec![Verdict::BoundaryMoved]);
        // CodeRabbit's case: nor when a credit spent in the same stretch
        // overlaps it. The jump alone handed the pair the credit, and the
        // boundary moving became a banked restart that split the history.
        let banks = vec![
            bank(A + H - M, vec![credit("c1", "available", Some(FAR))]),
            bank(A + H + 11.0 * M, vec![credit("c1", "spent", Some(FAR))]),
        ];
        let i = input(&watching, &banks, &[]);
        assert_eq!(verdicts(&i), vec![Verdict::BoundaryMoved], "{:#?}", judge(&i));
        // Offline, the same jump is evidence of its own and may take it.
        let offline = vec![wk(A + H, 30.0), s(A + 3.0 * H, 31.0, Some(A + WEEK + 24.0 * H), Some(WEEK))];
        let banks = vec![
            bank(A + H - M, vec![credit("c1", "available", Some(FAR))]),
            bank(A + 3.0 * H + M, vec![credit("c1", "spent", Some(FAR))]),
        ];
        assert_eq!(one(&input(&offline, &banks, &[])).kind, ResetKind::Banked);
    }

    #[test]
    fn a_rolling_pool_and_an_overage_pool_have_no_resets() {
        let w = 6.0 * H;
        let rolling: Vec<Sample> = (0..4)
            .map(|k| {
                let at = A + k as f64 * 0.5 * H;
                s(at, if k == 3 { 0.0 } else { 20.0 }, Some(at + w), Some(w))
            })
            .collect();
        assert!(judge(&input(&rolling, &[], &[])).is_empty());
        let spend = vec![s(A, 40.0, None, None), s(A + 10.0 * M, 0.0, None, None)];
        let mut i = input(&spend, &[], &[]);
        i.role = Role::Overage;
        assert!(judge(&i).is_empty());
    }

    #[test]
    fn an_undated_pool_takes_its_effect_from_config_only_when_no_instant_says() {
        // No instant at all: a fall to zero is the pool's own rollover...
        let samples = vec![s(A, 40.0, None, None), s(A + 10.0 * M, 0.0, None, None)];
        assert!(detect(&input(&samples, &[], &[])).is_empty());
        // ...but a spent credit is a reset, of unknown effect by default.
        let samples = vec![s(A, 40.0, None, None), s(A + 2.0 * H, 41.0, None, None)];
        let banks = vec![
            bank(A, vec![credit("c1", "available", Some(FAR))]),
            bank(A + 2.0 * H, vec![]),
        ];
        let e = one(&input(&samples, &banks, &[]));
        assert_eq!(e.effect, CycleEffect::Unknown);
        assert_eq!(evidence(&e)["effect"]["source"], "none");
        // The configured effect fills it in, and says it was configured.
        let mut i = input(&samples, &banks, &[]);
        i.effect_override = Some("restarted");
        let e = one(&i);
        assert_eq!(e.effect, CycleEffect::Restarted { new_start: A + 2.0 * H });
        let ev = evidence(&e);
        assert_eq!(ev["effect"]["source"], "config");
        assert_eq!(ev["effect"]["value"], "restarted");
        let out = fold::fold(&FoldInput {
            role: Role::Included,
            samples: &samples,
            plans: &[],
            resets: &[e],
            prev: None,
            next_ordinal: 1,
            rolling_seed: None,
            resume: None,
            gone_at: None,
            thresholds: Thresholds::default(),
        });
        assert_eq!(out.cycles.len(), 2);
        assert_eq!(out.cycles[0].end_reason, Some(EndReason::Reset));
        // An event with instants to read ignores the configured effect.
        let dated = vec![wk(A + H, 60.0), wk(A + H + 10.0 * M, 0.0)];
        let mut i = input(&dated, &[], &[]);
        i.effect_override = Some("restarted");
        let e = one(&i);
        assert_eq!(e.effect, CycleEffect::KeptBoundary);
        assert_eq!(evidence(&e)["effect"]["source"], "instant");
    }

    #[test]
    fn a_reset_dated_on_one_side_only_never_takes_the_configured_effect() {
        // CodeRabbit's case: 60% with a reset date, then 0% with none. The
        // help says a date on either side keeps the setting out; the code
        // reached for it whenever the pair could not place the effect.
        let samples = vec![
            s(A + H, 60.0, Some(A + WEEK), Some(WEEK)),
            s(A + H + 10.0 * M, 0.0, None, None),
        ];
        let mut i = input(&samples, &[], &[]);
        i.effect_override = Some("restarted");
        let e = one(&i);
        assert_eq!(e.effect, CycleEffect::Unknown);
        assert_eq!(evidence(&e)["effect"]["source"], "none");
        // Two dates that differ, with no window to start a restart from.
        let samples = vec![s(A + H, 60.0, Some(A + WEEK), None), s(A + H + 10.0 * M, 0.0, Some(A + 2.0 * WEEK), None)];
        let mut i = input(&samples, &[], &[]);
        i.effect_override = Some("restarted");
        for e in detect(&i) {
            assert_ne!(evidence(&e)["effect"]["source"], "config", "{e:?}");
        }
    }

    #[test]
    fn the_reset_effect_setting_reads_known_providers_and_effects_only() {
        let v = serde_json::json!({"droid": "restarted", "claude": "kept_boundary", "nobody": "restarted", "codex": "sideways"});
        let r = ResetEffects::from_config(&v);
        assert_eq!(r.get("droid"), Some(EffectOverride::Restarted));
        assert_eq!(r.get("claude"), Some(EffectOverride::KeptBoundary));
        assert_eq!(r.get("codex"), None);
        assert_eq!(r.get("nobody"), None);
        assert_eq!(ResetEffects::from_config(&serde_json::Value::Null), ResetEffects::default());
    }

    #[test]
    fn a_kept_cycles_last_reading_is_compared_with_the_first_new_one() {
        // Folding on from a kept cycle: `before` is its last reading, and
        // only pairs ending in `samples` are judged.
        let before = wk(A + H, 80.0);
        let samples = vec![wk(A + H + 10.0 * M, 0.0), wk(A + 2.0 * H, 5.0)];
        let mut i = input(&samples, &[], &[]);
        i.before = Some(&before);
        let j = judge(&i);
        assert_eq!(j.len(), 2);
        assert!(matches!(j[0].verdict, Verdict::Reset(_)));
        assert_eq!(j[0].at_lo, A + H);
    }

    // ===== Through the store: rows written, and re-folded =====

    use super::super::store::History;
    use super::super::{Observation, fold::Source as Src};
    use crate::shared::Lane;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "opscope-resets-{name}-{}-{}",
            std::process::id(),
            opscope_core::now().to_bits()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(super::super::store::FILE)
    }

    fn lane(pct: f64, at: f64) -> Lane {
        Lane {
            label: "7d".into(),
            pct,
            window_secs: Some(WEEK),
            reset: Some(A + WEEK),
            pool_key: "secondary_window".into(),
            fetched_at: at,
            ..Lane::default()
        }
    }

    fn obs(lanes: Vec<Lane>, bank: Option<BankReading>) -> Observation {
        Observation {
            provider: "codex".into(),
            account_id: "acct-1".into(),
            heading: "CODEX".into(),
            tab: "codex".into(),
            lanes,
            bank,
            ..Observation::default()
        }
    }

    #[test]
    fn an_out_of_order_sample_refolds_the_pools_resets() {
        let p = temp("order");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        // Two readings three hours apart, 40% then 38%: an adjustment.
        h.record(&[obs(vec![lane(40.0, A + H)], None)], &t, A + H).unwrap();
        h.record(&[obs(vec![lane(38.0, A + 4.0 * H)], None)], &t, A + 4.0 * H)
            .unwrap();
        assert_eq!(h.count("reset_event"), 0);
        // Another pane's older fetch lands between them: 0% at hour two.
        // Re-folded, 40 -> 0 is a reset and 0 -> 38 a climb.
        h.record(&[obs(vec![lane(0.0, A + 2.0 * H)], None)], &t, A + 5.0 * H)
            .unwrap();
        assert_eq!(h.count("reset_event"), 1);
        let key = obs(vec![], None).account_key();
        let cycles: Vec<Cycle> = h.cycles("codex", &key, "secondary_window").unwrap();
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].allowances, 2);
        assert_eq!(cycles[0].consumed_pct, Some(78.0));
        // Found offline, so what the first allowance used is "at least".
        assert!(cycles[0].consumed_is_lower_bound);
        let samples = h.samples("codex", &key, "secondary_window").unwrap();
        assert!(samples.iter().all(|s| s.source == Src::Live));
    }

    #[test]
    fn a_bank_read_after_the_reset_fills_in_its_kind_on_the_next_fold() {
        let p = temp("bank-later");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let held = BankReading {
            at: A + H,
            left: 1,
            credits: Some(vec![credit("c1", "available", Some(FAR))]),
        };
        h.record(&[obs(vec![lane(60.0, A + H)], Some(held))], &t, A + H).unwrap();
        // The pool falls to zero; the bank is not read on this pass.
        h.record(&[obs(vec![lane(0.0, A + H + 10.0 * M)], None)], &t, A + H + 10.0 * M)
            .unwrap();
        // `count` takes the rest of a FROM clause, which is enough to ask
        // for one kind without a reader made for the test.
        let kind = |h: &History, k: &str| h.count(&format!("reset_event WHERE kind = '{k}'"));
        assert_eq!(kind(&h, "unknown"), 1);
        // Read later, the credit is gone: the same reset is banked now.
        let gone = BankReading {
            at: A + H + 12.0 * M,
            left: 0,
            credits: Some(vec![]),
        };
        h.record(&[obs(vec![], Some(gone))], &t, A + H + 12.0 * M).unwrap();
        assert_eq!(kind(&h, "banked"), 1);
        assert_eq!(h.count("reset_event"), 1);
        assert_eq!(h.count("bank_event"), 1);
    }
}
