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

//! Readings shared between every pane of one widget on this machine
//! (OPS-274, moved here from agent-usage by OPS-283).
//!
//! A pane holds each reading for a while in a [`Cache`]. When one goes
//! stale, the pane asks the widget's shared store first: another pane may
//! have just fetched it, or be fetching it now. Only when nobody has does
//! this pane fetch, under a lease, and publish what it got for the rest.
//! [`lease`] decides who fetches without touching the disk; [`store`]
//! carries that out in one SQLite file per widget.

use std::collections::HashMap;

pub mod lease;
pub mod store;

pub use store::{Got, Shared, Store};

/// Readings held between passes, and the store they are shared through.
#[derive(Default)]
pub struct Cache {
    /// key -> (when, value) of the last good reading, for `kept`.
    pub good: HashMap<String, (f64, serde_json::Value)>,
    /// key -> (when, value, ttl)
    pub live: HashMap<String, (f64, Option<serde_json::Value>, f64)>,
    /// key -> refusals in a row, which is what the backoff doubles on.
    /// Cleared the moment one gets through.
    pub fails: HashMap<String, u32>,
    /// When the reader pressed `r`, for the one pass that press asked for.
    /// A good reading taken before this is asked again rather than served
    /// out of its hold; see `cached`. Zero on every other pass.
    pub asked_at: f64,
    /// The readings shared with every other pane on this machine, when
    /// this pane is sharing.
    pub store: Option<Store>,
    /// Why this pane is not sharing, when it meant to and cannot.
    pub sharing_off: String,
    /// key -> (when, pid) of the shared reading this pane last took or
    /// gave: who fetched it, and when, even after a reader has dropped it
    /// from `live` to have it asked again.
    pub shared: HashMap<String, (f64, u32)>,
    /// Every token this pass holds. No shared reading may contain one.
    pub secrets: Vec<String>,
}

/// How old a good reading must be before `r` asks for it again.
///
/// Without this a press held every quota for its full interval - two
/// minutes to an hour - so the local counts moved and the numbers the
/// reader was actually waiting on did not. The floor keeps a key held
/// down from turning into a request per frame against endpoints whose
/// limits are shared with the agent itself (Claude's answers 429 to three
/// copies on a two-minute hold).
pub const ASKED_FLOOR: f64 = 30.0;
/// Where a refusal starts waiting, and where it stops.
///
/// Two minutes for the first, because one failure is usually nothing - a
/// dropped connection, a server having a moment - and waiting five for it
/// would make a blip look like an outage. Doubling after that, because a
/// refusal that keeps coming is not a blip, and the flat hold this replaced
/// walked back in at the same rate however many times it was turned away.
/// That is what sustains a rate limit rather than clearing it.
///
/// 120, 240, 480, 960, then 1800 and no further. The ceiling is thirty
/// minutes because that is when the screen starts calling a reading old:
/// backing off past the point where the reader is told something is wrong
/// would leave the widget quietly not trying.
pub const BACKOFF_FROM: f64 = 120.0;
pub const BACKOFF_MAX: f64 = 1800.0;

/// How long to wait after `n` refusals in a row.
pub fn backoff(n: u32) -> f64 {
    let doubled = BACKOFF_FROM * 2f64.powi(n.saturating_sub(1).min(16) as i32);
    doubled.min(BACKOFF_MAX)
}

/// Hold a reading for a while, but never hold a failure that long.
///
/// The pane redraws every thirty seconds; these windows move over hours. A
/// refusal is held too, so a dead endpoint is retried occasionally rather
/// than on every frame - and held longer each time it is refused again, so
/// an endpoint saying "too often" is not answered at the same rate that
/// provoked it.
pub fn cached<F>(cache: &mut Cache, key: &str, ttl: f64, fetch: F) -> Option<serde_json::Value>
where
    F: FnOnce() -> Option<serde_json::Value>,
{
    held_for(cache, key, ttl, true, fetch)
}

/// `cached` for a reading keyed on the file it came from, which `r` leaves
/// alone. A key carrying the file's own (mtime, size) cannot hold anything
/// the file does not still say, so asking again only re-parses every
/// session on disk to learn the same numbers.
pub fn cached_file<F>(cache: &mut Cache, key: &str, ttl: f64, fetch: F) -> Option<serde_json::Value>
where
    F: FnOnce() -> Option<serde_json::Value>,
{
    held_for(cache, key, ttl, false, fetch)
}

