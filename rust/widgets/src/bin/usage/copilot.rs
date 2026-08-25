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

//! GitHub Copilot: the account's premium-request pool, and the local
//! session store.
//!
//! The two halves are independent: the quota is the account's and arrives
//! over the network, while the per-turn detail is this machine's and is
//! frequently empty. Either can be present without the other.

use std::collections::HashMap;

use chrono::{Datelike, TimeZone, Timelike, Utc};
use rusqlite::{Connection, OpenFlags};
use toys_core as tc;

use crate::shared::*;
use crate::*;

const COPILOT_USER_API: &str = "https://api.github.com/copilot_internal/user";

fn copilot_db() -> String {
    under_home(".copilot/session-store.db")
}

fn copilot_config() -> String {
    under_home(".copilot/config.json")
}

/// The account's own description of itself. Names are what the API
/// returns, shortened only where it repeats itself.
const COPILOT_FEATURES: &[(&str, &str)] = &[
    ("chat_enabled", "chat"),
    ("cli_enabled", "cli"),
    ("is_mcp_enabled", "mcp"),
    ("cli_remote_control_enabled", "remote control"),
    ("cloud_session_storage_enabled", "cloud sessions"),
    ("copilot_app_enabled", "app"),
    ("editor_preview_features_enabled", "editor previews"),
    ("copilotignore_enabled", "copilotignore"),
];

/// The per-turn aggregates the session store keeps, summed.
#[derive(Clone, Default)]
pub struct Spent {
    input: f64,
    output: f64,
    cache: f64,
    reasoning: f64,
    nano_aiu: f64,
    ttft: f64,
    itl: f64,
    ms: f64,
}

/// Live quota, plus whatever the local session store has recorded.
#[derive(Clone, Default)]
pub struct Data {
    /// The /copilot_internal/user response, when the request worked.
    live: Option<serde_json::Value>,
    /// Why it did not, when it did not.
    live_why: String,
    sessions: i64,
    events: i64,
    /// None until an event has been read - a locked store and an empty one
    /// both leave this unset, and `why` says which it was.
    usage: Option<Spent>,
    /// day -> model -> tokens by priced kind, from the session store.
    daily: HashMap<String, HashMap<String, Tokens>>,
    /// (model, turns, output tokens), busiest first.
    models: Vec<(String, i64, f64)>,
    why: String,
}

/// The span a monthly quota covers, worked back from its reset, as its
/// label and its length in seconds.
///
/// Copilot states when the quota resets but never how long the window is.
/// It can be derived, but only safely when the reset lands on midnight UTC
/// on the first of a month - which is what a calendar-month cycle looks
/// like, and what this account shows. Anything else and the window is not
/// known, so nothing is claimed about it: no label, and no pace figure,
/// because a pace against a guessed window is a fabricated number.
///
/// Label and length come from the one function so the refusal cannot
/// diverge between them. usage.py refuses only for the label and derives
/// the pace span unconditionally, which contradicts its own comment here.
pub fn quota_window(reset_ts: Option<f64>) -> Option<(String, f64)> {
    let stamp = reset_ts?;
    if stamp <= 0.0 {
        return None;
    }
    let end = Utc.timestamp_opt(stamp as i64, 0).single()?;
    if (end.day(), end.hour(), end.minute(), end.second()) != (1, 0, 0, 0) {
        return None;
    }
    let start = if end.month() == 1 {
        Utc.with_ymd_and_hms(end.year() - 1, 12, 1, 0, 0, 0)
    } else {
        Utc.with_ymd_and_hms(end.year(), end.month() - 1, 1, 0, 0, 0)
    }
    .single()?;
    Some((
        format!(
            "{} {} → {} {}",
            start.day(),
            MONTHS[start.month0() as usize],
            end.day(),
            MONTHS[end.month0() as usize]
        ),
        stamp - start.timestamp() as f64,
    ))
}

/// The OAuth token Copilot's CLI keeps in ~/.copilot/config.json.
///
/// That file is JSON with `//` comments on top, which a JSON parser
/// refuses, so the comments come off first. Keyed by host and login,
/// because one machine can be signed in to github.com and an Enterprise
/// host at once.
pub fn copilot_token() -> Option<String> {
    token_in(&std::fs::read_to_string(copilot_config()).ok()?)
}

