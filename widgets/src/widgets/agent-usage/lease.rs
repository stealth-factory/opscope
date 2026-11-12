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

//! Who fetches a shared reading, decided without touching the disk.
//!
//! Several panes on one machine read one store (`sharing.rs`). When a
//! reading goes stale, exactly one of them asks the vendor and the rest wait
//! for what it publishes. There is no long-lived leader: each reading has a
//! lease that lasts one fetch, so a pane that dies - a dropped SSH session,
//! a closed tab - holds nothing for longer than `LEASE`, and nothing has to
//! notice it went. Publishing is the heartbeat and the release at once, so
//! a machine with nothing to fetch writes nothing.
//!
//! Every rule is a pure function of times and holders, so the races can be
//! tested here without a file, and the store only carries them out inside a
//! transaction.

/// How long one fetch may hold a reading before another pane may take it.
///
/// Longer than any fetch here takes - the slowest is a curl with a
/// thirty-second ceiling, or a CLI that has to start first - and short
/// enough that a pane that vanished mid-fetch costs at most a minute and a
/// half of an older reading.
pub const LEASE: f64 = 90.0;

/// How far ahead of this clock a time may be before it is distrusted.
///
/// Every pane sharing a store runs on one machine, so their clocks are one
/// clock. A time further ahead than this was written before the clock was
/// stepped back - NTP, a resume - and is treated as unknown rather than as
/// a reading from the future that nothing could ever replace.
pub const SKEW: f64 = 60.0;

/// A reading nobody has published for this long is deleted.
///
/// Four times `KEEP_FOR`, so nothing that could still be shown is lost, and
/// short enough that an agent turned off, a Claude directory removed or a
/// token rotated leaves no row behind for long.
pub const FORGET_AFTER: f64 = 24.0 * 3600.0;

/// Who is fetching a reading, and until when they may.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lease {
    pub holder: u32,
    pub expires_at: f64,
}

/// What a pane that wants a fresh reading should do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Claim {
    /// Fetch it: the lease is this pane's now.
    Mine,
    /// Another pane is fetching it; show what there is and look again on
    /// the next pass.
    Held(u32),
    /// Another pane published a reading newer than the one this pane has,
    /// and it is still good. Take that instead of asking.
    Fresh,
}

/// Whether `me` may take the lease at `now`.
///
/// Free, its own, run out, or so far in the future that it was written
/// before the clock went back.
pub fn claimable(lease: Option<Lease>, me: u32, now: f64) -> bool {
    match lease {
        None => true,
        Some(l) => l.holder == me || l.expires_at <= now || l.expires_at > now + 2.0 * LEASE,
    }
}

/// Whether a stored reading taken at `taken` and held for `held` is still
/// good at `now`.
pub fn good_at(taken: f64, held: f64, now: f64) -> bool {
    taken <= now + SKEW && now - taken < held
}

/// Decide a claim.
///
/// `stored` is the reading in the store, as (taken, held); `known` is when
/// the reading this pane holds was taken. A stored reading newer than the
/// pane's and still good means somebody has just done the work, and it is
/// taken instead of asked for again - that is what turns six panes crossing
/// a TTL in the same second into one request.
pub fn claim(
    stored: Option<(f64, f64)>,
    known: Option<f64>,
    lease: Option<Lease>,
    me: u32,
    now: f64,
) -> Claim {
    if let Some((taken, held)) = stored {
        if known.is_none_or(|k| taken > k) && good_at(taken, held, now) {
            return Claim::Fresh;
        }
    }
    if claimable(lease, me, now) {
        Claim::Mine
    } else {
        Claim::Held(lease.map_or(0, |l| l.holder))
    }
}

/// Whether a reading taken at `mine` may replace the stored one.
///
/// Newer wins, by the reading's own time rather than by who wrote last, so
/// a fetch that hung past its lease and finished after another pane's
/// cannot put an older reading over a newer one. A stored time from the
/// future is not a reading anyone can beat, so it loses.
pub fn supersedes(stored: Option<f64>, mine: f64, now: f64) -> bool {
    match stored {
        None => true,
        Some(s) => mine > s || s > now + SKEW,
    }
}