fn held_for<F>(
    cache: &mut Cache,
    key: &str,
    ttl: f64,
    askable: bool,
    fetch: F,
) -> Option<serde_json::Value>
where
    F: FnOnce() -> Option<serde_json::Value>,
{
    let at = crate::now();
    pull(cache, key, at, ttl);
    if let Some((when, value, held)) = cache.live.get(key) {
        // A refusal keeps its backoff even when asked: pressing `r` at an
        // endpoint that is rate-limiting us is how the limit is sustained.
        let good = is_reading(value);
        // Aged at the press, not now: a reading kept for being under the
        // floor must not be asked later by a pass nobody pressed for.
        let asked = askable
            && good
            && *when < cache.asked_at
            && cache.asked_at - when >= ASKED_FLOOR;
        if at - when < *held && !asked {
            return value.clone();
        }
    }
    // Stale here. Before asking, ask the other panes: one of them may have
    // just fetched it, or be fetching it now.
    if let Some(store) = cache.store.as_mut() {
        let known = cache
            .live
            .get(key)
            .map(|e| e.0)
            .or_else(|| cache.shared.get(key).map(|s| s.0));
        match store.claim(key, known, at) {
            Ok(store::Got::Mine) => {}
            Ok(store::Got::Fresh(s)) => {
                adopt_row(cache, key, s, at, ttl);
                if let Some((_, value, _)) = cache.live.get(key) {
                    return value.clone();
                }
            }
            // Another pane is asking. Show what there is until it lands.
            Ok(store::Got::Held(_)) => {
                if let Some((_, value, _)) = cache.live.get(key) {
                    return value.clone();
                }
                // Nothing at all to show - a pane just started. Asking too
                // is exactly the burst sharing exists to stop: six panes
                // opened together would each ask Claude at once. Wait for
                // the one that is asking instead; its fetch is no slower
                // than this pane's would have been.
                if let Some(value) = wait_for_holder(cache, key, ttl) {
                    return value;
                }
            }
            Err(why) => stop_sharing(cache, &why),
        }
    }
    let value = fetch();
    let held = if value.is_some() {
        cache.fails.remove(key);
        ttl
    } else {
        let n = cache.fails.entry(key.to_string()).or_insert(0);
        *n += 1;
        // Never longer than the interval itself would have been, for a
        // reading asked for hourly: backing an hourly plan read off to
        // thirty minutes is fine, but it must not exceed the hour.
        backoff(*n).min(ttl.max(BACKOFF_FROM))
    };
    if let Some(v) = value.as_ref().filter(|_| is_reading(&value)) {
        cache.good.insert(key.to_string(), (at, v.clone()));
    }
    cache.live.insert(key.to_string(), (at, value.clone(), held));
    if let Some(store) = cache.store.as_mut() {
        let s = store::Shared {
            taken_at: at,
            held,
            value: value.clone(),
            fails: cache.fails.get(key).copied().unwrap_or(0),
            good: cache.good.get(key).cloned(),
            fetched_by: store.me,
        };
        let me = store.me;
        match store.publish(key, &s, &cache.secrets, at) {
            Ok(_) => {
                cache.shared.insert(key.to_string(), (at, me));
            }
            Err(why) => stop_sharing(cache, &why),
        }
    }
    value
}

/// Whether a held value is a reading, rather than a refusal carrying its
/// reason. Antigravity holds its refusal under a name of its own.
fn is_reading(value: &Option<serde_json::Value>) -> bool {
    value
        .as_ref()
        .is_some_and(|v| v.get("why").is_none() && v.get("opscope_refusal").is_none())
}

/// How often a pane waiting on another pane's fetch looks for its answer.
const WAIT_STEP: std::time::Duration = std::time::Duration::from_millis(500);