fn token_in(raw: &str) -> Option<String> {
    let clean: String = raw
        .lines()
        .map(|line| if line.trim_start().starts_with("//") { "" } else { line })
        .collect::<Vec<_>>()
        .join("\n");
    let parsed: serde_json::Value = serde_json::from_str(&clean).ok()?;
    parsed["copilotTokens"]
        .as_object()
        .into_iter()
        .flatten()
        .find_map(|(_, tok)| tok.as_str().filter(|t| !t.is_empty()).map(String::from))
}

/// Entitlement and quota, from the endpoint the Copilot CLI itself uses.
///
/// This is where Copilot's remaining quota actually lives. The session
/// store records what was spent per turn and is empty on plenty of
/// machines; this is the account's standing, and it answers the only
/// question a limit pane is really asked.
fn copilot_live() -> Option<serde_json::Value> {
    let Some(tok) = copilot_token() else {
        return Some(serde_json::json!({"why": "no token in ~/.copilot/config.json"}));
    };
    // Which of the two went wrong matters: blaming the config file for a
    // dropped connection sends the reader to edit a file that is fine.
    let failed = |e: String| {
        let short: String = e.chars().take(40).collect();
        Some(serde_json::json!({"why": format!("quota request failed: {}", short)}))
    };
    let auth = format!("token {}", tok);
    match tc::get(
        COPILOT_USER_API,
        &[
            ("Authorization", &auth),
            ("User-Agent", "terminal-toys"),
            ("Accept", "application/json"),
        ],
        20,
    ) {
        Ok(body) => match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(data) => Some(serde_json::json!({"data": data})),
            Err(e) => failed(e.to_string()),
        },
        Err(e) => failed(e),
    }
}

pub fn read(caches: &mut Caches, _cfg: &Config) -> Data {
    let mut d = Data::default();
    if let Some(got) = cached(caches, "copilot", LIVE_TTL, copilot_live) {
        if got["data"].is_object() {
            d.live = Some(got["data"].clone());
        }
        d.live_why = text(&got, "why");
    }
    read_store_at(&copilot_db(), &mut d);
    d
}

/// The session store, read without disturbing it.
///
/// Read-only is not decoration: this is a live agent's working state.
/// Any failure - missing, locked, corrupt - leaves the counts unset and
/// says why, because those numbers are then unknown, not zero.
fn read_store_at(path: &str, d: &mut Data) {
    if !std::path::Path::new(path).exists() {
        d.why = "no session store".into();
        return;
    }
    let opened = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|con| scan_store(&con, d));
    if let Err(e) = opened {
        d.why = e.to_string().chars().take(40).collect();
    }
}

