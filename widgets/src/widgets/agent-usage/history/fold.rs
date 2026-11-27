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

//! The cycle fold: samples in, cycles out. Pure, ungated, and deterministic.
//!
//! Samples are the record (spec §3.5). Everything here is derived from them
//! and can be thrown away and rebuilt: when a sample arrives out of order,
//! when a backfill lands, and when [`DETECTOR_VERSION`] moves. Nothing in
//! this file reads a clock, a file or the database - the store hands it the
//! samples, the plan changes and the reset events, and writes back what it
//! returns.
//!
//! # What it does (phase 1)
//!
//! * Opens a cycle at the first sample, starting where the server's reset
//!   instant and window put it (`reset_at - window_secs`).
//! * Closes it on schedule (§5.4 rule 1) when a sample lands at or past its
//!   `ends_at` (less [`tolerance`], and never while it still states that
//!   same end: [`rolled_over`]); the new cycle's model is read off that
//!   boundary (§5.1): a start equal to the old end is `contiguous`, a later
//!   one `use_anchored`.
//! * Closes it on a plan change, a limit change and a window change
//!   (§5.4 rules 2 and 3), which are not resets.
//! * Writes hollow cycles (`coverage = 0`, no figures) for whole windows a
//!   contiguous pool passed through unseen (§5.8). A use-anchored pool's gap
//!   is idle time and gets nothing.
//! * Keeps no cycles for a rolling pool, nor for a balance (§5.1, §6.2).
//! * Infers an undated pool's cycles from its figure falling back to about
//!   zero (`start_source = 'inferred'`, §5.1 and §6.2).
//! * Works out coverage and whether a closed cycle's last figure is only a
//!   lower bound (§5.8).
//!
//! # The seam for resets (phase 2)
//!
//! Reset detection is not done here. [`super::resets::detect`] reads the
//! same samples and returns [`ResetEvent`]s; the fold takes them as input
//! ([`FoldInput::resets`]) and applies each one where it falls:
//!
//! * [`CycleEffect::KeptBoundary`] starts a new [`Segment`] in the current
//!   cycle and adds an allowance;
//! * [`CycleEffect::Restarted`] closes the cycle at `new_start` with
//!   `end_reason = 'reset'` and opens the next one there;
//! * [`CycleEffect::Unknown`] changes nothing about the boundaries.
//!
//! That application is written and tested here, so phase 2 only has to fill
//! `detect` and the fold already knows what to do with what it returns.

/// Bumped whenever a change here, or in [`super::resets`], would fold the
/// same samples differently. A file stamped with another version is folded
/// again from its samples when it is opened.
pub const DETECTOR_VERSION: i64 = 5;

/// A figure at or above this is "exhausted" (§3.6 `exhausted_at`, §6.2).
pub const EXHAUSTED_PCT: f64 = 99.5;

/// A gap between two samples longer than this is unobserved time (§5.8).
pub const COVERAGE_GAP: f64 = 2.0 * 3600.0;

/// A closed cycle last seen more than this before its end has only a lower
/// bound for its final figure (§5.8).
pub const LOWER_BOUND_AFTER: f64 = 3600.0;

/// How far two instants may differ and still be the same boundary:
/// `max(2 min, 1% of the window)` (§5.3).
pub fn tolerance(window_secs: Option<f64>) -> f64 {
    (window_secs.unwrap_or(0.0) * 0.01).max(120.0)
}

/// Where a sample came from (§4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Fetched from the provider for this reading.
    Live,
    /// A cache or a snapshot - Claude's fallback file, Codex's rollout, a
    /// reading kept through a failure. It extends a cycle and can close one
    /// on schedule, but is never reset evidence.
    Cached,
    /// Replayed from a provider's own log (§8). Full evidence.
    Backfill,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Live => "live",
            Source::Cached => "cached",
            Source::Backfill => "backfill",
        }
    }

    pub fn from_name(name: &str) -> Source {
        match name {
            "cached" => Source::Cached,
            "backfill" => Source::Backfill,
            _ => Source::Live,
        }
    }
}

/// One stored reading of one pool: a row of `sample`.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// The fetch time, wall-clock epoch seconds. Never "now".
    pub at: f64,
    pub pct: Option<f64>,
    pub used: Option<f64>,
    pub limit: Option<f64>,
    /// The window as this reading stated it.
    pub window_secs: Option<f64>,
    /// The server's stated reset instant. `None` when none was sent; a
    /// projected (worked-out) instant is never stored.
    pub reset_at: Option<f64>,
    pub source: Source,
    /// `no` / `alone` / `shared` / `separate` on Cursor and Grok, else `None`.
    pub spacexsi: Option<String>,
}

/// An account's plan, from the time it was first seen (`plan_sample`).
#[derive(Clone, Debug, PartialEq)]
pub struct PlanChange {
    pub at: f64,
    pub plan: String,
}

/// What a pool is, as far as the fold needs to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Role {
    #[default]
    Included,
    Overage,
    Balance,
}

impl Role {
    pub fn from_name(name: &str) -> Role {
        match name {
            "overage" => Role::Overage,
            "balance" => Role::Balance,
            _ => Role::Included,
        }
    }
}

/// How a cycle's boundaries behaved (§5.1). Observed, never declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CycleModel {
    /// Each cycle starts where the last ended.
    Contiguous,
    /// The next window starts at first use after the last one expired.
    UseAnchored,
    /// The reset instant slides with the clock. No cycles are kept.
    Rolling,
    /// No reset instant at all; boundaries inferred from the figure.
    Undated,
}

impl CycleModel {
    pub fn name(self) -> &'static str {
        match self {
            CycleModel::Contiguous => "contiguous",
            CycleModel::UseAnchored => "use_anchored",
            CycleModel::Rolling => "rolling",
            CycleModel::Undated => "undated",
        }
    }
}

/// Where a cycle's `starts_at` came from (§3.6 `start_source`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartSource {
    /// The server's reset instant less its window.
    Server,
    /// Worked out from the window length: a hollow cycle nobody saw.
    Derived,
    /// The first observation, or a figure falling to zero, or a plan or
    /// limit change seen between two readings.
    Inferred,
    /// A restarted reset's own instant (phase 2).
    Reset,
}

impl StartSource {
    pub fn name(self) -> &'static str {
        match self {
            StartSource::Server => "server",
            StartSource::Derived => "derived",
            StartSource::Inferred => "inferred",
            StartSource::Reset => "reset",
        }
    }
}

/// Why a cycle closed (§3.6 `end_reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    Scheduled,
    Reset,
    WindowChanged,
    PlanChanged,
    LimitChanged,
    /// The pool missing from live readings for a whole window while its
    /// siblings answered live (§4): worked out by the store from the
    /// readings ([`FoldInput::gone_at`]) and applied to the open cycle.
    PoolGone,
}

impl EndReason {
    pub fn name(self) -> &'static str {
        match self {
            EndReason::Scheduled => "scheduled",
            EndReason::Reset => "reset",
            EndReason::WindowChanged => "window_changed",
            EndReason::PlanChanged => "plan_changed",
            EndReason::LimitChanged => "limit_changed",
            EndReason::PoolGone => "pool_gone",
        }
    }
}

/// What a reset did to the cycle it happened in (§5.5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CycleEffect {
    /// The allowance refilled and the cycle carries on to its old end.
    KeptBoundary,
    /// The old cycle closes at `new_start` (`N.reset_at - N.window_secs`)
    /// and a new one opens there.
    Restarted { new_start: f64 },
    /// Neither side stated an instant to read the effect from.
    Unknown,
}

impl CycleEffect {
    pub fn name(self) -> &'static str {
        match self {
            CycleEffect::KeptBoundary => "kept_boundary",
            CycleEffect::Restarted { .. } => "restarted",
            CycleEffect::Unknown => "unknown",
        }
    }
}

/// Banked, issued, or not known (§5.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetKind {
    Banked,
    Issued,
    Unknown,
}

impl ResetKind {
    pub fn name(self) -> &'static str {
        match self {
            ResetKind::Banked => "banked",
            ResetKind::Issued => "issued",
            ResetKind::Unknown => "unknown",
        }
    }
}

/// A reset established between two readings of one pool: a row of
/// `reset_event`, less the ids the store assigns. Produced by
/// [`super::resets::detect`] (phase 2); consumed by [`fold`].
#[derive(Clone, Debug, PartialEq)]
pub struct ResetEvent {
    /// It happened after the reading at `at_lo` and by the one at `at_hi`.
    /// `at_hi` is the sample the fold applies it at.
    pub at_lo: f64,
    pub at_hi: f64,
    pub kind: ResetKind,
    pub effect: CycleEffect,
    /// Which of `pct_fall`, `instant_jump`, `bank_fall` fired, comma-joined.
    pub signals: String,
    pub pct_before: Option<f64>,
    pub pct_after: Option<f64>,
    pub reset_at_before: Option<f64>,
    pub reset_at_after: Option<f64>,
    /// Shared by every pool one redemption reset (§5.6).
    pub group_id: Option<i64>,
    /// JSON: the rules, thresholds and sources that decided it.
    pub evidence: String,
    /// Whether the figure just before the reset is known: false when the
    /// reset was found by the bank or the instant alone, or after an offline
    /// gap, so the cycle's `consumed_pct` is only a lower bound (§5.7).
    /// Kept in `evidence` as `before_known`, not a column of its own.
    pub before_known: bool,
}

/// A stretch of a cycle between kept-boundary resets (`segment`).
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    /// 1, 2, 3 ... within the cycle.
    pub n: u32,
    pub starts_at: f64,
    pub ends_at: Option<f64>,
    pub peak_pct: Option<f64>,
}

