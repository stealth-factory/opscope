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

//! Cursor: the three lanes its own Usage view shows, the spend its server
//! prices, and the tracking database that says how much code it wrote.
//!
//! The one agent whose metered section needs no rate card: the API states
//! both what Cursor charges against the plan and what the same traffic
//! costs at vendor list, so nothing here is estimated. The authorship
//! counts answer a different question - how much code the agent wrote,
//! not what it cost - and the tab keeps the two apart.

use std::collections::HashMap;

use chrono::{Datelike, Duration as Days, Local, NaiveDate, TimeZone};
use rusqlite::{Connection, OpenFlags};
use toys_core as tc;

use crate::shared::*;
use crate::*;

/// The Connect endpoint cursor-agent's own Usage view calls. Not the
/// documented cursor.com dashboard API - that one wants a browser cookie
/// and returns 401 to anything this machine has. The CLI talks to
/// aiserver.v1.DashboardService with the bearer token it stores beside its
/// config, which is the credential this reuses.
const CURSOR_RPC: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/";

/// The lanes in the order cursor-agent's Usage view lists them: each one's
/// label, the field it reads, and how far up Cursor's own colour it sits.
///
/// One tint per lane rather than three unrelated hues. These are
/// categories, not one gauge, so a green-to-red severity ramp would imply
/// a relationship between them that does not exist - and each bar is
/// labelled and carries its own percentage, so the colour is decoration.
/// Tints of Cursor's own colour stay distinguishable while reading as
/// Cursor's, which three borrowed colours never did.
const CURSOR_LANES: &[(&str, &str, f64)] = &[
    ("included", "totalPercentUsed", 1.0),
    ("auto", "autoPercentUsed", 0.80),
    ("api", "apiPercentUsed", 0.62),
];

/// The events RPC's own ceiling per request.
const EVENT_PAGE: usize = 1000;
/// Enough pages to reach past any sane window.
const EVENT_PAGES: usize = 8;
/// Half an hour: thirty days of events is about five pages and eleven
/// seconds of paging, far too slow to repeat on a redraw.
const EVENTS_TTL: f64 = 1800.0;

/// What Cursor publishes, and what its tracking database recorded here.
#[derive(Clone, Default)]
pub struct Data {
    /// The tracking database was readable; the counts below mean something.
    ok: bool,
    why: String,
    /// Absent rather than broken, so the fix is running the agent.
    db_missing: bool,
    /// GetCurrentPeriodUsage: the three lanes and the billing cycle.
    live: Option<serde_json::Value>,
    /// GetPlanInfo: the plan the percentages are percentages of.
    plan: Option<serde_json::Value>,
    /// The per-day summary folded out of GetFilteredUsageEvents.
    events: Option<serde_json::Value>,
    /// GetAggregatedUsageEvents: per-model cents over the window.
    spend: Option<serde_json::Value>,
    hashes: i64,
    conversations: i64,
    models: i64,
    by_model: Vec<(String, i64)>,
    commits: i64,
    lines: i64,
    human_lines: i64,
    /// Milliseconds, from the newest tracked edit.
    last: Option<f64>,
}

/// A number whether it arrived as one or as a string.
///
/// Connect writes int64 as JSON strings, so reading the billing cycle's
/// timestamps with a plain as_f64 would produce a wrong window with no
/// error - the kind of silent zero this widget exists to avoid.
fn loose(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())
}