/// Wait for the pane holding `key`'s lease to publish, and take what it
/// publishes.
///
/// `None` when this pane should fetch after all: the lease ran out or was
/// given back without a reading - a reading too large or holding a token
/// is kept by the pane that fetched it - or the store failed. A lease
/// lasts `LEASE` at most, so this waits no longer than that.
fn wait_for_holder(cache: &mut Cache, key: &str, ttl: f64) -> Option<Option<serde_json::Value>> {
    let until = crate::now() + lease::LEASE + 5.0;
    loop {
        std::thread::sleep(WAIT_STEP);
        let at = crate::now();
        let store = cache.store.as_mut()?;
        match store.claim(key, None, at) {
            Ok(store::Got::Fresh(s)) => {
                adopt_row(cache, key, s, at, ttl);
                return cache.live.get(key).map(|e| e.1.clone());
            }
            // Ours now: the holder went without publishing.
            Ok(store::Got::Mine) => return None,
            Ok(store::Got::Held(_)) if at < until => {}
            Ok(store::Got::Held(_)) => return None,
            // Another pane mid-write: look again.
            Err(why) if store::busy(&why) && at < until => {}
            Err(why) => {
                stop_sharing(cache, &why);
                return None;
            }
        }
    }
}

/// Take what another pane has published under `key` since this pane last
/// looked. Costs nothing when nobody has written.
fn pull(cache: &mut Cache, key: &str, at: f64, ttl: f64) {
    let Some(store) = cache.store.as_mut() else {
        return;
    };
    match store.load(key) {
        Ok(Some(s)) => adopt_row(cache, key, s, at, ttl),
        Ok(None) => {}
        Err(why) => stop_sharing(cache, &why),
    }
}

/// Put a shared reading in this pane's cache, when it is newer than what
/// the pane holds.
///
/// A reading is held for this pane's own `ttl`, not the publisher's: a pane
/// polling every minute must not be held to another's thirty. A refusal
/// keeps the publisher's hold, which is the backoff every pane must honour.
///
/// The reading this pane already holds, changed since it was taken, is
/// taken again: `set_live` writes a refusal's reason, or Codex's inventory,
/// in after the publish, and a pane that read between the two would
/// otherwise keep the bare refusal for the whole backoff.
fn adopt_row(cache: &mut Cache, key: &str, s: store::Shared, at: f64, ttl: f64) {
    let local = cache.live.get(key).map(|e| e.0);
    let same = local == Some(s.taken_at)
        && cache.shared.get(key) == Some(&(s.taken_at, s.fetched_by));
    if same {
        if let Some(entry) = cache.live.get_mut(key) {
            entry.1 = s.value.clone();
        }
    } else if lease::adopt(local, s.taken_at, at) {
        let held = if is_reading(&s.value) { ttl } else { s.held };
        cache.live.insert(key.to_string(), (s.taken_at, s.value, held));
        if s.fails == 0 {
            cache.fails.remove(key);
        } else {
            cache.fails.insert(key.to_string(), s.fails);
        }
        cache.shared.insert(key.to_string(), (s.taken_at, s.fetched_by));
    }
    if let Some((good_at, good)) = s.good {
        if cache.good.get(key).is_none_or(|(l, _)| good_at > *l) {
            cache.good.insert(key.to_string(), (good_at, good));
        }
    }
}

/// Go on alone, saying why. A store that failed once is not trusted for
/// the rest of the run; the poller opens it again later.
pub fn stop_sharing(cache: &mut Cache, why: &str) {
    cache.store = None;
    cache.sharing_off = why.to_string();
}

/// Replace the value held under `key` and, when this pane fetched it, the
/// shared copy too - so a reason or a detail a reader adds after the fetch
/// reaches the panes that did not fetch.
pub fn set_live(cache: &mut Cache, key: &str, value: serde_json::Value) {
    let Some((when, _, held)) = cache.live.get(key).cloned() else {
        return;
    };
    cache.live.insert(key.to_string(), (when, Some(value.clone()), held));
    let Some(store) = cache.store.as_mut() else {
        return;
    };
    if cache.shared.get(key) != Some(&(when, store.me)) {
        return;
    }
    if let Err(why) = store.annotate(key, when, &value) {
        stop_sharing(cache, &why);
    }
}

