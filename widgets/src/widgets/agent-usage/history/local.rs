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

//! When local work could have drawn overage (spec §6.3, source 3; OPS-309).
//!
//! A spend bracket after an offline gap says only "somewhere in these
//! hours". This machine's own agent logs carry a timestamp per model
//! response, so inside a long bracket the turns this machine made show
//! *when* local work could have drawn the spend. It is **activity, never an
//! amount**: nothing here divides the bracket's spend among turns, and spend
//! from other devices, the web app or another config dir is invisible to
//! it, which is why every line it puts on screen says *local*.
//!
//! **Which turns.** Only the provider's own agent: Claude Code transcripts
//! for Claude's extra usage, Codex rollouts for Codex credits. A provider
//! with no local agent (Cursor, Grok, Copilot, SpaceXSI and the rest) gets
//! no brackets at all. A turn is one model response that billed tokens:
//! a Claude `assistant` line with `usage`, de-duplicated on its message id
//! (Claude writes one line per content block of a response); a Codex
//! `token_count` event whose running total moved. Claude transcripts are
//! those under the projects directory of each config dir signed in as the
//! account, and only in brackets that began after it was: a transcript
//! names no account, so a dir signed in as someone else before holds their
//! turns. When it changed is the first pass that saw the dir as this
//! account ([`History::signed_in_since`]); where that was never recorded,
//! the dir counts in no bracket before the next pass, which records it.
//! A bracket the change fell inside says nothing. Codex rollouts count only
//! in sessions whose `session_meta` names the account, as the backfill does,
//! so another account's turns never land in this one's bracket.
//!
//! **Bounded reads.** A bracket is read once and cached under its agent,
//! account and span, while a tab asks for it and for [`EVICT_AFTER`] past
//! the last pass that did: a reading taken at `at_hi` closed it, so no
//! later write can add a turn inside it. Only logs written to since `at_lo` are opened; in a
//! transcript over [`WHOLE_BELOW`], the start of the bracket is found by a
//! binary search on the lines' own timestamps. A Codex rollout is always
//! read from its first line: whose the turns are is the latest header above
//! them, which can sit anywhere in the file, and whether a `token_count` is
//! a turn depends on the total before it. Either read stops at the first
//! line past the bracket. Lines are read through a fixed buffer and capped
//! at [`MAX_LINE`], so memory follows the buffer, never the file (the rule
//! OPS-207 and OPS-208 set for the token history). All of it happens on a
//! thread of its own, one job at a time, so a pass never waits for it.
//!
//! **What is never said.** A bracket with no local turns draws nothing,
//! not "no local turns": Claude Code deletes transcripts after its cleanup
//! period, and an absent log is indistinguishable from an idle machine. A
//! read that ran past [`JOB_BYTES`], a listing cut short or a log that
//! failed part-way draws nothing either, rather than a count that might be
//! partial. A bracket the budget ran out on only because another in the
//! same job spent it, or whose listing was cut short, is not kept as that
//! answer: it is asked for again on a later pass.

use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::sync::mpsc;

use rusqlite::params;

use super::Observation;
use super::chart::TabHistory;
use super::store::History;

/// A bracket this wide or wider is worth narrowing; a shorter one was
/// watched to within the poll interval and needs no help.
pub const LONG: f64 = 3600.0;
/// The newest long brackets of a pool given a line each.
pub const MOST_BRACKETS: usize = 3;
/// How far before a bracket's start the read begins, for lines written a
/// little out of order.
const SLACK: f64 = 600.0;
/// A log this small is read from its first line rather than searched.
pub const WHOLE_BELOW: u64 = 8 << 20;
/// The longest line read whole; a longer one is a pasted file and is
/// skipped, never held.
const MAX_LINE: usize = 8 << 20;
/// The most one job reads across every log. Past it the job's brackets are
/// left unsaid rather than given a count that may be short.
const JOB_BYTES: u64 = 512 << 20;
/// How long a read bracket no tab asked for is kept. A pass that misses a
/// tab, or one of its log directories, does not cost the reads again.
const EVICT_AFTER: f64 = 600.0;

/// The agent whose local logs answer for a provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Agent {
    ClaudeCode,
    Codex,
}

impl Agent {
    /// The local agent of a provider, or `None` for one with no local agent
    /// whose logs this reads.
    pub fn of(provider: &str) -> Option<Agent> {
        match provider {
            "claude" => Some(Agent::ClaudeCode),
            "codex" => Some(Agent::Codex),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
        }
    }
}

/// The turns one agent made on this machine inside one bracket.
#[derive(Clone, Debug, PartialEq)]
pub struct Activity {
    pub agent: Agent,
    pub first: f64,
    pub last: f64,
    pub turns: usize,
}

/// What this machine's logs say about one bracket.
#[derive(Clone, Debug, PartialEq)]
pub enum Local {
    /// Not read yet.
    Unread,
    /// Turns found.
    Found(Activity),
    /// No turn found, or none that could be counted whole. Drawn as nothing.
    Nothing,
    /// The reading thread stopped, and why.
    Failed(String),
}

/// One long spend bracket of an overage or balance pool, drawn while a
/// linked allowance was used up.
#[derive(Clone, Debug, PartialEq)]
pub struct Bracket {
    pub at_lo: f64,
    pub at_hi: f64,
    /// The span's own amount, in the pool's unit: the stored figure.
    pub amount: f64,
    pub local: Local,
}

/// **The matcher**: the turns at `turns` (epoch seconds, any order) that
/// fall inside `at_lo..=at_hi`, as a range and a count. `None` when none do.
pub fn activity(agent: Agent, turns: &[f64], at_lo: f64, at_hi: f64) -> Option<Activity> {
    let inside: Vec<f64> = turns.iter().copied().filter(|t| *t >= at_lo && *t <= at_hi).collect();
    let first = inside.iter().copied().fold(f64::INFINITY, f64::min);
    let last = inside.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (!inside.is_empty()).then_some(Activity {
        agent,
        first,
        last,
        turns: inside.len(),
    })
}

// ----- parsers: always compiled, always tested -----

/// A line's own top-level `timestamp`, for the search. Lines past 256 KiB
/// are not parsed for it.
pub fn parse_line_time(line: &str) -> Option<f64> {
    if line.len() > 256 << 10 || !line.contains("\"timestamp\"") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    crate::iso_epoch(v["timestamp"].as_str()?)
}

/// One Claude Code transcript line as a turn: its message id (the key
/// that makes a response's content blocks one turn) and when. `None` for
/// anything that is not a billed response.
pub fn parse_claude_turn(line: &str) -> Option<(String, f64)> {
    if !line.contains("\"usage\"") || !line.contains("\"assistant\"") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v["type"].as_str() != Some("assistant") {
        return None;
    }
    let msg = &v["message"];
    // An error Claude Code wrote itself, not a response.
    if !msg["usage"].is_object() || msg["model"].as_str().is_none_or(|m| m.is_empty() || m == "<synthetic>") {
        return None;
    }
    let at = crate::iso_epoch(v["timestamp"].as_str()?)?;
    let key = msg["id"]
        .as_str()
        .or_else(|| v["uuid"].as_str())
        .filter(|k| !k.is_empty())?;
    Some((key.to_string(), at))
}

