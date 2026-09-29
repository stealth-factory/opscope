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

//! Droid (Factory): token rate limits, or the older standard and premium
//! token pools, read with a Factory API key.
//!
//! The key comes from settings, then the environment, then `~/.factory/.env`.
//! It is sent only to Factory. Browser cookies and a WorkOS refresh are not
//! this reader.

use std::hash::{Hash, Hasher};

use opscope_core as tc;

use crate::parse::{
    format_balance, parse_factory_auth, parse_factory_billing_limits, parse_factory_dotenv,
    parse_factory_usage, FactoryAuth, FactoryBilling, FactoryPool, FactoryTokens, FactoryUsage,
    FactoryWindow,
};
use crate::shared::*;
use crate::*;

const API: &str = "https://api.factory.ai";
const APP: &str = "https://app.factory.ai";

/// Five hours, which is what the `fiveHour` key is. Weekly and monthly
/// carry no length in the body, so they get no pace.
const FIVE_HOURS: f64 = 5.0 * 3600.0;

/// The environment variable the key is read from when `factory_api_key` is
/// empty and `factory_api_key_env` names nothing.
pub const API_KEY_ENV: &str = "FACTORY_API_KEY";

const REPORT_TTL: f64 = 300.0;

#[derive(Clone, Default)]
pub struct Data {
    auth: Option<FactoryAuth>,
    billing: Option<FactoryBilling>,
    usage: Option<FactoryUsage>,
    read_at: f64,
    /// `config`, `env`, `file`, or empty when no key was set.
    source: &'static str,
    why: String,
}

/// The API key, and where it came from. The setting wins, then the named
/// variable, then the `FACTORY_API_KEY` line in `~/.factory/.env`.
pub fn api_key(cfg: &Config) -> (String, &'static str) {
    if !cfg.factory_api_key.trim().is_empty() {
        return (strip_bearer(cfg.factory_api_key.trim()), "config");
    }
    let name = if cfg.factory_api_key_env.trim().is_empty() {
        API_KEY_ENV
    } else {
        cfg.factory_api_key_env.trim()
    };
    if let Ok(value) = std::env::var(name) {
        if !value.trim().is_empty() {
            return (strip_bearer(value.trim()), "env");
        }
    }
    let path = format!("{}/.factory/.env", home());
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Some(value) = parse_factory_dotenv(&text) {
            return (strip_bearer(&value), "file");
        }
    }
    (String::new(), "")
}

fn strip_bearer(raw: &str) -> String {
    let text = raw.trim();
    if text.len() >= 7 && text[..7].eq_ignore_ascii_case("bearer ") {
        text[7..].trim().to_string()
    } else {
        text.to_string()
    }
}

fn cache_key(key: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    format!("droid:{:016x}", hasher.finish())
}

fn factory_get(url: &str, key: &str) -> Result<String, String> {
    if key.contains(['\r', '\n']) {
        return Err("the API key has a line break in it · paste it again as one line".into());
    }
    let auth = format!("Bearer {key}");
    let headers = [
        ("Accept", "application/json"),
        ("Content-Type", "application/json"),
        ("Origin", "https://app.factory.ai"),
        ("Referer", "https://app.factory.ai/"),
        ("x-factory-client", "web-app"),
        ("Authorization", auth.as_str()),
    ];
    tc::get(url, &headers, 15)
}

fn status_of(said: &str) -> Option<u16> {
    said.rsplit_once("error: ")
        .and_then(|(_, tail)| tail.split_whitespace().next())
        .and_then(|code| code.parse().ok())
}

fn rejected(said: &str) -> bool {
    status_of(said).is_some_and(|code| code == 401 || code == 403)
}

