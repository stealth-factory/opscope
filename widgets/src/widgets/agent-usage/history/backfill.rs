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

//! The one-off Codex backfill (spec §8, OPS-308).
//!
//! Codex rollouts carry `rate_limits` snapshots on their `token_count`
//! events: `used_percent`, `resets_at` and the window in minutes, each at the
//! event's own time. Replayed as samples with `source = 'backfill'` they are
//! full evidence (§4), so Codex gets its history, resets included, on the
//! first day rather than from the day this shipped.
//!
//! **Which account.** A rollout's `session_meta` names the ChatGPT account
//! that created the session (`creator_account_id`), which is the same id
//! `auth.json` holds and the live lanes are keyed by. A snapshot is replayed
//! only when the session it sits in was created by the account being
//! backfilled. A snapshot under no `session_meta`, or under one that names
//! no account, is counted and dropped: it may belong to an account this
//! machine was signed in as before, and a guess would put another account's
//! week into this one's history. A later `session_meta` naming another
//! account ends the attribution from that line on. What cannot be seen: a
//! session resumed after an account switch that writes no new
//! `session_meta`. Nothing else in a rollout names the account - checked
//! against real rollouts, where only `session_meta` carries
//! `creator_account_id` - so such a session's later snapshots are still
//! read as its creator's.
//!
//! **Which pool.** The keys the live lanes use (`codex::lanes`): the main
//! limit's `primary` is `primary_window` and its `secondary` is
//! `secondary_window` - the live endpoint calls them that and `codex::lanes`
//! already maps its own snapshot fallback the same way - and a limit with a
//! `limit_id` of its own is `additional:{limit_name}`, its primary window
//! only, as the live lane reads it.
//!
//! **How much.** Rollouts write a snapshot on every turn, many a minute, so
//! the replay is thinned by the §3.7 write rule before it is written ([`thin`]),
//! and then written through the same rule against what is already stored.
//! `(pool_id, at)` is the sample's key, so a snapshot already in the file is
//! not written twice, and a re-run writes nothing new.
//!
//! **Off the poller.** The rollouts can run to gigabytes, so they are read on
//! a thread of their own ([`Runner`]); the pass that finds the result waiting
//! writes it in one transaction and re-folds the pools it touched from the
//! earliest replayed sample. While the thread reads, `meta` holds a lease, so
//! a second pane does not read the same gigabytes again; a lease nobody
//! renews runs out and another pane takes it.
//!
//! **When it runs again.** Once per account: `meta` records [`done`] under
//! [`meta_key`]. A new [`VERSION`] - a reader that finds more - runs it again,
//! and that is safe, because nothing already stored is written twice. A new
//! fold (`fold::DETECTOR_VERSION`) does not need it: the replayed samples are
//! in the file, and opening it re-folds every pool from them.
//!
//! **Not read.** Rollouts Codex has compressed (`.jsonl.zst`) are counted and
//! left alone: reading them needs a zstd decoder this build does not carry.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use super::fold::{Sample, Source, Thresholds};
use super::{Observation, store};
use crate::shared::{PoolRole, Unit};

/// Bumped when the reader would find more than it did: every account is
/// backfilled again, and only what is new is written.
pub const VERSION: u32 = 1;

/// How long a running backfill's claim holds without being renewed. The
/// pane holding it renews it on every pass.
pub const LEASE: f64 = 600.0;

/// The longest line read whole. A `token_count` line is a few hundred bytes;
/// a line past this is a pasted file or an image, and is skipped.
const MAX_LINE: usize = 8 << 20;

/// The `meta` key recording one account's backfill.
pub fn meta_key(account_key: &str) -> String {
    format!("backfill:codex:{account_key}")
}

/// What `meta` holds once the backfill has run and written `written`
/// samples.
pub fn done(written: u64) -> String {
    format!("done:{VERSION}:{written}")
}

/// What `meta` holds while a pane is reading the rollouts.
pub fn running(at: f64) -> String {
    format!("running:{at}")
}

/// How many samples a finished backfill of this [`VERSION`] wrote, from what
/// `meta` holds. `None` for anything else, an older version's included.
pub fn parse_done(recorded: &str) -> Option<u64> {
    recorded.strip_prefix(&format!("done:{VERSION}:"))?.parse().ok()
}

