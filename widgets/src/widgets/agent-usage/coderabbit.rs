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
//!
//! The tab does draw one estimate, and says it is one: the fair-use rate
//! CodeRabbit's published table gives for the seven-day review count, which
//! is worked out from how `Your reviews` moved across the readings this
//! widget has kept. The count is measured; the rate is the table's, looked
//! up with the plan `coderabbit auth status` names.
//! The CLI owns the login; nothing here reads or touches its credentials.

use chrono::{Local, NaiveDate};
use opscope_core as tc;

use crate::parse::{
    coderabbit_fair_use, coderabbit_fair_use_tiers, coderabbit_quota_field, coderabbit_signed_out,
    parse_coderabbit_plan, parse_coderabbit_usage, CodeRabbitUsage,
};
use crate::shared::*;
use crate::*;

const CLI: &str = "coderabbit";

/// How long a report is held. A review count moves a few times a day, and
/// each ask is a round trip to CodeRabbit on the reader's login.
const REPORT_TTL: f64 = 600.0;

/// The span CodeRabbit's fair-use policy counts pull request reviews over.
const WEEK: f64 = 7.0 * 86400.0;

/// How long the plan from `coderabbit auth status` is held. It changes when
/// somebody changes the subscription, which is not an hourly event.
const PLAN_TTL: f64 = 6.0 * 3600.0;

/// The fields the tab lays out itself; any other line the report carries
/// is listed after them as it came.
const PLACED: &[&str] = &["your reviews", "period resets", "organization", "user", "plan"];

#[derive(Clone, Default)]
pub struct Data {
    usage: Option<CodeRabbitUsage>,
    /// When the report was taken.
    read_at: f64,
    why: String,
    /// The billing-period review count as this widget has read it over the
    /// last week, as (when, count), oldest first.
    samples: Vec<(f64, u64)>,
    /// The plan `coderabbit auth status` named, or empty when it named none.
    plan: String,
    /// The reading could not be written, so a restarted pane starts over.
    unsaved: bool,
}

/// One `coderabbit usage`, as the report or as why there was none.
fn ask(repo: &str) -> Result<serde_json::Value, String> {
    let dir = repo_dir(repo);
    // Checked here rather than left to the spawn, which reports a missing
    // directory as `coderabbit: No such file or directory` - the CLI's name
    // on the setting's fault, pointing at a reinstall that would not help.
    if let Some(dir) = dir.as_deref().filter(|d| !d.is_dir()) {
        return Err(format!("coderabbit_repo {} is not a directory", dir.display()));
    }
    let out = tc::run_full_in(&[CLI, "usage"], 15, dir.as_deref())?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // A partial report from a run that then failed is not a report: taking
    // it would hold a failure as a reading for the full ten minutes.
    if let Some(u) = parse_coderabbit_usage(&text).filter(|_| out.status.success()) {
        let at = now();
        // Kept on disk because a week is longer than any pane stays open,
        // and held with the report so a frame does not read the file.
        let period = u.get("period resets").unwrap_or("");
        let (samples, saved) = u
            .reviews()
            .map(|n| record_sample(&samples_path(), &sample_key(&u), period, at, n))
            .unwrap_or((Vec::new(), true));
        return Ok(serde_json::json!({
            "text": text, "at": at, "samples": samples, "unsaved": !saved,
        }));
    }
    Err(if coderabbit_signed_out(&text) {
        "not signed in · run coderabbit auth login".to_string()
    } else if !out.status.success() {
        format!("coderabbit usage exited {}", out.status)
    } else {
        "coderabbit usage printed no report this widget can read".to_string()
    })
}

/// The plan from `coderabbit auth status`, or None when it names none. A
/// failure here costs the rate and nothing else, and the tab says the plan
/// is not known.
fn ask_plan() -> Option<serde_json::Value> {
    let out = tc::run_full(&[CLI, "auth", "status"], 15).ok()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let plan = parse_coderabbit_plan(&text).filter(|_| out.status.success())?;
    Some(serde_json::json!({"plan": plan}))
}

fn samples_path() -> String {
    format!("{}/opscope/coderabbit-reviews.json", crate::claude::snapshot_state_home())
}

