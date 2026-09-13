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

//! Pull request throughput across every account you can see.
//!
//! A port of github.py. Counted with aliased searches rather than by
//! reading nodes: a search connection returns at most 100 nodes a page, so
//! a busy fortnight lost everything past the hundredth record, while an
//! issueCount is exact at any volume and costs one rate-limit point per
//! request however many are packed into it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use chrono::{DateTime, Duration as Days, NaiveDate, Utc};
use opscope_core as tc;

/// The environment variable a GitHub token is read from when `token_env`
/// says nothing. Named once so the code and the schema cannot drift: the
/// settings screen draws its default from `settings.json`, and a screen
/// showing one string while the code falls back to another is a screen
/// describing a value nobody uses. `a_declared_token_env_matches_the_code`
/// holds the two together.
const TOKEN_ENV: &str = "GITHUB_TOKEN";

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "github",
    section: "github",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};

/// The two figures beside PR FLOW, and the only place their wording is
/// written down.
///
/// `last 24h` rather than `today`, because the window is rolling: at nine
/// in the morning a calendar day is three hours of evidence and reads as a
/// collapse in throughput. Opened first, merged second, so the column
/// repeats the chart's own grammar - opened above the axis, merged below.
const FIG_LABELS: [&str; 2] = ["opened · last 24h", "merged · last 24h"];

/// A chart narrower than this is a smudge rather than a chart, so the
/// figures stand down before it gets there. The chart is the section; the
/// figures are the addition.
const MIN_CHART: usize = 20;

/// The width to reserve on the right of PR FLOW, or zero to stand down.
///
/// PR FLOW spreads its days across the whole pane, so this is taken out of
/// `avail` *before* the days are spread - a column claimed afterwards would
/// land on top of bars already drawn. The figures come off where the chart
/// would have to lose days or fall under [`MIN_CHART`] to pay for them.
fn figure_col(avail: usize, days: usize) -> usize {
    let want = FIG_LABELS
        .iter()
        .map(|l| tc::display_width(l))
        .max()
        .unwrap_or(0)
        + 2;
    if avail >= want + days.max(MIN_CHART) {
        want
    } else {
        0
    }
}

/// How a pane divides between the chart and the figures: the columns the
/// days are spread across, then the columns reserved to their right.
///
/// One function for both PR FLOWs, and the only place the subtraction
/// happens - a chart that takes the column without giving up the width
/// draws bars the figures then sit on top of.
fn chart_split(w: usize, days: usize) -> (usize, usize) {
    let full = w.saturating_sub(3).max(10);
    let figw = figure_col(full, days);
    ((full - figw).max(10), figw)
}

/// Three text rows tall, five pixel rows deep - the top half and the bottom
/// half of a cell are used as two pixel rows, so three rows of text carry
/// six and the digits stand exactly as tall as the three rows of bars they
/// sit beside.
///
/// Copied from `github-prs` rather than moved to core, on the same line
/// that keeps `latency` and `link` each holding their own braille canvas:
/// if the two diverge later, that is the point of keeping them apart.
const DIGITS: [[&str; 5]; 10] = [
    ["###", "# #", "# #", "# #", "###"],
    ["  #", "  #", "  #", "  #", "  #"],
    ["###", "  #", "###", "#  ", "###"],
    ["###", "  #", "###", "  #", "###"],
    ["# #", "# #", "###", "  #", "  #"],
    ["###", "#  ", "###", "  #", "###"],
    ["###", "#  ", "###", "# #", "###"],
    ["###", "  #", "  #", "  #", "  #"],
    ["###", "# #", "###", "# #", "###"],
    ["###", "# #", "###", "  #", "###"],
];

/// One number, three text rows tall. Every row is the same width.
fn big_digits(value: i64) -> Vec<String> {
    let shown = value.to_string();
    let mut rows = vec![String::new(); 3];
    for (i, ch) in shown.chars().enumerate() {
        let glyph = ch
            .to_digit(10)
            .map(|d| DIGITS[d as usize])
            .unwrap_or(["   ", "   ", "   ", "   ", "   "]);
        for (r, row) in rows.iter_mut().enumerate() {
            if i > 0 {
                row.push(' ');
            }
            for c in 0..3 {
                let lit = |pixels: Option<&&str>| {
                    pixels
                        .and_then(|line| line.as_bytes().get(c).copied())
                        .unwrap_or(b' ')
                        == b'#'
                };
                // The sixth pixel row does not exist, which is the gap that
                // keeps two stacked figures from touching.
                row.push(match (lit(glyph.get(r * 2)), lit(glyph.get(r * 2 + 1))) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                });
            }
        }
    }
    rows
}

/// The eight rows of the figure column, to sit against the eight rows of
/// the chart - three of bars, the axis rule, three of bars, the axis
/// labels.
///
/// A figure that has not arrived shimmers rather than drawing a zero. This
/// is the one place that difference is easy to lose: nothing opened in a
/// day is a real and unremarkable reading, and a pane still counting has to
/// look like a pane still counting.
fn figure_rows(
    opened: Option<i64>,
    merged: Option<i64>,
    width: usize,
    tick: usize,
    p: &Palette,
) -> Vec<Vec<(String, String)>> {
    let inner = width.saturating_sub(2);
    let gutter = || (tc::RST.to_string(), "  ".to_string());
    let mut out: Vec<Vec<(String, String)>> = Vec::new();
    for (n, value) in [opened, merged].into_iter().enumerate() {
        let colour = if n == 0 { p.pr.clone() } else { p.ok.clone() };
        match value {
            Some(v) => {
                let digits = big_digits(v);
                if digits.first().map(|r| r.chars().count()).unwrap_or(0) <= inner {
                    for line in digits {
                        out.push(vec![gutter(), (colour.clone(), line)]);
                    }
                } else {
                    // More digits than the column is wide. A truncated
                    // number is a wrong number, so it drops to plain text
                    // rather than being cut.
                    out.push(Vec::new());
                    out.push(vec![gutter(), (colour.clone(), v.to_string())]);
                    out.push(Vec::new());
                }
            }
            None => {
                out.push(Vec::new());
                let mut line = vec![gutter()];
                line.extend(tc::skeleton(inner.min(11).max(4), tick * 2, 5));
                out.push(line);
                out.push(Vec::new());
            }
        }
        out.push(vec![gutter(), (p.dim.clone(), FIG_LABELS[n].to_string())]);
    }
    out
}

/// One rolling-day figure off a payload, `None` where the alias did not
/// arrive.
///
/// Deliberately not `count_at`, which lands a missing alias on zero: a zero
/// is a reading, and the whole point of the skeleton beside it is that a
/// reading and an absence do not look alike.
fn figure_at(d: &serde_json::Value, key: &str) -> Option<i64> {
    d[key]["issueCount"].as_i64()
}

/// The board's figure: the sum across accounts, or `None`.
///
/// `watched` is how many accounts are configured, and it is the check that
/// matters, because `stats` carries only the accounts that have *arrived*.
/// Counting its rows is not the same as counting the accounts: on the first
/// pass it holds one row of ten with every field of it present, and summing
/// that would draw a tenth of the board as the whole of it. A sum missing a
/// member is a smaller number wearing the same label.
fn board_24h(stats: &[Account], watched: usize, pick: fn(&Account) -> Option<i64>) -> Option<i64> {
    if watched == 0 || stats.len() != watched || stats.iter().any(|s| pick(s).is_none()) {
        return None;
    }
    Some(stats.iter().filter_map(pick).sum())
}

/// Drop the rolling-day pair on a row that is not current for this pass.
///
/// `by_acc` is seeded from what is already on screen, so last pass's
/// `Some` values would otherwise survive: a later aggregate failure keeps
/// them, and a healthy pass publishes after each account and mixes this
/// cutoff with the previous one for accounts not yet queried. The rest of
/// the row stays — the table should not empty — but a mixture of 24h
/// figures wearing the same `last 24h` label is a smaller number wearing
/// a complete one, and the gate above cannot see the difference until
/// these two are gone.
fn forget_24h(row: &mut Account) {
    row.opened_24h = None;
    row.merged_24h = None;
}

/// `tc::seg` over segments that own their colours.
fn seg_owned(parts: &[(String, String)], w: usize) -> String {
    let borrowed: Vec<(&str, String)> =
        parts.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
    tc::seg(&borrowed, w)
}

/// One chart row with its figure fragment beside it.
///
/// Padded to `pad_to` first: the axis-label row is shorter than the bars
/// above it whenever `Nd ago` and `today` do not reach across the chart,
/// and without the pad the figure column would step sideways on that one
/// row.
fn with_figure(
    mut parts: Vec<(String, String)>,
    fig: Option<&Vec<(String, String)>>,
    pad_to: usize,
    w: usize,
) -> String {
    if let Some(fig) = fig {
        let have: usize = parts.iter().map(|(_, t)| tc::display_width(t)).sum();
        if have < pad_to {
            parts.push((tc::RST.to_string(), " ".repeat(pad_to - have)));
        }
        parts.extend(fig.iter().cloned());
    }
    seg_owned(&parts, w)
}

/// How many of the longest-open PRs an account's own screen names.
/// How long ago an ISO-8601 stamp was, coarse on purpose: "47d" answers the
/// question a queue raises and a timestamp does not.
fn age_since(iso: &str) -> String {
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(iso) else {
        return String::new();
    };
    let secs = (Utc::now() - at.with_timezone(&Utc)).num_seconds().max(0);
    if secs >= 86400 {
        format!("{}d", secs / 86400)
    } else {
        format!("{}h", secs / 3600)
    }
}

const OLDEST_WANTED: usize = 10;

const API: &str = "https://api.github.com/graphql";
const WINDOWS: &[i64] = &[7, 14, 30, 60, 90];
/// A full year, like the calendar on github.com.
const CONTRIB_WEEKS: i64 = 52;
/// Two searches a day; the alias ceiling sits between 60 and 90.
const DAY_CHUNK: usize = 20;
/// Trailing days to always refetch: today is still running, and the search
/// index lags a little behind a merge.
const FRESH_DAYS: usize = 2;
const SETTLE_FRAMES: usize = 8;
const WEEKDAYS: &[&str] = &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const GHOST: (u8, u8, u8) = (96, 106, 124);
const PR_RGB: (u8, u8, u8) = (180, 160, 255);
const OK_RGB: (u8, u8, u8) = (90, 240, 160);

/// A GitHub token from config.json or the environment.
///
/// Deliberately not shelled out to the `gh` CLI: this widget talks to the
/// API directly and should not require another program to be installed,
/// logged in and current just to read a number.
fn token(cfg: &serde_json::Value) -> (String, &'static str) {
    let from_config = tc::cfg_str(cfg, "token", "");
    if !from_config.is_empty() {
        return (from_config, "config");
    }
    let name = tc::cfg_str(cfg, "token_env", TOKEN_ENV);
    let name = if name.is_empty() { TOKEN_ENV.into() } else { name };
    match std::env::var(&name) {
        Ok(value) if !value.is_empty() => (value, "env"),
        _ => (String::new(), "missing"),
    }
}

/// The open PRs that have been open longest, for one account.
///
/// The board and the row are built entirely from `issueCount` aggregates,
/// which are exact at any volume and cost one rate-limit point per request
/// rather than per alias. That is the right shape for counting and useless
/// for naming: a queue of six hundred says nothing about which of them has
/// been sitting there since June.
///
/// So this asks for nodes, and only when an account's own screen is opened -
/// five of them, oldest first. One request, on demand, for the question the
/// aggregates cannot answer.
fn fetch_oldest(acc: &str, viewer: &str, tok: &str, scopes: &Arc<Mutex<Scopes>>) -> serde_json::Value {
    let q = scope_of(acc, viewer);
    let query = format!(
        r#"{{
  search(query:"{q} is:pr is:open sort:created-asc", type:ISSUE, first:{n}) {{
    nodes {{
      ... on PullRequest {{
        number
        title
        url
        createdAt
        isDraft
        repository {{ name }}
      }}
    }}
  }}
}}"#,
        q = q,
        n = OLDEST_WANTED
    );
    match graphql(&query, tok, scopes) {
        Ok(v) => v["data"]["search"]["nodes"].clone(),
        Err(e) => serde_json::json!({ "_error": e }),
    }
}