/// When a running backfill last renewed its claim.
pub fn parse_running(recorded: &str) -> Option<f64> {
    recorded.strip_prefix("running:")?.parse().ok()
}

/// Whether the backfill should run, given what `meta` holds under
/// [`meta_key`] (`None` when nothing): not when this version has run, not
/// while another pane's claim is live, otherwise yes.
pub fn due(recorded: Option<&str>, now: f64) -> bool {
    let Some(v) = recorded else {
        return true;
    };
    if parse_done(v).is_some() {
        return false;
    }
    match parse_running(v) {
        Some(at) => now - at >= LEASE,
        None => true,
    }
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

/// What one rollout line says to the backfill.
#[derive(Clone, Debug, PartialEq)]
pub enum RolloutLine {
    /// A `session_meta`: the account that created the session, when it
    /// names one.
    Meta(Option<String>),
    /// A `token_count` event's `rate_limits`, one entry per window it read.
    /// Empty when it held none with a percentage.
    Limits(Vec<Replayed>),
    /// Anything else.
    Other,
    /// A line that looked like one of the above and would not parse.
    Malformed,
}

/// Read one rollout line.
pub fn parse_rollout_line(line: &str) -> RolloutLine {
    // Nearly every line is neither; parsing them all would be most of the
    // cost of reading gigabytes.
    if !line.contains("\"rate_limits\"") && !line.contains("\"session_meta\"") {
        return RolloutLine::Other;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return RolloutLine::Malformed;
    };
    let payload = &v["payload"];
    if v["type"].as_str() == Some("session_meta") {
        let who = payload["creator_account_id"].as_str().filter(|s| !s.is_empty());
        return RolloutLine::Meta(who.map(str::to_string));
    }
    if payload["type"].as_str() != Some("token_count") {
        return RolloutLine::Other;
    }
    let limits = match payload["rate_limits"].is_object() {
        true => &payload["rate_limits"],
        false => &payload["info"]["rate_limits"],
    };
    if !limits.is_object() {
        return RolloutLine::Other;
    }
    let Some(at) = v["timestamp"].as_str().and_then(crate::iso_epoch) else {
        return RolloutLine::Malformed;
    };
    RolloutLine::Limits(limits_at(limits, at))
}

/// The windows one `rate_limits` object read at `at`, keyed as the live
/// lanes key them.
fn limits_at(limits: &serde_json::Value, at: f64) -> Vec<Replayed> {
    let mut out = Vec::new();
    let replay = |out: &mut Vec<Replayed>, win: &serde_json::Value, key: String, label: &dyn Fn(Option<f64>) -> String| {
        let Some(pct) = win["used_percent"].as_f64().filter(|p| p.is_finite()) else {
            return;
        };
        let window_secs = win["window_minutes"].as_f64().filter(|m| *m > 0.0).map(|m| m * 60.0);
        // Older rollouts state the time left rather than the instant: the
        // same "read time plus seconds left" a live reading is.
        let reset_at = win["resets_at"]
            .as_f64()
            .or_else(|| win["resets_in_seconds"].as_f64().map(|s| at + s));
        out.push(Replayed {
            pool_key: key,
            label: label(window_secs),
            unit: Unit::Pct,
            role: PoolRole::Included,
            sample: Sample {
                at,
                pct: Some(pct),
                used: None,
                limit: None,
                window_secs,
                reset_at,
                source: Source::Backfill,
                spacexsi: None,
            },
        });
    };
    match limits["limit_id"].as_str().unwrap_or("codex") {
        // The main limit; rollouts from before `limit_id` existed are it.
        "codex" => {
            for (side, key) in [("primary", "primary_window"), ("secondary", "secondary_window")] {
                replay(&mut out, &limits[side], key.into(), &crate::codex::window_name);
            }
        }
        // A metered feature's own limit. Without its name there is no live
        // pool to put it in.
        _ => {
            if let Some(name) = limits["limit_name"].as_str().filter(|n| !n.is_empty()) {
                let short = name.rsplit('-').next().unwrap_or("?").to_string();
                let label = move |secs: Option<f64>| format!("{short} {}", crate::codex::window_name(secs));
                replay(&mut out, &limits["primary"], format!("additional:{name}"), &label);
            }
        }
    }
    out
}

/// What the reader saw and what it left out, and why.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub files: usize,
    /// Files that could not be opened or read to the end.
    pub unreadable: usize,
    /// `.jsonl.zst` rollouts, not read (see the module note).
    pub compressed: usize,
    pub malformed_lines: usize,
    pub oversize_lines: usize,
    /// Readings in a session that names no account, or none known.
    pub untied: usize,
    /// Readings in another account's sessions.
    pub other_account: usize,
    /// Readings replayed for this account, before thinning.
    pub readings: usize,
}