/// Auth on the API host, then the app host when the API host failed for a
/// reason other than 401 or 403. An API-host 401 stays a bad key: trying the
/// app host afterwards would hide it behind a later 404.
fn fetch_auth(key: &str) -> Result<Option<String>, String> {
    match factory_get(&format!("{API}/api/app/auth/me"), key) {
        Ok(text) => Ok(Some(text)),
        Err(said) if rejected(&said) => Err("Factory rejected the API key.".into()),
        Err(_) => match factory_get(&format!("{APP}/api/app/auth/me"), key) {
            Ok(text) => Ok(Some(text)),
            Err(said) if rejected(&said) => Err("Factory rejected the API key.".into()),
            Err(_) => Ok(None),
        },
    }
}

fn usage_url(host: &str, user_id: Option<&str>) -> String {
    let mut url = format!("{host}/api/organization/subscription/usage?useCache=true");
    // The id is a query parameter only when auth sent one. The API key is
    // not decoded to invent a subject.
    if let Some(id) = user_id {
        if !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            url.push_str("&userId=");
            url.push_str(id);
        }
    }
    url
}

fn ask(key: &str) -> Result<serde_json::Value, String> {
    let auth = fetch_auth(key)?;
    let user_id = auth
        .as_deref()
        .and_then(parse_factory_auth)
        .and_then(|auth| auth.user_id);
    let limits = match factory_get(&format!("{API}/api/billing/limits"), key) {
        Ok(text) => text,
        Err(said) if rejected(&said) => return Err("Factory rejected the API key.".into()),
        Err(_) => String::new(),
    };
    if parse_factory_billing_limits(&limits).is_some() {
        return Ok(serde_json::json!({
            "auth": auth.unwrap_or_default(),
            "limits": limits,
            "usage": "",
            "at": now(),
        }));
    }
    let usage = match factory_get(&usage_url(API, user_id.as_deref()), key) {
        Ok(text) => text,
        Err(said) if rejected(&said) => return Err("Factory rejected the API key.".into()),
        // A 401 on the API host already returned. Any other failure gets one
        // try on the app host, and that answer is the one the tab reports.
        Err(_) => match factory_get(&usage_url(APP, user_id.as_deref()), key) {
            Ok(text) => text,
            Err(said) if rejected(&said) => return Err("Factory rejected the API key.".into()),
            Err(said) => return Err(format!("usage {}", refusal(&said))),
        },
    };
    if parse_factory_usage(&usage).is_none() {
        return Err("Factory's answer had no usage window.".into());
    }
    Ok(serde_json::json!({
        "auth": auth.unwrap_or_default(),
        "limits": "",
        "usage": usage,
        "at": now(),
    }))
}

/// `shown` is whether Droid has a tab the reader chose.
pub fn read(caches: &mut Caches, cfg: &Config, shown: bool) -> Data {
    let mut data = Data::default();
    if !shown {
        data.why = "not asked · Droid is left out of this widget's agents".into();
        return data;
    }
    let (key, source) = api_key(cfg);
    data.source = source;
    if key.is_empty() {
        return data;
    }
    let slot = cache_key(&key);
    let mut refused = String::new();
    let mut got = None;
    for _ in 0..2 {
        got = cached(caches, &slot, REPORT_TTL, || match ask(&key) {
            Ok(value) => Some(value),
            Err(why) => {
                refused = why;
                None
            }
        });
        if !got.as_ref().is_some_and(reset_passed) {
            break;
        }
        caches.live.remove(&slot);
    }
    remember_refusal(caches, &slot, &refused);
    if !refused.is_empty() {
        data.why = refused;
        return data;
    }
    let Some(got) = got else {
        return data;
    };
    data.why = text(&got, "why");
    data.auth = parse_factory_auth(&text(&got, "auth"));
    data.billing = parse_factory_billing_limits(&text(&got, "limits"));
    if data.billing.is_none() {
        data.usage = parse_factory_usage(&text(&got, "usage"));
    }
    data.read_at = num(&got, "at");
    data
}

