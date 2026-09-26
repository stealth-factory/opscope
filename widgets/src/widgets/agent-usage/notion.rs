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

//! Notion AI: the usage allowance on Business and Enterprise workspaces.
//!
//! Notion publishes no API for this. Its web app reads the allowance from
//! its own internal `/api/v3` endpoints, signed in with the `token_v2`
//! session cookie, and this asks the same two: `getSpaces` for the
//! workspace, `getCreditRateLimitStatus` for the allowance. They are not a
//! supported interface and may change without notice; when one does, the
//! tab says what it got rather than drawing an empty gauge.
//!
//! The cookie is the reader's to give, in `config.json` or an environment
//! variable. Nothing here reads a browser's cookie store or the Notion app's:
//! a program that reaches into those is a different kind of program from
//! this one (see the README), and a session copied by hand is at least one
//! the reader chose to hand over.

use chrono::{DateTime, Months, Utc};
use opscope_core as tc;

use crate::parse::{
    notion_span_secs, parse_notion_allowance, parse_notion_spaces, pick_notion_space,
    NotionAllowance, NotionSpace,
};
use crate::shared::*;
use crate::*;

const API: &str = "https://app.notion.com/api/v3/";

/// The environment variable the cookie is read from when `notion_token` is
/// empty and `notion_token_env` names nothing. One constant, so the code
/// and `settings.json` cannot drift.
pub const TOKEN_ENV: &str = "NOTION_TOKEN_V2";

/// How long a reading is held. The rolling window is six hours, so five
/// minutes keeps its bar honest without asking Notion on every refresh.
const REPORT_TTL: f64 = 300.0;

#[derive(Clone, Default)]
pub struct Data {
    allowance: Option<NotionAllowance>,
    space: Option<NotionSpace>,
    email: String,
    /// When the reading was taken. `resetsInSeconds` counts from then.
    read_at: f64,
    /// Where the cookie came from: `config`, `env`, or empty for none.
    source: &'static str,
    why: String,
}

/// The cookie, and where it came from. `config` wins over the environment.
pub fn token(cfg: &Config) -> (String, &'static str) {
    if !cfg.notion_token.trim().is_empty() {
        return (cfg.notion_token.trim().to_string(), "config");
    }
    let name = if cfg.notion_token_env.trim().is_empty() {
        TOKEN_ENV
    } else {
        cfg.notion_token_env.trim()
    };
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => (v.trim().to_string(), "env"),
        _ => (String::new(), ""),
    }
}

/// The `Cookie` header for a pasted value: the bare `token_v2`, or a whole
/// cookie header copied from the browser, which is sent as it is.
fn cookie_header(token: &str) -> String {
    if token.contains("token_v2=") {
        token.trim_start_matches("Cookie:").trim().to_string()
    } else {
        format!("token_v2={}", token)
    }
}

/// One POST to the web app's API.
///
/// The headers are the ones Notion's own web client sends, which is the
/// only shape these endpoints are known to answer. The cookie reaches curl
/// on its standard input, never on its command line.
fn post(endpoint: &str, body: &str, cookie: &str) -> Result<String, String> {
    let headers = [
        ("Content-Type", "application/json"),
        ("Accept", "*/*"),
        ("Origin", "https://app.notion.com"),
        ("Referer", "https://app.notion.com/"),
        ("Sec-Fetch-Dest", "empty"),
        ("Sec-Fetch-Mode", "cors"),
        ("Sec-Fetch-Site", "same-origin"),
        ("Cookie", cookie),
    ];
    tc::post_json(&format!("{}{}", API, endpoint), &headers, body, 15)
        .map(|(text, _)| text)
        .map_err(|said| {
            if said.starts_with("HTTP 401") || said.starts_with("HTTP 403") {
                "token rejected · copy a fresh token_v2 from app.notion.com".to_string()
            } else {
                format!("{} {}", endpoint, refusal(&said))
            }
        })
}

