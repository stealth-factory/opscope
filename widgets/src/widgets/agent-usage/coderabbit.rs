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

//! CodeRabbit: reviews this billing period, from its own CLI.
//!
//! `coderabbit usage` is the only source. It answers with a review count,
//! whether usage billing is on, and when the period resets. From CLI 0.8 it
//! also gives the included reviews left in the rolling window, the window,
//! and when capacity returns, which the tab draws. `[+]` does not: the
//! report does not say which of CodeRabbit's allowances it is (see
//! `lanes`), so CodeRabbit is named there as publishing no quota rather
//! than drawn as a limit it may not be. The plan's documented hourly rates are
//! never drawn in its place: a limit the CLI did not give is not a reading.
//! The CLI owns the login; nothing here reads or touches its credentials.

use chrono::{Local, NaiveDate};
use opscope_core as tc;

use crate::parse::{
    coderabbit_quota_field, coderabbit_signed_out, parse_coderabbit_usage, CodeRabbitUsage,
};
use crate::shared::*;
use crate::*;

const CLI: &str = "coderabbit";

/// How long a report is held. A review count moves a few times a day, and
/// each ask is a round trip to CodeRabbit on the reader's login.
const REPORT_TTL: f64 = 600.0;

/// The fields the tab lays out itself; any other line the report carries
/// is listed after them as it came.
const PLACED: &[&str] = &["your reviews", "period resets", "organization", "user", "plan"];

#[derive(Clone, Default)]
pub struct Data {
    usage: Option<CodeRabbitUsage>,
    /// When the report was taken.
    read_at: f64,
    why: String,
}

/// One `coderabbit usage`, as the report or as why there was none.
fn ask(repo: &str) -> Result<serde_json::Value, String> {
    let dir = repo_dir(repo);
    let out = tc::run_full_in(&[CLI, "usage"], 15, dir.as_deref())?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // A partial report from a run that then failed is not a report: taking
    // it would hold a failure as a reading for the full ten minutes.
    if out.status.success() && parse_coderabbit_usage(&text).is_some() {
        return Ok(serde_json::json!({"text": text, "at": now()}));
    }
    Err(if coderabbit_signed_out(&text) {
        "not signed in · run coderabbit auth login".to_string()
    } else if !out.status.success() {
        format!("coderabbit usage exited {}", out.status)
    } else {
        "coderabbit usage printed no report this widget can read".to_string()
    })
}