/// Thousands separators: "1,482,113 lines" is readable, "1482113" is not.
fn commas(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
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

fn cursor_token() -> Option<String> {
    let auth = read_json(&under_home(".config/cursor/auth.json"))?;
    let tok = text(&auth, "accessToken");
    if tok.is_empty() {
        None
    } else {
        Some(tok)
    }
}

/// One Connect-style RPC to the dashboard service.
///
/// Undocumented and versioned only by the CLI bundle it was read out of,
/// so every failure is silent and the tab simply falls back to whatever
/// else it has. The bearer token rides in a header that post_json feeds to
/// curl on its standard input, never in an argument - /proc/<pid>/cmdline
/// is world-readable.
fn cursor_rpc(method: &str, body: &serde_json::Value) -> Option<serde_json::Value> {
    let tok = cursor_token()?;
    let bearer = format!("Bearer {}", tok);
    post_json(
        &format!("{}{}", CURSOR_RPC, method),
        &[
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
            ("Connect-Protocol-Version", "1"),
            ("User-Agent", "terminal-toys"),
        ],
        &body.to_string(),
        25,
    )
}

/// Plan usage, from the endpoint cursor-agent's own Usage view calls.
fn cursor_live() -> Option<serde_json::Value> {
    cursor_rpc("GetCurrentPeriodUsage", &serde_json::json!({}))
}

/// Which Cursor plan the percentages are percentages of.
///
/// GetPlanInfo is where the plan's name and price live - the usage call
/// carries neither, and its $400 limit is meaningless without knowing that
/// is what Ultra includes.
fn cursor_plan() -> Option<serde_json::Value> {
    cursor_rpc("GetPlanInfo", &serde_json::json!({}))
}

/// Per-model tokens and real cost over a window.
///
/// This is what the plan percentages are made of: which model spent the
/// money. totalCents is Cursor's own figure, not an estimate.
fn cursor_spend(days: i64) -> Option<serde_json::Value> {
    let at = (now() * 1000.0) as i64;
    cursor_rpc(
        "GetAggregatedUsageEvents",
        &serde_json::json!({
            "startDate": (at - days * 86_400_000).to_string(),
            "endDate": at.to_string(),
        }),
    )
}

/// The per-day running totals that pages of raw events fold into.
#[derive(Default)]
struct Tally {
    by_day: HashMap<String, f64>,
    tokens_by_day: HashMap<String, f64>,
    /// day -> model -> (cents, tokens).
    by_day_model: HashMap<String, HashMap<String, (f64, f64)>>,
    vendor_cents: f64,
    tokens: f64,
    counted: u64,
}

/// Fold one page of events into the tally.
///
/// Returns the oldest timestamp on the page, which is what tells the
/// caller a page has reached past the window - events older than the cut
/// are not counted, but their age is still the stop signal.
fn tally_events(events: &[serde_json::Value], cut: f64, t: &mut Tally) -> f64 {
    let mut oldest = now();
    for e in events {
        let Some(when) = loose(&e["timestamp"]).map(|ms| ms / 1000.0) else {
            continue;
        };
        oldest = oldest.min(when);
        if when < cut {
            continue;
        }
        let usage = &e["tokenUsage"];
        let cents = loose(&usage["totalCents"]).unwrap_or(0.0);
        let n = loose(&usage["inputTokens"]).unwrap_or(0.0)
            + loose(&usage["outputTokens"]).unwrap_or(0.0);
        let Some(day) = Local
            .timestamp_opt(when as i64, 0)
            .single()
            .map(|d| d.format("%Y-%m-%d").to_string())
        else {
            continue;
        };
        *t.by_day.entry(day.clone()).or_default() += cents;
        *t.tokens_by_day.entry(day.clone()).or_default() += n;
        let model = match text(e, "model") {
            s if s.is_empty() => "?".to_string(),
            s => s,
        };
        let slot = t.by_day_model.entry(day).or_default().entry(model).or_default();
        slot.0 += cents;
        slot.1 += n;
        t.vendor_cents += cents;
        t.tokens += n;
        t.counted += 1;
    }
    oldest
}

/// Per-day spend, from the raw events cursor.com's own dashboard uses.
///
/// GetAggregatedUsageEvents totals by model and carries no timestamp at
/// all, so no calendar can be built from it. GetFilteredUsageEvents
/// returns the individual events - each with a timestamp, a model and its
/// cents - newest first, a thousand at a time.
///
/// Paging stops as soon as a page reaches past the window, so the cost is
/// proportional to the window rather than to the account's whole history;
/// EVENT_PAGES caps it regardless. Held for half an hour by the caller,
/// because this is far too slow to repeat on a redraw.
fn cursor_events(days: i64) -> Option<serde_json::Value> {
    let cut = now() - days as f64 * 86400.0;
    let mut t = Tally::default();
    for page in 1..=EVENT_PAGES {
        let got = cursor_rpc(
            "GetFilteredUsageEvents",
            &serde_json::json!({ "page": page, "pageSize": EVENT_PAGE }),
        );
        let events: Vec<serde_json::Value> = got
            .as_ref()
            .and_then(|g| g["usageEventsDisplay"].as_array())
            .cloned()
            .unwrap_or_default();
        if events.is_empty() {
            break;
        }
        let oldest = tally_events(&events, cut, &mut t);
        if oldest < cut || events.len() < EVENT_PAGE {
            break;
        }
    }
    if t.counted == 0 {
        return None;
    }
    let mut by_day_model = serde_json::Map::new();
    for (day, models) in &t.by_day_model {
        let mut inner = serde_json::Map::new();
        for (model, (cents, tokens)) in models {
            inner.insert(
                model.clone(),
                serde_json::json!({ "cents": cents, "tokens": tokens }),
            );
        }
        by_day_model.insert(day.clone(), serde_json::Value::Object(inner));
    }
    Some(serde_json::json!({
        "by_day": t.by_day,
        "tokens_by_day": t.tokens_by_day,
        "by_day_model": by_day_model,
        "vendor_cents": t.vendor_cents,
        "tokens": t.tokens,
        "events": t.counted,
        "days": days,
    }))
}

/// The authorship counts, straight out of the tracking database.
struct Tracking {
    hashes: i64,
    conversations: i64,
    models: i64,
    by_model: Vec<(String, i64)>,
    commits: i64,
    lines: i64,
    human_lines: i64,
    last: Option<f64>,
}

fn read_tracking(con: &Connection) -> rusqlite::Result<Tracking> {
    let (hashes, conversations, models) = con.query_row(
        "select count(*), count(distinct conversationId), count(distinct model) \
         from ai_code_hashes",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut stmt = con.prepare(
        "select model, count(*) from ai_code_hashes group by model order by 2 desc limit 8",
    )?;
    let by_model = stmt
        .query_map([], |r| {
            let name: Option<String> = r.get(0)?;
            Ok((
                match name {
                    Some(s) if !s.is_empty() => s,
                    _ => "?".to_string(),
                },
                r.get::<_, i64>(1)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (commits, lines, human_lines) = con.query_row(
        "select count(*), sum(linesAdded), sum(humanLinesAdded) from scored_commits",
        [],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                // sum() over no rows is NULL, and NULL here is a true zero -
                // a machine that has scored nothing - not a failure to read.
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        },
    )?;
    let last: Option<f64> =
        con.query_row("select max(timestamp) from ai_code_hashes", [], |r| r.get(0))?;
    Ok(Tracking {
        hashes,
        conversations,
        models,
        by_model,
        commits,
        lines,
        human_lines,
        last,
    })
}

pub fn read(caches: &mut Caches, _cfg: &Config) -> Data {
    let mut d = Data::default();
    // The published sections do not depend on the local database, so a
    // locked or missing file must not take the live quota down with it -
    // which is what usage.py's sqlite-error branch does.
    d.live = cached(caches, "cursor", LIVE_TTL, cursor_live);
    d.plan = cached(caches, "cursor-plan", PLAN_TTL, cursor_plan);
    d.events = cached(caches, "cursor-events", EVENTS_TTL, || cursor_events(30));
    d.spend = cached(caches, "cursor-spend", LIVE_TTL, || cursor_spend(30));
    let path = under_home(".cursor/ai-tracking/ai-code-tracking.db");
    if !std::path::Path::new(&path).exists() {
        d.why = "no tracking database".into();
        d.db_missing = true;
        return d;
    }
    // Read-only is not decoration: this is a live agent's working state.
    let opened = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|con| read_tracking(&con));
    match opened {
        Ok(t) => {
            d.ok = true;
            d.hashes = t.hashes;
            d.conversations = t.conversations;
            d.models = t.models;
            d.by_model = t.by_model;
            d.commits = t.commits;
            d.lines = t.lines;
            d.human_lines = t.human_lines;
            d.last = t.last;
        }
        // Locked or corrupt means the counts are unknown, never zero, and
        // the reason is kept for the tab to show.
        Err(e) => d.why = e.to_string().chars().take(40).collect(),
    }
    d
}

/// The billing cycle as (window seconds, reset epoch), where stated.
fn cycle_of(live: &serde_json::Value) -> (Option<f64>, Option<f64>) {
    match (loose(&live["billingCycleStart"]), loose(&live["billingCycleEnd"])) {
        (Some(s), Some(e)) if e > s => (Some((e - s) / 1000.0), Some(e / 1000.0)),
        _ => (None, None),
    }
}

/// Every quota this agent publishes, for the summary screen.
///
/// The same three percentages the tab draws, against the same billing
/// cycle. Never marked stale: the CLI keeps no quota cache on disk to fall
/// back to, so a reading is either recent or absent.
pub fn lanes(d: &Data) -> Vec<Lane> {
    let Some(live) = d.live.as_ref() else {
        return Vec::new();
    };
    let plan = &live["planUsage"];
    let (secs, reset) = cycle_of(live);
    let mut out = Vec::new();
    for (label, key, _) in CURSOR_LANES {
        let Some(pct) = loose(&plan[*key]) else {
            continue;
        };
        out.push(Lane {
            label: (*label).to_string(),
            pct,
            window_secs: secs,
            reset,
            stale: false,
            projected: false,
        });
    }
    out
}

/// The three lanes cursor-agent's Usage view shows, plus the cycle.
fn cursor_quota(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let Some(live) = d.live.as_ref() else {
        return Vec::new();
    };
    let plan = &live["planUsage"];
    if plan.as_object().is_none_or(|o| o.is_empty()) {
        return Vec::new();
    }
    let when = match loose(&live["billingCycleEnd"]) {
        Some(ends) => {
            let left = ends / 1000.0 - now();
            if left > 0.0 {
                // Truncated days read "resets in 0d" for the whole of the
                // last day of a cycle, which is when it matters most.
                format!("resets in {}", left_span(left))
            } else {
                "resetting".to_string()
            }
        }
        None => String::new(),
    };
    let (cycle_secs, reset_ts) = cycle_of(live);
    let elapsed = match (loose(&live["billingCycleStart"]), loose(&live["billingCycleEnd"])) {
        (Some(s), Some(e)) if e > s => {
            Some((100.0 * (now() * 1000.0 - s) / (e - s)).clamp(0.0, 100.0))
        }
        _ => None,
    };
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── QUOTA ── ".into()),
            (p.ok.as_str(), "live".into()),
            (
                p.dim.as_str(),
                crate::claude::scope_phrase(w, 17 + when.chars().count()).into(),
            ),
            (p.dim.as_str(), when.clone()),
        ],
        w - 1,
    )];
    if let Some(elapsed) = elapsed {
        // Just the fact; the +/- column it explains is on every tab now, so
        // a per-tab legend was both redundant and the thing that clipped.
        rows.push(tc::seg(
            &[(p.dim.as_str(), format!("  {:.0}% of the cycle gone", elapsed))],
            w - 1,
        ));
    }
    let base = agent_hue("cursor").unwrap();
    for (name, key, stop) in CURSOR_LANES {
        let Some(pct) = loose(&plan[*key]) else {
            continue;
        };
        let used = (pct / 100.0).clamp(0.0, 1.0);
        let hue = blend(base, *stop);
        // The +/- cell is how far ahead of the clock this lane is: the
        // share of the billing period gone minus the share of the
        // allowance spent. Positive is a cushion, negative means the lane
        // runs out before the cycle does. Pure arithmetic on the cycle
        // dates - nothing new is fetched for it.
        let (pace_colour, pace_txt) = pace_cell(lead(pct, cycle_secs, reset_ts), p);
        let mut line: Vec<(String, String)> =
            vec![(p.dim.clone(), format!(" {:<9}", name))];
        line.extend(paced_bar(
            used,
            elapsed_of(cycle_secs, reset_ts),
            w.saturating_sub(40).max(8),
            Some(hue),
            p,
        ));
        line.push((pct_colour(pct, Some(hue), p), pct_text(pct)));
        line.push((pace_colour, pace_txt));
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&refs, w - 1));
    }
    if let Some(limit) = loose(&plan["limit"]).filter(|v| *v != 0.0) {
        // Deliberately dollars rather than a fourth bar. This is spend
        // against the plan limit - a different denominator from the three
        // lanes above, which are the server's own percentages - and drawing
        // it as a bar beside them would invite reading 12% and 2% as the
        // same scale.
        let spent = loose(&plan["totalSpend"]).unwrap_or(0.0);
        let left = loose(&plan["remaining"]);
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "  spend ".into()),
                (p.txt.as_str(), format!("${:.2}", spent / 100.0)),
                (p.dim.as_str(), " of ".into()),
                (p.txt.as_str(), format!("${:.2}", limit / 100.0)),
                (
                    p.dim.as_str(),
                    match left {
                        Some(v) => format!("   ${:.2} left", v / 100.0),
                        None => String::new(),
                    },
                ),
            ],
            w - 1,
        ));
    }
    rows.push(String::new());
    rows
}