/// Both requests, as the raw answers, or why there was no reading.
fn ask(token: &str, wanted: &str) -> Result<serde_json::Value, String> {
    let cookie = cookie_header(token);
    let spaces = post("getSpaces", "{}", &cookie)?;
    let (_, list) = parse_notion_spaces(&spaces)
        .ok_or("getSpaces answered in a shape this widget cannot read")?;
    let space = pick_notion_space(&list, wanted).ok_or("this Notion account has no workspace")?;
    let body = serde_json::json!({ "spaceId": space.id }).to_string();
    let status = post("getCreditRateLimitStatus", &body, &cookie)?;
    if parse_notion_allowance(&status).is_none() {
        return Err("getCreditRateLimitStatus answered with no usage window".into());
    }
    Ok(serde_json::json!({ "spaces": spaces, "status": status, "space": space.id, "at": now() }))
}

/// `shown` is whether Notion has a tab under the reader's settings; with no
/// tab, or no cookie, nothing is sent.
pub fn read(caches: &mut Caches, cfg: &Config, shown: bool) -> Data {
    let mut d = Data::default();
    if !shown {
        d.why = "not asked · Notion is left out of this widget's agents".into();
        return d;
    }
    let (token, source) = token(cfg);
    d.source = source;
    if token.is_empty() {
        return d;
    }
    let wanted = cfg.notion_workspace.clone();
    let mut refused = String::new();
    let got = cached(caches, "notion", REPORT_TTL, || match ask(&token, &wanted) {
        Ok(v) => Some(v),
        Err(why) => {
            refused = why;
            None
        }
    });
    remember_refusal(caches, "notion", &refused);
    if !refused.is_empty() {
        d.why = refused;
        return d;
    }
    let Some(got) = got else {
        return d;
    };
    d.why = text(&got, "why");
    if let Some((email, list)) = parse_notion_spaces(&text(&got, "spaces")) {
        d.email = email;
        d.space = list.into_iter().find(|s| s.id == text(&got, "space"));
    }
    d.allowance = parse_notion_allowance(&text(&got, "status"));
    d.read_at = num(&got, "at");
    d
}

/// The billing period's length: the calendar month ending where Notion says
/// it ends. Notion gives only the end, and a flat thirty days would put the
/// pace mark in the wrong place in every month that is not thirty days long.
fn period_secs(ends: f64) -> Option<f64> {
    let end: DateTime<Utc> = DateTime::from_timestamp(ends as i64, 0)?;
    let start = end.checked_sub_months(Months::new(1))?;
    Some((end - start).num_seconds() as f64)
}

/// The rolling window and the billing period, each against its own limit.
pub fn lanes(d: &Data) -> Vec<Lane> {
    let Some(a) = d.allowance.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(w) = &a.rolling {
        out.push(Lane {
            label: if w.span.is_empty() { "rolling".into() } else { w.span.clone() },
            pct: w.pct(),
            window_secs: notion_span_secs(&w.span),
            reset: a.resets_in.map(|s| d.read_at + s),
            stale: false,
            projected: false,
            apart: false,
        });
    }
    if let Some(w) = &a.period {
        out.push(Lane {
            label: "month".into(),
            pct: w.pct(),
            window_secs: w.ends.and_then(period_secs),
            reset: w.ends,
            stale: false,
            projected: false,
            apart: false,
        });
    }
    out
}

/// Why `[+]` has no bar for Notion.
pub fn why_no_lane(d: &Data) -> String {
    if !lanes(d).is_empty() {
        return String::new();
    }
    if d.allowance.as_ref().is_some_and(|a| a.not_applicable()) {
        let name = d.space.as_ref().map(|s| s.name.as_str()).unwrap_or("this workspace");
        return format!(
            "no quota · Notion answered, and published no limit for {} · the allowance is \
             on Business and Enterprise plans.",
            name
        );
    }
    if !d.why.is_empty() {
        return format!("no quota · {}", d.why);
    }
    if d.source.is_empty() {
        return format!("no quota · no token · set notion_token or {}.", TOKEN_ENV);
    }
    "no quota · no reading from Notion yet.".into()
}

