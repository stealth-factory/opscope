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

//! OpenAI Codex: its rollouts, and the account-wide quota the CLI itself reads.
//!
//! ~/.codex/logs is diagnostics and carries no counters, which is where an
//! earlier look stopped. The sessions directory is the one that counts: each
//! rollout is a JSONL transcript whose `token_count` events carry both the
//! session's running total and the per-turn delta.
//!
//! The per-turn deltas are what get summed. `total_token_usage` is
//! cumulative for the *session*, and a session spans several files - resuming
//! writes a new rollout for a session that already existed - so summing one
//! tail per file counted most sessions two or three times over.

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use opscope_core as tc;

use crate::shared::*;
use crate::*;

/// The endpoint the Codex CLI itself asks for the account's quota. Found by
/// reading how CodexBar does it (github.com/steipete/CodexBar), which
/// documents it.
const CODEX_USAGE_API: &str = "https://chatgpt.com/backend-api/wham/usage";
/// Unused rate-limit reset credits still in the account. The same host and
/// the same credential as the usage call, and a different body: a list of
/// credits with expiries, not the 5h and 7d windows.
const CODEX_RESET_CREDITS_API: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";
/// Seconds; outside this range two `token_count` stamps do not bracket a turn.
const MIN_GAP: f64 = 0.5;
const MAX_GAP: f64 = 300.0;
/// Rollouts to tail looking for the newest quota snapshot. More than one
/// because a session that has only just opened has no answer back yet.
const SNAPSHOT_FILES: usize = 5;

/// The token totals, in the names OpenAI uses for them.
///
/// `input` already contains `cached`, and `output` already contains
/// `reasoning`; both are carried separately because they are worth reading,
/// and neither is added to `all`, which would count them twice.
#[derive(Clone, Default)]
struct Totals {
    input: f64,
    output: f64,
    reasoning: f64,
    cached: f64,
    all: f64,
}

/// What the rollouts on this machine recorded, plus the account's quota.
#[derive(Clone, Default)]
pub struct Data {
    /// False when there are no rollouts here at all - which is a fact about
    /// this machine, not about the account, so the quota is still shown.
    ok: bool,
    /// The live account-wide reading from the endpoint the CLI uses.
    live: Option<serde_json::Value>,
    /// Reset credits still in the account. Absent when the reset-credit
    /// inventory could not be read — never a guessed zero. The usage payload
    /// is not a source.
    bank: Option<crate::parse::ResetBank>,
    /// Why there is no live reading, when there is none. Empty while the
    /// endpoint is answering, or when nothing has been asked yet.
    live_why: String,
    /// The rate_limits the newest rollout recorded, for when the live call
    /// cannot run. A snapshot from whenever Codex last spoke to the server.
    limits: Option<serde_json::Value>,
    sessions: usize,
    files: usize,
    /// Modification time of the newest rollout.
    last: f64,
    total: Totals,
    /// Output tokens per second, sorted, from the newest rollout.
    rates: Vec<f64>,
    /// day -> model -> tokens by priced kind, plus reasoning.
    daily: HashMap<String, HashMap<String, Tokens>>,
}

impl Data {
    /// `N reset available` when the inventory was read and the count is
    /// greater than zero.
    ///
    /// `None` when the inventory was not read, or when it was read and the
    /// count is zero. The parenthetical is the soonest expiry, and only
    /// when every listed credit has a readable one. A credit with no
    /// readable expiry keeps the count and drops the date: a dated
    /// neighbour is not the soonest. A count with no list has no
    /// parenthetical, because no date was in the payload.
    pub(crate) fn reset_summary_line(&self) -> Option<String> {
        let bank = self.bank.as_ref()?;
        if bank.left == 0 {
            return None;
        }
        Some(match soonest_when_every_credit_is_dated(bank) {
            Some(stamp) => format!("{} reset available ({stamp})", bank.left),
            None => format!("{} reset available", bank.left),
        })
    }

    /// A live window, and a bank only when `bank` is `Some`.
    #[cfg(test)]
    pub(crate) fn with_window_and_bank(pct: Option<f64>, bank: Option<u64>) -> Self {
        Self::with_reset(pct, bank.map(|left| (left, None)))
    }

    /// A live window and, when `soonest` is set, that many dated credits.
    #[cfg(test)]
    pub(crate) fn with_reset(pct: Option<f64>, bank: Option<(u64, Option<f64>)>) -> Self {
        Self {
            live: pct.map(|pct| {
                serde_json::json!({
                    "rate_limit": {
                        "primary_window": {
                            "used_percent": pct,
                            "limit_window_seconds": 604800
                        }
                    }
                })
            }),
            bank: bank.map(|(left, soonest)| crate::parse::ResetBank {
                left,
                credits: match soonest {
                    Some(expiry) if left > 0 => (0..left)
                        .map(|i| crate::parse::ResetCredit {
                            title: None,
                            expiry: Some(expiry + i as f64 * 86_400.0),
                        })
                        .collect(),
                    _ => Vec::new(),
                },
            }),
            ..Self::default()
        }
    }
}

/// Account-wide quota, live from the same endpoint the Codex CLI uses.
///
/// The rollouts carry a rate_limits snapshot, but only from whenever Codex
/// last ran - it can be days stale. This is the current figure, and it is the
/// account rather than this machine.
///
/// The token comes from ~/.codex/auth.json and goes to the same host Codex
/// itself talks to; it is never printed. Any failure falls back to the
/// snapshot, so an expired token costs freshness and nothing else.
///
/// A missing token is a local fact and rides in the cached value. A silent
/// endpoint is a refusal: it stays `None` so `cached` backs off.
fn codex_live() -> Option<serde_json::Value> {
    let Some(auth) = read_json(&under_home(".codex/auth.json")) else {
        return Some(serde_json::json!({"why": "no token - Codex has not signed in here"}));
    };
    let tok = match text(&auth["tokens"], "access_token") {
        s if !s.is_empty() => s,
        _ => text(&auth, "access_token"),
    };
    if tok.is_empty() {
        return Some(serde_json::json!({"why": "no token - Codex has not signed in here"}));
    }
    let account = match text(&auth["tokens"], "account_id") {
        s if !s.is_empty() => s,
        _ => text(&auth, "account_id"),
    };
    let authz = format!("Bearer {}", tok);
    let usage_headers = [("Authorization", authz.as_str()), ("User-Agent", "opscope")];
    // The usage call stays as it was. The inventory is account-scoped, so
    // the account id rides on that request only, and only when it is a
    // header the client can send.
    let mut inventory_headers = usage_headers.to_vec();
    if account
        .bytes()
        .all(|b| (0x20..0x7f).contains(&b) && b != b'"')
        && !account.is_empty()
    {
        inventory_headers.push(("ChatGPT-Account-Id", account.as_str()));
    }
    let usage = get_json(CODEX_USAGE_API, &usage_headers, 20);
    // Same bound as the usage call, and still best effort. A miss here must
    // not throw away a usage reading, and it must not become a bank of zero.
    // A miss on usage must not skip this call either: the bank has its own
    // source.
    let bank = tc::get(CODEX_RESET_CREDITS_API, &inventory_headers, 20).ok();
    live_payload(usage, bank)
}

/// Pair a usage reading with an inventory body.
///
/// The two calls are independent. When usage did not answer and the
/// inventory did, the body rides along as `usage_miss` so the caller can
/// keep the bank and still treat the usage call as a refusal. When neither
/// answered there is nothing to draw.
fn live_payload(
    usage: Option<serde_json::Value>,
    bank: Option<String>,
) -> Option<serde_json::Value> {
    match usage {
        Some(usage) => Some(serde_json::json!({"u": usage, "bank": bank})),
        None => bank.map(|bank| serde_json::json!({"usage_miss": true, "bank": bank})),
    }
}

/// The bank to draw. The only source is the inventory body from
/// `GET /wham/rate-limit-reset-credits`. A usage payload that happens to
/// carry a summary count is not read: that number did not come from the
/// inventory.
fn bank_of(inventory: Option<&str>, now: f64) -> Option<crate::parse::ResetBank> {
    crate::parse::parse_codex_reset_credits(inventory?, now)
}