/// Whether a reading from the store should replace the one this pane holds.
pub fn adopt(local: Option<f64>, shared: f64, now: f64) -> bool {
    shared <= now + SKEW && local.is_none_or(|l| shared > l)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: u32 = 100;
    const THEM: u32 = 200;
    const NOW: f64 = 1_000_000.0;

    fn lease(holder: u32, expires_at: f64) -> Option<Lease> {
        Some(Lease { holder, expires_at })
    }

    #[test]
    fn a_fresh_reading_this_pane_already_has_is_not_fresh_news() {
        // Already held: the caller's own TTL check serves it, and a claim
        // here only happens because it went stale or `r` asked.
        let stored = Some((NOW - 10.0, 300.0));
        assert_eq!(claim(stored, Some(NOW - 10.0), None, ME, NOW), Claim::Mine);
    }

    #[test]
    fn a_newer_good_reading_from_another_pane_is_taken_not_asked() {
        let stored = Some((NOW - 5.0, 300.0));
        assert_eq!(claim(stored, Some(NOW - 400.0), None, ME, NOW), Claim::Fresh);
        assert_eq!(claim(stored, None, None, ME, NOW), Claim::Fresh);
    }

    #[test]
    fn a_stale_reading_with_no_lease_is_fetched() {
        let stored = Some((NOW - 400.0, 300.0));
        assert_eq!(claim(stored, Some(NOW - 400.0), None, ME, NOW), Claim::Mine);
        assert_eq!(claim(None, None, None, ME, NOW), Claim::Mine);
    }

    #[test]
    fn a_stale_reading_another_pane_is_fetching_is_left_to_it() {
        let stored = Some((NOW - 400.0, 300.0));
        assert_eq!(
            claim(stored, Some(NOW - 400.0), lease(THEM, NOW + 30.0), ME, NOW),
            Claim::Held(THEM)
        );
    }

    #[test]
    fn a_lease_that_ran_out_is_taken_over() {
        // The pane that held it died or hung: nobody has to notice, the
        // lease simply stops counting.
        let stored = Some((NOW - 400.0, 300.0));
        assert_eq!(claim(stored, None, lease(THEM, NOW - 1.0), ME, NOW), Claim::Mine);
        assert_eq!(claim(stored, None, lease(THEM, NOW), ME, NOW), Claim::Mine);
    }

    #[test]
    fn a_lease_from_the_future_can_be_claimed() {
        assert!(claimable(lease(THEM, NOW + 2.0 * LEASE + 1.0), ME, NOW));
        assert!(!claimable(lease(THEM, NOW + LEASE), ME, NOW));
    }

    #[test]
    fn a_pane_may_renew_its_own_lease() {
        assert!(claimable(lease(ME, NOW + 30.0), ME, NOW));
    }

    #[test]
    fn a_reading_from_the_future_is_never_good_and_never_adopted() {
        assert!(!good_at(NOW + SKEW + 1.0, 300.0, NOW));
        assert!(!adopt(None, NOW + SKEW + 1.0, NOW));
        let stored = Some((NOW + 3600.0, 300.0));
        assert_eq!(claim(stored, None, None, ME, NOW), Claim::Mine);
    }

    #[test]
    fn a_refusal_is_fresh_for_its_backoff() {
        // A refusal is stored with its backoff as its hold, so the panes
        // that did not ask also wait it out.
        let stored = Some((NOW - 100.0, 240.0));
        assert_eq!(claim(stored, None, None, ME, NOW), Claim::Fresh);
        let stored = Some((NOW - 300.0, 240.0));
        assert_eq!(claim(stored, None, None, ME, NOW), Claim::Mine);
    }

    #[test]
    fn a_late_older_publish_loses_to_a_newer_one() {
        assert!(!supersedes(Some(NOW - 10.0), NOW - 60.0, NOW));
        assert!(supersedes(Some(NOW - 60.0), NOW - 10.0, NOW));
        assert!(supersedes(None, NOW, NOW));
        assert!(supersedes(Some(NOW + 3600.0), NOW, NOW));
    }

    #[test]
    fn only_a_newer_shared_reading_replaces_the_panes_own() {
        assert!(adopt(None, NOW - 10.0, NOW));
        assert!(adopt(Some(NOW - 60.0), NOW - 10.0, NOW));
        assert!(!adopt(Some(NOW - 10.0), NOW - 10.0, NOW));
        assert!(!adopt(Some(NOW - 10.0), NOW - 60.0, NOW));
    }
}