fn scan_store(con: &Connection, d: &mut Data) -> rusqlite::Result<()> {
    d.sessions = con.query_row("select count(*) from sessions", [], |r| r.get(0))?;
    // Every sum() comes back NULL on an empty table, so each is read as an
    // Option rather than trusted to be a number.
    type Sums = (
        i64,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
    );
    let row: Sums = con.query_row(
        "select count(*), sum(input_tokens), sum(output_tokens), \
         sum(cache_read_tokens), sum(reasoning_tokens), \
         sum(total_nano_aiu), avg(time_to_first_token_ms), \
         avg(inter_token_latency_ms), sum(duration_ms) \
         from assistant_usage_events",
        [],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        },
    )?;
    d.events = row.0;
    if d.events == 0 {
        return Ok(());
    }
    d.usage = Some(Spent {
        input: row.1.unwrap_or(0.0),
        output: row.2.unwrap_or(0.0),
        cache: row.3.unwrap_or(0.0),
        reasoning: row.4.unwrap_or(0.0),
        nano_aiu: row.5.unwrap_or(0.0),
        ttft: row.6.unwrap_or(0.0),
        itl: row.7.unwrap_or(0.0),
        ms: row.8.unwrap_or(0.0),
    });
    // localtime, because claude, codex, cursor and grok all bucket by the
    // reader's own midnight and day_calendar draws them on one wall under
    // the same headings. created_at is stored with a trailing Z, so plain
    // date() is the UTC day: east of Greenwich, an evening turn filed
    // under yesterday.
    let mut daily = con.prepare(
        "select date(created_at, 'localtime'), model, sum(input_tokens), \
         sum(output_tokens), sum(cache_read_tokens), \
         sum(cache_write_tokens) from assistant_usage_events \
         group by 1, 2",
    )?;
    let found = daily.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<f64>>(2)?,
            r.get::<_, Option<f64>>(3)?,
            r.get::<_, Option<f64>>(4)?,
            r.get::<_, Option<f64>>(5)?,
        ))
    })?;
    for got in found {
        let (day, model, i_tok, o_tok, cr, cw) = got?;
        // A NULL created_at has no day. usage.py keys it "None", which
        // sorts above every real date and would count into today forever.
        let Some(day) = day.filter(|x| !x.is_empty()) else {
            continue;
        };
        let bucket = d
            .daily
            .entry(day)
            .or_default()
            .entry(model.unwrap_or_default())
            .or_insert_with(empty_tokens);
        *bucket.get_mut("input").unwrap() += i_tok.unwrap_or(0.0);
        *bucket.get_mut("output").unwrap() += o_tok.unwrap_or(0.0);
        *bucket.get_mut("cache_read").unwrap() += cr.unwrap_or(0.0);
        *bucket.get_mut("cache_write").unwrap() += cw.unwrap_or(0.0);
    }
    let mut models = con.prepare(
        "select model, count(*), sum(output_tokens), \
         sum(input_tokens), sum(cache_read_tokens), \
         sum(cache_write_tokens) \
         from assistant_usage_events group by model \
         order by 3 desc limit 6",
    )?;
    let found = models.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<f64>>(2)?,
        ))
    })?;
    for got in found {
        let (model, turns, out_tok) = got?;
        d.models.push((
            model.filter(|m| !m.is_empty()).unwrap_or_else(|| "?".into()),
            turns,
            out_tok.unwrap_or(0.0),
        ));
    }
    Ok(())
}

/// Every quota this agent publishes, for the summary screen.
///
/// window_secs carries quota_window's refusal: None whenever the reset is
/// not midnight UTC on the first of a month, and the summary then draws no
/// pace mark rather than a wrong one.
pub fn lanes(d: &Data) -> Vec<Lane> {
    let Some(live) = d.live.as_ref() else {
        return Vec::new();
    };
    let stamp = iso_epoch(&text(live, "quota_reset_date_utc"));
    let span = quota_window(stamp).map(|(_, secs)| secs);
    let mut out = Vec::new();
    for (key, snap) in live["quota_snapshots"].as_object().into_iter().flatten() {
        // An enterprise seat is why two of the three pools come back
        // unlimited; a pool with no denominator has no percentage to rank.
        if snap["unlimited"].as_bool().unwrap_or(false) || snap["percent_remaining"].is_null() {
            continue;
        }
        out.push(Lane {
            label: key.replace('_', " ").replace("interactions", "reqs"),
            pct: 100.0 - num(snap, "percent_remaining"),
            window_secs: span,
            reset: stamp,
            stale: false,
            projected: false,
        });
    }
    out
}

/// Which subscription this quota belongs to, and since when.
///
/// An enterprise seat is why two of the three pools come back unlimited,
/// and assigned_date is the only field saying how long it has been so.
pub fn copilot_plan_rows(live: &serde_json::Value, w: usize, p: &Palette) -> Vec<String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let assigned = text(live, "assigned_date");
    let day = iso_day(&assigned);
    if let Some(since) = iso_epoch(&assigned) {
        if !day.is_empty() {
            pairs.push(("seat since".into(), format!("{} · {} ago", day, ago(since))));
        }
    }
    let orgs: Vec<String> = live["organization_list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            [text(o, "name"), text(o, "login")].into_iter().find(|s| !s.is_empty())
        })
        .collect();
    if !orgs.is_empty() {
        pairs.push(("organisation".into(), orgs.join(", ")));
    }
    for (label, key) in [("account", "login"), ("sku", "access_type_sku")] {
        let value = text(live, key);
        if !value.is_empty() {
            pairs.push((label.into(), value));
        }
    }
    if live["token_based_billing"].as_bool().unwrap_or(false) {
        pairs.push(("billing".into(), "token-based".into()));
    }
    let on: Vec<String> = COPILOT_FEATURES
        .iter()
        .filter(|(flag, _)| live[*flag].as_bool().unwrap_or(false))
        .map(|(_, name)| name.to_string())
        .collect();
    plan_rows(
        &text(live, "copilot_plan"),
        &pairs,
        w,
        if live["can_upgrade_plan"].as_bool().unwrap_or(false) { "upgradeable" } else { "" },
        Some(("enabled", &on)),
        "",
        p,
    )
}

