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

//! Perplexity: the account's credit pools.
//!
//! Perplexity publishes no API for this. Its account page reads the pools
//! from `/rest/billing/credits`, signed in with the web session cookie, and
//! this asks the same endpoint, as CodexBar does. It is not a supported
//! interface and may change without notice; when it does, the tab says what
//! it got rather than drawing an empty gauge.
//!
//! The cookie is the reader's to give, in `config.json` or an environment
//! variable. Nothing here reads a browser's cookie store, on the same
//! terms as Notion: a program that reaches into those is a different kind
//! of program from this one, and a session copied by hand is at least one
//! the reader chose to hand over.

use chrono::{DateTime, Local, Months, Utc};
use opscope_core as tc;

use crate::parse::{parse_perplexity_credits, PerplexityCredits};
use crate::shared::*;
use crate::*;

const URL: &str = "https://www.perplexity.ai/rest/billing/credits?version=2.18&source=default";

/// The environment variable the cookie is read from when
/// `perplexity_token` is empty and `perplexity_token_env` names nothing.
/// The name CodexBar reads, so one variable serves both.
pub const TOKEN_ENV: &str = "PERPLEXITY_SESSION_TOKEN";

/// The session cookie names Perplexity's sign-in has used. A bare token is
/// sent under all four, since which one the account holds is not visible
/// from the value.
const COOKIE_NAMES: [&str; 4] = [
    "__Secure-authjs.session-token",
    "authjs.session-token",
    "__Secure-next-auth.session-token",
    "next-auth.session-token",
];

/// How long a reading is held. Credits move as the reader searches, and
/// five minutes keeps them current without asking on every refresh.
const REPORT_TTL: f64 = 300.0;

#[derive(Clone, Default)]
pub struct Data {
    credits: Option<PerplexityCredits>,
    /// When the reading was taken.
    read_at: f64,
    /// Where the cookie came from: `config`, `env`, or empty for none.
    source: &'static str,
    why: String,
}

/// The cookie, and where it came from. `config` wins over the environment.
pub fn token(cfg: &Config) -> (String, &'static str) {
    if !cfg.perplexity_token.trim().is_empty() {
        return (cfg.perplexity_token.trim().to_string(), "config");
    }
    let name = if cfg.perplexity_token_env.trim().is_empty() {
        TOKEN_ENV
    } else {
        cfg.perplexity_token_env.trim()
    };
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => (v.trim().to_string(), "env"),
        _ => (String::new(), ""),
    }
}

/// The `Cookie` header for a pasted value: a whole header copied from the
/// browser is sent as it is, less its `Cookie:` name, so numbered chunks of
/// a long session cookie arrive the way the browser sent them. Anything
/// naming no session cookie is a bare token.
///
/// A line break is refused rather than stripped. curl's config file turns
/// an escaped one back into a real one, which would end the header early
/// and start another, and a value that has one was not pasted as intended.
fn cookie_header(token: &str) -> Result<String, String> {
    if token.contains(['\r', '\n']) {
        return Err("the token has a line break in it · paste it again as one line".into());
    }
    let named = token.get(..7).is_some_and(|head| head.eq_ignore_ascii_case("cookie:"));
    let value = if named { &token[7..] } else { token }.trim();
    if value.contains("session-token") {
        return Ok(value.to_string());
    }
    Ok(COOKIE_NAMES
        .iter()
        .map(|name| format!("{}={}", name, value))
        .collect::<Vec<_>>()
        .join("; "))
}

/// The one request, as the raw answer, or why there was no reading.
///
/// The headers are the ones the account page sends. The cookie reaches curl
/// on its standard input, never on its command line.
fn ask(token: &str) -> Result<serde_json::Value, String> {
    let cookie = cookie_header(token)?;
    let headers = [
        ("Accept", "application/json"),
        ("Origin", "https://www.perplexity.ai"),
        ("Referer", "https://www.perplexity.ai/account/usage"),
        (
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36",
        ),
        ("Cookie", cookie.as_str()),
    ];
    let body = tc::get(URL, &headers, 15).map_err(|said| {
        if status_of(&said).is_some_and(|code| code == 401 || code == 403) {
            "session refused · copy a fresh session token from perplexity.ai".to_string()
        } else {
            format!("credits {}", refusal(&said))
        }
    })?;
    if parse_perplexity_credits(&body, now()).is_none() {
        return Err("Perplexity answered in a shape this widget cannot read".into());
    }
    Ok(serde_json::json!({ "body": body, "at": now() }))
}