fn reset_passed(got: &serde_json::Value) -> bool {
    let mut data = Data {
        auth: parse_factory_auth(&text(got, "auth")),
        billing: parse_factory_billing_limits(&text(got, "limits")),
        read_at: num(got, "at"),
        ..Data::default()
    };
    if data.billing.is_none() {
        data.usage = parse_factory_usage(&text(got, "usage"));
    }
    lanes(&data)
        .iter()
        .any(|lane| lane.reset.is_some_and(|reset| reset <= now()))
}

/// The windows that were sent. A 5h lane is the one length the key name
/// states. Weekly, monthly, and the legacy pools have a reset and no pace.
/// Core lanes sit apart from the standard group.
pub fn lanes(data: &Data) -> Vec<Lane> {
    if let Some(billing) = data.billing.as_ref() {
        let mut out = pool_lanes(&billing.standard, "", false, data.read_at);
        if let Some(core) = &billing.core {
            out.extend(pool_lanes(core, "core", true, data.read_at));
        }
        return out;
    }
    let Some(usage) = data.usage.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(pct) = usage.standard.pct {
        out.push(token_lane("standard", pct, usage.end));
    }
    if let Some(pct) = usage.premium.pct {
        out.push(token_lane("premium", pct, usage.end));
    }
    out
}

fn pool_lanes(pool: &FactoryPool, prefix: &str, apart_first: bool, read_at: f64) -> Vec<Lane> {
    let specs = [
        ("5h", pool.five_hour.as_ref(), Some(FIVE_HOURS)),
        ("weekly", pool.weekly.as_ref(), None),
        ("monthly", pool.monthly.as_ref(), None),
    ];
    let mut out = Vec::new();
    for (label, window, window_secs) in specs {
        let Some(window) = window else {
            continue;
        };
        let reset = factory_reset(window, read_at);
        out.push(Lane {
            label: if prefix.is_empty() {
                label.into()
            } else {
                format!("{prefix} {label}")
            },
            pct: window.pct,
            window_secs,
            reset,
            stale: reset.is_some_and(|reset| reset <= now()),
            projected: false,
            apart: apart_first && out.is_empty(),
        });
    }
    out
}

/// `secondsRemaining` counts from the reading. A `windowEnd` that was
/// already past then contributes no countdown, and does not change `pct`.
fn factory_reset(window: &FactoryWindow, read_at: f64) -> Option<f64> {
    if let Some(seconds) = window.seconds_remaining {
        return Some(read_at + seconds);
    }
    window.window_end.filter(|end| *end > read_at)
}

fn token_lane(label: &str, pct: f64, end: Option<f64>) -> Lane {
    Lane {
        label: label.into(),
        pct,
        window_secs: None,
        reset: end,
        stale: end.is_some_and(|end| end <= now()),
        projected: false,
        apart: false,
    }
}

pub fn why_no_lane(data: &Data) -> String {
    if !lanes(data).is_empty() {
        return String::new();
    }
    if data.usage.as_ref().is_some_and(no_token_percent) && data.why.is_empty() {
        return "no quota · Factory answered, and published no ratio and no allowance.".into();
    }
    if !data.why.is_empty() {
        return format!("no quota · {}", data.why);
    }
    if data.source.is_empty() {
        return format!(
            "no quota · no Factory API key · set factory_api_key, {API_KEY_ENV}, or ~/.factory/.env."
        );
    }
    "no quota · no reading from Factory yet.".into()
}

fn no_token_percent(usage: &FactoryUsage) -> bool {
    usage.standard.pct.is_none()
        && usage.premium.pct.is_none()
        && !usage.standard.unlimited
        && !usage.premium.unlimited
}