pub fn copilot_metered(d: &Data, w: usize, cfg: &Config, p: &Palette) -> Vec<String> {
    metered_rows(
        &[
            ("today".to_string(), claude::window_models(&d.daily, 1)),
            ("30 days".to_string(), claude::window_models(&d.daily, 30)),
        ],
        w,
        "",
        "copilot",
        "this machine",
        "Counted from the local session store. Copilot in an editor, on \
         another machine or on github.com is not in here.",
        cfg,
        p,
    )
}

/// 1234567 -> "1,234,567". Entitlements run to thousands of requests, and
/// nobody counts an unbroken run of digits.
fn commas(n: i64) -> String {
    let raw = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in raw.chars().enumerate() {
        if i > 0 && (raw.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        format!("-{}", out)
    } else {
        out
    }
}

fn copilot_tab(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let null = serde_json::Value::Null;
    let live = d.live.as_ref().unwrap_or(&null);
    let mut rows: Vec<String> = Vec::new();
    let snaps = live["quota_snapshots"].as_object().cloned().unwrap_or_default();
    if !snaps.is_empty() {
        // A monthly quota reset arrives as a bare date; days remaining is
        // the form every other tab here uses, and the one anyone reads.
        // quota_reset_date_utc, not quota_reset_date: the bare date carries
        // no zone, so it parses as local midnight and the countdown drifts
        // by the machine's UTC offset. Zero on the machine this was written
        // on, which is exactly why it would have gone unnoticed there.
        let stamp = iso_epoch(&text(live, "quota_reset_date_utc"));
        let mut when = String::new();
        if let Some(stamp) = stamp {
            let left = stamp - now();
            // "resets today" was already better than a truncated 0d, and
            // the hours it was rounding away are better still.
            when = if left > 0.0 {
                format!("resets in {}", left_span(left))
            } else {
                "resetting".into()
            };
        }
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── QUOTA ── ".into()),
                (p.ok.as_str(), "live".into()),
                (p.dim.as_str(), " · account-wide".into()),
            ],
            w - 1,
        ));
        // The reset sits with the window rather than on the header: they
        // are two halves of the same cycle, and the header had no room for
        // it. The window is claimed only when quota_window could derive it.
        let window = quota_window(stamp);
        let span_label = window.as_ref().map(|(label, _)| label.clone());
        let span_secs = window.map(|(_, secs)| secs);
        if span_label.is_some() || !when.is_empty() {
            rows.push(tc::seg(
                &[
                    (p.dim.as_str(), "  window ".into()),
                    (p.txt.as_str(), span_label.clone().unwrap_or_else(|| "—".into())),
                    (
                        p.dim.as_str(),
                        if span_label.is_some() { " · monthly · ".into() } else { " ".into() },
                    ),
                    (p.dim.as_str(), when),
                ],
                w - 1,
            ));
        }
        let label_w = snaps.keys().map(|k| k.chars().count()).max().unwrap_or(0).max(9);
        let mut keys: Vec<&String> = snaps.keys().collect();
        keys.sort_by_key(|k| (snaps[*k]["unlimited"].as_bool().unwrap_or(false), (*k).clone()));
        for key in keys {
            let q = &snaps[key];
            let name = tc::pad(&key.replace('_', " "), label_w);
            if q["unlimited"].as_bool().unwrap_or(false) {
                // No denominator, so no bar. An unlimited pool drawn as an
                // empty gauge invents a limit that was explicitly denied.
                rows.push(tc::seg(
                    &[
                        (p.dim.as_str(), format!(" {} ", name)),
                        (p.ok.as_str(), "unlimited".into()),
                    ],
                    w - 1,
                ));
                continue;
            }
            let ent = num(q, "entitlement");
            // percent_remaining is what the API gives; every other tab here
            // shows what is spent, and red belongs at the empty end.
            let pct = 100.0 - num(q, "percent_remaining");
            let used = (pct / 100.0).clamp(0.0, 1.0);
            let room = ((w as i64) - 38 - label_w as i64).max(8) as usize;
            let hue = agent_hue("copilot");
            let mut line: Vec<(String, String)> = vec![(p.dim.clone(), format!(" {} ", name))];
            line.extend(paced_bar(used, elapsed_of(span_secs, stamp), room, hue, p));
            line.push((pct_colour(pct, hue, p), pct_text(pct)));
            line.push(pace_cell(lead(pct, span_secs, stamp), p));
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
            // A pool can carry its own reset, in which case it is not on
            // the account-wide cycle in the header and has to say so
            // itself.
            let own = num(q, "quota_reset_at");
            let mut own_when = String::new();
            if own > 0.0 && (own - stamp.unwrap_or(0.0)).abs() > 3600.0 {
                let left = own - now();
                own_when = if left > 0.0 {
                    format!("   resets in {}", left_span(left))
                } else {
                    "   resetting".into()
                };
            }
            if ent > 0.0 {
                let mut line: Vec<(String, String)> = vec![
                    (p.dim.clone(), format!(" {}  ", " ".repeat(label_w))),
                    (p.txt.clone(), commas(num(q, "credits_used") as i64)),
                    (p.dim.clone(), " of ".into()),
                    (p.txt.clone(), commas(ent as i64)),
                ];
                // No room for the remainder on a narrow pane.
                if (label_w as i64) + 34 <= (w as i64) - 1 {
                    line.push((p.dim.clone(), " · ".into()));
                    line.push((p.txt.clone(), commas(num(q, "remaining") as i64)));
                    line.push((p.dim.clone(), " left".into()));
                }
                let over = num(q, "overage_count");
                line.push((
                    p.warn.clone(),
                    if over > 0.0 { format!("   {} over", over as i64) } else { String::new() },
                ));
                line.push((p.dim.clone(), own_when));
                let refs: Vec<(&str, String)> =
                    line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                rows.push(tc::seg(&refs, w - 1));
            }
        }
        rows.push(String::new());
    } else if !d.live_why.is_empty() {
        rows.push(tc::seg(
            &[(p.warn.as_str(), format!("  no quota: {}", d.live_why))],
            w - 1,
        ));
        rows.push(String::new());
    }

    match d.usage.as_ref() {
        Some(u) => {
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── SPENT ── ".into()),
                    (
                        p.dim.as_str(),
                        format!("{} turns across {} sessions", d.events, d.sessions),
                    ),
                ],
                w - 1,
            ));
            let cells: Vec<(&str, String, &str)> = vec![
                ("input tokens", big_num(u.input), p.txt.as_str()),
                ("output tokens", big_num(u.output), p.agent.as_str()),
                ("cache read", big_num(u.cache), p.dim.as_str()),
                ("reasoning tokens", big_num(u.reasoning), p.txt.as_str()),
                // total_nano_aiu is billionths of an AI unit.
                ("AI units", format!("{:.3}", u.nano_aiu / 1e9), p.txt.as_str()),
                ("time generating", span_ms(u.ms), p.dim.as_str()),
            ];
            let label_w = cells.iter().map(|c| c.0.chars().count()).max().unwrap_or(0);
            let ncols: usize =
                if (w as i64 - 2) / 2 - label_w as i64 - 3 >= 8 { 2 } else { 1 };
            let cw = (w - 2) / ncols;
            let val_w = (cw as i64 - label_w as i64 - 3).max(5) as usize;
            for chunk in cells.chunks(ncols) {
                let mut line: Vec<(String, String)> = vec![(tc::RST.to_string(), " ".into())];
                for (lab, value, colour) in chunk {
                    line.push((p.dim.clone(), format!(" {} ", tc::pad(lab, label_w))));
                    line.push((colour.to_string(), tc::pad(value, val_w)));
                }
                let refs: Vec<(&str, String)> =
                    line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                rows.push(tc::seg(&refs, w - 1));
            }
            rows.push(String::new());
            // The one agent that measures this rather than leaving it to be
            // inferred from timestamps, which is why it is stated flatly.
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── LATENCY ── ".into()),
                    (p.dim.as_str(), "measured by Copilot, not inferred".into()),
                ],
                w - 1,
            ));
            rows.push(tc::seg(
                &[
                    (p.dim.as_str(), "  first token ".into()),
                    (p.txt.as_str(), format!("{:.0} ms", u.ttft)),
                    (p.dim.as_str(), "   between tokens ".into()),
                    (p.txt.as_str(), format!("{:.1} ms", u.itl)),
                ],
                w - 1,
            ));
            rows.push(String::new());
            if !d.models.is_empty() {
                rows.push(tc::seg(
                    &[
                        (p.lbl.as_str(), " ── BY MODEL ── ".into()),
                        (p.dim.as_str(), "output tokens".into()),
                    ],
                    w - 1,
                ));
                for (model, turns, out_tok) in &d.models {
                    rows.push(tc::seg(
                        &[
                            (p.txt.as_str(), format!("  {}", tc::pad(model, 28))),
                            (p.dim.as_str(), format!("{:5} turns  ", turns)),
                            (p.agent.as_str(), big_num(*out_tok)),
                        ],
                        w - 1,
                    ));
                }
            }
        }
        // usage.py says "no local sessions" here whatever `why` holds, so a
        // locked store read as an empty one. Unreadable and empty are
        // different facts and get different sentences.
        None if !d.why.is_empty() && d.why != "no session store" => {
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── SPENT ── ".into()),
                    (p.warn.as_str(), "session store unreadable".into()),
                ],
                w - 1,
            ));
            rows.push(String::new());
            rows.extend(no_local(
                &format!(
                    "The session store could not be read ({}), so what it holds is \
                     unknown - not zero.",
                    d.why
                ),
                "",
                w,
                p,
            ));
        }
        None => {
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── SPENT ── ".into()),
                    (p.dim.as_str(), "no local sessions".into()),
                ],
                w - 1,
            ));
            rows.push(String::new());
            rows.extend(no_local(
                "Nothing recorded in the local session store yet.",
                run_hint("copilot"),
                w,
                p,
            ));
        }
    }
    rows
}

