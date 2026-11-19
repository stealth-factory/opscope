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

//! Reset detection and bank events. **Phase 2 (OPS-305) fills this file.**
//!
//! Both functions are already called, with real inputs, by the store every
//! time a pool or a bank is re-folded, and what they return is already
//! written: [`detect`]'s events go to `reset_event` and into
//! [`super::fold::fold`] (which applies them - see its module comment), and
//! [`bank_events`]' go to `bank_event`. Phase 1 returns nothing from either,
//! so no reset is ever claimed. Phase 2 replaces the two bodies and adds its
//! table tests (spec §9) here, and bumps
//! [`super::fold::DETECTOR_VERSION`] so files folded without resets are
//! folded again.
//!
//! The rules are spec §5.2 to §5.6:
//!
//! * compare consecutive samples `P`, `N` of one pool, in `watching` mode
//!   when `N.at - P.at <= thresholds.watch_gap_secs`, else `offline`;
//! * rule out the scheduled end, plan / limit / window changes, a rolling
//!   pool and a boundary that moved with no fall (§5.4) first;
//! * signals: `instant_jump`, `pct_fall` (to `near_zero_pct` while
//!   watching; by `max(offline_fall_pts, offline_fall_share% of P)` after a
//!   gap), `bank_fall` (a credit spent, or gone before its expiry);
//! * a `Cached` sample is never evidence;
//! * the effect is read off the event's own instants (§5.5) and the kind off
//!   the bank, diffed by credit identity (§5.6);
//! * every event's `evidence` JSON records the mode and the thresholds.

use super::BankReading;
use super::fold::{PlanChange, ResetEvent, Sample, Thresholds};

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
    /// `agent_usage.history_reset_effect` for this provider, when set: the
    /// effect to record for an event with no instant to read one from
    /// (§5.5). Written to the event's evidence as `source: config`.
    pub effect_override: Option<&'a str>,
}

/// The resets established between consecutive samples of one pool, oldest
/// first. Each `at_hi` is the time of a sample in `input.samples`.
///
/// Phase 1: none. Nothing is claimed until phase 2 detects it.
pub fn detect(input: &DetectInput) -> Vec<ResetEvent> {
    let _ = input;
    Vec::new()
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

/// Every bank event in one account's bank readings, oldest first, from the
/// whole list - the store rebuilds the account's `bank_event` rows from it
/// whenever a bank reading is written.
///
/// Phase 1: none.
pub fn bank_events(banks: &[BankReading]) -> Vec<BankEvent> {
    let _ = banks;
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_one_claims_no_reset_and_no_bank_event() {
        let samples = vec![Sample {
            at: 1.0,
            pct: Some(50.0),
            used: None,
            limit: None,
            window_secs: None,
            reset_at: None,
            source: super::super::fold::Source::Live,
            spacexsi: None,
        }];
        let input = DetectInput {
            before: None,
            samples: &samples,
            banks: &[],
            plans: &[],
            thresholds: Thresholds::default(),
            effect_override: None,
        };
        assert!(detect(&input).is_empty());
        assert!(bank_events(&[]).is_empty());
    }
}
