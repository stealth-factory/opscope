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

//! Claude Code: its stats cache, its transcripts, and what is left of the
//! account's limits.
//!
//! The money comes from the transcripts rather than the cache. The cache
//! has one total per model per day, and input, output and the two cache
//! durations differ in price by up to fifty times, so a total cannot be
//! costed.

use std::collections::HashMap;

use chrono::{Datelike, Duration as Days, Local, NaiveDate, TimeZone};
use opscope_core as tc;

use crate::shared::*;
use crate::*;

/// Newest transcripts to sample for a rate.
const RATE_FILES: usize = 3;
/// Seconds; below this the timestamps are not a turn.
const MIN_GAP: f64 = 1.0;

/// What Claude Code has recorded, plus what is left of the limits.
#[derive(Clone, Default)]
pub struct Data {
    ok: bool,
    why: String,
    stats: serde_json::Value,
    /// The live or cached rate-limit reading, which is account-wide rather
    /// than about this machine.
    quota: Option<serde_json::Value>,
    quota_live: bool,
    quota_at: f64,
    /// Why the last attempt did not answer, when it did not. Empty while it
    /// is answering.
    quota_why: String,
    quota_plan: String,
    profile: Option<serde_json::Value>,
    /// Output tokens per second, sorted, and how many transcripts it came
    /// from.
    rates: Vec<f64>,
    sampled: usize,
    /// day -> model -> tokens by priced kind.
    daily: HashMap<String, HashMap<String, Tokens>>,
}

/// The OAuth token Claude Code already holds.
///
/// It goes only to Anthropic, is never printed, and an expired one is not
/// used at all: the refresh token sits beside it, but spending it would
/// race Claude Code's own credential handling for a number that has a local
/// cache anyway.
pub fn claude_token() -> Option<(String, String)> {
    let creds = read_json(&under_home(".claude/.credentials.json"))?;
    let o = &creds["claudeAiOauth"];
    let tok = text(o, "accessToken");
    if tok.is_empty() || num(o, "expiresAt") / 1000.0 <= now() {
        return None;
    }
    Some((tok, text(o, "subscriptionType")))
}

pub fn claude_get(url: &str, tok: &str) -> Option<serde_json::Value> {
    claude_try(url, tok).ok()
}

/// The same request, keeping why it failed.
///
/// `.ok()?` threw that away, so the tab could say a reading was old and
/// never why. The server had been answering "too many requests" for an hour
/// while the screen said "cached" and a credential with three hours left on
/// it got the blame.
fn claude_try(url: &str, tok: &str) -> Result<serde_json::Value, String> {
    let body = tc::get(
        url,
        &[
            ("Authorization", &format!("Bearer {}", tok)),
            ("User-Agent", "opscope"),
        ],
        20,
    )
    .map_err(|said| refusal(&said))?;
    serde_json::from_str(&body).map_err(|e| format!("unreadable answer: {}", e))
}

/// What Claude Code last fetched, for when the live call cannot run.
///
/// It is a cache with a timestamp, so it is shown with its age - and a
/// window whose reset has already gone by is said to have passed rather
/// than counted down to, because a stale five-hour window describes a
/// period that has ended.
/// How long Claude Code trusts its own cache, taken from its own reader:
/// `let r = Date.now() - n.data.fetchedAtMs; if (r < 0 || r > zNo) return null`
/// with `zNo = 3600000`. Identical in 2.1.233 and 2.1.243, so it is the
/// contract rather than a detail of one build.
const CLAUDE_CACHE_TTL: f64 = 3600.0;

/// How often this endpoint is actually asked, where the other agents use
/// the shared two minutes.
///
/// api.anthropic.com/api/oauth/usage rate-limits this token, and the limit
/// is shared with everything else holding it: every running copy of this
/// widget, and Claude Code itself. Three copies on the two-minute hold came
/// to a call every forty seconds between them and the endpoint answered 429
/// - consistently with three running, intermittently with two, and 200 with
/// none, which is as clear as that gets.
///
/// Five minutes because that is the cadence Claude Code chose for the same
/// data: its own refresh is throttled to BNo = 300000. There is no argument
/// for asking more often than the client that owns the number.
const CLAUDE_LIVE_TTL: f64 = 300.0;

/// How old a reading may be and still be shown as current.
///
/// Half an hour. A reading is already up to five minutes behind the server
/// in normal running - a live one is held for CLAUDE_LIVE_TTL - and a quota
/// window that turns over weekly does not move enough in half an hour to
/// mislead anyone. It is also where the backoff stops doubling, so the
/// screen starts saying "old" at the same moment the widget has given up
/// trying quickly.
///
/// Marking by where a reading came from rather than by how old it is put a
/// star on a figure thirty-five seconds old while a live one four minutes
/// old carried none - the fresher of the two flagged as the doubtful one.
/// The mark is worth having when it means "older than this widget's own
/// cycle"; it is noise when it means "another process fetched it".
const CLAUDE_FRESH_FOR: f64 = 1800.0;

/// True when the reading is old enough to be worth flagging, whatever its
/// source. `quota_at` is when it was taken, not when it was read.
fn reading_is_old(taken_at: f64) -> bool {
    now() - taken_at > CLAUDE_FRESH_FOR
}

/// Where our own last good reading lives.
///
/// Claude Code keeps one in ~/.claude.json and expires it after an hour.
/// This widget was reading that file with no age check at all, and on this
/// machine it had been showing a reading from nine days after Claude Code
/// stopped refreshing it - a percentage its own author discards, presented
/// as today's.
///
/// So we keep our own, written whenever the live call answers. On a machine
/// where Claude is in use that is most refreshes, which makes it minutes old
/// rather than days, and it does not depend on another program's cache still
/// being maintained. CodexBar does the same and for the same reason: its
/// Claude sources are the API and the CLI, never that file, with its own
/// snapshot shown by capture age when they all fail.
fn snapshot_path() -> String {
    let base = std::env::var("XDG_STATE_HOME").unwrap_or_else(|_| {
        format!("{}/.local/state", std::env::var("HOME").unwrap_or_default())
    });
    format!("{}/opscope/claude-usage.json", base)
}

/// The account the reading belongs to, so switching accounts does not show
/// the old one's figures. Claude Code guards its cache the same way; we
/// borrow its marker because the usage response carries no account of its
/// own. Absent on a machine whose Claude Code has never written the key, and
/// then the guard is simply not applied - a missing marker is not a mismatch.
fn account_marker() -> Option<String> {
    let config = read_json(&under_home(".claude.json"))?;
    let uuid = text(&config["cachedUsageUtilization"], "accountUuid");
    (!uuid.is_empty()).then_some(uuid)
}

fn save_snapshot(utilization: &serde_json::Value) {
    save_snapshot_at(&snapshot_path(), utilization)
}

/// The write, against a named path so it can be tested without reaching for
/// the real one or rewriting the environment out from under other tests.
fn save_snapshot_at(path: &str, utilization: &serde_json::Value) {
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut body = serde_json::json!({
        "fetchedAtMs": now() * 1000.0,
        "utilization": utilization,
    });
    if let Some(uuid) = account_marker() {
        body["accountUuid"] = serde_json::json!(uuid);
    }
    let _ = std::fs::write(path, body.to_string());
}

fn read_snapshot_at(path: &str) -> Option<(serde_json::Value, f64)> {
    let saved = read_json(path)?;
    let u = saved["utilization"].clone();
    if u.is_null() {
        return None;
    }
    let held = text(&saved, "accountUuid");
    if let (false, Some(now_uuid)) = (held.is_empty(), account_marker()) {
        if held != now_uuid {
            return None;
        }
    }
    Some((u, num(&saved, "fetchedAtMs") / 1000.0))
}

fn claude_code_cache_at(path: &str) -> Option<(serde_json::Value, f64)> {
    let config = read_json(path)?;
    let c = &config["cachedUsageUtilization"];
    let u = c["utilization"].clone();
    if u.is_null() {
        return None;
    }
    let at = num(c, "fetchedAtMs") / 1000.0;
    let age = now() - at;
    if !(0.0..=CLAUDE_CACHE_TTL).contains(&age) {
        return None;
    }
    Some((u, at))
}

/// The best reading we have that is not live: ours if we have one, Claude
/// Code's while it is still within the hour it trusts it for, whichever was
/// taken more recently.
pub fn claude_stale() -> Option<(serde_json::Value, f64)> {
    stale_from(&snapshot_path(), &under_home(".claude.json"))
}

/// The whole fallback against two named files, so a test can put a fossil
/// and a fresh reading on disk and check which one comes back.
fn stale_from(ours: &str, theirs: &str) -> Option<(serde_json::Value, f64)> {
    fresher(read_snapshot_at(ours), claude_code_cache_at(theirs))
}

