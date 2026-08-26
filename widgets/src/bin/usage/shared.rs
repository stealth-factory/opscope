// terminal-toys - small dependency-free terminal widgets
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

//! What every vendor reader needs: the caches, the file walking, and the
//! one shape the summary screen understands.
//!
//! This is a toolkit rather than a caller, so an entry nobody has
//! reached for yet is not dead code - it is the next reader's.
#![allow(dead_code)]

use std::collections::HashMap;

use crate::*;

/// Readings held between passes, so a finished transcript is parsed once
/// and a quota endpoint is not asked six times a minute.
#[derive(Default)]
pub struct Caches {
    /// path -> ((mtime, size), records keyed by uuid)
    pub transcripts: HashMap<String, ((u64, u64), HashMap<String, (String, String, Tokens)>)>,
    /// key -> (when, value, ttl)
    pub live: HashMap<String, (f64, Option<serde_json::Value>, f64)>,
    /// key -> refusals in a row, which is what the backoff doubles on.
    /// Cleared the moment one gets through.
    pub fails: HashMap<String, u32>,
}

pub const LIVE_TTL: f64 = 120.0;
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
/// A plan does not change between refreshes; the windows do.
pub const PLAN_TTL: f64 = 3600.0;

/// Hold a reading for a while, but never hold a failure that long.
///
/// The pane redraws every thirty seconds; these windows move over hours. A
/// refusal is held too, so a dead endpoint is retried occasionally rather
/// than on every frame - and held longer each time it is refused again, so
/// an endpoint saying "too often" is not answered at the same rate that
/// provoked it.
pub fn cached<F>(caches: &mut Caches, key: &str, ttl: f64, fetch: F) -> Option<serde_json::Value>
where
    F: FnOnce() -> Option<serde_json::Value>,
{
    let at = now();
    if let Some((when, value, held)) = caches.live.get(key) {
        if at - when < *held {
            return value.clone();
        }
    }
    let value = fetch();
    let held = if value.is_some() {
        caches.fails.remove(key);
        ttl
    } else {
        let n = caches.fails.entry(key.to_string()).or_insert(0);
        *n += 1;
        // Never longer than the interval itself would have been, for a
        // reading asked for hourly: backing an hourly plan read off to
        // thirty minutes is fine, but it must not exceed the hour.
        backoff(*n).min(ttl.max(BACKOFF_FROM))
    };
    caches.live.insert(key.to_string(), (at, value.clone(), held));
    value
}

pub fn read_json(path: &str) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Every file under a directory whose name ends in `suffix`.
///
/// Recursive on purpose: several agents nest their transcripts two or three
/// levels down, and globbing one level deep silently missed most of them.
pub fn walk(dir: &str, suffix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.to_string_lossy().to_string();
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&name, suffix, out),
            Ok(t) if t.is_file() && name.ends_with(suffix) => out.push(name),
            _ => {}
        }
    }
}

/// Newest first, by modification time.
pub fn newest_first(paths: Vec<String>) -> Vec<String> {
    use std::os::unix::fs::MetadataExt;
    let mut with_time: Vec<(u64, String)> = paths
        .into_iter()
        .filter_map(|path| {
            let meta = std::fs::metadata(&path).ok()?;
            Some((meta.mtime() as u64, path))
        })
        .collect();
    with_time.sort_by(|a, b| b.0.cmp(&a.0));
    with_time.into_iter().map(|(_, p)| p).collect()
}

/// The last `size` bytes of a file, as lines.
///
/// A rollout carries its running total on every token_count event, so the
/// newest one is all that is needed - reading thirty megabytes per refresh
/// to learn a number that is repeated at the end would be daft.
pub fn tail_lines(path: &str, size: u64) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let end = f.seek(SeekFrom::End(0)).unwrap_or(0);
    if f.seek(SeekFrom::Start(end.saturating_sub(size))).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    String::from_utf8_lossy(&buf)
        .split('\n')
        .map(String::from)
        .collect()
}

/// Enough to reach the last running total in a rollout.
pub const TAIL: u64 = 256 * 1024;

/// One quota an agent publishes, flattened to the four numbers the summary
/// screen can compare across agents.
///
/// Read as data rather than borrowed from each tab's renderer, because
/// those render six different shapes - Cursor's coloured lanes,
/// Antigravity's groups, Codex's per-feature windows - and only these are
/// common to all of them.
#[derive(Clone, Debug, Default)]
pub struct Lane {
    pub label: String,
    pub pct: f64,
    /// How long the window is, where the agent says.
    pub window_secs: Option<f64>,
    /// When it resets, as epoch seconds.
    pub reset: Option<f64>,
    /// True when this came from a cache rather than from the agent just
    /// now. A number nobody labelled as old reads as current.
    pub stale: bool,
    /// True when `reset` was worked out rather than read - a window rolled
    /// forward from an older one on the length the agent stated. It is
    /// shown with a `~`, because a date this widget calculated and a date
    /// the server sent are not the same kind of fact.
    pub projected: bool,
    /// True when this lane is not part of the group above it and should be
    /// separated from it. Cursor's Grok Bot allowance is the case this
    /// exists for: three plan lanes on the monthly cycle, then a weekly
    /// allowance that is not a fourth slice of them. Carried on the lane so
    /// the agent's own tab and the summary cannot drift apart on it.
    pub apart: bool,
}