/// Where to run `coderabbit usage`: `coderabbit_repo` with a leading `~`
/// read as home, or nowhere in particular when it is empty.
fn repo_dir(repo: &str) -> Option<std::path::PathBuf> {
    let repo = repo.trim();
    if repo.is_empty() {
        return None;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    Some(match repo.strip_prefix("~/") {
        Some(rest) => std::path::Path::new(&home).join(rest),
        None if repo == "~" => home.into(),
        None => repo.into(),
    })
}

/// `shown` is whether CodeRabbit has a tab under the reader's settings.
///
/// Asked only where it could matter: the CLI is installed and the reader
/// has a tab for it. Every other agent here reads a file or an endpoint;
/// this one starts a program that spends a request on the reader's login,
/// so a fixed `agents` list without it, or `exclude_agents` with it, means
/// it is never run.
pub fn read(caches: &mut Caches, shown: bool, repo: &str) -> Data {
    let mut d = Data::default();
    if !shown {
        // Can still be drawn: excluding every chosen agent brings all the
        // tabs back, and this one must not then claim the CLI is missing.
        d.why = "not asked · CodeRabbit is left out of this widget's agents".into();
        return d;
    }
    if !tc::missing(&[CLI]).is_empty() {
        return d;
    }
    // A failure is held as a refusal, so it is retried on the backoff
    // rather than trusted for the full ten minutes a report is.
    let mut refused = String::new();
    let got = cached(caches, "coderabbit", REPORT_TTL, || match ask(repo) {
        Ok(v) => Some(v),
        Err(why) => {
            refused = why;
            None
        }
    });
    remember_refusal(caches, "coderabbit", &refused);
    if !refused.is_empty() {
        d.why = refused;
        return d;
    }
    let Some(got) = got else {
        return d;
    };
    d.why = text(&got, "why");
    if let Some(usage) = parse_coderabbit_usage(&text(&got, "text")) {
        d.usage = Some(usage);
        d.read_at = num(&got, "at");
    }
    d
}

/// The rolling window's name on a lane: `hour` for the documented one.
fn window_label(secs: Option<f64>) -> String {
    match secs {
        Some(s) if (s - 3600.0).abs() < 1.0 => "hour".into(),
        Some(s) if s >= 3600.0 && s % 3600.0 == 0.0 => format!("{}h", s / 3600.0),
        Some(s) if s >= 60.0 => format!("{}m", (s / 60.0).round()),
        _ => "rolling".into(),
    }
}

/// None on `[+]`. The allowance `coderabbit usage` reports is real, but it
/// does not say which of CodeRabbit's separate PR, CLI and IDE allowances
/// it is, and it read `10 of 10` while pull request reviews were held to 4
/// an hour. On the summary it would sit beside every other agent's limit
/// and read as the one that stops reviews, empty while that one is spent.
/// The tab draws it, where there is room to say what it covers.
pub fn lanes(_d: &Data) -> Vec<Lane> {
    Vec::new()
}

/// The included reviews used of the rolling window, when the CLI gave both
/// what is left and out of how many. A count with no limit has nothing to
/// be a share of, and draws nothing.
fn allowance(d: &Data) -> Vec<Lane> {
    let Some(u) = &d.usage else {
        return Vec::new();
    };
    let Some((left, Some(of))) = u.available() else {
        return Vec::new();
    };
    let used = of.saturating_sub(left) as f64;
    let window = u.window_secs();
    // Capacity returns review by review as old ones age out, so there is a
    // reset only when the CLI names one - which it does when none are left.
    let reset = u.returns_at(d.read_at);
    vec![Lane {
        label: window_label(window),
        pct: used / of as f64 * 100.0,
        // Left out on purpose. A rolling window has no start, so the pace
        // marker, which reads elapsed time back from the reset, would put
        // a start where there is none and draw a pace that is not real.
        window_secs: None,
        reset,
        // A return time since the reading means the count has moved on, and
        // so does a reading older than this widget's own ten-minute cycle.
        // Inside the cycle it is this widget's reading, which the other
        // agents do not flag either, and the tab says how old it is.
        stale: reset.is_some_and(|r| r <= now()) || now() - d.read_at > REPORT_TTL,
        projected: false,
        apart: false,
    }]
}

/// Why `[+]` has no bar for CodeRabbit.
///
/// A report that arrived is not the reader's to fix, so it uses the
/// "answered, and published no" wording that keeps it out of the warning
/// colour. A failed ask is, and says what failed.
pub fn why_no_lane(d: &Data) -> String {
    if let Some(u) = &d.usage {
        let count = u
            .reviews()
            .map(|n| format!(" · {} reviews this period", n))
            .unwrap_or_default();
        if !allowance(d).is_empty() {
            return format!(
                "no quota · CodeRabbit answered, and published no limit it says covers pull \
                 request reviews · its tab has the rolling allowance{}.",
                count
            );
        }
        if let Some(why) = u.unavailable_why() {
            return format!(
                "no quota · CodeRabbit could not check the included reviews: {}{} · set \
                 coderabbit_repo to a git repository.",
                why, count
            );
        }
        if let Some((left, None)) = u.available() {
            return format!(
                "no quota · CodeRabbit answered, and published no limit for the {} \
                 reviews left{}.",
                left, count
            );
        }
        return format!(
            "no quota · CodeRabbit answered, and published no limit{} · coderabbit CLI \
             0.8 or later reports the rolling allowance.",
            count
        );
    }
    if !d.why.is_empty() {
        return format!("no quota · {}", d.why);
    }
    "no quota · the coderabbit CLI is not installed on this machine.".into()
}

/// Days from today to a bare `YYYY-MM-DD`, reckoned in this machine's zone
/// because the CLI gives the date with none.
fn days_until(date: &str, today: NaiveDate) -> Option<i64> {
    let day = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok()?;
    Some((day - today).num_days())
}

/// The rolling allowance, drawn as another agent's `[+]` lane is: a bar of the share
/// used, with how many are left in words, since a share of five reviews
/// reads better as a count.
fn allowance_rows(d: &Data, u: &CodeRabbitUsage, w: usize, p: &Palette) -> Vec<String> {
    let lanes = allowance(d);
    let Some(lane) = lanes.first() else {
        return Vec::new();
    };
    let Some((left, Some(of))) = u.available() else {
        return Vec::new();
    };
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── ALLOWANCE ── ".into()),
            (p.dim.as_str(), format!("included reviews · per developer · read {} ago", ago(d.read_at))),
        ],
        w - 1,
    )];
    let hue = agent_hue("coderabbit");
    let label_w = 7;
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
        Some(left) if left > 0.0 => format!(" · back in {}", left_span(left)),
        Some(_) => " · returning".into(),
        None => String::new(),
    };
    // CodeRabbit names the repository it read the allowance in, which is
    // the one coderabbit_repo points at; saying so shows where it came from.
    let repo = u.get("repository").map(|r| format!(" · in {}", r)).unwrap_or_default();
    rows.push(tc::seg(
        &[
            (p.dim.as_str(), format!(" {}  ", " ".repeat(label_w))),
            (p.txt.as_str(), format!("{} of {}", left, of)),
            (p.dim.as_str(), format!(" left{}{}", when, repo)),
        ],
        w - 1,
    ));
    // The report does not say which allowance this is, and pull request
    // reviews have been held to fewer while it read full; see `lanes`.
    let caveat = "CodeRabbit keeps pull request, CLI and IDE reviews on separate allowances, \
                  and does not say which this is; pull request reviews may be limited sooner.";
    let indent = " ".repeat(label_w + 3);
    let room = w.saturating_sub(indent.len() + 1).max(8);
    for line in tc::wrap_words(caveat, room) {
        rows.push(tc::seg(&[(p.dim.as_str(), format!("{}{}", indent, line))], w - 1));
    }
    rows
}

