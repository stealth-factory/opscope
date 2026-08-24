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
}

pub const LIVE_TTL: f64 = 120.0;
/// A plan does not change between refreshes; the windows do.
pub const PLAN_TTL: f64 = 3600.0;

/// Hold a reading for a while, but never hold a failure that long.
///
/// The pane redraws every thirty seconds; these windows move over hours. A
/// failure is cached too, so a dead endpoint is retried occasionally rather
/// than on every frame - but only ever for the short interval, never the
/// long one. One transient 429 should not blank a section for an hour.
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
    let held = if value.is_some() { ttl } else { ttl.min(LIVE_TTL) };
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
    let (text, _) = tc::post_json(url, headers, body, seconds).ok()?;
    serde_json::from_str(&text).ok()
}