/// Whichever reading was taken later, and neither being present is not an
/// error - it is a machine that has never had a live one. A tie goes to the
/// first, which is ours: the one whose provenance we know.
fn fresher<T>(ours: Option<(T, f64)>, theirs: Option<(T, f64)>) -> Option<(T, f64)> {
    match (ours, theirs) {
        (Some(a), Some(b)) => Some(if a.1 >= b.1 { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// Token counts from one transcript usage block, by priced kind.
///
/// A block's top-level numbers can all be zero while its `iterations` carry
/// the real figures, so the iterations win where they exist. Cache writes
/// are split by duration because they are priced differently, and the flat
/// cache_creation_input_tokens is only used when that split is absent.
pub fn usage_kinds(u: &serde_json::Value) -> Tokens {
    let mut out = empty_tokens();
    let empty = vec![u.clone()];
    let blocks: Vec<serde_json::Value> = match u["iterations"].as_array() {
        Some(list) if !list.is_empty() => list.clone(),
        _ => empty,
    };
    for x in &blocks {
        *out.get_mut("input").unwrap() += num(x, "input_tokens");
        *out.get_mut("output").unwrap() += num(x, "output_tokens");
        *out.get_mut("cache_read").unwrap() += num(x, "cache_read_input_tokens");
        let split = &x["cache_creation"];
        if split.is_object() {
            *out.get_mut("cache_write").unwrap() += num(split, "ephemeral_5m_input_tokens");
            *out.get_mut("cache_write_1h").unwrap() += num(split, "ephemeral_1h_input_tokens");
        } else {
            *out.get_mut("cache_write").unwrap() += num(x, "cache_creation_input_tokens");
        }
    }
    out
}

/// Per-record token counts from one transcript, keyed by record uuid.
///
/// Keyed rather than summed because the same message appears in more than
/// one file: resuming or forking a session replays its history into the new
/// transcript, and subagent turns are written twice over. Left raw that
/// inflated one model by 29% against Claude Code's own totals.
///
/// Cached on (mtime, size): a finished transcript never changes, so each is
/// parsed once.
pub fn scan_transcript(
    caches: &mut Caches,
    path: &str,
) -> HashMap<String, (String, String, Tokens)> {
    let Ok(meta) = std::fs::metadata(path) else {
        return HashMap::new();
    };
    use std::os::unix::fs::MetadataExt;
    let key = (meta.mtime() as u64, meta.size());
    if let Some((had, records)) = caches.transcripts.get(path) {
        if *had == key {
            return records.clone();
        }
    }
    let mut records = HashMap::new();
    let Ok(body) = std::fs::read_to_string(path) else {
        return records;
    };
    for line in body.lines() {
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(r) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let msg = &r["message"];
        let u = &msg["usage"];
        let (model, uid, stamp) = (text(msg, "model"), text(&r, "uuid"), text(&r, "timestamp"));
        if u.is_null() || model.is_empty() || uid.is_empty() || stamp.is_empty() {
            continue;
        }
        let Some(when) = iso_epoch(&stamp) else {
            continue;
        };
        let got = usage_kinds(u);
        if total_tokens(&got) <= 0.0 {
            continue;
        }
        let day = Local
            .timestamp_opt(when as i64, 0)
            .single()
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        records.insert(uid, (day, model, got));
    }
    caches
        .transcripts
        .insert(path.to_string(), (key, records.clone()));
    records
}

/// Every transcript's per-day, per-model tokens, de-duplicated.
///
/// stats-cache.json has dailyModelTokens, but only one total per model per
/// day - and input, output and the two cache kinds differ in price by up to
/// fifty times, so a total cannot be costed. The transcripts carry the
/// split, which is why the money comes from here and not from the cache.
pub fn claude_daily(caches: &mut Caches) -> HashMap<String, HashMap<String, Tokens>> {
    let mut files = Vec::new();
    // Recursive on purpose: subagent transcripts live a further two levels
    // down, and that is where most of the smaller models actually run.
    walk(&under_home(".claude/projects"), ".jsonl", &mut files);
    let mut seen: HashMap<String, (String, String, Tokens)> = HashMap::new();
    for path in &files {
        seen.extend(scan_transcript(caches, path));
    }
    let mut merged: HashMap<String, HashMap<String, Tokens>> = HashMap::new();
    for (day, model, tokens) in seen.into_values() {
        let bucket = merged
            .entry(day)
            .or_default()
            .entry(model)
            .or_insert_with(empty_tokens);
        for kind in RATE_KINDS {
            *bucket.get_mut(*kind).unwrap() += tokens.get(*kind).copied().unwrap_or(0.0);
        }
    }
    merged
}

/// Per-model token totals over the last N days (1 = today only).
pub fn window_models(
    daily: &HashMap<String, HashMap<String, Tokens>>,
    days: i64,
) -> Vec<(String, Tokens)> {
    let first = (Local::now().date_naive() - Days::days(days - 1))
        .format("%Y-%m-%d")
        .to_string();
    let mut out: HashMap<String, Tokens> = HashMap::new();
    for (day, models) in daily {
        if *day < first {
            continue;
        }
        for (model, tokens) in models {
            let bucket = out.entry(model.clone()).or_insert_with(empty_tokens);
            for kind in RATE_KINDS {
                *bucket.get_mut(*kind).unwrap() += tokens.get(*kind).copied().unwrap_or(0.0);
            }
        }
    }
    let mut list: Vec<(String, Tokens)> = out.into_iter().collect();
    list.sort_by(|a, b| a.0.cmp(&b.0));
    list
}

/// Output tokens per second, from the newest transcripts.
///
/// A turn is a `user` record followed by an `assistant` one, and the rate is
/// that assistant's output tokens over the gap between them. Measuring from
/// any previous record instead inflates it wildly - two assistant records
/// can be milliseconds apart while the second reports a whole turn's output.
///
/// The median is what gets shown: it barely moves whichever way the outliers
/// are trimmed, which is the reason to trust it, while the maximum moves by
/// a factor of twenty on the same data, which is the reason not to show one.
pub fn claude_rates() -> (Vec<f64>, usize) {
    let mut files = Vec::new();
    walk(&under_home(".claude/projects"), ".jsonl", &mut files);
    let mut with_time: Vec<(u64, String)> = files
        .into_iter()
        .filter_map(|path| {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::metadata(&path).ok()?;
            Some((meta.mtime() as u64, path))
        })
        .collect();
    with_time.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out: Vec<f64> = Vec::new();
    let mut sampled = 0usize;
    for (_, path) in with_time.iter().take(RATE_FILES) {
        sampled += 1;
        let (mut prev, mut prev_type): (Option<f64>, String) = (None, String::new());
        for line in tail_lines(path, 4 * 1024 * 1024) {
            if !line.contains("\"timestamp\"") {
                continue;
            }
            let Ok(d) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let (stamp, typ) = (text(&d, "timestamp"), text(&d, "type"));
            let at = iso_epoch(&stamp);
            if typ == "assistant"
                && at.is_some()
                && prev.is_some()
                && prev_type == "user"
                && !d["isAbortedMidStream"].as_bool().unwrap_or(false)
            {
                let tok = num(&d["message"]["usage"], "output_tokens");
                if tok > 0.0 {
                    let gap = at.unwrap() - prev.unwrap();
                    if (MIN_GAP..300.0).contains(&gap) {
                        out.push(tok / gap);
                    }
                }
            }
            if let Some(at) = at {
                prev = Some(at);
                prev_type = typ;
            }
        }
    }
    out.sort_by(f64::total_cmp);
    (out, sampled)
}

pub fn read(caches: &mut Caches, _cfg: &Config) -> Data {
    let mut claude = Data::default();
    // The reason rides along in the cached value, so it is held and shown
    // for as long as the failure it describes rather than only on the frame
    // the request happened to be made.
    let live = cached(caches, "claude", CLAUDE_LIVE_TTL, || {
        let Some((tok, plan)) = claude_token() else {
            return Some(serde_json::json!({ "why": "no token - Claude Code has not signed in here" }));
        };
        match claude_try("https://api.anthropic.com/api/oauth/usage", &tok) {
            Ok(u) => Some(serde_json::json!({ "u": u, "at": now(), "plan": plan })),
            Err(why) => Some(serde_json::json!({ "why": why })),
        }
    });
    // A cached entry carrying only a reason is a refusal, not a reading.
    let live = match live {
        Some(got) if !text(&got, "why").is_empty() => {
            claude.quota_why = text(&got, "why");
            None
        }
        other => other,
    };
    match live {
        Some(got) => {
            claude.quota = Some(got["u"].clone());
            claude.quota_live = true;
            claude.quota_at = num(&got, "at");
            claude.quota_plan = text(&got, "plan");
            // Kept for the next refresh that cannot reach the endpoint. The
            // reading is held for LIVE_TTL either way, so this writes about
            // once every two minutes rather than on every frame.
            save_snapshot(&got["u"]);
        }
        None => {
            if let Some((u, at)) = claude_stale() {
                claude.quota = Some(u);
                claude.quota_live = false;
                claude.quota_at = at;
            }
        }
    }
    claude.profile = cached(caches, "claude-plan", PLAN_TTL, || {
        let (tok, plan) = claude_token()?;
        let mut d = claude_get("https://api.anthropic.com/api/oauth/profile", &tok)?;
        d["_plan"] = serde_json::Value::String(plan);
        Some(d)
    });
    if claude.profile.is_none() {
        // The profile endpoint is richer, but the credentials file needs no
        // network and is always there, so the section degrades to two true
        // lines instead of vanishing.
        if let Some(creds) = read_json(&under_home(".claude/.credentials.json")) {
            let o = &creds["claudeAiOauth"];
            let plan = text(o, "subscriptionType");
            if !plan.is_empty() {
                claude.profile = Some(serde_json::json!({
                    "_plan": plan,
                    "_local": true,
                    "organization": { "rate_limit_tier": text(o, "rateLimitTier") },
                }));
            }
        }
    }
    match read_json(&under_home(".claude/stats-cache.json")) {
        Some(stats) => {
            claude.ok = true;
            claude.stats = stats;
            let (rates, sampled) = claude_rates();
            claude.rates = rates;
            claude.sampled = sampled;
            claude.daily = claude_daily(caches);
        }
        None => claude.why = "no stats cache".into(),
    }
    claude
}

/// Where a Claude limit belongs in the list, shortest leash first.
///
/// The server returns them in no order worth keeping. Read top to bottom
/// they should widen: the five-hour session is what stops you this
/// afternoon, the weekly total is what stops you this week, and a
/// model-scoped weekly limit stops only one model.
pub fn claude_lane_rank(limit: &serde_json::Value) -> usize {
    if text(limit, "kind") == "session" {
        return 0;
    }
    if text(&limit["scope"]["model"], "display_name").is_empty() {
        1
    } else {
        2
    }
}

/// The scope note, shortened before it can push a reset off the line.
///
/// "not this machine" is the point of the sentence, but it is also sixteen
/// characters, and seg() clips whatever runs past the pane. Losing the
/// clause leaves a shorter true line; losing the end of "resets in 15d"
/// leaves "resets in 1", which is a different and wrong number.
pub fn scope_phrase(w: usize, used: usize) -> &'static str {
    let full = " · account-wide, not this machine   ";
    if used + full.len() <= w - 1 {
        full
    } else {
        " · account-wide   "
    }
}

/// The windows Claude Code's own /usage shows.
///
/// Read from limits[], which is the server's own curated list: the rest of
/// the response carries a dozen null pools that /usage does not render
/// either. Each entry names itself, so a model-scoped weekly limit arrives
/// labelled without this having to know the name.
pub fn claude_quota(c: &Data, w: usize, p: &Palette) -> Vec<String> {
    let Some(u) = c.quota.as_ref() else {
        return Vec::new();
    };
    let mut lanes: Vec<&serde_json::Value> = u["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|l| !l["percent"].is_null())
        .collect();
    if lanes.is_empty() {
        return Vec::new();
    }
    lanes.sort_by_key(|l| claude_lane_rank(l));
    let src = if c.quota_live {
        "live".to_string()
    } else if c.quota_why.is_empty() {
        format!("cached {} ago", ago(c.quota_at))
    } else {
        // Why, not just how old. "cached 4m ago" tells a reader the number
        // is behind and leaves them to guess whether their login has
        // lapsed; naming the refusal is the difference between closing a
        // spare pane and going through a credential that was never wrong.
        format!("cached {} ago · {}", ago(c.quota_at), c.quota_why)
    };
    let hue = agent_hue("claude");
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── QUOTA ── ".into()),
            (
                if c.quota_live { p.ok.as_str() } else { p.warn.as_str() },
                src.clone(),
            ),
            (
                p.dim.as_str(),
                scope_phrase(w, 13 + src.len() + c.quota_plan.len()).to_string(),
            ),
            (p.dim.as_str(), c.quota_plan.clone()),
        ],
        w - 1,
    )];

    let label_of = |l: &serde_json::Value| -> String {
        let scope = text(&l["scope"]["model"], "display_name");
        let group = text(l, "group");
        let name = if !scope.is_empty() {
            scope
        } else if text(l, "kind") == "weekly_all" {
            "overall".to_string()
        } else if !group.is_empty() {
            group.clone()
        } else {
            match text(l, "kind") {
                s if s.is_empty() => "?".into(),
                s => s,
            }
        };
        let window = match group.as_str() {
            "session" => "5h",
            "weekly" => "7d",
            _ => "",
        };
        format!("{} {}", name, window).trim().to_string()
    };
    let texts: Vec<String> = lanes.iter().map(|l| label_of(l)).collect();
    let label_w = texts.iter().map(|t| t.chars().count()).max().unwrap_or(9).max(9);
    for (l, label) in lanes.iter().zip(&texts) {
        let pct = num(l, "percent");
        let used = (pct / 100.0).clamp(0.0, 1.0);
        let reset = iso_epoch(&text(l, "resets_at"));
        let when = match reset {
            None => String::new(),
            Some(ts) => {
                let left = ts - now();
                if left > 0.0 {
                    format!("resets in {}", left_span(left))
                } else if c.quota_live {
                    "resetting".into()
                } else {
                    "already reset".into()
                }
            }
        };
        let sev = text(l, "severity").to_lowercase();
        let window = CLAUDE_WINDOW_SECS
            .iter()
            .find(|(g, _)| *g == text(l, "group"))
            .map(|(_, s)| *s);
        let cushion = lead(pct, window, reset);
        let (pace_colour, pace_txt) = pace_cell(cushion, p);
        // is_active marks the limit currently doing the binding - the one
        // that will stop you first - so it is the one worth reading brightly.
        let mut line: Vec<(String, String)> = vec![(
            if l["is_active"].as_bool().unwrap_or(false) {
                p.txt.clone()
            } else {
                p.dim.clone()
            },
            format!(" {} ", tc::pad(label, label_w)),
        )];
        line.extend(paced_bar(
            used,
            elapsed_of(window, reset),
            w.saturating_sub(35 + label_w).max(8),
            hue,
            p,
        ));
        line.push((pct_colour(pct, hue, p), pct_text(pct)));
        line.push((pace_colour, pace_txt));
        line.push((
            if sev.is_empty() || sev == "normal" { p.dim.clone() } else { p.bad.clone() },
            if sev.is_empty() || sev == "normal" {
                format!("  {}", when)
            } else {
                format!("  {} · {}", sev, when)
            },
        ));
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&refs, w - 1));
    }
    rows.push(extra_row(&extra_state(u), w, p));
    rows.push(String::new());
    rows
}