/// One metered window over the per-day, per-model summary: dollars, tokens
/// and the models under them, costliest first.
fn window_of(by: &serde_json::Value, days: i64, today: NaiveDate) -> (f64, f64, Vec<(String, f64)>) {
    let first = (today - Days::days(days - 1)).format("%Y-%m-%d").to_string();
    let (mut cents, mut tokens) = (0.0, 0.0);
    let mut models: HashMap<String, f64> = HashMap::new();
    for (day, entries) in by.as_object().into_iter().flatten() {
        if day.as_str() < first.as_str() {
            continue;
        }
        for (model, got) in entries.as_object().into_iter().flatten() {
            cents += num(got, "cents");
            tokens += num(got, "tokens");
            *models.entry(model.clone()).or_default() += num(got, "cents");
        }
    }
    let mut ranked: Vec<(String, f64)> = models.into_iter().map(|(m, c)| (m, c / 100.0)).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    (cents / 100.0, tokens, ranked)
}

/// What Cursor charges, beside what the same traffic costs at list.
///
/// Cursor is the one agent that publishes both, so no rate card - and no
/// config - is needed: GetAggregatedUsageEvents returns what it meters
/// against the plan, and the raw events carry their own vendor-rate cents.
/// The gap is what the subscription is worth, and every event states its
/// own discount.
fn cursor_metered(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let nothing = serde_json::Value::Null;
    let ev = d.events.as_ref().unwrap_or(&nothing);
    let metered = d
        .spend
        .as_ref()
        .and_then(|s| loose(&s["totalCostCents"]))
        .unwrap_or(0.0);
    let by = &ev["by_day_model"];
    if by.as_object().is_none_or(|o| o.is_empty()) && metered == 0.0 {
        return Vec::new();
    }
    let today = Local::now().date_naive();
    let span = match ev["days"].as_i64() {
        Some(days) if days > 0 => days,
        _ => 30,
    };
    let mut windows: Vec<Window> = Vec::new();
    for (label, days) in [("today", 1), ("30 days", span)] {
        let (cost, tokens, models) = window_of(by, days, today);
        windows.push((label.to_string(), cost, tokens, models));
    }
    let vendor = loose(&ev["vendor_cents"]).unwrap_or(0.0);
    let saves = if vendor > 0.0 && metered > 0.0 {
        Some((vendor - metered) / 100.0)
    } else {
        None
    };
    metered_block(
        "vendor rates",
        &windows,
        w,
        &[
            (
                "cursor meters".to_string(),
                (metered > 0.0).then_some(metered / 100.0),
                p.txt.clone(),
            ),
            ("the plan saves".to_string(), saves, p.ok.clone()),
        ],
        "",
        "account-wide",
        "From Cursor's own API, so it covers every device on the account, not just this one.",
        p,
    )
}

