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
    /// Claude transcripts: path -> what has been read of it so far.
    pub transcripts: HashMap<String, Tail<crate::claude::TranscriptLines>>,
    /// Codex rollouts, the same way.
    pub rollouts: HashMap<String, Tail<crate::codex::RolloutLines>>,
    /// Antigravity's prompt history, counted a line at a time.
    pub history: HashMap<String, Tail<Counted>>,
    /// Grok's session logs, summed as they grow.
    pub grok: HashMap<String, Tail<crate::grok::SessionLines>>,
    /// key -> (when, value, ttl)
    pub live: HashMap<String, (f64, Option<serde_json::Value>, f64)>,
    /// key -> refusals in a row, which is what the backoff doubles on.
    /// Cleared the moment one gets through.
    pub fails: HashMap<String, u32>,
    /// When the reader pressed `r`, for the one pass that press asked for.
    /// A good reading taken before this is asked again rather than served
    /// out of its hold; see `cached`. Zero on every other pass.
    pub asked_at: f64,
}

/// One record a transcript reader keeps: day, billed model, tokens.
pub type Record = (String, String, Tokens);
/// A transcript's records, keyed on whatever makes a turn the same turn
/// when it is written into a second file.
pub type Records = HashMap<String, Record>;

/// A reader of one append-only log, fed a line at a time in file order.
///
/// It holds whatever it carries between lines - the model a turn was
/// taken on, the session a rollout belongs to - as well as what it has
/// found, so that feeding it the rest of a file later lands exactly where
/// feeding it the whole file at once would have.
pub trait Lines {
    fn line(&mut self, line: &str);
}

/// How much of an append-only log has been read, and what reading it found.
///
/// Agents append to their transcripts for as long as a session runs, and a
/// live Codex rollout passes four gigabytes. Reading the whole file again
/// because it grew - as this did, on every refresh - put that whole file
/// in memory every ninety seconds. So a log is read once, a line at a
/// time, and after that only what was added is read: from `offset`, which
/// is always the end of the last line handed to the parser.
///
/// An unterminated last line is left where it is - the agent is still
/// writing it - unless it already parses as JSON, which is what a
/// whole-file read would have counted. The next pass starts from the same
/// byte, so a line torn across two appends reaches the parser once, whole.
///
/// The file is read from the start again when it is no longer the file
/// that was read: a different inode, a size below what was read, or a
/// first kilobyte that has changed under the same inode.
pub struct Tail<P> {
    /// (mtime, size) when it was last read. Unchanged means nothing to read.
    stamp: (i64, u64),
    /// (device, inode), so a log replaced under the same name is noticed.
    file: (u64, u64),
    /// Bytes consumed, always at a line boundary.
    offset: u64,
    /// The first bytes of the file, up to `HEAD`.
    head: Vec<u8>,
    /// Bytes taken off the disk for this log, all passes together. A torn
    /// last line is read again on the pass that finishes it, so this is
    /// the file's size plus those re-reads and nothing more.
    pub read: u64,
    /// The longest single line held while reading - the most of this log
    /// that was ever in memory at once, past the reader's fixed buffer.
    pub widest: usize,
    pub parser: P,
}

/// How much of a log's start is kept to recognise it by.
const HEAD: usize = 1024;

