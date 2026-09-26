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
//! whether usage billing is on, and when the period resets - and no limit.
//! So this tab has a count and no bar, and on `[+]` CodeRabbit is named as
//! publishing no quota rather than drawn as an empty one. The CLI owns the
//! login; nothing here reads or touches its credentials.

use chrono::{Local, NaiveDate};
use opscope_core as tc;

use crate::parse::{coderabbit_signed_out, parse_coderabbit_usage, CodeRabbitUsage};
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
fn ask() -> Result<serde_json::Value, String> {
    let out = tc::run_full(&[CLI, "usage"], 15)?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if parse_coderabbit_usage(&text).is_some() {
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

pub fn read(caches: &mut Caches, cfg: &Config) -> Data {
    let mut d = Data::default();
    // Asked only where it could matter: the CLI is installed and the reader
    // has not excluded it. Every other agent here reads a file or an
    // endpoint; this one starts a program that spends a request on the
    // reader's login.
    if !tc::missing(&[CLI]).is_empty() || cfg.exclude_agents.iter().any(|a| a == "coderabbit") {
        return d;
    }
    // A failure is held as a refusal, so it is retried on the backoff
    // rather than trusted for the full ten minutes a report is.
    let mut refused = String::new();
    let got = cached(caches, "coderabbit", REPORT_TTL, || match ask() {
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

/// CodeRabbit never has a lane: it publishes a count, not a limit.
pub fn lanes(_d: &Data) -> Vec<Lane> {
    Vec::new()
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
        return format!("no quota · CodeRabbit answered, and published no limit{}.", count);
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
            (p.dim.as_str(), "   no limit published".into()),
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

fn plan(u: &CodeRabbitUsage, w: usize, p: &Palette) -> Vec<String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for key in ["organization", "user"] {
        if let Some(v) = u.get(key) {
            pairs.push((key.into(), v.to_string()));
        }
    }
    for (k, v) in &u.fields {
        if !PLACED.contains(&k.as_str()) {
            pairs.push((k.clone(), v.clone()));
        }
    }
    plan_rows(u.get("plan").unwrap_or(""), &pairs, w, "", None, "", p)
}

pub fn tab(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    let Some(u) = d.usage.as_ref() else {
        let what = if d.why.is_empty() {
            "The coderabbit CLI is not installed, so there is no report to read.".to_string()
        } else {
            d.why.clone()
        };
        return no_local(&what, run_hint("coderabbit"), w, p);
    };
    add_section(report_rows(d, u, w, p), plan(u, w, p))
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
        assert!(lanes(&d).is_empty());
        let note = why_no_lane(&d);
        assert!(note.contains("25 reviews this period"), "{note}");
        // The wording `[+]` reads to decide this is not the reader's to fix.
        assert!(note.contains("answered, and published no"), "{note}");
    }

    #[test]
    fn a_failed_ask_says_what_failed() {
        let d = Data { why: "not signed in · run coderabbit auth login".into(), ..Data::default() };
        assert!(why_no_lane(&d).contains("coderabbit auth login"));
        let rows = tab(&d, 60, 20, &Config::default(), &palette());
        assert!(!rows.is_empty());
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