fn status_of(said: &str) -> Option<u16> {
    said.rsplit_once("error: ")
        .and_then(|(_, tail)| tail.split_whitespace().next())
        .and_then(|code| code.parse().ok())
}

/// The cache slot for one account, so a token changed in settings is asked
/// afresh. Hashed, so the token is not kept as a key.
fn cache_key(token: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut h);
    format!("perplexity:{:016x}", h.finish())
}

/// `shown` is whether Perplexity has a tab under the reader's settings; with
/// no tab, or no cookie, nothing is sent.
pub fn read(caches: &mut Caches, cfg: &Config, shown: bool) -> Data {
    let mut d = Data::default();
    if !shown {
        d.why = "not asked · Perplexity is left out of this widget's agents".into();
        return d;
    }
    let (token, source) = token(cfg);
    d.source = source;
    if token.is_empty() {
        return d;
    }
    let key = cache_key(&token);
    let mut refused = String::new();
    let mut got = None;
    for _ in 0..2 {
        got = cached(caches, &key, REPORT_TTL, || match ask(&token) {
            Ok(v) => Some(v),
            Err(why) => {
                refused = why;
                None
            }
        });
        if !got.as_ref().is_some_and(renewal_passed) {
            break;
        }
        // A reading held past the renewal it reported is the last
        // period's, so the cache gives way rather than outliving it.
        caches.live.remove(&key);
    }
    remember_refusal(caches, &key, &refused);
    if !refused.is_empty() {
        d.why = refused;
        return d;
    }
    let Some(got) = got else {
        return d;
    };
    d.why = text(&got, "why");
    d.read_at = num(&got, "at");
    d.credits = parse_perplexity_credits(&text(&got, "body"), d.read_at);
    d
}

fn renewal_passed(got: &serde_json::Value) -> bool {
    parse_perplexity_credits(&text(got, "body"), num(got, "at"))
        .and_then(|c| c.renews)
        .is_some_and(|r| r <= now())
}

/// The period the recurring credits cover: the calendar month ending at the
/// renewal. Perplexity gives only the end, and a flat thirty days would put
/// the pace mark in the wrong place in every month that is not thirty long.
fn period_secs(ends: f64) -> Option<f64> {
    let end: DateTime<Utc> = DateTime::from_timestamp(ends as i64, 0)?;
    let start = end.checked_sub_months(Months::new(1))?;
    Some((end - start).num_seconds() as f64)
}

/// One lane per pool that has anything in it. Only the recurring credits
/// renew; purchased and bonus credits have no clock, and none is invented.
pub fn lanes(d: &Data) -> Vec<Lane> {
    let Some(c) = d.credits.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if c.recurring.total > 0.0 {
        let length = c.renews.and_then(period_secs);
        out.push(Lane {
            label: "monthly".into(),
            pct: c.recurring.pct(),
            window_secs: length,
            reset: length.and(c.renews),
            stale: length.and(c.renews).is_some_and(|r| r <= now()),
            projected: false,
            apart: false,
        });
    }
    for (label, pool) in [("bought", &c.purchased), ("bonus", &c.bonus)] {
        if pool.total > 0.0 {
            out.push(Lane {
                label: label.into(),
                pct: pool.pct(),
                window_secs: None,
                reset: None,
                stale: false,
                projected: false,
                apart: false,
            });
        }
    }
    out
}

impl Data {
    /// A reading of whatever credits answer `raw` is, taken a minute ago.
    #[cfg(test)]
    pub(crate) fn answered(raw: &str) -> Self {
        let read_at = now() - 60.0;
        Data {
            credits: parse_perplexity_credits(raw, read_at),
            read_at,
            source: "config",
            why: String::new(),
        }
    }
}