/// The whole tab: the quota, what this machine spent, what that cost, and
/// which subscription the percentages are percentages of.
pub fn tab(d: &Data, w: usize, _h: usize, cfg: &Config, p: &Palette) -> Vec<String> {
    let body = add_section(copilot_tab(d, w, p), copilot_metered(d, w, cfg, p));
    let null = serde_json::Value::Null;
    add_section(body, copilot_plan_rows(d.live.as_ref().unwrap_or(&null), w, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc_epoch(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> f64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap().timestamp() as f64
    }

    #[test]
    fn a_reset_at_midnight_utc_on_the_first_yields_the_window() {
        let (label, secs) = quota_window(Some(utc_epoch(2026, 8, 1, 0, 0, 0))).unwrap();
        assert_eq!(label, "1 Jul → 1 Aug");
        // Computed rather than a literal, so the test cannot encode the
        // same arithmetic slip as the code.
        let expect = utc_epoch(2026, 8, 1, 0, 0, 0) - utc_epoch(2026, 7, 1, 0, 0, 0);
        assert_eq!(secs, expect);
        assert_eq!(secs, 31.0 * 86400.0);
    }

    #[test]
    fn a_january_reset_reaches_back_into_december() {
        let (label, secs) = quota_window(Some(utc_epoch(2026, 1, 1, 0, 0, 0))).unwrap();
        assert_eq!(label, "1 Dec → 1 Jan");
        assert_eq!(secs, utc_epoch(2026, 1, 1, 0, 0, 0) - utc_epoch(2025, 12, 1, 0, 0, 0));
    }

    #[test]
    fn a_march_reset_gets_februarys_shorter_window() {
        let (_, secs) = quota_window(Some(utc_epoch(2026, 3, 1, 0, 0, 0))).unwrap();
        assert_eq!(secs, 28.0 * 86400.0);
    }

    #[test]
    fn a_reset_anywhere_but_midnight_on_the_first_is_refused() {
        // Not the first of a month.
        assert!(quota_window(Some(utc_epoch(2026, 8, 15, 0, 0, 0))).is_none());
        // The first, but not midnight.
        assert!(quota_window(Some(utc_epoch(2026, 8, 1, 7, 0, 0))).is_none());
        assert!(quota_window(Some(utc_epoch(2026, 8, 1, 0, 30, 0))).is_none());
        assert!(quota_window(Some(utc_epoch(2026, 8, 1, 0, 0, 30))).is_none());
    }

    #[test]
    fn no_reset_means_no_window() {
        assert!(quota_window(None).is_none());
        // Epoch zero is 1 Jan 1970 00:00 UTC, which would pass the shape
        // check while meaning "no timestamp".
        assert!(quota_window(Some(0.0)).is_none());
    }

    fn live_with_reset(reset: &str) -> Data {
        let mut d = Data::default();
        d.live = Some(serde_json::json!({
            "quota_reset_date_utc": reset,
            "quota_snapshots": {
                "premium_interactions": {"unlimited": false, "percent_remaining": 25.0},
                "chat": {"unlimited": true},
                "completions": {"unlimited": false}
            }
        }));
        d
    }

    #[test]
    fn lanes_skip_unlimited_pools_and_pools_with_no_percentage() {
        let got = lanes(&live_with_reset("2026-09-01T00:00:00Z"));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].label, "premium reqs");
        assert_eq!(got[0].pct, 75.0);
    }

    #[test]
    fn lanes_carry_the_window_only_when_it_could_be_derived() {
        // Midnight UTC on the first: a calendar month, so the window is
        // claimed - August's 31 days.
        let got = lanes(&live_with_reset("2026-09-01T00:00:00Z"));
        assert_eq!(got[0].window_secs, Some(31.0 * 86400.0));
        assert!(got[0].reset.is_some());
        // Mid-month: the window is not known, so no pace mark is drawn -
        // but the lane itself and its reset are still real.
        let got = lanes(&live_with_reset("2026-09-15T00:00:00Z"));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].window_secs, None);
        assert!(got[0].reset.is_some());
    }

    fn store_with(rows: &[&str]) -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "create table sessions (id integer primary key); \
             create table assistant_usage_events ( \
               created_at text, model text, input_tokens integer, \
               output_tokens integer, cache_read_tokens integer, \
               cache_write_tokens integer, reasoning_tokens integer, \
               total_nano_aiu integer, time_to_first_token_ms real, \
               inter_token_latency_ms real, duration_ms integer);",
        )
        .unwrap();
        for values in rows {
            con.execute_batch(&format!(
                "insert into assistant_usage_events values ({});",
                values
            ))
            .unwrap();
        }
        con
    }

    #[test]
    fn a_turn_is_filed_under_the_day_it_was_here() {
        // 20:00 UTC is already tomorrow anywhere east of Greenwich. The
        // other five readers bucket by the reader's own midnight and
        // day_calendar draws them all on one wall, so this one must too.
        // Every other store test uses a mid-morning stamp, which falls on
        // the same date in both zones and cannot tell them apart.
        let con = store_with(&[
            "'2026-08-16T20:00:00Z', 'model-a', 100, 0, 0, 0, 0, 0, 0.0, 0.0, 0",
        ]);
        let mut d = Data::default();
        scan_store(&con, &mut d).expect("a readable store");
        let days: Vec<&String> = d.daily.keys().collect();
        assert_eq!(days.len(), 1, "{:?}", days);
        let want = chrono::Local
            .timestamp_opt(
                chrono::DateTime::parse_from_rfc3339("2026-08-16T20:00:00Z")
                    .expect("a fixed stamp")
                    .timestamp(),
                0,
            )
            .single()
            .expect("a local time")
            .format("%Y-%m-%d")
            .to_string();
        assert_eq!(days[0], &want, "filed under the wrong day");
    }

    #[test]
    fn the_session_store_sums_read_whole() {
        let con = store_with(&[
            "'2026-08-20T10:00:00Z', 'model-a', 100, 200, 50, 10, 5, 1500000000, 500.0, 20.0, 3000",
            "'2026-08-21T11:00:00Z', 'model-b', 10, 999, 40, 0, 0, 500000000, 300.0, 10.0, 2000",
        ]);
        con.execute_batch("insert into sessions values (1); insert into sessions values (2);")
            .unwrap();
        let mut d = Data::default();
        scan_store(&con, &mut d).unwrap();
        assert_eq!(d.sessions, 2);
        assert_eq!(d.events, 2);
        let u = d.usage.as_ref().unwrap();
        assert_eq!(u.input, 110.0);
        assert_eq!(u.output, 1199.0);
        assert_eq!(u.cache, 90.0);
        assert_eq!(u.reasoning, 5.0);
        assert_eq!(u.nano_aiu, 2_000_000_000.0);
        assert_eq!(u.ttft, 400.0);
        assert_eq!(u.itl, 15.0);
        assert_eq!(u.ms, 5000.0);
        let day = &d.daily["2026-08-20"]["model-a"];
        assert_eq!(day["input"], 100.0);
        assert_eq!(day["output"], 200.0);
        assert_eq!(day["cache_read"], 50.0);
        assert_eq!(day["cache_write"], 10.0);
        assert_eq!(day["cache_write_1h"], 0.0);
        // Busiest model first, by output tokens.
        assert_eq!(d.models[0], ("model-b".to_string(), 1, 999.0));
        assert_eq!(d.models[1], ("model-a".to_string(), 1, 200.0));
    }

    #[test]
    fn null_sums_are_read_as_options_not_trusted_as_numbers() {
        // One event whose every column is NULL: count(*) is 1, and every
        // sum() and avg() beside it is NULL rather than a number.
        let con = store_with(&[
            "NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL",
        ]);
        let mut d = Data::default();
        scan_store(&con, &mut d).unwrap();
        assert_eq!(d.events, 1);
        let u = d.usage.as_ref().unwrap();
        assert_eq!(u.input, 0.0);
        assert_eq!(u.ttft, 0.0);
        // date(NULL) is NULL: no day to file it under, so it is skipped
        // rather than keyed as a fake day that counts into today forever.
        assert!(d.daily.is_empty());
        assert_eq!(d.models[0].0, "?");
    }

    #[test]
    fn an_empty_store_leaves_the_spending_unset() {
        let con = store_with(&[]);
        let mut d = Data::default();
        scan_store(&con, &mut d).unwrap();
        assert_eq!(d.events, 0);
        assert!(d.usage.is_none());
        assert!(d.models.is_empty());
    }

    #[test]
    fn a_missing_store_is_unknown_not_zero() {
        let mut d = Data::default();
        read_store_at("/nonexistent-invented-path/session-store.db", &mut d);
        assert_eq!(d.why, "no session store");
        assert!(d.usage.is_none());
        assert_eq!(d.events, 0);
    }

    #[test]
    fn an_unreadable_store_fails_soft_with_a_reason() {
        // An invented file that is not SQLite at all; the reader must come
        // back with a why rather than a panic or a plausible zero.
        let path = std::env::temp_dir().join(format!("toys-copilot-test-{}.db", std::process::id()));
        std::fs::write(&path, "not a database, on purpose").unwrap();
        let mut d = Data::default();
        read_store_at(path.to_str().unwrap(), &mut d);
        std::fs::remove_file(&path).ok();
        assert!(!d.why.is_empty());
        assert_ne!(d.why, "no session store");
        assert!(d.usage.is_none());
    }

    #[test]
    fn the_token_survives_the_configs_comment_header() {
        let raw = "// GitHub Copilot CLI configuration\n\
                   // managed by the CLI, do not edit\n\
                   {\n  \"copilotTokens\": {\n    \"github.example\": \"tok_invented_0000\"\n  },\n\
                   \"theme\": \"auto\"\n}\n";
        assert_eq!(token_in(raw), Some("tok_invented_0000".into()));
    }

    #[test]
    fn an_empty_or_missing_token_is_none_rather_than_blank() {
        assert_eq!(token_in("{\"copilotTokens\": {\"github.example\": \"\"}}"), None);
        assert_eq!(token_in("{\"copilotTokens\": {}}"), None);
        assert_eq!(token_in("{}"), None);
        assert_eq!(token_in("not json at all"), None);
    }

    #[test]
    fn commas_group_digits_the_way_a_person_counts_them() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1000), "1,000");
        assert_eq!(commas(1234567), "1,234,567");
    }
}