/// Spend per day, one column per day, the way the usage page shows it.
///
/// A bar chart rather than the calendar the token tabs use: this window is
/// thirty days, and thirty cells of a year-wide grid is six columns of
/// colour in a field of dots. Money over a month reads better as a
/// profile, and it is the shape Cursor's own dashboard draws.
fn cursor_daily(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let Some(ev) = d.events.as_ref() else {
        return Vec::new();
    };
    let Some(by_day) = ev["by_day"].as_object().filter(|o| !o.is_empty()) else {
        return Vec::new();
    };
    let days = match ev["days"].as_i64() {
        Some(n) if n > 0 => n,
        _ => 30,
    };
    let today = Local::now().date_naive();
    let series: Vec<NaiveDate> = (0..days).rev().map(|n| today - Days::days(n)).collect();
    let cents: Vec<f64> = series
        .iter()
        .map(|day| {
            by_day
                .get(&day.format("%Y-%m-%d").to_string())
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
        })
        .collect();
    let top = cents.iter().cloned().fold(0.0f64, f64::max);
    let peak = if top > 0.0 { top } else { 1.0 };
    // The peak's own day, or nothing when every day is zero - a peak of $0
    // has no date worth naming.
    let best = (top > 0.0)
        .then(|| cents.iter().position(|c| *c == top))
        .flatten()
        .map(|i| series[i]);
    let label = |d: &NaiveDate| format!("{} {}", MONTHS[d.month0() as usize], d.day());
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── SPEND / DAY ── ".into()),
            (p.dim.as_str(), format!("{}d · peak ", days)),
            (p.agent.as_str(), format!("${:.0}", peak / 100.0)),
            (
                p.dim.as_str(),
                format!(
                    " on {}",
                    best.as_ref().map(|b| label(b)).unwrap_or_else(|| "--".into())
                ),
            ),
            (p.dim.as_str(), " · today ".into()),
            (
                p.txt.as_str(),
                format!("${:.2}", cents.last().copied().unwrap_or(0.0) / 100.0),
            ),
        ],
        w - 1,
    )];
    let avail = w.saturating_sub(3).max(10);
    let mut cols: Vec<(f64, String)> = Vec::new();
    for (c, wide) in cents.iter().zip(tc::spread(cents.len(), avail)) {
        cols.extend(std::iter::repeat_n((*c, p.agent.clone()), wide));
    }
    for line in tc::vbars(&cols, 3, peak) {
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
    let (left, right) = (label(&series[0]), label(&series[series.len() - 1]));
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
    rows.push(String::new());
    rows
}