/// One cycle of one pool: a row of `cycle` plus its segments.
#[derive(Clone, Debug, PartialEq)]
pub struct Cycle {
    /// 1, 2, 3 ... per pool, the dedupe key.
    pub ordinal: u32,
    pub starts_at: f64,
    /// The scheduled end, as last stated.
    pub ends_at: Option<f64>,
    /// `None` while the cycle is open.
    pub closed_at: Option<f64>,
    pub window_secs: Option<f64>,
    pub model: CycleModel,
    pub start_source: StartSource,
    pub end_reason: Option<EndReason>,
    pub plan: Option<String>,
    /// Highest single-segment reading.
    pub peak_pct: Option<f64>,
    /// Sum of segment peaks; passes 100 when a kept-boundary reset refilled
    /// the allowance (§5.7).
    pub consumed_pct: Option<f64>,
    /// 1 + kept-boundary resets.
    pub allowances: u32,
    /// `100 × allowances`.
    pub available_pct: Option<f64>,
    /// The last reading came more than [`LOWER_BOUND_AFTER`] before the end,
    /// so the figure is "at least" (§5.8). Always true of a hollow cycle.
    pub end_is_lower_bound: bool,
    pub used_amount: Option<f64>,
    pub limit_amount: Option<f64>,
    /// First reading at or above [`EXHAUSTED_PCT`].
    pub exhausted_at: Option<f64>,
    pub first_obs_at: Option<f64>,
    pub last_obs_at: Option<f64>,
    /// 0..1: how much of the cycle was watched (§5.8). For an open cycle,
    /// of the part up to its last observation.
    pub coverage: f64,
    /// A reset in this cycle came with no known figure before it (§5.7), so
    /// `consumed_pct` is "at least". Derived: the store reads it back from
    /// the cycle's reset events rather than from a column.
    pub consumed_is_lower_bound: bool,
    pub segments: Vec<Segment>,
}

impl Cycle {
    /// A cycle with no observation in it: a window a contiguous pool passed
    /// through while nothing was watching.
    pub fn is_hollow(&self) -> bool {
        self.first_obs_at.is_none()
    }

    /// Cut short by a `restarted` reset (§5.7): a two-day "week" at 60% is
    /// not comparable with a whole one, so it is left out of averages by
    /// default and drawn at its true width.
    pub fn cut_short(&self) -> bool {
        self.end_reason == Some(EndReason::Reset)
    }

    fn open(
        ordinal: u32,
        starts_at: f64,
        ends_at: Option<f64>,
        window_secs: Option<f64>,
        model: CycleModel,
        start_source: StartSource,
    ) -> Cycle {
        Cycle {
            ordinal,
            starts_at,
            ends_at,
            closed_at: None,
            window_secs,
            model,
            start_source,
            end_reason: None,
            plan: None,
            peak_pct: None,
            consumed_pct: None,
            allowances: 1,
            available_pct: None,
            end_is_lower_bound: false,
            used_amount: None,
            limit_amount: None,
            exhausted_at: None,
            first_obs_at: None,
            last_obs_at: None,
            coverage: 0.0,
            consumed_is_lower_bound: false,
            segments: Vec::new(),
        }
    }
}

/// How a whole pool behaves, as the fold found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolShape {
    /// Cycles with server-stated boundaries.
    Dated,
    /// No instant; cycles inferred from the figure returning to zero.
    Undated,
    /// The instant slides with the clock; no cycles.
    Rolling,
    /// A balance with no cycle (§6.2); no cycles.
    Balance,
}

/// The thresholds the fold and the reset detector read, from settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    /// Two readings this close are "watching"; further apart, "offline"
    /// (§5.2). `history_watch_gap`, 15 minutes.
    pub watch_gap_secs: f64,
    /// A figure at or below this is "about zero" (§5.3). `history_near_zero`.
    pub near_zero_pct: f64,
    /// After an offline gap a fall of at least the larger of these counts
    /// (§5.3): `history_offline_fall` points, `history_offline_fall_pct`
    /// percent of the earlier figure.
    pub offline_fall_pts: f64,
    pub offline_fall_share: f64,
    /// `history_reset_effect`: per provider, the effect recorded for a reset
    /// with no instant to read one from (§5.5).
    pub reset_effect: super::resets::ResetEffects,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            watch_gap_secs: 15.0 * 60.0,
            near_zero_pct: 1.0,
            offline_fall_pts: 3.0,
            offline_fall_share: 5.0,
            reset_effect: super::resets::ResetEffects::default(),
        }
    }
}

/// Everything one pool's fold reads.
#[derive(Clone, Debug)]
pub struct FoldInput<'a> {
    pub role: Role,
    /// The samples to fold, oldest first. Only those after `prev`'s last
    /// observation when folding from a kept cycle.
    pub samples: &'a [Sample],
    /// The account's plan history, oldest first, all of it.
    pub plans: &'a [PlanChange],
    /// Resets inside the samples' span, from [`super::resets::detect`].
    pub resets: &'a [ResetEvent],
    /// The last cycle kept from an earlier fold, closed. Its end is where
    /// the next cycle's model is read from.
    pub prev: Option<&'a Cycle>,
    /// The ordinal the first cycle produced takes.
    pub next_ordinal: u32,
    /// The reading before `samples`, when it is known to be in an
    /// established rolling stretch ([`rolling_mask`]).
    pub rolling_seed: Option<&'a Sample>,
    /// An undated pool's open cycle to carry on from, rather than folding
    /// it again from its first reading: `samples` are those after its last
    /// observation. See [`Resume`].
    pub resume: Option<&'a Resume>,
    /// The time the pool's open cycle closes as `pool_gone` (§4), worked
    /// out by the store from its siblings' readings; ignored unless it is
    /// after the cycle's last observation.
    pub gone_at: Option<f64>,
    pub thresholds: Thresholds,
}

/// An undated pool's open cycle as stored, to fold on from (the fold
/// checkpoint for a pool that has nothing closed to keep: Claude's extra
/// usage on a cap nobody reaches never falls back to zero, so its one cycle
/// would otherwise be folded again from its first reading on every write).
#[derive(Clone, Debug, PartialEq)]
pub struct Resume {
    /// The open cycle, with its segments; the last segment is the open one.
    pub cycle: Cycle,
    /// Seconds of it watched so far: its coverage times its span to its last
    /// observation.
    pub covered: f64,
    /// The last figure and amount that can mark a boundary (not cached),
    /// at or before its last observation.
    pub last_pct: Option<f64>,
    pub last_used: Option<f64>,
}

/// What a fold produced.
#[derive(Clone, Debug, PartialEq)]
pub struct FoldOutput {
    pub shape: PoolShape,
    /// Oldest first; at most the last is open.
    pub cycles: Vec<Cycle>,
}

/// The plan in force at `at`: the latest change at or before it.
fn plan_at(plans: &[PlanChange], at: f64) -> Option<&str> {
    plans.iter().rev().find(|p| p.at <= at).map(|p| p.plan.as_str())
}

/// The first plan change strictly after `lo` and at or before `hi` that
/// names a different plan from the one in force at `lo`.
pub fn plan_change_between(plans: &[PlanChange], lo: f64, hi: f64) -> Option<&PlanChange> {
    let before = plan_at(plans, lo)?;
    plans.iter().find(|p| p.at > lo && p.at <= hi && p.plan != before)
}

fn differs(a: Option<f64>, b: Option<f64>, by: f64) -> bool {
    matches!((a, b), (Some(a), Some(b)) if (a - b).abs() > by)
}

/// Whether reading `n` has passed the end of a cycle that was due at `end`
/// (§5.4 rule 1). A reading that states the same instant, within `tol`, is
/// still in that cycle: the server says the reset has not happened, so it
/// is no rollover until it is well past the end. One that states a later
/// instant rolls once it is within `early` of the end - a reset instant
/// read as "now plus seconds left" jitters by the request's latency. One
/// that states no instant rolls only once the end has passed.
pub fn rolled_over(end: f64, n: &Sample, tol: f64, early: f64) -> bool {
    match n.reset_at {
        Some(r) if (r - end).abs() <= tol => n.at >= end + tol,
        Some(r) if r > end => n.at >= end - early,
        _ => n.at >= end,
    }
}

/// Which readings belong to a stretch where the pool's reset instant
/// slides with the clock (§5.1 rolling): three readings in a row, each a
/// window ahead of its own fetch time and in use (above `near` - an unused
/// window states "now plus a window" too, and that is an unstarted window,
/// not a rolling one), the instant moving later each time, spanning more
/// than two tolerances. Once a stretch is established, readings that go on
/// sliding belong to it even as the figure decays to zero. A fixed
/// window's instant stands still while the clock catches it up.
///
/// Judged per stretch, never for the whole pool: a rolling stretch keeps no
/// cycles, and the readings either side of it fold as they would without it.
///
/// `seed`, where given, is the reading just before `samples`, known to be
/// in an established rolling stretch: the store folds a pool that has only
/// ever been rolling on from its last reading rather than from its first.
pub fn rolling_mask(samples: &[Sample], near: f64, seed: Option<&Sample>) -> Vec<bool> {
    let ahead = |s: &Sample| match (s.reset_at, s.window_secs) {
        (Some(r), Some(w)) if w > 0.0 => (r - s.at - w).abs() <= tolerance(Some(w)),
        _ => false,
    };
    let used = |s: &Sample| s.pct.is_some_and(|p| p > near);
    let mut mask = vec![false; samples.len()];
    // The readings of the run so far, with their index (`None`: the seed).
    let mut run: Vec<(Option<usize>, &Sample)> = seed.map(|s| (None, s)).into_iter().collect();
    let mut established = seed.is_some();
    for (i, s) in samples.iter().enumerate() {
        let slid = run.last().is_some_and(|(_, p)| s.reset_at > p.reset_at);
        if ahead(s) && slid && (used(s) || established) {
            run.push((Some(i), s));
        } else {
            run.clear();
            established = false;
            if ahead(s) && used(s) {
                run.push((Some(i), s));
            }
        }
        if established {
            mask[i] = true;
        } else if run.len() >= 3 {
            let (first, last) = (run[0].1, run[run.len() - 1].1);
            if last.at - first.at > 2.0 * tolerance(first.window_secs) {
                established = true;
                for k in run.iter().filter_map(|(k, _)| *k) {
                    mask[k] = true;
                }
            }
        }
    }
    mask
}