/// What one Codex rollout line says to the turn count.
#[derive(Clone, Debug, PartialEq)]
pub enum CodexLine {
    /// A `session_meta`, with the account that created the session.
    Meta(Option<String>),
    /// A `token_count` event with usage: when, and its running total.
    Turn { at: f64, total: Option<f64> },
    Other,
}

pub fn parse_codex_line(line: &str) -> CodexLine {
    if !line.contains("\"token_count\"") && !line.contains("\"session_meta\"") {
        return CodexLine::Other;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return CodexLine::Other;
    };
    let payload = &v["payload"];
    if v["type"].as_str() == Some("session_meta") {
        let who = payload["creator_account_id"].as_str().filter(|s| !s.is_empty());
        return CodexLine::Meta(who.map(str::to_string));
    }
    // An event with no `info` carries only rate limits: no response.
    if payload["type"].as_str() != Some("token_count") || !payload["info"].is_object() {
        return CodexLine::Other;
    }
    let Some(at) = v["timestamp"].as_str().and_then(crate::iso_epoch) else {
        return CodexLine::Other;
    };
    let total = payload["info"]["total_token_usage"]["total_tokens"].as_f64();
    CodexLine::Turn { at, total }
}

// ----- reading -----

/// Read one line into `buf`, keeping at most `cap` bytes. `None` at the end,
/// `Some(true)` when the line was longer than `cap`; returns the bytes the
/// line took on disk as well. Reading stops after `limit` bytes even inside
/// a line, which is then `over`: a line longer than what is left of the
/// budget is never read whole to find out.
fn read_capped<R: BufRead>(
    r: &mut R,
    buf: &mut Vec<u8>,
    cap: usize,
    limit: u64,
) -> std::io::Result<Option<(bool, u64)>> {
    buf.clear();
    let (mut over, mut any, mut took) = (false, false, 0u64);
    loop {
        if took >= limit {
            return Ok(Some((true, took)));
        }
        let avail = match r.fill_buf() {
            Ok(a) => a,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if avail.is_empty() {
            return Ok(any.then_some((over, took)));
        }
        any = true;
        let avail = &avail[..avail.len().min(usize::try_from(limit - took).unwrap_or(usize::MAX))];
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
        took += used as u64;
        if end.is_some() {
            return Ok(Some((over, took)));
        }
    }
}

/// The first line timestamp at or after byte `off`, skipping the line `off`
/// lands inside. Looks at a few dozen lines at most.
fn time_after(f: &std::fs::File, off: u64, spent: &mut u64) -> Option<f64> {
    let mut f = f;
    f.seek(SeekFrom::Start(off)).ok()?;
    let mut r = std::io::BufReader::with_capacity(1 << 16, f);
    let mut buf = Vec::new();
    if off > 0 {
        let (_, took) = read_capped(&mut r, &mut buf, 0, u64::MAX).ok()??;
        *spent += took;
    }
    for _ in 0..64 {
        let (over, took) = read_capped(&mut r, &mut buf, 256 << 10, u64::MAX).ok()??;
        *spent += took;
        if !over {
            if let Some(t) = parse_line_time(&String::from_utf8_lossy(&buf)) {
                return Some(t);
            }
        }
    }
    None
}

/// A byte offset at or before the first line stamped `target` or later.
fn seek_before(f: &std::fs::File, size: u64, target: f64, spent: &mut u64) -> u64 {
    let (mut lo, mut hi) = (0u64, size);
    while hi - lo > 1 << 16 {
        let mid = lo + (hi - lo) / 2;
        match time_after(f, mid, spent) {
            Some(t) if t < target => lo = mid,
            _ => hi = mid,
        }
    }
    lo
}

/// One bracket of one account's agent: what a read is cached under. The
/// directories it is read from are not part of it: a closed bracket's
/// answer does not change, and a pass that misses a directory must not
/// throw the answer away.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub agent: Agent,
    /// The history's key for the account.
    pub account: String,
    /// The account the Codex sessions must name; empty for Claude.
    pub account_id: String,
    lo: u64,
    hi: u64,
}

impl Key {
    pub fn new(agent: Agent, account: String, account_id: String, at_lo: f64, at_hi: f64) -> Key {
        Key { agent, account, account_id, lo: at_lo.to_bits(), hi: at_hi.to_bits() }
    }
    fn at_lo(&self) -> f64 {
        f64::from_bits(self.lo)
    }
    fn at_hi(&self) -> f64 {
        f64::from_bits(self.hi)
    }
}

/// What a rollout line says about whose the session is, parsing nothing
/// but a header: `Some(who)` for a `session_meta` (`None` inside when it
/// names nobody, or is too long to read whole, so the account is unknown
/// rather than the previous one's), `None` for any other line. The line's
/// own `type` is written first, so a header is looked for at its start
/// only, and a line quoting one further in is not taken for one.
fn codex_header(buf: &[u8], over: bool) -> Option<Option<String>> {
    let head = &buf[..buf.len().min(512)];
    if !head.windows(14).any(|w| w == b"\"session_meta\"") {
        return None;
    }
    if over {
        return Some(None);
    }
    match parse_codex_line(&String::from_utf8_lossy(buf)) {
        CodexLine::Meta(who) => Some(who),
        _ => None,
    }
}

/// Why a log was not read to the end of the bracket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cut {
    /// The job's byte budget ran out first.
    Budget,
    /// A read failed: what was read before it is not the whole bracket.
    Failed,
}

