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

//! Over-utilisation: spend past an allowance. **Phase 4 (OPS-307) fills
//! this file.**
//!
//! The call sites are in place and their output is written:
//!
//! * [`spans`] is called by the store each time an `overage` pool is
//!   re-folded, with that pool's samples and those of every included pool
//!   `pool_link` ties it to; its spans replace the pool's `overage_span`
//!   rows from the same point on.
//! * [`events_of`] is called by `observe` for every observation; its
//!   per-request charges are written to `overage_event`, deduped by key.
//! * [`links_of`] is called by `observe` for every observation; its pairs
//!   are written to `pool_link` (insert-or-ignore).
//!
//! Phase 1 returns nothing from any of them. The rules are spec §6: spend is
//! measured in the pool's own unit, never a percentage; each delta between
//! two readings is one span with the full amount and the list of linked
//! included pools at or above 99.5% at the time - never split between them.

use super::Observation;
use super::fold::Sample;

/// An included pool an overage pool serves, with its samples over the same
/// stretch.
#[derive(Clone, Debug)]
pub struct Included<'a> {
    pub pool_id: i64,
    pub samples: &'a [Sample],
}

/// A row of `overage_span`, less the overage pool's id.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub at_lo: f64,
    pub at_hi: f64,
    /// In the overage pool's unit and currency.
    pub amount: f64,
    /// Ids of the included pools at or above 99.5% then. May be empty.
    pub exhausted: Vec<i64>,
}

/// The spend spans in an overage pool's samples, oldest first.
///
/// Phase 1: none.
pub fn spans(overage: &[Sample], included: &[Included]) -> Vec<Span> {
    let _ = (overage, included);
    Vec::new()
}

/// A per-request charge a provider published (Cursor's
/// `GetFilteredUsageEvents`, §6.3): a row of `overage_event`.
#[derive(Clone, Debug, PartialEq)]
pub struct Charge {
    /// The overage pool it belongs to, by the provider's pool key.
    pub pool_key: String,
    /// The provider's event id, or a hash of the event's fields.
    pub event_key: String,
    pub at: f64,
    pub amount: f64,
    pub model: Option<String>,
}

/// The per-request charges carried by one observation.
///
/// Phase 1: none.
pub fn events_of(obs: &Observation) -> Vec<Charge> {
    let _ = obs;
    Vec::new()
}

/// Which included pools each overage pool of one observation serves, as
/// `(overage pool key, included pool key)` pairs (Claude's extra usage
/// serves every lane of the account).
///
/// Phase 1: none.
pub fn links_of(obs: &Observation) -> Vec<(String, String)> {
    let _ = obs;
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_one_draws_no_overage() {
        assert!(spans(&[], &[]).is_empty());
        let obs = Observation::default();
        assert!(events_of(&obs).is_empty());
        assert!(links_of(&obs).is_empty());
    }
}