/// Whether every dated reading of `samples` is in a rolling stretch (`mask`,
/// from [`rolling_mask`]): the pool, as far as these readings show, keeps no
/// cycles at all.
pub fn is_rolling(samples: &[Sample], mask: &[bool]) -> bool {
    samples.iter().any(|s| s.reset_at.is_some())
        && samples.iter().zip(mask).all(|(s, m)| *m || s.reset_at.is_none())
}

/// The running state of the cycle being built.
struct Building {
    cycle: Cycle,
    /// The previous observation in this cycle, for coverage.
    last_at: Option<f64>,
    covered: f64,
    seg_start: f64,
    seg_peak: Option<f64>,
}

impl Building {
    fn new(cycle: Cycle) -> Building {
        let seg_start = cycle.starts_at;
        Building {
            cycle,
            last_at: None,
            covered: 0.0,
            seg_start,
            seg_peak: None,
        }
    }

    /// The open cycle a [`Resume`] holds, as it stood after its last
    /// observation: [`Building::finish`] pushed its open segment, so that
    /// segment is taken back off and carried on.
    fn resumed(r: &Resume) -> Building {
        let mut cycle = r.cycle.clone();
        let open = cycle.segments.pop();
        let seg_start = open.as_ref().map_or(cycle.starts_at, |s| s.starts_at);
        let seg_peak = open.and_then(|s| s.peak_pct);
        cycle.closed_at = None;
        cycle.end_reason = None;
        Building {
            last_at: cycle.last_obs_at,
            covered: r.covered,
            seg_start,
            seg_peak,
            cycle,
        }
    }

    /// Finish the cycle still open at the end of a fold: closed as
    /// `pool_gone` at `gone` when that is after its last observation,
    /// otherwise left open.
    fn finish_open(self, gone: Option<f64>) -> Cycle {
        let last = self.cycle.last_obs_at.unwrap_or(self.cycle.starts_at);
        match gone.filter(|g| *g > last) {
            Some(at) => self.finish(Some((at, EndReason::PoolGone))),
            None => self.finish(None),
        }
    }

    fn observe(&mut self, s: &Sample, plans: &[PlanChange]) {
        let c = &mut self.cycle;
        if let Some(prev) = self.last_at {
            let gap = s.at - prev;
            if gap > 0.0 && gap <= COVERAGE_GAP {
                self.covered += gap;
            }
        }
        self.last_at = Some(s.at);
        c.first_obs_at.get_or_insert(s.at);
        c.last_obs_at = Some(s.at);
        // An instant at or before the cycle's own start is the end of a
        // window already behind it - a reading past the tolerance that
        // still states the old end - and never this cycle's end.
        if let Some(r) = s.reset_at.filter(|r| *r > c.starts_at + tolerance(s.window_secs.or(c.window_secs))) {
            c.ends_at = Some(r);
        }
        if s.window_secs.is_some() {
            c.window_secs = s.window_secs;
        }
        if let Some(p) = s.pct {
            self.seg_peak = Some(self.seg_peak.map_or(p, |q: f64| q.max(p)));
            if p >= EXHAUSTED_PCT && c.exhausted_at.is_none() {
                c.exhausted_at = Some(s.at);
            }
        }
        if let Some(u) = s.used {
            c.used_amount = Some(c.used_amount.map_or(u, |v: f64| v.max(u)));
        }
        if s.limit.is_some() {
            c.limit_amount = s.limit;
        }
        if let Some(plan) = plan_at(plans, s.at) {
            c.plan = Some(plan.to_string());
        }
    }

    /// Close the current segment at `at` and start the next one there.
    fn split(&mut self, at: f64) {
        self.push_segment(Some(at));
        self.cycle.allowances += 1;
        self.seg_start = at;
        self.seg_peak = None;
    }

    fn push_segment(&mut self, ends_at: Option<f64>) {
        let n = self.cycle.segments.len() as u32 + 1;
        self.cycle.segments.push(Segment {
            n,
            starts_at: self.seg_start,
            ends_at,
            peak_pct: self.seg_peak,
        });
    }

    /// Finish the cycle: closed at `closed` for `why`, or left open.
    fn finish(mut self, closed: Option<(f64, EndReason)>) -> Cycle {
        let end = closed.map(|(at, _)| at).or(self.cycle.ends_at);
        self.push_segment(closed.map(|(at, _)| at).or(self.cycle.ends_at));
        let c = &mut self.cycle;
        if let Some((at, why)) = closed {
            c.closed_at = Some(at);
            c.end_reason = Some(why);
            c.end_is_lower_bound = c.last_obs_at.is_none_or(|last| at - last > LOWER_BOUND_AFTER);
        }
        let peaks: Vec<f64> = c.segments.iter().filter_map(|s| s.peak_pct).collect();
        c.peak_pct = peaks.iter().copied().reduce(f64::max);
        c.consumed_pct = (!peaks.is_empty()).then(|| peaks.iter().sum());
        c.available_pct = Some(100.0 * c.allowances as f64);
        // Coverage of the whole cycle once it is closed; of the part up to
        // the last observation while it is open, since the fold has no clock.
        let until = if closed.is_some() { end } else { c.last_obs_at };
        let span = until.map(|u| u - c.starts_at).unwrap_or(0.0);
        c.coverage = if span > 0.0 {
            (self.covered / span).clamp(0.0, 1.0)
        } else if c.first_obs_at.is_some() {
            1.0
        } else {
            0.0
        };
        self.cycle
    }
}

/// A hollow cycle: a whole window nothing observed.
fn hollow(ordinal: u32, starts_at: f64, window: f64, plan: Option<String>) -> Cycle {
    let mut c = Cycle::open(
        ordinal,
        starts_at,
        Some(starts_at + window),
        Some(window),
        CycleModel::Contiguous,
        StartSource::Derived,
    );
    c.closed_at = Some(starts_at + window);
    c.end_reason = Some(EndReason::Scheduled);
    c.end_is_lower_bound = true;
    c.plan = plan;
    c
}

/// Fold one pool's samples into cycles. See the module comment.
pub fn fold(input: &FoldInput) -> FoldOutput {
    let samples = input.samples;
    if input.role == Role::Balance {
        return FoldOutput {
            shape: PoolShape::Balance,
            cycles: Vec::new(),
        };
    }
    if input.resume.is_some() {
        return fold_undated(input);
    }
    let dated = samples.iter().any(|s| s.reset_at.is_some());
    if !dated && input.prev.is_none_or(|p| p.model == CycleModel::Undated) {
        return fold_undated(input);
    }
    let mask = rolling_mask(samples, input.thresholds.near_zero_pct, input.rolling_seed);
    if !mask.contains(&true) {
        return fold_dated(input);
    }
    if is_rolling(samples, &mask) {
        // Every dated reading is rolling; readings with no instant outside
        // the stretch are still folded, as an undated pool's.
        let undated: Vec<Sample> = samples
            .iter()
            .zip(&mask)
            .filter(|(s, m)| !**m && s.reset_at.is_none())
            .map(|(s, _)| s.clone())
            .collect();
        if undated.is_empty() {
            return FoldOutput {
                shape: PoolShape::Rolling,
                cycles: Vec::new(),
            };
        }
        return fold_undated(&FoldInput {
            samples: &undated,
            ..input.clone()
        });
    }
    // A rolling stretch is left out; the readings either side fold as if
    // it were a gap.
    let fixed: Vec<Sample> = samples
        .iter()
        .zip(&mask)
        .filter(|(_, m)| !**m)
        .map(|(s, _)| s.clone())
        .collect();
    fold_dated(&FoldInput {
        samples: &fixed,
        ..input.clone()
    })
}