pub fn tab(data: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    if let Some(billing) = data.billing.as_ref() {
        let mut rows = rate_rows(data, billing, w, p);
        rows.extend(token_warning(data, w, p));
        return add_section(
            rows,
            account_rows(data, billing.overage_preference.as_deref(), w, p),
        );
    }
    if let Some(usage) = data.usage.as_ref() {
        let mut rows = legacy_rows(data, usage, w, p);
        rows.extend(token_warning(data, w, p));
        return add_section(rows, account_rows(data, None, w, p));
    }
    let what = if data.source.is_empty() && data.why.is_empty() {
        format!(
            "No Factory API key. Set factory_api_key in config.json, export {API_KEY_ENV}, or \
             put FACTORY_API_KEY in ~/.factory/.env. It is sent only to api.factory.ai, and only \
             while Droid is one of the agents shown. Nothing is read from a browser. Keep \
             config.json chmod 600."
        )
    } else {
        why_no_lane(data)
            .trim_start_matches("no quota · ")
            .to_string()
    };
    let mut rows = no_local(&what, "", w, p);
    rows.extend(token_warning(data, w, p));
    rows
}

fn rate_rows(data: &Data, billing: &FactoryBilling, w: usize, p: &Palette) -> Vec<String> {
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── QUOTA ── ".into()),
            (p.ok.as_str(), "live".into()),
            (p.dim.as_str(), " · token rate limits · account-wide".into()),
        ],
        w - 1,
    )];
    let hue = agent_hue("droid");
    let all = lanes(data);
    let standard: Vec<&Lane> = all
        .iter()
        .filter(|lane| !lane.label.starts_with("core "))
        .collect();
    let core: Vec<&Lane> = all
        .iter()
        .filter(|lane| lane.label.starts_with("core "))
        .collect();
    rows.extend(draw_lanes(&standard, hue, w, p));
    rows.extend(note(
        "Factory did not send a length for the weekly or monthly window, so no pace is drawn for them.",
        &p.dim,
        w,
    ));
    if !core.is_empty() {
        rows.push(String::new());
        rows.push(tc::seg(&[(p.lbl.as_str(), " ── CORE ── ".into())], w - 1));
        rows.extend(draw_lanes(&core, hue, w, p));
    }
    if let Some(balance) = billing.balance {
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "  extra usage balance  ".into()),
                (p.txt.as_str(), format_balance(balance)),
            ],
            w - 1,
        ));
    }
    rows
}

fn legacy_rows(data: &Data, usage: &FactoryUsage, w: usize, p: &Palette) -> Vec<String> {
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── QUOTA ── ".into()),
            (p.ok.as_str(), "live".into()),
            (
                p.dim.as_str(),
                " · standard and premium tokens · account-wide".into(),
            ),
        ],
        w - 1,
    )];
    let hue = agent_hue("droid");
    rows.extend(draw_lanes(
        &lanes(data).iter().collect::<Vec<_>>(),
        hue,
        w,
        p,
    ));
    for (label, pool) in [("standard", &usage.standard), ("premium", &usage.premium)] {
        rows.extend(pool_note(label, pool, w, p));
    }
    rows.extend(org_row("org standard", usage.standard.org_tokens, w, p));
    rows.extend(org_row("org premium", usage.premium.org_tokens, w, p));
    rows
}

fn pool_note(label: &str, pool: &FactoryTokens, w: usize, p: &Palette) -> Vec<String> {
    if pool.unlimited {
        return note(
            &format!("{label} is unlimited · no denominator, so no bar."),
            &p.dim,
            w,
        );
    }
    if pool.pct.is_none() {
        return note(
            &format!(
                "Factory's answer had no ratio and no allowance for {label}, so it is not shown."
            ),
            &p.warn,
            w,
        );
    }
    Vec::new()
}

fn org_row(label: &str, tokens: Option<f64>, w: usize, p: &Palette) -> Vec<String> {
    let Some(tokens) = tokens else {
        return Vec::new();
    };
    vec![tc::seg(
        &[
            (p.dim.as_str(), format!("  {label}  ")),
            (p.txt.as_str(), format!("{} tokens", big_num(tokens))),
        ],
        w - 1,
    )]
}