/// A short tag for the account a credential belongs to, for the keys of
/// readings shared between panes.
///
/// A shared reading outlives the pane that fetched it, so a key naming only
/// the directory - or nothing, for Codex - served the account signed in
/// before to the one signed in now. Hashing the credential changes the key
/// when the account does; a refresh that rotates it costs one early fetch.
/// FNV-1a, so every pane on this build computes the same tag, and only the
/// hash is ever written.
pub fn account_tag(credential: &str) -> String {
    if credential.is_empty() {
        return "none".into();
    }
    let mut hash = 0xcbf29ce484222325u64;
    for b in credential.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Note a token this pass holds, so no shared reading carries it.
pub fn secret(cache: &mut Cache, token: &str) {
    if !token.is_empty() && !cache.secrets.iter().any(|t| t == token) {
        cache.secrets.push(token.to_string());
    }
}

/// What the status line says about sharing: how many of the readings this
/// pane holds another pane fetched, or why this pane is not sharing.
pub fn sharing_status(cache: &Cache) -> String {
    match &cache.store {
        Some(store) => {
            let others = cache
                .live
                .keys()
                .filter(|k| cache.shared.get(*k).is_some_and(|(_, by)| *by != store.me))
                .count();
            let mut said = if others == 0 {
                "readings shared".to_string()
            } else {
                format!("readings shared · {others} fetched by other panes")
            };
            if !store.notice.is_empty() {
                said = format!("{said} · {}", store.notice);
            }
            said
        }
        None if !cache.sharing_off.is_empty() => {
            format!("not sharing readings: {}", cache.sharing_off)
        }
        None => String::new(),
    }
}

/// How long a good reading is kept on screen through failures in transit.
///
/// Long enough to ride out an outage, a laptop off the network, or a run of
/// rate limits - and short of the point where the 5-hour windows most of
/// these quotas have would have rolled over twice. Past it the tab says the
/// reading is missing and why, as it would have before this kept anything.
pub const KEEP_FOR: f64 = 6.0 * 3600.0;

/// Whether a refusal is the network or the server having a moment, rather
/// than the account saying no.
///
/// Only these are: a timeout, a host that could not be reached, an endpoint
/// that did not answer, a rate limit, and a server error. A refused token,
/// a missing one, a gone endpoint or an answer that did not parse are not,
/// and must not be papered over with a reading from before: those are the
/// account changing under the widget, and the last reading may not be its.
/// The phrases are the ones `refusal` and the readers write.
pub fn transient(why: &str) -> bool {
    let w = why.to_ascii_lowercase();
    ["timed out", "could not reach", "did not answer", "too many requests", "the server answered 5"]
        .iter()
        .any(|m| w.contains(m))
}

/// A quota reading kept from an earlier pass, and why it is not fresh.
#[derive(Clone, Debug, PartialEq)]
pub struct Kept {
    /// When the kept reading was taken - its own time, not this pass's.
    pub at: f64,
    /// Why this pass could not take a new one.
    pub why: String,
}

/// The last good reading under `key`, when this pass's failure to get one
/// was in transit and that reading is not older than `KEEP_FOR`.
///
/// Any other failure drops what was kept, so a token that was refused or an
/// account that changed can never bring an old reading back later.
pub fn kept(cache: &mut Cache, key: &str, why: &str) -> Option<(serde_json::Value, Kept)> {
    if why.is_empty() {
        return None;
    }
    if !transient(why) {
        cache.good.remove(key);
        return None;
    }
    let (at, value) = cache.good.get(key)?;
    if crate::now() - at > KEEP_FOR {
        return None;
    }
    Some((value.clone(), Kept { at: *at, why: why.to_string() }))
}

/// Put a refusal's reason in the slot `cached` just stored as `None`.
///
/// A diagnostic wrapped in `Some` looks like a reading: the fail count
/// clears and the next ask is two minutes later, forever. The reason still
/// has to survive the frames inside the backoff, so it is written in after
/// the hold and the tally have already been set. Later frames see the
/// sentence; they do not see a success.
pub fn remember_refusal(cache: &mut Cache, key: &str, why: &str) {
    if why.is_empty() {
        return;
    }
    set_live(cache, key, serde_json::json!({"why": why}));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::now;

    #[test]
    fn only_a_failure_in_transit_is_transient() {
        for why in [
            "the request timed out",
            "quota could not reach it",
            "Cursor's usage endpoint did not answer",
            "too many requests - something else is polling the same token",
            "the server answered 503",
        ] {
            assert!(transient(why), "{why}");
        }
        for why in [
            "the token was refused - the agent may need signing in again",
            "token rejected · copy a fresh bearer token",
            "no token - Codex has not signed in here",
            "the endpoint is gone",
            "the server answered 400",
            "",
        ] {
            assert!(!transient(why), "{why}");
        }
    }

    #[test]
    fn a_good_reading_is_kept_through_a_failure_in_transit_and_dropped_on_a_refusal() {
        let mut caches = Cache::default();
        let good = serde_json::json!({"u": {"pct": 40}});
        assert!(cached(&mut caches, "probe", 120.0, || Some(good.clone())).is_some());
        let taken = caches.good["probe"].0;
        // Past its hold, the endpoint does not answer.
        caches.live.get_mut("probe").unwrap().0 -= 500.0;
        caches.good.get_mut("probe").unwrap().0 -= 500.0;
        assert!(cached(&mut caches, "probe", 120.0, || None).is_none());
        let (value, k) = kept(&mut caches, "probe", "the request timed out").expect("kept");
        assert_eq!(value, good);
        assert_eq!(k.at, taken - 500.0, "the reading's own time, not this pass's");
        assert_eq!(k.why, "the request timed out");
        // Too old to keep is not kept, and says nothing it cannot back.
        caches.good.get_mut("probe").unwrap().0 = now() - KEEP_FOR - 1.0;
        assert!(kept(&mut caches, "probe", "the request timed out").is_none());
        caches.good.get_mut("probe").unwrap().0 = now() - 60.0;
        // The token refused: the old reading is dropped for good, so a later
        // timeout cannot bring it back under an account that said no.
        assert!(kept(&mut caches, "probe", "the token was refused").is_none());
        assert!(kept(&mut caches, "probe", "the request timed out").is_none());
        // A local fact riding in the value is not a reading either.
        assert!(cached(&mut caches, "fact", 120.0, || Some(serde_json::json!({"why": "no token"}))).is_some());
        assert!(!caches.good.contains_key("fact"));
    }

    /// The sequence, and why it is not a flat hold: the one it replaced
    /// walked back in at the same interval however many times it was turned
    /// away, which sustains a rate limit rather than clearing it.
    #[test]
    fn a_refusal_waits_longer_each_time_it_is_refused() {
        assert_eq!(backoff(1), 120.0, "one failure is usually nothing");
        assert_eq!(backoff(2), 240.0);
        assert_eq!(backoff(3), 480.0);
        assert_eq!(backoff(4), 960.0);
        assert_eq!(backoff(5), BACKOFF_MAX, "and then it stops doubling");
        assert_eq!(backoff(50), BACKOFF_MAX, "including well past the point of doubling");
        // Never zero and never negative, whatever it is handed.
        assert_eq!(backoff(0), 120.0);
        for n in 0..40 {
            let w = backoff(n);
            assert!(w >= BACKOFF_FROM && w <= BACKOFF_MAX, "n={} gave {}", n, w);
        }
        // Strictly growing until the ceiling, which is the whole point.
        for n in 1..5 {
            assert!(backoff(n) < backoff(n + 1), "n={} did not grow", n);
        }
    }

    #[test]
    fn the_hold_grows_across_calls_and_a_success_forgets_them() {
        let mut caches = Cache::default();
        let held = |c: &Cache| c.live.get("probe").map(|(_, _, h)| *h).unwrap();

        for (call, want) in [(1, 120.0), (2, 240.0), (3, 480.0)] {
            // Force the hold to have lapsed, so the next call really asks.
            caches.live.remove("probe");
            assert!(cached(&mut caches, "probe", 900.0, || None).is_none());
            assert_eq!(held(&caches), want, "refusal {}", call);
        }

        // One that gets through clears the tally, so the next blip starts
        // from two minutes again rather than from eight.
        caches.live.remove("probe");
        assert!(cached(&mut caches, "probe", 900.0, || Some(serde_json::json!(1))).is_some());
        assert_eq!(held(&caches), 900.0, "a good reading is held for its own interval");
        caches.live.remove("probe");
        assert!(cached(&mut caches, "probe", 900.0, || None).is_none());
        assert_eq!(held(&caches), 120.0, "the count did not reset on success");
    }

    /// An hourly reading may back off, but not past its own interval - and a
    /// two-minute one is never held for longer than the backoff says.
    #[test]
    fn the_backoff_never_outlasts_the_interval_it_belongs_to() {
        for ttl in [30.0, 120.0, 300.0, 900.0, 3600.0] {
            let mut caches = Cache::default();
            let key = format!("probe-{}", ttl);
            assert!(cached(&mut caches, &key, ttl, || None).is_none());
            let (_, _, held) = caches.live.get(&key).unwrap();
            assert!(
                *held <= ttl.max(BACKOFF_FROM),
                "ttl {}: held {}s, past the interval it belongs to",
                ttl,
                held
            );
            assert!(*held <= BACKOFF_MAX, "ttl {}: held {}s", ttl, held);
        }
    }

    #[test]
    fn a_remembered_refusal_does_not_clear_the_backoff() {
        // The shape that used to cost the backoff: wrapping a silent
        // endpoint in Some({"why": ...}) looks like a reading, so the fail
        // count reset and the next ask was two minutes later forever.
        let mut caches = Cache::default();
        assert!(cached(&mut caches, "probe", 120.0, || None).is_none());
        assert_eq!(caches.fails.get("probe"), Some(&1));
        let held = caches.live.get("probe").map(|(_, _, h)| *h).unwrap();
        remember_refusal(&mut caches, "probe", "the endpoint did not answer");
        assert_eq!(caches.fails.get("probe"), Some(&1), "the tally must stand");
        assert_eq!(
            caches.live.get("probe").map(|(_, _, h)| *h),
            Some(held),
            "the hold must stand"
        );

        let again = cached(&mut caches, "probe", 120.0, || {
            panic!("asked again inside the backoff")
        });
        assert_eq!(again.unwrap()["why"].as_str(), Some("the endpoint did not answer"));
        assert_eq!(caches.fails.get("probe"), Some(&1));
    }

    #[test]
    fn a_refresh_asks_again_for_a_good_reading_past_the_floor() {
        // The bug: `r` woke the poller, but every quota was still inside
        // its hold, so the press re-read local files and nothing else.
        let mut caches = Cache::default();
        let at = now() - ASKED_FLOOR - 1.0;
        caches.live.insert("probe".into(), (at, Some(serde_json::json!(1)), 300.0));
        caches.asked_at = now();
        let got = cached(&mut caches, "probe", 300.0, || Some(serde_json::json!(2)));
        assert_eq!(got, Some(serde_json::json!(2)));
        // Asked once per press, not on every pass after it.
        let again = cached(&mut caches, "probe", 300.0, || {
            panic!("asked twice for one press")
        });
        assert_eq!(again, Some(serde_json::json!(2)));
    }

    #[test]
    fn a_refresh_leaves_a_fresh_reading_and_a_refusal_alone() {
        let mut caches = Cache::default();
        // Taken a moment ago: a held-down key must not become a request
        // per frame.
        let fresh = now() - 1.0;
        caches.live.insert("fresh".into(), (fresh, Some(serde_json::json!(1)), 300.0));
        // Refused, and still inside its backoff, with or without a reason.
        let old = now() - ASKED_FLOOR - 1.0;
        let why = Some(serde_json::json!({"why": "429"}));
        let agy = Some(serde_json::json!({"opscope_refusal": "refused"}));
        caches.live.insert("refused".into(), (old, None, 300.0));
        caches.live.insert("why".into(), (old, why, 300.0));
        caches.live.insert("agy".into(), (old, agy, 300.0));
        caches.asked_at = now();
        for key in ["fresh", "refused", "why", "agy"] {
            cached(&mut caches, key, 300.0, || panic!("{} was asked again", key));
        }
    }

    #[test]
    fn a_reading_kept_under_the_floor_is_not_asked_once_it_crosses_it() {
        // Ten seconds old at the press, thirty now: the press kept it, and
        // nothing since asked for it.
        let mut caches = Cache::default();
        caches.asked_at = now() - 20.0;
        let taken = now() - 30.0;
        caches.live.insert("probe".into(), (taken, Some(serde_json::json!(1)), 300.0));
        cached(&mut caches, "probe", 300.0, || panic!("asked without a press"));
    }

    #[test]
    fn a_refresh_does_not_reparse_a_file_keyed_reading() {
        // Keyed on the file's (mtime, size), so asking again reads the same
        // bytes back.
        let mut caches = Cache::default();
        let old = now() - ASKED_FLOOR - 1.0;
        caches.live.insert("file".into(), (old, Some(serde_json::json!(1)), 3600.0));
        caches.asked_at = now();
        cached_file(&mut caches, "file", 3600.0, || panic!("re-parsed an unchanged file"));
    }

    #[test]
    fn a_good_reading_is_held_for_its_full_interval() {
        let mut caches = Cache::default();
        let one = cached(&mut caches, "probe", 300.0, || Some(serde_json::json!(1)));
        assert_eq!(one, Some(serde_json::json!(1)));
        // The second call must not reach the fetcher at all - that is the
        // whole point of the hold, and what keeps the endpoint's rate down.
        let two = cached(&mut caches, "probe", 300.0, || {
            panic!("asked again inside the hold")
        });
        assert_eq!(two, Some(serde_json::json!(1)));
        let (_, _, held) = caches.live.get("probe").unwrap();
        assert_eq!(*held, 300.0);
    }

    /// Two panes on one store, as `share_readings` would open them.
    fn two_panes(name: &str) -> (Cache, Cache) {
        let dir = std::env::temp_dir().join(format!(
            "opscope-held-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-usage.sqlite").to_string_lossy().to_string();
        let pane = |me: u32| {
            let mut store = Store::open(&path).unwrap();
            store.me = me;
            Cache { store: Some(store), ..Cache::default() }
        };
        (pane(41), pane(42))
    }

    #[test]
    fn a_pane_that_starts_while_another_is_asking_waits_for_its_answer() {
        let (mut a, mut b) = two_panes("wait");
        let mut store = a.store.take().unwrap();
        assert_eq!(store.claim("k", None, now()).unwrap(), Got::Mine);
        let asking = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(700));
            let at = now();
            let s = Shared {
                taken_at: at,
                held: 120.0,
                value: Some(serde_json::json!({"u": 1})),
                fails: 0,
                good: None,
                fetched_by: store.me,
            };
            store.publish("k", &s, &[], at).unwrap();
        });
        let got = cached(&mut b, "k", 120.0, || panic!("asked while another pane was asking"));
        asking.join().unwrap();
        assert_eq!(got, Some(serde_json::json!({"u": 1})));
        assert_eq!(b.shared.get("k").map(|s| s.1), Some(41));
    }

    #[test]
    fn a_reason_written_after_the_publish_reaches_a_pane_that_read_before_it() {
        let (mut a, mut b) = two_panes("annotate");
        assert_eq!(cached(&mut a, "k", 120.0, || None), None);
        b.store.as_mut().unwrap().begin_pass().unwrap();
        assert_eq!(cached(&mut b, "k", 120.0, || panic!("asked inside the backoff")), None);
        remember_refusal(&mut a, "k", "too many requests");
        b.store.as_mut().unwrap().begin_pass().unwrap();
        let got = cached(&mut b, "k", 120.0, || panic!("asked inside the backoff"));
        assert_eq!(got, Some(serde_json::json!({"why": "too many requests"})));
        assert_eq!(b.fails.get("k"), Some(&1));
    }

    #[test]
    fn a_shared_reading_is_held_for_this_panes_interval_and_a_refusal_for_the_backoff() {
        let (mut a, mut b) = two_panes("ttl");
        cached(&mut a, "slow", 1800.0, || Some(serde_json::json!({"u": 1})));
        cached(&mut a, "refused", 1800.0, || None);
        b.store.as_mut().unwrap().begin_pass().unwrap();
        cached(&mut b, "slow", 60.0, || panic!("asked for a reading another pane just took"));
        cached(&mut b, "refused", 60.0, || panic!("asked inside the backoff"));
        assert_eq!(b.live["slow"].2, 60.0);
        assert_eq!(b.live["refused"].2, a.live["refused"].2);
    }
}