fn allowance_rows(d: &Data, a: &NotionAllowance, w: usize, p: &Palette) -> Vec<String> {
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── NOTION AI ── ".into()),
            (p.ok.as_str(), "live".into()),
            (p.dim.as_str(), format!(" · per member · read {} ago", ago(d.read_at))),
        ],
        w - 1,
    )];
    let hue = agent_hue("notion");
    let label_w = 7;
    for lane in lanes(d) {
        let used = (lane.pct / 100.0).clamp(0.0, 1.0);
        let room = ((w as i64) - 38 - label_w as i64).max(8) as usize;
        let mut line: Vec<(String, String)> =
            vec![(p.dim.clone(), format!(" {} ", tc::pad(&lane.label, label_w)))];
        line.extend(paced_bar(used, elapsed_of(lane.window_secs, lane.reset), room, hue, p));
        line.push((pct_colour(lane.pct, hue, p), pct_text(lane.pct)));
        line.push(pace_cell(lead(lane.pct, lane.window_secs, lane.reset), p));
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&refs, w - 1));
        let when = match lane.reset.map(|r| r - now()) {
            Some(left) if left > 0.0 => format!("resets in {}", left_span(left)),
            Some(_) => "resetting".into(),
            None => String::new(),
        };
        if !when.is_empty() {
            rows.push(tc::seg(
                &[(p.dim.as_str(), format!(" {}  {}", " ".repeat(label_w), when))],
                w - 1,
            ));
        }
    }
    if a.enforcement.eq_ignore_ascii_case("preview") {
        rows.push(tc::seg(
            &[(p.dim.as_str(), "  Notion reports this allowance as a preview, not yet enforced.".into())],
            w - 1,
        ));
    }
    rows
}

fn account_rows(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    if let Some(s) = &d.space {
        if !s.name.is_empty() {
            pairs.push(("workspace".into(), s.name.clone()));
        }
    }
    if !d.email.is_empty() {
        pairs.push(("account".into(), d.email.clone()));
    }
    let tier = d.space.as_ref().map(|s| s.tier.as_str()).unwrap_or("");
    plan_rows(tier, &pairs, w, "", None, "", p)
}