fn draw_lanes(lanes: &[&Lane], hue: Option<(u8, u8, u8)>, w: usize, p: &Palette) -> Vec<String> {
    let label_w = lanes
        .iter()
        .map(|lane| short_label(&lane.label).chars().count())
        .max()
        .unwrap_or(7)
        .max(7);
    let mut rows = Vec::new();
    for lane in lanes {
        let shown = short_label(&lane.label);
        let used = (lane.pct / 100.0).clamp(0.0, 1.0);
        let room = ((w as i64) - 38 - label_w as i64).max(8) as usize;
        let mut line: Vec<(String, String)> =
            vec![(p.dim.clone(), format!(" {} ", tc::pad(shown, label_w)))];
        line.extend(paced_bar(
            used,
            elapsed_of(lane.window_secs, lane.reset),
            room,
            hue,
            p,
        ));
        line.push((pct_colour(lane.pct, hue, p), pct_text(lane.pct)));
        line.push(pace_cell(lead(lane.pct, lane.window_secs, lane.reset), p));
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&refs, w - 1));
        let when = match lane.reset.map(|reset| reset - now()) {
            Some(left) if left > 0.0 => format!("resets in {}", left_span(left)),
            Some(_) => "resetting".into(),
            None => String::new(),
        };
        if !when.is_empty() {
            rows.push(tc::seg(
                &[(
                    p.dim.as_str(),
                    format!(" {}  {}", " ".repeat(label_w), when),
                )],
                w - 1,
            ));
        }
    }
    rows
}

fn short_label(label: &str) -> &str {
    label.strip_prefix("core ").unwrap_or(label)
}