/// One rollout's lines, in order, for one account.
struct Scan<'a> {
    account_id: &'a str,
    /// The account of the session the next reading belongs to: `None` before
    /// any `session_meta`, or after one that could not be read.
    session: Option<Option<String>>,
    rows: Vec<Replayed>,
}

impl Scan<'_> {
    fn line(&mut self, line: &str, stats: &mut Stats) {
        match parse_rollout_line(line) {
            RolloutLine::Meta(who) => self.session = Some(who),
            RolloutLine::Limits(rows) => match &self.session {
                Some(Some(who)) if who == self.account_id => {
                    stats.readings += rows.len();
                    self.rows.extend(rows);
                }
                Some(Some(_)) => stats.other_account += rows.len(),
                _ => stats.untied += rows.len(),
            },
            RolloutLine::Other => {}
            RolloutLine::Malformed => {
                stats.malformed_lines += 1;
                // A session header that would not read leaves the readings
                // after it with no account.
                if header_type(line.as_bytes()) == Some("session_meta") {
                    self.session = None;
                }
            }
        }
    }

    /// A line too long to read whole. Its start says whether it was a
    /// session header, and if it was, the account is no longer known.
    fn oversize(&mut self, start: &[u8], stats: &mut Stats) {
        stats.oversize_lines += 1;
        if header_type(start) == Some("session_meta") {
            self.session = None;
        }
    }
}

/// The line's own `type`, read from its start without parsing the rest: the
/// first `"type":` in a rollout line is the top-level one, which is written
/// before the payload. Anything inside the payload - a tool's output quoting
/// a rollout, say - comes after it.
fn header_type(start: &[u8]) -> Option<&str> {
    let head = &start[..start.len().min(512)];
    let at = head.windows(7).position(|w| w == b"\"type\":")? + 7;
    let rest = std::str::from_utf8(&head[at..]).ok().or_else(|| {
        // Cut mid-character at 512: the valid prefix is enough.
        let valid = std::str::from_utf8(&head[at..]).err()?.valid_up_to();
        std::str::from_utf8(&head[at..at + valid]).ok()
    })?;
    let rest = rest.trim_start().strip_prefix('"')?;
    rest.split('"').next()
}

