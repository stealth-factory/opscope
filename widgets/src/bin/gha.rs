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

//! GitHub Actions runs across the viewer's personal repos and every org
//! they belong to, grouped the way `deployments` groups Vercel teams.
//!
//! `github` counts PRs and `pr` rolls up one PR's checks. Neither says
//! what is queued rather than running, which workflow is failing
//! repeatedly, which job and step broke, or whether the pipeline is
//! getting slower. Those are all stamps and conclusions GitHub already
//! holds. This widget reads them and does not write.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use chrono::Utc;
use opscope_core as tc;

const GQL: &str = "https://api.github.com/graphql";
const REST: &str = "https://api.github.com";
const WINDOWS: &[i64] = &[12, 24, 48, 168];
const FILTERS: &[&str] = &["all", "failed", "running"];
/// How many recently-pushed repos to inspect per GraphQL page.
/// The cap is what is asked for runs; this is only how far one page looks.
const DISCOVER_EACH: usize = 40;
/// Pages per account. 200 most recently pushed, then the screen says so.
const DISCOVER_PAGES: usize = 5;
/// One page of runs per repo. GitHub's max; a repo with more in the
/// window says so rather than presenting the page as the 48h total.
const RUN_PAGE: usize = 100;
const DETAIL_TTL: f64 = 60.0;
const LIVE: &[&str] = &["in_progress", "queued", "waiting", "pending", "requested"];

/// The GitHub token, shared with `github` rather than duplicated.
fn token(gha: &serde_json::Value, gh: &serde_json::Value) -> (String, &'static str) {
    for cfg in [gha, gh] {
        let value = tc::cfg_str(cfg, "token", "");
        if !value.is_empty() {
            return (value, "config");
        }
    }
    let name = {
        let own = tc::cfg_str(gha, "token_env", "");
        if !own.is_empty() {
            own
        } else {
            tc::cfg_str(gh, "token_env", "GITHUB_TOKEN")
        }
    };
    let name = if name.is_empty() {
        "GITHUB_TOKEN".into()
    } else {
        name
    };
    match std::env::var(&name) {
        Ok(value) if !value.is_empty() => (value, "env"),
        _ => (String::new(), "missing"),
    }
}

fn graphql(
    query: &str,
    tok: &str,
    rate: &mut Option<(i64, i64)>,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "query": query }).to_string();
    let (out, headers) = tc::post_json(
        GQL,
        &[
            ("Authorization", &format!("Bearer {}", tok)),
            ("Content-Type", "application/json"),
            ("User-Agent", "opscope"),
        ],
        &body,
        45,
    )?;
    take_rate(rate, &headers);
    let data: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    graphql_payload(&data)
}

/// GraphQL answers 200 with `data` and `errors` together. The discover
/// query used to ask for both `organization` and `user` for one login;
/// the missing side is `NOT_FOUND`, and treating that as a failed page
/// dropped every org's repos. Use `data` when it is there. A null
/// payload is the real failure.
fn graphql_payload(body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let payload = body["data"].clone();
    if !payload.is_null() {
        return Ok(payload);
    }
    if let Some(first) = body["errors"].as_array().and_then(|a| a.first()) {
        return Err(first["message"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(100)
            .collect());
    }
    Err("empty graphql response".into())
}

fn rest_get(
    path: &str,
    tok: &str,
    rate: &mut Option<(i64, i64)>,
) -> Result<serde_json::Value, String> {
    let url = format!("{}{}", REST, path);
    let (body, headers) = tc::get_with_headers(
        &url,
        &[
            ("Authorization", &format!("Bearer {}", tok)),
            ("Accept", "application/vnd.github+json"),
            ("User-Agent", "opscope"),
            ("X-GitHub-Api-Version", "2022-11-28"),
        ],
        30,
    )?;
    take_rate(rate, &headers);
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

/// Remaining/limit from GitHub's rate-limit headers. GraphQL and REST
/// have separate buckets; the last response wins, and a full poll ends
/// on REST, which is what this widget spends.
fn take_rate(rate: &mut Option<(i64, i64)>, headers: &[(String, String)]) {
    if let Some(got) = rate_from_headers(headers) {
        *rate = Some(got);
    }
}

fn rate_from_headers(headers: &[(String, String)]) -> Option<(i64, i64)> {
    let mut remaining = None;
    let mut limit = None;
    for (name, value) in headers {
        match name.as_str() {
            "x-ratelimit-remaining" => remaining = value.parse().ok(),
            "x-ratelimit-limit" => limit = value.parse().ok(),
            _ => {}
        }
    }
    match (remaining, limit) {
        (Some(left), Some(max)) if max > 0 => Some((left, max)),
        _ => None,
    }
}

fn text(value: &serde_json::Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_string()
}

/// Seconds since the epoch for an RFC-3339 stamp, or nothing.
///
/// Nothing rather than zero: a missing stamp is not midnight 1970, and
/// treating it as one would invent a queue time of fifty years.
fn iso_secs(iso: &str) -> Option<f64> {
    let at = chrono::DateTime::parse_from_rfc3339(iso).ok()?;
    Some(at.timestamp() as f64 + f64::from(at.timestamp_subsec_nanos()) / 1_000_000_000.0)
}

/// How long a run sat queued before it started, or has been sitting queued.
///
/// `run_started_at - created_at` when GitHub recorded a start. A run that
/// is still queued and has no start stamp uses `now - created_at`, which
/// is elapsed time, not a guess. A finished run with no start stamp is
/// nothing — GitHub did not say, so this does not invent a number.
fn queue_secs(run: &serde_json::Value, at: f64) -> Option<f64> {
    let created = iso_secs(&text(run, "created_at"))?;
    if let Some(started) = iso_secs(&text(run, "run_started_at")) {
        return Some((started - created).max(0.0));
    }
    if LIVE.contains(&text(run, "status").as_str()) {
        return Some((at - created).max(0.0));
    }
    None
}

/// How long a run ran, or has been running.
///
/// Finished: `updated_at - run_started_at`. In progress with a start stamp:
/// `now - run_started_at`. Anything else is nothing — a queued run has not
/// started, and a finished run without a start stamp is not a duration of
/// zero.
fn run_secs(run: &serde_json::Value, at: f64) -> Option<f64> {
    let started = iso_secs(&text(run, "run_started_at"))?;
    if let Some(ended) = iso_secs(&text(run, "updated_at")) {
        if text(run, "status") == "completed" {
            return Some((ended - started).max(0.0));
        }
    }
    if text(run, "status") == "in_progress" {
        return Some((at - started).max(0.0));
    }
    None
}

fn dur_label(seconds: Option<f64>) -> String {
    let Some(s) = seconds else {
        return "  --  ".to_string();
    };
    if s < 60.0 {
        format!("{:>5.0}s", s)
    } else if s < 3600.0 {
        format!("{}m{:02}s", (s / 60.0) as i64, (s as i64) % 60)
    } else {
        format!("{}h{:02}m", (s / 3600.0) as i64, ((s as i64) % 3600) / 60)
    }
}

fn window_label(hours: i64) -> String {
    // 48h stays 48h, matching deployments' "last 48h" — collapsing it to
    // 2d made the default window look like a different thing.
    if hours >= 168 && hours % 24 == 0 {
        format!("{}d", hours / 24)
    } else {
        format!("{}h", hours)
    }
}

/// The word the row shows: status while it is live, conclusion once it is not.
fn outcome(run: &serde_json::Value) -> String {
    let status = text(run, "status");
    if status != "completed" && !status.is_empty() {
        return status;
    }
    let conclusion = text(run, "conclusion");
    if conclusion.is_empty() {
        status
    } else {
        conclusion
    }
}

fn outcome_label(kind: &str) -> &'static str {
    match kind {
        "in_progress" => "running",
        "queued" | "waiting" | "pending" | "requested" => "queued",
        "success" => "success",
        "failure" | "startup_failure" => "failure",
        "timed_out" => "timeout",
        "cancelled" => "cancel",
        "skipped" => "skip",
        "action_required" => "action",
        "neutral" => "neutral",
        "stale" => "stale",
        other if other.is_empty() => "—",
        _ => "done",
    }
}

fn is_failed(run: &serde_json::Value) -> bool {
    matches!(outcome(run).as_str(), "failure" | "startup_failure" | "timed_out")
}

fn is_running(run: &serde_json::Value) -> bool {
    LIVE.contains(&text(run, "status").as_str())
}

/// A repo GraphQL handed back, and whether it has workflow files.
#[derive(Debug, Clone, PartialEq)]
struct FoundRepo {
    name: String,
    pushed_at: String,
    has_workflows: bool,
}

/// The repositories connection from one account's GraphQL answer.
///
/// Discovery asks `repositoryOwner`, which is a user or an org. Tests and
/// the viewer query still arrive as `organization` / `user` / `viewer`.
fn repos_conn(data: &serde_json::Value) -> Option<&serde_json::Value> {
    for key in ["repositoryOwner", "organization", "user", "viewer"] {
        let conn = &data[key]["repositories"];
        if conn["nodes"].is_array() {
            return Some(conn);
        }
    }
    None
}

/// Repos from one account's GraphQL answer.
fn repos_from(data: &serde_json::Value) -> Vec<FoundRepo> {
    let Some(nodes) = repos_conn(data).and_then(|c| c["nodes"].as_array()) else {
        return Vec::new();
    };
    nodes
        .iter()
        .filter_map(|n| {
            let name = n["nameWithOwner"].as_str()?.to_string();
            if name.is_empty() || n["isEmpty"].as_bool().unwrap_or(false) {
                return None;
            }
            let entries = n["object"]["entries"].as_array();
            let has_workflows = entries.is_some_and(|e| !e.is_empty());
            Some(FoundRepo {
                name,
                pushed_at: n["pushedAt"].as_str().unwrap_or("").to_string(),
                has_workflows,
            })
        })
        .collect()
}

fn repo_owner(name: &str) -> String {
    name.split_once('/')
        .map(|(o, _)| o.to_string())
        .unwrap_or_default()
}

fn is_personal(owner: &str, viewer: &str) -> bool {
    !viewer.is_empty() && owner.eq_ignore_ascii_case(viewer)
}

/// How many eligible repos were kept, split so a cap on one org cannot
/// be drawn as the whole board.
#[derive(Debug, Default, Clone, PartialEq)]
struct PickStats {
    personal: usize,
    personal_found: usize,
    org: usize,
    org_found: usize,
}

/// The recently-pushed repos that actually have workflows, capped per owner.
///
/// A global cap lets one busy org eat every slot and hide personal repos.
/// The cap is applied inside each owner after the filter, personal first,
/// then orgs alphabetically. What comes back beside the list is how many
/// passed the filter in each scope — so a board that shows 16 of 40 in
/// one org can say so, rather than presenting 16 as the set.
fn pick_repos(
    found: &[FoundRepo],
    pushed_since: f64,
    cap: usize,
    viewer: &str,
) -> (Vec<String>, PickStats) {
    let mut groups: HashMap<String, Vec<&FoundRepo>> = HashMap::new();
    for repo in found {
        if !repo.has_workflows {
            continue;
        }
        match iso_secs(&repo.pushed_at) {
            Some(at) if at < pushed_since => continue,
            _ => {}
        }
        groups.entry(repo_owner(&repo.name)).or_default().push(repo);
    }
    for list in groups.values_mut() {
        list.sort_by(|a, b| b.pushed_at.cmp(&a.pushed_at));
    }
    let mut owners: Vec<String> = groups.keys().cloned().collect();
    owners.sort_by(|a, b| {
        match (is_personal(a, viewer), is_personal(b, viewer)) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()),
        }
    });
    let mut picked = Vec::new();
    let mut stats = PickStats::default();
    for owner in owners {
        let list = groups.get(&owner).map(|v| v.as_slice()).unwrap_or(&[]);
        let take: Vec<String> = list.iter().take(cap).map(|r| r.name.clone()).collect();
        if is_personal(&owner, viewer) {
            stats.personal = take.len();
            stats.personal_found = list.len();
        } else {
            stats.org += take.len();
            stats.org_found += list.len();
        }
        picked.extend(take);
    }
    (picked, stats)
}