/// Whose count a sample is, so a second login's counts are never
/// subtracted from the first's.
fn sample_key(u: &CodeRabbitUsage) -> String {
    format!("{}@{}", u.get("user").unwrap_or(""), u.get("organization").unwrap_or(""))
}

/// Add a reading to the file at `path`, and return this login's samples
/// and whether the file took them.
///
/// A run of the same count keeps only its first and last reading: the
/// first says when the count got there, the last how long it held, and
/// everything between says nothing more. Samples older than a week go,
/// except the newest of them, which is where the week's count starts, and
/// a login with nothing newer than a week goes altogether.
///
/// `period` is the report's `Period resets` date. When it moves, the count
/// restarted between the last reading and this one, which a count that
/// rose anyway would hide, so a zero is kept just before this reading.
fn record_sample(path: &str, key: &str, period: &str, at: f64, count: u64) -> (Vec<(f64, u64)>, bool) {
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Two panes may share the file, so the whole read, change and rename is
    // held under a lock beside it; otherwise the second rename drops the
    // first pane's reading. Released when `lock` closes. With no lock to be
    // had nothing is written, so a pane cannot overwrite another's reading,
    // and the reading is reported unsaved, which the tab says.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(format!("{}.lock", path))
        .ok()
        .filter(|f| f.lock().is_ok());
    let mut all = read_json(path).filter(|v| v.is_object()).unwrap_or_else(|| serde_json::json!({}));
    let mut samples = stored_samples(&all[key]);
    samples.sort_by(|a, b| a.0.total_cmp(&b.0));
    let newest = samples.last().map_or(at, |(t, _)| t.max(at));
    let was = text(&all[key], "period");
    let n = samples.len();
    if newest > at || samples.last().is_some_and(|(t, _)| *t == at) {
        // Older than a reading already kept: a clock stepped back, or a
        // pane that wrote late. Put in its place rather than erasing what
        // came after, replacing one taken at the same moment.
        samples.retain(|(t, _)| *t != at);
        let i = samples.partition_point(|(t, _)| *t < at);
        samples.insert(i, (at, count));
    } else {
        if n > 0 && !was.is_empty() && !period.is_empty() && was != period {
            samples.push((at - 0.001, 0));
        } else if n >= 2 && samples[n - 1].1 == count && samples[n - 2].1 == count {
            samples.pop();
        }
        samples.push((at, count));
    }
    if let Some(start) = samples.iter().rposition(|(t, _)| *t <= newest - WEEK) {
        samples.drain(..start);
    }
    // The period of the newest reading, which is what the next one compares.
    let period = if newest > at { was } else { period.to_string() };
    all[key] = serde_json::json!({
        "period": period,
        "samples": samples.iter().map(|(t, c)| serde_json::json!([t, c])).collect::<Vec<_>>(),
    });
    if let Some(map) = all.as_object_mut() {
        map.retain(|_, v| stored_samples(v).iter().any(|(t, _)| *t > newest - WEEK));
    }
    // Renamed into place, so a pane reading it never sees half a file.
    if lock.is_none() {
        return (samples, false);
    }
    let tmp = format!("{}.{}.tmp", path, std::process::id());
    let saved = std::fs::write(&tmp, all.to_string()).is_ok() && std::fs::rename(&tmp, path).is_ok();
    if !saved {
        let _ = std::fs::remove_file(&tmp);
    }
    (samples, saved)
}

/// One login's readings as the file holds them, as (when, count).
fn stored_samples(entry: &serde_json::Value) -> Vec<(f64, u64)> {
    entry["samples"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| Some((s.get(0)?.as_f64()?, s.get(1)?.as_u64()?)))
                .collect()
        })
        .unwrap_or_default()
}

/// Reviews added over a span, from readings of a count that restarts each
/// billing period.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Counted {
    reviews: u64,
    /// Where the count starts: a week back, or the first reading when this
    /// widget has not been reading that long.
    since: f64,
    /// Whether the readings reach back a whole week.
    whole: bool,
}