/// What the account may spend past its plan, in the states Claude describes.
///
/// Extra usage is on-demand spend past the subscription: real money, billed,
/// and capped by a monthly limit the account sets on claude.com. That cap
/// can be raised, lowered or switched off mid-cycle, so every state has to
/// be drawable from whatever the next response says - and a shape this
/// parser has not seen must never be drawn as one of the others.
///
/// `cursor.rs` carries the same reading for the same money, and the wording
/// on screen is deliberately its wording: two panes side by side saying
/// `disabled` and `off` about the same state would be two vocabularies for
/// one fact.
#[derive(Debug, Clone, PartialEq)]
pub enum Extra {
    /// A stated ceiling. `used` may exceed `limit`: lowering the cap below
    /// what has already gone is something the account can do at any time,
    /// and the figures then have to stay the real ones.
    Fixed {
        used: f64,
        limit: f64,
        currency: String,
        /// The server's own `severity`, and its `spend_limit_reached`.
        /// Neither is worked out here: the limit rows directly above read
        /// the same `severity` field, and one pane holding two readings of
        /// one account's health is worse than either alone.
        severity: String,
        reached: bool,
    },
    /// Extra usage switched off. Anything spent before that is still
    /// billable and stays on screen.
    Off { used: f64, currency: String, why: String },
    /// No spend block, or a shape this parser has not seen. The keys that
    /// did arrive ride along, so an unmapped response can be read off the
    /// pane and mapped rather than guessed at.
    NotReported { keys: Vec<String> },
}

/// One of Claude's money amounts, which arrive in minor units.
///
/// 5000 with `exponent` 2 is fifty, and reading it as five thousand is a
/// hundredfold error in a figure about money.
fn minor(m: &serde_json::Value) -> Option<f64> {
    let amount = m.get("amount_minor")?.as_f64()?;
    Some(amount / 10f64.powf(m["exponent"].as_f64().unwrap_or(2.0)))
}

/// Which state the account's extra usage is in.
///
/// Only `Fixed` has been seen on a real account, at nothing spent. `Off`
/// rests on an assumption named in the test that covers it, and anything
/// matching neither is `NotReported` carrying its keys - an admitted gap is
/// worth more than a guessed state.
///
/// There is no uncapped state here on purpose. Cursor has one because
/// Cursor sends a block that says so; nothing in Claude's response has been
/// seen to mean "allowed, no ceiling", so a response shaped that way lands
/// in `NotReported` and can be mapped when one is actually seen.
pub fn extra_state(u: &serde_json::Value) -> Extra {
    let spend = &u["spend"];
    let x = &u["extra_usage"];
    let Some(obj) = spend.as_object().filter(|o| !o.is_empty()) else {
        return Extra::NotReported { keys: Vec::new() };
    };
    let currency = match text(&spend["limit"], "currency") {
        s if !s.is_empty() => s,
        _ => match text(&spend["used"], "currency") {
            s if !s.is_empty() => s,
            _ => text(x, "currency"),
        },
    };
    // `spend.used` is the money; `extra_usage.used_credits` is the same
    // figure stated a second time. The first is authoritative because it
    // carries its own currency and exponent, and a test pins the two in
    // agreement so a divergence surfaces rather than one being silently
    // preferred.
    let used = minor(&spend["used"]).or_else(|| x["used_credits"].as_f64());
    // Three separate fields can say it is off and any one of them saying so
    // is enough. They have not been seen disagreeing, and treating either
    // block as the only authority would draw a cap for an account that
    // cannot spend against it.
    let off = spend["enabled"].as_bool() == Some(false)
        || x["user_disabled"].as_bool() == Some(true)
        || x["is_enabled"].as_bool() == Some(false);
    if off {
        let why = match text(x, "disabled_reason") {
            s if !s.is_empty() => s,
            _ => text(spend, "disabled_reason"),
        };
        return Extra::Off { used: used.unwrap_or(0.0), currency, why };
    }
    if let Some(limit) = minor(&spend["limit"]).filter(|l| *l > 0.0) {
        return Extra::Fixed {
            // A block carrying a cap and no spend is a zero left out, not a
            // spend nobody knows - the server answered the call.
            used: used.unwrap_or(0.0),
            limit,
            currency,
            severity: text(spend, "severity"),
            reached: x["spend_limit_reached"].as_bool().unwrap_or(false),
        };
    }
    Extra::NotReported { keys: obj.keys().cloned().collect() }
}