/// Per-turn, per-model token counts from one rollout's text.
///
/// The model is not on the token counts: it arrives in `turn_context`, one
/// per turn, and applies to the `token_count` events that follow it. So the
/// lines are walked in order, carrying the model forward.
///
/// `last_token_usage` is the per-turn delta - the running total is on every
/// event, and summing those would count the session once per turn. Within
/// input_tokens, cached_input_tokens is the cheaper subset, and within
/// output_tokens the reasoning tokens are already included, so only the
/// uncached remainder is charged at the input rate.
///
/// Keyed by session and timestamp, not by filename and not by a sequence
/// number: a resumed session replays its earlier events into a new file, so
/// the same turn is written twice with the same stamp. Sequence numbers
/// restart per file and would pair unrelated events, and `ordinal` is absent
/// from older rollouts.
fn rollout_records(body: &str, fallback: &str) -> HashMap<String, (String, String, Tokens)> {
    let mut records: HashMap<String, (String, String, Tokens)> = HashMap::new();
    let mut session = fallback.to_string();
    let mut model = String::new();
    for line in body.lines() {
        if !line.contains("\"model\"")
            && !line.contains("\"token_count\"")
            && !line.contains("\"session_meta\"")
        {
            continue;
        }
        let Ok(r) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let payload = &r["payload"];
        match text(&r, "type").as_str() {
            "session_meta" => {
                let id = text(payload, "session_id");
                if !id.is_empty() {
                    session = id;
                }
                continue;
            }
            "turn_context" => {
                let named = text(payload, "model");
                if !named.is_empty() {
                    model = named;
                }
                continue;
            }
            _ => {}
        }
        if text(payload, "type") != "token_count" || model.is_empty() {
            continue;
        }
        let used = &payload["info"]["last_token_usage"];
        let stamp = text(&r, "timestamp");
        let Some(when) = iso_epoch(&stamp) else {
            continue;
        };
        let cached = num(used, "cached_input_tokens");
        let mut got = empty_tokens();
        got.insert("reasoning".into(), num(used, "reasoning_output_tokens"));
        *got.get_mut("input").unwrap() = (num(used, "input_tokens") - cached).max(0.0);
        *got.get_mut("cache_read").unwrap() = cached;
        *got.get_mut("cache_write").unwrap() = num(used, "cache_write_input_tokens");
        *got.get_mut("output").unwrap() = num(used, "output_tokens");
        if got.values().all(|n| *n == 0.0) {
            continue;
        }
        let day = Local
            .timestamp_opt(when as i64, 0)
            .single()
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        records.insert(
            format!("{}\u{0}{}", session, stamp),
            (day, model.clone(), got),
        );
    }
    records
}

/// One rollout's records, parsed once.
///
/// Cached on (mtime, size): a finished rollout never changes, and some run to
/// thirty megabytes, so the full parse happens once per file rather than on
/// every refresh.
fn scan_rollout(caches: &mut Caches, path: &str) -> HashMap<String, (String, String, Tokens)> {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return HashMap::new();
    };
    let key = (meta.mtime() as u64, meta.size());
    if let Some((had, records)) = caches.transcripts.get(path) {
        if *had == key {
            return records.clone();
        }
    }
    let Ok(body) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    // The basename only stands in until session_meta names the session. Two
    // rollouts in different directories can share one, so it is a last resort
    // rather than an identifier.
    let base = path.rsplit('/').next().unwrap_or(path);
    let records = rollout_records(&body, base);
    caches
        .transcripts
        .insert(path.to_string(), (key, records.clone()));
    records
}

/// Every rollout on this machine, newest first.
fn rollout_files() -> Vec<String> {
    let mut files = Vec::new();
    walk(&under_home(".codex/sessions"), ".jsonl", &mut files);
    newest_first(files)
}

/// Per-day, per-model tokens, and how many distinct sessions they came from.
///
/// Sessions rather than rollout files: thirty rollouts here hold eight
/// sessions, because resuming writes a new file for a session that already
/// existed. Counting files and calling them sessions was the same mistake
/// that made the totals wrong, in the label.
fn merge_days(
    seen: HashMap<String, (String, String, Tokens)>,
) -> (HashMap<String, HashMap<String, Tokens>>, usize) {
    let mut sessions: HashSet<String> = HashSet::new();
    let mut merged: HashMap<String, HashMap<String, Tokens>> = HashMap::new();
    for (key, (day, model, tokens)) in seen {
        if let Some(id) = key.split('\u{0}').next() {
            sessions.insert(id.to_string());
        }
        let bucket = merged
            .entry(day)
            .or_default()
            .entry(model)
            .or_insert_with(empty_tokens);
        // Every kind, not just the priced ones: reasoning is carried
        // alongside them and iterating the rate card would drop it.
        for (kind, n) in &tokens {
            *bucket.entry(kind.clone()).or_insert(0.0) += n;
        }
    }
    (merged, sessions.len())
}

/// Totals from the de-duplicated per-turn deltas.
///
/// Not from the rollout tails. Summing one cumulative tail per file counted
/// most sessions two or three times over: 664.5M against a true 370.0M for
/// the primary model. Summing the deltas instead reproduces Codex's own
/// cumulative figure exactly on four of eight sessions, and picks up the
/// review model besides, which the session total never included.
fn codex_totals(daily: &HashMap<String, HashMap<String, Tokens>>) -> Totals {
    let mut out = Totals::default();
    let at = |t: &Tokens, kind: &str| t.get(kind).copied().unwrap_or(0.0);
    for models in daily.values() {
        for tokens in models.values() {
            out.input += at(tokens, "input") + at(tokens, "cache_read");
            out.cached += at(tokens, "cache_read");
            out.output += at(tokens, "output");
            out.reasoning += at(tokens, "reasoning");
        }
    }
    out.all = out.input + out.output;
    out
}

/// Output tokens per second, from the newest rollout only.
///
/// The gap between consecutive `token_count` events, which brackets one
/// turn's generation. The median is what gets shown: it barely moves
/// whichever way the outliers are trimmed, while the maximum moves by a
/// factor of twenty on the same data.
fn codex_rates(path: &str) -> Vec<f64> {
    let mut rates: Vec<f64> = Vec::new();
    let mut prev: Option<f64> = None;
    for line in tail_lines(path, 4 * 1024 * 1024) {
        if !line.contains("\"token_count\"") {
            continue;
        }
        let Ok(d) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(at) = iso_epoch(&text(&d, "timestamp")) else {
            continue;
        };
        let out = num(&d["payload"]["info"]["last_token_usage"], "output_tokens");
        if let Some(before) = prev {
            let gap = at - before;
            if out > 0.0 && gap > MIN_GAP && gap < MAX_GAP {
                rates.push(out / gap);
            }
        }
        prev = Some(at);
    }
    rates.sort_by(f64::total_cmp);
    rates
}

/// The newest rate_limits snapshot on disk.
///
/// The server returns the account's windows with each response and the
/// rollout writes them down, so the last one in the newest file is the
/// freshest thing here. Only the tail is read: this is the fallback for a
/// failed live call, and re-reading every rollout in full to find a number
/// that is repeated at the end of one of them would be daft.
fn newest_limits(files: &[String]) -> Option<serde_json::Value> {
    for path in files.iter().take(SNAPSHOT_FILES) {
        let mut found = None;
        for line in tail_lines(path, TAIL) {
            if !line.contains("\"rate_limits\"") {
                continue;
            }
            let Ok(d) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let payload = &d["payload"];
            let limits = match payload["rate_limits"].is_object() {
                true => payload["rate_limits"].clone(),
                false => payload["info"]["rate_limits"].clone(),
            };
            if limits.is_object() {
                found = Some(limits);
            }
        }
        if found.is_some() {
            return found;
        }
    }
    None
}