/// The turns one log holds between `lo` and `hi`, keyed for de-duplication,
/// appended to `out`. An error when the log was not read to the end of the
/// bracket, so what is in `out` may be short.
///
/// A Codex rollout is always read from its first line. Whose a turn is is
/// the latest `session_meta` above it, which can sit anywhere in the file,
/// and whether a `token_count` is a turn at all depends on the total before
/// it, so a read begun part-way through would have to carry both across the
/// point it began at. Read from the top, they carry as in any whole read,
/// for no more bytes than a scan of the headers above the bracket took.
fn file_turns(path: &str, key: &Key, budget: &mut u64, out: &mut HashMap<String, f64>) -> Result<(), Cut> {
    let Ok(file) = std::fs::File::open(path) else {
        // Gone between the listing and now: nothing in it to count.
        return Ok(());
    };
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let (lo, hi) = (key.at_lo(), key.at_hi());
    let mut spent = 0u64;
    let mut session: Option<Option<String>> = None;
    let start = if size <= WHOLE_BELOW || key.agent == Agent::Codex {
        0
    } else {
        seek_before(&file, size, lo - SLACK, &mut spent)
    };
    let mut f = &file;
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Err(Cut::Failed);
    }
    let mut r = std::io::BufReader::with_capacity(1 << 16, f);
    let mut buf = Vec::new();
    if start > 0 {
        // The line `start` lands inside, which is before the bracket.
        match read_capped(&mut r, &mut buf, 0, u64::MAX) {
            Ok(Some((_, took))) => spent += took,
            Ok(None) => {}
            Err(_) => return Err(Cut::Failed),
        }
    }
    let mut last_total: Option<f64> = None;
    loop {
        if spent > *budget {
            *budget = 0;
            return Err(Cut::Budget);
        }
        // One byte past the budget, so a line it cuts short stops the read
        // above rather than being read whole first.
        let left = *budget - spent + 1;
        let (over, took) = match read_capped(&mut r, &mut buf, MAX_LINE, left) {
            Ok(Some(x)) => x,
            Ok(None) => break,
            Err(_) => return Err(Cut::Failed),
        };
        spent += took;
        if over {
            // A header too long to read leaves the account unknown.
            if key.agent == Agent::Codex && codex_header(&buf, true).is_some() {
                session = Some(None);
            }
            continue;
        }
        let line = String::from_utf8_lossy(&buf);
        match key.agent {
            Agent::ClaudeCode => {
                if let Some((id, at)) = parse_claude_turn(&line) {
                    if at > hi + SLACK {
                        break;
                    }
                    if at >= lo && at <= hi {
                        out.insert(id, at);
                    }
                } else if parse_line_time(&line).is_some_and(|t| t > hi + SLACK) {
                    break;
                }
            }
            Agent::Codex => match parse_codex_line(&line) {
                CodexLine::Meta(who) => session = Some(who),
                CodexLine::Turn { at, total } => {
                    if at > hi + SLACK {
                        break;
                    }
                    // The same total again is the event repeated, not a turn.
                    let moved = total.is_none() || total != last_total;
                    last_total = total;
                    let ours = matches!(&session, Some(Some(who)) if *who == key.account_id);
                    if moved && ours && at >= lo && at <= hi {
                        out.insert(format!("{at}:{}", total.unwrap_or(-1.0)), at);
                    }
                }
                CodexLine::Other => {}
            },
        }
    }
    *budget = budget.saturating_sub(spent);
    Ok(())
}

/// Read one bracket from this machine's logs under `dirs`. `Unread` when it
/// could not be read whole for now (the budget ran out, or a listing was cut
/// short): not an answer, so it is asked for again rather than kept.
/// `Nothing` when no turn was found, or a log could not be read.
pub fn read_bracket(key: &Key, dirs: &[String], now: f64, budget: &mut u64) -> Local {
    if key.agent == Agent::Codex && key.account_id.is_empty() {
        return Local::Nothing;
    }
    let mut turns: HashMap<String, f64> = HashMap::new();
    for dir in dirs {
        // Only logs written to since the bracket began can hold a turn in it.
        let (listed, complete) = crate::shared::list_logs(dir, ".jsonl", Some(now - key.at_lo() + SLACK), now);
        if !complete {
            return Local::Unread;
        }
        for l in &listed {
            match file_turns(&l.path, key, budget, &mut turns) {
                Ok(()) => {}
                Err(Cut::Budget) => return Local::Unread,
                Err(Cut::Failed) => return Local::Nothing,
            }
        }
    }
    let at: Vec<f64> = turns.into_values().collect();
    activity(key.agent, &at, key.at_lo(), key.at_hi()).map_or(Local::Nothing, Local::Found)
}

/// A bracket to read, and the directories to read it from.
type Job = (Key, Vec<String>);

type Outcome = Result<Vec<(Job, Local)>, String>;

/// One bracket read: what was found, from which directories, and when a
/// tab last asked for it.
struct Read {
    local: Local,
    dirs: Vec<String>,
    used: f64,
}

/// Read a job's brackets out of one `budget`. A bracket the budget ran out
/// on after another had spent some of it is left `Unread`, for a job of its
/// own; one a whole budget could not cover is `Nothing`, since another job
/// would only run out on it again.
fn read_job(keys: Vec<Job>, now: f64, mut budget: u64) -> Vec<(Job, Local)> {
    let whole = budget;
    keys.into_iter()
        .map(|(k, dirs)| {
            let fresh = budget == whole;
            let mut got = read_bracket(&k, &dirs, now, &mut budget);
            if got == Local::Unread && fresh && budget == 0 {
                got = Local::Nothing;
            }
            ((k, dirs), got)
        })
        .collect()
}

fn start(keys: Vec<Job>, now: f64) -> mpsc::Receiver<Outcome> {
    let (tx, rx) = mpsc::channel();
    let fallback = tx.clone();
    let spawned = std::thread::Builder::new().name("local-turns".into()).spawn(move || {
        let read = std::panic::catch_unwind(|| {
            read_job(keys, now, JOB_BYTES)
        });
        let outcome = read.map_err(|_| {
            opscope_core::take_panic_reason().unwrap_or_else(|| "the local log reader panicked".to_string())
        });
        let _ = tx.send(outcome);
    });
    if let Err(e) = spawned {
        let _ = fallback.send(Err(format!("could not start a reading thread: {e}")));
    }
    rx
}

/// The poller's handle on the local turn reads: hands each tab's brackets
/// what is known of them, and reads the rest on a thread.
#[derive(Default)]
pub struct Reader {
    cache: HashMap<Key, Read>,
    job: Option<mpsc::Receiver<Outcome>>,
    /// Why the reading thread stopped. Not started again in this pane: a
    /// reader that panicked would panic again.
    failed: Option<String>,
}

