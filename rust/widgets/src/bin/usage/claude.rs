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

//! Claude Code: its stats cache, its transcripts, and what is left of the
//! account's limits.
//!
//! The money comes from the transcripts rather than the cache. The cache
//! has one total per model per day, and input, output and the two cache
//! durations differ in price by up to fifty times, so a total cannot be
//! costed.

use std::collections::HashMap;

use chrono::{Datelike, Duration as Days, Local, NaiveDate, TimeZone};
use toys_core as tc;

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
    let body = tc::get(
        url,
        &[
            ("Authorization", &format!("Bearer {}", tok)),
            ("User-Agent", "terminal-toys"),
        ],
        20,
    )
    .ok()?;
    serde_json::from_str(&body).ok()
}

/// What Claude Code last fetched, for when the live call cannot run.
///
/// It is a cache with a timestamp, so it is shown with its age - and a
/// window whose reset has already gone by is said to have passed rather
/// than counted down to, because a stale five-hour window describes a
/// period that has ended.
pub fn claude_stale() -> Option<(serde_json::Value, f64)> {
    let config = read_json(&under_home(".claude.json"))?;
    let c = &config["cachedUsageUtilization"];
    let u = c["utilization"].clone();
    if u.is_null() {
        return None;
    }
    Some((u, num(c, "fetchedAtMs") / 1000.0))
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
    let live = cached(caches, "claude", LIVE_TTL, || {
        let (tok, plan) = claude_token()?;
        let u = claude_get("https://api.anthropic.com/api/oauth/usage", &tok)?;
        Some(serde_json::json!({ "u": u, "at": now(), "plan": plan }))
    });
    match live {
        Some(got) => {
            claude.quota = Some(got["u"].clone());
            claude.quota_live = true;
            claude.quota_at = num(&got, "at");
            claude.quota_plan = text(&got, "plan");
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
    } else {
        format!("cached {} ago", ago(c.quota_at))
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
    let extra = &u["extra_usage"];
    let spend = &u["spend"];
    if extra["is_enabled"].as_bool().unwrap_or(false) && !spend["limit"].is_null() {
        let money = |m: &serde_json::Value| -> String {
            format!(
                "{:.2}",
                num(m, "amount_minor") / 10f64.powf(m["exponent"].as_f64().unwrap_or(2.0))
            )
        };
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "  extra usage ".into()),
                (p.txt.as_str(), money(&spend["used"])),
                (p.dim.as_str(), " of ".into()),
                (p.txt.as_str(), money(&spend["limit"])),
                (p.dim.as_str(), format!(" {}", text(&spend["limit"], "currency"))),
                (p.dim.as_str(), " monthly".into()),
            ],
            w - 1,
        ));
    }
    rows.push(String::new());
    rows
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
    found
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
                stale: !c.quota_live,
                projected: rolled.1,
            }
        })
        .collect()
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
