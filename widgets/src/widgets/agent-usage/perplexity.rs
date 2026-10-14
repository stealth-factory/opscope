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

use crate::parse::{parse_perplexity_credits, parse_perplexity_plan, parse_perplexity_quota,
    parse_perplexity_subscription, PerplexitySubscription, PerplexityCredits, PerplexityQuota,
    PerplexityTopup};
use crate::shared::*;
use crate::*;

/// The query the usage page itself sends, which leaves out per-thread usage
/// and purchase history the tab does not draw.
const URL: &str = "https://www.perplexity.ai/rest/billing/credits?include_thread_usage=false\
                   &fetch_purchase_history=false&version=2.18&source=default";

/// How the plan is billed: its interval and next billing date.
const SUBSCRIPTION_URL: &str =
    "https://www.perplexity.ai/rest/stripe/subscription-billing-info?version=2.18&source=default";

/// The weekly limit a plan includes, asked separately: the usage page draws
/// it above the credits from this endpoint, not from the credits answer.
const QUOTA_URL: &str = "https://www.perplexity.ai/rest/billing/quota?version=2.18&source=default";

/// The sign-in's session, which names the plan: Pro, Max and so on.
const SESSION_URL: &str = "https://www.perplexity.ai/api/auth/session";

/// What `ask` says when Perplexity refuses the cookie. Every billing
/// endpoint answers a stale or signed-out session with 401, so a refusal is
/// read as the sign-in having lapsed, and the tab says how to replace it.
const EXPIRED: &str = "session expired";

/// The weekly limit's length. The answer gives only its end.
const WEEK: f64 = 7.0 * 86400.0;

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
    /// The plan's weekly limit, when the quota endpoint answered.
    quota: Option<PerplexityQuota>,
    /// Why there is no weekly limit, when the quota endpoint did not answer.
    /// Kept apart from `why`, so one endpoint failing does not hide the other.
    quota_why: String,
    /// The plan the session names, when it named one.
    plan: Option<String>,
    /// How the plan is billed, when that answered.
    subscription: Option<PerplexitySubscription>,
    /// Why the billing request did not answer, so the billing drawn without
    /// it is not taken for all of it.
    subscription_why: String,
    /// When the reading was taken.
    read_at: f64,
    /// Where the cookie came from: `config`, `env`, or empty for none.
    source: &'static str,
    /// The variable read, when `source` is `env`.
    env_name: String,
    why: String,
}

/// The cookie, and where it came from. `config` wins over the environment.
pub fn token(cfg: &Config) -> (String, &'static str) {
    if !cfg.perplexity_token.trim().is_empty() {
        return (cfg.perplexity_token.trim().to_string(), "config");
    }
    match std::env::var(env_name(cfg)) {
        Ok(v) if !v.trim().is_empty() => (v.trim().to_string(), "env"),
        _ => (String::new(), ""),
    }
}