pub fn tab(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    match d.allowance.as_ref() {
        Some(a) if !a.not_applicable() => {
            add_section(allowance_rows(d, a, w, p), account_rows(d, w, p))
        }
        _ => {
            let what = if d.source.is_empty() && d.why.is_empty() {
                format!(
                    "No Notion token. Notion publishes no API for AI usage, so this reads the \
                     one its web app uses, signed in with your token_v2 cookie. Copy it from \
                     app.notion.com (developer tools, cookies) into notion_token in \
                     config.json, or into {}. It is a full sign-in to your Notion account, \
                     so keep config.json chmod 600.",
                    TOKEN_ENV
                )
            } else {
                why_no_lane(d).trim_start_matches("no quota · ").to_string()
            };
            let mut rows = no_local(&what, "", w, p);
            if let Some(warn) = tc::config_token_warning().filter(|_| d.source == "config") {
                rows.push(tc::seg(&[(p.warn.as_str(), format!("  {}", warn))], w - 1));
            }
            add_section(rows, if d.space.is_some() { account_rows(d, w, p) } else { Vec::new() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = r#"{"status":"within_limit",
        "window":{"window":"6h","used":42.5,"limit":100},"resetsInSeconds":12600,
        "billingPeriodWindow":{"used":18,"limit":100,"periodEndMs":1788000000000},
        "enforcement":"enforced"}"#;

    fn reading() -> Data {
        Data {
            allowance: parse_notion_allowance(STATUS),
            space: Some(NotionSpace {
                id: "s".into(),
                name: "Acme".into(),
                tier: "business".into(),
            }),
            email: "person@example.com".into(),
            read_at: now() - 60.0,
            source: "config",
            why: String::new(),
        }
    }

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

    #[test]
    fn a_reading_is_two_lanes_each_on_its_own_clock() {
        let d = reading();
        let lanes = lanes(&d);
        assert_eq!(lanes.len(), 2);
        assert_eq!((lanes[0].label.as_str(), lanes[0].pct), ("6h", 42.5));
        assert_eq!(lanes[0].window_secs, Some(21600.0));
        // The reset counts from when Notion was asked, not from now.
        assert_eq!(lanes[0].reset, Some(d.read_at + 12600.0));
        assert_eq!((lanes[1].label.as_str(), lanes[1].pct), ("month", 18.0));
        assert_eq!(lanes[1].reset, Some(1_788_000_000.0));
        // 2026-08-29 to 2026-09-29 is 31 days, not a flat 30.
        assert_eq!(lanes[1].window_secs, Some(31.0 * 86400.0));
        assert!(why_no_lane(&d).is_empty());
    }

    #[test]
    fn a_workspace_with_no_allowance_is_named_without_a_warning() {
        let mut d = reading();
        d.allowance = parse_notion_allowance(r#"{"status":"not_applicable"}"#);
        d.space.as_mut().unwrap().name = "Personal".into();
        assert!(lanes(&d).is_empty());
        let note = why_no_lane(&d);
        assert!(note.contains("Personal") && note.contains("Business"), "{note}");
        // The wording `[+]` reads to decide this is not the reader's to fix.
        assert!(note.contains("answered, and published no"), "{note}");
    }

    #[test]
    fn with_no_token_nothing_is_asked_and_the_tab_says_where_one_goes() {
        let cfg = Config {
            notion_token_env: "OPSCOPE_TEST_NO_SUCH_VARIABLE".into(),
            ..Config::default()
        };
        let mut caches = Caches::default();
        let d = read(&mut caches, &cfg, true);
        assert!(!caches.live.contains_key("notion"));
        assert!(why_no_lane(&d).contains("notion_token"));
        let all = plain(&tab(&d, 60, 20, &cfg, &palette()));
        assert!(all.contains("notion_token") && all.contains("token_v2"), "{all}");
    }

    #[test]
    fn a_notion_without_a_tab_is_never_asked() {
        let cfg = Config { notion_token: "secret".into(), ..Config::default() };
        let mut caches = Caches::default();
        let d = read(&mut caches, &cfg, false);
        assert!(!caches.live.contains_key("notion"));
        assert!(why_no_lane(&d).contains("left out"));
    }

    #[test]
    fn the_token_comes_from_config_before_the_environment() {
        let cfg = Config { notion_token: " abc ".into(), ..Config::default() };
        assert_eq!(token(&cfg), ("abc".to_string(), "config"));
        let none = Config {
            notion_token_env: "OPSCOPE_TEST_NO_SUCH_VARIABLE".into(),
            ..Config::default()
        };
        assert_eq!(token(&none), (String::new(), ""));
    }

    #[test]
    fn a_bare_token_and_a_copied_cookie_header_both_sign_in() {
        assert_eq!(cookie_header("abc"), "token_v2=abc");
        assert_eq!(cookie_header("Cookie: a=1; token_v2=abc"), "a=1; token_v2=abc");
        assert_eq!(cookie_header("token_v2=abc; b=2"), "token_v2=abc; b=2");
    }

    #[test]
    fn the_tab_fits_the_pane_at_every_width() {
        let d = reading();
        for w in [30usize, 40, 60, 100] {
            let rows = plain(&tab(&d, w, 30, &Config::default(), &palette()));
            for r in rows.lines() {
                assert!(tc::display_width(r) <= w - 1, "width {w}: {r:?}");
            }
            if w >= 60 {
                assert!(rows.contains("Acme") && rows.contains("resets in"), "{rows}");
            }
        }
    }
}