/// The reviews added in the `span` up to `at`.
///
/// Counted from the newest reading at or before the start of the span, so
/// reviews between it and the next reading are counted even where some of
/// them fell just before the start: the count can run over, never under,
/// which puts the estimated rate on the cautious side. A count that fell
/// is a new billing period, and everything in it is new.
fn reviews_across(samples: &[(f64, u64)], at: f64, span: f64) -> Option<Counted> {
    let first = samples.iter().rposition(|(t, _)| *t <= at - span);
    let from = first.unwrap_or(0);
    let start = samples.get(from)?;
    let reviews = samples[from..]
        .windows(2)
        .map(|p| if p[1].1 >= p[0].1 { p[1].1 - p[0].1 } else { p[1].1 })
        .sum();
    Some(Counted { reviews, since: start.0, whole: first.is_some() })
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
    forget_once_returned(caches);
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
        d.read_at = num(&got, "at");
        d.unsaved = got["unsaved"].as_bool().unwrap_or(false);
        d.samples = got["samples"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| Some((s.get(0)?.as_f64()?, s.get(1)?.as_u64()?)))
                    .collect()
            })
            .unwrap_or_default();
        // Only once a report has come back: a signed-out CLI has no plan.
        // Held per login, so switching accounts is never judged on the
        // last account's table for the six hours a plan is held.
        let key = format!("coderabbit-plan:{}", sample_key(&usage));
        d.plan = cached(caches, &key, PLAN_TTL, ask_plan)
            .map(|v| text(&v, "plan"))
            .unwrap_or_default();
        d.usage = Some(usage);
    }
    d
}

/// Drop a held report once the return time it named has passed, so the
/// next frame asks again rather than drawing `0 of 5 left` for the rest of
/// the ten minutes after capacity came back.
fn forget_once_returned(caches: &mut Caches) {
    let returned = caches
        .live
        .get("coderabbit")
        .and_then(|(_, v, _)| v.as_ref())
        .and_then(|v| {
            let u = parse_coderabbit_usage(&text(v, "text"))?;
            u.returns_at(num(v, "at"))
        })
        .is_some_and(|r| r <= now());
    if returned {
        caches.live.remove("coderabbit");
    }
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
        // Only between the return and the next ask, which is due now.
        Some(_) => " · returned since this reading, asking again".into(),
        None => String::new(),
    };
    // CodeRabbit names the repository it read the allowance in, which is
    // the one coderabbit_repo points at; saying so shows where it came from.
    let repo = u.get("repository").map(|r| format!(" · in {}", r)).unwrap_or_default();
    // Under the bar where there is room for it, flush left where there is
    // not: a fixed indent on a pane this narrow leaves nothing to wrap into.
    let indent_w = if w > label_w + 3 + 16 { label_w + 3 } else { 1 };
    let indent = " ".repeat(indent_w);
    let room = w.saturating_sub(indent_w + 1).max(1);
    // Wrapped rather than cut: the return and the repository together run
    // past a narrow pane. The count keeps its own colour on the first line.
    let count = format!("{} of {}", left, of);
    let said = format!("{} left{}{}", count, when, repo);
    for (i, line) in tc::wrap_words(&said, room).into_iter().enumerate() {
        let rest = line.strip_prefix(&count).filter(|_| i == 0);
        let parts = match rest {
            Some(rest) => vec![
                (p.dim.as_str(), indent.clone()),
                (p.txt.as_str(), count.clone()),
                (p.dim.as_str(), rest.to_string()),
            ],
            None => vec![(p.dim.as_str(), format!("{}{}", indent, line))],
        };
        rows.push(tc::seg(&parts, w - 1));
    }
    // The report does not say which allowance this is, and pull request
    // reviews have been held to fewer while it read full; see `lanes`.
    let caveat = "CodeRabbit keeps pull request, CLI and IDE reviews on separate allowances, \
                  and does not say which this is; pull request reviews may be limited sooner.";
    for line in tc::wrap_words(caveat, room) {
        rows.push(tc::seg(&[(p.dim.as_str(), format!("{}{}", indent, line))], w - 1));
    }
    rows
}

