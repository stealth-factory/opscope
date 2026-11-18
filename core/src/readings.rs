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

pub use store::{Caps, Got, Publish, Shared, Store};

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
    /// key -> the pane fetching it now, as last seen by `take`, for the
    /// `pane 48213 asking now` a stale reading says.
    pub asking: HashMap<String, u32>,
    /// key -> why the reading this pane fetched stayed with it: too large
    /// to share, or it carried a token.
    pub unshared: HashMap<String, &'static str>,
    /// The store [`share`] opens, its limits, and when it last tried.
    pub share_path: String,
    pub caps: store::Caps,
    pub share_tried: Option<f64>,
    /// Set when [`sharing_rows`] has drawn the store's notice, so the next
    /// [`share`] clears it: shown for one pass, not for the whole run.
    pub notice_drawn: std::sync::atomic::AtomicBool,
    /// Keys whose [`set_live`] change the store was too busy to take; the
    /// next [`pull`] of each writes it.
    pub annotate_pending: std::collections::HashSet<String>,
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
            let value = value.clone();
            cache.asking.remove(key);
            return value;
        }
    }
    // Stale here. Before asking, ask the other panes: one of them may have
    // just fetched it, or be fetching it now.
    let mut claimed_at = None;
    if let Some(store) = cache.store.as_mut() {
        let known = cache
            .live
            .get(key)
            .map(|e| e.0)
            .or_else(|| cache.shared.get(key).map(|s| s.0));
        match store.claim(key, known, at) {
            Ok(store::Got::Mine) => claimed_at = Some(at),
            Ok(store::Got::Fresh(s)) => {
                adopt_row(cache, key, s, at, ttl);
                cache.asking.remove(key);
                if let Some((_, value, _)) = cache.live.get(key) {
                    return value.clone();
                }
            }
            // Another pane is asking. Show what there is until it lands,
            // noting who, for the `pane 48213 asking now` it says.
            Ok(store::Got::Held(by)) => {
                if let Some((_, value, _)) = cache.live.get(key) {
                    let value = value.clone();
                    cache.asking.insert(key.to_string(), by);
                    return value;
                }
                // Nothing at all to show - a pane just started. Asking too
                // is exactly the burst sharing exists to stop: six panes
                // opened together would each ask Claude at once. Wait for
                // the one that is asking instead; its fetch is no slower
                // than this pane's would have been.
                if let Some(value) = wait_for_holder(cache, key, ttl, &mut claimed_at) {
                    return value;
                }
            }
            // Another pane mid-write past `BUSY`: not this pass. Fetch
            // alone this once rather than give up sharing for ten minutes,
            // which is the moment sharing matters most.
            Err(why) if store::busy(&why) => {}
            Err(why) => stop_sharing(cache, &why),
        }
    }
    cache.asking.remove(key);
    let value = fetch();
    // The reading's own time is when the answer arrived, not when it was
    // asked for: a fetch slower than the TTL would otherwise land already
    // stale, and be asked for again on the next pass (OPS-284).
    let at = crate::now();
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
        match store.publish_since(key, &s, &cache.secrets, at, claimed_at) {
            Ok(done) => {
                cache.shared.insert(key.to_string(), (at, me));
                note_unshared(cache, key, done);
            }
            Err(why) if store::busy(&why) => {}
            Err(why) => stop_sharing(cache, &why),
        }
    }
    value
}