/// Why `[+]` has no bar for Perplexity.
pub fn why_no_lane(d: &Data) -> String {
    if !lanes(d).is_empty() {
        return String::new();
    }
    if d.credits.as_ref().is_some_and(|c| c.is_empty()) {
        return "no quota · Perplexity answered, and published no credits for this account."
            .into();
    }
    if !d.why.is_empty() {
        return format!("no quota · {}", d.why);
    }
    if d.source.is_empty() {
        return format!("no quota · no token · set perplexity_token or {}.", TOKEN_ENV);
    }
    "no quota · no reading from Perplexity yet.".into()
}

/// Cents as dollars, the unit every field in the answer is named for.
fn cents(value: f64) -> String {
    format!("${:.2}", value / 100.0)
}

fn day(ts: f64) -> String {
    DateTime::from_timestamp(ts as i64, 0)
        .map(|t| t.with_timezone(&Local).format("%b %-d").to_string())
        .unwrap_or_default()
}

fn credit_rows_with_plots(
    d: &Data,
    c: &PerplexityCredits,
    w: usize,
    p: &Palette,
    plots: &mut Vec<tc::Plot>,
) -> Vec<String> {
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── PERPLEXITY ── ".into()),
            (p.ok.as_str(), "live".into()),
            (p.dim.as_str(), format!(" · credits · read {} ago", ago(d.read_at))),
        ],
        w - 1,
    )];
    let hue = agent_hue("perplexity");
    let label_w = 7;
    for lane in lanes(d) {
        let used = (lane.pct / 100.0).clamp(0.0, 1.0);
        let room = ((w as i64) - 38 - label_w as i64).max(8) as usize;
        let mut line: Vec<(String, String)> =
            vec![(p.dim.clone(), format!(" {} ", tc::pad(&lane.label, label_w)))];
        line.extend(paced_bar_with_plot(
            plots,
            rows.len(),
            &line,
            used,
            elapsed_of(lane.window_secs, lane.reset),
            room,
            hue,
            p,
        ));
        line.push((pct_colour(lane.pct, hue, p), pct_text(lane.pct)));
        line.push(pace_cell(
            lead(lane.pct, lane.window_secs, lane.reset),
            pace_guessed(lane.stale, lane.projected, lane.reset),
            p,
        ));
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&refs, w - 1));
        let pool = match lane.label.as_str() {
            "monthly" => &c.recurring,
            "bought" => &c.purchased,
            _ => &c.bonus,
        };
        let mut detail = format!("{} of {} used", cents(pool.used), cents(pool.total));
        match lane.label.as_str() {
            "monthly" => match lane.reset.map(|r| r - now()) {
                Some(left) if left > 0.0 => detail.push_str(&format!(" · renews in {}", left_span(left))),
                Some(_) => detail.push_str(" · renewing"),
                None => {}
            },
            "bonus" => {
                if let Some(t) = c.bonus_expires {
                    detail.push_str(&format!(" · expires {}", day(t)));
                }
            }
            _ => detail.push_str(" · does not expire"),
        }
        rows.push(tc::seg(
            &[(p.dim.as_str(), format!(" {}  {}", " ".repeat(label_w), detail))],
            w - 1,
        ));
    }
    if c.recurring.total > 0.0 && c.renews.and_then(period_secs).is_none() {
        let text = "Perplexity's answer had no renewal date this widget can read, so the \
                    monthly credits have no pace or renewal drawn.";
        rows.extend(note(text, &p.warn, w));
    }
    if c.uncovered > 0.0 {
        let text = format!(
            "{} of the period's usage is more than every pool above holds, so it is \
             not drawn on any of them.",
            cents(c.uncovered)
        );
        rows.extend(note(&text, &p.warn, w));
    }
    if c.unknown_grants > 0 {
        let text = format!(
            "{} credit grant{} of a type this widget does not know {} left out.",
            c.unknown_grants,
            if c.unknown_grants == 1 { "" } else { "s" },
            if c.unknown_grants == 1 { "is" } else { "are" },
        );
        rows.extend(note(&text, &p.warn, w));
    }
    rows
}

