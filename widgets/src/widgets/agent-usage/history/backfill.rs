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

//! The one-off Codex backfill. **Phase 5 (OPS-308) fills this file.**
//!
//! Codex rollouts carry `rate_limits` snapshots on their `token_count`
//! events (`codex.rs`, `newest_limits`): `used_percent`, `resets_at` and the
//! window in minutes, each at the event's own time. Replayed as samples with
//! `source = 'backfill'` they are full evidence (spec §4, §8), and Codex gets
//! months of history on day one.
//!
//! `observe` already calls this for the Codex observation on every pass:
//! when [`due`] says the backfill has not run for that account, it writes
//! whatever [`collect`] returns through the ordinary sample path (so the
//! five-minute rule and the re-fold from the earliest sample apply), then
//! stores [`DONE`] under the account's [`meta_key`] so it never runs twice.
//! Phase 1's [`due`] is always false.

use super::Observation;
use super::fold::Sample;
use crate::shared::{PoolRole, Unit};

/// What `meta` holds under [`meta_key`] once the backfill has run.
pub const DONE: &str = "done";

/// The `meta` key recording that one account's backfill has run.
pub fn meta_key(account_key: &str) -> String {
    format!("backfill:codex:{account_key}")
}

/// Whether the backfill should run, given what `meta` holds under
/// [`meta_key`] (`None` when nothing).
///
/// Phase 1: never.
pub fn due(recorded: Option<&str>) -> bool {
    let _ = recorded;
    false
}

/// One replayed reading, with the pool it belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct Replayed {
    /// The same key the live lane uses (`primary_window`, ...), so replayed
    /// and live samples land in one pool.
    pub pool_key: String,
    pub label: String,
    pub unit: Unit,
    pub role: PoolRole,
    /// `source` is always `Backfill`.
    pub sample: Sample,
}

/// Every rate-limit snapshot this machine's Codex rollouts hold for the
/// account `codex` describes, oldest first.
///
/// Phase 1: none.
pub fn collect(codex: &Observation) -> Vec<Replayed> {
    let _ = codex;
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_one_never_backfills() {
        assert!(!due(None));
        assert!(collect(&Observation::default()).is_empty());
        assert_eq!(meta_key("abc"), "backfill:codex:abc");
    }
}