fn account_rows(data: &Data, preference: Option<&str>, w: usize, p: &Palette) -> Vec<String> {
    let Some(auth) = data.auth.as_ref() else {
        return preference_only(preference, w, p);
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    if let Some(tier) = &auth.tier {
        pairs.push(("tier".into(), tier.clone()));
    }
    // A plan whose name already says Factory repeats the product. The tier
    // and the organization are the rest of the line.
    if let Some(plan) = auth
        .plan
        .as_ref()
        .filter(|plan| !plan.to_lowercase().contains("factory"))
    {
        pairs.push(("plan".into(), plan.clone()));
    }
    if let Some(org) = &auth.org {
        pairs.push(("org".into(), org.clone()));
    }
    if let Some(preference) = preference.filter(|text| !text.is_empty()) {
        pairs.push(("fallback".into(), preference.to_string()));
    }
    if pairs.is_empty() {
        return Vec::new();
    }
    let headline = pairs[0].1.clone();
    plan_rows(&headline, &pairs, w, "", None, "", p)
}

fn preference_only(preference: Option<&str>, w: usize, p: &Palette) -> Vec<String> {
    let Some(preference) = preference.filter(|text| !text.is_empty()) else {
        return Vec::new();
    };
    plan_rows(
        preference,
        &[("fallback".into(), preference.to_string())],
        w,
        "",
        None,
        "",
        p,
    )
}

fn token_warning(data: &Data, w: usize, p: &Palette) -> Vec<String> {
    tc::config_token_warning()
        .filter(|_| data.source == "config")
        .map(|warn| note(&warn, &p.warn, w))
        .unwrap_or_default()
}

fn note(text: &str, colour: &str, w: usize) -> Vec<String> {
    wrap_text(text, w.saturating_sub(4).max(20))
        .into_iter()
        .map(|line| tc::seg(&[(colour, format!("  {line}"))], w - 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(rows: &[String]) -> String {
        let mut out = String::new();
        for row in rows {
            let mut chars = row.chars();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    for c in chars.by_ref() {
                        if c == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out.push('\n');
        }
        out
    }

    fn limits(extra: &str) -> String {
        format!(
            r#"{{"usesTokenRateLimitsBilling":true,{extra}
            "limits":{{"standard":{{
                "fiveHour":{{"usedPercent":12,"secondsRemaining":9000}},
                "weekly":{{"usedPercent":30,"secondsRemaining":100000}},
                "monthly":{{"usedPercent":4,"windowEnd":1600000000}}}}}}}}"#
        )
    }

    fn reading_limits(raw: &str) -> Data {
        Data {
            billing: parse_factory_billing_limits(raw),
            read_at: now() - 60.0,
            source: "config",
            ..Data::default()
        }
    }

    #[test]
    fn rate_limits_pace_only_the_five_hour_window_and_keep_a_closed_percent() {
        let data = reading_limits(&limits(
            r#""extraUsageBalanceCents":250,"overagePreference":"on_demand","#,
        ));
        let lanes = lanes(&data);
        assert_eq!(
            lanes
                .iter()
                .map(|lane| lane.label.as_str())
                .collect::<Vec<_>>(),
            vec!["5h", "weekly", "monthly"]
        );
        assert_eq!(lanes[0].window_secs, Some(FIVE_HOURS));
        assert!(lanes[1].window_secs.is_none() && lanes[2].window_secs.is_none());
        assert_eq!(lanes[0].pct, 12.0);
        // The monthly windowEnd is in the past and secondsRemaining was not
        // sent. The 4% stays, and there is no countdown to invent from it.
        assert_eq!(lanes[2].pct, 4.0);
        assert!(lanes[2].reset.is_none());
        let rows = plain(&tab(&data, 100, 40, &Config::default(), &palette()));
        assert!(rows.contains("token rate limits"), "{rows}");
        assert!(rows.contains("extra usage balance  2.5"), "{rows}");
        assert!(!rows.contains('$'), "{rows}");
        assert!(!rows.contains("── CORE ──"), "{rows}");
        assert!(rows.contains("no pace is drawn"), "{rows}");
    }

    #[test]
    fn a_core_window_the_server_left_out_is_not_a_zero_bar() {
        let raw = r#"{"usesTokenRateLimitsBilling":true,"limits":{"standard":{
            "fiveHour":{"usedPercent":1,"secondsRemaining":100},
            "weekly":{"usedPercent":1,"secondsRemaining":100},
            "monthly":{"usedPercent":1,"secondsRemaining":100}},
            "core":{"weekly":{"usedPercent":55,"secondsRemaining":100}}}}"#;
        let data = reading_limits(raw);
        let lanes = lanes(&data);
        assert!(lanes
            .iter()
            .any(|lane| lane.label == "core weekly" && lane.pct == 55.0));
        assert!(lanes
            .iter()
            .any(|lane| lane.label == "core weekly" && lane.apart));
        assert!(!lanes.iter().any(|lane| lane.label.contains("core 5h")));
        assert!(!lanes.iter().any(|lane| lane.label.contains("core monthly")));
        let rows = plain(&tab(&data, 100, 40, &Config::default(), &palette()));
        assert!(rows.contains("── CORE ──"), "{rows}");
        assert!(rows.contains("55%"), "{rows}");
        let words = rows.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            !words.contains("core 5h") && !rows.contains("  0%"),
            "{rows}"
        );
    }

    #[test]
    fn a_missing_balance_key_is_not_drawn_as_zero() {
        let data = reading_limits(&limits(""));
        assert!(data.billing.as_ref().unwrap().balance.is_none());
        let rows = plain(&tab(&data, 90, 30, &Config::default(), &palette()));
        assert!(!rows.contains("extra usage balance"), "{rows}");
    }

    #[test]
    fn legacy_tokens_name_the_body_and_leave_an_absent_pool_off_the_bar() {
        let raw = r#"{"usage":{"endDate":4102444800000,
            "standard":{"userTokens":10,"totalAllowance":100,"usedRatio":0.10,"orgTotalTokensUsed":4000},
            "premium":{"userTokens":1,"totalAllowance":2000000000000}}}"#;
        let data = Data {
            usage: parse_factory_usage(raw),
            auth: parse_factory_auth(
                r#"{"organization":{"name":"Acme","subscription":{"factoryTier":"pro",
                    "orbSubscription":{"plan":{"name":"Factory Pro"}}}}}"#,
            ),
            read_at: now() - 30.0,
            source: "env",
            ..Data::default()
        };
        let lanes = lanes(&data);
        assert_eq!(lanes.len(), 1);
        assert_eq!((lanes[0].label.as_str(), lanes[0].pct), ("standard", 10.0));
        assert!(lanes[0].window_secs.is_none());
        let rows = plain(&tab(&data, 100, 40, &Config::default(), &palette()));
        assert!(rows.contains("standard and premium tokens"), "{rows}");
        assert!(!rows.contains("token rate limits"), "{rows}");
        assert!(
            rows.contains("no ratio and no allowance") || rows.contains("unlimited"),
            "{rows}"
        );
        assert!(rows.contains("unlimited"), "{rows}");
        assert!(
            rows.contains("org standard") && rows.contains("tokens"),
            "{rows}"
        );
        assert!(!rows.contains("extra usage balance"), "{rows}");
        assert!(!rows.contains('$'), "{rows}");
        assert!(rows.contains("tier") && rows.contains("pro"), "{rows}");
        assert!(rows.contains("Acme"), "{rows}");
        // The plan name is the product again. It is not a second tier.
        assert!(!rows.contains("Factory Pro"), "{rows}");
    }

    #[test]
    fn an_answer_with_no_ratio_and_no_allowance_is_not_a_zero() {
        let data = Data {
            usage: parse_factory_usage(r#"{"usage":{"standard":{},"premium":{}}}"#),
            read_at: now(),
            source: "env",
            ..Data::default()
        };
        assert!(lanes(&data).is_empty());
        let note = why_no_lane(&data);
        assert!(note.contains("answered, and published no"), "{note}");
        let rows = plain(&tab(&data, 80, 24, &Config::default(), &palette()));
        assert!(!rows.contains("0%"), "{rows}");
    }

    #[test]
    fn with_no_key_nothing_is_asked() {
        let cfg = Config {
            factory_api_key_env: "OPSCOPE_TEST_NO_SUCH_VARIABLE".into(),
            ..Config::default()
        };
        let mut caches = Caches::default();
        // Point the dotenv read at a home that has no Factory file by
        // relying on the process home. The test only asserts that an empty
        // setting and a missing variable do not open a cache slot when the
        // file is also absent. When the file is present, a key is real and
        // the assertion below is the wrong one — skip the cache check then.
        let data = read(&mut caches, &cfg, true);
        if data.source != "file" {
            assert!(caches.live.keys().all(|key| !key.starts_with("droid")));
            assert!(why_no_lane(&data).contains("factory_api_key"));
        }
    }

    #[test]
    fn a_droid_without_a_tab_is_never_asked() {
        let cfg = Config {
            factory_api_key: "secret".into(),
            ..Config::default()
        };
        let mut caches = Caches::default();
        let data = read(&mut caches, &cfg, false);
        assert!(caches.live.keys().all(|key| !key.starts_with("droid")));
        assert!(why_no_lane(&data).contains("left out"));
    }

    #[test]
    fn the_setting_wins_over_the_environment_name() {
        let cfg = Config {
            factory_api_key: " abc ".into(),
            factory_api_key_env: "OPSCOPE_TEST_NO_SUCH_VARIABLE".into(),
            ..Config::default()
        };
        assert_eq!(api_key(&cfg).0, "abc");
        assert_eq!(api_key(&cfg).1, "config");
    }

    #[test]
    fn a_user_id_is_a_query_parameter_and_not_taken_from_the_key() {
        assert_eq!(
            usage_url(API, Some("user-1")),
            "https://api.factory.ai/api/organization/subscription/usage?useCache=true&userId=user-1"
        );
        assert!(!usage_url(API, None).contains("userId"));
        assert!(!usage_url(API, Some("a b")).contains("userId"));
    }
}