/// Read one line into `buf`, keeping at most `cap` bytes of it. `None` at the
/// end; `Some(true)` when the line was longer than `cap`.
fn read_capped<R: BufRead>(r: &mut R, buf: &mut Vec<u8>, cap: usize) -> std::io::Result<Option<bool>> {
    buf.clear();
    let mut over = false;
    let mut any = false;
    loop {
        let avail = match r.fill_buf() {
            Ok(a) => a,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if avail.is_empty() {
            return Ok(any.then_some(over));
        }
        any = true;
        let end = avail.iter().position(|&b| b == b'\n');
        let chunk = &avail[..end.unwrap_or(avail.len())];
        let room = cap.saturating_sub(buf.len());
        if chunk.len() > room {
            over = true;
            buf.extend_from_slice(&chunk[..room]);
        } else {
            buf.extend_from_slice(chunk);
        }
        let used = end.map_or(avail.len(), |e| e + 1);
        r.consume(used);
        if end.is_some() {
            return Ok(Some(over));
        }
    }
}

/// One rollout's readings for `account_id`, oldest first and thinned. A file
/// that stops reading part-way keeps what it read before, and is counted.
fn read_rollout(path: &Path, account_id: &str, stats: &mut Stats) -> Vec<Replayed> {
    let mut scan = Scan {
        account_id,
        session: None,
        rows: Vec::new(),
    };
    let Ok(file) = std::fs::File::open(path) else {
        stats.unreadable += 1;
        return Vec::new();
    };
    let mut r = std::io::BufReader::with_capacity(1 << 16, file);
    let mut buf = Vec::new();
    loop {
        match read_capped(&mut r, &mut buf, MAX_LINE) {
            Ok(None) => break,
            Ok(Some(true)) => scan.oversize(&buf, stats),
            Ok(Some(false)) => scan.line(&String::from_utf8_lossy(&buf), stats),
            Err(_) => {
                stats.unreadable += 1;
                break;
            }
        }
    }
    thin(scan.rows)
}

/// Every `.jsonl` under `roots`, sorted, and how many compressed ones were
/// passed over. A directory that cannot be listed is skipped: there is
/// nothing in it this reader could have read.
fn rollout_files(roots: &[PathBuf], stats: &mut Stats) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo: Vec<PathBuf> = roots.to_vec();
    while let Some(dir) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Ok(kind) = e.file_type() else {
                continue;
            };
            let name = e.file_name().to_string_lossy().into_owned();
            if kind.is_dir() {
                todo.push(path);
            } else if name.ends_with(".jsonl") {
                out.push(path);
            } else if name.ends_with(".jsonl.zst") {
                stats.compressed += 1;
            }
        }
    }
    out.sort();
    out
}

/// Where Codex keeps its rollouts: live sessions and archived ones.
pub fn rollout_roots() -> Vec<PathBuf> {
    [".codex/sessions", ".codex/archived_sessions"]
        .iter()
        .map(|d| PathBuf::from(crate::under_home(d)))
        .collect()
}

/// Every rate-limit reading the rollouts under `roots` hold for the account
/// `account_id`, thinned, oldest first within each pool.
pub fn collect(account_id: &str, roots: &[PathBuf]) -> (Vec<Replayed>, Stats) {
    let mut stats = Stats::default();
    if account_id.is_empty() {
        return (Vec::new(), stats);
    }
    let files = rollout_files(roots, &mut stats);
    let mut rows = Vec::new();
    for path in &files {
        stats.files += 1;
        rows.extend(read_rollout(path, account_id, &mut stats));
    }
    (thin(rows), stats)
}

/// The §3.7 write rule over a replay: per pool, oldest first, a reading is
/// kept when the one kept before it is at least five minutes earlier and it
/// changed something or an hour has passed. Two readings at one instant are
/// one.
pub fn thin(rows: Vec<Replayed>) -> Vec<Replayed> {
    let mut by_pool: BTreeMap<String, Vec<Replayed>> = BTreeMap::new();
    for r in rows {
        by_pool.entry(r.pool_key.clone()).or_default().push(r);
    }
    let mut out = Vec::new();
    for (_, mut list) in by_pool {
        list.sort_by(|a, b| a.sample.at.total_cmp(&b.sample.at));
        let mut kept: Vec<Replayed> = Vec::new();
        for r in list {
            if super::should_write(kept.last().map(|k| &k.sample), None, &r.sample) {
                kept.push(r);
            }
        }
        out.extend(kept);
    }
    out
}

/// What the reading thread hands back.
enum Outcome {
    Done(Vec<Replayed>, Stats),
    Failed(String),
}

/// A backfill being read.
struct Job {
    account_key: String,
    heading: String,
    tab: String,
    got: mpsc::Receiver<Outcome>,
}

/// The poller's handle on the backfill: claims it, reads the rollouts on a
/// thread, writes what the thread found, and says what happened.
pub struct Runner {
    roots: Vec<PathBuf>,
    job: Option<Job>,
    /// What the last run in this pane left out, said beside its count.
    skipped: Option<String>,
    /// Why the last run in this pane stopped. It is not tried again in this
    /// pane: a reader that panicked would panic again.
    failed: Option<String>,
}

impl Runner {
    pub fn new(roots: Vec<PathBuf>) -> Runner {
        Runner {
            roots,
            job: None,
            skipped: None,
            failed: None,
        }
    }