/// One account in full.
///
/// Everything here is already on the board somewhere - the row it came from
/// carries all of it - but the row has one line and has to choose. Open
/// splits into what is waiting on a reviewer and what is still a draft;
/// merged splits into what landed and what was closed unmerged; and the
/// flow chart, which the board draws once for every account added together,
/// is drawn here for this one alone. That last is the reason to open it: a
/// queue growing in one account is invisible in a total that six others
/// are also feeding.
///
/// No new request. The figures were fetched for the row.
/// Built at whatever height it needs, and the caller windows it.
///
/// It used to take the pane's height and drop the state bar, the oldest
/// list and the flow chart when what was left came to less than four rows,
/// four rows and ten - so a short pane showed some of the account and said
/// nothing about the rest. A section that is not drawn looks exactly like a
/// section with nothing in it, which is the opposite reading. Every section
/// is built now and the screen scrolls to reach them, which is also what
/// gives the wheel somewhere to go.
fn account_detail(
    a: &Account,
    oldest: Option<&serde_json::Value>,
    pick: usize,
    w: usize,
    tick: usize,
    p: &Palette,
) -> (Vec<String>, Option<usize>) {
    // Where the cursor over the oldest list ended up, so the caller can
    // scroll to it. The caller cannot work it out: how far down the page
    // that list starts depends on how many fields this account had.
    let mut cursor: Option<usize> = None;
    let mut rows = vec![tc::title(&a.account, w, &p.accent)];
    let label_w = 16usize;
    let mut field = |name: &str, value: String, aside: String, colour: &str| {
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!("  {}", tc::pad(name, label_w))),
                (colour, format!("{:>7}", value)),
                (p.dim.as_str(), format!("   {}", aside)),
            ],
            w - 1,
        ));
    };

    let waiting = a.review;
    let drafts = a.draft;
    field(
        "open",
        a.open.to_string(),
        match (waiting, drafts) {
            (0, 0) => String::new(),
            (r, 0) => format!("{} awaiting review", r),
            (0, d) => format!("{} draft", d),
            (r, d) => format!("{} awaiting review · {} draft", r, d),
        },
        p.pr.as_str(),
    );
    field("issues", a.issues.to_string(), String::new(), p.txt.as_str());

    // Opened against merged over the same window is the question the row
    // cannot answer: a queue of six hundred is a different thing depending
    // on whether it grew by forty this week or shrank by ten.
    let window_days = a.hist_window.unwrap_or(a.window).max(1);
    let opened_total: i64 = a.opened_hist.values().sum();
    let merged_total: i64 = a.hist.values().sum();
    let net = opened_total - merged_total;
    field(
        "opened",
        opened_total.to_string(),
        format!("in {}d", window_days),
        p.pr.as_str(),
    );

    let window = format!("in {}d", a.window);
    field(
        "merged",
        a.merged.to_string(),
        match a.dropped {
            0 => window.clone(),
            n => format!("{} · {} closed unmerged", window, n),
        },
        p.ok.as_str(),
    );
    if opened_total > 0 || merged_total > 0 {
        field(
            "net",
            format!("{:+}", net),
            match net {
                0 => "the queue held level".to_string(),
                n if n > 0 => format!("the queue grew by {}", n),
                n => format!("the queue shrank by {}", -n),
            },
            if net > 0 { p.warn.as_str() } else { p.ok.as_str() },
        );
    }
    if merged_total > 0 {
        let per_day = merged_total as f64 / window_days as f64;
        field(
            "merged/day",
            format!("{:.1}", per_day),
            // How long the open queue would take at the rate actually
            // observed. A number people usually estimate and get wrong.
            if per_day > 0.0 && a.open > 0 {
                format!("{:.0}d of open PRs at that rate", a.open as f64 / per_day)
            } else {
                String::new()
            },
            p.txt.as_str(),
        );
        let busiest = a.hist.iter().max_by_key(|(_, n)| **n);
        if let Some((day, n)) = busiest {
            if *n > 0 {
                field("busiest day", n.to_string(), day.clone(), p.dim.as_str());
            }
        }
        let idle = window_days as usize - a.hist.values().filter(|n| **n > 0).count();
        if idle > 0 {
            field(
                "days with none",
                idle.to_string(),
                format!("of {}", window_days),
                p.dim.as_str(),
            );
        }
    }
    if let Some(rate) = a.rate {
        let bar = tc::meter(rate / 100.0, w.saturating_sub(label_w + 22).clamp(6, 24));
        // The board's ramp, not a flat green: a screen where every account's
        // merge rate is the same colour whatever it is cannot warn at all,
        // and it disagreed with the figure two screens up.
        let hot = tc::health(rate / 100.0);
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!("  {}", tc::pad("merge rate", label_w))),
                (hot.as_str(), format!("{:>6.0}%", rate)),
                (p.dim.as_str(), "   ".into()),
                (hot.as_str(), bar),
            ],
            w - 1,
        ));
    }

    // The same bar the board draws for everything at once, for this account
    // alone: a queue is a different shape depending on whether it is waiting
    // on reviewers or waiting on authors.
    if a.open > 0 {
        let ready = (a.open - a.draft - a.review).max(0);
        let legend: Vec<(&str, i64, &str)> = [
            ("awaiting review", a.review, p.warn.as_str()),
            ("ready to merge", ready, p.ok.as_str()),
            ("draft", a.draft, p.dim.as_str()),
        ]
        .into_iter()
        .filter(|x| x.1 > 0)
        .collect();
        if !legend.is_empty() {
            rows.push(String::new());
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── OPEN PR STATE ── ".into()),
                    (p.dim.as_str(), "any age".into()),
                ],
                w - 1,
            ));
            let parts: Vec<(f64, String)> = legend
                .iter()
                .map(|(_, n, c)| (*n as f64 / a.open as f64, c.to_string()))
                .collect();
            let bar = tc::stacked_bar(&parts, w.saturating_sub(3).max(10));
            let mut line: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for (colour, txt) in &bar {
                line.push((colour.as_str(), txt.clone()));
            }
            rows.push(tc::seg(&line, w - 1));
            let mut key: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for (label, count, colour) in &legend {
                key.push((colour, "▇ ".into()));
                key.push((p.txt.as_str(), (*label).into()));
                key.push((
                    p.dim.as_str(),
                    format!(" {} ({:.0}%)   ", count, 100.0 * *count as f64 / a.open as f64),
                ));
            }
            rows.push(tc::seg(&key, w - 1));
        }
    }

    // The ones that have been open longest, which no count can name.
    {
        rows.push(String::new());
        match oldest {
            None => {
                rows.push(tc::seg(
                    &[(p.lbl.as_str(), " ── OLDEST OPEN ──".into())],
                    w - 1,
                ));
                rows.push(tc::seg(&[(p.dim.as_str(), "  asking…".into())], w - 1));
            }
            Some(v) if !v["_error"].is_null() => {
                rows.push(tc::seg(
                    &[(p.lbl.as_str(), " ── OLDEST OPEN ──".into())],
                    w - 1,
                ));
                rows.push(tc::seg(
                    &[(p.dim.as_str(), format!("  {}", v["_error"].as_str().unwrap_or("")))],
                    w - 1,
                ));
            }
            Some(v) => {
                let nodes = v.as_array().cloned().unwrap_or_default();
                rows.push(tc::seg(
                    &[
                        (p.lbl.as_str(), " ── OLDEST OPEN ── ".into()),
                        (
                            p.dim.as_str(),
                            if nodes.is_empty() {
                                "nothing open".to_string()
                            } else {
                                format!("{} longest waiting", nodes.len())
                            },
                        ),
                    ],
                    w - 1,
                ));
                for (i, node) in nodes.iter().enumerate() {
                    let here = i == pick.min(nodes.len().saturating_sub(1));
                    if here {
                        cursor = Some(rows.len());
                    }
                    let age = age_since(node["createdAt"].as_str().unwrap_or(""));
                    let repo = node["repository"]["name"].as_str().unwrap_or("").to_string();
                    let num = node["number"].as_i64().unwrap_or(0);
                    let draft = node["isDraft"].as_bool().unwrap_or(false);
                    let head = format!("  {:>5}  #{:<6}", age, num);
                    let room = w.saturating_sub(head.chars().count() + repo.chars().count() + 6);
                    let title = node["title"].as_str().unwrap_or("").to_string();
                    let title: String = if title.chars().count() > room {
                        format!("{}…", title.chars().take(room.saturating_sub(1)).collect::<String>())
                    } else {
                        title
                    };
                    let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
                    let c = |colour: &str| {
                // Same shape as the other widgets that do this, so one rule
                // reads them all: a guard per colour, each reaching its own
                // lighter twin.
                let colour = if tint.is_empty() {
                    colour
                } else if colour == p.dim {
                    p.dim_lit.as_str()
                } else {
                    colour
                };
                format!("{}{}", tint, colour)
            };
                    rows.push(tc::seg(
                        &[
                            (
                                &c(if here { p.accent.as_str() } else { p.dim.as_str() }),
                                if here { " ▸".into() } else { "  ".to_string() },
                            ),
                            (
                                &c(if draft { p.dim.as_str() } else { p.warn.as_str() }),
                                head.trim_start().to_string(),
                            ),
                            (&c(p.dim.as_str()), format!("  {}  ", repo)),
                            (&c(p.txt.as_str()), title),
                            (&tint, if here { " ".repeat(w) } else { String::new() }),
                        ],
                        w - 1,
                    ));
                }
            }
        }
    }

    // The same chart the board draws for every account at once, for this
    // one on its own.
    let want = a.hist_window.unwrap_or(a.window).max(1);
    let base = today();
    let mut days: Vec<String> = (0..want)
        .rev()
        .map(|n| (base - Days::days(n)).format("%Y-%m-%d").to_string())
        .collect();
    // The figure column comes out of the width before the days are spread,
    // exactly as on the board, and stands down at the same point.
    let (avail, figw) = chart_split(w, want as usize);
    if days.len() > avail {
        days = days[days.len() - avail..].to_vec();
    }
    let slot = (avail / days.len().max(1)).max(1);
    let gap = if slot >= 3 { 1 } else { 0 };
    let barw = slot - gap;
    let spread = |per_day: &[f64]| -> Vec<f64> {
        let mut cols = Vec::new();
        for (n, v) in per_day.iter().enumerate() {
            cols.extend(std::iter::repeat_n(*v, barw));
            if gap > 0 && n + 1 < per_day.len() {
                cols.extend(std::iter::repeat_n(0.0, gap));
            }
        }
        cols
    };
    let opened: Vec<f64> = days
        .iter()
        .map(|d| a.opened_hist.get(d).copied().unwrap_or(0) as f64)
        .collect();
    let merged: Vec<f64> = days
        .iter()
        .map(|d| a.hist.get(d).copied().unwrap_or(0) as f64)
        .collect();
    let (up, down) = (spread(&opened), spread(&merged));
    // One scale both ways, or the comparison lies.
    let hi = up
        .iter()
        .chain(down.iter())
        .cloned()
        .fold(0.0f64, f64::max)
        .max(1.0);
    if !up.is_empty() {
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── PR FLOW ── ".into()),
                (p.dim.as_str(), format!("{}d · ", days.len())),
                (p.pr.as_str(), format!("▲ {} opened", opened.iter().sum::<f64>() as i64)),
                (p.dim.as_str(), " · ".into()),
                (p.ok.as_str(), format!("▼ {} merged", merged.iter().sum::<f64>() as i64)),
                (p.dim.as_str(), format!("   peak {}/day", hi as i64)),
            ],
            w - 1,
        ));
        let figs = (figw > 0)
            .then(|| figure_rows(a.opened_24h, a.merged_24h, figw, tick, p));
        let fig = |n: usize| figs.as_ref().and_then(|f| f.get(n));
        let pad_to = 1 + up.len();
        for (n, line) in
            tc::vbars(&up.iter().map(|v| (*v, p.pr.clone())).collect::<Vec<_>>(), 3, hi)
                .into_iter()
                .enumerate()
        {
            let mut parts: Vec<(String, String)> = vec![(tc::RST.to_string(), " ".into())];
            for (colour, ch) in &line {
                parts.push((colour.clone(), ch.clone()));
            }
            rows.push(with_figure(parts, fig(n), pad_to, w - 1));
        }
        rows.push(with_figure(
            vec![
                (tc::RST.to_string(), " ".into()),
                (p.grid.clone(), "─".repeat(up.len())),
            ],
            fig(3),
            pad_to,
            w - 1,
        ));
        for (n, line) in
            tc::vbars_down(&down.iter().map(|v| (*v, p.ok.clone())).collect::<Vec<_>>(), 3, hi)
                .into_iter()
                .enumerate()
        {
            let mut parts: Vec<(String, String)> = vec![(tc::RST.to_string(), " ".into())];
            for (colour, ch) in &line {
                parts.push((colour.clone(), ch.clone()));
            }
            rows.push(with_figure(parts, fig(4 + n), pad_to, w - 1));
        }
        let left = format!("{}d ago", days.len());
        rows.push(with_figure(
            vec![
                (p.dim.clone(), format!(" {}", left)),
                (
                    p.dim.clone(),
                    format!(
                        "{:>width$}",
                        "today",
                        width = up.len().saturating_sub(left.chars().count() + 1)
                    ),
                ),
            ],
            fig(7),
            pad_to,
            w - 1,
        ));
    }
    (rows, cursor)
}