fn note_unshared(cache: &mut Cache, key: &str, done: store::Publish) {
    match done.unshared() {
        Some(why) => {
            cache.unshared.insert(key.to_string(), why);
        }
        None => {
            cache.unshared.remove(key);
        }
    }
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
/// lasts `LEASE` at most, so this waits no longer than that. When this
/// pane takes the lease over, `claimed_at` says when.
fn wait_for_holder(
    cache: &mut Cache,
    key: &str,
    ttl: f64,
    claimed_at: &mut Option<f64>,
) -> Option<Option<serde_json::Value>> {
    let until = crate::now() + lease::LEASE + 5.0;
    loop {
        std::thread::sleep(WAIT_STEP);
        let at = crate::now();
        let store = cache.store.as_mut()?;
        match store.claim(key, None, at) {
            Ok(store::Got::Fresh(s)) => {
                adopt_row(cache, key, s, at, ttl);
                cache.asking.remove(key);
                return cache.live.get(key).map(|e| e.1.clone());
            }
            // Ours now: the holder went without publishing.
            Ok(store::Got::Mine) => {
                *claimed_at = Some(at);
                return None;
            }
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

/// Read `key` into this pane from the store, without claiming its lease.
///
/// A spent rate limit stops the asking. It must not stop a pane that has
/// just opened from showing a reading another pane already published:
/// [`take`] claims the lease on its way to the row, and a closed gate
/// never gets that far. [`share`] and [`follow`] do not load it either.
pub fn recall(cache: &mut Cache, key: &str, ttl: f64) {
    pull(cache, key, crate::now(), ttl);
}

/// Take what another pane has published under `key` since this pane last
/// looked. Costs nothing when nobody has written.
fn pull(cache: &mut Cache, key: &str, at: f64, ttl: f64) {
    if cache.annotate_pending.remove(key) {
        annotate_shared(cache, key);
    }
    let Some(store) = cache.store.as_mut() else {
        return;
    };
    match store.load(key) {
        Ok(Some(s)) => adopt_row(cache, key, s, at, ttl),
        Ok(None) => {}
        Err(why) if store::busy(&why) => {}
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
/// otherwise keep the bare refusal for the whole backoff. This pane's own
/// reading is never taken back that way: its copy is the one `set_live`
/// changed, and the stored one may be behind it.
fn adopt_row(cache: &mut Cache, key: &str, s: store::Shared, at: f64, ttl: f64) {
    let local = cache.live.get(key).map(|e| e.0);
    let same = local == Some(s.taken_at)
        && cache.shared.get(key) == Some(&(s.taken_at, s.fetched_by));
    let mine = cache.store.as_ref().is_some_and(|st| st.me == s.fetched_by);
    if same {
        if !mine {
            if let Some(entry) = cache.live.get_mut(key) {
                entry.1 = s.value.clone();
            }
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
        // Why this pane's own reading stayed here no longer describes the
        // one on screen, which another pane did share.
        cache.unshared.remove(key);
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
    cache.live.insert(key.to_string(), (when, Some(value), held));
    annotate_shared(cache, key);
}

/// Write this pane's held value under `key` over its shared copy, when this
/// pane fetched it. A busy store leaves it for the next [`pull`].
fn annotate_shared(cache: &mut Cache, key: &str) {
    let Some((when, Some(value), _)) = cache.live.get(key).cloned() else {
        return;
    };
    let Some(store) = cache.store.as_mut() else {
        return;
    };
    if cache.shared.get(key) != Some(&(when, store.me)) {
        return;
    }
    match store.annotate(key, when, &value, &cache.secrets) {
        Ok(_) => {}
        Err(why) if store::busy(&why) => {
            cache.annotate_pending.insert(key.to_string());
        }
        Err(why) => stop_sharing(cache, &why),
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

// ── readings that take many requests (OPS-284) ──────────────────────────

/// How long to wait after a store could not be opened before trying again:
/// a permission or a full disk does not fix itself in a pass.
pub const SHARE_RETRY: f64 = 600.0;

/// How often a pane looks for a reading another pane published, between
/// its own passes. Costs one page of the file's header and no network when
/// nothing changed; a widget's poller calls [`follow`] this often.
pub const FOLLOW_EVERY: f64 = 5.0;

/// Open, keep or drop this widget's store, as `share_readings` asks, and
/// note whether another pane has written since the last call.
///
/// `name` is the widget's own: one file per widget, never shared with
/// another widget. A store that will not open is tried again after
/// `SHARE_RETRY`; a store whose file was replaced under it - another build
/// started it again - is opened again at once, or this pane would share
/// with nobody.
pub fn share(cache: &mut Cache, name: &str, caps: store::Caps, on: bool, now: f64) {
    if !on {
        cache.store = None;
        cache.sharing_off = "share_readings is off".into();
        // Turned back on, it opens at once: the retry wait is for a store
        // that would not open, not for one the reader closed.
        cache.share_tried = None;
        return;
    }
    cache.share_path = store::path(name);
    cache.caps = caps;
    clear_drawn_notice(cache);
    if cache.store.as_ref().is_some_and(|s| s.replaced()) {
        cache.store = None;
        cache.share_tried = None;
    }
    if cache.store.is_none() && cache.share_tried.is_none_or(|t| now - t >= SHARE_RETRY) {
        cache.share_tried = Some(now);
        match Store::open_with(&cache.share_path, caps) {
            Ok(s) => {
                cache.store = Some(s);
                cache.sharing_off.clear();
                // A gate this pane wrote to a store it no longer has open
                // is written again to this one by the next `gate`.
                cache.shared.retain(|key, _| !key.starts_with("gate/"));
            }
            Err(why) => cache.sharing_off = why,
        }
    }
    follow(cache);
}

/// Forget the store's notice once [`sharing_rows`] has drawn it.
fn clear_drawn_notice(cache: &mut Cache) {
    if cache.notice_drawn.swap(false, std::sync::atomic::Ordering::Relaxed) {
        if let Some(store) = cache.store.as_mut() {
            store.notice.clear();
        }
    }
}

/// Note whether another pane has written since this pane last looked, so
/// the next `take` reads only the rows that changed. Call it every
/// `FOLLOW_EVERY` on the poller thread, never the render thread.
pub fn follow(cache: &mut Cache) {
    if let Some(store) = cache.store.as_mut() {
        match store.begin_pass() {
            Ok(()) => {}
            Err(why) if store::busy(&why) => {}
            Err(why) => stop_sharing(cache, &why),
        }
    }
}

/// The reading under `key` as any pane last left it, without claiming a
/// lease or fetching anything: for a figure a widget shares but does not
/// ask for on screen - GitHub's remaining budget, for one - so that it
/// changes one small row instead of being copied into every reading's
/// value (OPS-298).
pub fn peek(cache: &mut Cache, key: &str) -> Option<serde_json::Value> {
    pull(cache, key, crate::now(), 60.0);
    cache.live.get(key).and_then(|e| e.1.clone())
}

/// Share `value` taken at `at`, without claiming a lease.
///
/// For a figure the widget already observed — GitHub's remaining budget —
/// which must be published even while the stored row is still inside its
/// hold. A stored reading newer than `at`, or one another pane is writing
/// now, is left as it is.
pub fn record(cache: &mut Cache, key: &str, value: serde_json::Value, at: f64, held: f64) {
    let Some(store) = cache.store.as_mut() else {
        cache.live.insert(key.to_string(), (at, Some(value.clone()), held));
        cache.good.insert(key.to_string(), (at, value));
        return;
    };
    let me = store.me;
    let shared = store::Shared {
        taken_at: at,
        held,
        value: Some(value.clone()),
        fails: cache.fails.get(key).copied().unwrap_or(0),
        good: Some((at, value.clone())),
        fetched_by: me,
    };
    match store.publish_since(key, &shared, &cache.secrets, crate::now(), None) {
        Ok(store::Publish::Written) => {
            cache.live.insert(key.to_string(), (at, Some(value.clone()), held));
            cache.good.insert(key.to_string(), (at, value));
            cache.shared.insert(key.to_string(), (at, me));
            cache.unshared.remove(key);
        }
        Ok(store::Publish::Older) | Ok(store::Publish::TakenOver) => {}
        // Too large, or it carried a token: the store refuses it, and the
        // pane keeps it, the same as a pane with no store.
        Ok(other @ (store::Publish::TooLarge | store::Publish::HeldSecret)) => {
            cache.live.insert(key.to_string(), (at, Some(value.clone()), held));
            cache.good.insert(key.to_string(), (at, value));
            note_unshared(cache, key, other);
        }
        Err(why) if store::busy(&why) => {
            cache.live.insert(key.to_string(), (at, Some(value), held));
        }
        Err(why) => stop_sharing(cache, &why),
    }
}

/// A short, stable fingerprint of `parts`: SHA-256 over a fixed prefix and
/// the parts, the first 16 bytes in hex. The same on every build and every
/// toolchain, which `std`'s hasher does not promise.
pub fn fingerprint(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"opscope-readings");
    for p in parts {
        h.update([0u8]);
        h.update(p.as_bytes());
    }
    h.finalize()[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// The key of one reading: `unit/shape:plain…:%sfingerprint`.
///
/// `secret` parts - a token, an account's credential - go into the key only
/// as a fingerprint, and are registered with [`secret`] in the same step,
/// so no reading containing one is ever written. A `:` or `%` in a plain
/// part is escaped, so `["a:b", "c"]` and `["a", "b:c"]` are two keys, and
/// the fingerprint is marked `%s`, which no escaped plain part can contain,
/// so a plain part equal to a fingerprint is not that fingerprint.
/// `shape` is bumped when
/// what is stored under `unit` changes, so an older build and a newer one
/// keep separate rows rather than reading each other's.
pub fn key(cache: &mut Cache, unit: &str, shape: u32, plain: &[&str], secrets: &[&str]) -> String {
    for s in secrets {
        secret(cache, s);
    }
    let mut out = format!("{unit}/{shape}");
    for p in plain {
        out.push(':');
        out.push_str(&p.replace('%', "%25").replace(':', "%3A"));
    }
    if !secrets.is_empty() {
        out.push_str(":%s");
        out.push_str(&fingerprint(secrets));
    }
    out
}

/// How one reading is held and asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Policy {
    /// This pane's own hold for a good reading.
    pub ttl: f64,
    /// One request of the fetch: its timeout and a margin. The lease is
    /// renewed for this long before each request, so a pane that dies
    /// mid-fetch holds the reading for one request, not a whole pass.
    pub lease_for: f64,
    /// Whether `r` asks for it again; false for a reading keyed on a file.
    pub askable: bool,
}

impl Policy {
    pub fn new(ttl: f64, lease_for: f64) -> Policy {
        Policy { ttl, lease_for, askable: true }
    }
}

/// What a pane should do about one reading.
pub enum Take<'a> {
    /// Show this: fresh here, or just published by another pane. A
    /// refusal is served too, for its backoff; [`refusal`] reads it.
    Serve(Option<serde_json::Value>),
    /// Fetch it: this pane holds the lease. Renew it before each request,
    /// then publish or refuse. Dropped, it gives the lease back.
    Fetch(Ticket<'a>),
    /// Another pane is fetching it (`by`, 0 when unknown), or the store was
    /// busy this pass. Show what this pane has, and look again next pass.
    Waiting { by: u32, have: Option<serde_json::Value> },
}

/// Decide what to do about `key`, without ever waiting on another pane.
///
/// The non-blocking counterpart of `cached`, for a fetch that is many
/// requests and may take minutes: the pane keeps drawing what it has while
/// another pane asks.
pub fn take<'a>(cache: &'a mut Cache, key: &str, p: Policy) -> Take<'a> {
    let at = crate::now();
    pull(cache, key, at, p.ttl);
    let have = |cache: &Cache| cache.live.get(key).and_then(|e| e.1.clone());
    if let Some((when, value, held)) = cache.live.get(key) {
        let asked = p.askable
            && is_reading(value)
            && *when < cache.asked_at
            && cache.asked_at - when >= cache.asked_floor(p.ttl);
        if at - when < *held && !asked {
            cache.asking.remove(key);
            return Take::Serve(value.clone());
        }
    }
    let mut claimed = None;
    if let Some(store) = cache.store.as_mut() {
        let known = cache.live.get(key).map(|e| e.0).or_else(|| cache.shared.get(key).map(|s| s.0));
        match store.claim_for(key, known, at, p.lease_for) {
            Ok(store::Got::Mine) => claimed = Some(at),
            Ok(store::Got::Fresh(s)) => {
                adopt_row(cache, key, s, at, p.ttl);
                cache.asking.remove(key);
                return Take::Serve(have(cache));
            }
            Ok(store::Got::Held(by)) => {
                cache.asking.insert(key.to_string(), by);
                return Take::Waiting { by, have: have(cache) };
            }
            Err(why) if store::busy(&why) => return Take::Waiting { by: 0, have: have(cache) },
            Err(why) => stop_sharing(cache, &why),
        }
    }
    cache.asking.remove(key);
    Take::Fetch(Ticket { cache, key: key.to_string(), policy: p, claimed, done: false })
}

impl Cache {
    /// `r`'s floor for a reading held `ttl`: thirty seconds, or the hold
    /// itself when that is shorter.
    fn asked_floor(&self, ttl: f64) -> f64 {
        ASKED_FLOOR.min(ttl)
    }
}

/// The lease on one reading while this pane fetches it.
pub struct Ticket<'a> {
    cache: &'a mut Cache,
    key: String,
    policy: Policy,
    /// When this pane claimed the lease, if it did.
    claimed: Option<f64>,
    done: bool,
}

impl Ticket<'_> {
    /// Hold the lease for one more request. `false` when it ran out and
    /// another pane took it: stop, and show what that pane publishes.
    pub fn renew(&mut self) -> bool {
        if self.claimed.is_none() {
            return true;
        }
        let Some(store) = self.cache.store.as_mut() else {
            return true;
        };
        match store.renew(&self.key, crate::now(), self.policy.lease_for) {
            Ok(mine) => mine,
            Err(why) if store::busy(&why) => true,
            Err(why) => {
                stop_sharing(self.cache, &why);
                true
            }
        }
    }

    /// Whether `bucket` is shut, after looking at the store again.
    ///
    /// [`Self::renew`] does not look at gates. A fetch of several pages
    /// checks this before each request after the first: a gate another
    /// pane closed mid-fetch stays unseen until the store's version is
    /// read again, because a key already read at the version [`follow`]
    /// noted is not read again.
    pub fn closed(&mut self, bucket: &str) -> Option<(f64, String)> {
        follow(self.cache);
        gate(self.cache, bucket)
    }

    /// Hold `value` for this pane's `ttl` and share it, stamped now - when
    /// the answer arrived. Returns why it stayed with this pane, if it did.
    pub fn publish(mut self, value: serde_json::Value) -> Option<&'static str> {
        self.done = true;
        self.cache.fails.remove(&self.key);
        let at = crate::now();
        self.cache.good.insert(self.key.clone(), (at, value.clone()));
        settle(self.cache, &self.key, at, Some(value), self.policy.ttl, self.claimed)
    }

    /// The service said no. Every pane waits until `until` when the
    /// service named a time (a rate limit's reset), else for the doubling
    /// backoff, never longer than this reading's own `ttl` would allow.
    pub fn refuse(mut self, why: &str, until: Option<f64>) {
        self.done = true;
        let at = crate::now();
        let n = {
            let n = self.cache.fails.entry(self.key.clone()).or_insert(0);
            *n += 1;
            *n
        };
        let held = match until {
            Some(u) if u > at => u - at,
            _ => backoff(n).min(self.policy.ttl.max(BACKOFF_FROM)),
        };
        let value = refusal_value(why, at + held);
        settle(self.cache, &self.key, at, Some(value), held, self.claimed);
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        if self.done || self.claimed.is_none() {
            return;
        }
        // Not published: an error, a panic unwinding, a pass cut short.
        // Give the lease back so the next pane need not wait it out.
        if let Some(store) = self.cache.store.as_mut() {
            let _ = store.release(&self.key);
        }
    }
}

fn refusal_value(why: &str, until: f64) -> serde_json::Value {
    serde_json::json!({"opscope_refusal": {"why": why, "until": until}})
}

/// A refusal `Ticket::refuse` stored: why, and until when no pane asks.
pub fn refusal(value: &serde_json::Value) -> Option<(String, f64)> {
    let r = value.get("opscope_refusal")?;
    Some((r["why"].as_str().unwrap_or("").to_string(), r["until"].as_f64().unwrap_or(0.0)))
}

/// Put a fetched reading (or refusal) in this pane and in the store.
/// `claimed_at` is when this pane claimed the lease for it, if it did.
fn settle(
    cache: &mut Cache,
    key: &str,
    at: f64,
    value: Option<serde_json::Value>,
    held: f64,
    claimed_at: Option<f64>,
) -> Option<&'static str> {
    cache.live.insert(key.to_string(), (at, value.clone(), held));
    let store = cache.store.as_mut()?;
    let s = store::Shared {
        taken_at: at,
        held,
        value,
        fails: cache.fails.get(key).copied().unwrap_or(0),
        good: cache.good.get(key).cloned(),
        fetched_by: store.me,
    };
    let me = store.me;
    match store.publish_since(key, &s, &cache.secrets, at, claimed_at) {
        Ok(done) => {
            cache.shared.insert(key.to_string(), (at, me));
            note_unshared(cache, key, done);
            done.unshared()
        }
        Err(why) if store::busy(&why) => None,
        Err(why) => {
            stop_sharing(cache, &why);
            None
        }
    }
}

// ── claims and answers a pass at a time (OPS-298) ───────────────────────

/// What came of asking for one reading in a pass that asks for many.
pub enum Claimed {
    /// Show this: fresh here, or just published by another pane.
    Serve(Option<serde_json::Value>),
    /// Fetch it: this pane holds the lease; `renew` before each request,
    /// then answer it in [`answer_many`].
    Fetch,
    /// Another pane is fetching it (`by`, 0 when unknown), or the store was
    /// busy. Show what this pane has.
    Waiting { by: u32, have: Option<serde_json::Value> },
}

/// Claim every reading a pass needs, in one transaction and one commit.
///
/// The batched counterpart of [`take`], for a widget that fetches many
/// readings per pass: twenty-one commits a minute cost more than the data
/// (OPS-298). Nothing here waits on another pane.
pub fn claim_many(cache: &mut Cache, asks: &[(&str, Policy)], now: f64) -> Vec<Claimed> {
    let at = now;
    for (key, p) in asks {
        pull(cache, *key, at, p.ttl);
    }
    let mut out: Vec<Claimed> = Vec::with_capacity(asks.len());
    let mut claims: Vec<(String, Option<f64>, usize)> = Vec::new();
    for (i, (key, p)) in asks.iter().enumerate() {
        let have = cache.live.get(*key).and_then(|e| e.1.clone());
        if let Some((when, value, held)) = cache.live.get(*key) {
            let asked = p.askable
                && is_reading(value)
                && *when < cache.asked_at
                && cache.asked_at - when >= ASKED_FLOOR.min(p.ttl);
            if at - when < *held && !asked {
                cache.asking.remove(*key);
                out.push(Claimed::Serve(value.clone()));
                continue;
            }
        }
        let known = cache.live.get(*key).map(|e| e.0).or_else(|| cache.shared.get(*key).map(|s| s.0));
        claims.push(((*key).to_string(), known, i));
        out.push(Claimed::Waiting { by: 0, have });
    }
    if claims.is_empty() {
        // Every reading was already this pane's and fresh: no claim, no
        // transaction, no write. A pane reading its neighbours' answers
        // costs nothing (OPS-298).
        return out;
    }
    let Some(store) = cache.store.as_mut() else {
        // No store: every reading still to get is this pane's to fetch.
        for (_, _, i) in claims {
            out[i] = Claimed::Fetch;
        }
        return out;
    };
    let lease_for = asks.iter().map(|(_, p)| p.lease_for).fold(0.0f64, f64::max);
    let borrows: Vec<(&str, Option<f64>)> = claims.iter().map(|(k, known, _)| (k.as_str(), *known)).collect();
    let reached = store.claim_many(&borrows, at, lease_for);
    drop(borrows);
    match reached {
        Ok(gots) => {
            for (got, (key, _, i)) in gots.into_iter().zip(claims) {
                out[i] = match got {
                    store::Got::Mine => Claimed::Fetch,
                    store::Got::Fresh(shared) => {
                        adopt_row(cache, &key, shared, at, asks[i].1.ttl);
                        cache.asking.remove(&key);
                        Claimed::Serve(cache.live.get(&key).and_then(|e| e.1.clone()))
                    }
                    store::Got::Held(by) => {
                        cache.asking.insert(key.clone(), by);
                        Claimed::Waiting { by, have: cache.live.get(&key).and_then(|e| e.1.clone()) }
                    }
                };
            }
        }
        Err(why) if store::busy(&why) => {}
        Err(why) => stop_sharing(cache, &why),
    }
    out
}

/// What a pass fetched for one reading.
pub enum Answer {
    /// The reading, taken now.
    Value(serde_json::Value),
    /// The service said no; `until` is when it said to ask again, if it said.
    Refused { why: String, until: Option<f64> },
}

impl Answer {
    /// A refusal with no time named: the doubling backoff.
    pub fn refused(why: impl Into<String>) -> Answer {
        Answer::Refused { why: why.into(), until: None }
    }
}

/// Publish what a pass fetched, in one transaction and one commit: the
/// other half of [`claim_many`]. Returns, per answer, why it stayed with
/// this pane when it did.
///
/// Each answer carries the time its own fetch finished, not the time the
/// batch is published: a repository fetched at the start of a long pass
/// is not as fresh as one fetched at the end. `claimed_at` is when this
/// pane claimed the leases. An answer whose claim was superseded is not
/// written. An empty batch writes nothing.
pub fn answer_many(
    cache: &mut Cache,
    done: &[(&str, Answer, Policy, f64)],
    now: f64,
    claimed_at: f64,
) -> Vec<Option<&'static str>> {
    if done.is_empty() {
        return Vec::new();
    }
    let mut shared = Vec::with_capacity(done.len());
    for (key, answer, p, at) in done {
        let (value, held) = match answer {
            Answer::Value(v) => {
                cache.fails.remove(*key);
                cache.good.insert((*key).to_string(), (*at, v.clone()));
                (Some(v.clone()), p.ttl)
            }
            Answer::Refused { why, until } => {
                let n = {
                    let n = cache.fails.entry((*key).to_string()).or_insert(0);
                    *n += 1;
                    *n
                };
                let held = match until {
                    Some(u) if *u > *at => *u - *at,
                    _ => backoff(n).min(p.ttl.max(BACKOFF_FROM)),
                };
                (Some(refusal_value(why, *at + held)), held)
            }
        };
        cache.live.insert((*key).to_string(), (*at, value.clone(), held));
        shared.push((
            (*key).to_string(),
            store::Shared {
                taken_at: *at,
                held,
                value,
                fails: cache.fails.get(*key).copied().unwrap_or(0),
                good: cache.good.get(*key).cloned(),
                fetched_by: 0,
            },
        ));
    }
    let mut out = vec![None; done.len()];
    let Some(store) = cache.store.as_mut() else {
        return out;
    };
    let me = store.me;
    for (_, s) in shared.iter_mut() {
        s.fetched_by = me;
    }
    let items: Vec<(&str, store::Shared)> = shared.iter().map(|(k, s)| (k.as_str(), s.clone())).collect();
    match store.publish_many(&items, &cache.secrets, now, Some(claimed_at)) {
        Ok(results) => {
            for (i, done) in results.into_iter().enumerate() {
                let key = &items[i].0;
                cache.shared.insert((*key).to_string(), (items[i].1.taken_at, me));
                out[i] = done.unshared();
                match done.unshared() {
                    Some(why) => {
                        cache.unshared.insert((*key).to_string(), why);
                    }
                    None => {
                        cache.unshared.remove(*key);
                    }
                }
                if done == store::Publish::TakenOver {
                    // Another pane holds it now: its answer is the shared one.
                    out[i] = Some("not shared: another pane took over");
                }
            }
        }
        Err(why) if store::busy(&why) => {}
        Err(why) => stop_sharing(cache, &why),
    }
    out
}

/// Hold `key`'s lease for another `lease_for` seconds, before the next
/// request of a long fetch. `false` when another pane took it over.
pub fn renew(cache: &mut Cache, key: &str, lease_for: f64) -> bool {
    let Some(store) = cache.store.as_mut() else {
        return true;
    };
    match store.renew(key, crate::now(), lease_for) {
        Ok(mine) => mine,
        Err(why) if store::busy(&why) => true,
        Err(why) => {
            stop_sharing(cache, &why);
            true
        }
    }
}

/// Extend every lease this pane still holds, and say whether `key` is one
/// of them. A pass that claimed many readings renews them together before
/// each request, so an answer already fetched stays owned until it is
/// published. `false` when another pane took `key` over. A busy store
/// counts as still holding it.
pub fn renew_all(cache: &mut Cache, key: &str, lease_for: f64) -> bool {
    let Some(store) = cache.store.as_mut() else {
        return true;
    };
    match store.renew_all(crate::now(), lease_for, key) {
        Ok(mine) => mine,
        Err(why) if store::busy(&why) => true,
        Err(why) => {
            stop_sharing(cache, &why);
            true
        }
    }
}

/// Give `key`'s lease back without publishing: the pass stopped short.
pub fn release(cache: &mut Cache, key: &str) {
    if let Some(store) = cache.store.as_mut() {
        let _ = store.release(key);
    }
}

/// Whether this widget's rate limit is spent, and until when: the record
/// [`shut`] left under `bucket`, by this pane or another.
pub fn gate(cache: &mut Cache, bucket: &str) -> Option<(f64, String)> {
    let key = format!("gate/{bucket}");
    let at = crate::now();
    pull(cache, &key, at, 0.0);
    publish_pending_gate(cache, &key, at);
    let (_, value, _) = cache.live.get(&key)?;
    let (why, until) = refusal(value.as_ref()?)?;
    (until > at).then_some((until, why))
}

/// Share a gate [`shut`] could not write - the store was busy, or not
/// open yet - so the other panes stop asking too, rather than only this
/// one. A gate taken from the store, or already written, is left alone.
fn publish_pending_gate(cache: &mut Cache, key: &str, at: f64) {
    let Some((taken_at, Some(value), held)) = cache.live.get(key).cloned() else {
        return;
    };
    let written = cache.shared.get(key).is_some_and(|s| s.0 == taken_at);
    let lifted = refusal(&value).is_none_or(|(_, until)| until <= at);
    if cache.store.is_some() && !written && !lifted {
        settle(cache, key, taken_at, Some(value), held, None);
    }
}

/// Stop every pane of this widget asking `bucket` until `until`: the
/// service said its limit is spent and when it resets. A store busy at
/// that moment gets the gate on this pane's next [`gate`].
pub fn shut(cache: &mut Cache, bucket: &str, until: f64, why: &str) {
    let key = format!("gate/{bucket}");
    let at = crate::now();
    let held = (until - at).max(0.0);
    settle(cache, &key, at, Some(refusal_value(why, until)), held, None);
}

/// Where the reading on screen came from, and how fresh it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Provenance {
    pub taken_at: f64,
    /// The pane that fetched it, or `None` for this one.
    pub by: Option<u32>,
    /// Past this pane's hold.
    pub stale: bool,
    /// Another pane is fetching it now.
    pub asking: Option<u32>,
    /// A refusal every pane is waiting out: why, and until when.
    pub refused: Option<(String, f64)>,
    /// Why this pane's own reading was not shared.
    pub unshared: Option<&'static str>,
}

/// Where `key`'s reading came from, as of `now`; `None` before there is one.
pub fn provenance(cache: &Cache, key: &str, now: f64) -> Option<Provenance> {
    let (taken_at, value, held) = cache.live.get(key)?;
    let me = cache.store.as_ref().map(|s| s.me);
    // Who fetched the reading on screen, and only that one: a reading this
    // pane fetched but could not share leaves the previous fetcher's entry.
    let by = cache
        .shared
        .get(key)
        .filter(|s| s.0 == *taken_at)
        .map(|s| s.1)
        .filter(|by| Some(*by) != me && *by != 0);
    // `cached`'s refusals carry only their reason, `{"why": ...}`; they
    // are held until the backoff `cached` set runs out.
    let refusal_held = value
        .as_ref()
        .and_then(|v| refusal(v).or_else(|| reason_only(v, taken_at + held)));
    // A refusal, or an empty answer a reader covers with its last good
    // value, says how old the reading still on screen is - not when this
    // request failed. `is_reading` covers both the explicit refusal shapes.
    let taken_at = match (is_reading(value), cache.good.get(key)) {
        (false, Some((at, _))) => *at,
        _ => *taken_at,
    };
    let refused = refusal_held.filter(|(_, until)| *until > now);
    Some(Provenance {
        taken_at,
        by,
        stale: now - taken_at >= *held,
        asking: cache.asking.get(key).copied(),
        refused,
        unshared: cache.unshared.get(key).copied(),
    })
}

/// A refusal [`cached`] holds - a value with a `why`, as [`is_reading`]
/// tells them apart - held until `until`.
fn reason_only(value: &serde_json::Value, until: f64) -> Option<(String, f64)> {
    let why = value.get("why")?.as_str().filter(|w| !w.is_empty())?;
    Some((why.to_string(), until))
}

/// A duration as a pane says it: `40s`, `7m`, `2h05m`, `3d`.
fn ago(seconds: f64) -> String {
    let s = seconds.max(0.0) as i64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d", s / 86400),
    }
}

/// The pane's one line about sharing, wrapped to `width`: why this pane is
/// not sharing, when it meant to and cannot, else [`provenance_rows`]. Drawn
/// where the freshness line goes, so a pane polling alone says so in the
/// same place another says who fetched its numbers (OPS-282). A store
/// started again or emptied says so there too, until the next [`share`].
pub fn sharing_rows(cache: &Cache, found: &[Provenance], now: f64, width: usize) -> Vec<String> {
    if cache.store.is_none() && !cache.sharing_off.is_empty() {
        return crate::wrap_words(&format!("not sharing readings: {}", cache.sharing_off), width.max(1));
    }
    let mut rows = provenance_rows(found, now, width);
    if let Some(notice) = cache.store.as_ref().map(|s| s.notice.as_str()).filter(|n| !n.is_empty()) {
        rows.extend(crate::wrap_words(notice, width.max(1)));
        cache.notice_drawn.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    rows
}

/// The one wording every widget gives for where its readings came from,
/// wrapped to `width`, from the readings on screen: nothing when every
/// one was fetched by this pane and is fresh, else the oldest of them, and
/// what is happening to it.
///
/// `fetched 2m ago by pane 48213`, `cached 7m ago · pane 48213 asking now`,
/// `cached 7m ago · too many requests, next ask in 3m`.
pub fn provenance_rows(found: &[Provenance], now: f64, width: usize) -> Vec<String> {
    let Some(oldest) = found.iter().min_by(|a, b| a.taken_at.total_cmp(&b.taken_at)) else {
        return Vec::new();
    };
    let asking = found.iter().find_map(|p| p.asking);
    let refused = found
        .iter()
        .filter_map(|p| p.refused.clone())
        .max_by(|a, b| a.1.total_cmp(&b.1));
    let unshared = found.iter().find_map(|p| p.unshared);
    let other = found.iter().find_map(|p| p.by);
    let stale = found.iter().any(|p| p.stale);
    if asking.is_none() && refused.is_none() && unshared.is_none() && other.is_none() && !stale {
        return Vec::new();
    }
    let age = ago(now - oldest.taken_at);
    let mut text = if refused.is_some() || asking.is_some() || stale {
        format!("cached {age} ago")
    } else {
        // The oldest reading's own fetcher: another pane's newer reading
        // must not lend it a pane that never fetched it.
        match oldest.by {
            Some(pid) => format!("fetched {age} ago by pane {pid}"),
            None => format!("fetched {age} ago by this pane"),
        }
    };
    if let Some(pid) = asking.filter(|p| *p != 0) {
        text.push_str(&format!(" · pane {pid} asking now"));
    }
    if let Some((why, until)) = refused {
        text.push_str(&format!(" · {why}, next ask in {}", ago(until - now)));
    }
    if let Some(why) = unshared {
        text.push_str(&format!(" · {why}"));
    }
    crate::wrap_words(&text, width.max(1))
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

    /// `cached`, agent-usage's call, says where its readings came from in
    /// the same words as `take`: who fetched it, who is asking now, and a
    /// refusal held in its older `{"why": ...}` shape (OPS-290).
    #[test]
    fn a_reading_held_by_cached_says_where_it_came_from() {
        let (mut a, mut b) = two_panes("cached-provenance");
        let rows = |c: &Cache, key: &str| {
            let found: Vec<_> = provenance(c, key, now()).into_iter().collect();
            provenance_rows(&found, now(), 80).join(" ")
        };
        // This pane's own, and fresh: nothing to say.
        assert!(cached(&mut a, "k", 120.0, || Some(serde_json::json!({"u": 1}))).is_some());
        assert_eq!(rows(&a, "k"), "");
        // Another pane's.
        assert!(cached(&mut b, "k", 120.0, || panic!("asked a fresh shared reading")).is_some());
        let said = rows(&b, "k");
        assert!(said.starts_with("fetched ") && said.ends_with(" ago by pane 41"), "{said}");

        // Past its hold here, while pane 42 holds the lease: shown as it
        // is, and said to be on its way.
        let t = a.live["k"].0 - 500.0;
        a.live.get_mut("k").unwrap().0 = t;
        a.shared.get_mut("k").unwrap().0 = t;
        let mut store = b.store.take().unwrap();
        rusqlite::Connection::open(store.path_for_tests())
            .unwrap()
            .execute("UPDATE readings SET taken_at = ?1 WHERE key = 'k'", [t])
            .unwrap();
        assert_eq!(store.claim("k", Some(t), now()).unwrap(), Got::Mine);
        assert!(cached(&mut a, "k", 120.0, || panic!("asked while pane 42 was asking")).is_some());
        assert_eq!(rows(&a, "k"), "cached 8m ago · pane 42 asking now");
        store.release("k").unwrap();

        // Refused, with the reason written in after: every pane waits it
        // out, and says when the next ask is.
        let mut alone = Cache::default();
        assert!(cached(&mut alone, "r", 120.0, || Some(serde_json::json!({"u": 2}))).is_some());
        alone.live.get_mut("r").unwrap().0 -= 420.0;
        alone.good.get_mut("r").unwrap().0 -= 420.0;
        assert!(cached(&mut alone, "r", 120.0, || None).is_none());
        remember_refusal(&mut alone, "r", "too many requests");
        assert_eq!(rows(&alone, "r"), "cached 7m ago · too many requests, next ask in 1m");
    }

    #[test]
    fn a_pane_that_took_the_lease_after_waiting_does_not_write_over_its_successor() {
        let (mut a, mut b) = two_panes("wait-late");
        let path = a.store.as_ref().unwrap().path_for_tests();
        let mut store = a.store.take().unwrap();
        assert_eq!(store.claim("k", None, now()).unwrap(), Got::Mine);
        // The holder gives the lease back without a reading, so b takes it.
        let giving = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(700));
            store.release("k").unwrap();
        });
        let got = cached(&mut b, "k", 120.0, || {
            // b's fetch outlives its lease; pane 43 takes over and answers.
            let raw = rusqlite::Connection::open(&path).unwrap();
            raw.execute("DELETE FROM leases WHERE key = 'k'", []).unwrap();
            let mut c = Store::open(&path).unwrap();
            c.me = 43;
            let at = now();
            let s = Shared { taken_at: at, held: 120.0, value: Some(serde_json::json!(2)), fails: 0, good: None, fetched_by: 43 };
            assert_eq!(c.publish("k", &s, &[], at).unwrap(), Publish::Written);
            Some(serde_json::json!(1))
        });
        giving.join().unwrap();
        assert_eq!(got, Some(serde_json::json!(1)));
        let mut other = Store::open(&path).unwrap();
        assert_eq!(other.load("k").unwrap().unwrap().value, Some(serde_json::json!(2)), "the late answer wrote over the successor's");
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

    // ── OPS-284 ──

    const P: Policy = Policy { ttl: 120.0, lease_for: 30.0, askable: true };

    #[test]
    fn a_fetch_published_by_one_pane_is_served_to_the_next_and_says_who() {
        let (mut a, mut b) = two_panes("take");
        match take(&mut a, "k", P) {
            Take::Fetch(t) => assert_eq!(t.publish(serde_json::json!({"n": 1})), None),
            _ => panic!("the first pane did not fetch"),
        }
        follow(&mut b);
        match take(&mut b, "k", P) {
            Take::Serve(v) => assert_eq!(v, Some(serde_json::json!({"n": 1}))),
            _ => panic!("the second pane did not take the published reading"),
        }
        let now = crate::now();
        let p = provenance(&b, "k", now).unwrap();
        assert_eq!(p.by, Some(41));
        assert_eq!(provenance_rows(&[p], now, 80), ["fetched 0s ago by pane 41"]);
        // The pane that fetched it says nothing: fresh, and its own.
        let own = provenance(&a, "k", now).unwrap();
        assert!(provenance_rows(&[own], now, 80).is_empty());
    }

    #[test]
    fn a_pane_never_waits_for_another_panes_long_fetch() {
        let (mut a, mut b) = two_panes("waiting");
        let Take::Fetch(mut ticket) = take(&mut a, "k", P) else { panic!("did not fetch") };
        let started = std::time::Instant::now();
        match take(&mut b, "k", P) {
            Take::Waiting { by, have } => {
                assert_eq!((by, have), (41, None));
            }
            _ => panic!("did not wait for the pane holding the lease"),
        }
        assert!(started.elapsed() < std::time::Duration::from_millis(400), "it blocked");
        assert!(ticket.renew(), "the holder lost its own lease");
        drop(ticket);
        // Given back unpublished, the lease is free for the next pane.
        assert!(matches!(take(&mut b, "k", P), Take::Fetch(_)));
        let p = provenance(&b, "k", crate::now());
        assert!(p.is_none(), "a reading appeared from nowhere");
    }

    #[test]
    fn a_refusal_with_a_reset_time_holds_every_pane_until_then() {
        let (mut a, mut b) = two_panes("refuse");
        let until = crate::now() + 600.0;
        let Take::Fetch(t) = take(&mut a, "k", P) else { panic!("did not fetch") };
        t.refuse("too many requests", Some(until));
        follow(&mut b);
        let Take::Serve(Some(v)) = take(&mut b, "k", P) else { panic!("asked inside the reset") };
        let (why, at) = refusal(&v).unwrap();
        assert_eq!(why, "too many requests");
        assert!((at - until).abs() < 1.0);
        let now = crate::now();
        let rows = provenance_rows(&[provenance(&b, "k", now).unwrap()], now, 80);
        assert!(rows[0].contains("too many requests, next ask in 9m"), "{rows:?}");
    }

    #[test]
    fn a_spent_rate_limit_shuts_every_pane_of_the_widget() {
        let (mut a, mut b) = two_panes("gate");
        assert_eq!(gate(&mut b, "core"), None);
        shut(&mut a, "core", crate::now() + 300.0, "rate limit spent");
        follow(&mut b);
        let (until, why) = gate(&mut b, "core").expect("the other pane did not see the limit");
        assert_eq!(why, "rate limit spent");
        assert!(until > crate::now() + 290.0);
    }

    #[test]
    fn a_gate_written_while_the_store_was_busy_reaches_the_other_panes() {
        let (mut a, mut b) = two_panes("gate-busy");
        let path = a.store.as_ref().unwrap().path_for_tests();
        let other = rusqlite::Connection::open(&path).unwrap();
        other.execute_batch("BEGIN EXCLUSIVE").unwrap();
        shut(&mut a, "core", crate::now() + 300.0, "rate limit spent");
        other.execute_batch("ROLLBACK").unwrap();
        follow(&mut b);
        assert_eq!(gate(&mut b, "core"), None, "the gate was written through an exclusive lock");
        assert!(gate(&mut a, "core").is_some());
        follow(&mut b);
        assert!(gate(&mut b, "core").is_some(), "a gate that met a busy store never reached the other pane");
    }

    #[test]
    fn a_detail_added_while_the_store_was_busy_is_kept_and_shared() {
        let (mut a, mut b) = two_panes("annotate-busy");
        let Take::Fetch(t) = take(&mut a, "k", P) else { panic!("did not fetch") };
        t.refuse("too many requests", Some(crate::now() + 600.0));
        let mut noted = a.live.get("k").unwrap().1.clone().unwrap();
        noted["bank"] = serde_json::json!(7);
        let path = a.store.as_ref().unwrap().path_for_tests();
        let other = rusqlite::Connection::open(&path).unwrap();
        other.execute_batch("BEGIN EXCLUSIVE").unwrap();
        set_live(&mut a, "k", noted);
        other.execute_batch("ROLLBACK").unwrap();
        // Another pane writes, so this one reads the row again.
        shut(&mut b, "core", crate::now() + 300.0, "rate limit spent");
        follow(&mut a);
        let Take::Serve(Some(v)) = take(&mut a, "k", P) else { panic!("asked inside the reset") };
        assert_eq!(v["bank"], 7, "the stored copy wrote over this pane's own detail");
        follow(&mut b);
        let Take::Serve(Some(v)) = take(&mut b, "k", P) else { panic!("asked inside the reset") };
        assert_eq!(v["bank"], 7, "the detail never reached the other pane");
    }

    #[test]
    fn a_gate_read_while_the_store_was_busy_is_read_again() {
        let (mut a, mut b) = two_panes("gate-reread");
        shut(&mut a, "core", crate::now() + 300.0, "rate limit spent");
        let path = a.store.as_ref().unwrap().path_for_tests();
        let other = rusqlite::Connection::open(&path).unwrap();
        other.execute_batch("BEGIN EXCLUSIVE").unwrap();
        follow(&mut b);
        assert_eq!(gate(&mut b, "core"), None);
        other.execute_batch("ROLLBACK").unwrap();
        follow(&mut b);
        assert!(gate(&mut b, "core").is_some(), "a busy read marked the gate as seen");
    }

    #[test]
    fn a_lifted_refusal_still_dates_the_reading_on_screen() {
        let mut c = Cache::default();
        let now = 10_000.0;
        c.good.insert("k".into(), (now - 3600.0, serde_json::json!(1)));
        c.live.insert("k".into(), (now - 121.0, Some(refusal_value("too many requests", now - 1.0)), 120.0));
        let p = provenance(&c, "k", now).unwrap();
        assert_eq!(p.taken_at, now - 3600.0);
        assert_eq!(p.refused, None);
    }

    #[test]
    fn an_empty_refresh_dates_the_good_reading_still_on_screen() {
        let mut c = Cache::default();
        assert!(cached(&mut c, "plan", 3600.0, || Some(serde_json::json!({"plan": "fixture"}))).is_some());
        c.good.get_mut("plan").unwrap().0 = crate::now() - 7200.0;
        c.live.get_mut("plan").unwrap().0 = crate::now() - 3700.0;
        assert!(cached(&mut c, "plan", 3600.0, || None).is_none());
        let at = crate::now();
        let p = provenance(&c, "plan", at).unwrap();
        assert_eq!(p.taken_at, c.good["plan"].0);
        assert!(p.stale);
        assert_eq!(provenance_rows(&[p], at, 80), ["cached 2h00m ago"]);
    }

    #[test]
    fn a_reading_this_pane_could_not_share_is_not_credited_to_the_last_fetcher() {
        let mut c = Cache::default();
        let now = 10_000.0;
        c.shared.insert("k".into(), (now - 500.0, 41));
        c.live.insert("k".into(), (now, Some(serde_json::json!(2)), 300.0));
        assert_eq!(provenance(&c, "k", now).unwrap().by, None);
        c.live.insert("k".into(), (now - 500.0, Some(serde_json::json!(1)), 300.0));
        assert_eq!(provenance(&c, "k", now).unwrap().by, Some(41));
    }

    #[test]
    fn the_oldest_reading_is_dated_by_the_pane_that_fetched_it() {
        let now = 10_000.0;
        let mine = Provenance { taken_at: now - 300.0, by: None, stale: false, asking: None, refused: None, unshared: None };
        let theirs = Provenance { taken_at: now - 10.0, by: Some(42), ..mine.clone() };
        assert_eq!(provenance_rows(&[mine, theirs], now, 80), ["fetched 5m ago by this pane"]);
    }

    #[test]
    fn a_colon_in_a_plain_part_cannot_make_two_readings_one_key() {
        let mut c = Cache::default();
        assert_ne!(key(&mut c, "u", 1, &["a:b", "c"], &[]), key(&mut c, "u", 1, &["a", "b:c"], &[]));
        assert_ne!(key(&mut c, "u", 1, &["a%3Ab"], &[]), key(&mut c, "u", 1, &["a:b"], &[]));
        assert_eq!(key(&mut c, "u", 1, &["host:8080"], &[]), "u/1:host%3A8080");
    }

    #[test]
    fn a_plain_part_equal_to_a_fingerprint_is_not_that_fingerprint() {
        let mut c = Cache::default();
        let token = "fixture-token-0123456789";
        let print = fingerprint(&[token]);
        assert_ne!(key(&mut c, "u", 1, &[], &[token]), key(&mut c, "u", 1, &[&print], &[]));
    }

    #[test]
    fn sharing_turned_off_and_on_again_opens_the_store_at_once() {
        use crate::statefile::tests::{env_lock, temp_dir, EnvVar};
        let _guard = env_lock();
        let dir = temp_dir("share-toggle");
        let _xdg = EnvVar::set("XDG_STATE_HOME", Some(dir.as_os_str()));
        let mut c = Cache::default();
        share(&mut c, "toggle-test", store::Caps::default(), true, 1_000.0);
        assert!(c.store.is_some(), "{}", c.sharing_off);
        share(&mut c, "toggle-test", store::Caps::default(), false, 1_010.0);
        assert!(c.store.is_none());
        share(&mut c, "toggle-test", store::Caps::default(), true, 1_020.0);
        assert!(c.store.is_some(), "turned back on, still not sharing: {}", c.sharing_off);
    }

    #[test]
    fn a_shared_replacement_drops_the_reason_this_panes_reading_stayed_here() {
        let (mut a, mut b) = two_panes("unshared");
        let path = a.store.as_ref().unwrap().path_for_tests();
        let mut small = Store::open_with(&path, store::Caps { max_value: 8, ..store::Caps::default() }).unwrap();
        small.me = 41;
        a.store = Some(small);
        let Take::Fetch(t) = take(&mut a, "k", P) else { panic!("a did not fetch") };
        assert_eq!(t.publish(serde_json::json!("far too large to share")), Some("too large to share"));
        let at = crate::now() + 1.0;
        settle(&mut b, "k", at, Some(serde_json::json!(1)), 300.0, None);
        follow(&mut a);
        pull(&mut a, "k", at, 300.0);
        let p = provenance(&a, "k", at).unwrap();
        assert_eq!(p.by, Some(42));
        assert_eq!(p.unshared, None, "a reading pane 42 shared still said it was too large to share");
    }

    #[test]
    fn a_gate_reaches_a_store_started_again_under_it() {
        use crate::statefile::tests::{env_lock, temp_dir, EnvVar};
        let _guard = env_lock();
        let dir = temp_dir("gate-replaced");
        let _xdg = EnvVar::set("XDG_STATE_HOME", Some(dir.as_os_str()));
        let mut a = Cache::default();
        share(&mut a, "gate-replaced", store::Caps::default(), true, 1_000.0);
        shut(&mut a, "core", crate::now() + 300.0, "rate limit spent");
        let path = store::path("gate-replaced");
        std::fs::remove_file(&path).unwrap();
        let _ = Store::open(&path).unwrap();
        share(&mut a, "gate-replaced", store::Caps::default(), true, 1_010.0);
        assert!(gate(&mut a, "core").is_some());
        let mut b = Store::open(&path).unwrap();
        assert!(b.load("gate/core").unwrap().is_some(), "the gate never reached the store started again");
    }

    #[test]
    fn a_store_notice_is_shown_for_one_pass() {
        let (mut a, _b) = two_panes("notice");
        a.store.as_mut().unwrap().notice = "shared store started again: damaged".into();
        clear_drawn_notice(&mut a);
        assert!(sharing_rows(&a, &[], 0.0, 80).iter().any(|r| r.contains("started again")), "cleared before it was drawn");
        // The next pass's `share` clears what this one drew.
        clear_drawn_notice(&mut a);
        assert!(sharing_rows(&a, &[], 0.0, 80).is_empty(), "the notice came back on the next pass");
    }

    #[test]
    fn a_key_carries_a_fingerprint_of_a_token_and_never_the_token() {
        let mut c = Cache::default();
        let token = "ghp_fixture0123456789abcdef";
        let k = key(&mut c, "gha.runs", 1, &["github.com", "acme/app"], &[token]);
        assert!(k.starts_with("gha.runs/1:github.com:acme/app:"), "{k}");
        assert!(!k.contains(token));
        assert!(c.secrets.iter().any(|s| s == token), "the token was not registered");
        assert_eq!(fingerprint(&[token]).len(), 32);
        assert_eq!(fingerprint(&[token]), fingerprint(&[token]));
        assert_ne!(fingerprint(&["a", "bc"]), fingerprint(&["ab", "c"]));
        assert_eq!(key(&mut c, "linear.board", 2, &[], &[]), "linear.board/2");
    }

    #[test]
    fn a_busy_store_is_not_this_pass_and_sharing_goes_on() {
        let (mut a, _b) = two_panes("busy");
        let path = a.store.as_ref().unwrap().path_for_tests();
        let other = rusqlite::Connection::open(&path).unwrap();
        other.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert!(matches!(take(&mut a, "k", P), Take::Waiting { by: 0, .. }));
        let _ = cached(&mut a, "j", 120.0, || Some(serde_json::json!(1)));
        other.execute_batch("ROLLBACK").unwrap();
        assert!(a.store.is_some(), "one busy moment stopped sharing: {}", a.sharing_off);
    }

    #[test]
    fn a_reading_is_stamped_when_its_answer_arrives() {
        let mut c = Cache::default();
        let before = crate::now();
        cached(&mut c, "slow", 1.0, || {
            std::thread::sleep(std::time::Duration::from_millis(1100));
            Some(serde_json::json!(1))
        });
        assert!(c.live["slow"].0 >= before + 1.0, "stamped when it was asked for");
        // A fetch slower than its hold still lands fresh.
        cached(&mut c, "slow", 1.0, || panic!("asked again for a reading that just arrived"));
    }

    #[test]
    fn provenance_rows_fit_every_width() {
        let now = 10_000.0;
        let p = Provenance {
            taken_at: now - 420.0,
            by: Some(48213),
            stale: true,
            asking: Some(48213),
            refused: Some(("too many requests".into(), now + 180.0)),
            unshared: Some("too large to share"),
        };
        let wide = provenance_rows(std::slice::from_ref(&p), now, 200);
        assert_eq!(
            wide,
            ["cached 7m ago · pane 48213 asking now · too many requests, next ask in 3m · too large to share"]
        );
        for w in 12..=120 {
            for row in provenance_rows(std::slice::from_ref(&p), now, w) {
                assert!(crate::display_width(&row) <= w, "{w}: {row:?}");
            }
        }
    }

    #[test]
    fn a_pane_polling_alone_says_why_where_the_freshness_line_goes() {
        let mut c = Cache::default();
        assert!(sharing_rows(&c, &[], 0.0, 80).is_empty(), "nothing to say, said something");
        c.sharing_off = "share_readings is off".into();
        assert_eq!(sharing_rows(&c, &[], 0.0, 80), ["not sharing readings: share_readings is off"]);
        for w in 12..=80 {
            for row in sharing_rows(&c, &[], 0.0, w) {
                assert!(crate::display_width(&row) <= w, "{w}: {row:?}");
            }
        }
        let (a, _b) = two_panes("rows");
        assert!(sharing_rows(&a, &[], 0.0, 80).is_empty(), "a pane sharing fresh readings of its own said something");
    }




    #[test]
    fn peek_reads_a_reading_without_claiming_or_fetching_it() {
        let (mut a, mut b) = two_panes("peek");
        assert_eq!(peek(&mut b, "k"), None, "a reading appeared from nowhere");
        let Take::Fetch(t) = take(&mut a, "k", P) else { panic!("did not fetch") };
        t.publish(serde_json::json!({"n": 7}));
        follow(&mut b);
        let writes = b.store.as_ref().unwrap().writes;
        assert_eq!(peek(&mut b, "k"), Some(serde_json::json!({"n": 7})));
        assert_eq!(b.store.as_ref().unwrap().writes, writes, "peek wrote to the store");
        let leases: i64 = b.store.as_ref().unwrap().lease_count_for_tests("k");
        assert_eq!(leases, 0, "peek took a lease");
    }

    // ── a pass at a time (OPS-298) ──

    fn asks<'a>(keys: &'a [String]) -> Vec<(&'a str, Policy)> {
        keys.iter().map(|k| (k.as_str(), Policy::new(60.0, 30.0))).collect()
    }

    #[test]
    fn a_pass_claims_and_answers_in_two_commits_whatever_its_size() {
        let (mut a, mut b) = two_panes("batch");
        let keys: Vec<String> = (0..8).map(|i| format!("k{i}")).collect();
        let now = crate::now();
        let claimed = claim_many(&mut a, &asks(&keys), now);
        assert!(claimed.iter().all(|c| matches!(c, Claimed::Fetch)), "a fresh store had something to serve");
        let done: Vec<(&str, Answer, Policy, f64)> = keys
            .iter()
            .map(|k| (k.as_str(), Answer::Value(serde_json::json!({"k": k})), Policy::new(60.0, 30.0), now))
            .collect();
        let unshared = answer_many(&mut a, &done, now, now);
        assert!(unshared.iter().all(Option::is_none));
        assert_eq!(a.store.as_ref().unwrap().writes, 2, "a claim and an answer commit once each, whatever the pass holds");
        // Every pane reads them all, and none of them asks.
        follow(&mut b);
        let writes = b.store.as_ref().unwrap().writes;
        let served = claim_many(&mut b, &asks(&keys), now2());
        assert!(served.iter().all(|c| matches!(c, Claimed::Serve(Some(_)))), "the second pane had to fetch");
        // One transaction to look, none to write: no lease was taken and no
        // reading answered.
        assert!(b.store.as_ref().unwrap().writes <= writes + 1);
        for k in &keys {
            assert_eq!(b.store.as_ref().unwrap().lease_count_for_tests(k), 0, "a reader took a lease on {k}");
        }
    }

    #[test]
    fn a_quiet_pass_leaves_the_reading_alone_and_only_moves_its_time() {
        let (mut a, mut b) = two_panes("quiet");
        let key = "k";
        let now = crate::now();
        let _ = claim_many(&mut a, &[(key, Policy::new(60.0, 30.0))], now);
        answer_many(&mut a, &[(key, Answer::Value(serde_json::json!({"n": 1})), Policy::new(60.0, 30.0), now)], now, now);
        let text = |c: &Cache| -> String {
            c.store
                .as_ref()
                .unwrap()
                .stored_text_for_tests(key)
        };
        let before = text(&a);
        // The same answer 40 s later: a check, not the reading.
        let later = now + 40.0;
        let _ = claim_many(&mut a, &[(key, Policy::new(60.0, 30.0))], later);
        answer_many(&mut a, &[(key, Answer::Value(serde_json::json!({"n": 1})), Policy::new(60.0, 30.0), later)], later, later);
        assert_eq!(text(&a), before, "the reading was rewritten with the same text");
        follow(&mut b);
        assert_eq!(peek(&mut b, key), Some(serde_json::json!({"n": 1})));
        // A follower asking 50 s after the last check must not refetch.
        let claimed = claim_many(&mut b, &[(key, Policy::new(60.0, 30.0))], later + 50.0);
        assert!(matches!(claimed[0], Claimed::Serve(Some(_))), "the check stamp did not hold the reading");
        // And 70 s after it, it does.
        let claimed = claim_many(&mut b, &[(key, Policy::new(60.0, 30.0))], later + 70.0);
        assert!(matches!(claimed[0], Claimed::Fetch), "a stale reading was not claimed");
    }

    #[test]
    fn a_record_the_store_refuses_stays_on_the_pane() {
        let dir = std::env::temp_dir().join(format!(
            "opscope-record-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.sqlite").to_string_lossy().to_string();
        let store = Store::open_with(&path, store::Caps { max_value: 8, ..store::Caps::default() }).unwrap();
        let mut cache = Cache { store: Some(store), ..Cache::default() };
        let v = serde_json::json!({"remaining": 1, "limit": 5000});
        record(&mut cache, "budget", v.clone(), 1_000.0, 60.0);
        assert_eq!(cache.live.get("budget").and_then(|e| e.1.clone()), Some(v.clone()));
        assert_eq!(cache.good.get("budget").map(|g| g.1.clone()), Some(v));
        assert_eq!(cache.unshared.get("budget").copied(), Some("too large to share"));
        assert!(cache.shared.get("budget").is_none(), "a refused observation was marked shared");
    }

    #[test]
    fn an_empty_answer_writes_nothing() {
        let (mut a, _) = two_panes("empty-answer");
        let writes = a.store.as_ref().unwrap().writes;
        let out = answer_many(&mut a, &[], crate::now(), crate::now());
        assert!(out.is_empty());
        assert_eq!(a.store.as_ref().unwrap().writes, writes, "an empty batch opened a transaction");
    }

    #[test]
    fn a_refusal_in_a_batch_holds_every_reading_it_came_with() {
        let (mut a, mut b) = two_panes("batch-refuse");
        let keys: Vec<String> = (0..3).map(|i| format!("k{i}")).collect();
        let now = crate::now();
        let _ = claim_many(&mut a, &asks(&keys), now);
        let until = now + 600.0;
        let done: Vec<(&str, Answer, Policy, f64)> = keys
            .iter()
            .map(|k| {
                (
                    k.as_str(),
                    Answer::Refused { why: "too many requests".into(), until: Some(until) },
                    Policy::new(60.0, 30.0),
                    now,
                )
            })
            .collect();
        answer_many(&mut a, &done, now, now);
        follow(&mut b);
        let served = claim_many(&mut b, &asks(&keys), now2());
        for got in served {
            match got {
                Claimed::Serve(Some(v)) => {
                    assert_eq!(refusal(&v).map(|(w, _)| w).as_deref(), Some("too many requests"));
                }
                _ => panic!("asked inside a batch refusal"),
            }
        }
    }

    fn now2() -> f64 {
        crate::now() + 1.0
    }
}