impl Reader {
    /// Fill in the brackets of `charts` from what has been read, and start
    /// reading the ones nobody has.
    pub fn tick(&mut self, h: &History, charts: &mut HashMap<String, TabHistory>, obs: &[Observation], now: f64) {
        if let Some(rx) = self.job.take() {
            match rx.try_recv() {
                // A bracket left unread is not an answer: the next pass
                // asks for it again.
                Ok(Ok(got)) => self.cache.extend(
                    got.into_iter()
                        .filter(|(_, local)| *local != Local::Unread)
                        .map(|((k, dirs), local)| (k, Read { local, dirs, used: now })),
                ),
                Ok(Err(why)) => self.failed = Some(why),
                Err(mpsc::TryRecvError::Empty) => self.job = Some(rx),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.failed = Some("the reading thread ended without a result".into());
                }
            }
        }
        let mut misses: Vec<Job> = Vec::new();
        for (tab, th) in charts.iter_mut() {
            let Some(o) = obs.iter().find(|o| &o.tab == tab && Agent::of(&o.provider).is_some()) else {
                continue;
            };
            let Some(agent) = Agent::of(&o.provider) else { continue };
            // Every config dir signed in as this account, not only the tab's
            // own: two dirs on one account share one history.
            // One filed under no account has no history of its own to name
            // turns in, and must not pool with another such reading.
            let Some(account) = h.account_key_of(o) else { continue };
            // Each with when its config dir was signed in as the account: a
            // Claude transcript names no account, so a dir's turns are the
            // account's only from then. A Codex rollout names its own, so
            // every one is read and its sessions decide.
            let mut dirs: Vec<(String, f64)> = obs
                .iter()
                .filter(|x| x.provider == o.provider && h.account_key_of(x).as_ref() == Some(&account))
                .filter_map(|x| match agent {
                    Agent::Codex => Some((x, f64::NEG_INFINITY)),
                    Agent::ClaudeCode => h.signed_in_since(x, &account).map(|since| (x, since)),
                })
                .flat_map(|(x, since)| x.local_logs.iter().map(move |d| (d.clone(), since)))
                .collect();
            dirs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
            dirs.dedup();
            let account_id = if agent == Agent::Codex { o.account_id.clone() } else { String::new() };
            for pool in &mut th.pools {
                for b in &mut pool.brackets {
                    if let Some(why) = &self.failed {
                        b.local = Local::Failed(why.clone());
                        continue;
                    }
                    // A dir signed in as the account during the bracket
                    // held someone else's turns before that, and which are
                    // which cannot be told: the bracket says nothing.
                    if dirs.iter().any(|(_, since)| *since > b.at_lo && *since <= b.at_hi) {
                        b.local = Local::Nothing;
                        continue;
                    }
                    let mut dirs: Vec<String> =
                        dirs.iter().filter(|(_, since)| *since <= b.at_lo).map(|(d, _)| d.clone()).collect();
                    dirs.dedup();
                    let k = Key::new(agent, account.clone(), account_id.clone(), b.at_lo, b.at_hi);
                    match self.cache.get_mut(&k) {
                        // A pass missing a directory keeps the answer; one
                        // with a directory the read did not cover reads
                        // again, rather than show a count that leaves it out.
                        Some(r) if dirs.iter().all(|d| r.dirs.contains(d)) => {
                            b.local = r.local.clone();
                            r.used = now;
                        }
                        _ => {
                            b.local = Local::Unread;
                            if !misses.iter().any(|(m, _)| *m == k) {
                                misses.push((k, dirs.clone()));
                            }
                        }
                    }
                }
            }
        }
        self.cache.retain(|_, r| now - r.used <= EVICT_AFTER);
        if self.job.is_none() && self.failed.is_none() && !misses.is_empty() {
            self.job = Some(start(misses, now));
        }
    }
}