/// What the token is allowed to see, read off the response headers.
#[derive(Default, Clone)]
struct Scopes {
    seen: bool,
    have: Vec<String>,
}

fn graphql(
    query: &str,
    tok: &str,
    scopes: &Arc<Mutex<Scopes>>,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "query": query }).to_string();
    let (text, headers) = tc::post_json(
        API,
        &[
            ("Authorization", &format!("Bearer {}", tok)),
            ("Content-Type", "application/json"),
            ("User-Agent", "opscope"),
        ],
        &body,
        30,
    )?;
    for (name, value) in &headers {
        // Absent on fine-grained tokens, which is itself information: a
        // header that never arrives means the check cannot be made.
        if name == "x-oauth-scopes" {
            if let Ok(mut g) = scopes.lock() {
                g.seen = true;
                g.have = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    }
    let data: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    // GraphQL answers 200 with an `errors` array, so a successful request is
    // not a successful query. Read past it and every alias the query asked
    // for is missing, which `count_at` turns into zeros - and those zeros go
    // into the day cache as historical fact, so the affected days are never
    // retried and the chart shows a quiet week that never happened. linear
    // and pr both refuse this in the same place; github did not.
    if let Some(first) = data["errors"].as_array().and_then(|a| a.first()) {
        return Err(first["message"].as_str().unwrap_or("").chars().take(80).collect());
    }
    Ok(data)
}

/// Flag a token that will undercount rather than fail.
///
/// A classic token without `repo` still searches happily - it just returns
/// public results only, so every figure comes back smaller with nothing to
/// say it did. Without `read:org` the account list comes back short the
/// same way. Both are worse than an error, so name them.
fn scope_warning(scopes: &Scopes) -> String {
    if !scopes.seen {
        return String::new();
    }
    let missing: Vec<&str> = ["repo", "read:org"]
        .into_iter()
        .filter(|want| !scopes.have.iter().any(|had| had == want))
        .collect();
    if missing.is_empty() {
        return String::new();
    }
    let why = if missing.contains(&"repo") {
        "private repos are not counted"
    } else {
        "orgs cannot be discovered"
    };
    format!("token lacks {} - {}", missing.join(" and "), why)
}

/// The search qualifier that limits results to a single account.
fn scope_of(acc: &str, viewer: &str) -> String {
    if acc == "@me" {
        format!("user:{}", viewer)
    } else {
        format!("org:{}", acc)
    }
}

/// Exact per-day PR counts for one account.
fn build_day_query(q: &str, dates: &[String]) -> String {
    let mut parts = vec!["{".to_string()];
    for (n, day) in dates.iter().enumerate() {
        parts.push(format!(
            "\n  m{n}: search(query:\"{q} is:pr is:merged merged:{d}\", type:ISSUE) {{ issueCount }}\
             \n  c{n}: search(query:\"{q} is:pr created:{d}\", type:ISSUE) {{ issueCount }}",
            n = n,
            q = q,
            d = day
        ));
    }
    parts.push("\n}".into());
    parts.join("")
}

/// Metrics for one account in one request.
///
/// Eight aliased searches per account keeps each request within GitHub's
/// complexity limit - asking for seven accounts at once returned HTTP 502 -
/// while still being far fewer round trips than one query per metric. Six
/// was the measured ceiling; eight was re-measured against the largest
/// configured account before the two rolling-day aliases were added, and
/// an `issueCount` costs one rate-limit point per *request* however many
/// aliases ride in it.
///
/// `now` is a parameter rather than read inside, so the caller can pin
/// one cut for a whole account pass (and a test can pin a frozen one).
/// Reading it per account would give each a different 24h window, and the
/// board sum would then cover several slightly different intervals under
/// one `last 24h` label.
fn build_query(acc: &str, days: i64, viewer: &str, now: DateTime<Utc>) -> String {
    // N days *ending today*, so this spans exactly the dates the per-day
    // charts plot - `days` rather than `days - 1` would cover one day more
    // and quietly disagree with the chart drawn directly beneath it.
    let since = (today() - Days::days(days - 1)).format("%Y-%m-%d").to_string();
    // A rolling day, carrying its time of day. GitHub's search reads the
    // time part, so this cuts twenty-four hours back from now rather than
    // at the last midnight - a calendar day would read as a collapse in
    // throughput every morning.
    let day = (now - Days::hours(24))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let q = scope_of(acc, viewer);
    format!(
        r#"{{
  o0_open:    search(query:"{q} is:pr is:open", type:ISSUE) {{ issueCount }}
  o0_draft:   search(query:"{q} is:pr is:open draft:true", type:ISSUE) {{ issueCount }}
  o0_review:  search(query:"{q} is:pr is:open review:required", type:ISSUE) {{ issueCount }}
  o0_merged:  search(query:"{q} is:pr is:merged merged:>={s}", type:ISSUE) {{ issueCount }}
  o0_dropped: search(query:"{q} is:pr is:unmerged is:closed closed:>={s}", type:ISSUE) {{ issueCount }}
  o0_issues:  search(query:"{q} is:issue is:open", type:ISSUE) {{ issueCount }}
  o0_o24:     search(query:"{q} is:pr created:>={d}", type:ISSUE) {{ issueCount }}
  o0_m24:     search(query:"{q} is:pr is:merged merged:>={d}", type:ISSUE) {{ issueCount }}
  rateLimit {{ remaining limit }}
}}"#,
        q = q,
        s = since,
        d = day
    )
}

/// GitHub's own contribution calendar - the green squares.
///
/// contributionsCollection is per-viewer rather than per-org, so this is
/// your activity across everything, which is what the calendar means on
/// github.com.
fn contribution_query(weeks: i64) -> String {
    let since = (Utc::now() - Days::weeks(weeks))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    format!(
        r#"{{ viewer {{ contributionsCollection(from:"{}") {{
      contributionCalendar {{ totalContributions
        weeks {{ contributionDays {{ date contributionCount weekday }} }} }} }} }} }}"#,
        since
    )
}

/// The calendar day this machine is having, which is what a chart headed
/// "today" has to agree with.
fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

fn count_at(d: &serde_json::Value, key: &str) -> i64 {
    d[key]["issueCount"].as_i64().unwrap_or(0)
}

/// One account's row, and the two per-day series behind its chart.
#[derive(Clone, Default)]
struct Account {
    key: String,
    account: String,
    is_me: bool,
    /// Which window these figures cover, so a half-updated board cannot sum
    /// two windows together.
    window: i64,
    open: i64,
    draft: i64,
    review: i64,
    issues: i64,
    merged: i64,
    dropped: i64,
    rate: Option<f64>,
    hist: HashMap<String, i64>,
    opened_hist: HashMap<String, i64>,
    hist_window: Option<i64>,
    /// The rolling twenty-four hours, `None` until the alias that carries
    /// it has arrived. Not an `i64` defaulting to zero: nothing opened in
    /// a day is a real reading, and a pane still counting has to look
    /// different from a quiet one.
    opened_24h: Option<i64>,
    merged_24h: Option<i64>,
}

#[derive(Default)]
struct State {
    stats: Vec<Account>,
    accounts: Vec<String>,
    rate: Option<(i64, i64)>,
    calendar: Option<serde_json::Value>,
    err: String,
    fetched: f64,
    days: i64,
    /// Set by [r]: drop the day cache and refetch even past days.
    bust: bool,
}