/// Where the money went, per model, over the last 30 days.
fn cursor_spend_rows(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let Some(spend) = d.spend.as_ref() else {
        return Vec::new();
    };
    let Some(aggs) = spend["aggregations"].as_array().filter(|a| !a.is_empty()) else {
        return Vec::new();
    };
    let grab = |key: &str| loose(&spend[key]).unwrap_or(0.0);
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── SPEND ── ".into()),
            (p.dim.as_str(), "last 30d · ".into()),
            (p.agent.as_str(), format!("${:.2}", grab("totalCostCents") / 100.0)),
            (p.dim.as_str(), "  in ".into()),
            (p.txt.as_str(), big_num(grab("totalInputTokens"))),
            (p.dim.as_str(), " · out ".into()),
            (p.txt.as_str(), big_num(grab("totalOutputTokens"))),
            (p.dim.as_str(), " · cache ".into()),
            (p.txt.as_str(), big_num(grab("totalCacheReadTokens"))),
        ],
        w - 1,
    )];
    let mut models: Vec<&serde_json::Value> = aggs.iter().collect();
    let cents_of = |a: &serde_json::Value| loose(&a["totalCents"]).unwrap_or(0.0);
    models.sort_by(|a, b| cents_of(b).total_cmp(&cents_of(a)));
    let top = match cents_of(models[0]) {
        v if v != 0.0 => v,
        _ => 1.0,
    };
    for a in models.iter().take(6) {
        let cents = cents_of(a);
        let bar = tc::meter(cents / top, w.saturating_sub(44).max(6));
        let filled = bar.chars().filter(|c| *c == '█').count();
        let name = match text(a, "modelIntent") {
            s if s.is_empty() => "?".to_string(),
            s => s,
        };
        rows.push(tc::seg(
            &[
                (p.txt.as_str(), format!("  {}", tc::pad(&name, 26))),
                (p.agent.as_str(), format!("{:>9} ", format!("${:.2}", cents / 100.0))),
                (p.agent.as_str(), bar.chars().take(filled).collect::<String>()),
                (p.grid.as_str(), bar.chars().skip(filled).collect::<String>()),
            ],
            w - 1,
        ));
    }
    rows.push(String::new());
    rows
}