/// Bring one log's reader up to date, reading only what was appended.
///
/// False when the file cannot be read at all, which is what callers treat
/// as a log with nothing in it - the same as before this kept state.
pub fn follow<P: Lines>(
    tails: &mut HashMap<String, Tail<P>>,
    path: &str,
    fresh: impl FnOnce() -> P,
) -> bool {
    use std::os::unix::fs::{FileExt, MetadataExt};
    let Ok(meta) = std::fs::metadata(path) else {
        // Gone: nothing held for it is true any more.
        tails.remove(path);
        return false;
    };
    let stamp = (meta.mtime(), meta.size());
    let file = (meta.dev(), meta.ino());
    if let Some(t) = tails.get(path) {
        if t.stamp == stamp && t.file == file {
            return true;
        }
    }
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    let same = |head: &[u8]| {
        let mut now = vec![0u8; head.len()];
        f.read_exact_at(&mut now, 0).is_ok() && now == head
    };
    let keep = match tails.get(path) {
        Some(t) => t.file == file && stamp.1 >= t.offset && same(&t.head),
        None => false,
    };
    if !keep {
        tails.insert(
            path.to_string(),
            Tail {
                stamp: (0, 0),
                file,
                offset: 0,
                head: Vec::new(),
                read: 0,
                widest: 0,
                parser: fresh(),
            },
        );
    }
    let Some(t) = tails.get_mut(path) else {
        return false;
    };
    let size = stamp.1;
    if t.head.len() < HEAD && (t.head.len() as u64) < size {
        let mut head = vec![0u8; HEAD.min(size as usize)];
        if f.read_exact_at(&mut head, 0).is_ok() {
            t.head = head;
        }
    }
    let (used, read, widest) = feed(&f, t.offset, size, &mut t.parser);
    t.offset += used;
    t.read += read;
    t.widest = t.widest.max(widest);
    t.stamp = stamp;
    true
}

/// Drop what is held for logs that are no longer on disk.
///
/// The state follows the files rather than the uptime: a transcript that
/// was deleted, or a rollout an agent cleaned up, takes its records with
/// it. `owns` says which paths this caller is responsible for, so one
/// reader's prune cannot empty another's entries in a shared map.
pub fn prune<P>(
    tails: &mut HashMap<String, Tail<P>>,
    owns: impl Fn(&str) -> bool,
    present: &std::collections::HashSet<&str>,
) {
    tails.retain(|path, _| !owns(path) || present.contains(path.as_str()));
}

/// Hand the parser every whole line between `from` and `to`, and say how
/// many bytes that consumed, how many were read, and the longest line.
///
/// Read through a buffer, a line at a time, so memory follows the longest
/// line rather than the file. `to` is the size the file had when it was
/// looked at, so a line being written while this reads is left for the
/// next pass rather than read half-finished.
fn feed<P: Lines>(f: &std::fs::File, from: u64, to: u64, parser: &mut P) -> (u64, u64, usize) {
    use std::io::{BufRead, Read, Seek, SeekFrom};
    if to <= from {
        return (0, 0, 0);
    }
    let mut f = f;
    if f.seek(SeekFrom::Start(from)).is_err() {
        return (0, 0, 0);
    }
    let mut reader = std::io::BufReader::with_capacity(256 * 1024, f.take(to - from));
    let (mut used, mut read, mut widest) = (0u64, 0u64, 0usize);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let n = match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        read += n as u64;
        widest = widest.max(n);
        if buf.last() != Some(&b'\n')
            && serde_json::from_slice::<serde_json::Value>(&buf).is_err()
        {
            break;
        }
        used += n as u64;
        let mut line: &[u8] = &buf;
        if let Some(rest) = line.strip_suffix(b"\n") {
            line = rest;
        }
        if let Some(rest) = line.strip_suffix(b"\r") {
            line = rest;
        }
        parser.line(&String::from_utf8_lossy(line));
    }
    (used, read, widest)
}

/// Lines that are not blank, counted as they are appended.
#[derive(Default)]
pub struct Counted(pub usize);

impl Lines for Counted {
    fn line(&mut self, line: &str) {
        if !line.trim().is_empty() {
            self.0 += 1;
        }
    }
}

pub const LIVE_TTL: f64 = 120.0;
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
    held_for(caches, key, ttl, true, fetch)
}

/// `cached` for a reading keyed on the file it came from, which `r` leaves
/// alone. A key carrying the file's own (mtime, size) cannot hold anything
/// the file does not still say, so asking again only re-parses every
/// session on disk to learn the same numbers.
pub fn cached_file<F>(caches: &mut Caches, key: &str, ttl: f64, fetch: F) -> Option<serde_json::Value>
where
    F: FnOnce() -> Option<serde_json::Value>,
{
    held_for(caches, key, ttl, false, fetch)
}