/// Streaks and totals behind the contribution calendar.
///
/// A streak is consecutive days carrying at least one contribution, counted
/// the way github.com does it: a day that has scored nothing *so far* does
/// not break the current streak, because it is not over yet.
struct CalendarStats {
    today: i64,
    current: i64,
    longest: i64,
    active: usize,
    span: usize,
    busiest: (String, i64),
    weekday: (&'static str, i64),
}

fn calendar_stats(weeks: &serde_json::Value) -> Option<CalendarStats> {
    let mut days: Vec<(String, i64, usize)> = weeks
        .as_array()?
        .iter()
        .flat_map(|wk| wk["contributionDays"].as_array().into_iter().flatten())
        .map(|d| {
            (
                d["date"].as_str().unwrap_or("").to_string(),
                d["contributionCount"].as_i64().unwrap_or(0),
                d["weekday"].as_u64().unwrap_or(0) as usize,
            )
        })
        .collect();
    if days.is_empty() {
        return None;
    }
    days.sort();
    let today_key = today().format("%Y-%m-%d").to_string();

    let (mut longest, mut run) = (0i64, 0i64);
    for (_, count, _) in &days {
        run = if *count > 0 { run + 1 } else { 0 };
        longest = longest.max(run);
    }

    let mut done: Vec<&(String, i64, usize)> =
        days.iter().filter(|(d, _, _)| *d <= today_key).collect();
    if done.last().is_some_and(|(_, c, _)| *c == 0) {
        done.pop(); // today is still in progress
    }
    let mut current = 0i64;
    for (_, count, _) in done.iter().rev() {
        if *count == 0 {
            break;
        }
        current += 1;
    }

    let mut per_weekday: HashMap<usize, i64> = HashMap::new();
    for (_, count, wd) in &days {
        *per_weekday.entry(*wd).or_insert(0) += count;
    }
    let top_wd = per_weekday
        .iter()
        .max_by_key(|(wd, n)| (**n, std::cmp::Reverse(**wd)))
        .map(|(wd, _)| *wd)
        .unwrap_or(0);
    let busiest = days
        .iter()
        .max_by_key(|(_, c, _)| *c)
        .map(|(d, c, _)| (d.clone(), *c))
        .unwrap_or_default();
    Some(CalendarStats {
        today: days
            .iter()
            .find(|(d, _, _)| *d == today_key)
            .map(|(_, c, _)| *c)
            .unwrap_or(0),
        current,
        longest,
        active: days.iter().filter(|(_, c, _)| *c > 0).count(),
        span: days.len(),
        busiest,
        weekday: (
            WEEKDAYS[top_wd.min(6)],
            per_weekday.get(&top_wd).copied().unwrap_or(0),
        ),
    })
}

/// The calendar as seven rows of one cell per week.
fn heatmap(weeks: &serde_json::Value, w: usize) -> (Vec<String>, i64, i64) {
    const LEVELS: &[char] = &[' ', '░', '▒', '▓', '█'];
    let all: Vec<&serde_json::Value> = weeks.as_array().map(|a| a.iter().collect()).unwrap_or_default();
    let counts: Vec<i64> = all
        .iter()
        .flat_map(|wk| wk["contributionDays"].as_array().into_iter().flatten())
        .map(|d| d["contributionCount"].as_i64().unwrap_or(0))
        .collect();
    let peak = counts.iter().copied().max().unwrap_or(0);
    let total: i64 = counts.iter().sum();
    let cols = all.len().min(w.saturating_sub(8)).max(4).min(all.len().max(4));
    let shown = &all[all.len().saturating_sub(cols)..];
    let mut grid = vec![vec![' '; shown.len()]; 7];
    for (x, wk) in shown.iter().enumerate() {
        for d in wk["contributionDays"].as_array().into_iter().flatten() {
            let n = d["contributionCount"].as_i64().unwrap_or(0);
            let wd = (d["weekday"].as_u64().unwrap_or(0) as usize).min(6);
            let level = if n == 0 {
                0
            } else {
                (1 + (n as f64 / peak.max(1) as f64 * 3.99) as usize).min(4)
            };
            grid[wd][x] = LEVELS[level];
        }
    }
    (
        grid.into_iter().map(|row| row.into_iter().collect()).collect(),
        peak,
        total,
    )
}

const CONTRIB_LABEL: &str = " ── CONTRIBUTIONS ── ";

/// The CONTRIBUTIONS heading, which has to say whose contributions these are.
///
/// Every other section on this board is scoped to the configured accounts;
/// `contributionsCollection` is per-viewer, so this one is the reader's own
/// activity across all of GitHub and reads as the board's unless it says so.
/// The qualifier is what gives way when the pane is narrow - it steps from
/// `yours, everywhere` to `yours` to nothing - because a heading that keeps
/// the wording and loses `peak 241/day` is worse than the ambiguity.
fn contributions_heading(total: i64, peak: i64, w: usize, lbl: &str, dim: &str) -> String {
    let budget = (w.saturating_sub(1)).saturating_sub(tc::display_width(CONTRIB_LABEL));
    let numbers = format!("{} in {} weeks, peak {}/day", total, CONTRIB_WEEKS, peak);
    let sep = " · ";
    let tail = ["yours, everywhere", "yours"]
        .iter()
        .find(|q| {
            tc::display_width(q) + tc::display_width(sep) + tc::display_width(&numbers) <= budget
        })
        .map(|q| format!("{}{}{}", q, sep, numbers))
        .unwrap_or_else(|| numbers.clone());
    tc::seg(
        &[(lbl, CONTRIB_LABEL.into()), (dim, tail)],
        w.saturating_sub(1),
    )
}

struct Palette {
    ok: String,
    warn: String,
    bad: String,
    dim: String,
    /// A colour to draw over the selected-row tint.
    ///
    /// `dim` is 3.81 against `bg(38, 56, 76)`, under the 4.5 CLAUDE.md asks for
    /// against the tint as well as the background. This is the same grey lifted
    /// until it clears - 4.94 - and it is used *only* where a tint is on, so an
    /// untinted row is exactly the colour it always was. Not quite the same as
    /// "unselected": herdr-panes tints a blocked or done row whether or not it
    /// is selected, and those get the lighter colours too.
    ///
    /// The substitution happens inside the closure that composes the tint, not
    /// at each call site. Seventeen sites were counted when this was found and
    /// there were twenty-three by the time it was fixed; more than half of them
    /// reach `dim` through a condition that has nothing to do with selection -
    /// `if count > 0 { loud } else { dim }` - and a zero count is the normal
    /// state, so those are the common case rather than the rare one. Anyone
    /// fixing this a call site at a time would fix the obvious half.
    dim_lit: String,
    grid: String,
    txt: String,
    lbl: String,
    accent: String,
    pr: String,
}

fn palette() -> Palette {
    Palette {
        ok: tc::rgb(90, 240, 160),
        warn: tc::rgb(255, 200, 90),
        bad: tc::rgb(255, 100, 110),
        dim: tc::rgb(127, 147, 172),
        dim_lit: tc::rgb(140, 170, 195),
        grid: tc::rgb(60, 78, 98),
        txt: tc::rgb(225, 235, 245),
        lbl: tc::rgb(130, 165, 200),
        accent: tc::rgb(150, 210, 255),
        pr: tc::rgb(180, 160, 255),
    }
}

/// One page of the orgs the viewer belongs to, from `after` onwards.
///
/// A hundred at a time, and the caller follows `endCursor` until GitHub
/// says there is no next page.
fn orgs_query(after: Option<&str>) -> String {
    let at = match after {
        Some(c) => format!(", after: {}", serde_json::Value::String(c.to_string())),
        None => String::new(),
    };
    format!(
        "{{ viewer {{ login organizations(first: 100{}) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ login }} }} }} }}",
        at
    )
}

#[allow(clippy::too_many_arguments)]
fn one_pass(
    tok: &str,
    source: &str,
    viewer: &mut String,
    day_cache: &mut HashMap<String, HashMap<String, (i64, i64)>>,
    state: &Arc<Mutex<State>>,
    scopes: &Arc<Mutex<Scopes>>,
) -> Result<(), String> {
    if viewer.is_empty() {
        let who = graphql("{ viewer { login } }", tok, scopes)?;
        // An errors envelope has no data.viewer, and `unwrap_or("")` turned
        // that into an empty login the pass then carried on with - every
        // query after it silently scoped to nobody. Unreadable rendered as
        // empty is the one failure this collection exists to avoid, so the
        // pass stops and says why, which is what github.py does.
        *viewer = match who["data"]["viewer"]["login"].as_str() {
            Some(login) if !login.is_empty() => login.to_string(),
            _ => {
                let why = who["errors"][0]["message"]
                    .as_str()
                    .unwrap_or("no viewer login in the response");
                // By characters, not bytes: a message with any multibyte
                // character crossing byte fifty would panic the slice, and
                // this one comes from a server.
                return Err(format!(
                    "who am I: {}",
                    why.chars().take(50).collect::<String>()
                ));
            }
        };
    }
    let mut accounts = state.lock().map(|g| g.accounts.clone()).unwrap_or_default();
    if accounts.is_empty() {
        // Every org you belong to, plus your own account - and *every* is
        // what docs/github.md promises for an empty `accounts`. One page of
        // twenty kept that promise only for people who belong to fewer than
        // twenty; past that the extra orgs were not undercounted, they were
        // never asked about, and every headline on the board was a total
        // over an account list that nothing on screen said was short. So
        // the cursor is followed to the end.
        let mut cursor: Option<String> = None;
        loop {
            let d = graphql(&orgs_query(cursor.as_deref()), tok, scopes)?;
            let conn = &d["data"]["viewer"]["organizations"];
            accounts.extend(
                conn["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| o["login"].as_str().unwrap_or("").to_string())
                    .filter(|s| !s.is_empty()),
            );
            let next = conn["pageInfo"]["endCursor"]
                .as_str()
                .unwrap_or("")
                .to_string();
            // This runs on the poller thread: a cursor that stops advancing
            // has to end the loop rather than spin it.
            if !conn["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false)
                || next.is_empty()
                || Some(&next) == cursor.as_ref()
            {
                break;
            }
            cursor = Some(next);
        }
        accounts.push("@me".into());
        if let Ok(mut g) = state.lock() {
            g.accounts = accounts.clone();
        }
    }

    // The calendar is best-effort: it is decoration beside the counts, and a
    // fine-grained token that cannot read it should not blank the board.
    if let Ok(cal) = graphql(&contribution_query(CONTRIB_WEEKS), tok, scopes) {
        let found = cal["data"]["viewer"]["contributionsCollection"]["contributionCalendar"].clone();
        if !found.is_null() {
            if let Ok(mut g) = state.lock() {
                g.calendar = Some(found);
            }
        }
    }

    let (days_now, bust) = match state.lock() {
        Ok(mut g) => {
            let out = (g.days, g.bust);
            g.bust = false;
            out
        }
        Err(_) => return Err("state lock poisoned".into()),
    };
    // Start the pass from what is already on screen, keyed by account, so
    // rows are replaced in place as each lands instead of the table
    // emptying and refilling every pass.
    let mut by_acc: HashMap<String, Account> = state
        .lock()
        .map(|g| g.stats.iter().map(|a| (a.key.clone(), a.clone())).collect())
        .unwrap_or_default();
    let base = today();
    let dates: Vec<String> = (0..days_now)
        .rev()
        .map(|k| (base - Days::days(k)).format("%Y-%m-%d").to_string())
        .collect();
    let keep_from = (base - Days::days(WINDOWS[WINDOWS.len() - 1] - 1))
        .format("%Y-%m-%d")
        .to_string();
    let mut failed: Vec<String> = Vec::new();
    let mut rate: Option<(i64, i64)> = None;

    // Aggregates first, for every account, before any per-day work. One
    // request each, so the headline is live in seconds; the day charts below
    // can cost fifty requests on a cold 90d window and would otherwise hold
    // the whole board grey for minutes.
    // One `now` for the whole pass: each account is queried sequentially,
    // and a fresh `Utc::now()` per request would stagger the 24h cut so
    // the board sum covered several slightly different windows.
    let rolling_now = Utc::now();
    // Last pass's figures are a different cutoff. Forget them before any
    // account lands, or the first publish mixes this window with the last.
    for row in by_acc.values_mut() {
        forget_24h(row);
    }
    publish(state, &accounts, &by_acc, rate);
    for acc in &accounts {
        let data = match graphql(&build_query(acc, days_now, viewer, rolling_now), tok, scopes) {
            Ok(d) => d,
            Err(e) => {
                // Fifty characters, which is what the branch below used to
                // take before `graphql` started refusing an errors payload
                // itself and made that branch unreachable.
                failed.push(format!("{} ({})", acc, e.chars().take(50).collect::<String>()));
                if let Some(row) = by_acc.get_mut(acc) {
                    forget_24h(row);
                    publish(state, &accounts, &by_acc, rate);
                }
                continue;
            }
        };
        let d = &data["data"];
        if let Some(limit) = d["rateLimit"]["limit"].as_i64() {
            rate = Some((d["rateLimit"]["remaining"].as_i64().unwrap_or(0), limit));
        }
        let (merged, dropped) = (count_at(d, "o0_merged"), count_at(d, "o0_dropped"));
        let prev = by_acc.get(acc).cloned().unwrap_or_default();
        by_acc.insert(
            acc.clone(),
            Account {
                key: acc.clone(),
                account: if acc == "@me" { viewer.clone() } else { acc.clone() },
                is_me: acc == "@me",
                window: days_now,
                open: count_at(d, "o0_open"),
                draft: count_at(d, "o0_draft"),
                review: count_at(d, "o0_review"),
                issues: count_at(d, "o0_issues"),
                merged,
                dropped,
                rate: if merged + dropped > 0 {
                    Some(100.0 * merged as f64 / (merged + dropped) as f64)
                } else {
                    None
                },
                hist: prev.hist,
                opened_hist: prev.opened_hist,
                hist_window: prev.hist_window,
                // Read straight off the payload rather than through
                // `count_at`, which lands a missing alias on zero - and a
                // zero here is a claim the skeleton exists to avoid
                // making.
                opened_24h: figure_at(d, "o0_o24"),
                merged_24h: figure_at(d, "o0_m24"),
            },
        );
        publish(state, &accounts, &by_acc, rate);
    }

    // Then the per-day counts. A past day cannot change - a PR merged on the
    // 3rd stays merged on the 3rd - so only days never seen before, plus the
    // trailing few, cost a request. Widening the window therefore buys only
    // the days it adds; narrowing is free.
    for acc in &accounts {
        if !by_acc.contains_key(acc) {
            continue;
        }
        let cache = day_cache.entry(acc.clone()).or_default();
        if bust {
            cache.clear();
        }
        let fresh: Vec<&String> = dates.iter().rev().take(FRESH_DAYS).collect();
        let want: Vec<String> = dates
            .iter()
            .filter(|x| !cache.contains_key(*x) || fresh.contains(x))
            .cloned()
            .collect();
        for chunk in want.chunks(DAY_CHUNK) {
            let dd = match graphql(&build_day_query(&scope_of(acc, viewer), chunk), tok, scopes) {
                Ok(d) => d["data"].clone(),
                Err(_) => continue,
            };
            for (n, day) in chunk.iter().enumerate() {
                cache.insert(
                    day.clone(),
                    (
                        count_at(&dd, &format!("m{}", n)),
                        count_at(&dd, &format!("c{}", n)),
                    ),
                );
            }
        }
        cache.retain(|day, _| *day >= keep_from); // older than any window
        if !dates.iter().all(|x| cache.contains_key(x)) {
            continue; // a chunk failed; leave it
        }
        if let Some(row) = by_acc.get_mut(acc) {
            row.hist = dates.iter().map(|x| (x.clone(), cache[x].0)).collect();
            row.opened_hist = dates.iter().map(|x| (x.clone(), cache[x].1)).collect();
            row.hist_window = Some(days_now);
        }
        publish(state, &accounts, &by_acc, rate);
    }

    if let Ok(mut g) = state.lock() {
        // With nothing else to report, surface a token sitting in a file
        // other users on the box can read.
        g.err = if !failed.is_empty() {
            format!("could not read: {}", failed.join(", "))
        } else {
            let warn = scopes.lock().map(|s| scope_warning(&s)).unwrap_or_default();
            if !warn.is_empty() {
                warn
            } else if source == "config" {
                tc::config_token_warning().unwrap_or_default()
            } else {
                String::new()
            }
        };
    }
    Ok(())
}

fn publish(
    state: &Arc<Mutex<State>>,
    accounts: &[String],
    by_acc: &HashMap<String, Account>,
    rate: Option<(i64, i64)>,
) {
    if let Ok(mut g) = state.lock() {
        g.stats = accounts
            .iter()
            .filter_map(|a| by_acc.get(a).cloned())
            .collect();
        if rate.is_some() {
            g.rate = rate;
        }
        g.fetched = tc::now();
    }
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    if !tc::dependencies_available("github", include_str!("dependencies.json"), Some(SETTINGS)) {
        return;
    }
    let cfg = tc::load_config("github");
    let mut refresh = tc::cfg_f64(&cfg, "refresh", 120.0);
    let configured: Vec<String> = tc::cfg_strings(&cfg, "accounts", &[]);
    let start_window = (tc::cfg_f64(&cfg, "window_days", 14.0) as i64).max(1);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut named: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--refresh" if i + 1 < args.len() => {
                refresh = args[i + 1].parse::<f64>().unwrap_or(120.0).max(30.0);
                i += 2;
            }
            other if !other.starts_with('-') => {
                named.push(other.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }
    refresh = tc::poll_secs(refresh, 120.0).max(30.0);

    let p = palette();
    let state = Arc::new(Mutex::new(State {
        accounts: if named.is_empty() { configured } else { named },
        days: start_window,
        ..Default::default()
    }));
    let scopes = Arc::new(Mutex::new(Scopes::default()));
    let wake = Arc::new((Mutex::new(false), Condvar::new()));
    let (tok, source) = token(&cfg);
    // The poller thread takes ownership of it; the render loop needs it too,
    // for the one on-demand request an account's own screen makes.
    let ui_tok = tok.clone();
    let ui_scopes = Arc::clone(&scopes);
    let env_name = {
        let name = tc::cfg_str(&cfg, "token_env", TOKEN_ENV);
        if name.is_empty() { TOKEN_ENV.to_string() } else { name }
    };

    let poller = Arc::clone(&state);
    let poller_wake = Arc::clone(&wake);
    let poller_scopes = Arc::clone(&scopes);
    std::thread::spawn(move || {
        let mut viewer = String::new();
        let mut day_cache: HashMap<String, HashMap<String, (i64, i64)>> = HashMap::new();
        loop {
            if tok.is_empty() {
                if let Ok(mut g) = poller.lock() {
                    g.err = tc::missing_config(&format!(
                        "no token: set github.token or ${} (needs repo + read:org)",
                        env_name
                    ));
                }
            } else if let Err(said) = one_pass(
                &tok,
                source,
                &mut viewer,
                &mut day_cache,
                &poller,
                &poller_scopes,
            ) {
                if let Ok(mut g) = poller.lock() {
                    g.err = said;
                }
            }
            let (lock, cond) = &*poller_wake;
            let mut asked = match lock.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            if !*asked {
                asked = match cond.wait_timeout(asked, Duration::from_secs_f64(refresh)) {
                    Ok((g, _)) => g,
                    Err(_) => return,
                };
            }
            *asked = false;
        }
    });

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let (mut selected, mut tick) = (0usize, 0usize);
    // The frame's own scroll, and whether the selection moved this tick.
    //
    // The two have to be separate. The window used to be a function of the
    // selection - centred on it, `selected - room/2` - so the mouse could
    // not move the view without moving what `↵` would open. Now the keys
    // own the selection and the wheel owns the view, and the only time the
    // view is dragged back to the cursor is the tick a key moved it. Chase
    // it every tick instead and a scroll snaps back before it is seen.
    let mut board = 0usize;
    let mut moved = false;
    // One account on its own screen, and how far down it is scrolled.
    let (mut detail, mut dscroll) = (false, 0usize);
    // Which of the oldest PRs the cursor is on, and what [c] last said.
    let mut osel = 0usize;
    let (mut note, mut note_at) = (String::new(), 0.0f64);
    // The longest-open PRs per account, fetched when that account's screen
    // is opened and kept after.
    let oldest: Arc<Mutex<HashMap<String, serde_json::Value>>> = Arc::new(Mutex::new(HashMap::new()));
    let asking: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut settle_t = 0usize;
    let mut settle_from: Option<(Vec<f64>, Vec<f64>)> = None;

    loop {
        tick += 1;
        for key in keyboard.poll() {
            match key.as_str() {
                "," => {
                    tc::run_settings(&mut keyboard, SETTINGS);
                    continue;
                }
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "r" | "R" => {
                    // A manual refresh re-reads even past days.
                    if let Ok(mut g) = state.lock() {
                        g.bust = true;
                    }
                    let (lock, cond) = &*wake;
                    if let Ok(mut asked) = lock.lock() {
                        *asked = true;
                        cond.notify_all();
                    }
                }
                "w" | "W" => {
                    if let Ok(mut g) = state.lock() {
                        g.days = tc::cycle(WINDOWS, g.days);
                    }
                    let (lock, cond) = &*wake;
                    if let Ok(mut asked) = lock.lock() {
                        *asked = true;
                        cond.notify_all();
                    }
                }
                "right" | "enter" => {
                    detail = true;
                    dscroll = 0;
                }
                "left" | "esc" if detail => detail = false,
                "up" if detail => {
                    osel = osel.saturating_sub(1);
                    moved = true;
                }
                "down" if detail => {
                    osel = osel.saturating_add(1);
                    moved = true;
                }
                "c" | "C" if detail => {
                    // The account under the cursor, read from the shared
                    // state rather than the render's copy - the keys are
                    // handled before the frame is built.
                    let key = state
                        .lock()
                        .ok()
                        .and_then(|g| {
                            g.stats
                                .get(selected.min(g.stats.len().saturating_sub(1)))
                                .map(|a| a.key.clone())
                        })
                        .unwrap_or_default();
                    let url = oldest
                        .lock()
                        .ok()
                        .and_then(|g| g.get(&key).cloned())
                        .and_then(|v| v.as_array().cloned())
                        .and_then(|n| n.get(osel).cloned())
                        .map(|n| n["url"].as_str().unwrap_or("").to_string())
                        .unwrap_or_default();
                    if !url.is_empty() {
                        note = if tc::clipboard(&url) {
                            format!("✓ copied {}", url)
                        } else {
                            format!("no clipboard: {}", url)
                        };
                        note_at = tc::now();
                    }
                }
                "pgup" if detail => {
                    let page = tc::size().1.saturating_sub(3).max(1);
                    dscroll = dscroll.saturating_sub(page);
                }
                "pgdn" if detail => {
                    let page = tc::size().1.saturating_sub(3).max(1);
                    dscroll = dscroll.saturating_add(page);
                }
                "home" if detail => dscroll = 0,
                "end" if detail => dscroll = usize::MAX,
                // Keys move the selection; the wheel moves the view. Never
                // the other way round: scrolling to look at something must
                // not change what `↵` opens.
                "up" => {
                    selected = selected.saturating_sub(1);
                    moved = true;
                }
                "down" => {
                    selected += 1;
                    moved = true;
                }
                // Whichever screen is on: the board and an account's own
                // screen keep separate offsets, so a wheel bound to one
                // does nothing on the other.
                "ctrl-y" | "wheel-up" => {
                    let at = if detail { &mut dscroll } else { &mut board };
                    *at = at.saturating_sub(1);
                }
                "ctrl-e" | "wheel-down" => {
                    let at = if detail { &mut dscroll } else { &mut board };
                    *at = at.saturating_add(1);
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let (mut stats, rate, err, fetched, calendar, want, watched) = match state.lock() {
            Ok(g) => (
                g.stats.clone(),
                g.rate,
                g.err.clone(),
                g.fetched,
                g.calendar.clone(),
                g.days,
                g.accounts.len(),
            ),
            Err(_) => return,
        };
        // Busiest first: open PRs decide it, and merged-in-window breaks ties
        // so an idle backlog ranks below an account of the same size that is
        // actually moving. Name last, to keep the order steady frame to frame.
        stats.sort_by(|a, b| {
            b.open
                .cmp(&a.open)
                .then(b.merged.cmp(&a.merged))
                .then(a.account.to_lowercase().cmp(&b.account.to_lowercase()))
        });
        // Windowed figures are stale until every account has reported for the
        // window now selected. The charts are tracked apart from the headline
        // because their data costs far more requests and lands well after it.
        let stale = stats.is_empty() || stats.iter().any(|x| x.window != want);
        let chart_stale = stats.is_empty() || stats.iter().any(|x| x.hist_window != Some(want));
        if !stats.is_empty() && selected >= stats.len() {
            selected = stats.len() - 1;
        }

        let mut rows = vec![tc::title("github ops", w, &p.accent)];
        let mut head = vec![(
            p.dim.as_str(),
            format!(
                " {} account{}",
                watched,
                if watched == 1 { "" } else { "s" }
            ),
        )];
        let tail = tc::polled(fetched, rate, &p.dim, &p.ok, &p.warn);
        for (colour, txt) in &tail {
            head.push((colour.as_str(), txt.clone()));
        }
        rows.push(tc::seg(&head, w - 1));
        if !err.is_empty() {
            rows.extend(tc::error_rows(p.bad.as_str(), &err, w));
        }
        if stats.is_empty() {
            rows.push(tc::seg(&[(p.dim.as_str(), " collecting…".into())], w - 1));
            let hints = vec![vec![(p.dim.as_str(), "[,] settings".into())], vec![(
                p.dim.as_str(),
                "[q]uit".into(),
            )]];
            let foot: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
                .into_iter()
                .map(|line| format!(" {}", line))
                .collect();
            rows.truncate(h.saturating_sub(foot.len()));
            while rows.len() < h.saturating_sub(foot.len()) {
                rows.push(String::new());
            }
            rows.extend(foot);
            tc::draw(&rows, w, h);
            std::thread::sleep(Duration::from_millis(400));
            continue;
        }

        let sum = |f: fn(&Account) -> i64| -> i64 { stats.iter().map(f).sum() };
        let (open, draft, review, issues, merged, dropped) = (
            sum(|a| a.open),
            sum(|a| a.draft),
            sum(|a| a.review),
            sum(|a| a.issues),
            sum(|a| a.merged),
            sum(|a| a.dropped),
        );
        let rate_pct = if merged + dropped > 0 {
            Some(100.0 * merged as f64 / (merged + dropped) as f64)
        } else {
            None
        };
        // What is outstanding right now leads the board: it is the question
        // asked most often, and the only section that is not windowed.
        if open > 0 {
            let ready = (open - draft - review).max(0);
            let legend: Vec<(&str, i64, &str)> = [
                ("awaiting review", review, p.warn.as_str()),
                ("ready to merge", ready, p.ok.as_str()),
                ("draft", draft, p.dim.as_str()),
            ]
            .into_iter()
            .filter(|x| x.1 > 0)
            .collect();
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── OPEN PR STATE ── ".into()),
                    (p.pr.as_str(), format!("{}", open)),
                    (p.dim.as_str(), " PRs · ".into()),
                    (p.warn.as_str(), format!("{}", issues)),
                    (p.dim.as_str(), " issues open   (any age)".into()),
                ],
                w - 1,
            ));
            let parts: Vec<(f64, String)> = legend
                .iter()
                .map(|(_, n, c)| (*n as f64 / open as f64, c.to_string()))
                .collect();
            let bar = tc::stacked_bar(&parts, w.saturating_sub(3).max(10));
            let mut line: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for (colour, txt) in &bar {
                line.push((colour.as_str(), txt.clone()));
            }
            rows.push(tc::seg(&line, w - 1));
            let mut key: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for (label, count, colour) in &legend {
                key.push((colour, "▇ ".into()));
                key.push((p.txt.as_str(), (*label).into()));
                key.push((
                    p.dim.as_str(),
                    format!(" {} ({:.0}%)   ", count, 100.0 * *count as f64 / open as f64),
                ));
            }
            rows.push(tc::seg(&key, w - 1));
        }

        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── MERGE RATE ── ".into()),
                (p.dim.as_str(), format!("last {} days", want)),
            ],
            w - 1,
        ));
        let bar_w = w.saturating_sub(34).max(10);
        if stale {
            let shimmer = tc::skeleton(bar_w, tick, 7);
            let mut line: Vec<(&str, String)> = vec![(p.dim.as_str(), format!(" {:<5}", "···"))];
            for (colour, txt) in &shimmer {
                line.push((colour.as_str(), txt.clone()));
            }
            line.push((p.dim.as_str(), format!("  loading {}d…", want)));
            rows.push(tc::seg(&line, w - 1));
        } else {
            // `health`, not `heat`: a merge rate is high-is-good, and the
            // ramp handed the raw fraction drew 97% merged in alarm red.
            let hot = match rate_pct {
                Some(v) => tc::health(v / 100.0),
                None => p.dim.clone(),
            };
            rows.push(tc::seg(
                &[
                    (
                        hot.as_str(),
                        format!(
                            " {:<5}",
                            match rate_pct {
                                Some(v) => format!("{:.0}%", v),
                                None => "--".into(),
                            }
                        ),
                    ),
                    (hot.as_str(), tc::meter(rate_pct.unwrap_or(0.0) / 100.0, bar_w)),
                    (p.ok.as_str(), format!("  {} merged", merged)),
                    (p.dim.as_str(), " / ".into()),
                    (p.bad.as_str(), format!("{} dropped", dropped)),
                ],
                w - 1,
            ));
        }

        let mut merged_all: HashMap<String, i64> = HashMap::new();
        let mut opened_all: HashMap<String, i64> = HashMap::new();
        for st in &stats {
            if st.hist_window != Some(want) {
                continue; // covers a different window; adding it lies
            }
            for (day, n) in &st.hist {
                *merged_all.entry(day.clone()).or_insert(0) += n;
            }
            for (day, n) in &st.opened_hist {
                *opened_all.entry(day.clone()).or_insert(0) += n;
            }
        }
        let base = today();
        let mut days: Vec<String> = (0..want)
            .rev()
            .map(|n| (base - Days::days(n)).format("%Y-%m-%d").to_string())
            .collect();
        // The chart fills what is left of the pane. Where there is room to
        // spare a day takes several columns; where there is not, the oldest
        // days are cropped rather than the whole chart squeezed into a
        // corner. The figure column comes out *first*, before the days are
        // spread, or it would land on bars already drawn - and it stands
        // down rather than costing the chart days it cannot spare.
        let (avail, figw) = chart_split(w, want as usize);
        if days.len() > avail {
            days = days[days.len() - avail..].to_vec();
        }
        let slot = (avail / days.len()).max(1);
        let gap = if slot >= 3 { 1 } else { 0 };
        let barw = slot - gap;
        let spread = |per_day: &[f64]| -> Vec<f64> {
            let mut cols = Vec::new();
            for (n, v) in per_day.iter().enumerate() {
                cols.extend(std::iter::repeat_n(*v, barw));
                if gap > 0 && n + 1 < per_day.len() {
                    cols.extend(std::iter::repeat_n(0.0, gap));
                }
            }
            cols
        };
        let opened_day: Vec<f64> = days
            .iter()
            .map(|d| opened_all.get(d).copied().unwrap_or(0) as f64)
            .collect();
        let merged_day: Vec<f64> = days
            .iter()
            .map(|d| merged_all.get(d).copied().unwrap_or(0) as f64)
            .collect();
        let (up, down) = (spread(&opened_day), spread(&merged_day));
        let chart_cols = up.len();
        let figs = (figw > 0).then(|| {
            figure_rows(
                board_24h(&stats, watched, |s| s.opened_24h),
                board_24h(&stats, watched, |s| s.merged_24h),
                figw,
                tick,
                &p,
            )
        });
        // Where the figure column starts: one for the chart's left margin,
        // then the bars. Every chart row is padded to it, because the axis
        // labels below are shorter than the bars above whenever `Nd ago`
        // and `today` do not reach across, and the column would otherwise
        // step sideways on that one row.
        let pad_to = 1 + chart_cols;
        let fig = |n: usize| figs.as_ref().and_then(|f| f.get(n));
        // One scale both ways, or the comparison lies.
        let span_hi = up
            .iter()
            .chain(down.iter())
            .cloned()
            .fold(0.0f64, f64::max)
            .max(1.0);
        rows.push(String::new());
        if chart_stale {
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── PR FLOW ── ".into()),
                    (p.dim.as_str(), format!("counting {}d…", want)),
                ],
                w - 1,
            ));
        } else {
            // Totals come from the days themselves: a day spans several
            // columns now, so summing the columns would multiply by bar width.
            let span = if days.len() < want as usize {
                format!("{}d of {}d", days.len(), want)
            } else {
                format!("{}d", days.len())
            };
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── PR FLOW ── ".into()),
                    (p.dim.as_str(), format!("{} · ", span)),
                    (
                        p.pr.as_str(),
                        format!("▲ {} opened", opened_day.iter().sum::<f64>() as i64),
                    ),
                    (p.dim.as_str(), " · ".into()),
                    (
                        p.ok.as_str(),
                        format!("▼ {} merged", merged_day.iter().sum::<f64>() as i64),
                    ),
                    (p.dim.as_str(), format!("   peak {}/day", span_hi as i64)),
                ],
                w - 1,
            ));
        }
        // While the figures are still arriving the bars bounce like a level
        // meter in pale versions of their own colours, then settle onto the
        // real values rather than cutting to them. Three rows each side,
        // always - trimming the unused half would make the chart change
        // height at the end of the animation, which is exactly when it
        // should be still.
        let (hu, hd, cu, cd) = if chart_stale {
            // Dance per day, then widen: bouncing each column on its own
            // would show a twelve-column day as twelve separate thin bars.
            let hu = spread(&tc::dance(days.len(), tick, 0.0));
            let hd = spread(&tc::dance(days.len(), tick, 2.1));
            settle_from = Some((hu.clone(), hd.clone()));
            settle_t = 0;
            (
                hu,
                hd,
                tc::mix(GHOST, PR_RGB, 0.45),
                tc::mix(GHOST, OK_RGB, 0.45),
            )
        } else {
            let real_u: Vec<f64> = up.iter().map(|v| v / span_hi).collect();
            let real_d: Vec<f64> = down.iter().map(|v| v / span_hi).collect();
            match &settle_from {
                Some((fu, fd)) if settle_t < SETTLE_FRAMES && fu.len() == chart_cols => {
                    settle_t += 1;
                    let q = settle_t as f64 / SETTLE_FRAMES as f64;
                    let q = q * q * (3.0 - 2.0 * q); // ease in and out
                    (
                        fu.iter().zip(&real_u).map(|(a, b)| a + (b - a) * q).collect(),
                        fd.iter().zip(&real_d).map(|(a, b)| a + (b - a) * q).collect(),
                        tc::mix(GHOST, PR_RGB, 0.45 + 0.55 * q),
                        tc::mix(GHOST, OK_RGB, 0.45 + 0.55 * q),
                    )
                }
                _ => (real_u, real_d, p.pr.clone(), p.ok.clone()),
            }
        };
        for (n, line) in
            tc::vbars(&hu.iter().map(|v| (*v, cu.clone())).collect::<Vec<_>>(), 3, 1.0)
                .into_iter()
                .enumerate()
        {
            let mut parts: Vec<(String, String)> = vec![(tc::RST.to_string(), " ".into())];
            for (colour, ch) in &line {
                parts.push((colour.clone(), ch.clone()));
            }
            rows.push(with_figure(parts, fig(n), pad_to, w - 1));
        }
        // An explicit baseline: without it the two series abut and the eye
        // cannot tell which row the bars grow from.
        rows.push(with_figure(
            vec![
                (tc::RST.to_string(), " ".into()),
                (p.grid.clone(), "─".repeat(chart_cols)),
            ],
            fig(3),
            pad_to,
            w - 1,
        ));
        for (n, line) in
            tc::vbars_down(&hd.iter().map(|v| (*v, cd.clone())).collect::<Vec<_>>(), 3, 1.0)
                .into_iter()
                .enumerate()
        {
            let mut parts: Vec<(String, String)> = vec![(tc::RST.to_string(), " ".into())];
            for (colour, ch) in &line {
                parts.push((colour.clone(), ch.clone()));
            }
            rows.push(with_figure(parts, fig(4 + n), pad_to, w - 1));
        }
        let left = format!("{}d ago", days.len());
        rows.push(with_figure(
            vec![
                (p.dim.clone(), format!(" {}", left)),
                (
                    p.dim.clone(),
                    " ".repeat(chart_cols.saturating_sub(left.len() + 5).max(1)),
                ),
                (p.dim.clone(), "today".into()),
            ],
            fig(7),
            pad_to,
            w - 1,
        ));
        rows.push(String::new());

        // Drawn whatever the pane is. It used to stand down below 39 rows so
        // the account table could have the height, but a contribution grid
        // that is not there looks exactly like an account with no
        // contributions, and the board scrolls now - so the pane costs it
        // nothing that a turn of the wheel does not get back.
        if let Some(cal) = calendar.as_ref() {
            let (grid, peak, total) = heatmap(&cal["weeks"], w);
            let total_c = cal["totalContributions"].as_i64().unwrap_or(total);
            rows.push(contributions_heading(
                total_c,
                peak,
                w,
                p.lbl.as_str(),
                p.dim.as_str(),
            ));
            for (r, line) in grid.iter().enumerate() {
                // Rows are GitHub's own weekday index, where 0 is Sunday, so
                // the labels come off the same constant rather than a
                // hand-written tuple. Written Monday-first they sat one row
                // early and put today under yesterday's name.
                let label = if r == 1 || r == 3 || r == 5 { WEEKDAYS[r] } else { "" };
                rows.push(tc::seg(
                    &[
                        (p.dim.as_str(), format!(" {:<4}", label)),
                        (p.ok.as_str(), line.clone()),
                    ],
                    w - 1,
                ));
            }
            if let Some(cs) = calendar_stats(&cal["weeks"]) {
                let cells: Vec<(String, String, &str)> = vec![
                    (
                        "current streak".into(),
                        format!("{} days", cs.current),
                        if cs.current > 0 { p.ok.as_str() } else { p.dim.as_str() },
                    ),
                    ("longest streak".into(), format!("{} days", cs.longest), p.txt.as_str()),
                    (
                        "today".into(),
                        format!("{}", cs.today),
                        if cs.today > 0 { p.ok.as_str() } else { p.dim.as_str() },
                    ),
                    (
                        "active days".into(),
                        format!(
                            "{} of {} ({:.0}%)",
                            cs.active,
                            cs.span,
                            100.0 * cs.active as f64 / cs.span as f64
                        ),
                        p.txt.as_str(),
                    ),
                    (
                        "busiest".into(),
                        format!("{} ({})", cs.busiest.0, cs.busiest.1),
                        p.txt.as_str(),
                    ),
                    (
                        "most on".into(),
                        format!("{} ({})", cs.weekday.0, cs.weekday.1),
                        p.txt.as_str(),
                    ),
                ];
                // As many columns as the width honestly allows, never fewer
                // than one - the labels are what make these readable.
                let ncols = if w >= 86 {
                    3
                } else if w >= 58 {
                    2
                } else {
                    1
                };
                let cw = (w - 2) / ncols;
                for chunk in cells.chunks(ncols) {
                    let mut line: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
                    for (label, value, colour) in chunk {
                        let used = label.len() + 1 + value.len();
                        line.push((p.dim.as_str(), format!("{} ", label)));
                        line.push((colour, value.clone()));
                        line.push((tc::RST, " ".repeat(cw.saturating_sub(used).max(2))));
                    }
                    rows.push(tc::seg(&line, w - 1));
                }
            }
            rows.push(String::new());
        }

        // Every account is drawn, and the frame is a window onto the lot -
        // the shape `linear` uses. Windowing the list inside a frame that
        // was itself being truncated meant two scrolls fighting over one
        // pane, and neither could be driven by the wheel without moving
        // the selection. `cursor` below records where the selected row
        // landed so the window can be dragged back to it when a key moves
        // it, and left alone when it does not.
        let first = 0usize;
        let room = stats.len();
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── BY ACCOUNT ──".into()),
                (
                    p.dim.as_str(),
                    if stats.len() > room {
                        format!(
                            "   {}-{} of {}",
                            first + 1,
                            (first + room).min(stats.len()),
                            stats.len()
                        )
                    } else {
                        String::new()
                    },
                ),
            ],
            w - 1,
        ));
        let wide = w >= 62;
        let bar_cols = w.saturating_sub(64).max(4);
        // No separators between these fields: the row emits its widths
        // back-to-back, so a space here drifts the header one column per
        // field. MRG takes seven, since "MRG60D" is six characters and would
        // sit flush against REVW in every window but the seven-day one.
        let mut head = format!(
            " {:<20}{:>5}{:>5}{:>7}{:>6}",
            "ACCOUNT",
            "OPEN",
            "REVW",
            format!("MRG{}D", want),
            "RATE"
        );
        let spark_days: Vec<String> = (0..(want as usize).min(bar_cols) as i64)
            .rev()
            .map(|n| (base - Days::days(n)).format("%Y-%m-%d").to_string())
            .collect();
        if wide {
            head += &format!("{:>7}", "ISSUES");
            // Each row is scaled to its own busiest day, so say what the
            // reader may do with it - read the shape - rather than naming
            // the mechanism. Pick the longest label that fits rather than
            // clipping one: a truncated hint is worse than a shorter one.
            let label = [
                "MERGED/DAY · SHAPE ONLY, NOT TO SCALE",
                "MERGED/DAY · SHAPE ONLY",
                "MERGED/DAY (shape)",
                "MERGED/DAY",
                "",
            ]
            .into_iter()
            .find(|l| l.len() <= bar_cols)
            .unwrap_or("");
            if !label.is_empty() {
                head += &format!("  {}", label);
            }
        }
        rows.push(tc::seg(&[(p.dim.as_str(), tc::pad(&head, w - 1))], w - 1));
        let mut cursor: Option<usize> = None;
        for (i, s) in stats.iter().enumerate().skip(first).take(room) {
            let here = i == selected;
            if here {
                cursor = Some(rows.len());
            }
            let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
            let c = |colour: &str| {
                // Same shape as the other widgets that do this, so one rule
                // reads them all: a guard per colour, each reaching its own
                // lighter twin.
                let colour = if tint.is_empty() {
                    colour
                } else if colour == p.dim {
                    p.dim_lit.as_str()
                } else {
                    colour
                };
                format!("{}{}", tint, colour)
            };
            // This row's own staleness: accounts land one at a time, so an
            // account already refetched for the new window shows real numbers
            // while the ones behind it still shimmer.
            let old = s.window != want;
            // The same high-is-good ramp as the section above it, so one
            // rate reads as one colour wherever it is drawn - and the lifted
            // form of it, because this one goes through the tint closure and
            // the plain ramp's hot end measures 3.18 there. `health` sends a
            // *low* rate to that end, so the unreadable colour was the
            // struggling account rather than the healthy one.
            let hot = match s.rate {
                Some(r) if !old => tc::health_on(r / 100.0, here),
                _ => p.dim.clone(),
            };
            let mut line = vec![
                (
                    c(if here { &p.accent } else { &p.txt }),
                    format!(
                        "{}{}",
                        if here { "▸" } else { " " },
                        tc::pad(
                            &format!("{}{}", s.account, if s.is_me { " (you)" } else { "" }),
                            20
                        )
                    ),
                ),
                (c(&p.pr), format!("{:>5}", s.open)),
                (
                    c(if s.review > 0 { &p.warn } else { &p.dim }),
                    format!("{:>5}", s.review),
                ),
                (
                    c(if old { &p.dim } else { &p.ok }),
                    format!("{:>7}", if old { "···".to_string() } else { s.merged.to_string() }),
                ),
                (
                    c(&hot),
                    format!(
                        "{:>6}",
                        if old {
                            "···".to_string()
                        } else {
                            match s.rate {
                                Some(r) => format!("{:.0}%", r),
                                None => "--".into(),
                            }
                        }
                    ),
                ),
            ];
            if wide {
                line.push((c(&p.dim), format!("{:>7}", s.issues)));
                // Each account's own merged-per-day. The columns carry
                // totals but no shape, and a fortnight of nothing ending in
                // a spike reads very differently from a steady trickle.
                if s.hist_window != Some(want) {
                    line.push((c(&p.grid), format!("  {}", "·".repeat(spark_days.len()))));
                } else {
                    let top = s.hist.values().copied().max().unwrap_or(0);
                    let mut marks = String::new();
                    for d in &spark_days {
                        let v = s.hist.get(d).copied().unwrap_or(0);
                        marks.push(if v > 0 && top > 0 {
                            tc::SPARK[(((v as f64 / top as f64) * 7.99) as usize).min(7)]
                        } else {
                            ' '
                        });
                    }
                    line.push((c(&p.ok), format!("  {}", marks)));
                }
            }
            if here {
                line.push((tint.clone(), " ".repeat(w)));
            }
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
        }

        // One account in full, opened from the row it belongs to.
        if detail {
            if let Some(a) = stats.get(selected.min(stats.len().saturating_sub(1))) {
                // One request, on opening, for the question the aggregates
                // cannot answer. Held per account so leaving and coming back
                // does not ask again.
                let key = a.key.clone();
                let held = oldest.lock().ok().and_then(|g| g.get(&key).cloned());
                if held.is_none() {
                    let start = asking
                        .lock()
                        .map(|mut g| g.insert(key.clone()))
                        .unwrap_or(false);
                    if start {
                        let (oldest, asking) = (Arc::clone(&oldest), Arc::clone(&asking));
                        // key is "@me" or the org; account is the bare
                        // login, which is what scope_of wants for "@me".
                        let (acc, viewer, tok, scopes) =
                            (a.key.clone(), a.account.clone(), ui_tok.clone(), Arc::clone(&ui_scopes));
                        std::thread::spawn(move || {
                            let got = fetch_oldest(&acc, &viewer, &tok, &scopes);
                            if let Ok(mut g) = oldest.lock() {
                                g.insert(acc.clone(), got);
                            }
                            if let Ok(mut g) = asking.lock() {
                                g.remove(&acc);
                            }
                        });
                    }
                }
                let nodes = held
                    .as_ref()
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();
                osel = osel.min(nodes.len().saturating_sub(1));
                let (body, cursor) = account_detail(a, held.as_ref(), osel, w, tick, &p);
                let hints: Vec<Vec<(&str, String)>> = vec![
                    vec![
                        (p.accent.as_str(), "↑↓".into()),
                        (p.dim.as_str(), if nodes.is_empty() { " scroll" } else { " oldest" }.into()),
                    ],
                    vec![(p.dim.as_str(), "[c]opy url".into())],
                    vec![(p.dim.as_str(), "pgup/pgdn page".into())],
                    vec![
                        (p.accent.as_str(), "←".into()),
                        (p.dim.as_str(), "/esc back".into()),
                    ],
                    vec![(p.dim.as_str(), "[,] settings".into())],
                    vec![(p.dim.as_str(), "[q]uit".into())],
                ];
                let foot: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
                    .into_iter()
                    .map(|l| format!(" {}", l))
                    .collect();
                let room = h.saturating_sub(foot.len()).max(1);
                // The page follows the cursor into the oldest list, the way
                // netwatch's detail follows one into a section. Without it
                // the row being selected is often off the bottom.
                // The title stays put while the rest scrolls under it. A
                // detail screen is where it matters most: the board at
                // least has its own name on every row, but scroll a detail
                // view and there is nothing left saying whose account you
                // opened. `cursor` indexes the whole body, so it shifts by
                // the header before the window chases it - miss that and
                // the selection lands a row off, only once you scroll.
                let (head, rest) = body.split_at(1.min(body.len()));
                let room_below = room.saturating_sub(head.len()).max(1);
                // Only on the tick a key moved the selection. Chasing it
                // every tick drags the view back to the cursor the instant
                // the wheel moves it, which reads as the wheel not working.
                if moved {
                    if let Some(at) = cursor {
                        let at = at.saturating_sub(head.len());
                        if at < dscroll {
                            dscroll = at;
                        } else if at >= dscroll + room_below {
                            dscroll = at + 1 - room_below;
                        }
                    }
                    moved = false;
                }
                dscroll = dscroll.min(rest.len().saturating_sub(room_below));
                let last = (dscroll + room_below).min(rest.len());
                let mut out: Vec<String> = head.to_vec();
                out.extend_from_slice(&rest[dscroll..last]);
                while out.len() < room {
                    out.push(String::new());
                }
                if !note.is_empty() && tc::now() - note_at < 6.0 {
                    if let Some(row) = out.last_mut() {
                        *row = tc::seg(&[(p.ok.as_str(), format!(" {}", note))], w - 1);
                    }
                }
                out.extend(foot);
                tc::draw(&out, w, h);
                std::thread::sleep(Duration::from_millis(300));
                continue;
            }
            detail = false;
        }

        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " account".into())],
            vec![
                (p.accent.as_str(), "→/↵".into()),
                (p.dim.as_str(), " account".into()),
            ],
            vec![(p.dim.as_str(), "[w]indow".into())],
            vec![(p.dim.as_str(), "[r]efresh".into())],
            vec![(p.dim.as_str(), "[,] settings".into())],
            vec![(p.dim.as_str(), "[q]uit".into())],
        ];
        let footer: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
        // A window onto the frame, title pinned, rather than a cut of it.
        // Truncating dropped every account past the fold with nothing
        // saying so.
        let room = h.saturating_sub(footer.len());
        let (head, rest) = rows.split_at(1.min(rows.len()));
        let room_below = room.saturating_sub(head.len()).max(1);
        if moved {
            if let Some(at) = cursor {
                board = tc::follow(board, at.saturating_sub(head.len()), room_below);
            }
            moved = false;
        }
        board = board.min(rest.len().saturating_sub(room_below));
        let last = (board + room_below).min(rest.len());
        let mut rows: Vec<String> = head.to_vec();
        rows.extend_from_slice(&rest[board..last]);
        while rows.len() < room {
            rows.push(String::new());
        }
        rows.extend(footer);
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_that_will_undercount_says_so() {
        // A classic token missing `repo` still searches - it just silently
        // returns public results only, which is worse than an error.
        let short = Scopes {
            seen: true,
            have: vec!["read:org".into()],
        };
        assert!(scope_warning(&short).contains("private repos are not counted"));
        let no_org = Scopes {
            seen: true,
            have: vec!["repo".into()],
        };
        assert!(scope_warning(&no_org).contains("orgs cannot be discovered"));
        let full = Scopes {
            seen: true,
            have: vec!["repo".into(), "read:org".into(), "gist".into()],
        };
        assert_eq!(scope_warning(&full), "");
        // A fine-grained token sends no scope header at all, so there is
        // nothing to check and nothing to claim.
        assert_eq!(scope_warning(&Scopes::default()), "");
    }

    #[test]
    fn org_discovery_follows_the_cursor() {
        // The first page asks for no cursor at all, and for the page
        // information that says whether there is another.
        let first = orgs_query(None);
        assert!(first.contains("organizations(first: 100)"), "{}", first);
        assert!(first.contains("hasNextPage") && first.contains("endCursor"));
        // A cursor is a string GitHub chose, so it goes in quoted rather
        // than pasted: it has carried `=` and `==` for as long as it has
        // been base64.
        let next = orgs_query(Some("Y3Vyc29yOnYyOpHOAAQ="));
        assert!(
            next.contains(r#"organizations(first: 100, after: "Y3Vyc29yOnYyOpHOAAQ=")"#),
            "{}",
            next
        );
    }

    #[test]
    fn an_account_scopes_its_own_search() {
        assert_eq!(scope_of("acme", "wiiiimm"), "org:acme");
        // @me is the viewer, resolved once rather than sent literally.
        assert_eq!(scope_of("@me", "wiiiimm"), "user:wiiiimm");
    }

    #[test]
    fn the_window_ends_today_and_spans_exactly_its_days() {
        // `days - 1` back from today, because the chart under it plots N
        // days *including* today - one more would quietly disagree with it.
        let q = build_query("acme", 7, "w", Utc::now());
        let since = (today() - Days::days(6)).format("%Y-%m-%d").to_string();
        assert!(q.contains(&format!("merged:>={}", since)), "{}", q);
        assert!(q.contains("org:acme is:pr is:open"));
        assert!(q.contains("rateLimit"));
    }

    #[test]
    fn the_rolling_day_carries_its_time_of_day() {
        // A date-only cut is the calendar-day bug: at nine in the morning
        // it reports three hours of evidence as a day's throughput. The
        // `T…Z` is what makes GitHub read this as twenty-four hours back
        // from now, and it is the whole reason `now` is a parameter.
        let now = DateTime::parse_from_rfc3339("2026-09-13T11:22:33Z")
            .unwrap()
            .with_timezone(&Utc);
        let q = build_query("acme", 7, "w", now);
        assert!(
            q.contains("is:pr created:>=2026-09-12T11:22:33Z"),
            "{}",
            q
        );
        assert!(
            q.contains("is:pr is:merged merged:>=2026-09-12T11:22:33Z"),
            "{}",
            q
        );
        // Eight aliases, the number the request was re-measured at. A
        // ninth is not free: the ceiling is the request's complexity, and
        // this is the line that says one was added without checking.
        assert_eq!(q.matches("search(").count(), 8, "{}", q);
        assert_eq!(q.matches("issueCount").count(), 8, "{}", q);
    }

    #[test]
    fn a_sum_missing_an_account_is_not_a_total() {
        // `stats` holds only the accounts that have arrived, so the board
        // spends its first seconds holding one row of ten - every field of
        // it present. Counting the rows would call that a total.
        let row = |o: Option<i64>| Account {
            opened_24h: o,
            ..Default::default()
        };
        let three = [row(Some(4)), row(Some(5)), row(Some(6))];
        assert_eq!(board_24h(&three, 3, |s| s.opened_24h), Some(15));
        assert_eq!(board_24h(&three[..1], 3, |s| s.opened_24h), None);
        // An account that arrived without the alias is the other half: a
        // full board of rows, one of them holding nothing.
        let holed = [row(Some(4)), row(None), row(Some(6))];
        assert_eq!(board_24h(&holed, 3, |s| s.opened_24h), None);
        // And no accounts at all sums to nothing, not to zero.
        assert_eq!(board_24h(&[], 0, |s| s.opened_24h), None);
    }

    #[test]
    fn a_failed_refresh_is_not_a_current_total() {
        // After an account has landed once, a later aggregate failure
        // leaves its row in `by_acc`. Clearing only the rolling-day pair
        // keeps the table on screen and the figures off it — the mutation
        // that would make this fail is leaving the previous Some values.
        let mut stale = Account {
            opened_24h: Some(4),
            merged_24h: Some(5),
            open: 12,
            ..Default::default()
        };
        forget_24h(&mut stale);
        assert_eq!(stale.opened_24h, None);
        assert_eq!(stale.merged_24h, None);
        assert_eq!(stale.open, 12);
        let fresh = Account {
            opened_24h: Some(7),
            merged_24h: Some(1),
            ..Default::default()
        };
        assert_eq!(board_24h(&[stale, fresh], 2, |s| s.opened_24h), None);
    }

    #[test]
    fn a_mid_pass_mix_is_not_a_current_total() {
        // After the first successful pass, every row holds Some. A new
        // rolling_now starts; the first account lands with this cutoff
        // while the rest still hold last pass. The gate sees two Somes
        // and would draw 4+7 as this window's total — that mix is 11.
        let last_pass = Account {
            opened_24h: Some(4),
            merged_24h: Some(1),
            ..Default::default()
        };
        let this_pass = Account {
            opened_24h: Some(7),
            merged_24h: Some(2),
            ..Default::default()
        };
        assert_eq!(
            board_24h(&[this_pass.clone(), last_pass.clone()], 2, |s| s.opened_24h),
            Some(11)
        );
        // Forgetting every retained row before the loop, then landing the
        // first account of this cutoff, leaves a hole. The figures shimmer
        // instead of showing 11.
        let mut mid = [last_pass, this_pass.clone()];
        for row in &mut mid {
            forget_24h(row);
        }
        mid[0] = this_pass;
        assert_eq!(board_24h(&mid, 2, |s| s.opened_24h), None);
        // Once every account reports for this cutoff, the sum is the
        // complete value — 7+9, not the mixed 11.
        let complete = [
            Account {
                opened_24h: Some(7),
                merged_24h: Some(2),
                ..Default::default()
            },
            Account {
                opened_24h: Some(9),
                merged_24h: Some(3),
                ..Default::default()
            },
        ];
        assert_eq!(board_24h(&complete, 2, |s| s.opened_24h), Some(16));
        assert_eq!(board_24h(&complete, 2, |s| s.merged_24h), Some(5));
    }

    #[test]
    fn a_missing_alias_is_absent_rather_than_zero() {
        // `count_at` lands a missing alias on zero, which is the one thing
        // these two figures must never do.
        let payload: serde_json::Value =
            serde_json::from_str(r#"{"o0_o24": {"issueCount": 0}}"#).unwrap();
        assert_eq!(figure_at(&payload, "o0_o24"), Some(0));
        assert_eq!(figure_at(&payload, "o0_m24"), None);
        assert_eq!(count_at(&payload, "o0_m24"), 0);
    }

    #[test]
    fn the_figures_stand_down_before_the_chart_does() {
        // The column is the wider of the two labels plus its gutter, and
        // it is only taken where the chart can still hold all its days and
        // stay above `MIN_CHART`.
        let col = FIG_LABELS.iter().map(|l| tc::display_width(l)).max().unwrap() + 2;
        assert_eq!(figure_col(col + MIN_CHART, 7), col);
        assert_eq!(figure_col(col + MIN_CHART - 1, 7), 0);
        // A window longer than the floor pays for itself in days, not in
        // the floor: 90 days needs 90 columns left over, not 20.
        assert_eq!(figure_col(col + 89, 90), 0);
        assert_eq!(figure_col(col + 90, 90), col);
        // The two widths the change was read back at, so the commit and
        // the pull request quote something pinned rather than recomputed
        // by hand: a 56-column pane, which is the narrower of the two this
        // board is actually on, and a 35-column one, which is not.
        assert_eq!(figure_col(56usize.saturating_sub(3), 7), col);
        assert_eq!(figure_col(35usize.saturating_sub(3), 7), 0);
    }

    #[test]
    fn a_figure_that_has_not_arrived_is_not_a_zero() {
        let p = palette();
        let plain = |rows: &[Vec<(String, String)>]| -> Vec<String> {
            rows.iter()
                .map(|r| r.iter().map(|(_, t)| t.clone()).collect::<String>())
                .collect()
        };
        // A real zero draws the glyph. Nothing opened in a day is a
        // reading, and it has to be legible as one.
        let zero = plain(&figure_rows(Some(0), Some(0), 19, 0, &p));
        assert!(
            zero.iter().any(|r| r.contains('█')),
            "a counted zero lost its digits: {:?}",
            zero
        );
        // Nothing arrived draws no digit at all - if this ever falls
        // through to `Some(0)` the two readings become one screen.
        let waiting = plain(&figure_rows(None, None, 19, 0, &p));
        for row in &waiting {
            assert!(
                !row.contains('▀') && !row.contains('▄'),
                "a figure still counting drew a digit: {:?}",
                waiting
            );
        }
        // Both halves always say what their window is, whichever state
        // they are in - a figure whose window nobody can read is not a
        // figure.
        for rows in [waiting, zero] {
            for label in FIG_LABELS {
                assert!(
                    rows.iter().any(|r| r.contains(label)),
                    "{} went missing: {:?}",
                    label,
                    rows
                );
            }
        }
        // Eight rows, always, because that is the height of the chart
        // beside it and the two have to end level.
        assert_eq!(figure_rows(Some(3), None, 19, 0, &p).len(), 8);
    }

    #[test]
    fn the_figure_column_never_overflows_or_loses_its_label() {
        // `seg` clips from the right, so an off-by-one in the reserve eats
        // the label silently rather than erroring.
        let p = palette();
        // `display_width` counts what it is given, and these rows carry
        // real colours - so the escapes come off before anything is
        // measured, or every row measures as wildly too wide.
        let plain = |s: &str| -> String {
            let mut out = String::new();
            let mut chars = s.chars();
            while let Some(ch) = chars.next() {
                if ch == '\u{1b}' {
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else {
                    out.push(ch);
                }
            }
            out
        };
        // One chart row of `cols` columns with the nth figure fragment
        // after it - the shape both PR FLOWs build.
        let row_at = |cols: usize, figw: usize, n: usize, w: usize| -> String {
            let figs = (figw > 0).then(|| figure_rows(Some(7), Some(123), figw, 0, &p));
            with_figure(
                vec![(String::new(), " ".to_string()), (String::new(), "─".repeat(cols))],
                figs.as_ref().and_then(|f| f.get(n)),
                1 + cols,
                w - 1,
            )
        };
        let mut narrowing_mattered = 0usize;
        for w in 20..=200usize {
            let (cols, figw) = chart_split(w, 7);
            for n in 0..8 {
                let row = row_at(cols, figw, n, w);
                assert!(
                    tc::display_width(&plain(&row)) <= w - 1,
                    "width {} row {} overflowed: {:?}",
                    w,
                    n,
                    row
                );
                let Some(label) = (figw > 0).then(|| FIG_LABELS.get(usize::from(n == 7)))
                    .flatten()
                    .filter(|_| n == 3 || n == 7)
                else {
                    continue;
                };
                assert!(row.contains(label), "width {} cut {}: {:?}", w, label, row);
                // And the narrowing is what saved it. Spreading the days
                // across the whole pane and hanging the figures off the end
                // is the mistake this whole column exists to avoid, so the
                // clause above is measured against a row that made it.
                let wide = row_at(w.saturating_sub(3).max(10), figw, n, w);
                if !wide.contains(label) {
                    narrowing_mattered += 1;
                }
            }
        }
        // Never zero, or every clause above passed on a column that was
        // never in any danger.
        assert!(
            narrowing_mattered > 0,
            "the label survived even without narrowing the chart"
        );
    }

    #[test]
    fn a_day_query_asks_for_counts_not_records() {
        // A search connection returns at most 100 nodes a page, so a busy
        // fortnight lost everything past the hundredth record. issueCount is
        // exact at any volume.
        let dates = vec!["2026-08-01".to_string(), "2026-08-02".to_string()];
        let q = build_day_query("org:acme", &dates);
        assert_eq!(q.matches("issueCount").count(), 4);
        assert!(q.contains("m0:") && q.contains("c0:"));
        assert!(q.contains("m1:") && q.contains("c1:"));
        assert!(!q.contains("nodes"));
    }

    #[test]
    fn a_streak_is_not_broken_by_a_day_still_running() {
        // Sunday through Saturday, with today scoring nothing yet.
        let weeks: serde_json::Value = serde_json::from_str(&format!(
            r#"[{{"contributionDays": [
                {{"date": "{}", "contributionCount": 3, "weekday": 0}},
                {{"date": "{}", "contributionCount": 5, "weekday": 1}},
                {{"date": "{}", "contributionCount": 0, "weekday": 2}}]}}]"#,
            (today() - Days::days(2)).format("%Y-%m-%d"),
            (today() - Days::days(1)).format("%Y-%m-%d"),
            today().format("%Y-%m-%d"),
        ))
        .unwrap();
        let cs = calendar_stats(&weeks).expect("a calendar");
        // Two days behind it, and today's zero does not end the run.
        assert_eq!(cs.current, 2);
        assert_eq!(cs.longest, 2);
        assert_eq!(cs.today, 0);
        assert_eq!(cs.active, 2);
        assert_eq!(cs.busiest.1, 5);
    }

    #[test]
    fn the_contributions_qualifier_gives_way_before_the_numbers_do() {
        // Colours are empty strings here, so what `seg` returns is exactly
        // what lands on the screen, character for character.
        for &(total, peak) in &[(6024i64, 241i64), (12345, 9), (7, 7)] {
            let numbers = format!("{} in {} weeks, peak {}/day", total, CONTRIB_WEEKS, peak);
            let mut widest_without = 0usize;
            for w in 20..=200usize {
                let with = contributions_heading(total, peak, w, "", "");
                // What the heading would be with no qualifier at all: the
                // numbers may only survive where they survived before.
                let bare = tc::seg(
                    &[("", CONTRIB_LABEL.into()), ("", numbers.clone())],
                    w - 1,
                );
                assert_eq!(
                    with.ends_with(&numbers),
                    bare.ends_with(&numbers),
                    "width {} cost the numbers: {:?}",
                    w,
                    with
                );
                assert!(tc::display_width(&with) <= w - 1, "width {}: {:?}", w, with);
                if !with.contains("yours") {
                    widest_without = w;
                }
            }
            // And it does actually say it where there is room - a ladder
            // that silently degrades to nothing everywhere would otherwise
            // pass the clause above.
            assert!(
                contributions_heading(total, peak, 200, "", "").contains("yours, everywhere"),
                "the full qualifier never appears"
            );
            assert!(
                widest_without < 200,
                "the qualifier is missing at every width"
            );
        }
        // And the rungs themselves, for the figures on the board this was
        // written against - so the widths quoted in the commit and the pull
        // request are pinned by something rather than recomputed by hand.
        let rung = |w: usize| contributions_heading(6024, 241, w, "", "");
        assert!(rung(72).contains("yours, everywhere"));
        assert!(!rung(71).contains("everywhere") && rung(71).contains("yours"));
        assert!(!rung(59).contains("yours"));
        assert!(rung(52).ends_with("6024 in 52 weeks, peak 241/day"));
        assert!(!rung(51).ends_with("peak 241/day"));
    }

    #[test]
    fn the_heatmap_is_seven_rows_whatever_the_data() {
        let weeks: serde_json::Value = serde_json::from_str(
            r#"[{"contributionDays": [
                {"date": "2026-08-16", "contributionCount": 0, "weekday": 0},
                {"date": "2026-08-17", "contributionCount": 9, "weekday": 1}]}]"#,
        )
        .unwrap();
        let (grid, peak, total) = heatmap(&weeks, 80);
        assert_eq!(grid.len(), 7);
        assert_eq!(peak, 9);
        assert_eq!(total, 9);
        // Nothing on a day is a blank cell, not the lowest shade - the
        // difference between "quiet" and "none" is the whole point.
        assert_eq!(grid[0].chars().next(), Some(' '));
        assert_eq!(grid[1].chars().next(), Some('█'));
    }
}