/// The seven-day review count and the fair-use rate the published table
/// gives for it, both marked as estimates.
fn fair_use_rows(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let Some(week) = reviews_across(&d.samples, d.read_at, WEEK) else {
        return Vec::new();
    };
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── FAIR USE ── ".into()),
            (p.dim.as_str(), "estimate · from how your reviews count moved".into()),
        ],
        w - 1,
    )];
    let label_w = 7;
    let indent_w = if w > label_w + 3 + 16 { label_w + 3 } else { 1 };
    let indent = " ".repeat(indent_w);
    let room = w.saturating_sub(indent_w + 1).max(1);
    // Said first, since it is why a count may start over after a restart.
    if d.unsaved {
        let said = format!("readings could not be saved to {}, so a restarted pane starts over", samples_path());
        for line in tc::wrap_words(&said, room) {
            rows.push(tc::seg(&[(p.warn.as_str(), format!("{}{}", indent, line))], w - 1));
        }
    }
    // One reading is where a count starts, not a count: a bar or a rate
    // from it would draw a zero nobody measured.
    if d.samples.len() < 2 {
        let said = format!("readings began {} ago; a count needs a later one", ago(week.since));
        for line in tc::wrap_words(&said, room) {
            rows.push(tc::seg(&[(p.dim.as_str(), format!("{}{}", indent, line))], w - 1));
        }
        return rows;
    }
    let plan = d.plan.trim();
    let tiers = coderabbit_fair_use_tiers(plan);
    // Filled toward the count where reviews go one at a time, which is the
    // end of the table and the thing the bar is there to show coming.
    if let Some(tiers) = tiers {
        let last = tiers.last().map(|(from, _)| *from).unwrap_or(1).max(1);
        let used = (week.reviews as f64 / last as f64).clamp(0.0, 1.0);
        let bar_room = ((w as i64) - 24 - label_w as i64).max(8) as usize;
        let mut line: Vec<(String, String)> =
            vec![(p.dim.clone(), format!(" {} ", tc::pad("7 days", label_w)))];
        line.extend(paced_bar(used, None, bar_room, agent_hue("coderabbit"), p));
        line.push((p.txt.clone(), format!(" {}", week.reviews)));
        line.push((p.dim.clone(), format!(" of {}", last)));
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&refs, w - 1));
    }
    let count = if week.whole {
        format!("~{} reviews in the last 7 days", week.reviews)
    } else {
        format!(
            "at least {} reviews in the {} since readings began",
            week.reviews,
            left_span(d.read_at - week.since)
        )
    };
    // A count short of a week can only grow, so its rate can only fall:
    // the most it can be, not what it is.
    let rate = match coderabbit_fair_use(plan, week.reviews) {
        Some(f) => {
            let now_at = if f.one_at_a_time {
                "one review at a time".to_string()
            } else {
                format!("{} reviews an hour", f.rate)
            };
            let then = match f.next {
                // The last tier is the only one at a rate of one.
                Some((from, 1)) => format!(" · one at a time from {}", from),
                Some((from, rate)) => format!(" · {} an hour from {}", rate, from),
                None => String::new(),
            };
            let so_far = if week.whole { "about" } else { "at most" };
            format!(" · {} {} on {}{}", so_far, now_at, plan, then)
        }
        None if plan.is_empty() => " · the plan is not known, so no rate".into(),
        None => format!(" · CodeRabbit publishes no fair-use table for {}, so no rate", plan),
    };
    let full = if week.whole {
        String::new()
    } else {
        let at = chrono::DateTime::from_timestamp((week.since + WEEK) as i64, 0)
            .map(|t| t.with_timezone(&Local).format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        format!(" · a full week on {}", at)
    };
    for line in tc::wrap_words(&format!("{}{}{}", count, rate, full), room) {
        rows.push(tc::seg(&[(p.txt.as_str(), format!("{}{}", indent, line))], w - 1));
    }
    let caveat = "Only pull request reviews count toward fair use, and your reviews may also \
                  count CLI and IDE ones, so the real rate may be higher than this.";
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
/// `auth_plan` is what `coderabbit auth status` named, for a report that
/// names no plan itself.
fn plan(u: &CodeRabbitUsage, auth_plan: &str, drawn: bool, w: usize, p: &Palette) -> Vec<String> {
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
    plan_rows(u.get("plan").unwrap_or(auth_plan), &pairs, w, "", None, "", p)
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
    let fair = fair_use_rows(d, w, p);
    if !fair.is_empty() {
        rows.extend(fair);
        rows.push(String::new());
    }
    rows.extend(report_rows(d, u, w, p));
    add_section(rows, plan(u, &d.plan, drawn, w, p))
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
        let d = Data { usage: Some(report()), read_at: now(), why: String::new(), ..Data::default() };
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
    fn a_held_report_is_dropped_once_its_return_time_passes() {
        // A return still ahead keeps the reading; one gone by forgets it.
        let held = |at: f64| {
            let v = serde_json::json!({
                "text": "Available reviews : 0 of 5\nCapacity returns : in 2m\n",
                "at": at,
            });
            let mut caches = Caches::default();
            caches.live.insert("coderabbit".into(), (at, Some(v), REPORT_TTL));
            forget_once_returned(&mut caches);
            caches.live.contains_key("coderabbit")
        };
        assert!(held(now() - 60.0));
        assert!(!held(now() - 180.0));
    }

    #[test]
    fn a_repository_that_is_not_a_directory_names_the_setting() {
        // Refused before the CLI is started, so the fault is not put on it.
        let why = ask("/no/such/directory/for/opscope").unwrap_err();
        assert!(why.starts_with("coderabbit_repo /no/such/directory"), "{why}");
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
        let d = Data { usage: Some(u), read_at: now(), why: String::new(), ..Data::default() };
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
        let d = Data { usage: Some(u), read_at, why: String::new(), ..Data::default() };
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
        let d = Data { usage: Some(with_allowance("4")), read_at: now(), why: String::new(), ..Data::default() };
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
        let fresh = Data { usage: Some(with_allowance("3 of 5")), read_at: now(), why: String::new(), ..Data::default() };
        assert!(!allowance(&fresh)[0].stale);
        let old = Data { read_at: now() - REPORT_TTL - 60.0, ..fresh };
        assert!(allowance(&old)[0].stale);
    }

    #[test]
    fn quota_lines_are_not_listed_again_among_the_plan_fields() {
        // The allowance has its own section; the plan list skips its lines.
        let d = Data { usage: Some(with_allowance("3 of 5")), read_at: now(), why: String::new(), ..Data::default() };
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
    fn the_week_counts_what_the_readings_added_across_it() {
        // Three days of readings: 40 at the start, 52 now, so twelve.
        let at = 10.0 * 86400.0;
        let day = 86400.0;
        let partial = [(at - 3.0 * day, 40), (at - day, 47), (at, 52)];
        assert_eq!(
            reviews_across(&partial, at, WEEK),
            Some(Counted { reviews: 12, since: at - 3.0 * day, whole: false })
        );
        // A reading from before the week starts the count there, and makes it whole.
        let whole = [(at - 9.0 * day, 1), (at - 8.0 * day, 30), (at - 2.0 * day, 70), (at, 85)];
        assert_eq!(
            reviews_across(&whole, at, WEEK),
            Some(Counted { reviews: 55, since: at - 8.0 * day, whole: true })
        );
        // A count that fell is a new billing period; all of it is new.
        let reset = [(at - 8.0 * day, 90), (at - 3.0 * day, 96), (at - day, 4), (at, 9)];
        assert_eq!(reviews_across(&reset, at, WEEK).unwrap().reviews, 15);
        assert_eq!(reviews_across(&[], at, WEEK), None);
    }

    #[test]
    fn a_sample_file_keeps_each_login_apart_and_a_run_at_its_ends() {
        // Written to a scratch file; the widget's own is never touched.
        let path = std::env::temp_dir()
            .join(format!("opscope-coderabbit-test-{}.json", std::process::id()))
            .display()
            .to_string();
        let _ = std::fs::remove_file(&path);
        let t = 1_000_000.0;
        let record = |key: &str, at: f64, n: u64| {
            let (samples, saved) = record_sample(&path, key, "2026-10-06", at, n);
            assert!(saved);
            samples
        };
        record("a@org", t, 10);
        record("a@org", t + 600.0, 10);
        record("a@org", t + 1200.0, 10);
        let got = record("a@org", t + 1800.0, 12);
        // The middle 10 said nothing the first and last did not.
        assert_eq!(got, vec![(t, 10), (t + 1200.0, 10), (t + 1800.0, 12)]);
        // Another login's readings are its own.
        assert_eq!(record("b@org", t + 60.0, 3), vec![(t + 60.0, 3)]);
        // Past a week, only the newest reading before the week is kept.
        let later = t + WEEK + 1500.0;
        let kept = record("a@org", later, 20);
        assert_eq!(kept, vec![(t + 1200.0, 10), (t + 1800.0, 12), (later, 20)]);
        // And the other login, with nothing newer than a week, is gone.
        let file = read_json(&path).unwrap();
        assert!(file.get("b@org").is_none(), "{file}");
        // A reading from before the newest, as after a clock steps back, is
        // put in its place and erases nothing after it.
        let back = record("a@org", later - 60.0, 19);
        assert_eq!(back, vec![(t + 1200.0, 10), (t + 1800.0, 12), (later - 60.0, 19), (later, 20)]);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path));
    }

    #[test]
    fn a_new_billing_period_restarts_the_count_even_when_it_rose() {
        // Nine before the reset and forty after is forty new, not thirty-one.
        let path = std::env::temp_dir()
            .join(format!("opscope-coderabbit-period-{}.json", std::process::id()))
            .display()
            .to_string();
        let _ = std::fs::remove_file(&path);
        let t = 2_000_000.0;
        record_sample(&path, "a@org", "2026-10-06", t, 9);
        let (samples, _) = record_sample(&path, "a@org", "2026-11-06", t + 3600.0, 40);
        assert_eq!(reviews_across(&samples, t + 3600.0, WEEK).unwrap().reviews, 40);
        // The same period carries on as a rise.
        let (samples, _) = record_sample(&path, "a@org", "2026-11-06", t + 7200.0, 45);
        assert_eq!(reviews_across(&samples, t + 7200.0, WEEK).unwrap().reviews, 45);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path));
    }

    #[test]
    fn a_reading_that_could_not_be_saved_says_so() {
        // Otherwise a restarted pane's shorter count looks like fewer reviews.
        let (_, saved) = record_sample("/proc/opscope-no-such/x.json", "a@org", "", now(), 3);
        assert!(!saved);
        let at = now();
        let d = Data { unsaved: true, ..week_of(vec![(at - 3600.0, 90), (at, 95)], "Team") };
        let all = tab(&d, 100, 40, &Config::default(), &palette()).join("\n");
        assert!(all.contains("could not be saved"), "{all}");
    }

    fn week_of(samples: Vec<(f64, u64)>, plan: &str) -> Data {
        let usage = parse_coderabbit_usage("Your reviews : 95\nPeriod resets : 2026-10-06\n");
        Data { usage, read_at: now(), samples, plan: plan.into(), ..Data::default() }
    }

    #[test]
    fn a_whole_week_gives_the_estimated_rate_on_the_plan() {
        // Fifty-five on Team is the 50-59 row: four an hour, two from sixty.
        let at = now();
        let d = week_of(vec![(at - 8.0 * 86400.0, 40), (at, 95)], "Team");
        let all = tab(&d, 100, 40, &Config::default(), &palette()).join("\n");
        assert!(all.contains("FAIR USE"), "{all}");
        assert!(all.contains("~55 reviews in the last 7 days"), "{all}");
        assert!(all.contains("about 4 reviews an hour on Team"), "{all}");
        assert!(all.contains("2 an hour from 60"), "{all}");
        assert!(all.contains(" of 70"), "{all}");
        // And the plan it was looked up on heads the subscription.
        assert!(all.lines().any(|r| r.contains("SUBSCRIPTION") && r.contains("Team")), "{all}");
    }

    #[test]
    fn a_part_week_gives_a_floor_and_the_most_the_rate_can_be() {
        // Two days in, twelve reviews is at least twelve, and at most eight an hour.
        let at = now();
        let d = week_of(vec![(at - 2.0 * 86400.0, 83), (at, 95)], "Team");
        let all = tab(&d, 100, 40, &Config::default(), &palette()).join("\n");
        assert!(all.contains("at least 12 reviews in the 2d"), "{all}");
        assert!(all.contains("at most 8 reviews an hour on Team"), "{all}");
        assert!(all.contains("a full week on"), "{all}");
    }

    #[test]
    fn with_no_plan_or_one_reading_the_tab_says_what_it_cannot_give() {
        let at = now();
        let unknown = week_of(vec![(at - 8.0 * 86400.0, 40), (at, 95)], "");
        let all = tab(&unknown, 100, 40, &Config::default(), &palette()).join("\n");
        assert!(all.contains("~55 reviews") && all.contains("plan is not known, so no rate"), "{all}");
        let free = week_of(vec![(at - 8.0 * 86400.0, 40), (at, 95)], "Free");
        let all = tab(&free, 100, 40, &Config::default(), &palette()).join("\n");
        assert!(all.contains("no fair-use table for Free"), "{all}");
        let first = week_of(vec![(at, 95)], "Team");
        let all = tab(&first, 100, 40, &Config::default(), &palette()).join("\n");
        assert!(all.contains("a count needs a later one"), "{all}");
        // And no bar or rate from the zero one reading gives.
        assert!(!all.contains(" of 70") && !all.contains("an hour"), "{all}");
        // No readings at all, and the section is not drawn.
        let none = week_of(Vec::new(), "Team");
        assert!(!tab(&none, 100, 40, &Config::default(), &palette()).join("\n").contains("FAIR USE"));
    }

    #[test]
    fn the_fair_use_section_fits_every_width_and_loses_no_words() {
        // Wrapped, never clipped, from the narrowest pane to a wide one.
        let at = now();
        let d = week_of(vec![(at - 2.0 * 86400.0, 83), (at, 95)], "Team");
        let strip = |t: &str| {
            let mut out = String::new();
            let mut chars = t.chars();
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
        for w in [8usize, 12, 18, 24, 40, 60, 100] {
            let rows = fair_use_rows(&d, w, &palette());
            for r in &rows {
                assert!(tc::display_width(&strip(r)) <= w - 1, "width {w}: {r:?}");
            }
            let all: String = rows.iter().map(|r| strip(r)).collect::<String>().split_whitespace().collect();
            for said in ["atleast12", "at most8reviewsanhouronTeam", "maybehigherthanthis."] {
                let said: String = said.split_whitespace().collect();
                assert!(all.contains(&said), "width {w}: {said} missing from {all}");
            }
        }
    }

    #[test]
    fn the_reset_is_counted_in_days_from_today() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        assert_eq!(days_until("2026-09-30", today), Some(4));
        assert_eq!(days_until("2026-09-26", today), Some(0));
        assert_eq!(days_until("soon", today), None);
    }

    #[test]
    fn the_allowance_text_is_wrapped_whole_on_the_narrowest_pane() {
        // Down to the eight columns a pane may be, every word under the bar
        // is somewhere on screen: wrapped, never clipped by `seg`.
        let usage = parse_coderabbit_usage(
            "Repository : example-org/example-repo\nRemaining : 0 of 5\nWindow : rolling 1 hour\n\
             Capacity returns : in 20m\n",
        );
        let d = Data { usage, read_at: now(), ..Data::default() };
        let squeeze = |t: &str| {
            let mut out = String::new();
            let mut chars = t.chars();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    for c in chars.by_ref() {
                        if c == 'm' {
                            break;
                        }
                    }
                } else if !c.is_whitespace() {
                    out.push(c);
                }
            }
            out
        };
        for w in [8usize, 12, 18, 24, 40] {
            let all = squeeze(&tab(&d, w, 40, &Config::default(), &palette()).join(""));
            for said in ["example-org/example-repo", "pullrequestreviewsmaybelimitedsooner."] {
                assert!(all.contains(said), "width {w}: {said} missing from {all}");
            }
        }
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
        let d = Data { usage: Some(report()), read_at: now() - 120.0, why: String::new(), ..Data::default() };
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