fn cursor_plan_rows(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let Some(info) = d.plan.as_ref() else {
        return Vec::new();
    };
    let plan = &info["planInfo"];
    if plan.as_object().is_none_or(|o| o.is_empty()) {
        return Vec::new();
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    let price = text(plan, "price");
    if !price.is_empty() {
        pairs.push(("price".into(), price));
    }
    if let Some(inc) = loose(&plan["includedAmountCents"]).filter(|v| *v != 0.0) {
        pairs.push(("included".into(), format!("${:.2} per cycle", inc / 100.0)));
    }
    let owner = text(plan, "planOwner").replace("PLAN_OWNER_", "").to_lowercase();
    if !owner.is_empty() {
        pairs.push(("billing".into(), owner));
    }
    plan_rows(&text(plan, "planName"), &pairs, w, "", None, "", p)
}

/// The tab's own body: the lanes, the spend charts, and the authorship
/// counts from the tracking database.
fn cursor_tab(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let mut rows = cursor_quota(d, w, p);
    rows.extend(cursor_daily(d, w, p));
    rows.extend(cursor_spend_rows(d, w, p));
    if !d.ok {
        // An unreadable database makes the counts unknown, not zero, and
        // which kind of unreadable matters: absent means run the agent,
        // locked or corrupt names sqlite's own reason and will likely fix
        // itself. usage.py shows a fixed "no tracking database" either way,
        // which is false for a locked file.
        let why = if d.why.is_empty() { "not read yet" } else { d.why.as_str() };
        let note = no_local(
            &format!("No AI-written-code counts: {}.", why),
            if d.db_missing { run_hint("cursor") } else { "" },
            w,
            p,
        );
        if rows.is_empty() {
            return note;
        }
        return add_section(rows, note);
    }
    rows.push(tc::seg(
        &[
            (p.lbl.as_str(), " ── AI-WRITTEN CODE ── ".into()),
            (
                p.dim.as_str(),
                format!("last seen {} ago", ago(d.last.map(|ms| ms / 1000.0).unwrap_or(0.0))),
            ),
        ],
        w - 1,
    ));
    let (ai, human) = (d.lines, d.human_lines);
    let total = ai + human;
    let cells: Vec<(&str, String, &str)> = vec![
        ("tracked edits", commas(d.hashes), p.agent.as_str()),
        ("conversations", commas(d.conversations), p.txt.as_str()),
        ("scored commits", commas(d.commits), p.txt.as_str()),
        ("lines by agent", commas(ai), p.agent.as_str()),
        ("lines by hand", commas(human), p.txt.as_str()),
        ("models used", format!("{}", d.models), p.dim.as_str()),
    ];
    let label_w = cells.iter().map(|c| c.0.len()).max().unwrap_or(8);
    let ncols = if w.saturating_sub(2) / 2 >= label_w + 11 { 2 } else { 1 };
    let cw = w.saturating_sub(2) / ncols;
    let val_w = cw.saturating_sub(label_w + 3).max(5);
    for chunk in cells.chunks(ncols) {
        let mut line: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
        for (label, value, colour) in chunk {
            line.push((p.dim.as_str(), format!(" {} ", tc::pad(label, label_w))));
            line.push((colour, tc::pad(value, val_w)));
        }
        rows.push(tc::seg(&line, w - 1));
    }
    if total > 0 {
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── WHO WROTE IT ── ".into()),
                (p.dim.as_str(), format!("{} lines scored", commas(total))),
            ],
            w - 1,
        ));
        let split = [
            (ai as f64 / total as f64, p.agent.clone()),
            (human as f64 / total as f64, p.dim.clone()),
        ];
        let mut bar: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
        let segs = tc::stacked_bar(&split, w.saturating_sub(3).max(10));
        for (colour, run) in &segs {
            bar.push((colour.as_str(), run.clone()));
        }
        rows.push(tc::seg(&bar, w - 1));
        rows.push(tc::seg(
            &[
                (
                    p.agent.as_str(),
                    format!(" ▇ agent {} ({:.0}%)", commas(ai), 100.0 * ai as f64 / total as f64),
                ),
                (
                    p.dim.as_str(),
                    format!(
                        "   ▇ hand {} ({:.0}%)",
                        commas(human),
                        100.0 * human as f64 / total as f64
                    ),
                ),
            ],
            w - 1,
        ));
    }
    if !d.by_model.is_empty() {
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── BY MODEL ── ".into()),
                (p.dim.as_str(), "tracked edits".into()),
            ],
            w - 1,
        ));
        let top = d.by_model[0].1.max(1);
        for (name, n) in d.by_model.iter().take(5) {
            let bar = tc::meter(*n as f64 / top as f64, w.saturating_sub(36).max(6));
            let filled = bar.chars().filter(|c| *c == '█').count();
            rows.push(tc::seg(
                &[
                    (p.txt.as_str(), format!("  {}", tc::pad(name, 22))),
                    (p.agent.as_str(), format!("{:>7} ", commas(*n))),
                    (p.agent.as_str(), bar.chars().take(filled).collect::<String>()),
                    (p.grid.as_str(), bar.chars().skip(filled).collect::<String>()),
                ],
                w - 1,
            ));
        }
    }
    rows.push(String::new());
    rows.push(tc::seg(
        &[(
            p.dim.as_str(),
            "  Authorship, not spend: this is how much code the agent wrote,".into(),
        )],
        w - 1,
    ));
    rows.push(tc::seg(
        &[(p.dim.as_str(), "  which is a different question from what it cost.".into())],
        w - 1,
    ));
    rows
}