fn fold_dated(input: &FoldInput) -> FoldOutput {
    let mut out: Vec<Cycle> = Vec::new();
    let mut ordinal = input.next_ordinal;
    let mut cur: Option<Building> = None;
    // Whether each produced cycle's model was read off a boundary, so a
    // first cycle that had none can take the model of the next boundary.
    let mut observed_model: Vec<bool> = Vec::new();
    let mut prev_sample: Option<&Sample> = None;
    let plans = input.plans;

    for s in input.samples {
        let Some(b) = cur.as_mut() else {
            // Folding on from a kept cycle that closed for something other
            // than its schedule: the sample after it is the one that closed
            // it, and the next cycle starts where it closed, as a fold over
            // everything opened it - never back at the server's start, which
            // would overlap the kept cycle.
            let seeded = input.prev.and_then(|p| {
                let at = p.closed_at?;
                let (source, seen) = match p.end_reason? {
                    EndReason::PlanChanged | EndReason::LimitChanged => (StartSource::Inferred, false),
                    EndReason::WindowChanged => (StartSource::Server, false),
                    EndReason::Reset => (StartSource::Reset, true),
                    EndReason::Scheduled | EndReason::PoolGone => return None,
                };
                Some((at, source, seen))
            });
            // Folding on from a kept cycle that closed on schedule: this is
            // the reading that closed it, and the next cycle opens exactly
            // as a fold over everything opened it at that reading.
            if let Some((p, end)) = input
                .prev
                .filter(|p| p.end_reason == Some(EndReason::Scheduled))
                .and_then(|p| Some((p, p.closed_at?)))
            {
                let window = p.window_secs.or(s.window_secs);
                cur = Some(after_end(
                    end,
                    window,
                    p.model,
                    p.plan.clone(),
                    s,
                    plans,
                    &mut ordinal,
                    &mut out,
                    &mut observed_model,
                ));
                prev_sample = Some(s);
                continue;
            }
            if let Some((start, source, seen)) = seeded {
                let mut b = Building::new(Cycle::open(
                    ordinal,
                    start,
                    s.reset_at,
                    s.window_secs,
                    CycleModel::Contiguous,
                    source,
                ));
                ordinal += 1;
                observed_model.push(seen);
                b.observe(s, plans);
                cur = Some(b);
                prev_sample = Some(s);
                continue;
            }
            // The first cycle of this fold.
            let (start, mut source) = match (s.reset_at, s.window_secs) {
                (Some(r), Some(w)) if w > 0.0 => (r - w, StartSource::Server),
                _ => (s.at, StartSource::Inferred),
            };
            let prev_end = input.prev.and_then(|p| p.closed_at.or(p.ends_at));
            // Folding on from a kept cycle: the windows a contiguous pool
            // passed through unseen since it are hollow cycles, exactly as
            // a fold over everything would have written them.
            if let (Some(p), Some(end), Some(w)) = (input.prev, prev_end, s.window_secs.filter(|w| *w > 0.0)) {
                let n = ((start - end) / w).round();
                let aligned = n >= 1.0 && (start - (end + n * w)).abs() <= tolerance(Some(w));
                if aligned && p.model == CycleModel::Contiguous && source == StartSource::Server {
                    for k in 0..n as u32 {
                        out.push(hollow(ordinal, end + k as f64 * w, w, p.plan.clone()));
                        observed_model.push(true);
                        ordinal += 1;
                    }
                }
            }
            let last_end = out.last().and_then(|c| c.closed_at).or(prev_end);
            let (model, seen) = match last_end {
                Some(end) if (start - end).abs() <= tolerance(s.window_secs) => (CycleModel::Contiguous, true),
                Some(end) if start > end => (CycleModel::UseAnchored, true),
                _ => (input.prev.map_or(CycleModel::Contiguous, |p| p.model), false),
            };
            // Folding on from the cycle a restarted reset closed: this one
            // starts at that reset's instant, as a fold over everything has it.
            if input
                .resets
                .iter()
                .any(|e| e.at_hi == s.at && matches!(e.effect, CycleEffect::Restarted { .. }))
            {
                source = StartSource::Reset;
            }
            let mut b = Building::new(Cycle::open(ordinal, start, s.reset_at, s.window_secs, model, source));
            ordinal += 1;
            observed_model.push(seen);
            b.observe(s, plans);
            cur = Some(b);
            prev_sample = Some(s);
            continue;
        };

        let p = prev_sample.expect("a cycle is open only after a sample");
        let tol = tolerance(s.window_secs.or(b.cycle.window_secs));

        // A reset established at this sample (phase 2 fills `resets`).
        if let Some(ev) = input.resets.iter().find(|e| e.at_hi == s.at) {
            // The figure before the reset is unknown: what this cycle used
            // is "at least" (§5.7).
            if !ev.before_known {
                b.cycle.consumed_is_lower_bound = true;
            }
            match ev.effect {
                CycleEffect::KeptBoundary => b.split(ev.at_hi),
                CycleEffect::Restarted { new_start } => {
                    let done = cur.take().expect("open").finish(Some((new_start, EndReason::Reset)));
                    out.push(done);
                    let mut nb = Building::new(Cycle::open(
                        ordinal,
                        new_start,
                        s.reset_at,
                        s.window_secs,
                        CycleModel::Contiguous,
                        StartSource::Reset,
                    ));
                    ordinal += 1;
                    observed_model.push(true);
                    nb.observe(s, plans);
                    cur = Some(nb);
                    prev_sample = Some(s);
                    continue;
                }
                CycleEffect::Unknown => {}
            }
        }

        // Rule 1: the scheduled end. Holds whatever the percentage did.
        let due = b.cycle.ends_at.is_some_and(|end| rolled_over(end, s, tol, tol));
        // Rules 2 and 3: a change of plan, limit or window between readings.
        let plan_changed = plan_change_between(plans, p.at, s.at).map(|c| c.at);
        let limit_changed = differs(p.limit, s.limit, 1e-9);
        let window_changed = differs(p.window_secs, s.window_secs, 1.0);

        if due {
            let end = b.cycle.ends_at.expect("due needs an end");
            let window = b.cycle.window_secs.or(s.window_secs);
            // The model the pool showed most recently decides what a gap
            // is (§5.8): the cycle closing now, as its boundary showed it.
            let model_before = b.cycle.model;
            let plan = b.cycle.plan.clone();
            out.push(cur.take().expect("open").finish(Some((end, EndReason::Scheduled))));
            cur = Some(after_end(
                end,
                window,
                model_before,
                plan,
                s,
                plans,
                &mut ordinal,
                &mut out,
                &mut observed_model,
            ));
        } else if let Some(at) = plan_changed {
            out.push(cur.take().expect("open").finish(Some((at, EndReason::PlanChanged))));
            cur = Some(reopen(&mut ordinal, &mut observed_model, at, s, plans));
        } else if limit_changed {
            out.push(cur.take().expect("open").finish(Some((s.at, EndReason::LimitChanged))));
            cur = Some(reopen(&mut ordinal, &mut observed_model, s.at, s, plans));
        } else if window_changed {
            let start = match (s.reset_at, s.window_secs) {
                (Some(r), Some(w)) if w > 0.0 && r - w >= p.at => r - w,
                _ => s.at,
            };
            out.push(
                cur.take()
                    .expect("open")
                    .finish(Some((start, EndReason::WindowChanged))),
            );
            let mut nb = Building::new(Cycle::open(
                ordinal,
                start,
                s.reset_at,
                s.window_secs,
                CycleModel::Contiguous,
                StartSource::Server,
            ));
            ordinal += 1;
            observed_model.push(false);
            nb.observe(s, plans);
            cur = Some(nb);
        } else {
            b.observe(s, plans);
        }
        prev_sample = Some(s);
    }
    if let Some(b) = cur {
        out.push(b.finish_open(input.gone_at));
    }
    // A cycle whose model no boundary showed - the first one ever, or the
    // first after a plan, limit or window change - takes the model of the
    // next boundary that did. With none, it stays as it was opened.
    for i in 0..out.len() {
        if !observed_model[i] {
            if let Some(j) = (i + 1..out.len()).find(|&j| observed_model[j] && !out[j].is_hollow()) {
                out[i].model = out[j].model;
            }
        }
    }
    FoldOutput {
        shape: PoolShape::Dated,
        cycles: out,
    }
}

/// The cycle `s` opens after a cycle that ended on schedule at `end`, with
/// the hollow cycles a contiguous pool passed through unseen before it.
/// One rule for a full fold and for folding on from a kept cycle, so the
/// cycles a store keeps never change what comes after them.
#[allow(clippy::too_many_arguments)]
fn after_end(
    end: f64,
    window: Option<f64>,
    model_before: CycleModel,
    plan: Option<String>,
    s: &Sample,
    plans: &[PlanChange],
    ordinal: &mut u32,
    out: &mut Vec<Cycle>,
    observed_model: &mut Vec<bool>,
) -> Building {
    // Where the new cycle starts, by the reading's own instant.
    let next_start = match (s.reset_at, s.window_secs) {
        (Some(r), Some(w)) if w > 0.0 && r > end => Some(r - w),
        _ => None,
    };
    let (start, model, source) = match (next_start, window) {
        (Some(ns), Some(w)) if w > 0.0 => {
            let tol = tolerance(Some(w));
            let n = ((ns - end) / w).round();
            let aligned = n >= 0.0 && (ns - (end + n * w)).abs() <= tol;
            // Aligned within the tolerance is one boundary: the new cycle
            // starts where the one before it (or the last hollow one) ended,
            // not a minute either side of it, or two adjacent cycles would
            // overlap - or leave a sliver - by the server's rounding.
            if aligned && n >= 1.0 && model_before == CycleModel::Contiguous {
                // Whole windows passed unseen on a contiguous pool.
                for k in 0..n as u32 {
                    out.push(hollow(*ordinal, end + k as f64 * w, w, plan.clone()));
                    observed_model.push(true);
                    *ordinal += 1;
                }
                (end + n * w, CycleModel::Contiguous, StartSource::Server)
            } else if aligned && n == 0.0 {
                (end, CycleModel::Contiguous, StartSource::Server)
            } else if ns > end + tol {
                (ns, CycleModel::UseAnchored, StartSource::Server)
            } else {
                (ns, CycleModel::Contiguous, StartSource::Server)
            }
        }
        (Some(ns), None) => (ns, CycleModel::Contiguous, StartSource::Server),
        // No instant on the reading that crossed the boundary: a
        // contiguous pool's next window is the one containing it.
        (None, Some(w)) if w > 0.0 && model_before == CycleModel::Contiguous => {
            let k = ((s.at - end) / w).floor().max(0.0);
            for i in 0..k as u32 {
                out.push(hollow(*ordinal, end + i as f64 * w, w, plan.clone()));
                observed_model.push(true);
                *ordinal += 1;
            }
            (end + k * w, CycleModel::Contiguous, StartSource::Derived)
        }
        _ => (s.at.max(end), model_before, StartSource::Inferred),
    };
    let ends = s.reset_at.filter(|r| *r > start).or_else(|| window.map(|w| start + w));
    let mut nb = Building::new(Cycle::open(*ordinal, start, ends, s.window_secs.or(window), model, source));
    *ordinal += 1;
    observed_model.push(true);
    nb.observe(s, plans);
    nb
}

