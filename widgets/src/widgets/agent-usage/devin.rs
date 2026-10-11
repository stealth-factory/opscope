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

//! Devin: daily and weekly quota, from the billing endpoint the web app uses.
//!
//! The bearer token and the organization are the reader's to give, in
//! `config.json` or an environment variable - or the Devin CLI's to hold:
//! a signed-in CLI's `credentials.toml` and `config.json` are read when
//! neither is set. Nothing here reads a browser profile. The organization
//! selects the request; it is not a usage figure.

use std::hash::{Hash, Hasher};

use opscope_core as tc;

use crate::parse::{
    format_balance, parse_devin_cli_org, parse_devin_cli_token, parse_devin_quota, DevinQuota,
    DevinWindow,
};
use crate::shared::*;
use crate::*;

const HOST: &str = "https://app.devin.ai";

/// The environment variable the token is read from when `devin_token` is
/// empty and `devin_token_env` names nothing.
pub const TOKEN_ENV: &str = "DEVIN_BEARER_TOKEN";

/// Accepted as another name for the same variable. Not a second setting.
const TOKEN_ALIAS: &str = "DEVIN_AUTHORIZATION";

/// How long a reading is held. The windows move over a day, so five minutes
/// keeps the bars honest without asking on every refresh.
const REPORT_TTL: f64 = 300.0;

#[derive(Clone, Default)]
pub struct Data {
    quota: Option<DevinQuota>,
    read_at: f64,
    /// `config`, `env`, `cli`, or empty when no token was set.
    source: &'static str,
    why: String,
}

/// The bearer token, and where it came from. The setting wins, then the
/// named variable, then `DEVIN_AUTHORIZATION`, then the credential a
/// signed-in Devin CLI stored - the only local sign of Devin there is,
/// since it installs no binary and writes no usage cache.
pub fn token(cfg: &Config) -> (String, &'static str) {
    if !cfg.devin_token.trim().is_empty() {
        return (strip_bearer(cfg.devin_token.trim()), "config");
    }
    let name = if cfg.devin_token_env.trim().is_empty() {
        TOKEN_ENV
    } else {
        cfg.devin_token_env.trim()
    };
    for var in [name, TOKEN_ALIAS] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return (strip_bearer(&v), "env");
            }
        }
    }
    if cfg.devin_cli_credentials {
        if let Some(v) = cli_token(&cli_credentials_paths()) {
            return (strip_bearer(&v), "cli");
        }
    }
    (String::new(), "")
}

/// The organization the quota is read for: `devin_org` when set, else the
/// one the signed-in CLI named. An org id is not a secret - it selects
/// the request - so borrowing the CLI's is safe even when the token came
/// from config.
pub fn org(cfg: &Config) -> String {
    let named = cfg.devin_org.trim();
    if !named.is_empty() {
        return named.to_string();
    }
    if !cfg.devin_cli_credentials {
        return String::new();
    }
    cli_org(&cli_config_paths()).unwrap_or_default()
}

/// The places a Devin CLI leaves `credentials.toml`, in read order: the
/// XDG data root (its default on Linux), the macOS Application Support
/// dir, then %APPDATA% for Windows. Existence decides - the right answer
/// varies within a platform, so `cfg` has no part in it.
fn cli_credentials_paths() -> Vec<std::path::PathBuf> {
    cli_paths("XDG_DATA_HOME", ".local/share", "credentials.toml")
}

/// The same walk for the CLI's `config.json`, which lives under the
/// config root rather than the data root.
fn cli_config_paths() -> Vec<std::path::PathBuf> {
    cli_paths("XDG_CONFIG_HOME", ".config", "config.json")
}

fn cli_paths(xdg: &str, fallback: &str, file: &str) -> Vec<std::path::PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let root = std::env::var(xdg)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{home}/{fallback}"));
    let mut found = vec![std::path::PathBuf::from(format!("{root}/devin/{file}"))];
    if !home.is_empty() {
        found.push(format!("{home}/Library/Application Support/devin/{file}").into());
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.is_empty() {
            found.push(format!("{appdata}/devin/{file}").into());
        }
    }
    found
}

/// `windsurf_api_key` from the first readable credentials file that has
/// one. A file that parses to nothing does not stop the walk - an empty
/// credentials.toml next to a real install should not hide it.
fn cli_token(paths: &[std::path::PathBuf]) -> Option<String> {
    paths.iter().find_map(|p| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|text| parse_devin_cli_token(&text))
    })
}

fn cli_org(paths: &[std::path::PathBuf]) -> Option<String> {
    paths.iter().find_map(|p| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|text| parse_devin_cli_org(&text))
    })
}