fn sort_runs_by_scope(runs: &mut [serde_json::Value], viewer: &str) {
    runs.sort_by(|a, b| {
        let oa = repo_owner(&text(a, "repo"));
        let ob = repo_owner(&text(b, "repo"));
        match (is_personal(&oa, viewer), is_personal(&ob, viewer)) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => oa
                .to_ascii_lowercase()
                .cmp(&ob.to_ascii_lowercase())
                .then_with(|| text(b, "created_at").cmp(&text(a, "created_at"))),
        }
    });
}

fn scope_heading(owner: &str, viewer: &str) -> String {
    if is_personal(owner, viewer) {
        format!(" ── PERSONAL · {} ── ", owner)
    } else {
        format!(" ── {} ── ", owner.to_ascii_uppercase())
    }
}

/// How long ago the run was created. A missing stamp is `--`, not `0s`.
fn run_age(run: &serde_json::Value, at: f64) -> String {
    match iso_secs(&text(run, "created_at")) {
        Some(created) => tc::age((at - created).max(0.0)),
        None => "--".into(),
    }
}

/// Run durations oldest-first so a sparkline suffix is the recent end.
fn recent_run_secs(runs: &[serde_json::Value], at: f64) -> Vec<f64> {
    runs.iter().rev().filter_map(|r| run_secs(r, at)).collect()
}

fn after_arg(after: Option<&str>) -> String {
    match after {
        Some(c) => format!(", after: {}", serde_json::Value::String(c.to_string())),
        None => String::new(),
    }
}

fn discover_query(login: &str, after: Option<&str>) -> String {
    let q = serde_json::Value::String(login.to_string());
    let at = after_arg(after);
    format!(
        r#"{{
  repositoryOwner(login: {q}) {{
    repositories(first: {n}, ownerAffiliations: OWNER, orderBy: {{field: PUSHED_AT, direction: DESC}}, isArchived: false{at}) {{
      pageInfo {{ hasNextPage endCursor }}
      nodes {{
        nameWithOwner isEmpty isFork pushedAt
        object(expression: "HEAD:.github/workflows") {{ ... on Tree {{ entries {{ name }} }} }}
      }}
    }}
  }}
}}"#,
        q = q,
        n = DISCOVER_EACH,
        at = at
    )
}

fn viewer_repos_query(after: Option<&str>) -> String {
    let at = after_arg(after);
    format!(
        r#"{{
  viewer {{
    login
    repositories(first: {n}, ownerAffiliations: OWNER, orderBy: {{field: PUSHED_AT, direction: DESC}}, isArchived: false{at}) {{
      pageInfo {{ hasNextPage endCursor }}
      nodes {{
        nameWithOwner isEmpty isFork pushedAt
        object(expression: "HEAD:.github/workflows") {{ ... on Tree {{ entries {{ name }} }} }}
      }}
    }}
  }}
}}"#,
        n = DISCOVER_EACH,
        at = at
    )
}

/// One page of the orgs the viewer belongs to.
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