    /// Advance the backfill by one pass: start it when it is due, renew its
    /// claim while it reads, write its result when it lands. Returns the tab
    /// and the dim words it should draw about the backfill, if any.
    pub fn tick(
        &mut self,
        h: &mut store::History,
        obs: &[Observation],
        t: &Thresholds,
        now: f64,
    ) -> Option<(String, String)> {
        if let Some(job) = self.job.take() {
            return self.poll(h, job, t, now);
        }
        // Only an account with Codex's own id: a rollout cannot be tied to
        // anything else.
        let o = obs.iter().find(|o| o.provider == "codex" && o.has_stable_id())?;
        let key = o.account_key();
        let mk = meta_key(&key);
        if let Some(why) = &self.failed {
            return Some((o.tab.clone(), format!("backfill stopped: {why}")));
        }
        let recorded = h.meta(&mk).ok().flatten();
        if let Some(n) = recorded.as_deref().and_then(parse_done) {
            let mut words = format!("{} backfilled from rollouts", group(n));
            if let Some(s) = &self.skipped {
                words.push_str(&format!(" · {s}"));
            }
            return Some((o.tab.clone(), words));
        }
        // Claimed in one transaction, so two panes cannot both start.
        let claimed = h
            .meta_update(&mk, |v| due(v, now).then(|| running(now)))
            .unwrap_or(false);
        if !claimed {
            // Another pane holds it; its result reaches this pane's line
            // through `meta` when it lands.
            return Some((o.tab.clone(), "backfilling from rollouts".into()));
        }
        self.job = Some(Job {
            account_key: key,
            heading: o.heading.clone(),
            tab: o.tab.clone(),
            got: start(o.account_id.clone(), self.roots.clone()),
        });
        Some((o.tab.clone(), "backfilling from rollouts".into()))
    }

    fn poll(&mut self, h: &mut store::History, job: Job, t: &Thresholds, now: f64) -> Option<(String, String)> {
        let mk = meta_key(&job.account_key);
        let outcome = match job.got.try_recv() {
            Ok(o) => o,
            Err(mpsc::TryRecvError::Empty) => {
                let _ = h.meta_update(&mk, |v| v.and_then(parse_running).map(|_| running(now)));
                let tab = job.tab.clone();
                self.job = Some(job);
                return Some((tab, "backfilling from rollouts".into()));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                Outcome::Failed("the reading thread ended without a result".into())
            }
        };
        match outcome {
            Outcome::Done(rows, stats) => {
                match h.replay("codex", &job.account_key, &job.heading, &rows, t, now) {
                    Ok(n) => {
                        self.skipped = skipped_words(&stats);
                        let mut words = format!("{} backfilled from rollouts", group(n));
                        if let Some(s) = &self.skipped {
                            words.push_str(&format!(" · {s}"));
                        }
                        Some((job.tab, words))
                    }
                    // Another pane holds the file: the rows are kept for the
                    // next pass rather than read again.
                    Err(why) if store::busy(&why) => {
                        let (tx, rx) = mpsc::channel();
                        let _ = tx.send(Outcome::Done(rows, stats));
                        let tab = job.tab.clone();
                        self.job = Some(Job { got: rx, ..job });
                        Some((tab, "backfilling from rollouts".into()))
                    }
                    Err(why) => self.stop(h, &mk, job.tab, format!("not written: {why}")),
                }
            }
            Outcome::Failed(why) => self.stop(h, &mk, job.tab, why),
        }
    }

    /// Record why the backfill stopped, and let the claim go so another pane
    /// can try.
    fn stop(&mut self, h: &mut store::History, mk: &str, tab: String, why: String) -> Option<(String, String)> {
        let _ = h.meta_update(mk, |v| v.and_then(parse_running).map(|_| "stopped".to_string()));
        let words = format!("backfill stopped: {why}");
        self.failed = Some(why);
        Some((tab, words))
    }
}