/// The whole tab: the lanes and charts, what Cursor metered, and the plan
/// the percentages are percentages of.
pub fn tab(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    // METERED sits last-but-one on every tab, so Cursor's - which is
    // published by the server rather than priced from a rate card - lands
    // in the same place as everyone else's rather than floating up beside
    // its quota.
    let body = add_section(cursor_tab(d, w, p), cursor_metered(d, w, p));
    add_section(body, cursor_plan_rows(d, w, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_schema() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "create table ai_code_hashes \
               (hash text, conversationId text, model text, timestamp integer);\
             create table scored_commits \
               (hash text, linesAdded integer, humanLinesAdded integer);",
        )
        .unwrap();
        con
    }

    #[test]
    fn the_tracking_reader_counts_distinct_not_rows() {
        let con = empty_schema();
        // Two rows share a conversation so the distinct counts actually
        // discriminate from the plain ones.
        con.execute_batch(
            "insert into ai_code_hashes values
               ('a1', 'conv-1', 'model-a', 1000),
               ('a2', 'conv-1', 'model-a', 2000),
               ('a3', 'conv-2', 'model-b', 3000);\
             insert into scored_commits values ('c1', 120, 30), ('c2', 10, 5);",
        )
        .unwrap();
        let t = read_tracking(&con).unwrap();
        assert_eq!(t.hashes, 3);
        assert_eq!(t.conversations, 2);
        assert_eq!(t.models, 2);
        assert_eq!(t.by_model[0], ("model-a".to_string(), 2));
        assert_eq!(t.commits, 2);
        assert_eq!(t.lines, 130);
        assert_eq!(t.human_lines, 35);
        assert_eq!(t.last, Some(3000.0));
    }

    #[test]
    fn an_empty_database_reads_as_zeros_not_an_error() {
        // sum() over no rows is NULL, and NULL here is a true zero - a
        // machine that has scored nothing - not a failure to read.
        let t = read_tracking(&empty_schema()).unwrap();
        assert_eq!(t.commits, 0);
        assert_eq!(t.lines, 0);
        assert_eq!(t.human_lines, 0);
        assert_eq!(t.last, None);
    }

    #[test]
    fn a_broken_database_is_a_why_not_a_zero() {
        // No tables at all: the reader must error so the tab can say why,
        // never coast through with zeros that look like idleness.
        let con = Connection::open_in_memory().unwrap();
        let why = match read_tracking(&con) {
            Ok(_) => String::new(),
            Err(e) => e.to_string(),
        };
        assert!(!why.is_empty(), "a missing table must carry a reason");
    }

    #[test]
    fn a_lane_that_publishes_nothing_is_absent_not_zero() {
        // autoPercentUsed is missing, so no "auto" lane may appear - a
        // fabricated 0% is indistinguishable from an untouched lane.
        let d = Data {
            live: Some(serde_json::json!({
                "planUsage": { "totalPercentUsed": 41.5, "apiPercentUsed": "2.5" },
                "billingCycleStart": "1700000000000",
                "billingCycleEnd": "1702592000000",
            })),
            ..Data::default()
        };
        let got = lanes(&d);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].label, "included");
        assert!((got[0].pct - 41.5).abs() < 1e-9);
        // Connect writes int64 as strings; the cycle still has to be read.
        assert_eq!(got[0].window_secs, Some(2_592_000.0));
        assert_eq!(got[0].reset, Some(1_702_592_000.0));
        // A percentage that arrives as a string is still a percentage.
        assert_eq!(got[1].label, "api");
        assert!((got[1].pct - 2.5).abs() < 1e-9);
        assert!(lanes(&Data::default()).is_empty());
    }

    #[test]
    fn a_window_takes_only_its_own_days() {
        let today = NaiveDate::parse_from_str("2026-08-23", "%Y-%m-%d").unwrap();
        let by = serde_json::json!({
            "2026-08-23": { "model-x": { "cents": 250.0, "tokens": 1000.0 } },
            "2026-08-01": { "model-x": { "cents": 100.0, "tokens": 400.0 },
                            "model-y": { "cents": 500.0, "tokens": 900.0 } },
            "2026-07-01": { "model-x": { "cents": 9999.0, "tokens": 9.0 } },
        });
        let (cost, tokens, _) = window_of(&by, 1, today);
        assert!((cost - 2.50).abs() < 1e-9);
        assert!((tokens - 1000.0).abs() < 1e-9);
        // Thirty days reaches 1 August but not 1 July.
        let (cost, _, models) = window_of(&by, 30, today);
        assert!((cost - 8.50).abs() < 1e-9);
        // Costliest model first, in dollars.
        assert_eq!(models[0].0, "model-y");
        assert!((models[0].1 - 5.0).abs() < 1e-9);
    }

    #[test]
    fn paging_stops_at_the_window_not_the_history() {
        let cut = now() - 30.0 * 86400.0;
        let recent = ((now() - 3600.0) * 1000.0) as i64;
        let ancient = ((cut - 86400.0) * 1000.0) as i64;
        let events = vec![
            serde_json::json!({
                "timestamp": recent.to_string(), "model": "model-x",
                "tokenUsage": { "totalCents": "12.5",
                                "inputTokens": "100", "outputTokens": "50" },
            }),
            serde_json::json!({
                "timestamp": ancient.to_string(), "model": "model-x",
                "tokenUsage": { "totalCents": 999.0,
                                "inputTokens": 9, "outputTokens": 9 },
            }),
        ];
        let mut t = Tally::default();
        let oldest = tally_events(&events, cut, &mut t);
        // The old event is not counted, but its age is what tells the
        // caller this page reached past the window and paging can stop.
        assert_eq!(t.counted, 1);
        assert!((t.vendor_cents - 12.5).abs() < 1e-9);
        assert!((t.tokens - 150.0).abs() < 1e-9);
        assert!(oldest < cut);
    }

    #[test]
    fn the_quota_names_all_three_lanes_and_the_spend_in_dollars() {
        let p = palette();
        let start = ((now() - 10.0 * 86400.0) * 1000.0) as i64;
        let end = ((now() + 20.0 * 86400.0) * 1000.0) as i64;
        let d = Data {
            live: Some(serde_json::json!({
                "planUsage": {
                    "totalPercentUsed": 41.0, "autoPercentUsed": 12.0,
                    "apiPercentUsed": 3.0,
                    "limit": "40000", "totalSpend": "16400", "remaining": "23600",
                },
                "billingCycleStart": start.to_string(),
                "billingCycleEnd": end.to_string(),
            })),
            ..Data::default()
        };
        let joined = cursor_quota(&d, 100, &p).join("\n");
        for want in [
            "included", "auto", "api", "$164.00", "$400.00", "$236.00",
            "of the cycle gone", "resets in",
        ] {
            assert!(joined.contains(want), "missing {}", want);
        }
    }

    #[test]
    fn an_unreadable_database_says_why_instead_of_zeros() {
        let p = palette();
        let d = Data {
            why: "database is locked".into(),
            ..Data::default()
        };
        let joined = cursor_tab(&d, 90, &p).join("\n");
        assert!(joined.contains("database is locked"));
        assert!(!joined.contains("AI-WRITTEN CODE"));
    }

    #[test]
    fn the_metered_section_reports_the_gap_the_plan_covers() {
        let p = palette();
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let d = Data {
            events: Some(serde_json::json!({
                "by_day": { today.clone(): 1000.0 },
                "by_day_model": {
                    today: { "model-x": { "cents": 1000.0, "tokens": 5000.0 } },
                },
                "vendor_cents": 1000.0,
                "days": 30,
            })),
            // Connect writes the metered figure as a string too.
            spend: Some(serde_json::json!({ "totalCostCents": "400" })),
            ..Data::default()
        };
        let joined = cursor_metered(&d, 100, &p).join("\n");
        // Vendor list $10.00, Cursor metered $4.00, so the plan saved $6.00
        // - and the scope is named, because the QUOTA above it is too.
        for want in ["account-wide", "$10.00", "cursor meters", "$4.00", "the plan saves", "$6.00"]
        {
            assert!(joined.contains(want), "missing {}", want);
        }
    }

    #[test]
    fn thousands_read_as_thousands() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_000), "1,000");
        assert_eq!(commas(1_482_113), "1,482,113");
    }
}