fn report_rows(d: &Data, u: &CodeRabbitUsage, w: usize, p: &Palette) -> Vec<String> {
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── REVIEWS ── ".into()),
            (p.dim.as_str(), format!("this billing period · read {} ago", ago(d.read_at))),
        ],
        w - 1,
    )];
    let count = u
        .reviews()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "—".into());
    rows.push(tc::seg(
        &[
            (p.dim.as_str(), "  your reviews  ".into()),
            (p.txt.as_str(), count),
            (
                p.dim.as_str(),
                if allowance(d).is_empty() { "   no limit published" } else { "" }.into(),
            ),
        ],
        w - 1,
    ));
    if let Some(date) = u.get("period resets") {
        let when = match days_until(date, Local::now().date_naive()) {
            Some(n) if n > 1 => format!(" · in {} days", n),
            Some(1) => " · tomorrow".into(),
            Some(0) => " · today".into(),
            _ => String::new(),
        };
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "  period resets ".into()),
                (p.txt.as_str(), date.to_string()),
                (p.dim.as_str(), when),
            ],
            w - 1,
        ));
    }
    rows
}

/// `drawn` is whether the ALLOWANCE section showed the quota lines; when it
/// did not, a count left with no limit is still listed here as it came.
fn plan(u: &CodeRabbitUsage, drawn: bool, w: usize, p: &Palette) -> Vec<String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for key in ["organization", "user"] {
        if let Some(v) = u.get(key) {
            pairs.push((key.into(), v.to_string()));
        }
    }
    for (k, v) in &u.fields {
        let beside_the_bar = coderabbit_quota_field(k).is_some() || k == "repository";
        if !PLACED.contains(&k.as_str()) && !(drawn && beside_the_bar) {
            pairs.push((k.clone(), v.clone()));
        }
    }
    plan_rows(u.get("plan").unwrap_or(""), &pairs, w, "", None, "", p)
}