fn split_repo(name: &str) -> Option<(String, String)> {
    let (owner, repo) = name.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Flatten one REST workflow run into the fields the board reads.
fn ingest_run(raw: &serde_json::Value, repo: &str, repo_total: i64, fetched: usize) -> serde_json::Value {
    let repo = if repo.is_empty() {
        raw["repository"]["full_name"]
            .as_str()
            .unwrap_or("")
            .to_string()
    } else {
        repo.to_string()
    };
    serde_json::json!({
        "id": raw["id"].as_i64().unwrap_or(0),
        "repo": repo,
        "workflow": raw["name"].as_str().unwrap_or(""),
        "branch": raw["head_branch"].as_str().unwrap_or(""),
        "event": raw["event"].as_str().unwrap_or(""),
        "status": raw["status"].as_str().unwrap_or(""),
        "conclusion": raw["conclusion"].as_str().unwrap_or(""),
        "created_at": raw["created_at"].as_str().unwrap_or(""),
        "run_started_at": raw["run_started_at"].as_str().unwrap_or(""),
        "updated_at": raw["updated_at"].as_str().unwrap_or(""),
        "html_url": raw["html_url"].as_str().unwrap_or(""),
        "run_number": raw["run_number"].as_i64().unwrap_or(0),
        "head_sha": raw["head_sha"].as_str().unwrap_or(""),
        "display_title": raw["display_title"].as_str().unwrap_or(""),
        "attempt": raw["run_attempt"].as_i64().unwrap_or(1),
        "repo_total": repo_total,
        "repo_fetched": fetched as i64,
    })
}

/// The first step that failed on a job, if any.
fn failed_step(job: &serde_json::Value) -> Option<(i64, String)> {
    job["steps"].as_array()?.iter().find_map(|step| {
        if step["conclusion"].as_str() == Some("failure") {
            Some((
                step["number"].as_i64().unwrap_or(0),
                step["name"].as_str().unwrap_or("").to_string(),
            ))
        } else {
            None
        }
    })
}

/// How many times each workflow failed in the fetched set.
fn repeat_failures(runs: &[serde_json::Value]) -> Vec<((String, String), usize)> {
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    for run in runs {
        if is_failed(run) {
            let key = (text(run, "repo"), text(run, "workflow"));
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    let mut out: Vec<_> = counts.into_iter().filter(|(_, n)| *n >= 2).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

fn matches_needle(run: &serde_json::Value, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let n = needle.to_ascii_lowercase();
    [
        text(run, "repo"),
        text(run, "workflow"),
        text(run, "branch"),
        text(run, "event"),
        outcome(run),
        text(run, "display_title"),
    ]
    .iter()
    .any(|s| s.to_ascii_lowercase().contains(&n))
}

fn counted(picked: usize, found: usize, noun: &str) -> String {
    if found > picked {
        format!("{} of {} {}", picked, found, noun)
    } else {
        format!("{} {}", picked, noun)
    }
}

/// What the board is looking at, when that is not already on the activity
/// line. The window itself is `runs/hour, last 48h`, like deployments.
/// This sentence is only the cap, because a cap that is not named is a total.
fn scope_sentence(
    explicit: bool,
    repos: usize,
    stats: &PickStats,
    orgs: usize,
    _window_hours: i64,
    pushed_days: i64,
) -> String {
    if explicit {
        return format!(
            "{} configured repo{}",
            repos,
            if repos == 1 { "" } else { "s" }
        );
    }
    let cut = stats.personal_found > stats.personal || stats.org_found > stats.org;
    let none = stats.personal + stats.org == 0;
    if !cut && !none {
        return String::new();
    }
    let orgs_said = match orgs {
        0 => "no orgs".to_string(),
        1 => "1 org".to_string(),
        n => format!("{} orgs", n),
    };
    format!(
        "{} · {} ({}, pushed in {}d)",
        counted(stats.personal, stats.personal_found, "personal"),
        counted(stats.org, stats.org_found, "org"),
        orgs_said,
        pushed_days
    )
}

/// Progressive disclosure: extra width buys extra columns, never padding.
///
/// `single` is the deployments shape: wide enough that the commit title
/// fits on the same row as the metadata, so the list is one line per run.
struct Columns {
    single: bool,
    detail: bool,
    event: bool,
    queued: bool,
    repo: usize,
    workflow: usize,
}

fn columns(w: usize) -> Columns {
    Columns {
        detail: w >= 72,
        single: w >= 110,
        event: w >= 100,
        queued: w >= 114,
        repo: if w < 70 {
            10
        } else if w < 100 {
            14
        } else {
            18
        },
        workflow: if w < 80 {
            10
        } else if w < 110 {
            14
        } else {
            18
        },
    }
}

struct Palette {
    ok: String,
    run: String,
    fail: String,
    fail_lit: String,
    queue: String,
    cancel: String,
    dim: String,
    dim_lit: String,
    grid: String,
    msg: String,
    hint: String,
    txt: String,
    lbl: String,
    accent: String,
    branch: String,
    sha: String,
}

fn palette() -> Palette {
    Palette {
        ok: tc::rgb(80, 235, 150),
        run: tc::rgb(255, 200, 90),
        fail: tc::rgb(255, 95, 105),
        fail_lit: tc::rgb(255, 128, 136),
        queue: tc::rgb(150, 190, 255),
        cancel: tc::rgb(170, 175, 190),
        // 127,147,172 measures 3.81 on the selected-row tint; the lighter
        // twin is what the tint closure reaches for.
        dim: tc::rgb(127, 147, 172),
        dim_lit: tc::rgb(140, 170, 195),
        grid: tc::rgb(71, 91, 116),
        msg: tc::rgb(158, 174, 196),
        hint: tc::rgb(126, 148, 173),
        txt: tc::rgb(225, 235, 245),
        lbl: tc::rgb(130, 165, 200),
        accent: tc::rgb(150, 210, 255),
        branch: tc::rgb(150, 210, 255),
        sha: tc::rgb(190, 170, 255),
    }
}

/// Runs per time bucket, coloured by the worst outcome in it — the same
/// chart `deployments` draws for deploys/hour.
fn activity(
    runs: &[serde_json::Value],
    w: usize,
    hours: f64,
    p: &Palette,
) -> (String, usize) {
    let cols = w.saturating_sub(2).max(10);
    let at = tc::now();
    let span = hours * 3_600.0;
    let mut buckets: Vec<Vec<&serde_json::Value>> = vec![Vec::new(); cols];
    for run in runs {
        let Some(created) = iso_secs(&text(run, "created_at")) else {
            continue;
        };
        let off = at - created;
        if (0.0..span).contains(&off) {
            let slot = cols - 1 - (off / span * cols as f64) as usize;
            buckets[slot.min(cols - 1)].push(run);
        }
    }
    let peak = buckets.iter().map(|b| b.len()).max().unwrap_or(0);
    if peak == 0 {
        return (
            tc::seg(
                &[(
                    p.dim.as_str(),
                    format!(" no runs in the last {}h", hours as i64),
                )],
                w - 1,
            ),
            0,
        );
    }
    let mut parts: Vec<(&str, String)> = vec![(p.dim.as_str(), " ".into())];
    for bucket in &buckets {
        if bucket.is_empty() {
            parts.push((p.grid.as_str(), "·".into()));
            continue;
        }
        let colour = if bucket.iter().any(|r| is_failed(r)) {
            &p.fail
        } else if bucket.iter().any(|r| text(r, "status") == "in_progress") {
            &p.run
        } else if bucket.iter().any(|r| is_running(r)) {
            &p.queue
        } else {
            &p.ok
        };
        let level = ((bucket.len() as f64 / peak as f64) * 7.99) as usize;
        parts.push((colour.as_str(), tc::SPARK[level.min(7)].to_string()));
    }
    (tc::seg(&parts, w - 1), peak)
}

fn outcome_colour<'a>(kind: &str, p: &'a Palette) -> &'a str {
    match kind {
        "in_progress" => &p.run,
        "queued" | "waiting" | "pending" | "requested" => &p.queue,
        "success" => &p.ok,
        "failure" | "startup_failure" | "timed_out" => &p.fail,
        "cancelled" | "skipped" | "stale" | "neutral" => &p.cancel,
        _ => &p.dim,
    }
}

#[derive(Default)]
struct State {
    runs: Vec<serde_json::Value>,
    err: String,
    fetched: f64,
    scope: String,
    window_hours: i64,
    viewer: String,
    accounts: usize,
    rate: Option<(i64, i64)>,
}

/// Fold a finished pass into the live state.
///
/// A result for a different window is dropped: `w` changes the label
/// immediately, and keeping the old runs would put one window's numbers
/// under another's name. The same-window empty-plus-error case still
/// keeps whatever this window already showed, so a one-repo failure
/// does not blank a board that had data.
fn apply_pass(live: &mut State, got: State) {
    if got.window_hours != live.window_hours {
        return;
    }
    if !got.runs.is_empty() || got.err.is_empty() || live.runs.is_empty() {
        live.runs = got.runs;
        live.fetched = got.fetched;
        live.scope = got.scope;
        live.viewer = got.viewer;
        live.accounts = got.accounts;
    }
    if got.rate.is_some() {
        live.rate = got.rate;
    }
    live.err = got.err;
}

/// Ask for a new window and drop figures that belonged to the old one.
fn request_window(live: &mut State, hours: i64) {
    if live.window_hours == hours {
        return;
    }
    live.window_hours = hours;
    live.runs.clear();
    live.scope.clear();
    live.fetched = 0.0;
    live.err.clear();
}

fn created_filter(hours: i64) -> String {
    let start = Utc::now() - chrono::Duration::hours(hours);
    format!(">={}", start.format("%Y-%m-%dT%H:%M:%SZ"))
}

fn fetch_runs_for(
    repo: &str,
    tok: &str,
    hours: i64,
    rate: &mut Option<(i64, i64)>,
) -> Result<(Vec<serde_json::Value>, Option<String>), String> {
    let (owner, name) = split_repo(repo).ok_or_else(|| format!("not owner/name: {}", repo))?;
    let created = created_filter(hours);
    let path = format!(
        "/repos/{}/{}/actions/runs?per_page={}&created={}",
        owner,
        name,
        RUN_PAGE,
        urlencoding_lite(&created)
    );
    let res = rest_get(&path, tok, rate)?;
    let total = res["total_count"].as_i64().unwrap_or(0);
    let raw = res["workflow_runs"].as_array().cloned().unwrap_or_default();
    let fetched = raw.len();
    let mut out = Vec::new();
    for run in &raw {
        out.push(ingest_run(run, repo, total, fetched));
    }
    let note = if total > fetched as i64 {
        Some(format!(
            "{}: {} most recent of {} runs in the window",
            repo, fetched, total
        ))
    } else {
        None
    };
    Ok((out, note))
}

/// Enough of a query-string escape that an ISO stamp survives the URL.
fn urlencoding_lite(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn discover_accounts(tok: &str, rate: &mut Option<(i64, i64)>) -> Result<(String, Vec<String>), String> {
    let mut viewer = String::new();
    let mut accounts = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let data = graphql(&orgs_query(cursor.as_deref()), tok, rate)?;
        if viewer.is_empty() {
            viewer = data["viewer"]["login"].as_str().unwrap_or("").to_string();
            if viewer.is_empty() {
                return Err("no viewer login in the response".into());
            }
        }
        accounts.extend(
            data["viewer"]["organizations"]["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| o["login"].as_str().map(String::from))
                .filter(|s| !s.is_empty()),
        );
        let conn = &data["viewer"]["organizations"];
        let next = conn["pageInfo"]["endCursor"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if !conn["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false)
            || next.is_empty()
            || Some(&next) == cursor.as_ref()
        {
            break;
        }
        cursor = Some(next);
    }
    if !viewer.is_empty() && !accounts.iter().any(|a| a == &viewer) {
        accounts.insert(0, viewer.clone());
    }
    Ok((viewer, accounts))
}

/// One account's recently-pushed repos, paged until they fall outside
/// `since` or `DISCOVER_PAGES` is spent. A look that stops with more
/// pages left is named so the eligible count is never drawn as complete.
fn discover_account_pages(
    tok: &str,
    acc: &str,
    since: f64,
    rate: &mut Option<(i64, i64)>,
) -> Result<(Vec<FoundRepo>, Option<String>), String> {
    let mut found = Vec::new();
    let mut cursor: Option<String> = None;
    let mut note = None;
    for page in 0..DISCOVER_PAGES {
        let query = if acc == "@me" {
            viewer_repos_query(cursor.as_deref())
        } else {
            discover_query(acc, cursor.as_deref())
        };
        let data = match graphql(&query, tok, rate) {
            Ok(d) => d,
            Err(said) => {
                return Ok((found, Some(format!("{}: {}", acc, said))));
            }
        };
        let batch = repos_from(&data);
        let past = batch.iter().any(|r| {
            iso_secs(&r.pushed_at).is_some_and(|at| at < since)
        });
        found.extend(batch);
        if past {
            break;
        }
        let Some(conn) = repos_conn(&data) else {
            break;
        };
        let has_next = conn["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
        let next = conn["pageInfo"]["endCursor"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if !has_next || next.is_empty() || Some(&next) == cursor.as_ref() {
            break;
        }
        if page + 1 == DISCOVER_PAGES {
            note = Some(format!(
                "{}: looked at the {} most recently pushed",
                acc,
                DISCOVER_PAGES * DISCOVER_EACH
            ));
            break;
        }
        cursor = Some(next);
    }
    Ok((found, note))
}

fn discover_repos(
    tok: &str,
    accounts: &[String],
    pushed_days: i64,
    cap: usize,
    viewer: &str,
    rate: &mut Option<(i64, i64)>,
) -> Result<(Vec<String>, PickStats, Option<String>), String> {
    let since = tc::now() - (pushed_days as f64 * 86400.0);
    let mut found: Vec<FoundRepo> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    for acc in accounts {
        match discover_account_pages(tok, acc, since, rate) {
            Ok((page, note)) => {
                found.extend(page);
                if let Some(said) = note {
                    notes.push(said);
                }
            }
            Err(said) => notes.push(format!("{}: {}", acc, said)),
        }
    }
    // Same-name rows have to be adjacent before dedup_by can collapse them.
    // Newest push wins, then the per-owner cap in pick_repos.
    found.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| b.pushed_at.cmp(&a.pushed_at))
    });
    found.dedup_by(|a, b| a.name == b.name);
    let (picked, stats) = pick_repos(&found, since, cap, viewer);
    let note = if notes.is_empty() {
        None
    } else {
        Some(notes.join(" · "))
    };
    Ok((picked, stats, note))
}

fn viewer_login(tok: &str, rate: &mut Option<(i64, i64)>) -> Result<String, String> {
    let data = graphql("{ viewer { login } }", tok, rate)?;
    let login = data["viewer"]["login"].as_str().unwrap_or("").to_string();
    if login.is_empty() {
        Err("no viewer login in the response".into())
    } else {
        Ok(login)
    }
}

fn one_pass(
    tok: &str,
    source: &str,
    accounts: &[String],
    explicit: &[String],
    pushed_days: i64,
    cap: usize,
    hours: i64,
) -> Result<State, String> {
    let mut err = if source == "config" {
        tc::config_token_warning().unwrap_or_default()
    } else {
        String::new()
    };
    let mut rate = None;
    let empty_stats = PickStats::default();
    let (repos, stats, viewer, orgs, n_accounts, is_explicit) = if !explicit.is_empty() {
        let viewer = viewer_login(tok, &mut rate).unwrap_or_default();
        let n = explicit
            .iter()
            .map(|r| repo_owner(r))
            .filter(|o| !o.is_empty())
            .collect::<HashSet<_>>()
            .len();
        (explicit.to_vec(), empty_stats, viewer, 0usize, n, true)
    } else {
        let (viewer, want) = if accounts.is_empty() {
            match discover_accounts(tok, &mut rate) {
                Ok(got) => got,
                Err(said) => {
                    return Ok(State {
                        err: format!("could not list accounts: {}", said),
                        rate,
                        window_hours: hours,
                        ..Default::default()
                    });
                }
            }
        } else {
            let viewer = viewer_login(tok, &mut rate).unwrap_or_default();
            (viewer, accounts.to_vec())
        };
        if want.is_empty() {
            return Ok(State {
                err: "no accounts: leave gha.accounts empty to discover your login and orgs, or name them".into(),
                viewer,
                rate,
                window_hours: hours,
                ..Default::default()
            });
        }
        let n_accounts = want.len();
        let orgs = want
            .iter()
            .filter(|a| !is_personal(a, &viewer))
            .count();
        match discover_repos(tok, &want, pushed_days, cap, &viewer, &mut rate) {
            Ok((repos, stats, note)) => {
                if let Some(said) = note {
                    err = if err.is_empty() {
                        said
                    } else {
                        format!("{} · {}", err, said)
                    };
                }
                (repos, stats, viewer, orgs, n_accounts, false)
            }
            Err(said) => {
                return Ok(State {
                    err: format!("could not list repos: {}", said),
                    viewer,
                    accounts: n_accounts,
                    rate,
                    window_hours: hours,
                    ..Default::default()
                });
            }
        }
    };

    if repos.is_empty() {
        let said = if is_explicit {
            "no repos: gha.repos is empty and no owner/repo was given".into()
        } else {
            format!(
                "no repos with workflows among those pushed in the last {}d — set gha.repos to name them",
                pushed_days
            )
        };
        return Ok(State {
            err: if err.is_empty() {
                said
            } else {
                format!("{} · {}", err, said)
            },
            scope: scope_sentence(is_explicit, 0, &stats, orgs, hours, pushed_days),
            viewer,
            accounts: n_accounts,
            rate,
            window_hours: hours,
            ..Default::default()
        });
    }

    let mut runs = Vec::new();
    let mut partial: Vec<String> = Vec::new();
    for repo in &repos {
        match fetch_runs_for(repo, tok, hours, &mut rate) {
            Ok((got, note)) => {
                runs.extend(got);
                if let Some(said) = note {
                    partial.push(said);
                }
            }
            Err(said) => {
                err = if err.is_empty() {
                    format!("{}: {}", repo, said)
                } else {
                    format!("{} · {}: {}", err, repo, said)
                };
            }
        }
    }
    runs.sort_by(|a, b| text(b, "created_at").cmp(&text(a, "created_at")));
    if !partial.is_empty() {
        let extra = if partial.len() == 1 {
            partial[0].clone()
        } else {
            format!(
                "{} repos show only their {} most recent runs in the window",
                partial.len(),
                RUN_PAGE
            )
        };
        err = if err.is_empty() {
            extra
        } else {
            format!("{} · {}", err, extra)
        };
    }
    Ok(State {
        runs,
        err,
        fetched: tc::now(),
        scope: scope_sentence(
            is_explicit,
            repos.len(),
            &stats,
            orgs,
            hours,
            pushed_days,
        ),
        viewer,
        accounts: n_accounts,
        rate,
        window_hours: hours,
    })
}

fn fetch_jobs(run: &serde_json::Value, tok: &str) -> serde_json::Value {
    let repo = text(run, "repo");
    let id = run["id"].as_i64().unwrap_or(0);
    let Some((owner, name)) = split_repo(&repo) else {
        return serde_json::json!({ "_error": "run has no owner/name" });
    };
    if id == 0 {
        return serde_json::json!({ "_error": "run has no id" });
    }
    let path = format!("/repos/{}/{}/actions/runs/{}/jobs?per_page=100", owner, name, id);
    match rest_get(&path, tok, &mut None) {
        Ok(mut v) => {
            v["_fetched_at"] = serde_json::json!(tc::now());
            v
        }
        Err(e) => serde_json::json!({ "_error": e, "_fetched_at": tc::now() }),
    }
}

fn counts(runs: &[serde_json::Value]) -> (usize, usize, usize, usize) {
    let mut running = 0;
    let mut queued = 0;
    let mut failed = 0;
    let mut ok = 0;
    for run in runs {
        match outcome(run).as_str() {
            "in_progress" => running += 1,
            "queued" | "waiting" | "pending" | "requested" => queued += 1,
            "failure" | "startup_failure" | "timed_out" => failed += 1,
            "success" => ok += 1,
            _ => {}
        }
    }
    (running, queued, failed, ok)
}

fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted.get(i).copied()
}

fn sparkline(values: &[f64], width: usize) -> String {
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let take = values.len().min(width);
    let slice = &values[values.len() - take..];
    let hi = slice.iter().cloned().fold(0.0f64, f64::max).max(1e-9);
    slice
        .iter()
        .map(|x| tc::SPARK[(((x / hi) * 7.99) as usize).min(7)])
        .collect()
}

fn copy_items(run: &serde_json::Value) -> Vec<(String, String)> {
    let mut items = Vec::new();
    let mut push = |label: &str, value: String| {
        if !value.is_empty() {
            items.push((label.to_string(), value));
        }
    };
    push("Run URL", text(run, "html_url"));
    push("Commit SHA", text(run, "head_sha"));
    push("Branch", text(run, "branch"));
    items
}

fn info_overlay(
    run: &serde_json::Value,
    jobs: Option<&serde_json::Value>,
    w: usize,
    repeats: usize,
    note: &str,
    p: &Palette,
) -> Vec<String> {
    let mut rows = vec![tc::title(
        &format!(
            "{} / {}",
            text(run, "repo"),
            text(run, "workflow")
        ),
        w,
        &p.accent,
    )];
    let kind = outcome(run);
    let colour = outcome_colour(&kind, p);
    rows.push(tc::seg(
        &[
            (p.dim.as_str(), "  #".into()),
            (p.txt.as_str(), run["run_number"].as_i64().unwrap_or(0).to_string()),
            (p.dim.as_str(), "  ".into()),
            (colour, outcome_label(&kind).to_string()),
            (p.dim.as_str(), format!("  {}", text(run, "event"))),
            (
                p.dim.as_str(),
                if run["attempt"].as_i64().unwrap_or(1) > 1 {
                    format!("  attempt {}", run["attempt"].as_i64().unwrap_or(1))
                } else {
                    String::new()
                },
            ),
        ],
        w - 1,
    ));
    rows.push(tc::seg(
        &[
            (p.dim.as_str(), "  ".into()),
            (p.branch.as_str(), text(run, "branch")),
            (p.dim.as_str(), "  queued ".into()),
            (p.txt.as_str(), dur_label(queue_secs(run, tc::now()))),
            (p.dim.as_str(), "  ran ".into()),
            (p.txt.as_str(), dur_label(run_secs(run, tc::now()))),
        ],
        w - 1,
    ));
    let sha = text(run, "head_sha");
    let title = text(run, "display_title");
    if !sha.is_empty() || !title.is_empty() {
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!("  {}", &sha[..sha.len().min(7)])),
                (p.txt.as_str(), if title.is_empty() { String::new() } else { format!("  {}", title) }),
            ],
            w - 1,
        ));
    }
    if repeats > 1 {
        rows.push(tc::seg(
            &[
                (p.fail.as_str(), format!("  failed {} times in this window", repeats)),
            ],
            w - 1,
        ));
    }
    rows.push(String::new());
    rows.push(tc::seg(&[(p.lbl.as_str(), " ── JOBS ──".into())], w - 1));
    match jobs {
        None => {
            rows.push(tc::seg(&[(p.dim.as_str(), "  loading jobs…".into())], w - 1));
        }
        Some(j) if j.get("_error").is_some() => {
            rows.push(tc::seg(
                &[(p.fail.as_str(), format!("  {}", text(j, "_error")))],
                w - 1,
            ));
        }
        Some(j) => {
            let list = j["jobs"].as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                    rows.push(tc::seg(
                        &[(p.dim.as_str(), "  GitHub returned no jobs for this run".into())],
                        w - 1,
                    ));
            } else {
                    for job in &list {
                        let status = job["status"].as_str().unwrap_or("");
                        let conclusion = job["conclusion"].as_str().unwrap_or("");
                        let kind = if status != "completed" && !status.is_empty() {
                            status
                        } else {
                            conclusion
                        };
                        let mark = if LIVE.contains(&status) {
                            '●'
                        } else if matches!(kind, "failure" | "startup_failure" | "timed_out") {
                            '✖'
                        } else if kind == "success" {
                            '●'
                        } else {
                            '○'
                        };
                        let started = iso_secs(job["started_at"].as_str().unwrap_or(""));
                        let ended = iso_secs(job["completed_at"].as_str().unwrap_or(""));
                        let took = match (started, ended) {
                            (Some(a), Some(b)) => Some((b - a).max(0.0)),
                            (Some(a), None) if LIVE.contains(&status) => Some((tc::now() - a).max(0.0)),
                            _ => None,
                        };
                        rows.push(tc::seg(
                            &[
                                (outcome_colour(kind, p), format!("  {} {}", mark, job["name"].as_str().unwrap_or(""))),
                                (p.dim.as_str(), format!("  {}", dur_label(took))),
                            ],
                            w - 1,
                        ));
                        if let Some((n, step)) = failed_step(job) {
                            rows.push(tc::seg(
                                &[
                                    (p.fail.as_str(), format!("     step {} · {}", n, step)),
                                ],
                                w - 1,
                            ));
                        }
                    }
            }
        }
    }
    if !note.is_empty() {
        rows.push(String::new());
        rows.push(tc::seg(&[(p.ok.as_str(), format!("  {}", note))], w - 1));
    }
    rows
}