/// What a refused request said, in words a reader can act on.
///
/// `tc::get` returns curl's own message, which for `--fail` names the status
/// - "curl: (22) The requested URL returned error: 429". Every caller here
/// threw that away with `.ok()?`, so the screen could say a reading was old
/// but never why, and an hour went into checking a credential that was fine
/// while the server had already said "too many requests".
///
/// The code is read out of that message rather than guessed at. Anything
/// unrecognised is passed through as the reason it was, which is still more
/// than nothing.
pub fn refusal(said: &str) -> String {
    let code = said
        .rsplit_once("error: ")
        .and_then(|(_, tail)| tail.split_whitespace().next())
        .and_then(|c| c.parse::<u16>().ok());
    match code {
        Some(429) => "too many requests - something else is polling the same token".into(),
        Some(401) | Some(403) => "the token was refused - the agent may need signing in again".into(),
        Some(404) => "the endpoint is gone".into(),
        Some(c) if (500..600).contains(&c) => format!("the server answered {}", c),
        Some(c) => format!("the server answered {}", c),
        // A timeout or a dead network never reaches a status at all.
        None if said.contains("timed out") || said.contains("Timeout") => {
            "the request timed out".into()
        }
        None if said.contains("Could not resolve") || said.contains("Failed to connect") => {
            "could not reach it".into()
        }
        None => said.trim_start_matches("curl: ").to_string(),
    }
}

/// An HTTPS GET carrying a bearer token, returning parsed JSON.
///
/// The token goes to curl on its standard input, never in its arguments:
/// /proc/<pid>/cmdline is world-readable, so an argument is a secret handed
/// to every user on the box for as long as the request lasts.
pub fn get_json(url: &str, headers: &[(&str, &str)], seconds: u64) -> Option<serde_json::Value> {
    serde_json::from_str(&tc::get(url, headers, seconds).ok()?).ok()
}

/// An HTTPS POST of a JSON body carrying a bearer token.
pub fn post_json(
    url: &str,
    headers: &[(&str, &str)],
    body: &str,
    seconds: u64,
) -> Option<serde_json::Value> {
    post_json_said(url, headers, body, seconds).ok()
}

/// The same POST, keeping why it failed rather than discarding it.
pub fn post_json_said(
    url: &str,
    headers: &[(&str, &str)],
    body: &str,
    seconds: u64,
) -> Result<serde_json::Value, String> {
    let (text, _) = tc::post_json(url, headers, body, seconds).map_err(|said| refusal(&said))?;
    serde_json::from_str(&text).map_err(|e| format!("unreadable answer: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The string this was built from is the one curl actually produced
    /// against api.anthropic.com while three widgets shared a token.
    #[test]
    fn a_refusal_says_which_refusal_it_was() {
        let real = "curl: (22) The requested URL returned error: 429";
        assert!(
            refusal(real).contains("too many requests"),
            "the message that cost an hour: {:?}",
            refusal(real)
        );
        assert!(refusal(real).contains("same token"), "and what to do about it");

        for (said, want) in [
            ("curl: (22) The requested URL returned error: 401", "token was refused"),
            ("curl: (22) The requested URL returned error: 403", "token was refused"),
            ("curl: (22) The requested URL returned error: 503", "answered 503"),
            ("curl: (28) Operation timed out after 20000 ms", "timed out"),
            ("curl: (6) Could not resolve host: api.anthropic.com", "could not reach it"),
        ] {
            assert!(refusal(said).contains(want), "{:?} -> {:?}", said, refusal(said));
        }

        // Anything unrecognised comes through as itself rather than as
        // nothing, which is what the old .ok()? made of all of them.
        let odd = "curl: (35) SSL connect error";
        assert_eq!(refusal(odd), "(35) SSL connect error");
        assert!(!refusal(odd).is_empty());
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
        let mut caches = Caches::default();
        let held = |c: &Caches| c.live.get("probe").map(|(_, _, h)| *h).unwrap();

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
            let mut caches = Caches::default();
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
    fn a_good_reading_is_held_for_its_full_interval() {
        let mut caches = Caches::default();
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
}