pub fn read(caches: &mut Caches, _cfg: &Config) -> Data {
    let mut refuse = String::new();
    let mut live_why = String::new();
    let mut bank = None;
    // Set inside the fetch, which `cached` does not run on a held reading.
    // A usage miss still carries the inventory body for this frame.
    let mut carried_bank: Option<String> = None;
    let live = match cached(caches, "codex", LIVE_TTL, || match codex_live() {
        Some(got) if got.get("usage_miss").is_some() => {
            carried_bank = got["bank"].as_str().map(str::to_string);
            refuse = "ChatGPT's usage endpoint did not answer".into();
            None
        }
        None => {
            refuse = "ChatGPT's usage endpoint did not answer".into();
            None
        }
        other => other,
    }) {
        Some(got) if !text(&got, "why").is_empty() => {
            live_why = text(&got, "why");
            bank = bank_of(got["bank"].as_str(), now());
            None
        }
        Some(got) if got.get("u").is_some() => {
            bank = bank_of(got["bank"].as_str(), now());
            Some(got["u"].clone())
        }
        other => {
            if let Some(got) = other.as_ref() {
                bank = bank_of(got["bank"].as_str(), now());
            }
            other
        }
    };
    if !refuse.is_empty() {
        live_why = refuse.clone();
        remember_refusal(caches, "codex", &refuse);
        // The refusal hold is `{"why"}` only. Put the inventory beside it
        // so the frames inside the backoff still draw the bank that arrived.
        if let Some(body) = carried_bank.clone() {
            if let Some((when, _, held)) = caches.live.get("codex").cloned() {
                caches.live.insert(
                    "codex".to_string(),
                    (
                        when,
                        Some(serde_json::json!({"why": &refuse, "bank": body})),
                        held,
                    ),
                );
            }
            bank = bank_of(Some(&body), now());
        }
    }
    let mut codex = Data {
        live,
        live_why,
        bank,
        ..Data::default()
    };
    let files = rollout_files();
    let Some(newest) = files.first().cloned() else {
        return codex;
    };
    use std::os::unix::fs::MetadataExt;
    codex.ok = true;
    codex.files = files.len();
    codex.last = std::fs::metadata(&newest)
        .map(|m| m.mtime() as f64)
        .unwrap_or(0.0);
    codex.limits = newest_limits(&files);
    let mut seen: HashMap<String, (String, String, Tokens)> = HashMap::new();
    for path in &files {
        seen.extend(scan_rollout(caches, path));
    }
    let (daily, sessions) = merge_days(seen);
    codex.total = codex_totals(&daily);
    codex.daily = daily;
    codex.sessions = sessions;
    codex.rates = codex_rates(&newest);
    codex
}

/// A window's length, said the way a reader would say it.
fn window_name(secs: Option<f64>) -> String {
    let secs = secs.unwrap_or(0.0) as i64;
    if secs >= 86400 {
        format!("{}d", secs / 86400)
    } else if secs > 0 {
        format!("{}h", secs / 3600)
    } else {
        "?".into()
    }
}

/// A JSON scalar as the text a reader would recognise.
///
/// Balances and spend limits arrive as numbers from one endpoint and as
/// strings from another, and `"12"` on screen with the quotes still on it is
/// not a balance.
fn scalar(v: &serde_json::Value) -> String {
    if let Some(n) = v.as_f64() {
        return format!("{}", n);
    }
    match v.as_str() {
        Some(s) => s.to_string(),
        None => "0".into(),
    }
}

/// One quota bar's worth of numbers, from whichever source answered.
struct Win {
    /// Empty for the account's own windows; a feature name for the rest.
    name: String,
    pct: f64,
    secs: f64,
    reset: Option<f64>,
}

/// The account's quota windows, live if the endpoint answered and from the
/// last session's snapshot if it did not.
///
/// The one genuine quota figure any of these agents publishes: the server
/// sends it back with each response, and the rollout records it.
fn codex_quota(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let mut wins: Vec<Win> = Vec::new();
    let mut plan = String::new();
    if let Some(live) = d.live.as_ref() {
        plan = text(live, "plan_type");
        for key in ["primary_window", "secondary_window"] {
            let win = &live["rate_limit"][key];
            if win["used_percent"].is_null() {
                continue;
            }
            wins.push(Win {
                name: String::new(),
                pct: num(win, "used_percent"),
                secs: num(win, "limit_window_seconds"),
                reset: win["reset_at"].as_f64(),
            });
        }
        // Some features meter separately from the account's general usage -
        // Spark is one - and each arrives named, with its own window and
        // reset. Rendering the list rather than the one name we know keeps
        // any future feature working without an edit.
        for extra in live["additional_rate_limits"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let win = &extra["rate_limit"]["primary_window"];
            if win["used_percent"].is_null() {
                continue;
            }
            wins.push(Win {
                name: match text(extra, "limit_name") {
                    s if s.is_empty() => "?".into(),
                    s => s,
                },
                pct: num(win, "used_percent"),
                secs: num(win, "limit_window_seconds"),
                reset: win["reset_at"].as_f64(),
            });
        }
    }
    let live_answered = !wins.is_empty();
    if !live_answered {
        // A snapshot with no percentage in it is not a lane. The percentage
        // is the only number on the row that cannot be inferred from the
        // others, so a window without one has nothing to draw.
        let win = d.limits.as_ref().map(|l| &l["primary"]);
        if let Some(win) = win.filter(|x| !x["used_percent"].is_null()) {
            wins.push(Win {
                name: String::new(),
                pct: num(win, "used_percent"),
                secs: num(win, "window_minutes") * 60.0,
                reset: win["resets_at"].as_f64(),
            });
        }
    }
    if wins.is_empty() {
        return Vec::new();
    }
    let source = if live_answered {
        "live"
    } else {
        "from the last session"
    };
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── QUOTA ── ".into()),
            (
                if live_answered {
                    p.ok.as_str()
                } else {
                    p.warn.as_str()
                },
                source.into(),
            ),
            (
                p.dim.as_str(),
                crate::claude::scope_phrase(w, 13 + source.len() + plan.len()).into(),
            ),
            (p.dim.as_str(), plan),
        ],
        w - 1,
    )];

    // Alone, the account-wide lanes are told apart by their window and a bare
    // "7d" is clear enough. Beside a named one it is not, so a lane says what
    // it covers only when there is something to confuse it with.
    let named = wins.iter().any(|x| !x.name.is_empty());
    let labels = |short: bool| -> Vec<String> {
        wins.iter()
            .map(|x| {
                // Spell a feature out while there is room; below that the
                // last segment carries it - GPT-5.3-Codex-Spark is Spark.
                let mut name = match short && !x.name.is_empty() {
                    true => x.name.rsplit('-').next().unwrap_or("").to_string(),
                    false => x.name.clone(),
                };
                if name.is_empty() && named {
                    name = "overall".into();
                }
                format!("{} {}", name, window_name(Some(x.secs)))
                    .trim()
                    .to_string()
            })
            .collect()
    };
    let mut lab = labels(false);
    let widest = |list: &[String]| list.iter().map(|x| x.chars().count()).max().unwrap_or(0);
    if w as i64 - 32 - (widest(&lab) as i64) < 20 {
        lab = labels(true);
    }
    let label_w = widest(&lab).max(9);
    let hue = agent_hue("codex");
    for (win, label) in wins.iter().zip(&lab) {
        let used = (win.pct / 100.0).clamp(0.0, 1.0);
        let secs = (win.secs > 0.0).then_some(win.secs);
        let when = match win.reset {
            None => String::new(),
            Some(at) if at - now() > 0.0 => format!("resets in {}", left_span(at - now())),
            Some(_) => "resetting".into(),
        };
        let (pace_colour, pace_txt) = pace_cell(lead(win.pct, secs, win.reset), p);
        let mut line: Vec<(String, String)> =
            vec![(p.dim.clone(), format!(" {} ", tc::pad(label, label_w)))];
        line.extend(paced_bar(
            used,
            elapsed_of(secs, win.reset),
            w.saturating_sub(34 + label_w).max(8),
            hue,
            p,
        ));
        line.push((pct_colour(win.pct, hue, p), pct_text(win.pct)));
        line.push((pace_colour, pace_txt));
        line.push((p.dim.clone(), format!("  {}", when)));
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&refs, w - 1));
    }
    rows.push(String::new());
    rows
}

/// What this machine's rollouts add up to.
fn codex_totals_rows(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let t = &d.total;
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── TOTALS ── ".into()),
            (
                p.dim.as_str(),
                format!("{} sessions · newest {} ago", d.sessions, ago(d.last)),
            ),
        ],
        w - 1,
    )];
    let cells: Vec<(&str, String, &str)> = vec![
        ("input tokens", big_num(t.input), p.txt.as_str()),
        ("output tokens", big_num(t.output), p.agent.as_str()),
        ("reasoning tokens", big_num(t.reasoning), p.txt.as_str()),
        ("cached input", big_num(t.cached), p.dim.as_str()),
        ("all tokens", big_num(t.all), p.txt.as_str()),
        ("rollout files", format!("{}", d.files), p.dim.as_str()),
    ];
    let label_w = cells.iter().map(|c| c.0.len()).max().unwrap_or(0);
    // Two columns while both fit; one when they do not. Spending extra width
    // on more content rather than on padding is the house rule, and a value
    // column under eight cells cannot hold "1.2M".
    let ncols = if (w as i64 - 2) / 2 - label_w as i64 - 3 >= 8 {
        2
    } else {
        1
    };
    let val_w = ((w - 2) / ncols).saturating_sub(label_w + 3).max(5);
    for chunk in cells.chunks(ncols) {
        let mut line: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
        for (label, value, colour) in chunk {
            line.push((p.dim.as_str(), format!(" {} ", tc::pad(label, label_w))));
            line.push((colour, tc::pad(value, val_w)));
        }
        rows.push(tc::seg(&line, w - 1));
    }
    rows
}