// The cookie is a full sign-in, so a config.json others can read is worth
// saying on every reading, not only on the ones that failed.
fn token_warning(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    tc::config_token_warning()
        .filter(|_| d.source == "config")
        .map(|warn| note(&warn, &p.warn, w))
        .unwrap_or_default()
}

/// A sentence under the bars, wrapped to the pane rather than cut by it.
fn note(text: &str, colour: &str, w: usize) -> Vec<String> {
    wrap_text(text, w.saturating_sub(4).max(20))
        .into_iter()
        .map(|line| tc::seg(&[(colour, format!("  {}", line))], w - 1))
        .collect()
}

pub fn tab_with_plots(
    d: &Data,
    w: usize,
    _h: usize,
    _cfg: &Config,
    p: &Palette,
    plots: &mut Vec<tc::Plot>,
) -> Vec<String> {
    match d.credits.as_ref() {
        Some(c) if !c.is_empty() => {
            let mut rows = credit_rows_with_plots(d, c, w, p, plots);
            rows.extend(token_warning(d, w, p));
            rows
        }
        _ => {
            let what = if d.source.is_empty() && d.why.is_empty() {
                format!(
                    "No Perplexity token. Perplexity publishes no API for account credits, \
                     so this reads the one its usage page uses, signed in with your session \
                     cookie. Copy __Secure-next-auth.session-token from perplexity.ai \
                     (developer tools, cookies) into perplexity_token in config.json, or \
                     into {}. It is a full sign-in to your Perplexity account, so keep \
                     config.json chmod 600.",
                    TOKEN_ENV
                )
            } else {
                why_no_lane(d).trim_start_matches("no quota · ").to_string()
            };
            let mut rows = no_local(&what, "", w, p);
            rows.extend(token_warning(d, w, p));
            rows
        }
    }
}