fn main() {
    tc::maybe_help(include_str!("gha_help.txt"));
    let cfg = tc::load_config("gha");
    let gh = tc::load_config("github");
    let mut refresh = tc::poll_secs(tc::cfg_f64(&cfg, "refresh", 60.0), 60.0).max(30.0);
    let accounts = tc::cfg_strings(&cfg, "accounts", &[]);
    let configured = tc::cfg_strings(&cfg, "repos", &[]);
    let start_window = (tc::cfg_f64(&cfg, "window_hours", 48.0) as i64).max(1);
    let max_repos = tc::cfg_usize(&cfg, "max_repos", 16).max(1);
    let pushed_days = (tc::cfg_f64(&cfg, "pushed_days", 14.0) as i64).max(1);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut named: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--refresh" if i + 1 < args.len() => {
                refresh = tc::poll_secs(args[i + 1].parse().unwrap_or(60.0), 60.0).max(30.0);
                i += 2;
            }
            other if !other.starts_with('-') => {
                named.push(other.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }
    let explicit: Vec<String> = if named.is_empty() { configured } else { named };

    let absent = tc::missing(&["curl"]);
    if !absent.is_empty() {
        tc::cannot_start(
            "github actions",
            &absent,
            &[
                "Everything here comes from GitHub's API, and curl is how",
                "this reaches it - the same way github and pr do.",
                "",
                "The token is passed to curl on its standard input rather than",
                "in its arguments, because /proc/<pid>/cmdline is readable by",
                "every user on the machine.",
            ],
            "apt install curl",
        );
        return;
    }

    let p = palette();
    let (tok, source) = token(&cfg, &gh);
    let env_name = {
        let own = tc::cfg_str(&cfg, "token_env", "");
        if !own.is_empty() {
            own
        } else {
            let name = tc::cfg_str(&gh, "token_env", "GITHUB_TOKEN");
            if name.is_empty() {
                "GITHUB_TOKEN".into()
            } else {
                name
            }
        }
    };

    let state = Arc::new(Mutex::new(State {
        window_hours: start_window,
        ..Default::default()
    }));
    let wake = Arc::new((Mutex::new(false), Condvar::new()));
    let poller = Arc::clone(&state);
    let poller_wake = Arc::clone(&wake);
    let poll_tok = tok.clone();
    let poll_accounts = accounts.clone();
    let poll_explicit = explicit.clone();
    let poll_env = env_name.clone();
    std::thread::spawn(move || loop {
        let hours = poller.lock().map(|g| g.window_hours).unwrap_or(start_window);
        if poll_tok.is_empty() {
            if let Ok(mut g) = poller.lock() {
                g.err = format!(
                    "no token: set github.token in config.json or ${}",
                    poll_env
                );
            }
        } else {
            let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                one_pass(
                    &poll_tok,
                    source,
                    &poll_accounts,
                    &poll_explicit,
                    pushed_days,
                    max_repos,
                    hours,
                )
            }));
            match step {
                Ok(Ok(got)) => {
                    if let Ok(mut g) = poller.lock() {
                        apply_pass(&mut g, got);
                    }
                }
                Ok(Err(said)) => {
                    if let Ok(mut g) = poller.lock() {
                        g.err = said;
                    }
                }
                Err(_) => {
                    if let Ok(mut g) = poller.lock() {
                        g.err = "poller stopped - see the pane it was started from".into();
                    }
                    return;
                }
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
    });

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let details: Arc<Mutex<HashMap<i64, serde_json::Value>>> = Arc::new(Mutex::new(HashMap::new()));
    let fetching: Arc<Mutex<HashSet<i64>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut filter = 0usize;
    let (mut needle, mut typing) = (String::new(), false);
    let mut overlay = false;
    let mut overlay_id: i64 = 0;
    let (mut tick, mut selected, mut scroll) = (0usize, 0usize, 0usize);
    let mut oscroll = 0usize;
    let mut note: (String, f64) = (String::new(), 0.0);
    let mut visible = 1usize;
    let mut shown: Vec<serde_json::Value> = Vec::new();

    loop {
        tick += 1;
        for key in keyboard.poll() {
            if typing && !overlay {
                match key.as_str() {
                    "esc" => {
                        needle.clear();
                        typing = false;
                    }
                    "enter" => typing = false,
                    "backspace" => {
                        needle.pop();
                    }
                    other if other.chars().count() == 1 => needle.push_str(other),
                    _ => {}
                }
                selected = 0;
                continue;
            }
            if overlay {
                match key.as_str() {
                    "left" | "esc" => {
                        overlay = false;
                        overlay_id = 0;
                    },
                    "up" | "k" | "K" => oscroll = oscroll.saturating_sub(1),
                    "down" | "j" | "J" => oscroll = oscroll.saturating_add(1),
                    "pgup" => {
                        let page = tc::size().1.saturating_sub(3).max(1);
                        oscroll = oscroll.saturating_sub(page);
                    }
                    "pgdn" => {
                        let page = tc::size().1.saturating_sub(3).max(1);
                        oscroll = oscroll.saturating_add(page);
                    }
                    "home" => oscroll = 0,
                    "end" => oscroll = usize::MAX,
                    "r" | "R" => {
                        if let Some(chosen) = shown.get(selected.min(shown.len().saturating_sub(1))) {
                            let id = chosen["id"].as_i64().unwrap_or(0);
                            if let Ok(mut g) = details.lock() {
                                g.remove(&id);
                            }
                        }
                    }
                    "c" | "C" => {
                        if let Some(chosen) = shown.get(selected.min(shown.len().saturating_sub(1))) {
                            let items = copy_items(chosen);
                            if let Some((_, value)) = items.first() {
                                note = (
                                    if tc::clipboard(value) {
                                        "✓ copied run URL".into()
                                    } else {
                                        "! no clipboard; select the URL with the mouse".into()
                                    },
                                    tc::now() + 3.0,
                                );
                            }
                        }
                    }
                    "q" | "Q" => {
                        keyboard.restore();
                        tc::restore_screen();
                        return;
                    }
                    _ => {}
                }
                continue;
            }
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "r" | "R" => {
                    let (lock, cond) = &*wake;
                    if let Ok(mut asked) = lock.lock() {
                        *asked = true;
                        cond.notify_all();
                    }
                }
                "s" | "S" => {
                    filter = (filter + 1) % FILTERS.len();
                    selected = 0;
                }
                "w" | "W" => {
                    if let Ok(mut g) = state.lock() {
                        let next = tc::cycle(WINDOWS, g.window_hours);
                        request_window(&mut g, next);
                    }
                    let (lock, cond) = &*wake;
                    if let Ok(mut asked) = lock.lock() {
                        *asked = true;
                        cond.notify_all();
                    }
                }
                "/" => typing = true,
                "up" => selected = selected.saturating_sub(1),
                "down" => selected += 1,
                "j" | "J" => selected += 1,
                "k" | "K" => selected = selected.saturating_sub(1),
                "pgup" => selected = selected.saturating_sub(visible),
                "pgdn" => selected += visible,
                "home" => selected = 0,
                "end" => selected = shown.len().saturating_sub(1),
                "right" | "enter" => {
                    if !shown.is_empty() {
                        overlay = true;
                        oscroll = 0;
                        note = (String::new(), 0.0);
                        overlay_id = shown
                            .get(selected)
                            .and_then(|r| r["id"].as_i64())
                            .unwrap_or(0);
                    }
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let (runs, err, fetched, scope, hours, viewer, n_accounts, rate) = match state.lock() {
            Ok(g) => (
                g.runs.clone(),
                g.err.clone(),
                g.fetched,
                g.scope.clone(),
                g.window_hours,
                g.viewer.clone(),
                g.accounts,
                g.rate,
            ),
            Err(_) => return,
        };
        if !note.0.is_empty() && tc::now() > note.1 {
            note = (String::new(), 0.0);
        }
        shown = runs
            .iter()
            .filter(|r| matches_needle(r, &needle))
            .cloned()
            .collect();
        match FILTERS[filter] {
            "failed" => shown.retain(is_failed),
            "running" => shown.retain(is_running),
            _ => {}
        }
        sort_runs_by_scope(&mut shown, &viewer);
        if overlay {
            if shown.is_empty() {
                overlay = false;
                overlay_id = 0;
            } else if overlay_id != 0 {
                if let Some(i) = shown
                    .iter()
                    .position(|r| r["id"].as_i64() == Some(overlay_id))
                {
                    selected = i;
                } else {
                    overlay = false;
                    overlay_id = 0;
                }
            }
        }
        if !shown.is_empty() && selected >= shown.len() {
            selected = shown.len() - 1;
        }

        if overlay && !shown.is_empty() {
            let chosen = shown[selected].clone();
            let id = chosen["id"].as_i64().unwrap_or(0);
            let mut held = details.lock().ok().and_then(|g| g.get(&id).cloned());
            if held
                .as_ref()
                .map(|v| tc::now() - v["_fetched_at"].as_f64().unwrap_or(0.0) > DETAIL_TTL)
                .unwrap_or(false)
            {
                if let Ok(mut g) = details.lock() {
                    g.remove(&id);
                }
                held = None;
            }
            if held.is_none() && id != 0 {
                let start = fetching
                    .lock()
                    .map(|mut g| g.insert(id))
                    .unwrap_or(false);
                if start {
                    let (details, fetching) = (Arc::clone(&details), Arc::clone(&fetching));
                    let (chosen, tok) = (chosen.clone(), tok.clone());
                    std::thread::spawn(move || {
                        let got = fetch_jobs(&chosen, &tok);
                        if let Ok(mut g) = details.lock() {
                            g.insert(id, got);
                        }
                        if let Ok(mut g) = fetching.lock() {
                            g.remove(&id);
                        }
                    });
                }
            }
            let repeats = runs
                .iter()
                .filter(|r| {
                    text(r, "repo") == text(&chosen, "repo")
                        && text(r, "workflow") == text(&chosen, "workflow")
                        && is_failed(r)
                })
                .count();
            let body = info_overlay(&chosen, held.as_ref(), w, repeats, &note.0, &p);
            let foot = 2;
            let room = h.saturating_sub(foot).max(1);
            let furthest = body.len().saturating_sub(room);
            oscroll = oscroll.min(furthest);
            let last = (oscroll + room).min(body.len());
            let mut out: Vec<String> = body[oscroll..last].to_vec();
            while out.len() < room {
                out.push(String::new());
            }
            out.push(tc::seg(
                &[(
                    p.hint.as_str(),
                    if furthest > 0 {
                        format!(
                            " ↑↓ scroll {}-{} of {} · [c]opy · [r]efresh · ← esc · [q]uit",
                            oscroll + 1,
                            last,
                            body.len()
                        )
                    } else {
                        " [c]opy · [r]efresh · ← or esc to close · [q]uit".to_string()
                    },
                )],
                w - 1,
            ));
            tc::draw(&out, w, h);
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }

        let mut rows = vec![tc::title("github actions", w, &p.accent)];
        let mut meta = vec![(
            p.dim.as_str(),
            format!(
                " {} account{}",
                n_accounts,
                if n_accounts == 1 { "" } else { "s" }
            ),
        )];
        let tail = tc::polled(fetched, rate, &p.dim, &p.ok, &p.run);
        for (colour, txt) in &tail {
            meta.push((colour.as_str(), txt.clone()));
        }
        rows.push(tc::seg(&meta, w - 1));
        let (running, queued, failed, ok) = counts(&runs);
        let repo_names: HashSet<String> = runs.iter().map(|r| text(r, "repo")).collect();
        let mut head = vec![
            (p.dim.as_str(), format!(" {} runs", runs.len())),
            (p.dim.as_str(), format!(" · {} repos", repo_names.len())),
        ];
        if ok > 0 || !runs.is_empty() {
            head.push((p.ok.as_str(), format!("  {} success", ok)));
        }
        if failed > 0 {
            head.push((p.fail.as_str(), format!("  {} failed", failed)));
        }
        if queued > 0 {
            head.push((p.queue.as_str(), format!("  {} queued", queued)));
        }
        if running > 0 {
            head.push((
                p.run.as_str(),
                format!("  {} {} running", tc::SPINNER[tick % tc::SPINNER.len()], running),
            ));
        }
        rows.push(tc::seg(&head, w - 1));
        if !scope.is_empty() {
            rows.push(tc::seg(&[(p.dim.as_str(), format!(" {}", scope))], w - 1));
        }
        if !err.is_empty() {
            rows.push(tc::seg(&[(p.fail.as_str(), format!(" ! {}", err))], w - 1));
        }
        let mut bits: Vec<String> = Vec::new();
        if FILTERS[filter] != "all" {
            bits.push(FILTERS[filter].to_string());
        }
        if !needle.is_empty() {
            bits.push(format!("/{}", needle));
        }
        if !bits.is_empty() || typing {
            rows.push(tc::seg(
                &[
                    (p.run.as_str(), format!(" filter: {}", bits.join(" + "))),
                    (p.run.as_str(), if typing { "▏".into() } else { String::new() }),
                    (
                        p.dim.as_str(),
                        if typing {
                            "  enter to keep · esc to clear".into()
                        } else {
                            String::new()
                        },
                    ),
                ],
                w - 1,
            ));
        }

        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── ACTIVITY ── ".into()),
                (p.dim.as_str(), format!("runs/hour, last {}h", hours)),
            ],
            w - 1,
        ));
        let (chart, peak) = activity(&runs, w, hours as f64, &p);
        rows.push(chart);
        if peak > 0 {
            rows.push(tc::seg(
                &[
                    (p.dim.as_str(), format!(" {}h ago", hours)),
                    (p.dim.as_str(), " ".repeat(w.saturating_sub(22).max(1))),
                    (p.dim.as_str(), format!("peak {}/h", peak)),
                ],
                w - 1,
            ));
        }

        if !runs.is_empty() {
            let mut durs: Vec<f64> = runs.iter().filter_map(|r| run_secs(r, tc::now())).collect();
            durs.sort_by(f64::total_cmp);
            let mut queues: Vec<f64> = runs.iter().filter_map(|r| queue_secs(r, tc::now())).collect();
            queues.sort_by(f64::total_cmp);
            if !durs.is_empty() {
                rows.push(String::new());
                rows.push(tc::seg(
                    &[
                        (p.lbl.as_str(), " ── DURATION ── ".into()),
                        (p.dim.as_str(), "median ".into()),
                        (p.txt.as_str(), dur_label(durs.get(durs.len() / 2).copied())),
                        (p.dim.as_str(), "  p95 ".into()),
                        (p.txt.as_str(), dur_label(percentile(&durs, 0.95))),
                        (p.dim.as_str(), "  max ".into()),
                        (p.txt.as_str(), dur_label(durs.last().copied())),
                        (p.dim.as_str(), "  queue ".into()),
                        (p.txt.as_str(), dur_label(queues.get(queues.len() / 2).copied())),
                    ],
                    w - 1,
                ));
                let recent = recent_run_secs(&runs, tc::now());
                let spark = sparkline(&recent, w.saturating_sub(2).max(10));
                if !spark.is_empty() {
                    rows.push(tc::seg(&[(p.ok.as_str(), format!(" {}", spark))], w - 1));
                }
            }

            let repeats = repeat_failures(&runs);
            if !repeats.is_empty() {
                rows.push(String::new());
                rows.push(tc::seg(
                    &[(p.lbl.as_str(), " ── REPEATED FAILURES ── ".into())],
                    w - 1,
                ));
                for ((repo, workflow), n) in repeats.iter().take(3) {
                    let short = repo.rsplit('/').next().unwrap_or(repo);
                    rows.push(tc::seg(
                        &[
                            (p.fail.as_str(), format!("  {}×  ", n)),
                            (p.txt.as_str(), format!("{}  ", workflow)),
                            (p.dim.as_str(), short.to_string()),
                        ],
                        w - 1,
                    ));
                }
            }
        }

        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── RECENT ── ".into()),
                (
                    p.dim.as_str(),
                    if shown.is_empty() {
                        String::new()
                    } else {
                        format!("{} of {}", selected + 1, shown.len())
                    },
                ),
            ],
            w - 1,
        ));

        let cols = columns(w);
        let per_item = if cols.single { 1 } else { 2 };
        visible = (h.saturating_sub(rows.len() + 2) / per_item).max(1);
        scroll = scroll.min(shown.len().saturating_sub(visible));
        if selected < scroll {
            scroll = selected;
        } else if selected >= scroll + visible {
            scroll = selected + 1 - visible;
        }

        let mut prev_owner = String::new();
        for (i, run) in shown.iter().enumerate().skip(scroll) {
            if rows.len() >= h.saturating_sub(1) {
                break;
            }
            let owner = repo_owner(&text(run, "repo"));
            if owner != prev_owner {
                rows.push(tc::seg(
                    &[(p.lbl.as_str(), scope_heading(&owner, &viewer))],
                    w - 1,
                ));
                prev_owner = owner;
                if rows.len() >= h.saturating_sub(1) {
                    break;
                }
            }
            let here = i == selected;
            let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
            let c = |colour: &str| {
                // Any colour that would not clear AA on this tint is swapped
                // for its lighter twin. `dim` is 3.81 and `fail` is 4.05
                // against bg(38, 56, 76); the twins are what reach the row.
                let colour = if tint.is_empty() {
                    colour
                } else if colour == p.dim {
                    p.dim_lit.as_str()
                } else if colour == p.fail {
                    p.fail_lit.as_str()
                } else {
                    colour
                };
                format!("{}{}", tint, colour)
            };
            let kind = outcome(run);
            let colour = outcome_colour(&kind, &p);
            let mark = if LIVE.contains(&text(run, "status").as_str()) {
                tc::SPINNER[tick % tc::SPINNER.len()]
            } else if is_failed(run) {
                '✖'
            } else if kind == "success" {
                '●'
            } else {
                '○'
            };
            let subject = text(run, "display_title");
            let subject = subject.lines().next().unwrap_or("").to_string();
            let sha = text(run, "head_sha");
            let mut line = vec![
                (
                    c(colour),
                    format!(
                        "{}{} {:<9}",
                        if here { "▸" } else { " " },
                        mark,
                        outcome_label(&kind)
                    ),
                ),
                (c(&p.txt), tc::pad(&repo_short(&text(run, "repo")), cols.repo)),
                (c(&p.txt), tc::pad(&text(run, "workflow"), cols.workflow)),
                (c(&p.dim), format!(" {}", dur_label(run_secs(run, tc::now())))),
                (c(&p.dim), format!(" {:>4}", run_age(run, tc::now()))),
            ];
            if cols.detail {
                line.push((
                    c(&p.sha),
                    format!("  {}", &sha[..sha.len().min(7)]),
                ));
                line.push((
                    c(&p.branch),
                    format!(" {}", tc::pad(&text(run, "branch"), 14)),
                ));
            }
            if cols.event {
                line.push((c(&p.dim), format!(" {}", tc::pad(&text(run, "event"), 12))));
            }
            if cols.queued {
                line.push((
                    c(&p.dim),
                    format!(" q{}", dur_label(queue_secs(run, tc::now())).trim()),
                ));
            }
            if cols.single {
                line.push((
                    c(if here { &p.txt } else { &p.msg }),
                    format!(" {}", subject),
                ));
            }
            if here {
                line.push((tint.clone(), " ".repeat(w)));
            }
            let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
            if !cols.single && rows.len() < h.saturating_sub(1) {
                rows.push(tc::seg(
                    &[
                        (
                            &c(if here { &p.txt } else { &p.msg }),
                            format!("   {}", subject),
                        ),
                        (&tint, if here { " ".repeat(w) } else { String::new() }),
                    ],
                    w - 1,
                ));
            }
            visible = i.saturating_sub(scroll) + 1;
        }

        if shown.is_empty() && err.is_empty() {
            // Only the first of these is a wait. The other two are answers -
            // GitHub replied and there is nothing to show - and animating
            // those would say a fetch was still running when none is.
            if runs.is_empty() && fetched == 0.0 {
                rows.extend(tc::waiting(
                    "waiting for GitHub…",
                    w,
                    tick,
                    &p.accent,
                    &p.dim,
                ));
            } else {
                let said = if !needle.is_empty() || FILTERS[filter] != "all" {
                    "   (nothing matches the current filter)"
                } else {
                    "   no runs in this window"
                };
                rows.push(tc::seg(&[(p.dim.as_str(), said.into())], w - 1));
            }
        }

        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
            vec![
                (p.accent.as_str(), "→/↵".into()),
                (p.dim.as_str(), " details".into()),
            ],
            vec![(p.dim.as_str(), format!("[s]tate {}", FILTERS[filter]))],
            vec![(p.dim.as_str(), "[/]filter".into())],
            vec![(p.dim.as_str(), format!("[w]indow {}", window_label(hours)))],
            vec![(p.dim.as_str(), "[r]efresh".into())],
            vec![(p.dim.as_str(), "[q]uit".into())],
        ];
        let footer: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
        rows.truncate(h.saturating_sub(footer.len()));
        while rows.len() < h.saturating_sub(footer.len()) {
            rows.push(String::new());
        }
        rows.extend(footer);
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn repo_short(full: &str) -> String {
    full.rsplit('/').next().unwrap_or(full).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_json(
        status: &str,
        conclusion: &str,
        created: &str,
        started: &str,
        updated: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "status": status,
            "conclusion": conclusion,
            "created_at": created,
            "run_started_at": started,
            "updated_at": updated,
        })
    }

    #[test]
    fn a_token_prefers_gha_then_github_then_the_environment() {
        let gha: serde_json::Value =
            serde_json::from_str(r#"{"token": "from-gha", "token_env": "NOPE_TOKEN"}"#).unwrap();
        let gh: serde_json::Value =
            serde_json::from_str(r#"{"token": "from-github"}"#).unwrap();
        assert_eq!(token(&gha, &gh), ("from-gha".to_string(), "config"));
        let empty: serde_json::Value = serde_json::from_str(r#"{"token": ""}"#).unwrap();
        assert_eq!(token(&empty, &gh), ("from-github".to_string(), "config"));
        // Isolate from $GITHUB_TOKEN: CI often has that set, and the
        // last-resort read would then look like a found token. A missing
        // named env is the "no token" case, not whatever the process
        // happens to carry.
        let isolated: serde_json::Value =
            serde_json::from_str(r#"{"token_env": "OPSCOPE_GHA_NO_SUCH_TOKEN"}"#).unwrap();
        assert_eq!(token(&isolated, &isolated).1, "missing");
    }

    #[test]
    fn queue_and_run_times_come_from_stamps() {
        let created = "2026-08-27T10:00:00Z";
        let started = "2026-08-27T10:00:12Z";
        let updated = "2026-08-27T10:03:12Z";
        let done = run_json("completed", "success", created, started, updated);
        assert_eq!(queue_secs(&done, 0.0), Some(12.0));
        assert_eq!(run_secs(&done, 0.0), Some(180.0));
        // A finished run with no start stamp is not a duration of zero.
        let odd = run_json("completed", "success", created, "", updated);
        assert_eq!(queue_secs(&odd, 0.0), None);
        assert_eq!(run_secs(&odd, 0.0), None);
        // Still queued: elapsed since created, not a guess of how long it will wait.
        let at = iso_secs("2026-08-27T10:02:00Z").unwrap();
        let queued = run_json("queued", "", created, "", "");
        assert_eq!(queue_secs(&queued, at), Some(120.0));
        assert_eq!(run_secs(&queued, at), None);
    }

    #[test]
    fn a_duration_keeps_its_column_width() {
        assert_eq!(dur_label(None), "  --  ");
        assert_eq!(dur_label(Some(45.0)), "   45s");
        assert_eq!(dur_label(Some(45.0)).chars().count(), 6);
        assert_eq!(dur_label(None).chars().count(), 6);
        assert_eq!(dur_label(Some(125.0)), "2m05s");
    }

    #[test]
    fn repos_with_workflows_are_picked_newest_first_and_the_cap_is_per_owner() {
        let found = vec![
            FoundRepo {
                name: "acme/old".into(),
                pushed_at: "2026-08-01T00:00:00Z".into(),
                has_workflows: true,
            },
            FoundRepo {
                name: "acme/docs".into(),
                pushed_at: "2026-08-27T00:00:00Z".into(),
                has_workflows: false,
            },
            FoundRepo {
                name: "acme/app".into(),
                pushed_at: "2026-08-26T00:00:00Z".into(),
                has_workflows: true,
            },
            FoundRepo {
                name: "acme/lib".into(),
                pushed_at: "2026-08-25T00:00:00Z".into(),
                has_workflows: true,
            },
            FoundRepo {
                name: "alice/toy".into(),
                pushed_at: "2026-08-10T00:00:00Z".into(),
                has_workflows: true,
            },
        ];
        let since = iso_secs("2026-08-20T00:00:00Z").unwrap();
        let (picked, stats) = pick_repos(&found, since, 1, "alice");
        // alice/toy is outside the push window; acme keeps its newest.
        assert_eq!(picked, vec!["acme/app".to_string()]);
        assert_eq!(stats.org, 1);
        assert_eq!(stats.org_found, 2, "the cap is not the set");
        assert_eq!(stats.personal, 0);

        let since = iso_secs("2026-08-01T00:00:00Z").unwrap();
        let (picked, stats) = pick_repos(&found, since, 1, "alice");
        assert_eq!(picked, vec!["alice/toy".to_string(), "acme/app".to_string()]);
        assert_eq!(stats.personal, 1);
        assert_eq!(stats.personal_found, 1);
        assert_eq!(stats.org, 1);
        assert_eq!(stats.org_found, 3, "acme had three eligible, kept one");

        let (none, stats) = pick_repos(&found, iso_secs("2026-08-27T12:00:00Z").unwrap(), 8, "alice");
        assert!(none.is_empty());
        assert_eq!(stats.personal_found + stats.org_found, 0);
    }

    #[test]
    fn personal_runs_sort_ahead_of_orgs_and_keep_newest_inside_a_scope() {
        let mut runs = vec![
            serde_json::json!({
                "repo": "acme/app", "created_at": "2026-08-27T12:00:00Z"
            }),
            serde_json::json!({
                "repo": "alice/toy", "created_at": "2026-08-27T10:00:00Z"
            }),
            serde_json::json!({
                "repo": "alice/toy", "created_at": "2026-08-27T11:00:00Z"
            }),
            serde_json::json!({
                "repo": "beta/lib", "created_at": "2026-08-27T13:00:00Z"
            }),
        ];
        sort_runs_by_scope(&mut runs, "alice");
        let names: Vec<String> = runs.iter().map(|r| text(r, "repo")).collect();
        assert_eq!(
            names,
            vec![
                "alice/toy".to_string(),
                "alice/toy".to_string(),
                "acme/app".to_string(),
                "beta/lib".to_string()
            ]
        );
        assert_eq!(runs[0]["created_at"], "2026-08-27T11:00:00Z");
        assert_eq!(scope_heading("alice", "alice"), " ── PERSONAL · alice ── ");
        assert_eq!(scope_heading("acme", "alice"), " ── ACME ── ");
    }

    #[test]
    fn graphql_nodes_become_found_repos() {
        let data = serde_json::json!({
            "organization": {
                "repositories": {
                    "nodes": [
                        {
                            "nameWithOwner": "acme/app",
                            "isEmpty": false,
                            "pushedAt": "2026-08-26T00:00:00Z",
                            "object": {"entries": [{"name": "ci.yml"}]}
                        },
                        {
                            "nameWithOwner": "acme/empty",
                            "isEmpty": true,
                            "pushedAt": "2026-08-26T00:00:00Z",
                            "object": {"entries": [{"name": "ci.yml"}]}
                        },
                        {
                            "nameWithOwner": "acme/docs",
                            "isEmpty": false,
                            "pushedAt": "2026-08-26T00:00:00Z",
                            "object": serde_json::Value::Null
                        }
                    ]
                }
            }
        });
        let got = repos_from(&data);
        assert_eq!(got.len(), 2);
        assert!(got[0].has_workflows);
        assert!(!got[1].has_workflows);
        assert_eq!(got[0].name, "acme/app");
    }

    #[test]
    fn a_run_keeps_the_repo_total_so_a_partial_page_can_say_so() {
        let raw = serde_json::json!({
            "id": 9,
            "name": "CI",
            "head_branch": "main",
            "event": "push",
            "status": "completed",
            "conclusion": "failure",
            "created_at": "2026-08-27T10:00:00Z",
            "run_started_at": "2026-08-27T10:00:04Z",
            "updated_at": "2026-08-27T10:02:00Z",
            "html_url": "https://github.com/acme/app/actions/runs/9",
            "run_number": 12,
            "head_sha": "abc1234deadbeef",
            "display_title": "fix: the thing",
            "run_attempt": 2
        });
        let got = ingest_run(&raw, "acme/app", 80, 30);
        assert_eq!(got["repo"], "acme/app");
        assert_eq!(got["workflow"], "CI");
        assert_eq!(got["repo_total"], 80);
        assert_eq!(got["repo_fetched"], 30);
        assert_eq!(got["attempt"], 2);
        assert!(is_failed(&got));
    }

    #[test]
    fn the_failed_step_is_the_first_one_that_failed() {
        let job = serde_json::json!({
            "steps": [
                {"number": 1, "name": "checkout", "conclusion": "success"},
                {"number": 4, "name": "cargo test", "conclusion": "failure"},
                {"number": 5, "name": "upload", "conclusion": "skipped"}
            ]
        });
        assert_eq!(failed_step(&job), Some((4, "cargo test".into())));
        assert_eq!(failed_step(&serde_json::json!({"steps": []})), None);
    }

    #[test]
    fn a_workflow_that_failed_once_is_not_a_repeat() {
        let fail = serde_json::json!({
            "repo": "acme/app", "workflow": "CI",
            "status": "completed", "conclusion": "failure"
        });
        let ok = serde_json::json!({
            "repo": "acme/app", "workflow": "CI",
            "status": "completed", "conclusion": "success"
        });
        let other = serde_json::json!({
            "repo": "acme/app", "workflow": "Release",
            "status": "completed", "conclusion": "failure"
        });
        let again = fail.clone();
        assert!(repeat_failures(&[fail.clone(), ok, other]).is_empty());
        let repeats = repeat_failures(&[fail, again]);
        assert_eq!(repeats, vec![(("acme/app".into(), "CI".into()), 2)]);
    }

    #[test]
    fn the_scope_sentence_names_a_cap_per_scope_instead_of_calling_it_a_total() {
        let cut = scope_sentence(
            false,
            17,
            &PickStats {
                personal: 3,
                personal_found: 3,
                org: 16,
                org_found: 40,
            },
            2,
            48,
            14,
        );
        assert!(cut.contains("3 personal"), "{cut}");
        assert!(cut.contains("16 of 40 org"), "{cut}");
        assert!(cut.contains("2 orgs"), "{cut}");
        assert!(!cut.contains("last "), "{cut}");
        assert!(!cut.contains("17 of"), "{cut}");
        let exact = scope_sentence(
            false,
            4,
            &PickStats {
                personal: 1,
                personal_found: 1,
                org: 3,
                org_found: 3,
            },
            1,
            24,
            14,
        );
        assert!(
            exact.is_empty(),
            "uncapped look is named on the activity line, not twice: {exact}"
        );
        let named = scope_sentence(true, 2, &PickStats::default(), 0, 12, 14);
        assert!(named.contains("configured"), "{named}");
        assert!(!named.contains("recently pushed"), "{named}");
        assert!(!named.contains("last "), "{named}");
    }

    #[test]
    fn a_missing_created_stamp_is_not_drawn_as_zero() {
        let at = iso_secs("2026-08-27T10:00:00Z").unwrap();
        let stamped = serde_json::json!({ "created_at": "2026-08-27T09:59:00Z" });
        assert_eq!(run_age(&stamped, at), "60s");
        let blank = serde_json::json!({ "created_at": "" });
        assert_eq!(run_age(&blank, at), "--");
    }

    #[test]
    fn duration_spark_is_oldest_first_so_the_suffix_is_recent() {
        let runs = vec![
            run_json(
                "completed",
                "success",
                "2026-08-27T10:03:00Z",
                "2026-08-27T10:03:00Z",
                "2026-08-27T10:03:10Z",
            ),
            run_json(
                "completed",
                "success",
                "2026-08-27T10:02:00Z",
                "2026-08-27T10:02:00Z",
                "2026-08-27T10:02:20Z",
            ),
            run_json(
                "completed",
                "success",
                "2026-08-27T10:01:00Z",
                "2026-08-27T10:01:00Z",
                "2026-08-27T10:01:30Z",
            ),
        ];
        let secs = recent_run_secs(&runs, 0.0);
        assert_eq!(secs, vec![30.0, 20.0, 10.0]);
    }

    #[test]
    fn activity_skips_a_run_with_no_created_stamp() {
        let p = palette();
        let runs = vec![
            serde_json::json!({
                "created_at": "",
                "status": "completed",
                "conclusion": "success"
            }),
        ];
        let (line, peak) = activity(&runs, 40, 48.0, &p);
        assert_eq!(peak, 0);
        assert!(line.contains("no runs"), "{line}");
    }

    #[test]
    fn width_buys_columns_rather_than_padding() {
        assert!(!columns(60).detail);
        assert!(columns(72).detail);
        assert!(!columns(100).single);
        assert!(columns(110).single);
        assert!(!columns(90).event);
        assert!(columns(100).event);
        assert!(!columns(100).queued);
        assert!(columns(114).queued);
        assert_eq!(columns(60).repo, 10);
        assert_eq!(columns(200).repo, 18);
    }

    #[test]
    fn a_window_label_says_days_when_it_is_a_whole_number_of_them() {
        assert_eq!(window_label(12), "12h");
        assert_eq!(window_label(24), "24h");
        assert_eq!(window_label(48), "48h");
        assert_eq!(window_label(168), "7d");
    }

    #[test]
    fn owner_and_name_have_to_be_exactly_two_parts() {
        assert_eq!(
            split_repo("acme/app"),
            Some(("acme".into(), "app".into()))
        );
        assert_eq!(split_repo("acme"), None);
        assert_eq!(split_repo("acme/app/extra"), None);
        assert_eq!(split_repo("/app"), None);
    }

    #[test]
    fn github_rate_headers_become_remaining_over_limit() {
        let headers = vec![
            ("x-ratelimit-remaining".into(), "4840".into()),
            ("x-ratelimit-limit".into(), "5000".into()),
            ("x-ratelimit-resource".into(), "core".into()),
        ];
        assert_eq!(rate_from_headers(&headers), Some((4840, 5000)));
        assert_eq!(rate_from_headers(&[]), None);
        assert_eq!(
            rate_from_headers(&[("x-ratelimit-remaining".into(), "10".into())]),
            None,
            "a remaining without a limit is not a reading"
        );
    }

    #[test]
    fn a_missing_poll_stamp_is_not_drawn_as_zero_seconds_ago() {
        assert_eq!(tc::ago(0.0), "--");
        assert_eq!(tc::ago(-1.0), "--");
        // A poll that has just landed says seconds, not the "1m" a floor
        // of one minute used to report on a board that was current.
        assert_eq!(tc::ago(tc::now() - 4.0), "4s");
    }

    fn sample_run(id: i64) -> serde_json::Value {
        serde_json::json!({ "id": id, "status": "completed", "conclusion": "success" })
    }

    #[test]
    fn a_result_for_another_window_does_not_relabel_the_board() {
        let mut live = State {
            runs: vec![sample_run(1)],
            err: String::new(),
            fetched: 10.0,
            scope: "last 2d · 1 configured repo".into(),
            window_hours: 24,
            ..Default::default()
        };
        apply_pass(
            &mut live,
            State {
                runs: vec![sample_run(2), sample_run(3)],
                err: String::new(),
                fetched: 20.0,
                scope: "last 7d · 1 configured repo".into(),
                window_hours: 168,
                ..Default::default()
            },
        );
        assert_eq!(live.runs.len(), 1);
        assert_eq!(live.runs[0]["id"], 1);
        assert_eq!(live.window_hours, 24);
        assert_eq!(live.scope, "last 2d · 1 configured repo");
        assert_eq!(live.fetched, 10.0);
    }

    #[test]
    fn a_same_window_error_keeps_the_runs_this_window_already_showed() {
        let mut live = State {
            runs: vec![sample_run(1)],
            err: String::new(),
            fetched: 10.0,
            scope: "last 2d · 1 configured repo".into(),
            window_hours: 48,
            ..Default::default()
        };
        apply_pass(
            &mut live,
            State {
                runs: vec![],
                err: "acme/app: 502".into(),
                fetched: 20.0,
                scope: "last 2d · 1 configured repo".into(),
                window_hours: 48,
                ..Default::default()
            },
        );
        assert_eq!(live.runs.len(), 1);
        assert_eq!(live.err, "acme/app: 502");
        assert_eq!(live.fetched, 10.0);
    }

    #[test]
    fn changing_window_drops_the_old_figures_instead_of_retinting_them() {
        let mut live = State {
            runs: vec![sample_run(1)],
            err: "partial".into(),
            fetched: 10.0,
            scope: "last 2d · 1 configured repo".into(),
            window_hours: 48,
            ..Default::default()
        };
        request_window(&mut live, 12);
        assert_eq!(live.window_hours, 12);
        assert!(live.runs.is_empty());
        assert!(live.scope.is_empty());
        assert_eq!(live.fetched, 0.0);
        assert!(live.err.is_empty());
    }

    #[test]
    fn repos_conn_prefers_the_org_side_and_reads_page_info() {
        let data = serde_json::json!({
            "organization": {
                "repositories": {
                    "pageInfo": { "hasNextPage": true, "endCursor": "c1" },
                    "nodes": [
                        {
                            "nameWithOwner": "acme/app",
                            "isEmpty": false,
                            "pushedAt": "2026-08-26T00:00:00Z",
                            "object": {"entries": [{"name": "ci.yml"}]}
                        }
                    ]
                }
            }
        });
        let conn = repos_conn(&data).expect("org connection");
        assert!(conn["pageInfo"]["hasNextPage"].as_bool().unwrap());
        assert_eq!(conn["pageInfo"]["endCursor"], "c1");
        assert_eq!(repos_from(&data).len(), 1);
    }

    #[test]
    fn a_not_found_side_does_not_drop_the_repos_that_did_arrive() {
        let body = serde_json::json!({
            "data": {
                "organization": {
                    "repositories": {
                        "nodes": [{
                            "nameWithOwner": "acme/app",
                            "isEmpty": false,
                            "pushedAt": "2026-08-26T00:00:00Z",
                            "object": {"entries": [{"name": "ci.yml"}]}
                        }]
                    }
                },
                "user": null
            },
            "errors": [{
                "type": "NOT_FOUND",
                "path": ["user"],
                "message": "Could not resolve to a User with the login of 'acme'."
            }]
        });
        let data = graphql_payload(&body).expect("data is present");
        assert_eq!(repos_from(&data).len(), 1);
        assert!(repos_from(&data)[0].has_workflows);
    }

    #[test]
    fn a_null_payload_is_the_graphql_failure() {
        let body = serde_json::json!({
            "data": null,
            "errors": [{"message": "Something went wrong while executing your query"}]
        });
        let err = graphql_payload(&body).unwrap_err();
        assert!(err.contains("Something went wrong"), "{err}");
    }

    #[test]
    fn repository_owner_is_the_discover_shape() {
        let data = serde_json::json!({
            "repositoryOwner": {
                "repositories": {
                    "pageInfo": { "hasNextPage": false, "endCursor": "c1" },
                    "nodes": [{
                        "nameWithOwner": "acme/app",
                        "isEmpty": false,
                        "pushedAt": "2026-08-26T00:00:00Z",
                        "object": {"entries": [{"name": "ci.yml"}]}
                    }]
                }
            }
        });
        let conn = repos_conn(&data).expect("owner connection");
        assert_eq!(conn["pageInfo"]["endCursor"], "c1");
        assert_eq!(repos_from(&data)[0].name, "acme/app");
    }
}