/// How fast it generates, and the shape of the distribution behind the median.
fn codex_rate_rows(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let rates = &d.rates;
    if rates.is_empty() {
        return Vec::new();
    }
    let med = rates[rates.len() / 2];
    let p90 = rates[((rates.len() as f64 * 0.9) as usize).min(rates.len() - 1)];
    let top = rates[rates.len() - 1];
    let mut rows = vec![
        tc::seg(
            &[
                (p.lbl.as_str(), " ── OUTPUT RATE ── ".into()),
                (
                    p.dim.as_str(),
                    format!("newest session, {} turns", rates.len()),
                ),
            ],
            w - 1,
        ),
        tc::seg(
            &[
                (p.dim.as_str(), "  median ".into()),
                (p.agent.as_str(), format!("{:.0}", med)),
                (p.dim.as_str(), " tok/s   p90 ".into()),
                (p.txt.as_str(), format!("{:.0}", p90)),
                (p.dim.as_str(), "   max ".into()),
                (p.txt.as_str(), format!("{:.0}", top)),
            ],
            w - 1,
        ),
    ];
    // The bucket count stays tied to the sample count - twenty-eight turns
    // spread over fifty columns is a comb, not a distribution - but each
    // bucket is then drawn as wide as the pane allows, so the chart fills its
    // line instead of stopping a third of the way in.
    let hi = if top > 0.0 { top } else { 1.0 };
    let count = rates.len().min(w.saturating_sub(6)).max(10);
    let mut buckets = vec![0.0f64; count];
    for r in rates {
        let at = ((r / hi * (count - 1) as f64) as usize).min(count - 1);
        buckets[at] += 1.0;
    }
    let mut cols: Vec<(f64, String)> = Vec::new();
    for (b, wide) in buckets
        .iter()
        .zip(tc::spread(count, w.saturating_sub(3).max(10)))
    {
        cols.extend(std::iter::repeat_n((*b, p.agent.clone()), wide));
    }
    for line in tc::vbars(&cols, 3, 0.0) {
        let mut parts: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
        for (colour, ch) in &line {
            parts.push((colour.as_str(), ch.clone()));
        }
        rows.push(tc::seg(&parts, w - 1));
    }
    rows.push(tc::seg(
        &[
            (tc::RST, " ".into()),
            (p.grid.as_str(), "─".repeat(cols.len())),
        ],
        w - 1,
    ));
    let right = format!("{:.0} tok/s", top);
    rows.push(tc::seg(
        &[
            (p.dim.as_str(), " 0 tok/s".into()),
            (
                p.dim.as_str(),
                " ".repeat(cols.len().saturating_sub(8 + right.len()).max(1)),
            ),
            (p.dim.as_str(), right),
        ],
        w - 1,
    ));
    rows
}

/// Tokens per day, as a calendar.
///
/// Summed from the de-duplicated per-turn deltas, the same figures the
/// totals and the cost come from. A per-file sum would count a resumed
/// session's replayed turns again and put a peak on the calendar that never
/// happened.
fn codex_calendar(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let mut totals: HashMap<NaiveDate, f64> = HashMap::new();
    for (day, models) in &d.daily {
        let Ok(at) = NaiveDate::parse_from_str(day, "%Y-%m-%d") else {
            continue;
        };
        *totals.entry(at).or_insert(0.0) += models.values().map(total_tokens).sum::<f64>();
    }
    let peak = totals.values().cloned().fold(0.0f64, f64::max);
    let Some(cal) = day_calendar(&totals, w, CODEX_STEPS, None, p) else {
        return Vec::new();
    };
    let mut rows = vec![tc::seg(
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
        ],
        w - 1,
    )];
    for line in &cal.rows {
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&refs, w - 1));
    }
    let mut legend: Vec<(&str, String)> = vec![(p.dim.as_str(), "  Less ".into())];
    let swatches: Vec<String> = CODEX_STEPS
        .iter()
        .map(|(r, g, b)| tc::rgb(*r, *g, *b))
        .collect();
    for colour in &swatches {
        legend.push((colour.as_str(), "█".into()));
    }
    legend.push((p.dim.as_str(), " More".into()));
    rows.push(tc::seg(&legend, w - 1));
    rows
}

fn codex_metered(d: &Data, w: usize, cfg: &Config, p: &Palette) -> Vec<String> {
    metered_rows(
        &[
            (
                "today".to_string(),
                crate::claude::window_models(&d.daily, 1),
            ),
            (
                "30 days".to_string(),
                crate::claude::window_models(&d.daily, 30),
            ),
        ],
        w,
        "",
        "codex",
        "this machine",
        "CLI rollouts only. Codex bills Cloud, Web, Desktop and the rest to \
         the same account, and none of those leave anything on this disk to \
         count.",
        cfg,
        p,
    )
}

/// Unused reset credits still in the account.
///
/// Drawn under the quota windows. The section is one row pair per credit the
/// inventory listed: that credit's `title`, then `Expires` and the local
/// datetime. A credit with no title gets no title line. A credit with no
/// readable expiry still has its row, and the date says it is unknown.
/// Rows inside a section sit together, the same way the quota lanes do, so
/// there is no blank line between credits.
fn codex_bank_rows(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let Some(bank) = d.bank.as_ref() else {
        return Vec::new();
    };
    let room = w.saturating_sub(1);
    let mut rows = vec![tc::seg(
        &[(p.lbl.as_str(), " ── BANK RESET ── ".into())],
        room,
    )];
    if bank.credits.is_empty() {
        let line = if bank.left == 0 {
            "None in the account.".to_string()
        } else {
            format!("{} reset available", bank.left)
        };
        push_visible(&mut rows, &line, room, p.dim.as_str());
        return rows;
    }
    for credit in &bank.credits {
        if let Some(title) = &credit.title {
            push_visible(&mut rows, title, room, p.txt.as_str());
        }
        let when = match credit.expiry.and_then(local_expiry) {
            Some(stamp) => format!("Expires {stamp}"),
            None => "Expires date unknown".to_string(),
        };
        push_visible(&mut rows, &when, room, p.dim.as_str());
    }
    rows
}

/// `expires_at` as a local datetime, with the zone named.
///
/// The instant is the one the inventory sent. The calendar fields are this
/// machine's zone. Minutes are the resolution the row shows. The offset is
/// in parentheses, and a zone abbreviation sits in front of it when this
/// machine has one that is not the offset itself.
pub(crate) fn local_expiry(at: f64) -> Option<String> {
    if !at.is_finite() {
        return None;
    }
    let local = Local.timestamp_opt(at.trunc() as i64, 0).single()?;
    let stamp = format!(
        "{} {} {} {:02}:{:02}",
        local.day(),
        MONTHS[local.month0() as usize],
        local.year(),
        local.hour(),
        local.minute(),
    );
    let zone = local.format("%Z").to_string();
    let offset = local.format("%:z").to_string();
    let zone_is_offset = zone.is_empty()
        || zone == offset
        || zone
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | ':'));
    if zone_is_offset {
        Some(format!("{stamp} ({offset})"))
    } else {
        Some(format!("{stamp} {zone} ({offset})"))
    }
}

/// Draw `text` indented, wrapping on spaces.
///
/// A word wider than the pane is broken by display width, so it stays on
/// screen. Nothing is dropped, and no omission marker is added. A datetime
/// is passed as its own parts (`4`, `Oct`, `2026`, `04:03`, `(+00:00)`),
/// which stay whole unless one part is still wider than the pane. That
/// part is broken by hand. A clock is not shortened into a different time.
pub(crate) fn push_visible(rows: &mut Vec<String>, text: &str, room: usize, colour: &str) {
    if room == 0 || text.is_empty() {
        return;
    }
    let indent = if room > 2 { 2 } else { 0 };
    let budget = room - indent;
    for piece in tc::wrap_words(text, budget) {
        if piece.is_empty() {
            continue;
        }
        let drawn = format!("{}{piece}", " ".repeat(indent));
        rows.push(tc::seg(&[(colour, drawn)], room));
    }
}