/// The lane the summary draws for extra usage, where there is one.
///
/// Only a stated ceiling is something to be a percentage of, and the figure
/// is deliberately not clamped: a cap lowered below what has already gone is
/// a real number over 100, and the summary draws a full bar and the true
/// figure for it.
///
/// The percentage is worked out from the pair on the tab rather than taken
/// from `spend.percent`, which is a whole number - 0.40 of 50.00 is `1%`
/// there and 0.80% here, and a small real spend reading as `0%` is what an
/// empty section looks like. The server's figure is not discarded: a test
/// holds the two to the same value once rounded.
pub fn extra_lane(state: &Extra) -> Option<(f64, String)> {
    match state {
        Extra::Fixed { used, limit, currency, .. } if *limit > 0.0 => {
            Some((100.0 * used / limit, cap_tag_in(&cap_prefix(currency), *limit)))
        }
        _ => None,
    }
}

/// What goes before the digits on a lane label.
///
/// A `$` only where the server said dollars. Anything else keeps its code,
/// because a label reading `extra $50` on an account billed in AUD is a
/// claim about the money that nobody made.
fn cap_prefix(currency: &str) -> String {
    match currency {
        "" | "USD" => "$".to_string(),
        code => format!("{} ", code),
    }
}

/// The extra-usage line, in whichever state the account is in.
///
/// Money rather than a bar, the way Cursor's is. This is spend against a
/// denominator of its own - the account's monthly cap - and not one of the
/// percentages above it, so drawing it on that scale would invite reading it
/// as a fourth quota lane.
///
/// Drawn in every state, including the ones with nothing to report. The line
/// used to appear only for an account with extra usage enabled and a cap
/// present, which meant a switched-off cap and a response nobody could parse
/// both drew nothing at all - and nothing reads as "this account has no
/// extra usage" rather than "we could not tell". Each state reads
/// differently on purpose: `0.00 of 0.00` for an absent cap would be a cap
/// of nothing rather than no cap.
fn extra_row(state: &Extra, w: usize, p: &Palette) -> String {
    const LBL: &str = "  extra usage ";
    let room = w.saturating_sub(2);
    match state {
        Extra::Fixed { used, limit, currency, severity, reached } => {
            // The server's own judgement, not a threshold invented here.
            // `spend_limit_reached` wins over the percentage: a cap that has
            // been hit is hit whatever rounding says.
            let hot = *reached || !matches!(severity.as_str(), "" | "normal");
            let (used_s, limit_s) = (format!("{:.2}", used), format!("{:.2}", limit));
            let head = LBL.chars().count() + used_s.len() + 4 + limit_s.len() + 1 + currency.len();
            // What the line gives up first on a narrow pane, widest last
            // wanted. Both tails are arithmetic on the pair already on the
            // line, so dropping one loses no fact - and the overage is
            // written as an overage rather than a negative remainder,
            // because `-9.00 left` is arithmetic where a reader needs a
            // fact.
            let full = if used > limit {
                format!(" limit · {:.2} over", used - limit)
            } else {
                format!(" limit · {:.2} left", limit - used)
            };
            let tail = [full.as_str(), " limit", ""]
                .into_iter()
                .find(|t| head + t.chars().count() <= room)
                .unwrap_or("");
            tc::seg(
                &[
                    (p.dim.as_str(), LBL.to_string()),
                    (if hot { p.bad.as_str() } else { p.txt.as_str() }, used_s),
                    (p.dim.as_str(), " of ".into()),
                    (p.txt.as_str(), limit_s),
                    (p.dim.as_str(), format!(" {}", currency)),
                    (if hot { p.bad.as_str() } else { p.dim.as_str() }, tail.to_string()),
                ],
                w - 1,
            )
        }
        Extra::Off { used, currency, why } => {
            let spent = (*used > 0.0).then(|| format!("{:.2} {} · ", used, currency));
            let head = LBL.chars().count()
                + spent.as_ref().map(|s| s.chars().count()).unwrap_or(0)
                + "disabled".len();
            // The reason only where it fits whole. Half a sentence about
            // why money cannot be spent is worse than the bare fact.
            let reason = match why {
                s if s.is_empty() => String::new(),
                s if head + 3 + s.chars().count() <= room => format!(" · {}", s),
                _ => String::new(),
            };
            tc::seg(
                &[
                    (p.dim.as_str(), LBL.to_string()),
                    (p.txt.as_str(), spent.unwrap_or_default()),
                    (p.dim.as_str(), format!("disabled{}", reason)),
                ],
                w - 1,
            )
        }
        Extra::NotReported { keys } => {
            let joined = keys.join(", ");
            // The keys that did arrive, so the next shape can be mapped from
            // the pane rather than guessed at - but only whole, since half a
            // key list names fields that are not there.
            let tail = match joined {
                s if s.is_empty() => String::new(),
                s if LBL.chars().count() + "not reported".len() + 3 + s.chars().count() <= room => {
                    format!(" · {}", s)
                }
                _ => String::new(),
            };
            tc::seg(
                &[
                    (p.dim.as_str(), LBL.to_string()),
                    (p.warn.as_str(), "not reported".to_string()),
                    (p.dim.as_str(), tail),
                ],
                w - 1,
            )
        }
    }
}


/// How far behind today the stats cache's own reckoning is.
///
/// `room` is the columns actually left on the line, measured by the caller
/// rather than guessed from the pane width - the text before this varies,
/// so a width threshold clipped at some widths and not others.
pub fn stats_lag(stats: &serde_json::Value, room: i64) -> String {
    let last = text(stats, "lastComputedDate");
    let Ok(when) = NaiveDate::parse_from_str(&last, "%Y-%m-%d") else {
        return String::new();
    };
    let days = (Local::now().date_naive() - when).num_days();
    if days <= 0 {
        return String::new();
    }
    let month = MONTHS[when.month0() as usize];
    for candidate in [
        format!("   cache is {}d behind, to {} {}", days, month, when.day()),
        format!("   cache {}d behind", days),
        format!("  -{}d", days),
    ] {
        if candidate.len() as i64 <= room {
            return candidate;
        }
    }
    String::new()
}

pub fn claude_plan_rows(prof: &serde_json::Value, w: usize, p: &Palette) -> Vec<String> {
    let org = &prof["organization"];
    let mut pairs: Vec<(String, String)> = Vec::new();
    let created = text(org, "subscription_created_at");
    if let (Some(since), day) = (iso_epoch(&created), iso_day(&created)) {
        if !day.is_empty() {
            pairs.push(("member since".into(), format!("{} · {} ago", day, ago(since))));
        }
    }
    for (label, key) in [
        ("status", "subscription_status"),
        ("rate limit tier", "rate_limit_tier"),
    ] {
        let value = text(org, key);
        if !value.is_empty() {
            pairs.push((label.into(), value));
        }
    }
    let billing = text(org, "billing_type");
    if !billing.is_empty() {
        pairs.push(("billing".into(), billing.replace('_', " ")));
    }
    let headline = match text(prof, "_plan") {
        s if !s.is_empty() => s,
        _ => text(org, "organization_type"),
    };
    plan_rows(
        &headline,
        &pairs,
        w,
        if prof["_local"].as_bool().unwrap_or(false) {
            "from credentials"
        } else {
            ""
        },
        None,
        "",
        p,
    )
}

pub fn claude_metered(c: &Data, w: usize, cfg: &Config, p: &Palette) -> Vec<String> {
    metered_rows(
        &[
            ("today".to_string(), window_models(&c.daily, 1)),
            ("30 days".to_string(), window_models(&c.daily, 30)),
        ],
        w,
        "",
        "claude",
        "this machine",
        "Counted from transcripts, which are written where the agent ran. \
         Claude used on another machine, or on claude.ai, is not in here.",
        cfg,
        p,
    )
}