/// A pasted `Authorization:` line or a `Bearer` prefix is the same token.
fn strip_bearer(raw: &str) -> String {
    let mut text = raw.trim();
    // `get` stops on a char boundary. A byte slice of a multibyte token
    // panics, and this runs before every other agent's poll.
    if text
        .get(..14)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("authorization:"))
    {
        text = text[14..].trim();
    }
    if text
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bearer "))
    {
        text = text[7..].trim();
    }
    text.to_string()
}

/// The quota URL, and the internal organization id when the path needs the
/// `x-cog-org-id` header.
///
/// An id that starts with `org-` or `org_` is the path segment and the
/// header. Anything else is `org/<slug>` with no header. One path: a list
/// of guesses would include an organization the reader did not name.
pub fn quota_target(org: &str) -> Result<(String, Option<String>), String> {
    let org = org.trim();
    if org.is_empty() {
        return Err("no organization · set devin_org.".into());
    }
    if org.contains(['\r', '\n', ' ', '\t']) {
        return Err("the organization has a space or a line break in it".into());
    }
    let path = if let Some(rest) = org
        .strip_prefix("https://")
        .or_else(|| org.strip_prefix("http://"))
    {
        rest.split_once('/').map(|(_, path)| path).unwrap_or("")
    } else {
        org
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix("/billing/quota/usage").unwrap_or(path);
    let path = path.trim_matches('/');
    let path = path.strip_prefix("api/").unwrap_or(path);
    let id = path
        .strip_prefix("organizations/")
        .or_else(|| path.strip_prefix("org/"))
        .unwrap_or(path)
        .trim_matches('/');
    if id.is_empty() || id.contains('/') || id.contains("..") {
        return Err("the organization is not a single path segment".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("the organization has a character this widget will not put in a URL".into());
    }
    if id.starts_with("org-") || id.starts_with("org_") {
        Ok((
            format!("{HOST}/api/{id}/billing/quota/usage"),
            Some(id.to_string()),
        ))
    } else {
        Ok((format!("{HOST}/api/org/{id}/billing/quota/usage"), None))
    }
}

fn cache_key(token: &str, org: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (token, org).hash(&mut hasher);
    format!("devin:{:016x}", hasher.finish())
}

fn ask(token: &str, org: &str) -> Result<serde_json::Value, String> {
    if token.contains(['\r', '\n']) {
        return Err("the token has a line break in it · paste it again as one line".into());
    }
    if token.is_empty() {
        return Err(format!("no token · set devin_token or {TOKEN_ENV}."));
    }
    let (url, org_header) = quota_target(org)?;
    let auth = format!("Bearer {token}");
    let mut headers = vec![
        ("Accept", "application/json"),
        ("Authorization", auth.as_str()),
    ];
    if let Some(id) = org_header.as_deref() {
        headers.push(("x-cog-org-id", id));
    }
    // `tc::get` uses curl `--fail`, so a 401 body — including the
    // "no organizations" detail — never arrives. Both refusals are the
    // token, which is the part the reader can replace.
    let body = tc::get(&url, &headers, 15).map_err(|said| {
        if status_of(&said).is_some_and(|code| code == 401 || code == 403) {
            "token rejected · copy a fresh bearer token".to_string()
        } else {
            format!("quota {}", refusal(&said))
        }
    })?;
    if parse_devin_quota(&body).is_none() {
        return Err("Devin's answer had no daily or weekly quota.".into());
    }
    Ok(serde_json::json!({ "body": body, "at": now() }))
}

fn status_of(said: &str) -> Option<u16> {
    said.rsplit_once("error: ")
        .and_then(|(_, tail)| tail.split_whitespace().next())
        .and_then(|code| code.parse().ok())
}

/// `shown` is whether Devin has a tab the reader chose. A tab the fallback
/// brought back was not a choice, and nothing is sent.
pub fn read(caches: &mut Caches, cfg: &Config, shown: bool) -> Data {
    let mut data = Data::default();
    if !shown {
        data.why = "not asked · Devin is left out of this widget's agents".into();
        return data;
    }
    let (token, source) = token(cfg);
    data.source = source;
    let org = org(cfg);
    if token.is_empty() {
        return data;
    }
    if org.is_empty() {
        data.why = "no organization · set devin_org.".into();
        return data;
    }
    let key = cache_key(&token, &org);
    let mut refused = String::new();
    let mut got = None;
    for _ in 0..2 {
        got = cached(caches, &key, REPORT_TTL, || match ask(&token, &org) {
            Ok(value) => Some(value),
            Err(why) => {
                refused = why;
                None
            }
        });
        if !got.as_ref().is_some_and(reset_passed) {
            break;
        }
        caches.live.remove(&key);
    }
    remember_refusal(caches, &key, &refused);
    if !refused.is_empty() {
        data.why = refused;
        return data;
    }
    let Some(got) = got else {
        return data;
    };
    data.why = text(&got, "why");
    data.quota = parse_devin_quota(&text(&got, "body"));
    data.read_at = num(&got, "at");
    data
}

fn reset_passed(got: &serde_json::Value) -> bool {
    let at = num(got, "at");
    let data = Data {
        quota: parse_devin_quota(&text(got, "body")),
        read_at: at,
        ..Data::default()
    };
    // A reset already behind `at` was past when the body was read. Dropping
    // the cache for it fetches again on every frame and still shows the
    // same instant. A reset that was ahead of the reading and has now
    // passed is a window that rolled.
    lanes(&data)
        .iter()
        .any(|lane| lane.reset.is_some_and(|reset| reset > at && reset <= now()))
}

/// Daily and weekly, each with the reset that was sent and no window
/// length. The balance is not a lane: a bar needs a ceiling, and the body
/// has none.
pub fn lanes(data: &Data) -> Vec<Lane> {
    let Some(quota) = data.quota.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(window) = &quota.daily {
        out.push(lane("daily", window));
    }
    if let Some(window) = &quota.weekly {
        out.push(lane("weekly", window));
    }
    out
}

fn lane(label: &str, window: &DevinWindow) -> Lane {
    Lane {
        label: label.into(),
        pct: window.pct,
        // The body has a reset instant and no length. A 24h or 7-day pace
        // would be a length this response did not carry.
        window_secs: None,
        reset: window.reset,
        stale: window.reset.is_some_and(|reset| reset <= now()),
        projected: false,
        apart: false,
    }
}

pub fn why_no_lane(data: &Data) -> String {
    if !lanes(data).is_empty() {
        return String::new();
    }
    if !data.why.is_empty() {
        return format!("no quota · {}", data.why);
    }
    if data.source.is_empty() {
        return tc::missing_config(&format!(
            "no quota · no token · set agent_usage.devin_token, {TOKEN_ENV}, or \
             {TOKEN_ALIAS}, or sign in with the Devin CLI."
        ));
    }
    "no quota · no reading from Devin yet.".into()
}

fn quota_rows_with_plots(data: &Data, quota: &DevinQuota, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── QUOTA ── ".into()),
            (p.ok.as_str(), "live".into()),
            (p.dim.as_str(), " · account-wide".into()),
        ],
        w - 1,
    )];
    let hue = agent_hue("devin");
    let drawn = lanes(data);
    let label_w = drawn
        .iter()
        .map(|lane| lane.label.chars().count())
        .max()
        .unwrap_or(6)
        .max(6);
    for lane in &drawn {
        tc::append_plot_rows(&mut rows, plots, |plots| bar_row_with_plots(lane, label_w, hue, w, p, plots));
    }
    if quota.daily_hidden {
        rows.extend(note("Daily quota is hidden.", &p.dim, w));
    } else if quota.daily.is_none() {
        rows.extend(note(
            "Devin's answer had no daily quota, so it is not shown.",
            &p.warn,
            w,
        ));
    }
    if quota.weekly.is_none() {
        rows.extend(note(
            "Devin's answer had no weekly quota, so it is not shown.",
            &p.warn,
            w,
        ));
    }
    if drawn.iter().any(|lane| lane.window_secs.is_none()) {
        rows.extend(note(
            "Devin did not send a window length, so these rows have a reset and no pace.",
            &p.dim,
            w,
        ));
    }
    if let Some(balance) = quota.balance {
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

fn bar_row_with_plots(
    lane: &Lane,
    label_w: usize,
    hue: Option<(u8, u8, u8)>,
    w: usize,
    p: &Palette,
    plots: &mut Vec<tc::Plot>,
) -> Vec<String> {
    let used = (lane.pct / 100.0).clamp(0.0, 1.0);
    let room = ((w as i64) - 38 - label_w as i64).max(8) as usize;
    let mut line: Vec<(String, String)> = vec![(
        p.dim.clone(),
        format!(" {} ", tc::pad(&lane.label, label_w)),
    )];
    line.extend(paced_bar_with_plot(plots, 0, &line,
        used,
        elapsed_of(lane.window_secs, lane.reset),
        room,
        hue,
        p,
    ));
    line.push((quota_mark(lane.pct, hue, p, lane.stale, lane.projected), pct_text(lane.pct)));
    line.push(pace_cell(lead(lane.pct, lane.window_secs, lane.reset), p));
    let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
    let mut rows = vec![tc::seg(&refs, w - 1)];
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
    rows
}

fn account_rows(quota: &DevinQuota, w: usize, p: &Palette) -> Vec<String> {
    let Some(plan) = quota.plan.as_deref() else {
        return Vec::new();
    };
    plan_rows(plan, &[], w, "", None, "", p)
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

pub fn tab_with_plots(data: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    match data.quota.as_ref() {
        Some(quota) => {
            let mut rows = quota_rows_with_plots(data, quota, w, p, plots);
            rows.extend(token_warning(data, w, p));
            add_section(rows, account_rows(quota, w, p))
        }
        None => {
            let what = if data.source.is_empty() && data.why.is_empty() {
                tc::missing_config(&format!(
                    "No Devin token. Set agent_usage.devin_token and agent_usage.devin_org, or \
                     export {TOKEN_ENV}. {TOKEN_ALIAS} is read when that variable is empty, \
                     and a signed-in Devin CLI's own credentials.toml and config.json are read \
                     after that (agent_usage.devin_cli_credentials = false stops it). \
                     Both are sent only to app.devin.ai, and only while Devin is one of the \
                     agents shown. Nothing is read from a browser. Keep config.json chmod 600."
                ))
            } else {
                why_no_lane(data)
                    .trim_start_matches("no quota · ")
                    .to_string()
            };
            let mut rows = no_local(&what, "", w, p);
            rows.extend(token_warning(data, w, p));
            rows
        }
    }
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

    fn reading(raw: &str) -> Data {
        Data {
            quota: parse_devin_quota(raw),
            read_at: now() - 60.0,
            source: "config",
            ..Data::default()
        }
    }

    #[test]
    fn an_internal_id_is_the_path_and_a_slug_is_under_org() {
        let (url, header) = quota_target("org-acme").unwrap();
        assert_eq!(url, "https://app.devin.ai/api/org-acme/billing/quota/usage");
        assert_eq!(header.as_deref(), Some("org-acme"));
        let (url, header) = quota_target("https://app.devin.ai/org/acme").unwrap();
        assert_eq!(url, "https://app.devin.ai/api/org/acme/billing/quota/usage");
        assert!(header.is_none());
        let (url, header) = quota_target("organizations/org_acme").unwrap();
        assert!(url.contains("/api/org_acme/"));
        assert_eq!(header.as_deref(), Some("org_acme"));
        assert!(quota_target("org/../other").is_err());
    }

    #[test]
    fn a_pasted_authorization_line_is_the_token() {
        let cfg = Config {
            devin_token: "  Authorization: Bearer abc  ".into(),
            ..Config::default()
        };
        assert_eq!(token(&cfg), ("abc".to_string(), "config"));
    }

    #[test]
    fn a_multibyte_token_is_left_whole() {
        let cfg = Config {
            devin_token: "éééééééé".into(),
            ..Config::default()
        };
        assert_eq!(token(&cfg).0, "éééééééé");
    }

    #[test]
    fn a_reset_already_past_at_the_reading_does_not_drop_the_cache() {
        let at = now() - 120.0;
        let already = serde_json::json!({
            "daily_percentage": 10.0,
            "daily_reset_at": at - 10.0,
        });
        let held = serde_json::json!({ "body": already.to_string(), "at": at });
        assert!(!reset_passed(&held));
        let rolled = serde_json::json!({
            "weekly_percentage": 10.0,
            "weekly_reset_at": at + 30.0,
        });
        let crossed = serde_json::json!({ "body": rolled.to_string(), "at": at });
        assert!(reset_passed(&crossed));
    }

    #[test]
    fn a_hidden_daily_is_not_drawn_as_zero_and_the_balance_has_no_bar() {
        let data = reading(
            r#"{"daily_percentage":40,"weekly_percentage":10,"hide_daily_quota":true,
                "weekly_reset_at":4102444800,"overage_balance":12.5,"plan":"pro_plus"}"#,
        );
        let lanes = lanes(&data);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].label, "weekly");
        assert!(lanes[0].window_secs.is_none());
        assert!(why_no_lane(&data).is_empty());
        let rows = plain(&tab(&data, 100, 30, &Config::default(), &palette()));
        assert!(!rows.contains("daily"), "{rows}");
        assert!(!rows.contains("40%"), "{rows}");
        assert!(rows.contains("10%"), "{rows}");
        assert!(rows.contains("extra usage balance  12.5"), "{rows}");
        assert!(!rows.contains('$'), "{rows}");
        assert!(!rows.contains('+'), "{rows}");
        assert!(rows.contains("pro_plus"), "{rows}");
        assert!(rows.contains("Daily quota is hidden"), "{rows}");
    }

    #[test]
    fn a_sent_zero_is_drawn_and_a_missing_weekly_is_not() {
        let data = reading(r#"{"daily_percentage":0,"hide_daily_quota":false}"#);
        assert_eq!(lanes(&data).len(), 1);
        assert_eq!(lanes(&data)[0].pct, 0.0);
        let rows = plain(&tab(&data, 80, 24, &Config::default(), &palette()));
        assert!(rows.contains("0%"), "{rows}");
        assert!(rows.contains("no weekly quota"), "{rows}");
        assert!(!rows.contains("extra usage balance"), "{rows}");
    }

    #[test]
    fn with_no_token_nothing_is_asked() {
        let cfg = Config {
            devin_token_env: "OPSCOPE_TEST_NO_SUCH_VARIABLE".into(),
            devin_org: "acme".into(),
            ..Config::default()
        };
        let mut caches = Caches::default();
        // The alias is a real variable name. An empty process env is the
        // case under test; a value set outside the test would be a token.
        std::env::remove_var(TOKEN_ALIAS);
        let data = read(&mut caches, &cfg, true);
        assert!(caches.live.keys().all(|key| !key.starts_with("devin")));
        assert!(
            why_no_lane(&data).contains("devin_token"),
            "{}",
            why_no_lane(&data)
        );
    }

    #[test]
    fn a_devin_without_a_tab_is_never_asked() {
        let cfg = Config {
            devin_token: "secret".into(),
            devin_org: "acme".into(),
            ..Config::default()
        };
        let mut caches = Caches::default();
        let data = read(&mut caches, &cfg, false);
        assert!(caches.live.keys().all(|key| !key.starts_with("devin")));
        assert!(why_no_lane(&data).contains("left out"));
    }

    #[test]
    fn a_signed_in_clis_files_supply_token_and_org() {
        let dir = std::env::temp_dir().join(format!("opscope-devin-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cred = dir.join("credentials.toml");
        let conf = dir.join("config.json");
        std::fs::write(
            &cred,
            "windsurf_api_key = \"cli-token\"\napi_server_url = \"https://x\"\n",
        )
        .unwrap();
        std::fs::write(&conf, r#"{"devin":{"org_id":"org-cli"},"version":1}"#).unwrap();
        assert_eq!(cli_token(&[cred.clone()]).as_deref(), Some("cli-token"));
        assert_eq!(cli_org(&[conf.clone()]).as_deref(), Some("org-cli"));
        // A file with no key does not stop the walk.
        let empty = dir.join("empty.toml");
        std::fs::write(&empty, "# nothing here\n").unwrap();
        assert_eq!(
            cli_token(&[empty, cred]).as_deref(),
            Some("cli-token")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_written_token_still_wins_over_the_cli() {
        // `devin_cli_credentials` is on for this one: the config token must
        // still be the answer, without any CLI file being opened.
        let cfg = Config {
            devin_token: "config-token".into(),
            devin_cli_credentials: true,
            ..Config::default()
        };
        assert_eq!(token(&cfg), ("config-token".to_string(), "config"));
    }

    #[test]
    fn the_cli_flag_off_is_the_only_way_tests_stay_away_from_real_files() {
        // `Config::default()` carries `devin_cli_credentials: false`, so
        // this would hold even on a machine whose CLI is signed in - the
        // runtime loader is what turns it on.
        let cfg = Config {
            devin_token_env: "OPSCOPE_TEST_NO_SUCH_VARIABLE".into(),
            ..Config::default()
        };
        std::env::remove_var(TOKEN_ALIAS);
        assert!(token(&cfg).0.is_empty());
        assert!(org(&cfg).is_empty());
    }

    #[test]
    fn a_token_without_an_organization_is_not_sent() {
        let cfg = Config {
            devin_token: "secret".into(),
            ..Config::default()
        };
        let mut caches = Caches::default();
        let data = read(&mut caches, &cfg, true);
        assert!(caches.live.keys().all(|key| !key.starts_with("devin")));
        assert!(why_no_lane(&data).contains("devin_org"));
    }
}

#[cfg(test)]
pub fn tab(data: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    tab_with_plots(data, w, _h, _cfg, p, &mut Vec::new())
}