/// The summary sentence, wrapped so the count and `available` both stay,
/// and so a parenthetical moves to the following lines when it does not
/// fit beside them.
///
/// One line when the whole sentence fits. Otherwise the count phrase is
/// drawn first, and the parenthetical follows, broken between the
/// datetime's parts. A part wider than the pane is broken by hand.
pub(crate) fn append_reset_summary(rows: &mut Vec<String>, line: &str, room: usize, colour: &str) {
    let whole = format!("  {line}");
    if room > 0 && tc::display_width(&whole) <= room {
        rows.push(tc::seg(&[(colour, whole)], room));
        return;
    }
    let (head, stamp) = split_summary(line);
    push_visible(rows, &head, room, colour);
    if let Some(stamp) = stamp {
        push_visible(rows, &format!("({stamp})"), room, colour);
    }
}

/// `{n} reset available` and, when present, the stamp inside the one pair
/// of parentheses this sentence adds. The stamp's own offset parentheses
/// stay inside it.
fn split_summary(line: &str) -> (String, Option<String>) {
    const MARK: &str = " reset available";
    let Some(at) = line.find(MARK) else {
        return (line.to_string(), None);
    };
    let head_end = at + MARK.len();
    let rest = line[head_end..].trim();
    let head = line[..head_end].to_string();
    match rest
        .strip_prefix('(')
        .and_then(|inner| inner.strip_suffix(')'))
    {
        Some(stamp) if !stamp.is_empty() => (head, Some(stamp.to_string())),
        _ => (head, None),
    }
}

/// The soonest local expiry, or nothing when any listed credit has none.
///
/// An empty list is a count with no dates. The first dated credit is not
/// called the soonest while a later credit's date is unknown.
fn soonest_when_every_credit_is_dated(bank: &crate::parse::ResetBank) -> Option<String> {
    if bank.credits.is_empty() {
        return None;
    }
    let mut soonest = None;
    for credit in &bank.credits {
        let stamp = credit.expiry.and_then(local_expiry)?;
        if soonest.is_none() {
            soonest = Some(stamp);
        }
    }
    soonest
}