pub fn claude_tab(c: &Data, w: usize, p: &Palette) -> Vec<String> {
    let mut rows = claude_quota(c, w, p);
    if rows.is_empty() {
        // Two facts have to survive a quota block that drew nothing: why the
        // bars are missing, and the extra-usage line, which lives inside that
        // block and is the one figure on this tab that is real money. An
        // account answering with a spend cap and no limit percentages lost
        // both - the second to the early return, and the first to
        // `why_no_lane`, which counts the extra-usage lane as a bar and
        // rightly says nothing once there is one.
        let note = why_no_limits(c);
        if !note.is_empty() {
            rows.extend(no_local(&note, "", w, p));
            rows.push(String::new());
        }
        if let Some(u) = c.quota.as_ref() {
            if !matches!(extra_state(u), Extra::NotReported { .. }) {
                rows.push(extra_row(&extra_state(u), w, p));
                rows.push(String::new());
            }
        }
    }
    if !c.ok {
        rows.extend(no_local(
            &format!("No stats cache yet ({}).", c.why),
            run_hint("claude"),
            w,
            p,
        ));
        return rows;
    }
    let d = &c.stats;
    let mu = &d["modelUsage"];
    let sum_over = |key: &str| -> f64 {
        mu.as_object()
            .into_iter()
            .flatten()
            .map(|(_, v)| num(v, key))
            .sum()
    };
    let (in_tok, out_tok) = (sum_over("inputTokens"), sum_over("outputTokens"));
    let (cache_r, cache_w) = (
        sum_over("cacheReadInputTokens"),
        sum_over("cacheCreationInputTokens"),
    );

    // The calendar is computed first: its streaks and active-day counts
    // belong in the summary above it, not only beside the calendar.
    let mut totals: HashMap<NaiveDate, f64> = HashMap::new();
    for entry in d["dailyModelTokens"].as_array().into_iter().flatten() {
        let day = text(entry, "date");
        let Ok(at) = NaiveDate::parse_from_str(&day, "%Y-%m-%d") else {
            continue;
        };
        let total: f64 = entry["tokensByModel"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, v)| v.as_f64().unwrap_or(0.0))
            .sum();
        totals.insert(at, total);
    }
    let peak = totals.values().cloned().fold(0.0f64, f64::max);
    let cal = day_calendar(&totals, w, HEAT_STEPS, None, p);

    let fav = mu
        .as_object()
        .into_iter()
        .flatten()
        .max_by(|a, b| num(a.1, "outputTokens").total_cmp(&num(b.1, "outputTokens")))
        .map(|(k, _)| k.replace("claude-", ""))
        .unwrap_or_else(|| "—".into());
    let all_tokens = in_tok + out_tok + cache_r + cache_w;
    rows.push(tc::seg(
        &[
            (p.lbl.as_str(), " ── SUMMARY ── ".into()),
            (
                p.dim.as_str(),
                format!(
                    "all time · since {}",
                    text(d, "firstSessionDate").chars().take(10).collect::<String>()
                ),
            ),
        ],
        w - 1,
    ));
    let facts = cal.as_ref();
    let best_txt = facts
        .and_then(|c| c.best)
        .map(|b| format!("{} {}", MONTHS[b.month0() as usize], b.day()))
        .unwrap_or_else(|| "—".into());
    let pairs: Vec<(&str, String, &str, &str, String, &str)> = vec![
        (
            "Favorite model",
            fav,
            p.agent.as_str(),
            "Total tokens",
            big_num(all_tokens),
            p.agent.as_str(),
        ),
        (
            "Sessions",
            format!("{}", num(d, "totalSessions") as i64),
            p.txt.as_str(),
            "Longest session",
            span_ms(num(&d["longestSession"], "duration")),
            p.txt.as_str(),
        ),
        (
            "Active days",
            facts
                .map(|c| format!("{}/{}", c.active, c.span))
                .unwrap_or_else(|| "—".into()),
            p.txt.as_str(),
            "Longest streak",
            facts
                .map(|c| format!("{} days", c.longest))
                .unwrap_or_else(|| "—".into()),
            p.txt.as_str(),
        ),
        (
            "Most active day",
            best_txt,
            p.txt.as_str(),
            "Current streak",
            facts
                .map(|c| format!("{} days", c.current))
                .unwrap_or_else(|| "—".into()),
            if facts.is_some_and(|c| c.current > 0) { p.ok.as_str() } else { p.dim.as_str() },
        ),
    ];
    let lw = pairs
        .iter()
        .map(|(a, _, _, c, _, _)| a.len().max(c.len()))
        .max()
        .unwrap_or(10);
    let half = (w - 3) / 2;
    let vw = half.saturating_sub(lw + 2).max(6);
    for (a, b, bc, c, e, ec) in &pairs {
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!(" {} ", tc::pad(a, lw))),
                (bc, tc::pad(b, vw)),
                (p.dim.as_str(), format!(" {} ", tc::pad(c, lw))),
                (ec, tc::pad(e, vw)),
            ],
            w - 1,
        ));
    }
    rows.push(tc::seg(
        &[
            (p.dim.as_str(), "  Input ".into()),
            (p.txt.as_str(), big_num(in_tok)),
            (p.dim.as_str(), " · Output ".into()),
            (p.txt.as_str(), big_num(out_tok)),
            (p.dim.as_str(), " · Cache read ".into()),
            (p.txt.as_str(), big_num(cache_r)),
            (p.dim.as_str(), " · Cache written ".into()),
            (p.txt.as_str(), big_num(cache_w)),
        ],
        w - 1,
    ));

    // Which model did the work.
    rows.push(String::new());
    let mut ranked: Vec<(String, f64)> = mu
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), num(v, "outputTokens")))
        .filter(|(_, tok)| *tok > 0.0)
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    rows.push(tc::seg(
        &[
            (p.lbl.as_str(), " ── BY MODEL ── ".into()),
            (p.dim.as_str(), "output tokens".into()),
        ],
        w - 1,
    ));
    if let Some((_, top)) = ranked.first() {
        let top = top.max(1.0);
        for (name, tok) in ranked.iter().take(5) {
            let bar = tc::meter(tok / top, w.saturating_sub(34).max(6));
            let filled = bar.chars().filter(|c| *c == '█').count();
            rows.push(tc::seg(
                &[
                    (
                        p.txt.as_str(),
                        format!("  {}", tc::pad(&name.replace("claude-", ""), 20)),
                    ),
                    (p.agent.as_str(), format!("{:>7} ", big_num(*tok))),
                    (p.agent.as_str(), bar.chars().take(filled).collect::<String>()),
                    (p.grid.as_str(), bar.chars().skip(filled).collect::<String>()),
                ],
                w - 1,
            ));
        }
    }

    // Messages per day, straight from the file.
    let daily: Vec<&serde_json::Value> = d["dailyActivity"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    if !daily.is_empty() {
        rows.push(String::new());
        let counts: Vec<f64> = daily.iter().map(|x| num(x, "messageCount")).collect();
        let msg_peak = counts.iter().cloned().fold(0.0f64, f64::max).max(1.0);
        // Both charts on this tab come from stats-cache.json, which Claude
        // Code recomputes on its own schedule. Unlabelled, that gap reads as
        // idle days rather than as days the cache has not caught up with.
        let head = " ── MESSAGES / DAY ── ";
        let tail = format!("{}d · peak {}", daily.len(), msg_peak as i64);
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), head.into()),
                (p.dim.as_str(), tail.clone()),
                (
                    p.warn.as_str(),
                    stats_lag(d, w as i64 - 1 - head.len() as i64 - tail.len() as i64),
                ),
            ],
            w - 1,
        ));
        let avail = w.saturating_sub(3).max(10);
        let widths = tc::spread(counts.len(), avail);
        let mut cols: Vec<(f64, String)> = Vec::new();
        for (c, wide) in counts.iter().zip(&widths) {
            cols.extend(std::iter::repeat_n((*c, p.agent.clone()), *wide));
        }
        for line in tc::vbars(&cols, 3, 0.0) {
            let mut parts: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for (colour, ch) in &line {
                parts.push((colour.as_str(), ch.clone()));
            }
            rows.push(tc::seg(&parts, w - 1));
        }
        rows.push(tc::seg(
            &[(tc::RST, " ".into()), (p.grid.as_str(), "─".repeat(cols.len()))],
            w - 1,
        ));
        let left: String = text(daily[0], "date").chars().skip(5).collect();
        let right: String = text(daily[daily.len() - 1], "date").chars().skip(5).collect();
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!(" {}", left)),
                (
                    p.dim.as_str(),
                    " ".repeat(cols.len().saturating_sub(left.len() + right.len()).max(1)),
                ),
                (p.dim.as_str(), right),
            ],
            w - 1,
        ));
    }

    // How fast it generates.
    if !c.rates.is_empty() {
        let med = c.rates[c.rates.len() / 2];
        let p90 = c.rates[((c.rates.len() as f64 * 0.9) as usize).min(c.rates.len() - 1)];
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── OUTPUT RATE ── ".into()),
                (
                    p.dim.as_str(),
                    format!("{} turns across {} transcripts", c.rates.len(), c.sampled),
                ),
            ],
            w - 1,
        ));
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "  median ".into()),
                (p.agent.as_str(), format!("{:.0}", med)),
                (p.dim.as_str(), " tok/s   p90 ".into()),
                (p.txt.as_str(), format!("{:.0}", p90)),
                (p.dim.as_str(), "   request to response, tools included".into()),
            ],
            w - 1,
        ));
    }

    // Tokens per day, as a calendar.
    if let Some(cal) = cal {
        rows.push(String::new());
        let head = " ── TOKENS / DAY ── peak ";
        let tail = format!(
            "{} on {}",
            big_num(peak),
            cal.best
                .map(|b| format!("{} {}", MONTHS[b.month0() as usize], b.day()))
                .unwrap_or_else(|| "--".into())
        );
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── TOKENS / DAY ── ".into()),
                (p.dim.as_str(), "peak ".into()),
                (p.agent.as_str(), big_num(peak)),
                (
                    p.dim.as_str(),
                    format!(
                        " on {}",
                        cal.best
                            .map(|b| format!("{} {}", MONTHS[b.month0() as usize], b.day()))
                            .unwrap_or_else(|| "--".into())
                    ),
                ),
                (
                    p.warn.as_str(),
                    stats_lag(d, w as i64 - 1 - head.len() as i64 - tail.len() as i64),
                ),
            ],
            w - 1,
        ));
        for line in &cal.rows {
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
        }
        let mut legend: Vec<(&str, String)> = vec![(p.dim.as_str(), "  Less ".into())];
        let swatches: Vec<String> = HEAT_STEPS.iter().map(|(r, g, b)| tc::rgb(*r, *g, *b)).collect();
        for colour in &swatches {
            legend.push((colour.as_str(), "█".into()));
        }
        legend.push((p.dim.as_str(), " More".into()));
        rows.push(tc::seg(&legend, w - 1));
    }
    rows
}