/// `1,234`.
fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What a run left out, in a few words, or `None` when it left nothing out.
fn skipped_words(s: &Stats) -> Option<String> {
    let mut parts = Vec::new();
    if s.compressed > 0 {
        parts.push(format!("{} compressed rollouts not read", group(s.compressed as u64)));
    }
    if s.unreadable > 0 {
        parts.push(format!("{} unreadable", group(s.unreadable as u64)));
    }
    let lines = s.malformed_lines + s.oversize_lines;
    if lines > 0 {
        parts.push(format!("{} lines skipped", group(lines as u64)));
    }
    if s.untied > 0 {
        parts.push(format!("{} readings with no account", group(s.untied as u64)));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Read the rollouts on a thread of its own. A panic in it is caught and its
/// reason handed back, so a reader that dies says why rather than leaving
/// the line on "backfilling" for ever.
fn start(account_id: String, roots: Vec<PathBuf>) -> mpsc::Receiver<Outcome> {
    let (tx, rx) = mpsc::channel();
    let fallback = tx.clone();
    let spawned = std::thread::Builder::new()
        .name("codex-backfill".into())
        .spawn(move || {
            let read = std::panic::catch_unwind(|| collect(&account_id, &roots));
            let outcome = match read {
                Ok((rows, stats)) => Outcome::Done(rows, stats),
                Err(_) => {
                    let why = opscope_core::take_panic_reason()
                        .unwrap_or_else(|| "the rollout reader panicked".to_string());
                    Outcome::Failed(why)
                }
            };
            let _ = tx.send(outcome);
        });
    if let Err(e) = spawned {
        let _ = fallback.send(Outcome::Failed(format!("could not start a reading thread: {e}")));
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;


    const ACCT: &str = "acct-live";

    fn meta(who: Option<&str>) -> String {
        let who = who.map_or(String::new(), |w| format!(r#","creator_account_id":"{w}""#));
        format!(r#"{{"timestamp":"2026-09-10T08:00:00.000Z","type":"session_meta","payload":{{"id":"s"{who}}}}}"#)
    }

    fn snap(stamp: &str, pct: f64, reset: i64) -> String {
        format!(
            r#"{{"timestamp":"{stamp}","type":"event_msg","payload":{{"type":"token_count","info":null,"rate_limits":{{"limit_id":"codex","limit_name":null,"primary":{{"used_percent":{pct},"window_minutes":10080,"resets_at":{reset}}},"secondary":null,"plan_type":"pro"}}}}}}"#
        )
    }

    fn limits(line: &str) -> Vec<Replayed> {
        match parse_rollout_line(line) {
            RolloutLine::Limits(rows) => rows,
            other => panic!("not limits: {other:?}"),
        }
    }

    #[test]
    fn a_snapshot_is_read_at_its_own_time_into_the_live_pool_keys() {
        let rows = limits(&snap("2026-09-10T08:43:23.583Z", 5.0, 1_795_588_616));
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.pool_key, "primary_window");
        assert_eq!(r.label, "7d");
        assert_eq!(r.sample.source, Source::Backfill);
        assert_eq!(r.sample.pct, Some(5.0));
        assert_eq!(r.sample.window_secs, Some(604_800.0));
        assert_eq!(r.sample.reset_at, Some(1_795_588_616.0));
        assert_eq!(r.sample.at, crate::iso_epoch("2026-09-10T08:43:23.583Z").unwrap());
        // Both windows, the older shape under `info`, and a time left in
        // place of an instant.
        let old = r#"{"timestamp":"2026-07-01T00:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"rate_limits":{"primary":{"used_percent":40,"window_minutes":300,"resets_in_seconds":600},"secondary":{"used_percent":12.5,"window_minutes":10080,"resets_in_seconds":86400}}}}}"#;
        let rows = limits(old);
        let at = crate::iso_epoch("2026-07-01T00:00:00Z").unwrap();
        let keys: Vec<(&str, &str, Option<f64>)> = rows
            .iter()
            .map(|r| (r.pool_key.as_str(), r.label.as_str(), r.sample.reset_at))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("primary_window", "5h", Some(at + 600.0)),
                ("secondary_window", "7d", Some(at + 86400.0))
            ]
        );
    }

    #[test]
    fn a_feature_limit_lands_in_its_additional_pool_and_a_nameless_one_nowhere() {
        let line = r#"{"timestamp":"2026-10-01T00:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"codex_spark","limit_name":"GPT-5.3-Codex-Spark","primary":{"used_percent":3,"window_minutes":10080,"resets_at":1},"secondary":{"used_percent":9,"window_minutes":300,"resets_at":2}}}}"#;
        let rows = limits(line);
        assert_eq!(rows.len(), 1, "only the primary window, as the live lane reads it");
        assert_eq!(rows[0].pool_key, "additional:GPT-5.3-Codex-Spark");
        assert_eq!(rows[0].label, "Spark 7d");
        let nameless = line.replace(r#""GPT-5.3-Codex-Spark""#, "null");
        assert!(limits(&nameless).is_empty());
    }

    #[test]
    fn lines_that_are_not_readings_are_told_apart() {
        assert_eq!(parse_rollout_line(&meta(Some(ACCT))), RolloutLine::Meta(Some(ACCT.into())));
        assert_eq!(parse_rollout_line(&meta(None)), RolloutLine::Meta(None));
        assert_eq!(parse_rollout_line(r#"{"type":"response_item","payload":{}}"#), RolloutLine::Other);
        assert_eq!(parse_rollout_line(r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":"#), RolloutLine::Malformed);
        let bad_stamp = snap("yesterday", 5.0, 1);
        assert_eq!(parse_rollout_line(&bad_stamp), RolloutLine::Malformed);
        // A null percentage is no reading.
        let null = snap("2026-09-10T08:00:00Z", 5.0, 1).replace("\"used_percent\":5", "\"used_percent\":null");
        assert_eq!(parse_rollout_line(&null), RolloutLine::Limits(Vec::new()));
        // A message that merely mentions the word is not a reading.
        let chat = r#"{"type":"response_item","payload":{"type":"message","content":"what are \"rate_limits\"?"}}"#;
        assert_eq!(parse_rollout_line(chat), RolloutLine::Other);
        // A line's own type is read from its start, not from a payload that
        // quotes another rollout.
        let quoting = r#"{"timestamp":"2026-09-10T08:00:00Z","type":"response_item","payload":{"output":"{\"type\":\"session_meta\"}"}}"#;
        assert_eq!(header_type(quoting.as_bytes()), Some("response_item"));
        assert_eq!(header_type(meta(None).as_bytes()), Some("session_meta"));
        assert_eq!(header_type(b"no type here"), None);
    }

    #[test]
    fn the_flag_runs_it_once_per_version_and_waits_out_a_live_claim() {
        assert!(due(None, 0.0));
        assert!(!due(Some(&done(12)), 0.0));
        assert_eq!(parse_done(&done(12)), Some(12));
        // An older version's record runs it again.
        assert!(due(Some("done:0:12"), 0.0));
        assert!(due(Some("done"), 0.0));
        // A claim holds until its lease runs out.
        assert!(!due(Some(&running(1000.0)), 1000.0 + LEASE - 1.0));
        assert!(due(Some(&running(1000.0)), 1000.0 + LEASE));
        // A run that stopped can be taken up again.
        assert!(due(Some("stopped"), 0.0));
        assert_eq!(meta_key("abc"), "backfill:codex:abc");
    }

    fn reading(at: f64, pct: f64) -> Replayed {
        let mut r = limits(&snap("2026-09-10T08:00:00Z", pct, 2_000_000))[0].clone();
        r.sample.at = at;
        r
    }

    #[test]
    fn thinning_follows_the_write_rule() {
        let t0 = 1_000_000.0;
        // A reading every ten seconds for two hours, unchanged, then a
        // change a minute later and another six minutes after that.
        let mut rows: Vec<Replayed> = (0..=720).map(|i| reading(t0 + i as f64 * 10.0, 5.0)).collect();
        rows.push(reading(t0 + 7260.0, 6.0));
        rows.push(reading(t0 + 7260.0, 6.0));
        rows.push(reading(t0 + 7620.0, 7.0));
        rows.reverse();
        let kept: Vec<(f64, Option<f64>)> = thin(rows).iter().map(|r| (r.sample.at, r.sample.pct)).collect();
        assert_eq!(
            kept,
            vec![
                (t0, Some(5.0)),
                // The heartbeat.
                (t0 + 3600.0, Some(5.0)),
                (t0 + 7200.0, Some(5.0)),
                // The change a minute after a kept reading waits; the next
                // one clear of five minutes is kept.
                (t0 + 7620.0, Some(7.0)),
            ]
        );
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "opscope-backfill-{name}-{}-{}",
            std::process::id(),
            opscope_core::now().to_bits()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_this_accounts_sessions_are_replayed_and_bad_lines_are_counted_not_fatal() {
        let root = temp_dir("scan");
        let day = root.join("2026/09/10");
        std::fs::create_dir_all(&day).unwrap();
        let huge = format!(r#"{{"type":"response_item","payload":{{"text":"{}"}}}}"#, "x".repeat(MAX_LINE + 10));
        let mine = [
            meta(Some(ACCT)),
            snap("2026-09-10T08:00:00Z", 5.0, 2_000_000),
            "not json at all {\"rate_limits\"".to_string(),
            huge,
            "\u{0}\u{1}garbage".to_string(),
            snap("2026-09-10T09:00:00Z", 9.0, 2_000_000),
        ]
        .join("\n");
        std::fs::write(day.join("rollout-a.jsonl"), mine).unwrap();
        // Another account's session, and one that names nobody.
        std::fs::write(
            day.join("rollout-b.jsonl"),
            [meta(Some("someone-else")), snap("2026-09-10T10:00:00Z", 50.0, 2_000_000)].join("\n"),
        )
        .unwrap();
        std::fs::write(
            day.join("rollout-c.jsonl"),
            [snap("2026-09-10T10:00:00Z", 1.0, 2_000_000), meta(None), snap("2026-09-10T11:00:00Z", 2.0, 2_000_000)]
                .join("\n"),
        )
        .unwrap();
        // A forked session: this account's header, then another's.
        std::fs::write(
            day.join("rollout-d.jsonl"),
            [
                meta(Some(ACCT)),
                snap("2026-09-10T12:00:00Z", 12.0, 2_000_000),
                meta(Some("someone-else")),
                snap("2026-09-10T13:00:00Z", 70.0, 2_000_000),
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(day.join("rollout-e.jsonl.zst"), b"\x28\xb5\x2f\xfd").unwrap();
        let (rows, stats) = collect(ACCT, &[root.clone(), root.join("missing")]);
        let pcts: Vec<f64> = rows.iter().filter_map(|r| r.sample.pct).collect();
        assert_eq!(pcts, vec![5.0, 9.0, 12.0]);
        assert_eq!(
            stats,
            Stats {
                files: 4,
                unreadable: 0,
                compressed: 1,
                malformed_lines: 1,
                oversize_lines: 1,
                untied: 2,
                other_account: 2,
                readings: 3,
            }
        );
        // No account, nothing read.
        assert!(collect("", &[root]).0.is_empty());
    }

    #[test]
    fn a_long_line_is_cut_and_the_next_one_still_reads() {
        let body = format!("{}\nshort\nlast", "y".repeat(100));
        let mut r = std::io::BufReader::with_capacity(16, body.as_bytes());
        let mut buf = Vec::new();
        assert_eq!(read_capped(&mut r, &mut buf, 10).unwrap(), Some(true));
        assert_eq!(buf.len(), 10);
        assert_eq!(read_capped(&mut r, &mut buf, 10).unwrap(), Some(false));
        assert_eq!(buf, b"short");
        assert_eq!(read_capped(&mut r, &mut buf, 10).unwrap(), Some(false));
        assert_eq!(buf, b"last");
        assert_eq!(read_capped(&mut r, &mut buf, 10).unwrap(), None);
    }

    #[test]
    fn counts_are_grouped_and_skips_said_in_a_few_words() {
        assert_eq!(group(0), "0");
        assert_eq!(group(1234), "1,234");
        assert_eq!(group(1_234_567), "1,234,567");
        assert_eq!(skipped_words(&Stats::default()), None);
        let s = Stats {
            compressed: 1430,
            oversize_lines: 1,
            malformed_lines: 2,
            ..Stats::default()
        };
        assert_eq!(skipped_words(&s).unwrap(), "1,430 compressed rollouts not read, 3 lines skipped");
    }
}