fn held_for<F>(
    caches: &mut Caches,
    key: &str,
    ttl: f64,
    askable: bool,
    fetch: F,
) -> Option<serde_json::Value>
where
    F: FnOnce() -> Option<serde_json::Value>,
{
    let at = now();
    if let Some((when, value, held)) = caches.live.get(key) {
        // A refusal keeps its backoff even when asked: pressing `r` at an
        // endpoint that is rate-limiting us is how the limit is sustained.
        // Antigravity holds its refusal under a name of its own.
        let good = value
            .as_ref()
            .is_some_and(|v| v.get("why").is_none() && v.get("opscope_refusal").is_none());
        // Aged at the press, not now: a reading kept for being under the
        // floor must not be asked later by a pass nobody pressed for.
        let asked = askable
            && good
            && *when < caches.asked_at
            && caches.asked_at - when >= ASKED_FLOOR;
        if at - when < *held && !asked {
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

/// Put a refusal's reason in the slot `cached` just stored as `None`.
///
/// A diagnostic wrapped in `Some` looks like a reading: the fail count
/// clears and the next ask is two minutes later, forever. The reason still
/// has to survive the frames inside the backoff, so it is written in after
/// the hold and the tally have already been set. Later frames see the
/// sentence; they do not see a success.
pub fn remember_refusal(caches: &mut Caches, key: &str, why: &str) {
    if why.is_empty() {
        return;
    }
    let Some((when, _, held)) = caches.live.get(key).cloned() else {
        return;
    };
    caches.live.insert(
        key.to_string(),
        (when, Some(serde_json::json!({"why": why})), held),
    );
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

/// The heading the unified Cursor and Grok plan is drawn under.
///
/// One plan sold through two products: a weekly included pool both of them
/// draw on, one prepaid credit balance, and one monthly limit on additional
/// usage. A fixed name rather than one read from either API, because each
/// vendor names the plan after itself - `cursorPlanName`, `grokPlanLabel` -
/// and a heading taken from whichever answered first would rename the
/// section depending on which tab was refreshed last.
pub const SPACEXSI: &str = "SpaceXSI";

/// Where an agent stands relative to the unified plan.
///
/// Decided once per poll in `vendors::link_spacexsi`, because it needs both
/// vendors' readings and each vendor's reader sees only its own. An agent
/// still on its own plan stays `No` and draws exactly what it always drew.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Spacexsi {
    /// Not on the unified plan, or not known to be.
    #[default]
    No,
    /// On the unified plan, and the other vendor is not on the same account
    /// here - absent, still on its own plan, or not signed in.
    Alone,
    /// On the unified plan with the other vendor on the same account: one
    /// pool, one balance, one limit, drawn once.
    Shared,
    /// Both vendors are unified but on different accounts, so each keeps a
    /// section of its own and nothing is merged.
    Separate,
}

impl Spacexsi {
    pub fn unified(self) -> bool {
        self != Spacexsi::No
    }
}

/// How much of a lane label the cap after it may take.
///
/// The summary's label column is shared by every agent and capped at 16
/// cells, and the longest bare label that carries a cap is `on-demand` at
/// nine - so seven, including the `$` and the space before it. Past that
/// the column would grow at the bar's expense on a narrow pane.
pub const CAP_TAG_ROOM: usize = 7;

/// A ceiling, short enough to ride on a lane label.
///
/// A percentage of an unnamed limit is not a number anyone can act on: 19%
/// says nothing until the reader knows it is 19% of fifty dollars. So the
/// label says the cap and the bar stays the percentage.
///
/// Written as exactly as it fits and never truncated, because `$10,0` is a
/// number that does not exist: whole dollars where the cap is whole, cents
/// where they fit, a rounded whole where they do not, and thousands folded
/// to `$10k` above that. `seg` would clip a label that overflowed and clip
/// is what this exists to avoid.
pub fn cap_tag(dollars: f64) -> String {
    cap_tag_in("$", dollars)
}

/// The same tag, in a currency that is not dollars.
///
/// Claude states the account's currency beside every amount and this one is
/// AUD, so a `$` on the label would be a claim about the money that the
/// server never made. `prefix` is whatever goes before the digits - `"$"`,
/// or a three-letter code and a space - and every candidate is still
/// measured against `CAP_TAG_ROOM`, so a wider prefix folds sooner rather
/// than growing the label column. ` AUD 50` fits exactly; ` AUD 5,000`
/// does not and becomes ` AUD 5k`.
pub fn cap_tag_in(prefix: &str, dollars: f64) -> String {
    let grouped = |n: f64| {
        let digits = (n.abs().round() as i64).to_string();
        let mut out = String::new();
        for (i, ch) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(ch);
        }
        out
    };
    let whole = (dollars - dollars.round()).abs() < 0.005;
    let mut tries: Vec<String> = Vec::new();
    if whole {
        tries.push(format!(" {}{}", prefix, grouped(dollars)));
    } else {
        tries.push(format!(
            " {}{}.{:02}",
            prefix,
            grouped(dollars.trunc()),
            ((dollars.fract() * 100.0).round() as i64).abs()
        ));
        tries.push(format!(" {}{}", prefix, grouped(dollars)));
    }
    // Folded rather than cut. One decimal while it fits, because $12.3k and
    // $12k are different claims about the ceiling.
    for (unit, div) in [("k", 1_000.0), ("m", 1_000_000.0), ("b", 1_000_000_000.0)] {
        let scaled = dollars / div;
        if scaled >= 1.0 {
            // No ".0" on a round one: $10.0k spends a cell to say nothing.
            if (scaled - scaled.round()).abs() >= 0.05 {
                tries.push(format!(" {}{:.1}{}", prefix, scaled, unit));
            }
            tries.push(format!(" {}{:.0}{}", prefix, scaled, unit));
        }
    }
    tries
        .into_iter()
        .find(|t| t.chars().count() <= CAP_TAG_ROOM)
        // Nothing left to fold: better an honest long label than a clipped
        // one, and no real cap reaches here.
        .unwrap_or_else(|| format!(" {}{:.0}", prefix, dollars))
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
        // The rest is whatever the server put in its body, which is text
        // the widget did not write and is about to draw.
        None => crate::parse::strip_controls(said.trim_start_matches("curl: ")),
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

/// A log that grows, is torn mid-line, and is replaced, for checking that
/// a reader fed by `follow` always lands where a whole-file read would.
#[cfg(test)]
pub mod growing {
    use super::*;
    use std::io::Write;

    pub struct Log {
        dir: std::path::PathBuf,
        pub path: String,
    }

    impl Log {
        pub fn new(tag: &str) -> Log {
            let dir = std::env::temp_dir().join(format!(
                "opscope-follow-{tag}-{}-{}",
                std::process::id(),
                now().to_bits()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("log.jsonl").to_string_lossy().to_string();
            std::fs::write(&path, "").unwrap();
            Log { dir, path }
        }

        pub fn append(&self, bytes: &str) {
            let mut f = std::fs::OpenOptions::new().append(true).open(&self.path).unwrap();
            f.write_all(bytes.as_bytes()).unwrap();
        }

        /// A new file put in place by rename: a different inode, the way
        /// an agent rotating its log or an editor saving it would.
        pub fn replace(&self, bytes: &str) {
            let next = self.dir.join("next.jsonl");
            std::fs::write(&next, bytes).unwrap();
            std::fs::rename(&next, &self.path).unwrap();
        }

        /// The same inode, truncated and written again.
        pub fn rewrite(&self, bytes: &str) {
            std::fs::write(&self.path, bytes).unwrap();
        }
    }

    impl Drop for Log {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The lines a writer has finished: up to the last newline, plus a last
    /// line only if it is already a whole JSON value. A line still being
    /// written is not a line yet, and a reader that counted `{"n":10` as
    /// ten would be wrong by the time it read `{"n":1000}`.
    fn finished(text: &str) -> &str {
        let cut = text.rfind('\n').map_or(0, |at| at + 1);
        match serde_json::from_str::<serde_json::Value>(&text[cut..]) {
            Ok(_) => text,
            Err(_) => &text[..cut],
        }
    }

    /// Append `body` a piece of `step` bytes at a time, then replace and
    /// rewrite the file, and after every change compare what the followed
    /// reader holds against `whole` run over the file as it now stands.
    ///
    /// Odd steps put cuts in the middle of lines, and of the characters
    /// inside them when the body is not all ASCII, which is the case a
    /// reader keeping its place has to get right.
    pub fn check<P: Lines, T: PartialEq + std::fmt::Debug>(
        tag: &str,
        body: &str,
        step: usize,
        replacement: &str,
        fresh: impl Fn() -> P,
        whole: impl Fn(&str) -> T,
        of: impl Fn(&P) -> T,
    ) {
        let log = Log::new(tag);
        let mut tails: HashMap<String, Tail<P>> = HashMap::new();
        let bytes = body.as_bytes();
        let mut written = 0;
        let compare = |tails: &mut HashMap<String, Tail<P>>, what: &str| {
            assert!(follow(tails, &log.path, &fresh), "{what}: unreadable");
            let on_disk = String::from_utf8_lossy(&std::fs::read(&log.path).unwrap()).to_string();
            assert_eq!(of(&tails[&log.path].parser), whole(finished(&on_disk)), "{what}");
        };
        while written < bytes.len() {
            let end = (written + step).min(bytes.len());
            let mut f = std::fs::OpenOptions::new().append(true).open(&log.path).unwrap();
            f.write_all(&bytes[written..end]).unwrap();
            drop(f);
            written = end;
            compare(&mut tails, &format!("after {written} of {} bytes", bytes.len()));
        }
        assert_eq!(of(&tails[&log.path].parser), whole(body), "the whole file");
        // Replaced by a shorter, different log: started again, not added to.
        log.replace(replacement);
        compare(&mut tails, "replaced");
        // Back to the long one under the same inode: bigger than what was
        // read, so only the changed first bytes say it is not the same log.
        log.rewrite(body);
        compare(&mut tails, "rewritten in place");
        // Cut short in place.
        log.rewrite(replacement);
        compare(&mut tails, "truncated");
        log.append(body);
        compare(&mut tails, "grown again");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_counted_log_counts_what_a_whole_read_does() {
        let body = "{\"a\":1}\n\n{\"a\":2}\n  \n{\"a\":3}\n";
        let count = |text: &str| text.lines().filter(|l| !l.trim().is_empty()).count();
        for step in [1, 3, 7, 64] {
            growing::check("counted", body, step, "{\"b\":1}\n", Counted::default, count, |c: &Counted| c.0);
        }
    }

    #[test]
    fn an_unterminated_last_line_waits_unless_it_is_already_whole() {
        let log = growing::Log::new("partial");
        let mut tails: HashMap<String, Tail<Counted>> = HashMap::new();
        log.append("{\"a\":1}\n{\"a\":");
        assert!(follow(&mut tails, &log.path, Counted::default));
        assert_eq!(tails[&log.path].parser.0, 1, "half a line is not a line yet");
        assert_eq!(tails[&log.path].offset, 8, "and the place is kept before it");
        log.append("2}");
        follow(&mut tails, &log.path, Counted::default);
        assert_eq!(tails[&log.path].parser.0, 2, "a whole object is counted, as a whole read would");
        log.append("\n{\"a\":3}\n");
        follow(&mut tails, &log.path, Counted::default);
        assert_eq!(tails[&log.path].parser.0, 3, "the newline that closed it is not a second line");
    }

    #[test]
    fn a_log_far_bigger_than_its_lines_is_never_held_whole() {
        // Grown to 24 MB in 3,000 appends, some of them tearing a line in
        // half. Whatever the size, what is held is one line at a time and
        // the first kilobyte, and every byte comes off the disk once -
        // apart from the torn halves, read again on the pass that ends them.
        let log = growing::Log::new("bounded");
        let mut tails: HashMap<String, Tail<Counted>> = HashMap::new();
        let line = format!("{{\"pad\":\"{}\"}}\n", "x".repeat(8 * 1024 - 12));
        let (mut written, mut lines, mut torn) = (0u64, 0usize, 0u64);
        let mut half = String::new();
        for i in 0..3000 {
            let mut piece = std::mem::take(&mut half);
            piece.push_str(&line);
            if i % 7 == 3 {
                // Leave the next line half written until the next append.
                let cut = line.len() / 2;
                piece.push_str(&line[..cut]);
                half = line[cut..].to_string();
                torn += cut as u64;
            }
            log.append(&piece);
            written += piece.len() as u64;
            lines += piece.matches('\n').count();
            let before = tails.get(&log.path).map_or(0, |t| t.read);
            assert!(follow(&mut tails, &log.path, Counted::default));
            let t = &tails[&log.path];
            assert_eq!(t.parser.0, lines, "append {i}");
            // Only what was appended, plus a half line left from before.
            assert!(t.read - before <= piece.len() as u64 + line.len() as u64, "append {i}");
        }
        if !half.is_empty() {
            log.append(&half);
            written += half.len() as u64;
            lines += 1;
            follow(&mut tails, &log.path, Counted::default);
        }
        let t = &tails[&log.path];
        assert_eq!(t.parser.0, lines);
        assert!(written > 20 * 1024 * 1024, "the log is meant to dwarf its lines");
        assert_eq!(t.read, written + torn, "each byte read once, torn halves twice");
        assert_eq!(t.widest, line.len(), "never more than one line at a time");
        assert!(t.head.len() <= HEAD);
    }

    #[test]
    fn a_deleted_log_takes_its_state_with_it() {
        let a = growing::Log::new("kept");
        let b = growing::Log::new("deleted");
        a.append("{}\n");
        b.append("{}\n{}\n");
        let mut tails: HashMap<String, Tail<Counted>> = HashMap::new();
        let other = "/somewhere/else/log.jsonl".to_string();
        for path in [&a.path, &b.path] {
            assert!(follow(&mut tails, path, Counted::default));
        }
        tails.insert(
            other.clone(),
            Tail { stamp: (0, 0), file: (0, 0), offset: 0, head: Vec::new(), read: 0, widest: 0, parser: Counted(9) },
        );
        let gone = b.path.clone();
        drop(b);
        // Asked for by name after it went, it is dropped on the spot.
        let mut asked = HashMap::new();
        assert!(follow(&mut asked, &a.path, Counted::default));
        std::fs::remove_file(&a.path).unwrap();
        assert!(!follow(&mut asked, &a.path, Counted::default));
        assert!(asked.is_empty(), "a log asked for after it went is forgotten");
        std::fs::write(&a.path, "{}\n").unwrap();
        let present: std::collections::HashSet<&str> = [a.path.as_str()].into_iter().collect();
        prune(&mut tails, |path| !path.starts_with("/somewhere/"), &present);
        assert!(tails.contains_key(&a.path));
        assert!(!tails.contains_key(&gone), "a deleted log's records are dropped");
        assert!(tails.contains_key(&other), "another reader's entries are not this prune's");
    }

    #[test]
    fn an_unchanged_log_is_not_read_again() {
        struct Loud(usize);
        impl Lines for Loud {
            fn line(&mut self, _: &str) {
                self.0 += 1;
            }
        }
        let log = growing::Log::new("unchanged");
        log.append("{}\n{}\n");
        let mut tails: HashMap<String, Tail<Loud>> = HashMap::new();
        for _ in 0..3 {
            assert!(follow(&mut tails, &log.path, || Loud(0)));
        }
        assert_eq!(tails[&log.path].parser.0, 2);
        log.append("{}\n");
        follow(&mut tails, &log.path, || Loud(0));
        assert_eq!(tails[&log.path].parser.0, 3, "only the appended line was read");
    }

    #[test]
    fn a_cap_on_a_label_fits_the_column_it_was_given() {
        // The label column is shared by every agent on the summary and
        // capped at sixteen cells, so a cap that overran it would take the
        // width off the bar beside it - and `seg` would clip the label,
        // which is how you get a ceiling of `$10,0`.
        for cap in [5.0, 50.0, 1000.0, 10000.0, 12.5] {
            let tag = cap_tag(cap);
            assert!(
                tag.chars().count() <= CAP_TAG_ROOM,
                "{} took {} cells: {:?}",
                cap,
                tag.chars().count(),
                tag
            );
            assert!(tag.starts_with(" $"), "{:?}", tag);
        }
        // Whole dollars where the cap is whole, cents where they fit, and
        // thousands folded rather than cut.
        assert_eq!(cap_tag(5.0), " $5");
        assert_eq!(cap_tag(50.0), " $50");
        assert_eq!(cap_tag(1000.0), " $1,000");
        assert_eq!(cap_tag(12.5), " $12.50");
        assert_eq!(cap_tag(10000.0), " $10k");
        // And a figure with cents too long to spell keeps its magnitude
        // rather than its pennies.
        assert_eq!(cap_tag(1234.56), " $1,235");
    }

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

    #[test]
    fn a_refusal_body_cannot_drive_the_terminal() {
        // An unrecognised refusal is drawn as the server said it, and what
        // the server said is its own body - a 500 page from a proxy, or
        // anything else on the path, with whatever bytes it chose to send.
        let said = "HTTP 500 \u{1b}]0;owned\u{7}bad \u{1b}[31mgateway\u{1b}[0m\nretry";
        let shown = refusal(said);
        assert!(!shown.chars().any(char::is_control), "{:?}", shown);
        assert!(shown.contains("bad gateway retry"), "{:?}", shown);
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
    fn a_remembered_refusal_does_not_clear_the_backoff() {
        // The shape that used to cost the backoff: wrapping a silent
        // endpoint in Some({"why": ...}) looks like a reading, so the fail
        // count reset and the next ask was two minutes later forever.
        let mut caches = Caches::default();
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
        assert_eq!(text(&again.unwrap(), "why"), "the endpoint did not answer");
        assert_eq!(caches.fails.get("probe"), Some(&1));
    }

    #[test]
    fn a_refresh_asks_again_for_a_good_reading_past_the_floor() {
        // The bug: `r` woke the poller, but every quota was still inside
        // its hold, so the press re-read local files and nothing else.
        let mut caches = Caches::default();
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
        let mut caches = Caches::default();
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
        let mut caches = Caches::default();
        caches.asked_at = now() - 20.0;
        let taken = now() - 30.0;
        caches.live.insert("probe".into(), (taken, Some(serde_json::json!(1)), 300.0));
        cached(&mut caches, "probe", 300.0, || panic!("asked without a press"));
    }

    #[test]
    fn a_refresh_does_not_reparse_a_file_keyed_reading() {
        // Keyed on the file's (mtime, size), so asking again reads the same
        // bytes back.
        let mut caches = Caches::default();
        let old = now() - ASKED_FLOOR - 1.0;
        caches.live.insert("file".into(), (old, Some(serde_json::json!(1)), 3600.0));
        caches.asked_at = now();
        cached_file(&mut caches, "file", 3600.0, || panic!("re-parsed an unchanged file"));
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