/// Why Claude publishes no bar on the summary, when it does not.
///
/// The tab still has the stats cache: that is spend on this machine, not a
/// live account window. The two used to look like the same empty pane.
pub fn why_no_lane(c: &Data) -> String {
    if !lanes(c).is_empty() {
        return String::new();
    }
    why_no_limits(c)
}

/// The same sentence, for the tab, where an extra-usage lane does not answer
/// for the missing bars.
///
/// `why_no_lane` goes quiet as soon as there is any lane at all, which is
/// right for the summary: a bar is drawn there and nothing needs explaining.
/// The tab's bars are the limits, and an account publishing a spend cap and
/// no limit percentages still has an unexplained gap where they should be.
fn why_no_limits(c: &Data) -> String {
    if !c.quota_why.is_empty() {
        return format!("no quota · {}", c.quota_why);
    }
    if c.quota.is_some() {
        return "no quota · Anthropic answered, and published no limit percentages.".into();
    }
    "no quota · no live reading, and Claude Code has left no cached usage on this machine.".into()
}

/// Every quota Claude publishes, in the shape the summary screen compares.
///
/// The same limits[] the tab renders, reduced to the four numbers that mean
/// the same thing across agents. Left in the server's own order, because
/// Claude's windows nest - the five-hour sits inside the weekly, which
/// contains the model-scoped limit in turn - and reading them widest-last
/// says more than reading them by percentage.
pub fn lanes(c: &Data) -> Vec<Lane> {
    let Some(u) = c.quota.as_ref() else {
        return Vec::new();
    };
    let mut found: Vec<&serde_json::Value> = u["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|l| !l["percent"].is_null())
        .collect();
    found.sort_by_key(|l| claude_lane_rank(l));
    let mut out: Vec<Lane> = found
        .into_iter()
        .map(|l| {
            let group = text(l, "group");
            let scope = text(&l["scope"]["model"], "display_name");
            let name = if !scope.is_empty() {
                scope
            } else if text(l, "kind") == "weekly_all" {
                "overall".to_string()
            } else if !group.is_empty() {
                group.clone()
            } else {
                match text(l, "kind") {
                    s if s.is_empty() => "?".into(),
                    s => s,
                }
            };
            let window = match group.as_str() {
                "session" => "5h",
                "weekly" => "7d",
                _ => "",
            };
            // A cached reading can be old enough that the window it names has
            // closed - this machine's was nine days back, naming windows that
            // ended on the sixteenth. Claude's windows are fixed lengths, so
            // the one we are in now is that one rolled forward, and a
            // countdown to it is worth more than the blank a past date left.
            //
            // The percentage is not rolled with it. That belonged to the
            // closed window and the current one's spend is unmeasured, which
            // is why `projected` also suppresses the pace.
            let secs = CLAUDE_WINDOW_SECS
                .iter()
                .find(|(g, _)| *g == group)
                .map(|(_, s)| *s);
            let rolled = match (iso_epoch(&text(l, "resets_at")), secs) {
                (Some(at), Some(len)) if at <= now() && len > 0.0 => {
                    let skipped = ((now() - at) / len).floor() + 1.0;
                    (Some(at + skipped * len), true)
                }
                (at, _) => (at, false),
            };
            Lane {
                label: format!("{} {}", name, window).trim().to_string(),
                pct: num(l, "percent"),
                window_secs: CLAUDE_WINDOW_SECS
                    .iter()
                    .find(|(g, _)| *g == group)
                    .map(|(_, s)| *s),
                reset: rolled.0,
                stale: reading_is_old(c.quota_at),
                projected: rolled.1,
                            apart: false,
            }
        })
        .collect();
    // Extra usage is the account's monthly cap and belongs with the windows
    // above it, but Claude states no reset for it - there is no `resets_at`
    // anywhere in the spend block. So the lane carries no window, and the
    // summary draws no countdown and no pace rather than a figure worked out
    // from a date nobody sent.
    //
    // Last, not ranked: Claude's lanes are read in the order the account's
    // own limits nest, and this is not one of them.
    if let Some((pct, cap)) = extra_lane(&extra_state(u)) {
        out.push(Lane {
            label: format!("extra{}", cap),
            pct,
            window_secs: None,
            reset: None,
            stale: reading_is_old(c.quota_at),
            projected: false,
            apart: false,
        });
    }
    out
}

/// The whole tab: the quota, what the machine recorded, what it cost, and
/// which subscription the percentages are percentages of.
pub fn tab(c: &Data, w: usize, _h: usize, cfg: &Config, p: &Palette) -> Vec<String> {
    let body = add_section(claude_tab(c, w, p), claude_metered(c, w, cfg, p));
    match c.profile.as_ref() {
        Some(prof) => add_section(body, claude_plan_rows(prof, w, p)),
        None => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule the fossil taught: Claude Code's cache is worth reading only
    /// while Claude Code itself would read it.
    #[test]
    fn claude_codes_cache_is_dropped_at_the_age_it_drops_it() {
        // From its own reader, identical in 2.1.233 and 2.1.243:
        //   let r = Date.now() - n.data.fetchedAtMs;
        //   if (r < 0 || r > zNo) return null;      // zNo = 3600000
        assert_eq!(CLAUDE_CACHE_TTL, 3600.0);
        // The machine this was written on had one 9.6 days old, drawn as a
        // current percentage because nothing here checked the age at all.
        assert!(9.6 * 86400.0 > CLAUDE_CACHE_TTL);
        assert!(59.0 * 60.0 <= CLAUDE_CACHE_TTL);
        assert!(61.0 * 60.0 > CLAUDE_CACHE_TTL);
    }

    /// A Claude Code cache file of a given age, as it writes them.
    fn their_cache(dir: &std::path::Path, name: &str, age_secs: f64, pct: i64) -> String {
        let path = dir.join(name);
        let body = serde_json::json!({
            "cachedUsageUtilization": {
                "fetchedAtMs": (now() - age_secs) * 1000.0,
                "utilization": {"limits": [{"group": "session", "kind": "session",
                                            "percent": pct,
                                            "resets_at": "2026-08-16T13:59:59.851930+00:00"}]},
            }
        });
        std::fs::write(&path, body.to_string()).unwrap();
        path.to_string_lossy().to_string()
    }

    /// The bug this whole thing was written for, pinned on disk.
    ///
    /// A 9.6-day-old Claude Code cache and nothing of our own used to come
    /// back as a percentage and get drawn as today's. Claude Code's own
    /// reader returns null for it, and now so does this.
    #[test]
    fn the_tab_says_why_it_is_on_a_cached_reading() {
        // The whole point of keeping curl's message. "cached 4m ago" leaves
        // a reader to guess whether their login lapsed; this morning that
        // guess cost an hour on a credential with three hours left on it.
        let p = palette();
        let bare = |rows: Vec<String>| {
            rows.iter()
                .map(|r| {
                    let mut out = String::new();
                    let mut cs = r.chars();
                    while let Some(c) = cs.next() {
                        if c == '\x1b' {
                            for n in cs.by_ref() {
                                if n.is_ascii_alphabetic() {
                                    break;
                                }
                            }
                        } else {
                            out.push(c)
                        }
                    }
                    out
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let with = |live: bool, why: &str| {
            let d = Data {
                quota: Some(serde_json::json!({
                    "limits": [{"group": "session", "kind": "session", "percent": 20,
                                "resets_at": "2026-08-26T00:00:00.000000+00:00"}]
                })),
                quota_live: live,
                quota_at: now() - 240.0,
                quota_why: why.to_string(),
                ..Data::default()
            };
            bare(claude_quota(&d, 100, &p))
        };

        let refused = with(false, "too many requests - something else is polling the same token");
        assert!(refused.contains("cached"), "{}", refused);
        assert!(
            refused.contains("too many requests"),
            "the reason was dropped again: {}",
            refused
        );

        // No reason recorded is the old wording, unchanged.
        // The heading carries a · of its own, so the check is for the
        // reason's words rather than for the separator - which is what the
        // first version of this test got wrong, and why it was run before
        // being believed.
        let quiet = with(false, "");
        assert!(quiet.contains("cached"), "{}", quiet);
        assert!(
            !quiet.contains("too many requests"),
            "a reason appeared from nowhere: {}",
            quiet
        );

        // And a live reading says nothing about refusals at all.
        let good = with(true, "");
        assert!(good.contains("live"), "{}", good);
        assert!(!good.contains("cached"), "{}", good);
    }

    #[test]
    fn the_lane_takes_its_star_from_the_age_and_not_from_quota_live() {
        // The wiring, not just the rule. A cached reading taken a moment ago
        // must come through unflagged, and a live-flagged one taken days ago
        // must not - which is the pair the old `!quota_live` got backwards.
        let with = |live: bool, taken: f64| {
            let d = Data {
                quota: Some(serde_json::json!({
                    "limits": [{"group": "session", "kind": "session", "percent": 20,
                                "resets_at": "2126-01-01T00:00:00.000000+00:00"}]
                })),
                quota_live: live,
                quota_at: taken,
                ..Data::default()
            };
            let got = lanes(&d);
            assert_eq!(got.len(), 1, "the fixture should make exactly one lane");
            got[0].stale
        };
        assert!(!with(false, now() - 35.0), "a 35s cached reading is not old");
        assert!(
            !with(false, now() - (CLAUDE_FRESH_FOR - 30.0)),
            "just inside the window is not old"
        );
        assert!(
            with(false, now() - (CLAUDE_FRESH_FOR + 30.0)),
            "past the window it has to be flagged"
        );
        assert!(
            with(true, now() - 9.6 * 86400.0),
            "calling a nine-day-old reading live does not make it current"
        );
        assert!(with(false, now() - 9.6 * 86400.0));
    }

    #[test]
    fn a_reading_is_flagged_by_its_age_not_by_who_fetched_it() {
        // The case that prompted this: a cached reading 35 seconds old sat
        // under a star while a live one four minutes old carried none. Both
        // are current; only one was being doubted.
        assert!(!reading_is_old(now() - 35.0), "35s is fresher than most live reads");
        assert!(!reading_is_old(now() - CLAUDE_LIVE_TTL), "a live read may be this old");
        assert!(
            !reading_is_old(now() - (CLAUDE_FRESH_FOR - 30.0)),
            "just inside the window is still current"
        );
        assert!(
            reading_is_old(now() - (CLAUDE_FRESH_FOR + 30.0)),
            "past twice the fetch interval it has to say so"
        );
        // The one this all started with.
        assert!(reading_is_old(now() - 9.6 * 86400.0));
        // A reading with no timestamp at all is not to be trusted quietly.
        assert!(reading_is_old(0.0));
    }

    #[test]
    fn a_fossil_of_theirs_is_not_a_reading() {
        let dir = std::env::temp_dir().join(format!("tt-fossil-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ours = dir.join("none.json").to_string_lossy().to_string();

        let fossil = their_cache(&dir, "fossil.json", 9.6 * 86400.0, 11);
        assert!(
            stale_from(&ours, &fossil).is_none(),
            "a cache its own owner discards was offered as a reading"
        );

        // Inside the hour it is still theirs to offer, and we take it.
        let recent = their_cache(&dir, "recent.json", 30.0 * 60.0, 22);
        let (got, at) = stale_from(&ours, &recent).expect("a half-hour-old reading is good");
        assert_eq!(got["limits"][0]["percent"], 22);
        assert!(now() - at < 3600.0);

        // The boundary is theirs, not ours: an hour and a minute is out.
        let edge = their_cache(&dir, "edge.json", 61.0 * 60.0, 33);
        assert!(stale_from(&ours, &edge).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And with both on disk, the fresher one wins - which is the point of
    /// keeping our own at all.
    #[test]
    fn our_snapshot_beats_their_cache_when_it_is_newer() {
        let dir = std::env::temp_dir().join(format!("tt-both-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ours = dir.join("ours.json").to_string_lossy().to_string();

        // Ours taken just now, theirs half an hour ago and still valid.
        save_snapshot_at(&ours, &serde_json::json!({
            "limits": [{"group": "session", "kind": "session", "percent": 77,
                        "resets_at": "2026-08-25T03:39:59.750751+00:00"}]
        }));
        let theirs = their_cache(&dir, "theirs.json", 30.0 * 60.0, 22);
        let (got, _) = stale_from(&ours, &theirs).expect("something should come back");
        assert_eq!(got["limits"][0]["percent"], 77, "the older reading won");

        // With theirs expired, ours is the only one left - which is the
        // state this machine is actually in.
        let fossil = their_cache(&dir, "fossil.json", 9.6 * 86400.0, 11);
        let (got, _) = stale_from(&ours, &fossil).expect("ours should still answer");
        assert_eq!(got["limits"][0]["percent"], 77);

        // And with neither, nothing - not an error, just a machine that has
        // never had a live reading.
        let nowhere = dir.join("nothing.json").to_string_lossy().to_string();
        assert!(stale_from(&nowhere, &nowhere).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_reading_comes_back_the_way_it_went_in() {
        let dir = std::env::temp_dir().join(format!("tt-claude-{}", std::process::id()));
        let path = dir.join("claude-usage.json");
        let path = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);
        // Nothing saved is not a failure - it is a machine that has never
        // had a live reading.
        assert!(read_snapshot_at(&path).is_none());

        let u = serde_json::json!({
            "limits": [{"group": "session", "kind": "session", "percent": 42,
                        "resets_at": "2026-08-25T03:39:59.750751+00:00"}]
        });
        save_snapshot_at(&path, &u);
        let (back, at) = read_snapshot_at(&path).expect("saved and not read back");
        assert_eq!(back, u, "the reading changed shape crossing the disk");
        assert!(
            (now() - at).abs() < 60.0,
            "the stamp is the moment it was taken, not the epoch: {}",
            at
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_later_reading_wins_and_neither_is_not_a_failure() {
        assert_eq!(fresher(Some(("ours", 100.0)), Some(("theirs", 50.0))), Some(("ours", 100.0)));
        assert_eq!(fresher(Some(("ours", 50.0)), Some(("theirs", 100.0))), Some(("theirs", 100.0)));
        assert_eq!(fresher(Some(("ours", 10.0)), None), Some(("ours", 10.0)));
        assert_eq!(fresher(None, Some(("theirs", 10.0))), Some(("theirs", 10.0)));
        assert_eq!(fresher::<&str>(None, None), None);
        // A tie goes to ours - we know when and how it was taken.
        assert_eq!(fresher(Some(("ours", 42.0)), Some(("theirs", 42.0))), Some(("ours", 42.0)));
    }

    #[test]
    fn a_missing_quota_says_which_step_failed() {
        let token = Data {
            quota_why: "no token - Claude Code has not signed in here".into(),
            ..Data::default()
        };
        let note = why_no_lane(&token);
        assert!(note.starts_with("no quota · "), "{note}");
        assert!(note.contains("not signed in"), "{note}");

        let empty = why_no_lane(&Data::default());
        assert!(empty.contains("no live reading"), "{empty}");
        assert!(lanes(&Data::default()).is_empty());
    }

    /// A drawn row without its colour escapes, so a length means cells on
    /// screen rather than bytes in the string.
    fn bare(row: &str) -> String {
        let mut out = String::new();
        let mut chars = row.chars();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                out.push(c);
                continue;
            }
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
        out
    }

    /// The spend and extra-usage blocks exactly as the account measured here
    /// sent them: a fifty-dollar monthly cap, enabled, nothing spent, and
    /// billed in AUD rather than dollars. The only state ever seen live.
    fn measured() -> serde_json::Value {
        serde_json::json!({
            "extra_usage": {
                "credits_ever_enabled": true,
                "currency": "AUD",
                "decimal_places": 2,
                "disabled_reason": null,
                "is_enabled": true,
                "monthly_limit": 5000,
                "spend_limit_reached": false,
                "used_credits": 0.0,
                "user_disabled": false
            },
            "spend": {
                "can_purchase_credits": false,
                "enabled": true,
                "limit": { "amount_minor": 5000, "currency": "AUD", "exponent": 2 },
                "percent": 0,
                "severity": "normal",
                "used": { "amount_minor": 0, "currency": "AUD", "exponent": 2 }
            }
        })
    }

    fn extra_line(u: &serde_json::Value, w: usize) -> String {
        bare(&extra_row(&extra_state(u), w, &palette()))
    }

    /// 5000 minor units with an exponent of 2 is fifty, and reading it as
    /// five thousand is a hundredfold error in a figure about money.
    #[test]
    fn the_cap_is_read_in_the_minor_units_it_arrives_in() {
        let got = extra_line(&measured(), 80);
        assert!(got.contains("0.00 of 50.00 AUD"), "{}", got);
        assert!(!got.contains("5000"), "minor units drawn as money: {}", got);
    }

    /// The line used to be drawn only for an account with extra usage
    /// enabled and a cap present. Switched off, it drew nothing at all - and
    /// nothing reads as "this account has no extra usage" when it means the
    /// opposite: money was spent and can no longer be.
    #[test]
    fn a_disabled_cap_still_draws_what_was_spent_before_it_went() {
        let mut u = measured();
        u["spend"]["enabled"] = serde_json::json!(false);
        u["extra_usage"]["is_enabled"] = serde_json::json!(false);
        u["spend"]["used"]["amount_minor"] = serde_json::json!(1000);
        u["extra_usage"]["used_credits"] = serde_json::json!(10.0);
        let got = extra_line(&u, 80);
        assert!(got.contains("disabled"), "{}", got);
        assert!(got.contains("10.00 AUD"), "billable money dropped: {}", got);
    }

    /// The assumption this rests on, named where it is made: `user_disabled`
    /// on its own is taken to mean off, without `spend.enabled` agreeing.
    /// Not measured - no account here has ever had it set.
    #[test]
    fn the_account_switching_it_off_is_enough_on_its_own() {
        let mut u = measured();
        u["extra_usage"]["user_disabled"] = serde_json::json!(true);
        assert!(matches!(extra_state(&u), Extra::Off { .. }));
    }

    /// A shape this parser has not seen is never drawn as one of the others,
    /// and the keys that did arrive come with it so the next one can be
    /// mapped off the pane rather than guessed at.
    #[test]
    fn an_unrecognised_spend_block_says_so_and_names_its_keys() {
        let u = serde_json::json!({
            "extra_usage": {},
            "spend": { "ration": 4, "tokens_left": 900 }
        });
        let got = extra_line(&u, 80);
        assert!(got.contains("not reported"), "{}", got);
        assert!(got.contains("ration") && got.contains("tokens_left"), "{}", got);
        assert!(!got.contains("0.00"), "an unread shape drew a figure: {}", got);
    }

    /// An absent block is not a cap of nothing. It says the same thing as an
    /// unreadable one, with no keys to name.
    #[test]
    fn no_spend_block_at_all_is_not_a_cap_of_zero() {
        let got = extra_line(&serde_json::json!({}), 80);
        assert!(got.contains("not reported"), "{}", got);
        assert!(!got.contains("0.00 of 0.00"), "{}", got);
    }

    /// The cap can be lowered below what has already gone. The figures stay
    /// the real ones, and the excess is written as an overage: "-9.00 left"
    /// is arithmetic where a reader needs a fact.
    #[test]
    fn a_cap_lowered_under_what_has_gone_reads_as_an_overage() {
        let mut u = measured();
        u["spend"]["limit"]["amount_minor"] = serde_json::json!(100);
        u["spend"]["used"]["amount_minor"] = serde_json::json!(1000);
        let got = extra_line(&u, 80);
        assert!(got.contains("10.00 of 1.00 AUD"), "{}", got);
        assert!(got.contains("9.00 over"), "{}", got);
        assert!(!got.contains("-9.00"), "a negative remainder: {}", got);
        // Unclamped on the summary too: over the cap is a real number over a
        // hundred, and the bar beside it draws full.
        let (pct, _) = extra_lane(&extra_state(&u)).expect("a lane");
        assert_eq!(pct, 1000.0);
    }

    /// The server's own judgement, not a threshold invented here. Both
    /// fields can say the same thing and either alone is enough.
    #[test]
    fn the_servers_severity_colours_the_figure() {
        let p = palette();
        let calm = extra_row(&extra_state(&measured()), 80, &p);
        assert!(!calm.contains(&p.bad), "a normal cap drawn as trouble");
        for field in ["severity", "spend_limit_reached"] {
            let mut u = measured();
            match field {
                "severity" => u["spend"]["severity"] = serde_json::json!("critical"),
                _ => u["extra_usage"]["spend_limit_reached"] = serde_json::json!(true),
            }
            let hot = extra_row(&extra_state(&u), 80, &p);
            assert!(hot.contains(&p.bad), "{} went uncoloured", field);
        }
    }

    /// The cap rides on the label because a percentage of an unnamed limit
    /// is not a number anyone can act on - and in the account's own
    /// currency, since `extra $50` on an AUD account is a claim about the
    /// money that nobody made.
    #[test]
    fn extra_usage_is_a_lane_carrying_its_cap_and_its_currency() {
        let mut c = Data { quota: Some(measured()), ..Default::default() };
        c.quota.as_mut().unwrap()["limits"] = serde_json::json!([
            {"group": "session", "kind": "session", "percent": 20, "resets_at": null}
        ]);
        let got = lanes(&c);
        let last = got.last().expect("a lane");
        assert_eq!(last.label, "extra AUD 50");
        // No `resets_at` anywhere in the spend block, so no countdown and no
        // pace rather than a figure worked out from a date nobody sent.
        assert_eq!(last.window_secs, None);
        assert_eq!(last.reset, None);
        assert_eq!(lead(last.pct, last.window_secs, last.reset), None);
        // It sits after the windows, not ranked among them.
        assert_eq!(got.len(), 2);
    }

    /// Dollars keep their sign. Only a currency that is not dollars carries
    /// its code, and the label column is the same seven cells either way.
    #[test]
    fn a_dollar_cap_still_reads_as_dollars() {
        let mut u = measured();
        for block in ["spend", "extra_usage"] {
            u[block]["currency"] = serde_json::json!("USD");
        }
        u["spend"]["limit"]["currency"] = serde_json::json!("USD");
        let (_, cap) = extra_lane(&extra_state(&u)).expect("a lane");
        assert_eq!(cap, " $50");
        assert!(cap.chars().count() <= CAP_TAG_ROOM);
        assert!(
            extra_lane(&extra_state(&measured())).unwrap().1.chars().count() <= CAP_TAG_ROOM,
            "an AUD cap grew the shared label column"
        );
    }

    /// There is no state for "allowed, with no ceiling". Cursor has one
    /// because Cursor sends a block that says so; nothing Claude sends has
    /// been seen to mean it, so a cap of zero lands in `NotReported` and
    /// waits to be mapped rather than being drawn as unlimited.
    #[test]
    fn an_enabled_block_with_no_cap_is_not_read_as_unlimited() {
        let mut u = measured();
        u["spend"]["limit"] = serde_json::json!(null);
        assert!(matches!(extra_state(&u), Extra::NotReported { .. }));
        assert_eq!(extra_lane(&extra_state(&u)), None);
    }

    /// The same money arrives twice, in two blocks with different units.
    /// `spend` is the one read, because it carries its own currency and
    /// exponent. This holds the other to it, so a payload where they part
    /// company fails here rather than being quietly halved on screen.
    #[test]
    fn the_two_statements_of_the_same_spend_agree() {
        let u = measured();
        let (x, spend) = (&u["extra_usage"], &u["spend"]);
        let places = 10f64.powf(x["decimal_places"].as_f64().unwrap());
        assert_eq!(minor(&spend["used"]).unwrap(), x["used_credits"].as_f64().unwrap());
        assert_eq!(
            minor(&spend["limit"]).unwrap(),
            x["monthly_limit"].as_f64().unwrap() / places
        );
        // And the server's whole-number percentage agrees with the one drawn
        // from the pair, which is kept at full precision because a real
        // 0.8% rounding to `0%` looks exactly like an empty section.
        let (pct, _) = extra_lane(&extra_state(&u)).expect("a lane");
        assert_eq!(pct.round(), spend["percent"].as_f64().unwrap());
    }

    /// The quota block draws nothing without lanes, and the line lived
    /// inside it - so a response carrying a spend block and no limits lost
    /// the one figure on the tab that is real money.
    #[test]
    fn a_quota_with_no_lanes_still_draws_the_money() {
        let mut u = measured();
        u["limits"] = serde_json::json!([]);
        u["spend"]["used"]["amount_minor"] = serde_json::json!(1234);
        let c = Data { quota: Some(u), quota_live: true, ..Default::default() };
        let joined = claude_tab(&c, 90, &palette()).join("\n");
        assert!(bare(&joined).contains("12.34 of 50.00 AUD"), "{}", bare(&joined));
        // And the money does not displace the reason the bars are missing.
        // `why_no_lane` falls silent here - an extra-usage lane is a lane, so
        // the summary has a bar and needs no sentence - but the tab's bars
        // are the limits, and their absence is still worth saying.
        assert!(why_no_lane(&c).is_empty(), "the summary has a lane to draw");
        assert!(
            bare(&joined).contains("published no limit percentages"),
            "the tab lost its reason: {}",
            bare(&joined)
        );
        // An unreadable block has nothing to say here: the note above
        // already covers a quota that answered nothing.
        let empty = Data { quota: Some(serde_json::json!({})), ..Default::default() };
        assert!(!bare(&claude_tab(&empty, 90, &palette()).join("\n")).contains("extra usage"));
    }

    /// A row wider than the pane is worse than a row that was cut, and both
    /// tails here are arithmetic on figures already on the line - so they go
    /// before the line does.
    #[test]
    fn the_line_sheds_its_tail_rather_than_overflowing() {
        let mut u = measured();
        u["spend"]["used"]["amount_minor"] = serde_json::json!(1234);
        for w in 20..=120 {
            let got = extra_line(&u, w);
            assert!(got.chars().count() <= w - 1, "width {}: {:?}", w, got);
            assert!(got.contains("12.34"), "the spend itself went: {:?}", got);
            // Shed, not cut. A row that overflows is clipped by `seg` rather
            // than wrapping, so measuring the length alone cannot tell the
            // two apart - a tail dropped whole and a tail sliced through the
            // middle of its own number are both short enough. The clause is
            // either there complete or not there at all.
            assert!(
                !got.contains('·') || got.ends_with("left") || got.ends_with("over"),
                "width {} cut the tail mid-clause: {:?}",
                w,
                got
            );
            assert!(
                !got.contains(" l") || got.contains(" limit"),
                "width {} cut the word itself: {:?}",
                w,
                got
            );
        }
        assert!(extra_line(&u, 120).contains("37.66 left"));
        assert!(!extra_line(&u, 34).contains("left"));
    }
}