/// Plan type and a credit balance - all Codex publishes about the plan.
///
/// Three lines rather than the section Copilot and Cursor get, because three
/// lines is genuinely all there is. Credits belong here rather than under the
/// quota bars: they are what the plan grants, not a window.
fn codex_plan_rows(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let null = serde_json::Value::Null;
    let live = d.live.as_ref().unwrap_or(&null);
    let limits = d.limits.as_ref().unwrap_or(&null);
    let plan = match text(live, "plan_type") {
        s if !s.is_empty() => s,
        _ => text(limits, "plan_type"),
    };
    let credits = match live["credits"].is_object() {
        true => &live["credits"],
        false => &limits["credits"],
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    if credits.is_object() {
        pairs.push((
            "credits".into(),
            match credits["unlimited"].as_bool().unwrap_or(false) {
                true => "unlimited".into(),
                false => scalar(&credits["balance"]),
            },
        ));
    }
    let limit = &live["spend_control"]["individual_limit"];
    if !limit.is_null() {
        pairs.push(("spend limit".into(), scalar(limit)));
    }
    // Nothing published is not a plan called "unknown", which is what the
    // shared block would otherwise print.
    if plan.is_empty() && pairs.is_empty() {
        return Vec::new();
    }
    plan_rows(&plan, &pairs, w, "", None, "", p)
}

/// Why Codex publishes no bar on the summary, when it does not.
///
/// The tab still has the rollouts: those are tokens spent on this machine.
/// `[+]` only ranks a live (or last-session) used_percent, so a busy tab
/// and an empty summary row can both be true.
pub fn why_no_lane(d: &Data) -> String {
    if !lanes(d).is_empty() {
        return String::new();
    }
    if !d.live_why.is_empty() {
        return format!("no quota · {}", d.live_why);
    }
    match (d.live.is_some(), d.limits.is_some()) {
        (false, false) => {
            "no quota · no live account window, and the last session left no used_percent on disk."
                .into()
        }
        (false, true) => {
            "no quota · no live account window, and the last session left no used_percent.".into()
        }
        (true, _) => {
            "no quota · Codex answered, and published no used_percent for this period.".into()
        }
    }
}

/// Every quota Codex publishes, for the summary screen.
///
/// The live account-wide windows only. The snapshot the tab falls back to is
/// a reading from whenever Codex last ran, and this screen ranks agents
/// against each other - a day-old percentage sorted beside live ones would
/// put the wrong agent at the top.
pub fn lanes(d: &Data) -> Vec<Lane> {
    let Some(live) = d.live.as_ref() else {
        // The tab falls back to the rollout snapshot here and says "from
        // the last session"; the summary used to show nothing at all, so
        // Codex silently left a screen that names every other agent. The
        // snapshot keeps the shape it has on disk - window_minutes, not
        // seconds - and is marked stale, which is what that flag is for.
        let win = d.limits.as_ref().map(|l| &l["primary"]);
        let Some(win) = win.filter(|x| !x["used_percent"].is_null()) else {
            return Vec::new();
        };
        let minutes = num(win, "window_minutes");
        return vec![Lane {
            label: window_name(Some(minutes * 60.0)),
            pct: num(win, "used_percent"),
            window_secs: (minutes > 0.0).then_some(minutes * 60.0),
            reset: win["resets_at"].as_f64(),
            stale: true,
            projected: false,
            apart: false,
        }];
    };
    let mut out: Vec<Lane> = Vec::new();
    for key in ["primary_window", "secondary_window"] {
        let win = &live["rate_limit"][key];
        if win["used_percent"].is_null() {
            continue;
        }
        let secs = win["limit_window_seconds"].as_f64();
        out.push(Lane {
            label: window_name(secs),
            pct: num(win, "used_percent"),
            window_secs: secs,
            reset: win["reset_at"].as_f64(),
            stale: false,
            projected: false,
            apart: false,
        });
    }
    for extra in live["additional_rate_limits"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let win = &extra["rate_limit"]["primary_window"];
        if win["used_percent"].is_null() {
            continue;
        }
        let name = match text(extra, "limit_name") {
            s if s.is_empty() => "?".into(),
            s => s,
        };
        let secs = win["limit_window_seconds"].as_f64();
        out.push(Lane {
            label: format!(
                "{} {}",
                name.rsplit('-').next().unwrap_or("?"),
                window_name(secs)
            ),
            pct: num(win, "used_percent"),
            window_secs: secs,
            reset: win["reset_at"].as_f64(),
            stale: false,
            projected: false,
            apart: false,
        });
    }
    out
}

/// The whole tab: the quota, what this machine recorded, what it cost, and
/// which subscription the percentages are percentages of.
pub fn tab(d: &Data, w: usize, _h: usize, cfg: &Config, p: &Palette) -> Vec<String> {
    let mut rows = codex_quota(d, w, p);
    if rows.is_empty() {
        let note = why_no_lane(d);
        if !note.is_empty() {
            rows.extend(no_local(&note, "", w, p));
            rows.push(String::new());
        }
    }
    rows = add_section(rows, codex_bank_rows(d, w, p));
    if !d.ok {
        // The quota above is the account's and is true whatever this machine
        // has on disk, so it stays; only the local half is missing.
        rows.extend(no_local(
            "No session rollouts on this machine.",
            run_hint("codex"),
            w,
            p,
        ));
        return add_section(rows, codex_plan_rows(d, w, p));
    }
    rows.extend(codex_totals_rows(d, w, p));
    rows = add_section(rows, codex_rate_rows(d, w, p));
    rows = add_section(rows, codex_calendar(d, w, p));
    rows.push(String::new());
    for line in wrap_text(
        "Tokens and rate are measured here, from the rollouts. Quota is the \
         account's, fetched from the same endpoint the Codex CLI uses.",
        w.saturating_sub(4).max(20),
    ) {
        rows.push(tc::seg(&[(p.dim.as_str(), format!("  {}", line))], w - 1));
    }
    let body = add_section(rows, codex_metered(d, w, cfg, p));
    add_section(body, codex_plan_rows(d, w, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two `token_count` events with the model named once before them.
    fn one_session(id: &str) -> String {
        [
            format!(
                r#"{{"type":"session_meta","payload":{{"session_id":"{}"}}}}"#,
                id
            ),
            r#"{"type":"turn_context","payload":{"model":"gpt-5.3-codex"}}"#.to_string(),
            r#"{"type":"event_msg","timestamp":"2026-08-16T10:00:00.000Z","payload":
               {"type":"token_count","info":{"last_token_usage":
               {"input_tokens":1000,"cached_input_tokens":800,"output_tokens":300,
                "reasoning_output_tokens":120}}}}"#
                .replace('\n', ""),
            r#"{"type":"event_msg","timestamp":"2026-08-16T10:00:20.000Z","payload":
               {"type":"token_count","info":{"last_token_usage":
               {"input_tokens":2000,"cached_input_tokens":1500,"output_tokens":500,
                "reasoning_output_tokens":200}}}}"#
                .replace('\n', ""),
        ]
        .join("\n")
    }

    #[test]
    fn cached_input_is_split_out_of_the_input_it_arrived_inside() {
        let got = rollout_records(&one_session("s-1"), "fallback.jsonl");
        assert_eq!(got.len(), 2);
        let (_, model, tokens) = got
            .values()
            .find(|(_, _, t)| t["output"] == 300.0)
            .expect("the first turn");
        assert_eq!(model, "gpt-5.3-codex");
        // 1000 input of which 800 were cached: only 200 are charged at the
        // input rate, and the cached 800 at the far cheaper one.
        assert_eq!(tokens["input"], 200.0);
        assert_eq!(tokens["cache_read"], 800.0);
        // Reasoning is carried, and is already inside output.
        assert_eq!(tokens["reasoning"], 120.0);
    }

    #[test]
    fn a_turn_takes_the_model_from_the_context_before_it() {
        // The model is not on the token counts: it arrives in turn_context
        // and applies to everything that follows, until the next one.
        let body = [
            r#"{"type":"turn_context","payload":{"model":"gpt-5.3-codex"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-08-16T10:00:00Z","payload":{"type":"token_count","info":{"last_token_usage":{"output_tokens":10}}}}"#,
            r#"{"type":"turn_context","payload":{"model":"codex-auto-review"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-08-16T10:01:00Z","payload":{"type":"token_count","info":{"last_token_usage":{"output_tokens":20}}}}"#,
        ]
        .join("\n");
        let got = rollout_records(&body, "fallback.jsonl");
        let mut models: Vec<&str> = got.values().map(|(_, m, _)| m.as_str()).collect();
        models.sort();
        assert_eq!(models, vec!["codex-auto-review", "gpt-5.3-codex"]);
    }

    #[test]
    fn a_turn_before_any_model_is_named_is_not_guessed_at() {
        // Without a turn_context there is no model to attribute the tokens
        // to, and attributing them to the wrong one would be worse than
        // leaving them out.
        let body = r#"{"type":"event_msg","timestamp":"2026-08-16T10:00:00Z","payload":{"type":"token_count","info":{"last_token_usage":{"output_tokens":10}}}}"#;
        assert!(rollout_records(body, "fallback.jsonl").is_empty());
    }

    #[test]
    fn a_replayed_session_is_counted_once() {
        // Resuming writes a new rollout that replays the earlier turns, with
        // the same session id and the same stamps. Summed per file that would
        // count them twice - the fault that inflated Claude's figures.
        let mut seen: HashMap<String, (String, String, Tokens)> = HashMap::new();
        seen.extend(rollout_records(&one_session("s-1"), "first.jsonl"));
        seen.extend(rollout_records(&one_session("s-1"), "second.jsonl"));
        assert_eq!(seen.len(), 2);
        let (daily, sessions) = merge_days(seen);
        assert_eq!(sessions, 1);
        let totals = codex_totals(&daily);
        // 1000 + 2000 input, cached included; 300 + 500 out; 120 + 200 of
        // that reasoning.
        assert_eq!(totals.input, 3000.0);
        assert_eq!(totals.output, 800.0);
        assert_eq!(totals.cached, 2300.0);
        assert_eq!(totals.reasoning, 320.0);
        // Reasoning sits inside output and cached inside input, so neither is
        // added again: 3000 + 800.
        assert_eq!(totals.all, 3800.0);
    }

    #[test]
    fn two_sessions_are_two_sessions_however_many_files_they_took() {
        let mut seen: HashMap<String, (String, String, Tokens)> = HashMap::new();
        for (id, file) in [("s-1", "a"), ("s-1", "b"), ("s-2", "c")] {
            seen.extend(rollout_records(&one_session(id), file));
        }
        let (_, sessions) = merge_days(seen);
        assert_eq!(sessions, 2);
    }

    #[test]
    fn a_window_is_named_by_how_long_it_is() {
        assert_eq!(window_name(Some(7.0 * 86400.0)), "7d");
        assert_eq!(window_name(Some(5.0 * 3600.0)), "5h");
        // No length published is a question mark, not a zero.
        assert_eq!(window_name(None), "?");
        assert_eq!(window_name(Some(0.0)), "?");
    }

    #[test]
    fn a_named_feature_lane_keeps_only_its_last_segment() {
        // GPT-5.3-Codex-Spark is Spark: the family name is already implied by
        // the tab it is sitting on.
        let d = Data {
            live: Some(
                serde_json::from_str(
                    r#"{"plan_type":"pro",
                        "rate_limit":{
                          "primary_window":{"used_percent":26.0,"limit_window_seconds":604800,"reset_at":1000},
                          "secondary_window":{"used_percent":4.5,"limit_window_seconds":18000,"reset_at":2000}},
                        "additional_rate_limits":[
                          {"limit_name":"GPT-5.3-Codex-Spark",
                           "rate_limit":{"primary_window":{"used_percent":12.0,"limit_window_seconds":86400,"reset_at":3000}}}]}"#,
                )
                .expect("a live reading"),
            ),
            ..Data::default()
        };
        let got = lanes(&d);
        let labels: Vec<&str> = got.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels, vec!["7d", "5h", "Spark 1d"]);
        assert_eq!(got[0].pct, 26.0);
        assert_eq!(got[2].window_secs, Some(86400.0));
        assert_eq!(got[2].reset, Some(3000.0));
        // Live is live: nothing here is a cached reading.
        assert!(got.iter().all(|l| !l.stale));
    }

    #[test]
    fn a_window_with_no_percentage_is_not_a_lane() {
        // The percentage is the only number on the row that cannot be worked
        // out from the others, so a window without one has nothing to say.
        let d = Data {
            live: Some(
                serde_json::from_str(
                    r#"{"rate_limit":{
                          "primary_window":{"limit_window_seconds":604800},
                          "secondary_window":{"used_percent":4.5,"limit_window_seconds":18000}},
                        "additional_rate_limits":[
                          {"limit_name":"Spark","rate_limit":{"primary_window":{}}}]}"#,
                )
                .expect("a live reading"),
            ),
            ..Data::default()
        };
        let got = lanes(&d);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].label, "5h");
    }

    #[test]
    fn an_agent_that_answered_nothing_publishes_no_lanes() {
        assert!(lanes(&Data::default()).is_empty());
    }

    #[test]
    fn the_snapshot_stands_in_only_when_the_live_call_answered_nothing() {
        let p = palette();
        let d = Data {
            limits: Some(
                serde_json::from_str(r#"{"primary":{"used_percent":71.0,"window_minutes":10080}}"#)
                    .expect("a snapshot"),
            ),
            ..Data::default()
        };
        let rows = codex_quota(&d, 90, &p).join(" ");
        assert!(rows.contains("from the last session"), "{}", rows);
        assert!(rows.contains("71%"), "{}", rows);
        // And it reaches the summary too, marked stale. The alternative was
        // Codex disappearing from a screen that names every other agent
        // while its own tab showed a quota - and the summary draws a stale
        // lane as "cached" rather than as a reset time, so it is not passed
        // off as a live figure beside live ones.
        let got = lanes(&d);
        assert_eq!(got.len(), 1, "{:?}", got);
        assert!(got[0].stale);
        assert_eq!(got[0].pct, 71.0);
        assert_eq!(got[0].window_secs, Some(10080.0 * 60.0));
        assert_eq!(got[0].label, "7d");
    }

    #[test]
    fn a_snapshot_with_no_window_length_still_reaches_the_summary() {
        // window_minutes absent reads as 0, which is not a window. The lane
        // must still appear - the percentage is the part that matters - but
        // without a length nothing can pace it, so window_secs stays None
        // and the summary draws no pace mark rather than an invented one.
        let d = Data {
            limits: Some(
                serde_json::from_str(r#"{"primary":{"used_percent":40.0}}"#).expect("a snapshot"),
            ),
            ..Data::default()
        };
        let got = lanes(&d);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].window_secs, None);
        assert!(got[0].stale);
    }

    #[test]
    fn a_snapshot_without_a_percentage_draws_nothing_rather_than_crashing() {
        let p = palette();
        let d = Data {
            limits: Some(
                serde_json::from_str(r#"{"primary":{"window_minutes":10080}}"#)
                    .expect("a snapshot"),
            ),
            ..Data::default()
        };
        assert!(codex_quota(&d, 90, &p).is_empty());
    }

    #[test]
    fn a_plan_nobody_published_is_left_out_rather_than_called_unknown() {
        let p = palette();
        assert!(codex_plan_rows(&Data::default(), 90, &p).is_empty());
        let d = Data {
            live: Some(
                serde_json::from_str(r#"{"plan_type":"pro","credits":{"balance":12.5}}"#)
                    .expect("a live reading"),
            ),
            ..Data::default()
        };
        let rows = codex_plan_rows(&d, 90, &p).join(" ");
        assert!(rows.contains("pro"), "{}", rows);
        // A balance is a number, and quotes around it are not part of one.
        assert!(rows.contains("12.5"), "{}", rows);
    }

    #[test]
    fn every_section_draws_at_every_pane_width() {
        // The tab adapts rather than truncates - two columns of totals
        // become one, a spelled-out feature name becomes its last segment,
        // and the histogram fills whatever line it is given. Each of those
        // is arithmetic on a width, and each is a place a narrow pane has
        // put a widget on the floor before.
        let p = palette();
        let cfg = Config::default();
        let seen = rollout_records(&one_session("s-1"), "a.jsonl");
        let (daily, sessions) = merge_days(seen);
        let d = Data {
            ok: true,
            live: Some(
                serde_json::from_str(
                    r#"{"plan_type":"pro",
                        "credits":{"balance":40},
                        "rate_limit":{
                          "primary_window":{"used_percent":26.0,"limit_window_seconds":604800},
                          "secondary_window":{"used_percent":4.5,"limit_window_seconds":18000}},
                        "additional_rate_limits":[
                          {"limit_name":"GPT-5.3-Codex-Spark",
                           "rate_limit":{"primary_window":{"used_percent":12.0,"limit_window_seconds":18000}}}]}"#,
                )
                .expect("a live reading"),
            ),
            sessions,
            files: 2,
            last: now() - 60.0,
            total: codex_totals(&daily),
            rates: vec![12.0, 40.0, 55.0, 61.0],
            daily,
            ..Data::default()
        };
        for w in [20usize, 40, 80, 200] {
            let rows = tab(&d, w, 40, &cfg, &p);
            let plain = rows.join("\n");
            for want in [
                "QUOTA",
                "TOTALS",
                "OUTPUT RATE",
                "TOKENS / DAY",
                "SUBSCRIPTION",
            ] {
                assert!(plain.contains(want), "{} missing at width {}", want, w);
            }
        }
        // Wide, the feature is spelled out; narrow, its last segment carries
        // it, because the alternative is a bar with nowhere to be drawn.
        let wide = codex_quota(&d, 200, &p).join("\n");
        assert!(wide.contains("GPT-5.3-Codex-Spark"), "{}", wide);
        let narrow = codex_quota(&d, 60, &p).join("\n");
        assert!(!narrow.contains("GPT-5.3-Codex-Spark"), "{}", narrow);
        assert!(narrow.contains("Spark"), "{}", narrow);
        // With a named lane beside them, the account's own windows say what
        // they cover rather than leaving "7d" to stand alone.
        assert!(narrow.contains("overall"), "{}", narrow);
    }

    #[test]
    fn a_machine_with_no_rollouts_still_shows_the_account_quota() {
        // The quota is the account's and is true whatever is on this disk, so
        // an empty sessions directory hides the local half and nothing else.
        let p = palette();
        let cfg = Config::default();
        let d = Data {
            live: Some(
                serde_json::from_str(
                    r#"{"plan_type":"pro","rate_limit":{"primary_window":
                        {"used_percent":26.0,"limit_window_seconds":604800}}}"#,
                )
                .expect("a live reading"),
            ),
            ..Data::default()
        };
        let rows = tab(&d, 90, 40, &cfg, &p).join(" ");
        assert!(rows.contains("QUOTA"), "{}", rows);
        assert!(rows.contains("No session rollouts"), "{}", rows);
        assert!(!rows.contains("TOTALS"), "{}", rows);
    }

    #[test]
    fn a_missing_quota_says_which_step_failed() {
        let empty = why_no_lane(&Data::default());
        assert!(empty.contains("no live account window"), "{empty}");
        assert!(empty.contains("used_percent"), "{empty}");
        assert!(lanes(&Data::default()).is_empty());

        let snapshot_only = Data {
            limits: Some(serde_json::json!({"primary": {}})),
            ..Data::default()
        };
        let note = why_no_lane(&snapshot_only);
        assert!(note.contains("no live account window"), "{note}");
        assert!(!note.contains("did not answer"), "{note}");

        let missing_token = Data {
            limits: Some(serde_json::json!({"primary": {}})),
            live_why: "no token - Codex has not signed in here".into(),
            ..Data::default()
        };
        let note = why_no_lane(&missing_token);
        assert!(note.contains("not signed in"), "{note}");
        assert!(!note.contains("did not answer"), "{note}");

        let refused = Data {
            limits: Some(serde_json::json!({"primary": {}})),
            live_why: "ChatGPT's usage endpoint did not answer".into(),
            ..Data::default()
        };
        let note = why_no_lane(&refused);
        assert!(note.contains("did not answer"), "{note}");
    }

    fn shown(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|s| {
                let mut out = String::new();
                let mut chars = s.chars();
                while let Some(c) = chars.next() {
                    if c == '\u{1b}' {
                        for c in chars.by_ref() {
                            if c.is_ascii_alphabetic() {
                                break;
                            }
                        }
                    } else {
                        out.push(c);
                    }
                }
                out.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn the_bank_lists_each_credit_with_its_local_expiry() {
        let p = palette();
        let cfg = Config::default();
        let soon = iso_epoch("2026-07-12T04:03:43Z").expect("soon");
        let later = iso_epoch("2026-07-18T00:39:53Z").expect("later");
        let soon_row = local_expiry(soon).expect("a local soon expiry");
        let later_row = local_expiry(later).expect("a local later expiry");
        // The offset is the zone label. A name, when the zone has one, sits
        // beside it. Neither is invented: both come from this machine's zone
        // applied to the instant the inventory sent.
        let soon_local = Local
            .timestamp_opt(soon as i64, 0)
            .single()
            .expect("soon local");
        let offset = soon_local.format("%:z").to_string();
        assert!(
            soon_row.contains(&offset),
            "the zone is not labeled: {soon_row}"
        );
        assert!(
            soon_row.contains(&format!(
                "{} {} {} {:02}:{:02}",
                soon_local.day(),
                MONTHS[soon_local.month0() as usize],
                soon_local.year(),
                soon_local.hour(),
                soon_local.minute(),
            )),
            "the datetime is not this machine's local time: {soon_row}"
        );
        assert!(
            !soon_row.contains(&format!(
                "{:02}:{:02}:{:02}",
                soon_local.hour(),
                soon_local.minute(),
                soon_local.second(),
            )),
            "seconds are not part of this row: {soon_row}"
        );
        let d = Data {
            ok: true,
            live: Some(
                serde_json::from_str(
                    r#"{"plan_type":"pro","rate_limit":{"primary_window":
                        {"used_percent":26.0,"limit_window_seconds":604800,"reset_at":1000}}}"#,
                )
                .expect("a live reading"),
            ),
            bank: Some(crate::parse::ResetBank {
                left: 3,
                credits: vec![
                    crate::parse::ResetCredit {
                        title: Some("Full reset".into()),
                        expiry: Some(soon),
                    },
                    crate::parse::ResetCredit {
                        title: Some("Full reset".into()),
                        expiry: Some(later),
                    },
                    crate::parse::ResetCredit {
                        title: None,
                        expiry: None,
                    },
                ],
            }),
            ..Data::default()
        };
        let wide = shown(&codex_bank_rows(&d, 200, &p));
        assert_eq!(wide, vec![
            " ── BANK RESET ──".to_string(),
            "  Full reset".to_string(),
            format!("  Expires {soon_row}"),
            "  Full reset".to_string(),
            format!("  Expires {later_row}"),
            "  Expires date unknown".to_string(),
        ]);
        let on_tab = tab(&d, 200, 40, &cfg, &p).join("\n");
        let soon_at = on_tab.find(&soon_row).expect("the soon expiry");
        let later_at = on_tab.find(&later_row).expect("the later expiry");
        let unknown_at = on_tab.find("date unknown").expect("the unknown date");
        assert!(soon_at < later_at && later_at < unknown_at);
        assert!(!on_tab.contains("⏱"), "{on_tab}");
        assert!(!on_tab.contains("does not expire"), "{on_tab}");
        assert!(!on_tab.contains("10d"), "{on_tab}");

        // A clock that fits stays whole. `04:0` is not `04:03`. A part
        // wider than the pane is broken by hand further down.
        let narrow = shown(&codex_bank_rows(&d, 8, &p)).join("\n");
        assert!(
            !narrow.contains("04:0") || narrow.contains("04:03"),
            "a clock was shortened: {narrow}"
        );
        assert!(
            !narrow.contains("00:3") || narrow.contains("00:39"),
            "a clock was shortened: {narrow}"
        );
        assert!(
            !narrow.contains("04:03:"),
            "seconds are not on this row: {narrow}"
        );

        let count_only = Data {
            bank: Some(crate::parse::ResetBank {
                left: 2,
                credits: Vec::new(),
            }),
            ..d.clone()
        };
        let count_only = shown(&codex_bank_rows(&count_only, 200, &p));
        assert_eq!(count_only, vec![
            " ── BANK RESET ──".to_string(),
            "  2 reset available".to_string(),
        ]);

        let unreadable = Data {
            bank: Some(crate::parse::ResetBank {
                left: 1,
                credits: vec![crate::parse::ResetCredit {
                    title: None,
                    expiry: None,
                }],
            }),
            ..d.clone()
        };
        let unreadable = shown(&codex_bank_rows(&unreadable, 200, &p)).join("\n");
        assert!(unreadable.contains("Expires date unknown"), "{unreadable}");
        assert!(
            !unreadable.contains("Full reset"),
            "a missing title was invented: {unreadable}"
        );
        assert!(
            !unreadable.contains(&soon_row),
            "an unreadable expiry was given a date: {unreadable}"
        );

        let bare = Data {
            ok: false,
            bank: Some(crate::parse::ResetBank {
                left: 0,
                credits: Vec::new(),
            }),
            ..d.clone()
        };
        let rows = tab(&bare, 80, 40, &cfg, &p).join("\n");
        assert!(rows.contains("BANK RESET"), "{rows}");
        assert!(rows.contains("None in the account"), "{rows}");
        assert!(
            !rows.contains("reset available"),
            "a real zero is not described as available: {rows}"
        );
        assert!(
            !rows.contains("Still available"),
            "a real zero is not described as still available: {rows}"
        );
        assert!(rows.contains("No session rollouts"), "{rows}");
        assert!(!rows.contains("date unknown"), "{rows}");
        assert!(!rows.contains("expires in"), "{rows}");
    }

    #[test]
    fn an_undated_credit_is_not_called_the_soonest() {
        let soon = iso_epoch("2026-10-04T11:03:00Z").expect("soon");
        let stamp = local_expiry(soon).expect("a local expiry");
        let mixed = Data {
            bank: Some(crate::parse::ResetBank {
                left: 2,
                credits: vec![
                    crate::parse::ResetCredit {
                        title: Some("Full reset".into()),
                        expiry: Some(soon),
                    },
                    crate::parse::ResetCredit {
                        title: None,
                        expiry: None,
                    },
                ],
            }),
            ..Data::default()
        };
        assert_eq!(
            mixed.reset_summary_line().as_deref(),
            Some("2 reset available"),
            "a dated credit was called the soonest"
        );
        let dated = Data::with_reset(None, Some((2, Some(soon))));
        assert_eq!(
            dated.reset_summary_line().as_deref(),
            Some(format!("2 reset available ({stamp})")).as_deref()
        );
    }

    #[test]
    fn a_narrow_bank_keeps_the_title_and_the_clock() {
        let p = palette();
        let soon = iso_epoch("2026-07-12T04:03:43Z").expect("soon");
        let stamp = local_expiry(soon).expect("a local expiry");
        let clock = stamp
            .split_whitespace()
            .find(|word| word.contains(':'))
            .expect("the clock");
        let offset = stamp
            .split_whitespace()
            .find(|word| word.starts_with('('))
            .expect("the offset");
        let d = Data {
            bank: Some(crate::parse::ResetBank {
                left: 1,
                credits: vec![crate::parse::ResetCredit {
                    title: Some("ABCDEFGHIJ".into()),
                    expiry: Some(soon),
                }],
            }),
            ..Data::default()
        };
        let rows = shown(&codex_bank_rows(&d, 8, &p));
        let flat: String = rows.iter().map(|row| row.trim()).collect();
        assert!(
            flat.contains("ABCDEFGHIJ"),
            "a wide title was dropped: {rows:#?}"
        );
        assert!(
            !flat.contains('…'),
            "an omission marker was added: {rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.trim() == clock),
            "the clock was not kept whole: {rows:#?}"
        );
        assert!(
            flat.contains(offset),
            "the zone was skipped: {rows:#?} offset {offset}"
        );

        let count_only = Data {
            bank: Some(crate::parse::ResetBank {
                left: 2,
                credits: Vec::new(),
            }),
            ..Data::default()
        };
        let count_only = shown(&codex_bank_rows(&count_only, 8, &p));
        let flat: String = count_only.iter().map(|row| row.trim()).collect();
        assert!(
            flat.contains("2resetavailable"),
            "the count lost its label: {count_only:#?}"
        );
    }

    #[test]
    fn a_bank_that_was_not_read_is_left_off_the_pane() {
        let p = palette();
        let d = Data {
            live: Some(
                serde_json::from_str(
                    r#"{"plan_type":"pro","rate_limit":{"primary_window":
                        {"used_percent":26.0,"limit_window_seconds":604800}}}"#,
                )
                .expect("a live reading"),
            ),
            ..Data::default()
        };
        let rows = codex_bank_rows(&d, 80, &p);
        assert!(rows.is_empty());
        let plain = tab(&d, 80, 40, &Config::default(), &p).join("\n");
        assert!(!plain.contains("BANK"), "{plain}");
    }

    #[test]
    fn the_inventory_is_the_only_source_of_the_count() {
        let now = 1_780_000_000.0;
        let inventory = r#"{"available_count":1,"credits":[
            {"status":"available","expires_at":"2026-08-01T00:00:00Z"}
        ]}"#;
        let from_list = bank_of(Some(inventory), now).expect("the list");
        assert_eq!(from_list.left, 1);
        assert_eq!(from_list.credits.len(), 1);

        // A usage payload can carry `rate_limit_reset_credits.available_count`.
        // That number is not the inventory, so a failed inventory stays blank
        // rather than becoming that count.
        assert!(bank_of(Some("not json"), now).is_none());
        assert!(bank_of(None, now).is_none());

        let empty = r#"{"credits":[],"available_count":0}"#;
        let zero = bank_of(Some(empty), now).expect("a real zero");
        assert_eq!(zero.left, 0);
        assert!(zero.credits.is_empty());
    }

    #[test]
    fn a_usage_miss_keeps_an_inventory_that_answered() {
        let inventory = r#"{"credits":[],"available_count":2}"#;
        let missed = live_payload(None, Some(inventory.to_string())).expect("the inventory");
        assert!(missed.get("u").is_none(), "usage did not answer");
        assert!(missed.get("usage_miss").is_some());
        let bank = bank_of(missed["bank"].as_str(), 0.0).expect("the count");
        assert_eq!(bank.left, 2);
        assert!(bank.credits.is_empty());

        let usage = serde_json::json!({"plan_type": "pro"});
        let both = live_payload(Some(usage), Some(inventory.to_string())).expect("both");
        assert!(both.get("u").is_some());
        assert!(both.get("usage_miss").is_none());

        assert!(live_payload(None, None).is_none(), "neither call answered");
    }
}