/// The variable the cookie is read from when `perplexity_token` is empty.
fn env_name(cfg: &Config) -> &str {
    match cfg.perplexity_token_env.trim() {
        "" => TOKEN_ENV,
        name => name,
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
fn ask(token: &str, url: &str, what: &str, reads: fn(&str) -> bool) -> Result<serde_json::Value, String> {
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
    // The cookie is a whole sign-in, so it is never followed off this host.
    let body = tc::get_without_redirects(url, &headers, 15).map_err(|said| {
        if status_of(&said).is_some_and(|code| code == 401 || code == 403) {
            EXPIRED.to_string()
        } else {
            format!("{what} {}", refusal(&said))
        }
    })?;
    if !reads(&body) {
        return Err(format!("Perplexity's {what} answer is in a shape this widget cannot read"));
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
    d.env_name = env_name(cfg).to_string();
    if token.is_empty() {
        return d;
    }
    let key = cache_key(&token);
    let mut refused = String::new();
    let mut got = None;
    for _ in 0..2 {
        got = cached(caches, &key, REPORT_TTL, || {
            match ask(&token, URL, "credits", |b| parse_perplexity_credits(b, now()).is_some()) {
                Ok(v) => Some(v),
                Err(why) => {
                    refused = why;
                    None
                }
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
    d.why = refused;
    if let Some(got) = got.as_ref().filter(|_| d.why.is_empty()) {
        d.why = text(got, "why");
        d.read_at = num(got, "at");
        d.credits = parse_perplexity_credits(&text(got, "body"), d.read_at);
    }
    let qkey = format!("{key}:quota");
    let mut refused = String::new();
    let mut got = None;
    for _ in 0..2 {
        got = cached(caches, &qkey, REPORT_TTL, || {
            match ask(&token, QUOTA_URL, "weekly limit", |b| {
                parse_perplexity_quota(b).is_some() || answers_no_weekly_limit(b)
            }) {
                Ok(v) => Some(v),
                Err(why) => {
                    refused = why;
                    None
                }
            }
        });
        // Past its reset, a held reading is last week's.
        let stale = got.as_ref().is_some_and(|g| {
            parse_perplexity_quota(&text(g, "body")).and_then(|q| q.resets).is_some_and(|r| r <= now())
        });
        if !stale {
            break;
        }
        caches.live.remove(&qkey);
    }
    remember_refusal(caches, &qkey, &refused);
    d.quota_why = refused;
    // The plan is only a label in the heading; a session that did not
    // answer leaves it out rather than saying anything went wrong.
    let pkey = format!("{key}:plan");
    let plan = cached(caches, &pkey, REPORT_TTL, || {
        ask(&token, SESSION_URL, "plan", |b| parse_perplexity_plan(b).is_some()).ok()
    });
    d.plan = plan.and_then(|g| parse_perplexity_plan(&text(&g, "body")));
    let skey = format!("{key}:subscription");
    let mut refused = String::new();
    let sub = cached(caches, &skey, REPORT_TTL, || {
        match ask(&token, SUBSCRIPTION_URL, "subscription", |b| parse_perplexity_subscription(b).is_some()) {
            Ok(v) => Some(v),
            Err(why) => {
                refused = why;
                None
            }
        }
    });
    remember_refusal(caches, &skey, &refused);
    d.subscription_why = refused;
    // A refusal held from an earlier read comes back as the cached value.
    if let Some(why) = sub.as_ref().map(|g| text(g, "why")).filter(|_| d.subscription_why.is_empty()) {
        d.subscription_why = why;
    }
    d.subscription = sub.as_ref().and_then(|g| parse_perplexity_subscription(&text(g, "body")));
    if let Some(got) = got.as_ref().filter(|_| d.quota_why.is_empty()) {
        // A refusal held from an earlier read comes back as the cached value.
        d.quota_why = text(got, "why");
        d.quota = parse_perplexity_quota(&text(got, "body"));
        if d.read_at == 0.0 {
            d.read_at = num(got, "at");
        }
    }
    // Billing alone is still a reading, and says when it was taken.
    if let Some(got) = sub.as_ref().filter(|_| d.read_at == 0.0 && d.subscription.is_some()) {
        d.read_at = num(got, "at");
    }
    d
}

/// A quota answer that is an object with no share used in it: a plan with
/// no weekly limit (Max, it may be). It is a reading, so it is not said as a
/// fault, where a timeout, an error status or a body that is not JSON is.
fn answers_no_weekly_limit(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body).ok().is_some_and(|v| {
        v.as_object()
            .is_some_and(|o| !o.contains_key("percent_used") && !o.contains_key("percentUsed"))
    })
}

/// Why the weekly limit is missing, when that is a fault and not a plan
/// without one. A 404 is taken as the plan having no such endpoint.
fn quota_fault(d: &Data) -> Option<String> {
    let why = d.quota_why.as_str();
    (d.quota.is_none() && !why.is_empty() && !why.ends_with("the endpoint is gone"))
        .then(|| format!("weekly limit not read · {why}"))
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
    let mut out = Vec::new();
    if let Some(q) = d.quota.as_ref() {
        out.push(Lane {
            label: "weekly".into(),
            pct: q.pct,
            window_secs: q.resets.map(|_| WEEK),
            reset: q.resets,
            stale: q.resets.is_some_and(|r| r <= now()),
            projected: false,
            apart: false,
        });
    }
    let Some(c) = d.credits.as_ref() else {
        return out;
    };
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
    for (label, pool) in [("credits", &c.purchased), ("bonus", &c.bonus)] {
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
            ..Data::default()
        }
    }
}

/// Why `[+]` has no bar for Perplexity.
impl Data {
    /// Perplexity refused the cookie: it expired, or the account signed out.
    fn expired(&self) -> bool {
        self.why == EXPIRED
    }
}

/// How to put a fresh cookie where this one came from.
/// The variable the cookie is read from, as configured, so no guidance
/// names a default the reader has changed.
fn var_name(d: &Data) -> &str {
    match d.env_name.as_str() {
        "" => TOKEN_ENV,
        name => name,
    }
}

fn replace_token(d: &Data) -> String {
    let find = "Sign in at perplexity.ai, open developer tools, then Application (Storage in \
                Firefox), Cookies, https://www.perplexity.ai, and copy the Value of \
                __Secure-next-auth.session-token.";
    match d.source {
        "env" => format!(
            "{find} Put it in {} and restart this widget, or press , and paste it into \
             perplexity_token.",
            var_name(d)
        ),
        _ => format!("{find} Then press , and paste it into perplexity_token."),
    }
}

pub fn why_no_lane(d: &Data) -> String {
    if !lanes(d).is_empty() {
        return String::new();
    }
    if d.credits.as_ref().is_some_and(|c| c.is_empty()) {
        return "no quota · Perplexity answered, and published no credits for this account."
            .into();
    }
    if d.expired() {
        return "no quota · Perplexity session expired · press , to paste a fresh token".into();
    }
    if !d.why.is_empty() {
        return format!("no quota · {}", d.why);
    }
    if !d.quota_why.is_empty() {
        return format!("no quota · {}", d.quota_why);
    }
    if d.source.is_empty() {
        return format!("no quota · no token · set perplexity_token or {}.", var_name(d));
    }
    "no quota · no reading from Perplexity yet.".into()
}

/// The answer's fields are named in cents, but Perplexity's own usage page
/// counts the same numbers as credits, one to one: `balance_cents` 1314 is
/// "1,314 credits left" there. So they are drawn as credits, whole, with the
/// page's thousands separator - a dollar sign would be a conversion nobody
/// was offered.
fn cents(value: f64) -> String {
    format!("{} credits", count(value))
}

/// A credit figure with no unit, for the first of two in one phrase.
fn count(value: f64) -> String {
    let whole = value.max(0.0).floor() as u64;
    let digits = whole.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn day(ts: f64) -> String {
    DateTime::from_timestamp(ts as i64, 0)
        .map(|t| t.with_timezone(&Local).format("%b %-d").to_string())
        .unwrap_or_default()
}

fn credit_rows_with_plots(
    d: &Data,
    c: Option<&PerplexityCredits>,
    w: usize,
    p: &Palette,
    plots: &mut Vec<tc::Plot>,
) -> Vec<String> {
    let read = match (d.quota.is_some(), c.is_some()) {
        (true, true) => "weekly limit · credits",
        (true, false) => "weekly limit",
        (false, true) => "credits",
        (false, false) => "billing",
    };
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── PERPLEXITY ".into()),
            (p.txt.as_str(), d.plan.as_ref().map(|plan| format!("{plan} ")).unwrap_or_default()),
            (p.lbl.as_str(), "── ".into()),
            (p.ok.as_str(), "live".into()),
            (p.dim.as_str(), format!(" · {read} · read {} ago", ago(d.read_at))),
        ],
        w - 1,
    )];
    let hue = agent_hue("perplexity");
    let label_w = 7;
    // Which token was sent: config wins over the variable, so a stale one
    // left in config.json is otherwise invisible behind a fresh variable.
    let from = match d.source {
        "env" => format!("from ${}", d.env_name),
        _ => "from config.json (perplexity_token)".to_string(),
    };
    rows.extend(field_rows("token", &from, &p.dim, w, p));
    if let Some(c) = c {
        rows.extend(balance_rows(c, w, p));
    }
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
        let detail = match (lane.label.as_str(), c) {
            ("weekly", _) => weekly_detail(lane.reset),
            (label, Some(c)) => pool_detail(label, &lane, c),
            _ => String::new(),
        };
        let indent = if w < 40 { 2 } else { label_w + 3 };
        for line in wrap_text(&detail, w.saturating_sub(indent + 2).max(8)) {
            rows.push(tc::seg(&[(p.dim.as_str(), format!("{}{}", " ".repeat(indent), line))], w - 1));
        }
    }
    // No weekly bar when the quota endpoint answered none: a plan without a
    // weekly limit (Max, it may be) is not something gone wrong. A request
    // that failed is, or the bars left would read as the whole account.
    if let Some(fault) = quota_fault(d) {
        rows.push(String::new());
        rows.extend(note(&fault, &p.warn, w));
    }
    // But credits that failed beside a quota that answered are said, or the
    // weekly bar alone would read as the whole account. Each answer stands
    // alone, so the billing a subscription answered is drawn either way.
    match c {
        Some(c) if c.is_empty() => {
            let text = "Perplexity answered, and published no credits for this account.";
            rows.extend(note(text, &p.dim, w));
        }
        Some(c) => rows.extend(grant_rows(c, w, p)),
        None => {
            let why = match d.why.as_str() {
                "" => "credits not read · no reading from Perplexity yet.".to_string(),
                why => format!("credits not read · {why}"),
            };
            rows.push(String::new());
            rows.extend(note(&why, &p.warn, w));
        }
    }
    rows.extend(billing_rows(c, d.subscription.as_ref(), w, p));
    if d.subscription.is_none() && !d.subscription_why.is_empty() {
        rows.extend(note(&format!("plan billing not read · {}", d.subscription_why), &p.warn, w));
    }
    if let Some(c) = c {
        rows.extend(credit_notes(c, w, p));
    }
    rows
}

/// The line under the weekly bar: when it resets, in the reader's own time,
/// the way the usage page says it.
fn weekly_detail(reset: Option<f64>) -> String {
    let Some(r) = reset else {
        return "Perplexity gave no reset time".into();
    };
    let at = DateTime::from_timestamp(r as i64, 0)
        .map(|t| t.with_timezone(&Local).format("%a %H:%M").to_string())
        .unwrap_or_default();
    match r - now() {
        left if left > 0.0 => format!("resets {at} · in {}", left_span(left)),
        _ => "resetting".into(),
    }
}

/// The line under a credit pool's bar: how much of it is gone, and when it
/// renews or lapses.
fn pool_detail(label: &str, lane: &Lane, c: &PerplexityCredits) -> String {
    let pool = match label {
        "monthly" => &c.recurring,
        "credits" => &c.purchased,
        _ => &c.bonus,
    };
    let mut detail = format!("{} of {} used", count(pool.used), cents(pool.total));
    {
        match label {
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
    }
    detail
}

/// What the credits answer said that the bars cannot show.
fn credit_notes(c: &PerplexityCredits, w: usize, p: &Palette) -> Vec<String> {
    let mut rows = Vec::new();
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

/// A section heading inside the tab, in the shape the QUOTAS view uses.
fn heading(name: &str, tail: &str, w: usize, p: &Palette) -> String {
    tc::seg(
        &[(p.lbl.as_str(), format!(" ── {name} ── ")), (p.dim.as_str(), tail.to_string())],
        w - 1,
    )
}

/// A label and its value, the value wrapped under itself when the pane is
/// narrow rather than cut.
fn field_rows(label: &str, value: &str, colour: &str, w: usize, p: &Palette) -> Vec<String> {
    let label_w = 15;
    // Too narrow for a value beside its label: the label goes above it.
    if w < label_w + 4 + 16 {
        let mut rows = vec![tc::seg(&[(p.dim.as_str(), format!("  {label}"))], w - 1)];
        rows.extend(
            wrap_text(value, w.saturating_sub(6).max(8))
                .into_iter()
                .map(|line| tc::seg(&[(colour, format!("    {line}"))], w - 1)),
        );
        return rows;
    }
    let room = w - label_w - 4;
    wrap_text(value, room)
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let lead = if i == 0 { label } else { "" };
            tc::seg(
                &[(p.dim.as_str(), format!("  {} ", tc::pad(lead, label_w))), (colour, line)],
                w - 1,
            )
        })
        .collect()
}

/// The balance as Perplexity states it, then this period's usage and when
/// the period turns over. Usage is labelled as the period's, because it is
/// not what the grants below have lost in all.
fn balance_rows(c: &PerplexityCredits, w: usize, p: &Palette) -> Vec<String> {
    let mut rows = Vec::new();
    if let Some(balance) = c.balance {
        rows.extend(field_rows("balance", &format!("{} left", cents(balance)), &p.ok, w, p));
    }
    let mut period = format!("{} used", cents(c.period_usage));
    match c.renews.map(|r| (r, r - now())) {
        Some((r, left)) if left > 0.0 => {
            period.push_str(&format!(" · ends {} (in {})", day(r), left_span(left)))
        }
        Some(_) => period.push_str(" · ended, renewing"),
        None => {}
    }
    rows.extend(field_rows("this period", &period, &p.txt, w, p));
    rows.push(String::new());
    rows
}

/// Every grant on its own row: what it was, what is left, when it came and
/// when it lapses. The bars above add grants of a kind together.
fn grant_rows(c: &PerplexityCredits, w: usize, p: &Palette) -> Vec<String> {
    if c.grants.is_empty() {
        return Vec::new();
    }
    let n = c.grants.len();
    let mut rows = vec![String::new(), heading("GRANTS", &format!("{n} on the account"), w, p)];
    for g in &c.grants {
        let kind = match g.kind.as_str() {
            "recurring" => "monthly",
            "purchased" => "purchased",
            "promotional" => "bonus",
            other => other,
        };
        let mut text = match g.remaining {
            Some(left) => format!("{} left of {}", count(left), cents(g.amount)),
            None => format!("{} granted", cents(g.amount)),
        };
        if let Some(t) = g.effective {
            text.push_str(&format!(" · added {}", day(t)));
        }
        if let Some(before) = g.prior_periods.filter(|x| *x > 0.0) {
            text.push_str(&format!(" · {} spent before this period", cents(before)));
        }
        if g.never_expires() {
            text.push_str(" · never expires");
        } else if let Some(t) = g.expires {
            text.push_str(&format!(" · expires {}", day(t)));
        }
        rows.extend(field_rows(kind, &text, &p.txt, w, p));
    }
    rows
}

/// What refills the balance and what caps it, as the account has them set.
fn billing_rows(
    c: Option<&PerplexityCredits>,
    sub: Option<&PerplexitySubscription>,
    w: usize,
    p: &Palette,
) -> Vec<String> {
    let mut body = Vec::new();
    if let Some(sub) = sub {
        let mut text = sub.interval.clone().unwrap_or_default();
        let when = match (sub.cancels, sub.next_bill) {
            (Some(t), _) => Some(format!("cancelled · ends {}", day(t))),
            (None, Some(t)) => Some(format!("next bill {}", day(t))),
            _ => None,
        };
        if let Some(when) = when {
            if !text.is_empty() {
                text.push_str(" · ");
            }
            text.push_str(&when);
        }
        let colour = if sub.cancels.is_some() { &p.warn } else { &p.txt };
        body.extend(field_rows("plan", &text, colour, w, p));
    }
    if let Some(c) = c {
        // On with a part missing stays on, and a setting the answer left out is
        // not drawn: reading either as off would misstate what buys credits.
        match c.auto_topup {
            Some(PerplexityTopup::On { adds: Some(adds), under: Some(under) }) => {
                let text =
                    format!("on · adds {} when the balance drops under {}", cents(adds), cents(under));
                body.extend(field_rows("auto top-up", &text, &p.txt, w, p));
            }
            Some(PerplexityTopup::On { adds, under }) => {
                let adds = adds.map(|a| format!(" · adds {}", cents(a))).unwrap_or_default();
                let under = under.map(|u| format!(" · under {}", cents(u))).unwrap_or_default();
                let text = format!("on{adds}{under} · Perplexity did not say all of how it tops up");
                body.extend(field_rows("auto top-up", &text, &p.txt, w, p));
            }
            Some(PerplexityTopup::Off) => body.extend(field_rows("auto top-up", "off", &p.dim, w, p)),
            None => {}
        }
        if let Some(limit) = c.spending_limit {
            body.extend(field_rows("spending limit", &cents(limit), &p.txt, w, p));
        }
    }
    if body.is_empty() {
        return body;
    }
    let mut rows = vec![String::new(), heading("BILLING", "", w, p)];
    rows.extend(body);
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
/// A word longer than the room - the cookie's name is 32 characters - is
/// broken by hand, since wrapping alone would leave it to be cut.
fn note(text: &str, colour: &str, w: usize) -> Vec<String> {
    let room = w.saturating_sub(4).max(8);
    wrap_text(text, room)
        .into_iter()
        .flat_map(|line| {
            let chars: Vec<char> = line.chars().collect();
            chars.chunks(room).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>()
        })
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
    if d.expired() {
        let mut rows = vec![
            tc::seg(&[(p.lbl.as_str(), " ── PERPLEXITY ──".into())], w - 1),
            tc::seg(&[(p.warn.as_str(), "  session expired".into())], w - 1),
            String::new(),
        ];
        rows.extend(note(
            "Perplexity no longer accepts the session token this widget was given: it expired, \
             or the account signed out. Nothing on this tab can be read until it is replaced.",
            &p.warn,
            w,
        ));
        rows.push(String::new());
        rows.extend(note(&replace_token(d), &p.txt, w));
        rows.extend(token_warning(d, w, p));
        return rows;
    }
    // An answer with no credit pools still has a balance and billing to
    // draw, so it is not filtered out here; it simply has no bars.
    let credits = d.credits.as_ref();
    match credits {
        _ if credits.is_some() || d.quota.is_some() || d.subscription.is_some() => {
            let mut rows = credit_rows_with_plots(d, credits, w, p, plots);
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
                     config.json chmod 600. Press , to open settings and paste it into \
                     perplexity_token.",
                    var_name(d)
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
        assert_eq!(labels, ["monthly", "credits", "bonus"]);
        assert_eq!(lanes[0].pct, 100.0);
        assert!(lanes[0].reset.is_some() && lanes[0].window_secs.is_some());
        // 10500 used: all 10000 recurring, then 500 of the 1500 bought.
        assert!((lanes[1].pct - 100.0 / 3.0).abs() < 1e-9);
        assert_eq!(lanes[2].pct, 0.0);
        assert!(lanes[1..].iter().all(|l| l.reset.is_none() && l.window_secs.is_none()));
        assert!(why_no_lane(&d).is_empty());
    }

    #[test]
    fn the_tab_says_each_pool_in_credits_and_fits_at_every_width() {
        let d = reading();
        for w in [30usize, 40, 60, 100] {
            let rows = plain(&tab(&d, w, 30, &Config::default(), &palette()));
            for r in rows.lines() {
                assert!(tc::display_width(r) <= w - 1, "width {w}: {r:?}");
            }
            if w >= 60 {
                assert!(rows.contains("10,000 of 10,000 credits used"), "{rows}");
                assert!(rows.contains("500 of 1,500 credits used"), "{rows}");
                assert!(rows.contains("renews in") && rows.contains("expires"), "{rows}");
            }
        }
    }

    /// The shape a real account answered with: one bought grant, part
    /// spent in an earlier period, nothing spent in this one, auto top-up on.
    const CARRIED_OVER: &str = r#"{
        "balance_cents": 1314.1, "renewal_date_ts": 4102444800,
        "auto_topup_enabled": true, "auto_topup_amount_cents": 2000,
        "auto_topup_threshold_cents": 500, "spending_limit_cents": 4000,
        "current_period_purchased_cents": 0, "total_usage_cents": 0,
        "credit_grants": [{"type": "purchased", "amount_cents": 2000,
            "expires_at_ts": 253402214400, "effective_at_ts": 1789282800,
            "amount_applied_prior_period_cents": 686, "remaining_cents": 1314}]
    }"#;

    #[test]
    fn a_grant_spent_in_an_earlier_period_is_drawn_as_spent() {
        let d = Data::answered(CARRIED_OVER);
        let lanes = lanes(&d);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].label, "credits");
        assert!((lanes[0].pct - 34.3).abs() < 1e-9, "{}", lanes[0].pct);
        for w in [30usize, 40, 60, 100] {
            let rows = plain(&tab(&d, w, 30, &Config::default(), &palette()));
            for r in rows.lines() {
                assert!(tc::display_width(r) <= w - 1, "width {w}: {r:?}");
            }
            let words = rows.split_whitespace().collect::<Vec<_>>().join(" ");
            for want in [
                "1,314 credits left",
                "686 of 2,000 credits used",
                "GRANTS",
                "1,314 left of 2,000 credits",
                "686 credits spent before this period",
                "never expires",
                "adds 2,000 credits when the balance drops under 500 credits",
                "4,000 credits",
            ] {
                assert!(words.contains(want), "width {w}, {want:?}:\n{rows}");
            }
        }
    }

    #[test]
    fn the_weekly_limit_comes_first_with_its_reset_and_is_left_out_when_not_answered() {
        let resets = now() + 6.0 * 86400.0;
        let mut d = Data::answered(CARRIED_OVER);
        d.quota = Some(PerplexityQuota { pct: 3.0, resets: Some(resets) });
        d.plan = Some("Pro".into());
        let got = lanes(&d);
        let labels: Vec<_> = got.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels, ["weekly", "credits"]);
        assert_eq!((got[0].pct, got[0].reset, got[0].window_secs), (3.0, Some(resets), Some(WEEK)));
        for w in [30usize, 60, 100] {
            let rows = plain(&tab(&d, w, 30, &Config::default(), &palette()));
            for r in rows.lines() {
                assert!(tc::display_width(r) <= w - 1, "width {w}: {r:?}");
            }
            assert!(rows.contains("PERPLEXITY Pro"), "{rows}");
            let solid: String = rows.split_whitespace().collect();
            assert!(solid.contains("fromconfig.json"), "{rows}");
            assert!(rows.contains("resets ") && rows.contains("weekly"), "{rows}");
        }
        // A plan with no weekly limit, or a quota endpoint that failed, has
        // no weekly bar and no complaint about one.
        let mut d = Data::answered(CARRIED_OVER);
        d.quota_why = "weekly limit the endpoint is gone".into();
        assert!(lanes(&d).iter().all(|l| l.label != "weekly"));
        let rows = plain(&tab(&d, 100, 30, &Config::default(), &palette()));
        assert!(!rows.contains("weekly") && !rows.contains("404"), "{rows}");
        // Only the weekly limit answering is still a tab.
        let d = Data {
            quota: Some(PerplexityQuota { pct: 40.0, resets: None }),
            source: "env",
            env_name: TOKEN_ENV.into(),
            ..Data::default()
        };
        let rows = plain(&tab(&d, 60, 30, &Config::default(), &palette()));
        assert!(rows.contains(&format!("from ${TOKEN_ENV}")), "{rows}");
        assert!(rows.contains("weekly") && rows.contains("no reset time"), "{rows}");
    }

    #[test]
    fn an_expired_session_says_so_and_where_to_paste_a_fresh_one() {
        for (source, place) in [("config", "press , and paste it into perplexity_token"), ("env", TOKEN_ENV)] {
            let d = Data { why: EXPIRED.into(), source, ..Data::default() };
            assert!(lanes(&d).is_empty());
            assert!(why_no_lane(&d).contains("session expired"), "{}", why_no_lane(&d));
            for w in [30usize, 60, 100] {
                let rows = plain(&tab(&d, w, 30, &Config::default(), &palette()));
                for r in rows.lines() {
                    assert!(tc::display_width(r) <= w - 1, "width {w}: {r:?}");
                }
                let words = rows.split_whitespace().collect::<Vec<_>>().join(" ");
                assert!(words.contains("session expired"), "{rows}");
                // The cookie's name is broken across lines in a narrow pane.
                let solid: String = rows.split_whitespace().collect();
                assert!(solid.contains("__Secure-next-auth.session-token"), "{rows}");
                assert!(solid.contains(&place.replace(' ', "")), "{source}: {rows}");
            }
        }
    }

    #[test]
    fn credits_that_failed_beside_an_answered_weekly_limit_are_said() {
        // The weekly bar drew alone and the credits, grants and billing just
        // went missing, which read as an account with a weekly limit only.
        let d = Data {
            quota: Some(PerplexityQuota { pct: 40.0, resets: None }),
            why: "credits refused (500)".into(),
            source: "config",
            ..Data::default()
        };
        for w in [30usize, 60, 100] {
            let rows = plain(&tab(&d, w, 30, &Config::default(), &palette()));
            for r in rows.lines() {
                assert!(tc::display_width(r) <= w - 1, "width {w}: {r:?}");
            }
            let words = rows.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(words.contains("weekly"), "{rows}");
            assert!(words.contains("credits not read · credits refused (500)"), "width {w}: {rows}");
        }
    }

    #[test]
    fn billing_and_an_empty_balance_are_drawn_when_credits_give_no_bars() {
        // A failed credits request, or an answer with no pools, took the
        // billing the subscription had answered down with it.
        let sub = Some(PerplexitySubscription {
            interval: Some("monthly".into()),
            next_bill: Some(now() + 86400.0 * 10.0),
            cancels: None,
        });
        let failed = Data {
            quota: Some(PerplexityQuota { pct: 3.0, resets: None }),
            why: "credits refused (500)".into(),
            subscription: sub.clone(),
            source: "config",
            ..Data::default()
        };
        let mut empty = Data::answered(r#"{"credit_grants":[],"total_usage_cents":0,"balance_cents":0}"#);
        empty.subscription = sub;
        for d in [&failed, &empty] {
            let rows = plain(&tab(d, 100, 30, &Config::default(), &palette()));
            let words = rows.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(words.contains("BILLING") && words.contains("monthly · next bill"), "{rows}");
        }
        let rows = plain(&tab(&empty, 100, 30, &Config::default(), &palette()));
        let words = rows.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("0 credits left") && words.contains("published no credits"), "{rows}");
    }

    #[test]
    fn guidance_names_the_variable_configured_and_a_failed_billing_request_is_said() {
        // The setup steps named PERPLEXITY_SESSION_TOKEN even when
        // perplexity_token_env pointed somewhere else, and a billing request
        // that failed just left the plan line out.
        let d = Data { env_name: "MY_PPLX".into(), ..Data::default() };
        assert!(why_no_lane(&d).contains("MY_PPLX"), "{}", why_no_lane(&d));
        let rows = plain(&tab(&d, 100, 30, &Config::default(), &palette()));
        assert!(rows.contains("MY_PPLX") && !rows.contains(TOKEN_ENV), "{rows}");
        let d = Data { why: EXPIRED.into(), source: "env", env_name: "MY_PPLX".into(), ..Data::default() };
        let solid: String = plain(&tab(&d, 100, 30, &Config::default(), &palette())).split_whitespace().collect();
        assert!(solid.contains("PutitinMY_PPLX"), "{solid}");
        let mut d = Data::answered(CARRIED_OVER);
        d.subscription_why = "subscription refused (502)".into();
        let rows = plain(&tab(&d, 100, 30, &Config::default(), &palette()));
        let words = rows.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("plan billing not read · subscription refused (502)"), "{rows}");
    }

    #[test]
    fn billing_alone_is_still_a_tab() {
        // With credits failed and no weekly limit (Max, it may be), the
        // billing that did answer fell through to the failure panel.
        let d = Data {
            subscription: Some(PerplexitySubscription {
                interval: Some("yearly".into()),
                next_bill: Some(now() + 86400.0 * 40.0),
                cancels: None,
            }),
            why: "credits refused (500)".into(),
            read_at: now() - 30.0,
            source: "config",
            ..Data::default()
        };
        let rows = plain(&tab(&d, 60, 30, &Config::default(), &palette()));
        let words = rows.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("live · billing"), "{rows}");
        assert!(words.contains("yearly · next bill"), "{rows}");
        assert!(words.contains("credits not read · credits refused (500)"), "{rows}");
    }

    #[test]
    fn a_weekly_limit_that_failed_is_said_and_a_plan_without_one_is_not() {
        // A timeout on the quota request looked exactly like a plan with no
        // weekly limit: the bar was simply not there.
        assert!(answers_no_weekly_limit(r#"{"plan":"max"}"#));
        assert!(!answers_no_weekly_limit(r#"{"percent_used":3}"#));
        assert!(!answers_no_weekly_limit("<html>bad gateway</html>"));
        let mut d = Data::answered(CARRIED_OVER);
        d.quota_why = "weekly limit the request timed out".into();
        let rows = plain(&tab(&d, 60, 30, &Config::default(), &palette()));
        let words = rows.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("weekly limit not read · weekly limit the request timed out"), "{rows}");
        // An answer with no weekly limit in it is a reading, not a fault.
        let d = Data::answered(CARRIED_OVER);
        let rows = plain(&tab(&d, 60, 30, &Config::default(), &palette()));
        assert!(!rows.contains("not read"), "{rows}");
    }

    #[test]
    fn auto_top_up_that_is_on_stays_on_when_its_amounts_are_missing() {
        // Missing amounts collapsed the setting to off, the opposite of it.
        let d = Data::answered(
            r#"{"total_usage_cents":0,"balance_cents":100,"auto_topup_enabled":true,
                "auto_topup_amount_cents":2000,
                "credit_grants":[{"type":"purchased","amount_cents":100,"remaining_cents":100}]}"#,
        );
        assert_eq!(
            d.credits.as_ref().unwrap().auto_topup,
            Some(PerplexityTopup::On { adds: Some(2000.0), under: None })
        );
        let rows = plain(&tab(&d, 100, 30, &Config::default(), &palette()));
        let words = rows.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("auto top-up on · adds 2,000 credits"), "{rows}");
        assert!(!words.contains("auto top-up off"), "{rows}");
        // An answer that does not say is not drawn as off either.
        let d = Data::answered(r#"{"total_usage_cents":0,"credit_grants":[
            {"type":"purchased","amount_cents":100,"remaining_cents":100}]}"#);
        assert_eq!(d.credits.as_ref().unwrap().auto_topup, None);
        let rows = plain(&tab(&d, 100, 30, &Config::default(), &palette()));
        assert!(!rows.contains("auto top-up"), "{rows}");
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
        assert!(words.contains("300 credits of the period's usage is more than"), "{rows}");
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