/// A cycle opened at `at` by a change that was not a rollover.
fn reopen(ordinal: &mut u32, observed: &mut Vec<bool>, at: f64, s: &Sample, plans: &[PlanChange]) -> Building {
    let mut nb = Building::new(Cycle::open(
        *ordinal,
        at,
        s.reset_at,
        s.window_secs,
        CycleModel::Contiguous,
        StartSource::Inferred,
    ));
    *ordinal += 1;
    observed.push(false);
    nb.observe(s, plans);
    nb
}

/// An undated pool: a new cycle starts when the figure falls back to about
/// zero from above it (the spend dropping back at the month's roll, §6.2).
/// Never projected forward: no countdown nobody sent.
fn fold_undated(input: &FoldInput) -> FoldOutput {
    let near = input.thresholds.near_zero_pct;
    let mut out = Vec::new();
    let mut ordinal = input.next_ordinal;
    // Folding on from the open cycle itself, where the store has one.
    let mut cur: Option<Building> = input.resume.map(Building::resumed);
    // The last figure that can mark a boundary: a cached one never does
    // (§4), so a cached zero between two live readings is no fall.
    let mut last_pct: Option<f64> = match input.resume {
        Some(r) => r.last_pct,
        None => input.prev.and_then(|p| p.peak_pct),
    };
    // The same for a figure stated only as an amount - spend with no cap,
    // so no percentage - which rolls when it falls back to nothing.
    let mut last_used: Option<f64> = match input.resume {
        Some(r) => r.last_used,
        None => input.prev.and_then(|p| p.used_amount),
    };
    let mut prev_at: Option<f64> = input.resume.and_then(|r| r.cycle.last_obs_at);
    for s in input.samples {
        // A reset established at this sample (phase 2): only a bank fall or
        // the configured effect makes one on a pool with no instant.
        let ev = input.resets.iter().find(|e| e.at_hi == s.at).filter(|_| cur.is_some());
        if let (Some(ev), Some(b)) = (ev, cur.as_mut()) {
            if !ev.before_known {
                b.cycle.consumed_is_lower_bound = true;
            }
            if ev.effect == CycleEffect::KeptBoundary {
                b.split(s.at);
            }
        }
        let restarted = ev.and_then(|e| match e.effect {
            CycleEffect::Restarted { new_start } => Some(new_start),
            _ => None,
        });
        // A refill that kept the boundary already accounts for the fall it
        // came with: a new segment, not also a new cycle.
        let refilled = ev.is_some_and(|e| e.effect == CycleEffect::KeptBoundary);
        let fell = s.source != Source::Cached
            && !refilled
            && match (last_pct, s.pct) {
                (Some(a), Some(b)) => a > near && b <= near,
                _ => matches!((last_used, s.used, s.pct), (Some(a), Some(b), None) if a > 1e-9 && b <= 1e-9),
            };
        let limit_changed =
            matches!((cur.as_ref(), prev_at), (Some(b), Some(_)) if differs(b.cycle.limit_amount, s.limit, 1e-9));
        // A change of plan between readings closes the cycle where it was
        // seen (§5.4 rule 2), dated or not.
        let plan_changed = match (cur.as_ref(), prev_at) {
            (Some(_), Some(p)) => plan_change_between(input.plans, p, s.at).map(|c| c.at),
            _ => None,
        };
        if cur.is_none() || fell || limit_changed || plan_changed.is_some() || restarted.is_some() {
            let start = restarted.or(plan_changed.filter(|_| !limit_changed)).unwrap_or(s.at);
            if let Some(b) = cur.take() {
                let why = if limit_changed {
                    EndReason::LimitChanged
                } else if restarted.is_some() {
                    EndReason::Reset
                } else if plan_changed.is_some() {
                    EndReason::PlanChanged
                } else {
                    EndReason::Scheduled
                };
                out.push(b.finish(Some((start, why))));
            }
            let mut b = Building::new(Cycle::open(
                ordinal,
                start,
                None,
                None,
                CycleModel::Undated,
                if restarted.is_some() {
                    StartSource::Reset
                } else {
                    StartSource::Inferred
                },
            ));
            ordinal += 1;
            b.observe(s, input.plans);
            cur = Some(b);
        } else if let Some(b) = cur.as_mut() {
            b.observe(s, input.plans);
        }
        if s.pct.is_some() && s.source != Source::Cached {
            last_pct = s.pct;
        }
        if s.used.is_some() && s.source != Source::Cached {
            last_used = s.used;
        }
        prev_at = Some(s.at);
    }
    if let Some(b) = cur {
        out.push(b.finish_open(input.gone_at));
    }
    // An inferred close is when the fall was seen, not when it happened: the
    // figure before it is the last one known, so it is a lower bound only if
    // the reading before the fall was long before it.
    FoldOutput {
        shape: PoolShape::Undated,
        cycles: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: f64 = 3600.0;
    const W5: f64 = 5.0 * H;
    const WEEK: f64 = 7.0 * 24.0 * H;

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

    fn run(samples: &[Sample]) -> FoldOutput {
        run_with(samples, &[], &[], Role::Included)
    }

    fn run_with(samples: &[Sample], plans: &[PlanChange], resets: &[ResetEvent], role: Role) -> FoldOutput {
        fold(&FoldInput {
            role,
            samples,
            plans,
            resets,
            prev: None,
            next_ordinal: 1,
            rolling_seed: None,
            resume: None,
            gone_at: None,
            thresholds: Thresholds::default(),
        })
    }

    /// A weekly pool read every hour from `from` to `to`, figure climbing.
    fn weekly(start: f64, from: f64, to: f64, step: f64, base: f64) -> Vec<Sample> {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            let pct = (base + (t - start) / WEEK * 100.0).min(100.0);
            out.push(s(t, pct, Some(start + WEEK), Some(WEEK)));
            t += step;
        }
        out
    }

    #[test]
    fn one_cycle_starts_at_reset_less_window_and_stays_open() {
        let start = 1_000_000.0;
        let out = run(&weekly(start, start + H, start + 3.0 * H, H, 0.0));
        assert_eq!(out.shape, PoolShape::Dated);
        assert_eq!(out.cycles.len(), 1);
        let c = &out.cycles[0];
        assert_eq!(c.starts_at, start);
        assert_eq!(c.ends_at, Some(start + WEEK));
        assert_eq!(c.closed_at, None);
        assert_eq!(c.start_source, StartSource::Server);
        assert_eq!(c.ordinal, 1);
        assert_eq!(c.first_obs_at, Some(start + H));
        assert_eq!(c.last_obs_at, Some(start + 3.0 * H));
    }

    #[test]
    fn rollover_at_zero_and_at_full_is_scheduled_whatever_the_figure() {
        for pct in [0.0, 100.0] {
            let a = 1_000_000.0;
            let samples = vec![
                s(a + H, pct, Some(a + WEEK), Some(WEEK)),
                s(a + WEEK - 2.0 * H, pct, Some(a + WEEK), Some(WEEK)),
                s(a + WEEK + H, 0.0, Some(a + 2.0 * WEEK), Some(WEEK)),
            ];
            let out = run(&samples);
            assert_eq!(out.cycles.len(), 2, "pct {pct}");
            let c = &out.cycles[0];
            assert_eq!(c.closed_at, Some(a + WEEK));
            assert_eq!(c.end_reason, Some(EndReason::Scheduled));
            assert_eq!(out.cycles[1].starts_at, a + WEEK);
            assert_eq!(out.cycles[1].model, CycleModel::Contiguous);
            if pct == 100.0 {
                assert_eq!(c.exhausted_at, Some(a + H));
            } else {
                assert_eq!(c.exhausted_at, None);
            }
        }
    }

    #[test]
    fn a_reading_inside_the_tolerance_that_states_the_same_end_is_not_a_rollover() {
        // Weekly tolerance is about 1.7 hours. The reading an hour before
        // the end still states that end: it belongs to this week.
        let a = 1_000_000.0;
        let e = a + WEEK;
        let samples = vec![
            s(a + H, 10.0, Some(e), Some(WEEK)),
            s(e - 2.0 * H, 50.0, Some(e), Some(WEEK)),
            s(e - H, 95.0, Some(e), Some(WEEK)),
            s(e + H, 1.0, Some(e + WEEK), Some(WEEK)),
        ];
        let out = run(&samples);
        assert_eq!(out.cycles.len(), 2, "{:#?}", out.cycles);
        let c = &out.cycles[0];
        assert_eq!(c.peak_pct, Some(95.0));
        assert_eq!(c.closed_at, Some(e));
        assert!(!c.end_is_lower_bound);
        assert_eq!(out.cycles[1].starts_at, e);
        assert_eq!(out.cycles[1].peak_pct, Some(1.0));
    }

    #[test]
    fn adjacent_cycles_share_one_boundary_when_the_next_start_is_a_minute_off() {
        // CodeRabbit's case: a five-hour cycle closes at E and the next
        // reading implies a start of E - 60 s. Within the tolerance that is
        // the same boundary, and the stored cycles overlapped by a minute.
        let a = 1_000_000.0;
        let w = 5.0 * H;
        let e = a + w;
        for off in [-60.0, 60.0] {
            let samples = vec![s(a + H, 40.0, Some(e), Some(w)), s(e + H, 5.0, Some(e + w + off), Some(w))];
            let out = run(&samples);
            assert_eq!(out.cycles.len(), 2, "{:#?}", out.cycles);
            assert_eq!(out.cycles[0].closed_at, Some(e));
            assert_eq!(out.cycles[1].starts_at, e, "offset {off}");
            assert_eq!(out.cycles[1].ends_at, Some(e + w + off), "the stated end is kept");
        }
        // Whole windows unseen in between: the new cycle starts where the
        // last hollow one ended.
        let samples = vec![s(a + H, 40.0, Some(e), Some(w)), s(e + 2.0 * w + H, 5.0, Some(e + 3.0 * w - 60.0), Some(w))];
        let out = run(&samples);
        let last = out.cycles.last().unwrap();
        let hollow = &out.cycles[out.cycles.len() - 2];
        assert_eq!(hollow.ends_at, Some(last.starts_at));
    }

    #[test]
    fn a_contiguous_rollover_and_a_use_anchored_one_are_told_apart() {
        let a = 1_000_000.0;
        // Contiguous: the next window starts at the old end.
        let contiguous = vec![
            s(a + H, 10.0, Some(a + W5), Some(W5)),
            s(a + W5 + H, 5.0, Some(a + 2.0 * W5), Some(W5)),
        ];
        let out = run(&contiguous);
        assert_eq!(out.cycles[1].model, CycleModel::Contiguous);
        assert_eq!(out.cycles[1].starts_at, a + W5);
        // Use-anchored: two idle hours, then a window starting at first use.
        let anchored = vec![
            s(a + H, 10.0, Some(a + W5), Some(W5)),
            s(a + W5 + 2.0 * H + 60.0, 5.0, Some(a + 2.0 * W5 + 2.0 * H), Some(W5)),
        ];
        let out = run(&anchored);
        assert_eq!(out.cycles.len(), 2);
        assert_eq!(out.cycles[1].model, CycleModel::UseAnchored);
        assert_eq!(out.cycles[1].starts_at, a + W5 + 2.0 * H);
        // The first cycle had no boundary of its own; it takes the pool's.
        assert_eq!(out.cycles[0].model, CycleModel::UseAnchored);
    }

    #[test]
    fn an_idle_use_anchored_instant_that_slides_before_its_end_is_not_a_cycle() {
        // A 5h window idle: its instant slides later while unused. Phase 1
        // moves the scheduled end and opens nothing; a reset is phase 2's.
        let a = 1_000_000.0;
        let samples = vec![
            s(a, 0.0, Some(a + W5), Some(W5)),
            s(a + H, 0.0, Some(a + W5 + 0.5 * H), Some(W5)),
            s(a + 2.0 * H, 0.0, Some(a + W5 + 0.7 * H), Some(W5)),
        ];
        let out = run(&samples);
        assert_eq!(out.cycles.len(), 1);
        assert_eq!(out.cycles[0].ends_at, Some(a + W5 + 0.7 * H));
    }

    #[test]
    fn three_unseen_windows_on_a_contiguous_pool_are_three_hollow_cycles() {
        let a = 1_000_000.0;
        let samples = vec![
            s(a + H, 10.0, Some(a + W5), Some(W5)),
            // Seen again inside the fifth window.
            s(a + 4.0 * W5 + H, 3.0, Some(a + 5.0 * W5), Some(W5)),
        ];
        let out = run(&samples);
        assert_eq!(out.cycles.len(), 5);
        for c in &out.cycles[1..4] {
            assert!(c.is_hollow());
            assert_eq!(c.coverage, 0.0);
            assert_eq!(c.peak_pct, None);
            assert_eq!(c.start_source, StartSource::Derived);
            assert!(c.end_is_lower_bound);
        }
        let ords: Vec<u32> = out.cycles.iter().map(|c| c.ordinal).collect();
        assert_eq!(ords, vec![1, 2, 3, 4, 5]);
        assert_eq!(out.cycles[4].starts_at, a + 4.0 * W5);
    }

    #[test]
    fn three_unseen_windows_on_a_use_anchored_pool_are_idle_time() {
        let a = 1_000_000.0;
        // A use-anchored boundary first, so the pool's model is known.
        let samples = vec![
            s(a + H, 10.0, Some(a + W5), Some(W5)),
            s(a + W5 + 2.0 * H, 5.0, Some(a + 2.0 * W5 + 2.0 * H - 60.0), Some(W5)),
            // Then a long gap, and a window starting at a time of its own.
            s(a + 6.0 * W5 + 1234.0, 1.0, Some(a + 7.0 * W5 + 1000.0), Some(W5)),
        ];
        let out = run(&samples);
        assert_eq!(out.cycles.len(), 3);
        assert!(out.cycles.iter().all(|c| !c.is_hollow()));
        assert_eq!(out.cycles[2].model, CycleModel::UseAnchored);
    }

    #[test]
    fn a_plan_upgrade_dropping_the_figure_closes_the_cycle_and_is_not_a_reset() {
        let a = 1_000_000.0;
        let samples = vec![
            s(a + H, 80.0, Some(a + WEEK), Some(WEEK)),
            s(a + 2.0 * H, 20.0, Some(a + WEEK), Some(WEEK)),
        ];
        let plans = vec![
            PlanChange {
                at: a,
                plan: "5x".into(),
            },
            PlanChange {
                at: a + 1.5 * H,
                plan: "20x".into(),
            },
        ];
        let out = run_with(&samples, &plans, &[], Role::Included);
        assert_eq!(out.cycles.len(), 2);
        assert_eq!(out.cycles[0].end_reason, Some(EndReason::PlanChanged));
        assert_eq!(out.cycles[0].closed_at, Some(a + 1.5 * H));
        assert_eq!(out.cycles[0].plan.as_deref(), Some("5x"));
        assert_eq!(out.cycles[1].plan.as_deref(), Some("20x"));
        assert_eq!(out.cycles[1].allowances, 1);
    }

    #[test]
    fn a_limit_change_closes_the_cycle() {
        let a = 1_000_000.0;
        let mut x = s(a + H, 50.0, Some(a + WEEK), Some(WEEK));
        x.limit = Some(50.0);
        let mut y = s(a + 2.0 * H, 25.0, Some(a + WEEK), Some(WEEK));
        y.limit = Some(100.0);
        let out = run(&[x, y]);
        assert_eq!(out.cycles.len(), 2);
        assert_eq!(out.cycles[0].end_reason, Some(EndReason::LimitChanged));
        assert_eq!(out.cycles[1].limit_amount, Some(100.0));
    }

    #[test]
    fn a_window_change_closes_the_cycle() {
        let a = 1_000_000.0;
        let samples = vec![
            s(a + H, 40.0, Some(a + WEEK), Some(WEEK)),
            s(a + 2.0 * H, 10.0, Some(a + 2.0 * H + W5 - 600.0), Some(W5)),
        ];
        let out = run(&samples);
        assert_eq!(out.cycles.len(), 2);
        assert_eq!(out.cycles[0].end_reason, Some(EndReason::WindowChanged));
        assert_eq!(out.cycles[1].window_secs, Some(W5));
    }

    #[test]
    fn a_small_adjustment_changes_nothing_about_the_cycle() {
        let a = 1_000_000.0;
        let samples = vec![
            s(a + H, 40.0, Some(a + WEEK), Some(WEEK)),
            s(a + 2.0 * H, 39.0, Some(a + WEEK), Some(WEEK)),
            s(a + 3.0 * H, 41.0, Some(a + WEEK), Some(WEEK)),
        ];
        let out = run(&samples);
        assert_eq!(out.cycles.len(), 1);
        assert_eq!(out.cycles[0].peak_pct, Some(41.0));
    }

    #[test]
    fn a_cached_reading_below_a_live_one_is_not_a_boundary() {
        let a = 1_000_000.0;
        let mut cached = s(a + 2.0 * H, 10.0, Some(a + WEEK), Some(WEEK));
        cached.source = Source::Cached;
        let samples = vec![
            s(a + H, 30.0, Some(a + WEEK), Some(WEEK)),
            cached,
            s(a + 3.0 * H, 32.0, Some(a + WEEK), Some(WEEK)),
        ];
        let out = run(&samples);
        assert_eq!(out.cycles.len(), 1);
        assert_eq!(out.cycles[0].peak_pct, Some(32.0));
    }

    #[test]
    fn coverage_counts_gaps_over_two_hours_as_unobserved() {
        let a = 1_000_000.0;
        let w = 10.0 * H;
        // Seen every hour for the first four hours, then not until hour 9,
        // then the rollover at hour 10 closes it.
        let mut samples: Vec<Sample> = (0..=4)
            .map(|h| s(a + h as f64 * H, 10.0, Some(a + w), Some(w)))
            .collect();
        samples.push(s(a + 9.0 * H, 20.0, Some(a + w), Some(w)));
        samples.push(s(a + w + 60.0, 0.0, Some(a + 2.0 * w), Some(w)));
        let out = run(&samples);
        let c = &out.cycles[0];
        assert!((c.coverage - 0.4).abs() < 1e-9, "coverage {}", c.coverage);
        // Last seen an hour before the end: not more than an hour, so exact.
        assert!(!c.end_is_lower_bound);
    }

    #[test]
    fn a_cycle_last_seen_two_days_before_its_end_is_a_lower_bound() {
        let a = 1_000_000.0;
        let samples = vec![
            s(a + H, 40.0, Some(a + WEEK), Some(WEEK)),
            s(a + WEEK - 48.0 * H, 60.0, Some(a + WEEK), Some(WEEK)),
            s(a + WEEK + H, 2.0, Some(a + 2.0 * WEEK), Some(WEEK)),
        ];
        let out = run(&samples);
        assert!(out.cycles[0].end_is_lower_bound);
        assert_eq!(out.cycles[0].peak_pct, Some(60.0));
    }

    #[test]
    fn a_rolling_pool_keeps_no_cycles() {
        let a = 1_000_000.0;
        let w = 6.0 * H;
        let samples: Vec<Sample> = (0..4)
            .map(|i| {
                let at = a + i as f64 * 0.5 * H;
                s(at, 20.0, Some(at + w), Some(w))
            })
            .collect();
        let out = run(&samples);
        assert_eq!(out.shape, PoolShape::Rolling);
        assert!(out.cycles.is_empty());
    }

    /// Three real 5h windows read hourly, then the pool idle past its end,
    /// read every five minutes: an unstarted window states "now plus five
    /// hours" at 0% on every reading.
    fn three_windows_then_idle(a: f64) -> Vec<Sample> {
        let mut out = Vec::new();
        for k in 0..3 {
            let start = a + k as f64 * W5;
            for h in 1..5 {
                out.push(s(start + h as f64 * H, 10.0 * h as f64, Some(start + W5), Some(W5)));
            }
        }
        let idle = a + 3.0 * W5 + 600.0;
        for i in 0..4 {
            let at = idle + i as f64 * 300.0;
            out.push(s(at, 0.0, Some(at + W5), Some(W5)));
        }
        out
    }

    #[test]
    fn an_idle_stretch_of_unstarted_windows_does_not_make_the_pool_rolling() {
        let a = 1_000_000.0;
        let out = run(&three_windows_then_idle(a));
        assert_eq!(out.shape, PoolShape::Dated);
        let closed: Vec<&Cycle> = out.cycles.iter().filter(|c| c.closed_at.is_some()).collect();
        assert_eq!(closed.len(), 3, "{:#?}", out.cycles);
        assert_eq!(closed[2].peak_pct, Some(40.0));
    }

    #[test]
    fn a_rolling_stretch_keeps_the_cycles_either_side_of_it() {
        let a = 1_000_000.0;
        let w = 6.0 * H;
        let mut samples = vec![s(a + H, 30.0, Some(a + w), Some(w)), s(a + 3.0 * H, 50.0, Some(a + w), Some(w))];
        // Then, for a while, a window that slides with the clock in use.
        for i in 0..4 {
            let at = a + w + H + i as f64 * 0.5 * H;
            samples.push(s(at, 20.0, Some(at + w), Some(w)));
        }
        let out = run(&samples);
        assert_eq!(out.shape, PoolShape::Dated);
        assert_eq!(out.cycles.len(), 1, "{:#?}", out.cycles);
        assert_eq!(out.cycles[0].peak_pct, Some(50.0));
    }

    #[test]
    fn a_balance_keeps_no_cycles() {
        let a = 1_000_000.0;
        let out = run_with(
            &[s(a, 20.0, None, None), s(a + H, 30.0, None, None)],
            &[],
            &[],
            Role::Balance,
        );
        assert_eq!(out.shape, PoolShape::Balance);
        assert!(out.cycles.is_empty());
    }

    #[test]
    fn an_undated_pool_infers_its_cycles_from_the_spend_returning_to_zero() {
        let a = 1_000_000.0;
        let samples = vec![
            s(a, 10.0, None, None),
            s(a + H, 40.0, None, None),
            s(a + 2.0 * H, 0.0, None, None),
            s(a + 3.0 * H, 5.0, None, None),
        ];
        let out = run_with(&samples, &[], &[], Role::Overage);
        assert_eq!(out.shape, PoolShape::Undated);
        assert_eq!(out.cycles.len(), 2);
        assert_eq!(out.cycles[0].model, CycleModel::Undated);
        assert_eq!(out.cycles[0].start_source, StartSource::Inferred);
        assert_eq!(out.cycles[0].peak_pct, Some(40.0));
        assert_eq!(out.cycles[1].starts_at, a + 2.0 * H);
        assert_eq!(out.cycles[1].ends_at, None);
    }

    #[test]
    fn a_kept_boundary_reset_adds_a_segment_and_an_allowance() {
        let a = 1_000_000.0;
        let samples = vec![
            s(a + H, 100.0, Some(a + WEEK), Some(WEEK)),
            s(a + 2.0 * H, 0.0, Some(a + WEEK), Some(WEEK)),
            s(a + 3.0 * H, 40.0, Some(a + WEEK), Some(WEEK)),
        ];
        let reset = ResetEvent {
            at_lo: a + H,
            at_hi: a + 2.0 * H,
            kind: ResetKind::Banked,
            effect: CycleEffect::KeptBoundary,
            signals: "pct_fall".into(),
            pct_before: Some(100.0),
            pct_after: Some(0.0),
            reset_at_before: Some(a + WEEK),
            reset_at_after: Some(a + WEEK),
            group_id: None,
            evidence: "{}".into(),
            before_known: true,
        };
        let out = run_with(&samples, &[], &[reset], Role::Included);
        let c = &out.cycles[0];
        assert_eq!(out.cycles.len(), 1);
        assert_eq!(c.allowances, 2);
        assert_eq!(c.segments.len(), 2);
        assert_eq!(c.peak_pct, Some(100.0));
        assert_eq!(c.consumed_pct, Some(140.0));
        assert_eq!(c.available_pct, Some(200.0));
    }

    #[test]
    fn a_restarted_reset_closes_at_the_new_start_and_opens_there() {
        let a = 1_000_000.0;
        let new_start = a + 2.0 * H - 300.0;
        let samples = vec![
            s(a + H, 90.0, Some(a + WEEK), Some(WEEK)),
            s(a + 2.0 * H, 0.0, Some(new_start + WEEK), Some(WEEK)),
        ];
        let reset = ResetEvent {
            at_lo: a + H,
            at_hi: a + 2.0 * H,
            kind: ResetKind::Unknown,
            effect: CycleEffect::Restarted { new_start },
            signals: "pct_fall,instant_jump".into(),
            pct_before: Some(90.0),
            pct_after: Some(0.0),
            reset_at_before: Some(a + WEEK),
            reset_at_after: Some(new_start + WEEK),
            group_id: None,
            evidence: "{}".into(),
            before_known: true,
        };
        let out = run_with(&samples, &[], &[reset], Role::Included);
        assert_eq!(out.cycles.len(), 2);
        assert_eq!(out.cycles[0].closed_at, Some(new_start));
        assert_eq!(out.cycles[0].end_reason, Some(EndReason::Reset));
        assert_eq!(out.cycles[1].starts_at, new_start);
        assert_eq!(out.cycles[1].start_source, StartSource::Reset);
    }

    #[test]
    fn folding_in_two_halves_from_a_kept_cycle_matches_folding_at_once() {
        // The store re-folds from the cycle before a point; that has to land
        // where one fold over everything lands.
        let a = 1_000_000.0;
        let mut samples = weekly(a, a + H, a + WEEK - H, 6.0 * H, 0.0);
        samples.extend(weekly(a + WEEK, a + WEEK + H, a + 2.0 * WEEK - H, 6.0 * H, 0.0));
        samples.extend(weekly(
            a + 2.0 * WEEK,
            a + 2.0 * WEEK + H,
            a + 2.0 * WEEK + 30.0 * H,
            6.0 * H,
            0.0,
        ));
        let whole = run(&samples);
        assert_eq!(whole.cycles.len(), 3);
        let kept = whole.cycles[0].clone();
        let rest: Vec<Sample> = samples
            .iter()
            .filter(|x| x.at > kept.last_obs_at.unwrap())
            .cloned()
            .collect();
        let tail = fold(&FoldInput {
            role: Role::Included,
            samples: &rest,
            plans: &[],
            resets: &[],
            prev: Some(&kept),
            next_ordinal: 2,
            rolling_seed: None,
            resume: None,
            gone_at: None,
            thresholds: Thresholds::default(),
        });
        assert_eq!(tail.cycles, whole.cycles[1..].to_vec());
    }

    /// Fold `samples` once, then again from each closed, observed cycle
    /// the store could keep, and check the tail lands where the whole did.
    fn incremental_matches_full(samples: &[Sample], plans: &[PlanChange]) {
        let whole = run_with(samples, plans, &[], Role::Included);
        for (j, kept) in whole.cycles.iter().enumerate() {
            let (Some(_), Some(last)) = (kept.closed_at, kept.last_obs_at) else {
                continue;
            };
            let rest: Vec<Sample> = samples.iter().filter(|x| x.at > last).cloned().collect();
            let tail = fold(&FoldInput {
                role: Role::Included,
                samples: &rest,
                plans,
                resets: &[],
                prev: Some(kept),
                next_ordinal: kept.ordinal + 1,
                rolling_seed: None,
                resume: None,
                gone_at: None,
                thresholds: Thresholds::default(),
            });
            assert_eq!(
                tail.cycles,
                whole.cycles[j + 1..].to_vec(),
                "kept cycle {}\nsamples {samples:#?}\nplans {plans:?}",
                kept.ordinal
            );
        }
    }

    #[test]
    fn folding_on_from_a_cycle_a_plan_change_closed_matches_folding_at_once() {
        let a = 1_000_000.0;
        let day = 24.0 * H;
        let samples: Vec<Sample> = (1..=6)
            .map(|d| s(a + d as f64 * day, 10.0 * d as f64, Some(a + WEEK), Some(WEEK)))
            .collect();
        let plans = vec![
            PlanChange { at: a, plan: "pro".into() },
            PlanChange {
                at: a + 2.5 * day,
                plan: "max".into(),
            },
        ];
        let whole = run_with(&samples, &plans, &[], Role::Included);
        assert_eq!(whole.cycles[1].starts_at, a + 2.5 * day);
        assert_eq!(whole.cycles[1].start_source, StartSource::Inferred);
        incremental_matches_full(&samples, &plans);
    }

    #[test]
    fn folding_on_from_any_kept_cycle_matches_folding_at_once() {
        // Randomised sequences of 5h windows: contiguous and use-anchored
        // rollovers, gaps, plan and limit changes.
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..300 {
            let a = 1_000_000.0;
            let mut samples = Vec::new();
            let mut plans = vec![PlanChange { at: a - H, plan: "p0".into() }];
            let mut start = a;
            let mut at = a + 60.0;
            let mut limit = 100.0;
            for _ in 0..(3 + rnd(5)) {
                let end = start + W5;
                let mut pct = 0.0;
                while at < end {
                    pct += rnd(20) as f64;
                    // Now and then a reading that states no instant.
                    let reset = if rnd(8) == 0 { None } else { Some(end) };
                    let mut x = s(at, pct.min(100.0), reset, Some(W5));
                    x.limit = Some(limit);
                    samples.push(x);
                    at += (10 + rnd(110)) as f64 * 60.0;
                    match rnd(30) {
                        0 => plans.push(PlanChange {
                            at: at - 30.0,
                            plan: format!("p{}", plans.len()),
                        }),
                        1 => limit += 50.0,
                        _ => {}
                    }
                }
                // Contiguous, use-anchored after idle, or a whole window unseen.
                start = match rnd(3) {
                    0 => end,
                    1 => at + (rnd(60) as f64) * 60.0,
                    _ => end + W5,
                };
                at = at.max(start + 60.0);
            }
            incremental_matches_full(&samples, &plans);
        }
    }

    #[test]
    fn folding_on_from_a_kept_cycle_across_a_gap_writes_the_same_hollow_cycles() {
        let a = 1_000_000.0;
        let samples = vec![
            s(a + H, 10.0, Some(a + W5), Some(W5)),
            s(a + W5 + H, 20.0, Some(a + 2.0 * W5), Some(W5)),
            // Two windows unseen, then seen again.
            s(a + 4.0 * W5 + H, 5.0, Some(a + 5.0 * W5), Some(W5)),
        ];
        let whole = run(&samples);
        assert_eq!(whole.cycles.len(), 5);
        let kept = whole.cycles[1].clone();
        let rest: Vec<Sample> = samples
            .iter()
            .filter(|x| x.at > kept.last_obs_at.unwrap())
            .cloned()
            .collect();
        let tail = fold(&FoldInput {
            role: Role::Included,
            samples: &rest,
            plans: &[],
            resets: &[],
            prev: Some(&kept),
            next_ordinal: 3,
            rolling_seed: None,
            resume: None,
            gone_at: None,
            thresholds: Thresholds::default(),
        });
        assert_eq!(tail.cycles, whole.cycles[2..].to_vec());
    }

    fn kept_refill(at_lo: f64, at_hi: f64) -> ResetEvent {
        ResetEvent {
            at_lo,
            at_hi,
            kind: ResetKind::Banked,
            effect: CycleEffect::KeptBoundary,
            signals: "bank_fall".into(),
            pct_before: None,
            pct_after: None,
            reset_at_before: None,
            reset_at_after: None,
            group_id: None,
            evidence: "{}".into(),
            before_known: false,
        }
    }

    #[test]
    fn a_kept_boundary_refill_on_an_undated_pool_is_a_segment_not_also_a_new_cycle() {
        // CodeRabbit's case: a banked refill takes an undated pool from 60%
        // to 0% and keeps its boundary. The fall is the refill's, so it adds
        // a segment and does not also close the cycle.
        let a = 1_000_000.0;
        let samples = vec![s(a, 30.0, None, None), s(a + H, 60.0, None, None), s(a + 2.0 * H, 0.0, None, None), {
            s(a + 3.0 * H, 10.0, None, None)
        }];
        let out = run_with(&samples, &[], &[kept_refill(a + H, a + 2.0 * H)], Role::Included);
        assert_eq!(out.cycles.len(), 1, "{:#?}", out.cycles);
        assert_eq!(out.cycles[0].allowances, 2);
        assert_eq!(out.cycles[0].consumed_pct, Some(70.0));
        // The same fall with no event is the undated pool's own boundary.
        assert_eq!(run(&samples).cycles.len(), 2);
    }

    #[test]
    fn spend_with_no_percentage_rolls_when_the_amount_falls_back_to_zero() {
        // Claude's extra usage with no cap states money and no percentage:
        // its month rolls when the spend falls back to nothing.
        let a = 1_000_000.0;
        let money = |at: f64, used: f64| Sample {
            pct: None,
            used: Some(used),
            ..s(at, 0.0, None, None)
        };
        let samples = vec![money(a, 10.0), money(a + H, 14.0), money(a + 2.0 * H, 0.0), money(a + 3.0 * H, 2.0)];
        let out = run_with(&samples, &[], &[], Role::Overage);
        assert_eq!(out.cycles.len(), 2, "{:#?}", out.cycles);
        assert_eq!(out.cycles[0].used_amount, Some(14.0));
        assert_eq!(out.cycles[1].starts_at, a + 2.0 * H);
    }

    #[test]
    fn a_cached_zero_never_marks_an_undated_boundary() {
        // CodeRabbit's case: live 40%, cached 0%, live 45%. A cached figure
        // can extend a cycle but never establish a reset (§4).
        let a = 1_000_000.0;
        let mut cached = s(a + H, 0.0, None, None);
        cached.source = Source::Cached;
        let samples = vec![s(a, 40.0, None, None), cached, s(a + 2.0 * H, 45.0, None, None)];
        let out = run(&samples);
        assert_eq!(out.cycles.len(), 1, "{:#?}", out.cycles);
        // A live fall to zero still does.
        let live = vec![s(a, 40.0, None, None), s(a + H, 0.0, None, None), s(a + 2.0 * H, 45.0, None, None)];
        assert_eq!(run(&live).cycles.len(), 2);
    }

    #[test]
    fn an_undated_cycle_is_split_at_a_plan_change() {
        // CodeRabbit's case: 80% on Pro, then 20% on Max. Neither near zero
        // nor a limit change, but a plan change closes the cycle (§5.4).
        let a = 1_000_000.0;
        let samples = vec![s(a, 80.0, None, None), s(a + 2.0 * H, 20.0, None, None)];
        let plans = vec![
            PlanChange { at: a - H, plan: "pro".into() },
            PlanChange { at: a + H, plan: "max".into() },
        ];
        let out = run_with(&samples, &plans, &[], Role::Included);
        assert_eq!(out.cycles.len(), 2, "{:#?}", out.cycles);
        assert_eq!(out.cycles[0].end_reason, Some(EndReason::PlanChanged));
        assert_eq!(out.cycles[0].closed_at, Some(a + H));
        assert_eq!(out.cycles[0].plan.as_deref(), Some("pro"));
        assert_eq!(out.cycles[1].starts_at, a + H);
        assert_eq!(out.cycles[1].peak_pct, Some(20.0));
        assert_eq!(out.cycles[1].plan.as_deref(), Some("max"));
    }

    #[test]
    fn undated_readings_before_a_rolling_stretch_are_still_folded() {
        // CodeRabbit's case: two readings with no instant, then a stretch
        // that slides with the clock. The rolling stretch keeps no cycles;
        // the two readings before it are not thrown away with it.
        let a = 1_000_000.0;
        let w = 6.0 * H;
        let mut samples = vec![s(a, 10.0, None, None), s(a + H, 12.0, None, None)];
        for i in 0..4 {
            let at = a + 2.0 * H + i as f64 * 0.5 * H;
            samples.push(s(at, 20.0, Some(at + w), Some(w)));
        }
        let out = run(&samples);
        assert_eq!(out.cycles.len(), 1, "{:#?}", out.cycles);
        assert_eq!(out.cycles[0].first_obs_at, Some(a));
        assert_eq!(out.cycles[0].peak_pct, Some(12.0));
        // Nothing but the rolling stretch: no cycles, a rolling pool.
        let only: Vec<Sample> = samples[2..].to_vec();
        let out = run(&only);
        assert_eq!(out.shape, PoolShape::Rolling);
        assert!(out.cycles.is_empty());
    }

    #[test]
    fn a_reading_with_no_instant_after_a_kept_cycle_opens_where_a_full_fold_does() {
        // CodeRabbit's case: a five-hour cycle ends at E, and the next
        // reading, an hour later, states no instant. A full fold opens the
        // next cycle at E and derives its end; folding on from the kept
        // cycle opened it at E + 1h with no end.
        let a = 1_000_000.0;
        let e = a + W5;
        let samples = vec![
            s(a + H, 10.0, Some(e), Some(W5)),
            s(a + 2.0 * H, 20.0, Some(e), Some(W5)),
            s(e + H, 5.0, None, Some(W5)),
            s(e + 2.0 * H, 8.0, None, Some(W5)),
        ];
        let whole = run(&samples);
        assert_eq!(whole.cycles[1].starts_at, e);
        assert_eq!(whole.cycles[1].ends_at, Some(e + W5));
        incremental_matches_full(&samples, &[]);
    }

    #[test]
    fn an_expired_instant_is_not_carried_into_the_cycle_after_it() {
        // CodeRabbit's case: two readings past the tolerance still state the
        // old end E. The first closes the cycle at E; the replacement must
        // not take E back as its own end, or the second closes a cycle from
        // E to E.
        let a = 1_000_000.0;
        let e = a + W5;
        let tol = tolerance(Some(W5));
        let samples = vec![
            s(a + H, 50.0, Some(e), Some(W5)),
            s(e + tol + 60.0, 0.0, Some(e), Some(W5)),
            s(e + tol + 30.0 * 60.0, 2.0, Some(e), Some(W5)),
        ];
        let out = run(&samples);
        assert_eq!(out.cycles.len(), 2, "{:#?}", out.cycles);
        assert_eq!(out.cycles[1].starts_at, e);
        assert_eq!(out.cycles[1].ends_at, Some(e + W5));
        assert!(out.cycles.iter().all(|c| c.closed_at.is_none_or(|x| x > c.starts_at)));
        // A later instant that does arrive is still taken.
        let mut later = samples.clone();
        later.push(s(e + 2.0 * H, 3.0, Some(e + W5 + 60.0), Some(W5)));
        assert_eq!(run(&later).cycles[1].ends_at, Some(e + W5 + 60.0));
    }

    #[test]
    fn samples_out_of_order_fold_the_same_once_sorted() {
        let a = 1_000_000.0;
        let samples = weekly(a, a + H, a + 2.0 * WEEK, 5.0 * H, 0.0);
        let mut shuffled = samples.clone();
        shuffled.reverse();
        shuffled.sort_by(|x, y| x.at.total_cmp(&y.at));
        assert_eq!(run(&samples), run(&shuffled));
    }
}