impl History {
    /// The newest [`MOST_BRACKETS`] spans of `pool_key` at least [`LONG`]
    /// wide that were drawn while a linked allowance was used up, oldest
    /// first, with nothing read about them yet.
    pub fn long_brackets(&self, provider: &str, account_key: &str, pool_key: &str) -> Result<Vec<Bracket>, String> {
        let mut st = self
            .conn
            .prepare(
                "SELECT s.at_lo, s.at_hi, s.amount FROM overage_span s
                   JOIN pool p ON p.id = s.overage_id JOIN account a ON a.id = p.account_id
                  WHERE a.provider = ?1 AND a.account_key = ?2 AND p.pool_key = ?3
                    AND s.at_hi - s.at_lo >= ?4 AND s.exhausted <> '[]' AND s.amount > 0
                  ORDER BY s.at_hi DESC LIMIT ?5",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![provider, account_key, pool_key, LONG, MOST_BRACKETS as i64], |r| {
                Ok(Bracket {
                    at_lo: r.get(0)?,
                    at_hi: r.get(1)?,
                    amount: r.get(2)?,
                    local: Local::Unread,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out: Vec<Bracket> = rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        out.reverse();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: f64 = 1_790_000_000.0;

    fn iso(at: f64) -> String {
        chrono::DateTime::from_timestamp(at as i64, 0).unwrap().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
    }

    fn claude_line(id: &str, at: f64) -> String {
        serde_json::json!({
            "type": "assistant",
            "uuid": format!("u-{id}-{at}"),
            "timestamp": iso(at),
            "message": { "id": id, "model": "claude-x", "usage": { "input_tokens": 3, "output_tokens": 5 } },
        })
        .to_string()
    }

    fn codex_meta(who: &str) -> String {
        serde_json::json!({ "timestamp": iso(T - 86_400.0), "type": "session_meta", "payload": { "creator_account_id": who } }).to_string()
    }

    fn codex_turn(at: f64, total: f64) -> String {
        serde_json::json!({
            "timestamp": iso(at),
            "type": "event_msg",
            "payload": { "type": "token_count", "info": { "total_token_usage": { "total_tokens": total } } },
        })
        .to_string()
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("opscope-local-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_matcher_gives_the_range_and_count_inside_the_bracket_only() {
        let turns = [T - 10.0, T + 600.0, T + 60.0, T + 7200.0, T + 9000.0];
        let got = activity(Agent::ClaudeCode, &turns, T, T + 7200.0).unwrap();
        assert_eq!(got, Activity { agent: Agent::ClaudeCode, first: T + 60.0, last: T + 7200.0, turns: 3 });
        assert_eq!(activity(Agent::Codex, &turns, T + 7300.0, T + 8000.0), None);
        assert_eq!(activity(Agent::Codex, &[], T, T + 1.0), None);
    }

    #[test]
    fn only_a_provider_with_a_local_agent_has_one() {
        assert_eq!(Agent::of("claude"), Some(Agent::ClaudeCode));
        assert_eq!(Agent::of("codex"), Some(Agent::Codex));
        for p in ["cursor", "grok", "copilot", "spacexsi", "antigravity", "notion", "perplexity", "devin", "droid"] {
            assert_eq!(Agent::of(p), None, "{p}");
        }
    }

    #[test]
    fn a_claude_response_is_one_turn_however_many_lines_it_took() {
        assert_eq!(parse_claude_turn(&claude_line("m1", T)), Some(("m1".into(), T)));
        let user = serde_json::json!({ "type": "user", "timestamp": iso(T), "message": { "usage": {} } }).to_string();
        assert_eq!(parse_claude_turn(&user), None);
        let synthetic = claude_line("m2", T).replace("claude-x", "<synthetic>");
        assert_eq!(parse_claude_turn(&synthetic), None);
        assert_eq!(parse_line_time(&claude_line("m1", T)), Some(T));
    }

    #[test]
    fn a_codex_line_is_a_session_header_a_turn_or_nothing() {
        assert_eq!(parse_codex_line(&codex_meta("acct")), CodexLine::Meta(Some("acct".into())));
        assert_eq!(parse_codex_line(&codex_turn(T, 50.0)), CodexLine::Turn { at: T, total: Some(50.0) });
        let limits_only = serde_json::json!({ "timestamp": iso(T), "type": "event_msg", "payload": { "type": "token_count", "info": null } }).to_string();
        assert_eq!(parse_codex_line(&limits_only), CodexLine::Other);
    }

    #[test]
    fn claude_turns_are_read_from_the_bracket_once_each() {
        let d = dir("claude");
        let mut body = String::new();
        for (i, at) in [T - 7200.0, T + 300.0, T + 300.0, T + 3600.0, T + 9000.0].iter().enumerate() {
            // The second line repeats the first response's id: one turn.
            let id = if i == 2 { "m1".to_string() } else { format!("m{i}") };
            body.push_str(&claude_line(&id, *at));
            body.push('\n');
        }
        std::fs::create_dir_all(d.join("p")).unwrap();
        std::fs::write(d.join("p/s.jsonl"), body).unwrap();
        let dirs: Vec<String> = vec![d.to_string_lossy().into()];
        let key = Key::new(Agent::ClaudeCode, "k".into(), String::new(), T, T + 5400.0);
        let got = read_bracket(&key, &dirs, opscope_core::now(), &mut JOB_BYTES.clone());
        assert_eq!(got, Local::Found(Activity { agent: Agent::ClaudeCode, first: T + 300.0, last: T + 3600.0, turns: 2 }));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn codex_turns_count_only_in_the_accounts_own_sessions() {
        let d = dir("codex");
        let mine = [codex_meta("me"), codex_turn(T + 60.0, 10.0), codex_turn(T + 61.0, 10.0), codex_turn(T + 120.0, 20.0)].join("\n");
        let theirs = [codex_meta("someone-else"), codex_turn(T + 90.0, 5.0)].join("\n");
        std::fs::write(d.join("a.jsonl"), mine).unwrap();
        std::fs::write(d.join("b.jsonl"), theirs).unwrap();
        let dirs = vec![d.to_string_lossy().to_string()];
        let key = Key::new(Agent::Codex, "k".into(), "me".into(), T, T + 4000.0);
        let got = read_bracket(&key, &dirs, opscope_core::now(), &mut JOB_BYTES.clone());
        assert_eq!(got, Local::Found(Activity { agent: Agent::Codex, first: T + 60.0, last: T + 120.0, turns: 2 }));
        // No account to tie a rollout to: nothing, never every session.
        let blind = Key::new(Agent::Codex, "k".into(), String::new(), T, T + 4000.0);
        assert_eq!(read_bracket(&blind, &dirs, opscope_core::now(), &mut JOB_BYTES.clone()), Local::Nothing);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_bracket_with_no_local_turns_is_nothing_not_zero() {
        let d = dir("none");
        std::fs::write(d.join("s.jsonl"), claude_line("m", T - 9000.0)).unwrap();
        let dirs: Vec<String> = vec![d.to_string_lossy().into()];
        let key = Key::new(Agent::ClaudeCode, "k".into(), String::new(), T, T + 5400.0);
        assert_eq!(read_bracket(&key, &dirs, opscope_core::now(), &mut JOB_BYTES.clone()), Local::Nothing);
        let _ = std::fs::remove_dir_all(&d);
    }

    fn store(name: &str) -> (std::path::PathBuf, History) {
        let d = dir(name);
        let h = History::open(&d.join(super::super::store::FILE)).unwrap();
        h.conn
            .execute_batch(
                "INSERT INTO account (id, provider, account_key, heading, first_seen, last_seen) VALUES (1, 'claude', 'k', 'CLAUDE', 0, 0);
                 INSERT INTO account (id, provider, account_key, heading, first_seen, last_seen)
                   VALUES (2, 'claude', 'profile:default', 'CLAUDE', 0, 0);
                 INSERT INTO pool (id, account_id, pool_key, label, role, unit, first_seen, last_seen) VALUES
                   (1, 1, 'weekly', 'overall 7d', 'included', 'pct', 0, 0),
                   (2, 1, 'extra', 'extra A$50', 'overage', 'money', 0, 0);",
            )
            .unwrap();
        (d, h)
    }

    #[test]
    fn only_long_brackets_drawn_while_an_allowance_was_used_up_are_given_lines() {
        let (d, h) = store("brackets");
        let span = |lo: f64, hi: f64, exhausted: &str| {
            h.conn
                .execute(
                    "INSERT INTO overage_span (overage_id, at_lo, at_hi, amount, exhausted) VALUES (2, ?1, ?2, 1.5, ?3)",
                    params![lo, hi, exhausted],
                )
                .unwrap();
        };
        span(T, T + 600.0, "[1]"); // watched: short
        span(T + 1000.0, T + 9000.0, "[]"); // no allowance used up
        for i in 0..4 {
            let lo = T + 10_000.0 * (i + 2) as f64;
            span(lo, lo + LONG, "[1]");
        }
        let got = h.long_brackets("claude", "k", "extra").unwrap();
        let starts: Vec<f64> = got.iter().map(|b| b.at_lo).collect();
        assert_eq!(starts, vec![T + 30_000.0, T + 40_000.0, T + 50_000.0]);
        assert!(got.iter().all(|b| b.local == Local::Unread && b.amount == 1.5));
        assert!(h.long_brackets("claude", "k", "weekly").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// One tab's chart with one long bracket at `T`, nothing read about it.
    fn one_bracket_chart() -> HashMap<String, TabHistory> {
        bracket_chart(T, T + LONG)
    }

    fn bracket_chart(at_lo: f64, at_hi: f64) -> HashMap<String, TabHistory> {
        use super::super::chart::{Body, PoolHistory};
        let pool = PoolHistory {
            label: "extra".into(),
            role: super::super::fold::Role::Overage,
            unit: "money".into(),
            currency: None,
            exponent: None,
            total: 1,
            since: None,
            body: Body::Overage { bars: Vec::new(), roll_day: None },
            brackets: vec![Bracket { at_lo, at_hi, amount: 2.0, local: Local::Unread }],
        };
        HashMap::from([("claude".to_string(), TabHistory { pools: vec![pool], expired: Vec::new() })])
    }

    /// Tick until the bracket of [`one_bracket_chart`] is read.
    fn tick_until_read(r: &mut Reader, h: &History, obs: &[Observation], now: f64) -> Local {
        tick_until_read_of(r, h, obs, now, (T, T + LONG))
    }

    fn tick_until_read_of(r: &mut Reader, h: &History, obs: &[Observation], now: f64, b: (f64, f64)) -> Local {
        for _ in 0..500 {
            let mut c = bracket_chart(b.0, b.1);
            r.tick(h, &mut c, obs, now);
            let got = c["claude"].pools[0].brackets[0].local.clone();
            if got != Local::Unread {
                return got;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Local::Unread
    }

    #[test]
    fn a_config_dir_s_turns_count_only_from_when_it_was_signed_in_as_the_account() {
        // Review on OPS-309: transcripts were bound to the account a config
        // dir is signed in as now. A dir signed in as someone else when a
        // bracket was drawn put that account's turns in this one's bracket.
        let d = dir("reader-switch");
        let mut h = History::open(&d.join(super::super::store::FILE)).unwrap();
        let t = super::super::fold::Thresholds::default();
        let (home, work) = (d.join("home"), d.join("work"));
        for p in [&home, &work] {
            std::fs::create_dir_all(p).unwrap();
        }
        let claude = |profile: &str, id: &str, logs: &std::path::Path| Observation {
            provider: "claude".into(),
            account_id: id.into(),
            profile: profile.into(),
            tab: if profile.is_empty() { "claude".into() } else { format!("claude:{profile}") },
            local_logs: vec![logs.to_string_lossy().into()],
            ..Observation::default()
        };
        // Through the bracket, `home` is someone else and `work` is X.
        h.record(&[claude("", "someone-else", &home), claude("work", "x", &work)], &t, T - 5000.0).unwrap();
        // Long after it, `home` signs in as X.
        let switched = T + 3.0 * LONG;
        let now_obs = vec![claude("", "x", &home), claude("work", "x", &work)];
        h.record(&now_obs, &t, switched).unwrap();
        std::fs::write(
            home.join("s.jsonl"),
            [claude_line("theirs-1", T + 60.0), claude_line("theirs-2", T + 120.0), claude_line("mine-later", switched + 600.0)]
                .join("\n"),
        )
        .unwrap();
        std::fs::write(work.join("s.jsonl"), claude_line("mine", T + 90.0)).unwrap();
        let now = opscope_core::now();
        let mut r = Reader::default();
        // X's bracket from before the switch: `work`'s turn only.
        assert_eq!(
            tick_until_read(&mut r, &h, &now_obs, now),
            Local::Found(Activity { agent: Agent::ClaudeCode, first: T + 90.0, last: T + 90.0, turns: 1 })
        );
        // A bracket the switch fell inside cannot say whose `home`'s turns
        // were: nothing.
        let across = (switched - 600.0, switched - 600.0 + LONG);
        assert_eq!(tick_until_read_of(&mut r, &h, &now_obs, now, across), Local::Nothing);
        // After it, `home`'s turns are X's.
        let after = (switched + 60.0, switched + 60.0 + LONG);
        assert_eq!(
            tick_until_read_of(&mut r, &h, &now_obs, now, after),
            Local::Found(Activity { agent: Agent::ClaudeCode, first: switched + 600.0, last: switched + 600.0, turns: 1 })
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_config_dir_with_no_recorded_sign_in_gives_no_older_bracket_its_turns() {
        // Review on OPS-309: a file from before sign-ins were dated knows a
        // dir is signed in as X, not since when, and the fallback took the
        // first time X was seen at all - which another dir could have made
        // while this one was Y's - and gave Y's turns to X's bracket.
        let d = dir("reader-undated");
        let mut h = History::open(&d.join(super::super::store::FILE)).unwrap();
        let t = super::super::fold::Thresholds::default();
        let (home, work) = (d.join("home"), d.join("work"));
        for p in [&home, &work] {
            std::fs::create_dir_all(p).unwrap();
        }
        let claude = |profile: &str, logs: &std::path::Path| Observation {
            provider: "claude".into(),
            account_id: "x".into(),
            profile: profile.into(),
            tab: if profile.is_empty() { "claude".into() } else { format!("claude:{profile}") },
            local_logs: vec![logs.to_string_lossy().into()],
            ..Observation::default()
        };
        // `work` has been X's since long before the bracket. `home` is X's
        // now, with no record of when it became so.
        let obs = vec![claude("", &home), claude("work", &work)];
        h.record(&obs, &t, T - 5000.0).unwrap();
        h.conn.execute("DELETE FROM meta WHERE k = 'account_since:claude:default'", []).unwrap();
        std::fs::write(home.join("s.jsonl"), [claude_line("ys-1", T + 60.0), claude_line("ys-2", T + 120.0)].join("\n")).unwrap();
        std::fs::write(work.join("s.jsonl"), claude_line("mine", T + 90.0)).unwrap();
        let x = h.account_key_of(&obs[0]).unwrap();
        assert_eq!(h.signed_in_since(&obs[0], &x), None);
        assert!(h.signed_in_since(&obs[1], &x).is_some());
        let now = opscope_core::now();
        let mut r = Reader::default();
        assert_eq!(
            tick_until_read(&mut r, &h, &obs, now),
            Local::Found(Activity { agent: Agent::ClaudeCode, first: T + 90.0, last: T + 90.0, turns: 1 })
        );
        // The next pass dates it from then: older brackets still leave it out.
        let next = T + 3.0 * LONG;
        h.record(&obs, &t, next).unwrap();
        assert_eq!(h.signed_in_since(&obs[0], &x), Some(next));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_read_bracket_outlives_a_pass_that_misses_one_of_its_logs() {
        // Review on OPS-309: the cache was keyed on every observation's
        // log directories, so one pass missing a directory dropped every
        // bracket read with it and forced the reads again.
        let (d, h) = store("reader-grace");
        let logs = d.join("projects");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("s.jsonl"), format!("{}\n", claude_line("a", T + 60.0))).unwrap();
        let mut o = Observation {
            provider: "claude".into(),
            tab: "claude".into(),
            local_logs: vec![logs.to_string_lossy().into()],
            ..Observation::default()
        };
        let now = opscope_core::now();
        let mut r = Reader::default();
        let want = Local::Found(Activity { agent: Agent::ClaudeCode, first: T + 60.0, last: T + 60.0, turns: 1 });
        assert_eq!(tick_until_read(&mut r, &h, std::slice::from_ref(&o), now), want);
        // A pass without the directory: the closed bracket keeps its answer.
        o.local_logs.clear();
        let mut c = one_bracket_chart();
        r.tick(&h, &mut c, std::slice::from_ref(&o), now + 60.0);
        assert_eq!(c["claude"].pools[0].brackets[0].local, want);
        assert!(r.job.is_none(), "the bracket was read again");
        // Unused for a while: still kept. Unused past the grace: let go.
        r.tick(&h, &mut HashMap::new(), &[], now + 60.0 + EVICT_AFTER - 1.0);
        assert_eq!(r.cache.len(), 1);
        r.tick(&h, &mut HashMap::new(), &[], now + 61.0 + EVICT_AFTER);
        assert!(r.cache.is_empty());
        // A directory the read did not cover is read, never left out.
        o.local_logs = vec![logs.to_string_lossy().into()];
        assert_eq!(tick_until_read(&mut r, &h, std::slice::from_ref(&o), now), want);
        let more = d.join("more");
        std::fs::create_dir_all(&more).unwrap();
        std::fs::write(more.join("s.jsonl"), format!("{}\n", claude_line("b", T + 90.0))).unwrap();
        o.local_logs.push(more.to_string_lossy().into());
        let both = Local::Found(Activity { agent: Agent::ClaudeCode, first: T + 60.0, last: T + 90.0, turns: 2 });
        assert_eq!(tick_until_read(&mut r, &h, std::slice::from_ref(&o), now), both);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_reader_fills_a_tabs_bracket_off_the_pass_and_keeps_it() {
        use super::super::chart::{Body, PoolHistory};
        let (d, h) = store("reader");
        let logs = d.join("projects");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("s.jsonl"), format!("{}\n{}\n", claude_line("a", T + 60.0), claude_line("b", T + 120.0))).unwrap();
        let obs = vec![Observation {
            provider: "claude".into(),
            tab: "claude".into(),
            local_logs: vec![logs.to_string_lossy().into()],
            ..Observation::default()
        }];
        let charts = || {
            let pool = PoolHistory {
                label: "extra".into(),
                role: super::super::fold::Role::Overage,
                unit: "money".into(),
                currency: None,
                exponent: None,
                total: 1,
                since: None,
                body: Body::Overage { bars: Vec::new(), roll_day: None },
                brackets: vec![Bracket { at_lo: T, at_hi: T + LONG, amount: 2.0, local: Local::Unread }],
            };
            HashMap::from([("claude".to_string(), TabHistory { pools: vec![pool], expired: Vec::new() })])
        };
        let mut r = Reader::default();
        let mut c = charts();
        r.tick(&h, &mut c, &obs, opscope_core::now());
        // The first pass starts the read and does not wait for it.
        assert_eq!(c["claude"].pools[0].brackets[0].local, Local::Unread);
        let want = Local::Found(Activity { agent: Agent::ClaudeCode, first: T + 60.0, last: T + 120.0, turns: 2 });
        let mut got = Local::Unread;
        for _ in 0..500 {
            let mut c = charts();
            r.tick(&h, &mut c, &obs, opscope_core::now());
            got = c["claude"].pools[0].brackets[0].local.clone();
            if got != Local::Unread {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(got, want);
        // Cached: the log going away does not change a closed bracket.
        let _ = std::fs::remove_dir_all(&logs);
        let mut c = charts();
        r.tick(&h, &mut c, &obs, opscope_core::now());
        assert_eq!(c["claude"].pools[0].brackets[0].local, want);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_large_rollout_is_bound_to_the_account_its_latest_header_before_the_bracket_names() {
        // Review on OPS-309: a rollout over WHOLE_BELOW is searched to the
        // bracket, so a second `session_meta` naming another account,
        // written before the point the search lands on, was never read and
        // the turn after it was counted for the first account.
        let d = dir("codex-large");
        let filler = |body: &mut String| {
            let pad = "x".repeat(2000);
            let mut at = T - 40.0 * 86_400.0;
            while (body.len() as u64) < WHOLE_BELOW + (4 << 20) {
                body.push_str(&serde_json::json!({ "timestamp": iso(at), "type": "response_item", "pad": pad }).to_string());
                body.push('\n');
                at += 10.0;
            }
            assert!(at < T);
        };
        let write = |name: &str, first: &str, second: &str| {
            let mut body = format!("{}\n{}\n", codex_meta(first), codex_meta(second));
            filler(&mut body);
            body.push_str(&codex_turn(T + 60.0, 10.0));
            body.push('\n');
            std::fs::write(d.join(name), &body).unwrap();
        };
        let dirs = vec![d.to_string_lossy().to_string()];
        write("a.jsonl", "me", "someone-else");
        let key = Key::new(Agent::Codex, "k".into(), "me".into(), T, T + 4000.0);
        assert_eq!(read_bracket(&key, &dirs, opscope_core::now(), &mut JOB_BYTES.clone()), Local::Nothing);
        // The other way round, the turn is this account's.
        write("a.jsonl", "someone-else", "me");
        assert_eq!(
            read_bracket(&key, &dirs, opscope_core::now(), &mut JOB_BYTES.clone()),
            Local::Found(Activity { agent: Agent::Codex, first: T + 60.0, last: T + 60.0, turns: 1 })
        );
        // A budget too small for the rollout leaves it unread, and a job
        // whose whole budget that is says nothing.
        assert_eq!(read_bracket(&key, &dirs, opscope_core::now(), &mut (1 << 20)), Local::Unread);
        assert_eq!(read_job(vec![(key.clone(), dirs.clone())], opscope_core::now(), 1 << 20)[0].1, Local::Nothing);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `v` as one line exactly `len` bytes long with its newline.
    fn padded(mut v: serde_json::Value, len: usize) -> String {
        v["pad"] = serde_json::Value::String(String::new());
        let bare = v.to_string().len();
        v["pad"] = serde_json::Value::String("x".repeat(len - 1 - bare));
        let line = format!("{v}\n");
        assert_eq!(line.len(), len);
        line
    }

    #[test]
    fn a_session_header_at_the_point_the_search_lands_on_is_read() {
        // Review on OPS-309: the search can land on the first byte of a
        // `session_meta`. The header scan stopped short of it and the
        // bracket read skipped it as the line it landed inside, so the
        // turns under it went to the account of the header before.
        const LINE: usize = 4096;
        let d = dir("codex-aligned");
        let line_at = |i: usize| T - 50_000.0 + 10.0 * i as f64;
        let header = |who: &str, at: f64| {
            padded(serde_json::json!({ "timestamp": iso(at), "type": "session_meta", "payload": { "creator_account_id": who } }), LINE)
        };
        let turn = |at: f64, total: f64| {
            padded(
                serde_json::json!({
                    "timestamp": iso(at),
                    "type": "event_msg",
                    "payload": { "type": "token_count", "info": { "total_token_usage": { "total_tokens": total } } },
                }),
                LINE,
            )
        };
        // 4096 lines of 4096 bytes: every point the search halves to is
        // the first byte of a line.
        let mut lines: Vec<String> = vec![header("someone-else", line_at(0))];
        for i in 1..4094 {
            lines.push(padded(serde_json::json!({ "timestamp": iso(line_at(i)), "type": "response_item" }), LINE));
        }
        lines.push(turn(T + 60.0, 10.0));
        lines.push(turn(T + 120.0, 20.0));
        let path = d.join("a.jsonl");
        std::fs::write(&path, lines.concat()).unwrap();
        let size = (LINE * lines.len()) as u64;
        let landed = seek_before(&std::fs::File::open(&path).unwrap(), size, T - SLACK, &mut 0);
        assert!(landed > 0 && landed % LINE as u64 == 0, "landed at {landed}");
        let i = landed as usize / LINE;
        assert!(i < 4094);
        lines[i] = header("me", line_at(i));
        std::fs::write(&path, lines.concat()).unwrap();
        assert_eq!(seek_before(&std::fs::File::open(&path).unwrap(), size, T - SLACK, &mut 0), landed);
        let dirs = vec![d.to_string_lossy().to_string()];
        let key = Key::new(Agent::Codex, "k".into(), "me".into(), T, T + 4000.0);
        assert_eq!(
            read_bracket(&key, &dirs, opscope_core::now(), &mut JOB_BYTES.clone()),
            Local::Found(Activity { agent: Agent::Codex, first: T + 60.0, last: T + 120.0, turns: 2 })
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_repeated_total_is_not_a_turn_however_far_above_the_bracket_its_first_was() {
        // Review on OPS-309: a rollout searched to the bracket began its
        // count with no total, so a first event in the bracket repeating
        // the last one above it was counted as a response.
        let d = dir("codex-repeat");
        let mut body = format!("{}\n{}\n", codex_meta("me"), codex_turn(T - 30.0 * 86_400.0, 50.0));
        let pad = "x".repeat(2000);
        let mut at = T - 29.0 * 86_400.0;
        while (body.len() as u64) < WHOLE_BELOW + (4 << 20) {
            body.push_str(&serde_json::json!({ "timestamp": iso(at), "type": "response_item", "pad": pad }).to_string());
            body.push('\n');
            at += 10.0;
        }
        assert!(at < T);
        body.push_str(&format!("{}\n", codex_turn(T + 60.0, 50.0)));
        std::fs::write(d.join("a.jsonl"), &body).unwrap();
        let dirs = vec![d.to_string_lossy().to_string()];
        let key = Key::new(Agent::Codex, "k".into(), "me".into(), T, T + 4000.0);
        assert_eq!(read_bracket(&key, &dirs, opscope_core::now(), &mut JOB_BYTES.clone()), Local::Nothing);
        // A total that moved is still a turn.
        body.push_str(&format!("{}\n", codex_turn(T + 120.0, 70.0)));
        std::fs::write(d.join("a.jsonl"), &body).unwrap();
        assert_eq!(
            read_bracket(&key, &dirs, opscope_core::now(), &mut JOB_BYTES.clone()),
            Local::Found(Activity { agent: Agent::Codex, first: T + 120.0, last: T + 120.0, turns: 1 })
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_log_that_fails_part_way_is_not_read_as_ending_there() {
        // Review on OPS-309: a read error was taken for the end of the
        // log, so the turns before it were counted as the whole bracket.
        // A directory opens and then fails to read, as a bad file would.
        let d = dir("failing");
        let key = Key::new(Agent::ClaudeCode, "k".into(), String::new(), T, T + 3600.0);
        let mut out = HashMap::new();
        let got = file_turns(&d.to_string_lossy(), &key, &mut JOB_BYTES.clone(), &mut out);
        assert_eq!(got, Err(Cut::Failed));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_line_longer_than_the_budget_left_is_not_read_whole() {
        // Review on OPS-309: the budget was charged after a line was read,
        // so one long line ran the read past it by its whole length.
        let line = format!("{}\n", "x".repeat(1 << 20));
        let mut r = std::io::BufReader::new(line.as_bytes());
        let mut buf = Vec::new();
        let (over, took) = read_capped(&mut r, &mut buf, MAX_LINE, 1000).unwrap().unwrap();
        assert!(over);
        assert_eq!(took, 1000);
        let mut r = std::io::BufReader::new(line.as_bytes());
        assert_eq!(read_capped(&mut r, &mut buf, MAX_LINE, u64::MAX).unwrap(), Some((false, line.len() as u64)));
    }

    #[test]
    fn a_bracket_the_budget_ran_out_on_after_another_is_asked_for_again() {
        // Review on OPS-309: a bracket a job ran out of budget on was
        // cached as having no turns, and while a tab kept asking it was
        // never read again, though a job of its own would have covered it.
        let (d, h) = store("reader-deferred");
        let logs = d.join("projects");
        std::fs::create_dir_all(&logs).unwrap();
        let body = format!("{}\n{}\n", claude_line("a", T + 60.0), claude_line("b", T + 2.0 * LONG + 60.0));
        std::fs::write(logs.join("s.jsonl"), &body).unwrap();
        let dirs = vec![logs.to_string_lossy().to_string()];
        let first = Key::new(Agent::ClaudeCode, "k".into(), String::new(), T, T + LONG);
        let second = Key::new(Agent::ClaudeCode, "k".into(), String::new(), T + 2.0 * LONG, T + 3.0 * LONG);
        // Enough for one read of the log, not two.
        let got = read_job(
            vec![(first.clone(), dirs.clone()), (second.clone(), dirs.clone())],
            opscope_core::now(),
            body.len() as u64 + 10,
        );
        assert_eq!(got[0].1, Local::Found(Activity { agent: Agent::ClaudeCode, first: T + 60.0, last: T + 60.0, turns: 1 }));
        assert_eq!(got[1].1, Local::Unread);
        // What the pass does with it: keeps the answer, asks again for the rest.
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(got)).unwrap();
        let mut r = Reader { job: Some(rx), ..Reader::default() };
        r.tick(&h, &mut HashMap::new(), &[], opscope_core::now());
        assert!(r.cache.contains_key(&first));
        assert!(!r.cache.contains_key(&second), "a deferred bracket was kept as an answer");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_large_log_is_searched_to_the_bracket_and_read_no_further() {
        let d = dir("large");
        let pad = "x".repeat(2000);
        let mut body = String::new();
        let mut at = T - 40.0 * 86_400.0;
        // Well past WHOLE_BELOW of lines before the bracket.
        while (body.len() as u64) < WHOLE_BELOW + (4 << 20) {
            body.push_str(&serde_json::json!({ "type": "user", "timestamp": iso(at), "pad": pad }).to_string());
            body.push('\n');
            at += 10.0;
        }
        assert!(at < T);
        for i in 0..5 {
            body.push_str(&claude_line(&format!("in{i}"), T + 100.0 * i as f64));
            body.push('\n');
        }
        body.push_str(&claude_line("after", T + 90_000.0));
        body.push('\n');
        std::fs::write(d.join("s.jsonl"), &body).unwrap();
        let dirs: Vec<String> = vec![d.to_string_lossy().into()];
        let key = Key::new(Agent::ClaudeCode, "k".into(), String::new(), T, T + 3600.0);
        let mut budget = JOB_BYTES;
        let got = read_bracket(&key, &dirs, opscope_core::now(), &mut budget);
        assert_eq!(got, Local::Found(Activity { agent: Agent::ClaudeCode, first: T, last: T + 400.0, turns: 5 }));
        let read = JOB_BYTES - budget;
        assert!(read < (1 << 20), "read {read} bytes of {}", body.len());
        // A budget too small for the bracket says nothing rather than a short count.
        let mut tiny = 10_000;
        assert_eq!(read_bracket(&key, &dirs, opscope_core::now(), &mut tiny), Local::Unread);
        // ...and a job whose whole budget it is says nothing, for good.
        assert_eq!(read_job(vec![(key.clone(), dirs.clone())], opscope_core::now(), 10_000)[0].1, Local::Nothing);
        let _ = std::fs::remove_dir_all(&d);
    }
}