#[cfg(test)]
pub fn tab(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    tab_with_plots(d, w, _h, _cfg, p, &mut Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(renews: f64) -> String {
        format!(
            r#"{{"balance_cents":7250,"renewal_date_ts":{renews},
                "current_period_purchased_cents":1500,
                "credit_grants":[
                    {{"type":"recurring","amount_cents":10000}},
                    {{"type":"promotional","amount_cents":2000,"expires_at_ts":{exp}}}],
                "total_usage_cents":10500}}"#,
            exp = now() + 30.0 * 86400.0
        )
    }

    fn reading() -> Data {
        Data::answered(&answer(now() + 10.0 * 86400.0))
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
    fn a_reading_is_one_lane_per_pool_and_only_the_monthly_one_has_a_clock() {
        let d = reading();
        let lanes = lanes(&d);
        let labels: Vec<_> = lanes.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels, ["monthly", "bought", "bonus"]);
        assert_eq!(lanes[0].pct, 100.0);
        assert!(lanes[0].reset.is_some() && lanes[0].window_secs.is_some());
        // 10500 used: all 10000 recurring, then 500 of the 1500 bought.
        assert!((lanes[1].pct - 100.0 / 3.0).abs() < 1e-9);
        assert_eq!(lanes[2].pct, 0.0);
        assert!(lanes[1..].iter().all(|l| l.reset.is_none() && l.window_secs.is_none()));
        assert!(why_no_lane(&d).is_empty());
    }

    #[test]
    fn the_tab_says_each_pool_in_dollars_and_fits_at_every_width() {
        let d = reading();
        for w in [30usize, 40, 60, 100] {
            let rows = plain(&tab(&d, w, 30, &Config::default(), &palette()));
            for r in rows.lines() {
                assert!(tc::display_width(r) <= w - 1, "width {w}: {r:?}");
            }
            if w >= 60 {
                assert!(rows.contains("$100.00 of $100.00 used"), "{rows}");
                assert!(rows.contains("$5.00 of $15.00 used"), "{rows}");
                assert!(rows.contains("renews in") && rows.contains("expires"), "{rows}");
            }
        }
    }

    #[test]
    fn a_cached_reading_gives_way_once_the_credits_renew() {
        let got = |renews: f64| serde_json::json!({ "body": answer(renews), "at": now() - 600.0 });
        assert!(!renewal_passed(&got(now() + 3600.0)));
        assert!(renewal_passed(&got(now() - 60.0)));
    }

    #[test]
    fn an_account_with_no_credits_says_so_rather_than_drawing_an_empty_bar() {
        let d = Data::answered(r#"{"credit_grants":[],"total_usage_cents":0}"#);
        assert!(lanes(&d).is_empty());
        let note = why_no_lane(&d);
        assert!(note.contains("answered, and published no credits"), "{note}");
    }

    #[test]
    fn usage_no_pool_covers_and_unknown_grants_are_said() {
        let d = Data::answered(
            r#"{"credit_grants":[{"type":"recurring","amount_cents":100},
                {"type":"mystery","amount_cents":5}],"total_usage_cents":400}"#,
        );
        let rows = plain(&tab(&d, 100, 30, &Config::default(), &palette()));
        let words = rows.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("$3.00 of the period's usage is more than"), "{rows}");
        assert!(words.contains("1 credit grant of a type this widget does not know is left out"), "{rows}");
        assert!(words.contains("no renewal date"), "{rows}");
    }

    #[test]
    fn with_no_token_nothing_is_asked_and_the_tab_says_where_one_goes() {
        let cfg = Config {
            perplexity_token_env: "OPSCOPE_TEST_NO_SUCH_VARIABLE".into(),
            ..Config::default()
        };
        let mut caches = Caches::default();
        let d = read(&mut caches, &cfg, true);
        assert!(caches.live.keys().all(|k| !k.starts_with("perplexity")));
        assert!(why_no_lane(&d).contains("perplexity_token"));
        let all = plain(&tab(&d, 60, 20, &cfg, &palette()));
        assert!(all.contains("perplexity_token") && all.contains("session-token"), "{all}");
    }

    #[test]
    fn a_perplexity_without_a_tab_is_never_asked() {
        let cfg = Config { perplexity_token: "secret".into(), ..Config::default() };
        let mut caches = Caches::default();
        let d = read(&mut caches, &cfg, false);
        assert!(caches.live.keys().all(|k| !k.starts_with("perplexity")));
        assert!(why_no_lane(&d).contains("left out"));
    }

    #[test]
    fn the_token_comes_from_config_before_the_environment() {
        let cfg = Config { perplexity_token: " abc ".into(), ..Config::default() };
        assert_eq!(token(&cfg), ("abc".to_string(), "config"));
        let none = Config {
            perplexity_token_env: "OPSCOPE_TEST_NO_SUCH_VARIABLE".into(),
            ..Config::default()
        };
        assert_eq!(token(&none), (String::new(), ""));
    }

    #[test]
    fn a_bare_token_and_a_copied_cookie_header_both_sign_in() {
        let ok = |t: &str| cookie_header(t).unwrap();
        let bare = ok("abc");
        for name in COOKIE_NAMES {
            assert!(bare.contains(&format!("{name}=abc")), "{bare}");
        }
        assert_eq!(
            ok("Cookie: a=1; __Secure-next-auth.session-token=abc"),
            "a=1; __Secure-next-auth.session-token=abc"
        );
        // A long session split into numbered chunks goes as the browser sent it.
        assert_eq!(
            ok("cookie: __Secure-authjs.session-token.0=ab; __Secure-authjs.session-token.1=c"),
            "__Secure-authjs.session-token.0=ab; __Secure-authjs.session-token.1=c"
        );
    }

    #[test]
    fn a_token_with_a_line_break_is_refused_rather_than_sent() {
        assert!(cookie_header("abc\r\nX-Other: 1").is_err());
        assert!(cookie_header("next-auth.session-token=abc\n").is_err());
    }

    #[test]
    fn a_changed_token_is_not_shown_the_last_reading() {
        let a = cache_key("one");
        assert_ne!(a, cache_key("two"));
        assert_eq!(a, cache_key("one"));
        assert!(!a.contains("one"));
    }
}