pub fn tab(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    let Some(u) = d.usage.as_ref() else {
        let what = if d.why.is_empty() {
            "The coderabbit CLI is not installed, so there is no report to read. \
             Install it from docs.coderabbit.ai/cli, then sign in."
                .to_string()
        } else {
            d.why.clone()
        };
        // The login command fixes a sign-in failure and nothing else.
        let run = if d.why.starts_with("not signed in") { run_hint("coderabbit") } else { "" };
        return no_local(&what, run, w, p);
    };
    let mut rows = allowance_rows(d, u, w, p);
    let drawn = !rows.is_empty();
    if drawn {
        rows.push(String::new());
    }
    rows.extend(report_rows(d, u, w, p));
    add_section(rows, plan(u, drawn, w, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> CodeRabbitUsage {
        parse_coderabbit_usage(
            "Organization  : Example Org\n\
             Usage billing : inactive\n\
             User          : example-user\n\
             Your reviews  : 25\n\
             Period resets : 2026-09-30\n",
        )
        .unwrap()
    }

    #[test]
    fn a_report_is_named_as_publishing_no_quota_without_a_warning() {
        let d = Data { usage: Some(report()), read_at: now(), why: String::new() };
        assert!(allowance(&d).is_empty());
        let note = why_no_lane(&d);
        assert!(note.contains("25 reviews this period"), "{note}");
        // The wording `[+]` reads to decide this is not the reader's to fix.
        assert!(note.contains("answered, and published no"), "{note}");
    }

    fn with_allowance(extra: &str) -> CodeRabbitUsage {
        parse_coderabbit_usage(&format!(
            "Your reviews      : 25\n\
             Available reviews : {}\n\
             Period resets     : 2026-09-30\n",
            extra
        ))
        .unwrap()
    }

    #[test]
    fn a_report_from_outside_a_repository_says_how_to_get_the_allowance() {
        // CLI 0.8 run anywhere but a repository gives the billing period and
        // CodeRabbit's own note; the pane passes both on and names the setting.
        let usage = parse_coderabbit_usage(
            "Availability : unavailable\nNote : Run from a git repository to check included \
             reviews.\nYour reviews : 94\nPeriod resets : 2026-10-06\n",
        );
        let d = Data { usage, read_at: now(), ..Data::default() };
        assert!(allowance(&d).is_empty());
        let note = why_no_lane(&d);
        assert!(note.contains("Run from a git repository"), "{}", note);
        assert!(note.contains("94 reviews this period"), "{}", note);
        assert!(note.contains("coderabbit_repo"), "{}", note);
    }

    #[test]
    fn the_allowance_names_the_repository_it_was_read_in() {
        // Beside the count, and not listed again among the plan fields.
        let usage = parse_coderabbit_usage(
            "Repository : example-org/example-repo\nRemaining : 10 of 10\nWindow : rolling 1 hour\n\
             Your reviews : 95\n",
        );
        let d = Data { usage, read_at: now(), ..Data::default() };
        let rows = tab(&d, 100, 40, &Config::default(), &palette());
        let found: Vec<&String> = rows.iter().filter(|r| r.contains("example-org/example-repo")).collect();
        assert_eq!(found.len(), 1, "{:#?}", rows);
        assert!(found[0].contains("10 of 10"), "{}", found[0]);
        assert!(found[0].contains(" left · in example-org/example-repo"), "{}", found[0]);
    }

    #[test]
    fn a_repository_path_reads_a_leading_tilde_as_home() {
        // Empty runs where the widget runs; `~/x` is under home, as typed.
        assert_eq!(repo_dir("  "), None);
        let home = std::env::var("HOME").unwrap_or_default();
        assert_eq!(repo_dir("~/src/x"), Some(std::path::Path::new(&home).join("src/x")));
        assert_eq!(repo_dir("/srv/x"), Some("/srv/x".into()));
    }

    #[test]
    fn a_rolling_allowance_is_drawn_as_the_share_used() {
        // Two of five left is sixty per cent used, over the hour the CLI named.
        let u = parse_coderabbit_usage(
            "Your reviews : 25\nAvailable reviews : 2 of 5\nRolling window : 1 hour\n",
        )
        .unwrap();
        let d = Data { usage: Some(u), read_at: now(), why: String::new() };
        let lanes = allowance(&d);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].label, "hour");
        assert!((lanes[0].pct - 60.0).abs() < 1e-9);
        // No window on the lane: a rolling window has no pace to draw.
        assert_eq!(lanes[0].window_secs, None);
        // Kept off `[+]`, which says where it is instead.
        assert!(super::lanes(&d).is_empty());
        let note = why_no_lane(&d);
        assert!(note.contains("answered, and published no"), "{note}");
        assert!(note.contains("its tab has the rolling allowance"), "{note}");
    }

    #[test]
    fn an_exhausted_allowance_says_when_capacity_returns() {
        // The return time is counted from when the report was taken.
        let read_at = now() - 60.0;
        let u = parse_coderabbit_usage(
            "Available reviews : 0 of 5\nRolling window : 1 hour\nCapacity returns : in 20m\n",
        )
        .unwrap();
        let d = Data { usage: Some(u), read_at, why: String::new() };
        let lane = allowance(&d).remove(0);
        assert!((lane.pct - 100.0).abs() < 1e-9);
        assert_eq!(lane.reset, Some(read_at + 1200.0));
        assert!(!lane.stale);
        let all = tab(&d, 80, 20, &Config::default(), &palette()).join("\n");
        assert!(all.contains("back in"), "{all}");
    }

    #[test]
    fn a_count_left_with_no_limit_draws_no_bar_and_says_why() {
        // Four left of an unstated limit is not a share of anything.
        let d = Data { usage: Some(with_allowance("4")), read_at: now(), why: String::new() };
        assert!(allowance(&d).is_empty());
        let note = why_no_lane(&d);
        assert!(note.contains("no limit for the 4 reviews left"), "{note}");
        assert!(note.contains("answered, and published no"), "{note}");
        // With no ALLOWANCE section, the tab still lists the line as it came.
        let all = tab(&d, 80, 30, &Config::default(), &palette()).join("\n");
        assert!(all.contains("available reviews"), "{all}");
    }

    #[test]
    fn a_reading_held_past_its_cycle_is_marked_stale() {
        // Older than the ten-minute hold, it is flagged however far off the return is.
        let fresh = Data { usage: Some(with_allowance("3 of 5")), read_at: now(), why: String::new() };
        assert!(!allowance(&fresh)[0].stale);
        let old = Data { read_at: now() - REPORT_TTL - 60.0, ..fresh };
        assert!(allowance(&old)[0].stale);
    }

    #[test]
    fn quota_lines_are_not_listed_again_among_the_plan_fields() {
        // The allowance has its own section; the plan list skips its lines.
        let d = Data { usage: Some(with_allowance("3 of 5")), read_at: now(), why: String::new() };
        let all = tab(&d, 80, 30, &Config::default(), &palette()).join("\n");
        assert_eq!(all.matches("available reviews").count(), 0, "{all}");
        assert!(all.contains("ALLOWANCE"), "{all}");
    }

    #[test]
    fn a_failed_ask_says_what_failed() {
        let d = Data { why: "not signed in · run coderabbit auth login".into(), ..Data::default() };
        assert!(why_no_lane(&d).contains("coderabbit auth login"));
        let rows = tab(&d, 60, 20, &Config::default(), &palette());
        assert!(!rows.is_empty());
    }

    #[test]
    fn a_coderabbit_without_a_tab_is_never_asked() {
        // Nothing is run and nothing is cached: no tab, no request.
        let mut caches = Caches::default();
        let d = read(&mut caches, false, "");
        assert!(d.usage.is_none());
        assert!(!caches.live.contains_key("coderabbit"));
        // And if its tab is drawn anyway, it says why rather than calling
        // an installed CLI missing.
        assert!(why_no_lane(&d).contains("left out"), "{}", why_no_lane(&d));
        let rows = tab(&d, 80, 20, &Config::default(), &palette()).join("\n");
        assert!(!rows.contains("not installed") && !rows.contains("auth login"), "{rows}");
    }

    #[test]
    fn only_a_sign_in_failure_offers_the_login_command() {
        let strip = |rows: Vec<String>| rows.join("\n");
        let signed_out = Data { why: "not signed in · run coderabbit auth login".into(), ..Data::default() };
        let other = Data { why: "coderabbit usage exited 2".into(), ..Data::default() };
        let cfg = Config::default();
        assert_eq!(strip(tab(&signed_out, 80, 20, &cfg, &palette())).matches("auth login").count(), 2);
        assert!(!strip(tab(&other, 80, 20, &cfg, &palette())).contains("auth login"));
    }

    #[test]
    fn the_reset_is_counted_in_days_from_today() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        assert_eq!(days_until("2026-09-30", today), Some(4));
        assert_eq!(days_until("2026-09-26", today), Some(0));
        assert_eq!(days_until("soon", today), None);
    }

    #[test]
    fn the_tab_fits_the_pane_and_keeps_unplaced_fields() {
        let strip = |s: &str| {
            let mut out = String::new();
            let mut chars = s.chars();
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
            out
        };
        let d = Data { usage: Some(report()), read_at: now() - 120.0, why: String::new() };
        let quota = Data { usage: Some(with_allowance("0 of 5")), ..d.clone() };
        for w in [24usize, 40, 60, 100] {
            for r in tab(&quota, w, 30, &Config::default(), &palette()) {
                assert!(tc::display_width(&strip(&r)) <= w - 1, "width {w}: {r:?}");
            }
        }
        for w in [40usize, 60, 100] {
            let rows: Vec<String> =
                tab(&d, w, 30, &Config::default(), &palette()).iter().map(|r| strip(r)).collect();
            for r in &rows {
                assert!(tc::display_width(r) <= w - 1, "width {w}: {r:?}");
            }
            let all = rows.join("\n");
            assert!(all.contains("your reviews  25"), "{all}");
            assert!(all.contains("2026-09-30"), "{all}");
            assert!(all.contains("usage billing"), "{all}");
            assert!(all.contains("Example Org"), "{all}");
        }
    }
}
