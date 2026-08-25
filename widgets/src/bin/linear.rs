// terminal-toys - small dependency-free terminal widgets
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

//! How the work is moving, across every Linear team.
//!
//! A port of linear.py. Linear has no totalCount on its connections, so
//! anything counted here is walked a page at a time; only the fields the
//! screen actually shows are asked for, because complexity is charged per
//! property and the page count is what costs.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{NaiveDateTime, Utc};
use toys_core as tc;

const API: &str = "https://api.linear.app/graphql";
/// Linear's maximum page size.
const PAGE: usize = 250;
/// Pages per query, so one huge team cannot spin forever.
const PAGE_CAP: usize = 12;
const WINDOWS: &[i64] = &[7, 14, 30, 60, 90];
const SETTLE_FRAMES: usize = 8;
/// Tail of a cycle's history that counts as "lately".
const CHURN_DAYS: usize = 6;
const STATE_ORDER: &[&str] = &["triage", "backlog", "unstarted", "started"];

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// A Linear personal API key, from config.json or the environment.
fn token(cfg: &serde_json::Value) -> (String, &'static str) {
    let from_config = tc::cfg_str(cfg, "token", "");
    if !from_config.is_empty() {
        return (from_config, "config");
    }
    let name = tc::cfg_str(cfg, "token_env", "LINEAR_API_KEY");
    let name = if name.is_empty() { "LINEAR_API_KEY".into() } else { name };
    match std::env::var(&name) {
        Ok(value) if !value.is_empty() => (value, "env"),
        _ => (String::new(), "missing"),
    }
}

/// What the API says is left of this hour's allowance.
#[derive(Clone, Copy, Default)]
struct Quota {
    requests: Option<i64>,
}

fn graphql(
    query: &str,
    tok: &str,
    variables: serde_json::Value,
    quota: &Arc<Mutex<Quota>>,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "query": query, "variables": variables }).to_string();
    let (text, headers) = tc::post_json(
        API,
        &[
            ("Authorization", tok),
            ("Content-Type", "application/json"),
            ("User-Agent", "terminal-toys"),
        ],
        &body,
        30,
    )?;
    for (name, value) in &headers {
        if name == "x-ratelimit-requests-remaining" {
            if let Ok(left) = value.parse() {
                if let Ok(mut guard) = quota.lock() {
                    guard.requests = Some(left);
                }
            }
        }
    }
    let data: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if let Some(first) = data["errors"].as_array().and_then(|a| a.first()) {
        return Err(first["message"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect());
    }
    Ok(data["data"].clone())
}

/// Follow pageInfo to the end, or to PAGE_CAP, and return every node.
///
/// The bool says the cap was hit, so the screen can mark the count as a
/// floor rather than reporting a truncated total as a total.
fn pages(
    tok: &str,
    query: &str,
    path: &[&str],
    variables: &serde_json::Value,
    quota: &Arc<Mutex<Quota>>,
) -> Result<(Vec<serde_json::Value>, bool), String> {
    let mut out = Vec::new();
    let mut cursor = serde_json::Value::Null;
    for _ in 0..PAGE_CAP {
        let mut v = variables.clone();
        v["after"] = cursor.clone();
        let mut conn = graphql(query, tok, v, quota)?;
        for step in path {
            conn = conn[*step].clone();
        }
        for node in conn["nodes"].as_array().into_iter().flatten() {
            out.push(node.clone());
        }
        if !conn["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false) {
            return Ok((out, false));
        }
        cursor = conn["pageInfo"]["endCursor"].clone();
    }
    Ok((out, true))
}

fn open_query() -> String {
    format!(
        r#"
query($after: String) {{
  issues(first: {}, after: $after,
         filter: {{ state: {{ type: {{ nin: ["completed", "canceled",
                                          "duplicate"] }} }} }}) {{
    nodes {{ identifier title url estimate startedAt createdAt
            state {{ type }} team {{ key }} project {{ id }} cycle {{ id }} }}
    pageInfo {{ hasNextPage endCursor }}
  }}
}}"#,
        PAGE
    )
}

fn created_query() -> String {
    format!(
        r#"
query($after: String, $since: DateTimeOrDuration!) {{
  issues(first: {}, after: $after, filter: {{ createdAt: {{ gte: $since }} }}) {{
    nodes {{ createdAt team {{ key }} }}
    pageInfo {{ hasNextPage endCursor }}
  }}
}}"#,
        PAGE
    )
}

fn done_query() -> String {
    format!(
        r#"
query($after: String, $since: DateTimeOrDuration!) {{
  issues(first: {}, after: $after, filter: {{ completedAt: {{ gte: $since }} }}) {{
    nodes {{ identifier completedAt startedAt createdAt team {{ key }} }}
    pageInfo {{ hasNextPage endCursor }}
  }}
}}"#,
        PAGE
    )
}

const CYCLES_QUERY: &str = r#"
{
  cycles(first: 50, filter: { isActive: { eq: true } }) {
    nodes {
      id name number startsAt endsAt progress
      issueCountHistory completedIssueCountHistory
      scopeHistory completedScopeHistory
      team { key name }
    }
    pageInfo { hasNextPage endCursor }
  }
}"#;

/// Every project in the workspace, with the teams that own it.
///
/// One request covers all of them - there are dozens, not thousands - so a
/// team's projects are already in hand when its screen opens, rather than
/// arriving a request later while the screen shows nothing.
const PROJECTS_QUERY: &str = r#"
query($after: String) {
  projects(first: 100, after: $after) {
    nodes {
      id name progress targetDate
      status { name type }
      lead { name }
      teams(first: 20) { nodes { key } }
    }
    pageInfo { hasNextPage endCursor }
  }
}"#;

/// One project in full, asked for only when its screen is opened.
///
/// The board's own pass carries what a list needs - name, status, progress,
/// a target date. The rest is here: the burn-up, the milestones, who is on
/// it, what it says it is for. Fetching that for every project in the
/// workspace every two minutes would be paying, continuously, for screens
/// nobody has opened.
const PROJECT_QUERY: &str = r#"
query($id: String!) {
  project(id: $id) {
    id name url description startedAt completedAt
    scopeHistory completedScopeHistory
    issueCountHistory completedIssueCountHistory
    members(first: 20) { nodes { name } }
    projectMilestones(first: 25) { nodes { name targetDate progress } }
    initiatives(first: 5) { nodes { name } }
  }
}"#;

const TEAMS_QUERY: &str = r#"
{ teams(first: 100) { nodes { key name } pageInfo { hasNextPage } } }"#;

fn text(value: &serde_json::Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_string()
}

/// The calendar day of an ISO timestamp, as Linear returns them.
fn day(ts: &str) -> String {
    ts.chars().take(10).collect()
}

fn parse(ts: &str) -> Option<NaiveDateTime> {
    // By characters, not bytes. len() and [..19] are both byte operations,
    // so a timestamp with any multibyte character in its first nineteen
    // bytes used to panic on a character boundary rather than decline to
    // parse - in a poll thread, on data from a server.
    let head: String = ts.chars().take(19).collect();
    if head.chars().count() < 19 {
        return None;
    }
    NaiveDateTime::parse_from_str(&head, "%Y-%m-%dT%H:%M:%S").ok()
}

fn hours_since(from: Option<NaiveDateTime>, to: Option<NaiveDateTime>) -> Option<f64> {
    let (from, to) = (from?, to?);
    Some((to - from).num_seconds() as f64 / 3600.0)
}

fn ago(t: f64) -> String {
    if t <= 0.0 {
        return "--".into();
    }
    let s = (now() - t) as i64;
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    }
}

/// A span at whatever unit keeps it readable.
///
/// Rolls over to years because these figures reach them: an issue open for
/// "1021.6d" is arithmetic, one open for "2.8y" is a decision.
fn dur(hours: Option<f64>) -> String {
    let Some(h) = hours else {
        return "--".into();
    };
    if h < 1.0 {
        return format!("{}m", ((h * 60.0) as i64).max(1));
    }
    if h < 48.0 {
        return format!("{:.1}h", h);
    }
    let days = h / 24.0;
    if days < 365.0 {
        format!("{:.1}d", days)
    } else {
        format!("{:.1}y", days / 365.0)
    }
}

fn median(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let mut s = xs.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    Some(if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    })
}

/// One open issue, as much of it as a list needs.
///
/// Every field here rides the pass the board already makes over every open
/// issue, so a cycle's list of them costs no request of its own.
#[derive(Clone, Default)]
struct Issue {
    ident: String,
    title: String,
    url: String,
    state: String,
    /// Hours since it was created.
    age: f64,
    /// None when nobody has pointed it, which is not the same as zero.
    points: Option<f64>,
}

/// One project, as much of it as a team's screen needs.
///
/// `progress` is Linear's own published figure, not one derived here: it
/// counts issues this widget never fetches, because the board asks only for
/// what is open and a finished project has none.
#[derive(Clone, Default)]
struct Proj {
    id: String,
    name: String,
    /// The workspace's own name for the status - "In Progress", not
    /// "started" - with `kind` left to pick the colour.
    label: String,
    kind: String,
    progress: f64,
    target: String,
    lead: String,
    /// Every team that owns it. A project can be shared, and the board's own
    /// list is not filed under any one of them.
    teams: Vec<String>,
}

/// One project's own record, fetched when its screen opens.
///
/// The error is kept in the same place the answer would be. A fetch that
/// fails silently leaves a screen saying "loading" for ever, which is this
/// widget's oldest lesson written a different way.
fn fetch_project(id: &str, tok: &str, quota: &Arc<Mutex<Quota>>) -> serde_json::Value {
    let vars = serde_json::json!({ "id": id });
    match graphql(PROJECT_QUERY, tok, vars, quota) {
        Ok(v) => v["project"].clone(),
        Err(said) => serde_json::json!({ "_error": said }),
    }
}

/// How much of a project row each optional column gets.
///
/// `base` is what never goes - the marker, the team key, the name and the
/// percentage. Returns what the status column costs, how wide the bar may
/// be (zero for none), and how many columns are left for the open count.
///
/// The order things go in as the pane narrows: the bar first, because the
/// percentage beside it says the same thing in five columns; then the
/// status, which the colour and the ordering carry anyway; and last the
/// open count, which is the one number on the row the percentage does not
/// say. Nothing is ever cut through the middle.
///
/// This is a function because the arithmetic was wrong twice inline: once
/// overflowing the pane and letting `seg` cut the percentage off, and once
/// reserving the bar's gap but not the open count's - which made a row lose
/// its open count as the pane got *wider*.
fn project_columns(w: usize, base: usize, label_w: usize, aside: usize) -> (usize, usize, usize) {
    let budget = w.saturating_sub(1);
    // Each column is shown only when it and everything above it in the
    // order fits, and the ones above it are reserved whether or not they
    // are shown. Anything less than that and widening the pane by one
    // column can trade one fact for another - at seventy-seven columns the
    // status arrived and the open count left, which is a pane that gets
    // wider and says less.
    let need_aside = if aside > 0 { 2 + aside } else { 0 };
    let need_label = 2 + label_w;
    let room = if base + need_aside <= budget { aside } else { 0 };
    let label_cost = if base + need_aside + need_label <= budget { need_label } else { 0 };
    let bar = budget.saturating_sub(base + need_aside + need_label + 2);
    let bar = if bar >= 6 { bar.min(30) } else { 0 };
    (label_cost, bar, room)
}


/// Break text to a width without breaking a word, and without dropping one.
fn wrap(t: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in t.split_whitespace() {
        let n = word.chars().count();
        if line.is_empty() {
            line = word.to_string();
        } else if line.chars().count() + 1 + n <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            out.push(std::mem::take(&mut line));
            line = word.to_string();
        }
        // A word longer than the line has to break somewhere; it breaks at
        // the width rather than being thrown away.
        while line.chars().count() > width {
            let head: String = line.chars().take(width).collect();
            line = line.chars().skip(width).collect();
            out.push(head);
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// A project's aside, as one string. Measured and drawn through the same
/// call so the separator cannot be counted one way and printed another.
fn joined(parts: &[String]) -> String {
    parts.join(" · ")
}

/// What a section heading says about the keys, given where the focus is.
///
/// The focused one carries the arrows. Exactly one other carries `[tab]` -
/// the one tab would actually focus next - because tab cycles, and a
/// `[tab]` on every heading would promise three sections that one press
/// reaches, when one press reaches one of them. netwatch learned this the
/// expensive way with per-heading letters bound to keys that had gone; the
/// note in its `section_head` says tab was the only one that was ever true.
fn heading_keys(me: usize, focus: Option<usize>, lens: &[usize]) -> &'static str {
    if focus == Some(me) {
        "   ↑↓"
    } else if tc::next_section(focus, lens) == Some(me) {
        "   [tab] to focus"
    } else {
        ""
    }
}

/// A count with its noun, singular when it is one.
fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{} {}", n, noun)
    } else {
        format!("{} {}s", n, noun)
    }
}

/// Where an issue's state sorts: what is moving first, what nobody has
/// looked at last.
fn state_rank(state: &str) -> usize {
    match state {
        "started" => 0,
        "unstarted" => 1,
        "backlog" => 2,
        "triage" => 3,
        _ => 4,
    }
}

/// Where a project's status sorts, running work first and finished last.
///
/// An unknown status sorts with the live ones rather than the dead ones: a
/// workspace can name its own statuses, and burying one this build has not
/// heard of would hide real work.
fn rank(kind: &str) -> usize {
    match kind {
        "started" => 0,
        "planned" => 2,
        "paused" => 3,
        "backlog" => 4,
        "completed" => 5,
        "canceled" => 6,
        _ => 1,
    }
}

/// An issue worth going and looking at: how long, and which one.
type Extreme = Option<(f64, String)>;

#[derive(Default)]
struct State {
    teams: Vec<(String, String)>,
    states: HashMap<String, usize>,
    by_team: HashMap<String, HashMap<String, usize>>,
    /// Team key to that team's projects, ordered as the screen shows them.
    projects: HashMap<String, Vec<Proj>>,
    /// Every project once, in the same order, for the board's own section.
    all_projects: Vec<Proj>,
    /// Team key to project id to how many of that team's open issues sit in
    /// it. The empty id is the bucket for issues in no project at all, which
    /// is why the per-project figures do not sum to the team's open count.
    proj_open: HashMap<String, HashMap<String, usize>>,
    /// Project id to its open issues by state, across every team that shares
    /// it - a project's own screen is about the project, not about whichever
    /// team's screen it was opened from.
    proj_state: HashMap<String, HashMap<String, usize>>,
    /// Project id to the oldest thing still open in it.
    proj_oldest: HashMap<String, (f64, String)>,
    /// Cycle id to its open issues by state, and to the issues themselves in
    /// the order a screen shows them.
    cycle_state: HashMap<String, HashMap<String, usize>>,
    cycle_issues: HashMap<String, Vec<Issue>>,
    cycles: Vec<serde_json::Value>,
    created: HashMap<String, usize>,
    completed: HashMap<String, usize>,
    lead: Vec<f64>,
    cycle_time: Vec<f64>,
    quickest: Extreme,
    slowest: Extreme,
    oldest_open: Extreme,
    oldest_wip: Extreme,
    /// Which window the counters describe, which is not always the window
    /// the keys have asked for.
    window: i64,
    truncated: bool,
    err: String,
    fetched: f64,
}

/// Whether a team key counts toward the board.
///
/// Named teams win outright; otherwise everything not excluded is in. This
/// lived inside `one_pass` as a closure, where the only way to reach it was
/// a live token and a network round trip - so its test asserted a copy
/// written in the test body, and would have passed with this wrong.
fn team_wanted(keep: &[String], exclude: &[String], key: &str) -> bool {
    if !keep.is_empty() {
        keep.iter().any(|k| k == key)
    } else {
        !exclude.iter().any(|k| k == key)
    }
}

#[allow(clippy::too_many_arguments)]
fn one_pass(
    tok: &str,
    source: &str,
    days: i64,
    keep: &[String],
    exclude: &[String],
    state: &Arc<Mutex<State>>,
    quota: &Arc<Mutex<Quota>>,
) -> Result<(), String> {
    let wanted = |key: &str| team_wanted(keep, exclude, key);
    let since = (Utc::now() - chrono::Duration::days(days - 1))
        .format("%Y-%m-%dT00:00:00.000Z")
        .to_string();

    let teams_res = graphql(TEAMS_QUERY, tok, serde_json::json!({}), quota)?;
    let teams: Vec<(String, String)> = teams_res["teams"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| (text(t, "key"), text(t, "name")))
        .filter(|(key, _)| wanted(key))
        .collect();
    let keys: Vec<String> = teams.iter().map(|(k, _)| k.clone()).collect();
    if let Ok(mut guard) = state.lock() {
        guard.teams = teams.clone();
    }

    // What is outstanding right now, at any age.
    let (rows, capped) = pages(tok, &open_query(), &["issues"], &serde_json::json!({}), quota)?;
    let mut states: HashMap<String, usize> = HashMap::new();
    let mut by_team: HashMap<String, HashMap<String, usize>> = HashMap::new();
    let mut proj_open: HashMap<String, HashMap<String, usize>> = HashMap::new();
    let mut proj_state: HashMap<String, HashMap<String, usize>> = HashMap::new();
    let mut proj_oldest: HashMap<String, (f64, String)> = HashMap::new();
    let mut cycle_state: HashMap<String, HashMap<String, usize>> = HashMap::new();
    let mut cycle_issues: HashMap<String, Vec<Issue>> = HashMap::new();
    let at = Utc::now().naive_utc();
    let (mut oldest_open, mut oldest_wip): (Extreme, Extreme) = (None, None);
    for it in &rows {
        let key = text(&it["team"], "key");
        if !keys.contains(&key) {
            continue;
        }
        let st = text(&it["state"], "type");
        if !STATE_ORDER.contains(&st.as_str()) {
            continue;
        }
        *states.entry(st.clone()).or_insert(0) += 1;
        // The empty string when the issue is in no project, which is a
        // real and common answer here, not a missing one.
        let in_project = text(&it["project"], "id");
        *proj_open
            .entry(key.clone())
            .or_default()
            .entry(in_project.clone())
            .or_insert(0) += 1;
        // A project's own screen wants these broken out, and every one of
        // them is already in this row: no second request buys them.
        if !in_project.is_empty() {
            *proj_state
                .entry(in_project.clone())
                .or_default()
                .entry(st.clone())
                .or_insert(0) += 1;
            if let Some(age) = hours_since(parse(&text(it, "createdAt")), Some(at)) {
                let seat = proj_oldest.entry(in_project).or_insert((0.0, String::new()));
                if age > seat.0 {
                    *seat = (age, text(it, "identifier"));
                }
            }
        }
        // Which cycle it is in, if any, and the issue itself - both ride
        // this pass, so a cycle's screen costs no request of its own.
        let in_cycle = text(&it["cycle"], "id");
        if !in_cycle.is_empty() {
            *cycle_state
                .entry(in_cycle.clone())
                .or_default()
                .entry(st.clone())
                .or_insert(0) += 1;
            cycle_issues.entry(in_cycle).or_default().push(Issue {
                ident: text(it, "identifier"),
                title: text(it, "title"),
                url: text(it, "url"),
                state: st.clone(),
                age: hours_since(parse(&text(it, "createdAt")), Some(at)).unwrap_or(0.0),
                points: it["estimate"].as_f64(),
            });
        }
        let slot = by_team.entry(key).or_default();
        *slot.entry(st.clone()).or_insert(0) += 1;
        *slot.entry("open".into()).or_insert(0) += 1;
        let ident = text(it, "identifier");
        if let Some(age) = hours_since(parse(&text(it, "createdAt")), Some(at)) {
            if oldest_open.as_ref().is_none_or(|(had, _)| age > *had) {
                oldest_open = Some((age, ident.clone()));
            }
        }
        if st == "started" {
            if let Some(age) = hours_since(parse(&text(it, "startedAt")), Some(at)) {
                if oldest_wip.as_ref().is_none_or(|(had, _)| age > *had) {
                    oldest_wip = Some((age, ident));
                }
            }
        }
    }

    // The running cycles, each already carrying its own burndown.
    let cycles_res = graphql(CYCLES_QUERY, tok, serde_json::json!({}), quota)?;
    let cycles: Vec<serde_json::Value> = cycles_res["cycles"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| keys.contains(&text(&c["team"], "key")))
        .cloned()
        .collect();

    // Every project, filed under each team that owns it. A project can be
    // shared, so one node lands in more than one team's list.
    let (proj_rows, cap4) = pages(
        tok,
        PROJECTS_QUERY,
        &["projects"],
        &serde_json::json!({}),
        quota,
    )?;
    let mut projects: HashMap<String, Vec<Proj>> = HashMap::new();
    let mut all_projects: Vec<Proj> = Vec::new();
    for pr in &proj_rows {
        let owners: Vec<String> = pr["teams"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| text(t, "key"))
            .filter(|k| keys.contains(k))
            .collect();
        if owners.is_empty() {
            continue;
        }
        let made = Proj {
            id: text(pr, "id"),
            name: text(pr, "name"),
            label: text(&pr["status"], "name"),
            kind: text(&pr["status"], "type"),
            progress: pr["progress"].as_f64().unwrap_or(0.0),
            target: text(pr, "targetDate"),
            lead: text(&pr["lead"], "name"),
            teams: owners.clone(),
        };
        for key in owners {
            projects.entry(key).or_default().push(made.clone());
        }
        all_projects.push(made);
    }
    let order = |a: &Proj, b: &Proj| {
        rank(&a.kind)
            .cmp(&rank(&b.kind))
            .then(b.progress.total_cmp(&a.progress))
            .then(a.name.cmp(&b.name))
    };
    for list in projects.values_mut() {
        list.sort_by(&order);
    }
    all_projects.sort_by(&order);

    // Arrivals and departures over the window.
    let vars = serde_json::json!({ "since": since });
    let (made, cap2) = pages(tok, &created_query(), &["issues"], &vars, quota)?;
    let (done, cap3) = pages(tok, &done_query(), &["issues"], &vars, quota)?;
    let mut created: HashMap<String, usize> = HashMap::new();
    let mut completed: HashMap<String, usize> = HashMap::new();
    let (mut lead, mut ctime): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    let (mut quickest, mut slowest): (Extreme, Extreme) = (None, None);
    for it in &made {
        if keys.contains(&text(&it["team"], "key")) {
            *created.entry(day(&text(it, "createdAt"))).or_insert(0) += 1;
        }
    }
    for it in &done {
        if !keys.contains(&text(&it["team"], "key")) {
            continue;
        }
        *completed.entry(day(&text(it, "completedAt"))).or_insert(0) += 1;
        let fin = parse(&text(it, "completedAt"));
        let ident = text(it, "identifier");
        if let Some(hrs) = hours_since(parse(&text(it, "createdAt")), fin) {
            lead.push(hrs);
            if quickest.as_ref().is_none_or(|(had, _)| hrs < *had) {
                quickest = Some((hrs, ident.clone()));
            }
            if slowest.as_ref().is_none_or(|(had, _)| hrs > *had) {
                slowest = Some((hrs, ident));
            }
        }
        if let Some(hrs) = hours_since(parse(&text(it, "startedAt")), fin) {
            ctime.push(hrs);
        }
    }
    // What is being worked first, then what is waiting, and inside each the
    // oldest first. Ties break on the identifier so a poll cannot shuffle
    // two equal rows under the cursor.
    for list in cycle_issues.values_mut() {
        list.sort_by(|a, b| {
            state_rank(&a.state)
                .cmp(&state_rank(&b.state))
                .then(b.age.total_cmp(&a.age))
                .then(a.ident.cmp(&b.ident))
        });
    }

    for (key, slot) in by_team.iter_mut() {
        let n = done
            .iter()
            .filter(|it| text(&it["team"], "key") == *key)
            .count();
        slot.insert("done".into(), n);
    }

    if let Ok(mut guard) = state.lock() {
        guard.states = states;
        guard.by_team = by_team;
        guard.projects = projects;
        guard.all_projects = all_projects;
        guard.proj_open = proj_open;
        guard.proj_state = proj_state;
        guard.proj_oldest = proj_oldest;
        guard.cycle_state = cycle_state;
        guard.cycle_issues = cycle_issues;
        guard.cycles = cycles;
        guard.created = created;
        guard.completed = completed;
        guard.lead = lead;
        guard.cycle_time = ctime;
        guard.quickest = quickest;
        guard.slowest = slowest;
        guard.oldest_open = oldest_open;
        guard.oldest_wip = oldest_wip;
        guard.window = days;
        guard.truncated = capped || cap2 || cap3 || cap4;
        guard.fetched = now();
        guard.err = if source == "config" {
            tc::config_token_warning().unwrap_or_default()
        } else {
            String::new()
        };
    }
    Ok(())
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
    /// until it clears - 4.94 - and it is used *only* where the tint is on, so
    /// an unselected row is exactly the colour it always was.
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
    new: String,
}

const GHOST: (u8, u8, u8) = (96, 106, 124);
const NEW_RGB: (u8, u8, u8) = (180, 160, 255);
const OK_RGB: (u8, u8, u8) = (90, 240, 160);

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
        new: tc::rgb(180, 160, 255),
    }
}

fn state_colour<'a>(state: &str, p: &'a Palette) -> &'a str {
    match state {
        "triage" => &p.bad,
        "backlog" => &p.dim,
        "unstarted" => &p.accent,
        _ => &p.warn,
    }
}

fn state_label(state: &str) -> &'static str {
    match state {
        "triage" => "triage",
        "backlog" => "backlog",
        "unstarted" => "todo",
        _ => "in progress",
    }
}

/// One cycle in full: how much work it holds, how much is done, and the
/// shape of both since it opened.
///
/// The board ranks cycles by churn and shows a bar. The two histories behind
/// that bar are the interesting part and there is no room for them in a row:
/// scope rising while completed stays flat is a cycle taking on work, and
/// the two converging is one closing. Nothing here is a new request - the
/// arrays arrive with the cycle.
#[allow(clippy::too_many_arguments)]
fn cycle_detail(
    c: &serde_json::Value,
    states: &HashMap<String, usize>,
    issues: &[Issue],
    pick: usize,
    w: usize,
    h: usize,
    p: &Palette,
) -> (Vec<String>, Option<usize>) {
    let team = text(&c["team"], "name");
    // Linear cycles are often unnamed - the board falls back to their
    // number and so does this, or the title reads " · TEAM" with a leading
    // separator hanging off nothing.
    let named = match text(c, "name") {
        n if n.is_empty() => format!("Cycle {}", tidy(c["number"].as_f64().unwrap_or(0.0))),
        n => n,
    };
    let title = if team.is_empty() { named } else { format!("{} · {}", named, team) };
    let mut rows = vec![tc::title(&title, w, &p.accent)];
    let label_w = 18usize;
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

    let series = |key: &str| -> Vec<f64> {
        c[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_f64())
            .collect()
    };
    let scope = series("scopeHistory");
    let done = series("completedScopeHistory");
    let issue_count = series("issueCountHistory");
    let issues_closed = series("completedIssueCountHistory");

    let at = |v: &[f64]| v.last().copied().unwrap_or(0.0);
    let pct = if at(&scope) > 0.0 {
        100.0 * at(&done) / at(&scope)
    } else {
        0.0
    };
    field(
        "progress",
        format!("{:.0}%", pct),
        tc::meter(pct / 100.0, w.saturating_sub(label_w + 22).clamp(6, 24)),
        if pct >= 80.0 { p.ok.as_str() } else { p.txt.as_str() },
    );
    field(
        "scope",
        format!("{:.0}", at(&scope)),
        format!("{:.0} done, {:.0} left", at(&done), (at(&scope) - at(&done)).max(0.0)),
        p.txt.as_str(),
    );
    if !issue_count.is_empty() {
        field(
            "issues",
            format!("{:.0}", at(&issue_count)),
            format!("{:.0} closed", at(&issues_closed)),
            p.dim.as_str(),
        );
    }
    // Scope that appeared after the cycle opened. The number people mean
    // when they say a cycle "grew".
    let added = at(&scope) - scope.first().copied().unwrap_or(0.0);
    if added.abs() > 0.001 {
        field(
            "scope added",
            format!("{:+.0}", added),
            "since it opened".into(),
            if added > 0.0 { p.warn.as_str() } else { p.ok.as_str() },
        );
    }
    let (moved, left) = churn(c);
    field(
        "ends in",
        if left >= 999 { "—".into() } else { format!("{}d", left) },
        text(c, "endsAt").chars().take(10).collect::<String>(),
        p.dim.as_str(),
    );
    // +0.0 formats as "-0" when the sum is a hair below zero, which reads
    // as a negative churn and is not one.
    field(
        "churn",
        format!("{:.0}", if moved.abs() < 0.05 { 0.0 } else { moved }),
        "points moved lately".into(),
        p.dim.as_str(),
    );
    // What is left against what the cycle has been closing per day. The
    // question a burn-up is usually read to answer, stated.
    let days = done.len().max(1) as f64;
    let rate = at(&done) / days;
    if rate > 0.0 {
        let remaining = (at(&scope) - at(&done)).max(0.0);
        field(
            "at this rate",
            format!("{:.0}d", remaining / rate),
            if left < 999 && remaining / rate > left as f64 {
                "longer than the cycle has".into()
            } else {
                "to clear what is left".into()
            },
            if left < 999 && remaining / rate > left as f64 {
                p.warn.as_str()
            } else {
                p.ok.as_str()
            },
        );
    }

    // Scope above the line, completed below it, on one scale.
    if h.saturating_sub(rows.len()) >= 10 && scope.len() > 1 {
        let hi = scope.iter().chain(done.iter()).cloned().fold(1.0f64, f64::max);
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── BURN-UP ── ".into()),
                (p.dim.as_str(), format!("{} days · ", scope.len())),
                (p.txt.as_str(), "▲ scope".into()),
                (p.dim.as_str(), " · ".into()),
                (p.ok.as_str(), "▼ completed".into()),
            ],
            w - 1,
        ));
        let cols = w.saturating_sub(3).max(10);
        let fit = |v: &[f64]| -> Vec<f64> {
            if v.len() >= cols {
                v[v.len() - cols..].to_vec()
            } else {
                v.to_vec()
            }
        };
        for line in tc::vbars(
            &fit(&scope).iter().map(|v| (*v, p.txt.clone())).collect::<Vec<_>>(),
            3,
            hi,
        ) {
            let mut parts: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for (colour, ch) in &line {
                parts.push((colour.as_str(), ch.clone()));
            }
            rows.push(tc::seg(&parts, w - 1));
        }
        rows.push(tc::seg(
            &[(tc::RST, " ".into()), (p.grid.as_str(), "─".repeat(fit(&scope).len()))],
            w - 1,
        ));
        for line in tc::vbars_down(
            &fit(&done).iter().map(|v| (*v, p.ok.clone())).collect::<Vec<_>>(),
            3,
            hi,
        ) {
            let mut parts: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for (colour, ch) in &line {
                parts.push((colour.as_str(), ch.clone()));
            }
            rows.push(tc::seg(&parts, w - 1));
        }
    }
    // What is open in it right now, by state. The burn-up above is where
    // the cycle has been; this is where it is.
    let open: usize = states.values().sum();
    if open > 0 {
        let legend: Vec<(&str, usize, &str)> = STATE_ORDER
            .iter()
            .map(|st| (state_label(st), states.get(*st).copied().unwrap_or(0), state_colour(st, p)))
            .filter(|x| x.1 > 0)
            .collect();
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── OPEN BY STATE ── ".into()),
                (p.dim.as_str(), plural(open, "issue")),
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
        let mut legend_row: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
        for (label, count, colour) in &legend {
            legend_row.push((colour, "▇ ".into()));
            legend_row.push((p.txt.as_str(), (*label).into()));
            legend_row.push((
                p.dim.as_str(),
                format!(" {} ({:.0}%)   ", count, 100.0 * *count as f64 / open as f64),
            ));
        }
        rows.push(tc::seg(&legend_row, w - 1));
    }

    // The issues themselves. Only the open ones are here, because the open
    // ones are all this widget ever fetches - so the heading says how many
    // are closed rather than letting a count of one read as a cycle of one.
    let closed = (at(&issues_closed) as usize).min(at(&issue_count) as usize);
    rows.push(String::new());
    rows.push(tc::seg(
        &[
            (p.lbl.as_str(), " ── OPEN IN THIS CYCLE ── ".into()),
            (
                p.dim.as_str(),
                if closed > 0 {
                    format!("{} · {} closed, not listed", issues.len(), closed)
                } else {
                    format!("{}", issues.len())
                },
            ),
        ],
        w - 1,
    ));
    if issues.is_empty() {
        rows.push(tc::seg(
            &[(
                p.dim.as_str(),
                if closed > 0 {
                    "  nothing open in it - every issue is closed".into()
                } else {
                    "  nothing open in it".to_string()
                },
            )],
            w - 1,
        ));
        return (rows, None);
    }

    let pick = pick.min(issues.len() - 1);
    let mut cursor = None;
    let widest = |xs: &mut dyn Iterator<Item = usize>| xs.max().unwrap_or(0);
    let ident_w = widest(&mut issues.iter().map(|i| i.ident.chars().count())).max(6);
    let state_w = widest(&mut issues.iter().map(|i| state_label(&i.state).chars().count())).max(4);
    // Identifier, state, age and points are fixed; the title takes the rest,
    // and is the one thing here long enough to need it.
    let head = 2 + ident_w + 2 + state_w + 2 + 6 + 2 + 5 + 2;
    let title_w = (w - 1).saturating_sub(head).max(10);
    for (i, it) in issues.iter().enumerate() {
        let here = i == pick;
        if here {
            cursor = Some(rows.len());
        }
        // A title long enough to overrun the column carries on underneath
        // it rather than being cut. An issue titled "Verify fares · Sun
        // Ferry Services · Peng Chau" and one cut to the same forty
        // characters are two different issues on screen.
        let lines = wrap(&it.title, title_w);
        let (first, rest) = lines.split_first().map(|(a, b)| (a.clone(), b)).unwrap_or_default();
        rows.push(tc::seg(
            &[
                (
                    if here { p.accent.as_str() } else { p.dim.as_str() },
                    format!("{} {}", if here { "▸" } else { " " }, tc::pad(&it.ident, ident_w)),
                ),
                (
                    state_colour(&it.state, p),
                    format!("  {}", tc::pad(state_label(&it.state), state_w)),
                ),
                (
                    if here { p.accent.as_str() } else { p.txt.as_str() },
                    format!("  {}", tc::pad(&first, title_w)),
                ),
                (p.dim.as_str(), format!("  {:>6}", dur(Some(it.age)))),
                // Nothing rather than "0 pts" when nobody has pointed it.
                (
                    p.dim.as_str(),
                    match it.points {
                        Some(n) if n > 0.0 => format!(" {:>4}", format!("{}p", tidy(n))),
                        _ => "     ".to_string(),
                    },
                ),
            ],
            w - 1,
        ));
        for line in rest {
            rows.push(tc::seg(
                &[(
                    if here { p.accent.as_str() } else { p.txt.as_str() },
                    format!("{}{}", " ".repeat(2 + ident_w + 2 + state_w + 2), line),
                )],
                w - 1,
            ));
        }
    }
    (rows, cursor)
}

/// One team in full: what it is holding, in the states it is holding it.
#[allow(clippy::too_many_arguments)]
fn team_detail(
    key: &str,
    name: &str,
    counts: &HashMap<String, usize>,
    projects: &[Proj],
    opens: &HashMap<String, usize>,
    pick: usize,
    window: i64,
    w: usize,
    h: usize,
    p: &Palette,
) -> (Vec<String>, Option<usize>) {
    let mut rows = vec![tc::title(&format!("{} · {}", key, name), w, &p.accent)];
    let label_w = 18usize;
    let get = |k: &str| counts.get(k).copied().unwrap_or(0);
    let open = get("open");
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
    field("open", open.to_string(), "issues not closed".into(), p.txt.as_str());
    field(
        "done",
        get("done").to_string(),
        format!("in the last {}d", window),
        p.ok.as_str(),
    );
    // Triage is the one worth calling out: it is work nobody has looked at,
    // and a team can hold hundreds of it while looking busy everywhere else.
    let triage = get("triage");
    if triage > 0 {
        field(
            "in triage",
            triage.to_string(),
            if open > 0 {
                format!("{:.0}% of open, unlooked at", 100.0 * triage as f64 / open as f64)
            } else {
                String::new()
            },
            p.bad.as_str(),
        );
    }
    field("in progress", get("started").to_string(), String::new(), p.warn.as_str());

    if open > 0 && h.saturating_sub(rows.len()) >= 4 {
        let legend: Vec<(&str, usize, &str)> = [
            (state_label("triage"), triage, p.bad.as_str()),
            (state_label("backlog"), get("backlog"), p.dim.as_str()),
            (state_label("unstarted"), get("unstarted"), p.txt.as_str()),
            (state_label("started"), get("started"), p.warn.as_str()),
        ]
        .into_iter()
        .filter(|x| x.1 > 0)
        .collect();
        if !legend.is_empty() {
            rows.push(String::new());
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── OPEN BY STATE ── ".into()),
                    (p.dim.as_str(), plural(open, "issue")),
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
            let mut legend_row: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for (label, count, colour) in &legend {
                legend_row.push((colour, "▇ ".into()));
                legend_row.push((p.txt.as_str(), (*label).into()));
                legend_row.push((
                    p.dim.as_str(),
                    format!(" {} ({:.0}%)   ", count, 100.0 * *count as f64 / open as f64),
                ));
            }
            rows.push(tc::seg(&legend_row, w - 1));
        }
    }

    rows.push(String::new());
    // The issues in no project. Said out loud because the per-project counts
    // below will not add up to the team's open count without it, and a
    // column of numbers that does not reconcile reads as a bug.
    let loose = opens.get("").copied().unwrap_or(0);
    rows.push(tc::seg(
        &[
            (p.lbl.as_str(), " ── PROJECTS ── ".into()),
            (
                p.dim.as_str(),
                if projects.is_empty() {
                    "none".to_string()
                } else if loose > 0 {
                    format!("{} · {} open in no project", projects.len(), loose)
                } else {
                    format!("{}", projects.len())
                },
            ),
        ],
        w - 1,
    ));
    if projects.is_empty() {
        rows.push(tc::seg(
            &[(p.dim.as_str(), "  this team owns no projects".into())],
            w - 1,
        ));
        return (rows, None);
    }

    // Every aside first, because the columns are sized to what is in them:
    // names and statuses take the width they need and the meter takes what
    // is left, so a wider pane draws a longer bar rather than a margin. No
    // column is capped, because capping one truncates a project's name and
    // a half-written name is a name for something else.
    //
    // Only what a project actually has goes in its aside: one with no open
    // issues left says nothing rather than "0 open", and one with no target
    // date says nothing rather than an em dash.
    let asides: Vec<Vec<String>> = projects
        .iter()
        .map(|q| {
            let mut parts = Vec::new();
            let open = opens.get(&q.id).copied().unwrap_or(0);
            if open > 0 {
                parts.push(format!("{} open", open));
            }
            if !q.target.is_empty() {
                parts.push(format!("due {}", q.target));
            }
            if !q.lead.is_empty() {
                parts.push(q.lead.clone());
            }
            parts
        })
        .collect();
    let widest = |xs: &mut dyn Iterator<Item = usize>| xs.max().unwrap_or(0);
    let name_w = widest(&mut projects.iter().map(|q| q.name.chars().count())).max(8);
    let label_w = widest(&mut projects.iter().map(|q| q.label.chars().count())).max(4);
    let full = widest(&mut asides.iter().map(|a| joined(a).chars().count()));
    // Everything on the row except the bar and the aside, the two that give.
    // The marker sits in the left margin the same way the board's own two
    // panes carry it, so the two-space indent is what it displaces.
    let head = 2 + name_w + 2 + label_w + 2 + 5 + 2;
    let bar_w = (w - 1).saturating_sub(head + full).clamp(6, 40);
    let room = (w - 1).saturating_sub(head + bar_w);
    let pick = pick.min(projects.len() - 1);
    let mut cursor = None;
    for (i, (q, parts)) in projects.iter().zip(&asides).enumerate() {
        let colour = match q.kind.as_str() {
            "started" => p.warn.as_str(),
            "completed" => p.ok.as_str(),
            "canceled" => p.dim.as_str(),
            _ => p.txt.as_str(),
        };
        // Drop whole facts off the end rather than let the pane cut one in
        // half: "due 2024-07-26 · William L" names nobody.
        let mut keep = parts.len();
        while keep > 0 && joined(&parts[..keep]).chars().count() > room {
            keep -= 1;
        }
        let here = i == pick;
        if here {
            cursor = Some(rows.len());
        }
        rows.push(tc::seg(
            &[
                (
                    if here { p.accent.as_str() } else { p.txt.as_str() },
                    format!("{} {}", if here { "▸" } else { " " }, tc::pad(&q.name, name_w)),
                ),
                (colour, format!("  {}", tc::pad(&q.label, label_w))),
                (colour, format!("  {}", tc::meter(q.progress, bar_w))),
                (p.txt.as_str(), format!(" {:>3.0}%", 100.0 * q.progress)),
                (p.dim.as_str(), format!("  {}", joined(&parts[..keep]))),
            ],
            w - 1,
        ));
    }
    (rows, cursor)
}

/// One project in full.
///
/// Two sources meet here and neither could answer alone. `held` is the
/// project's own record, fetched when this screen opened: its burn-up, its
/// milestones, who is on it. `states` and `oldest` come from the board's
/// own pass over every open issue, which is why they cost nothing and stay
/// current while the screen is up.
///
/// `q` carries what the list already knew, so the screen has a name, a
/// status and a progress figure on the frame it opens - not one request
/// later.
#[allow(clippy::too_many_arguments)]
fn project_detail(
    q: &Proj,
    team: &str,
    held: Option<&serde_json::Value>,
    states: &HashMap<String, usize>,
    oldest: Option<&(f64, String)>,
    w: usize,
    p: &Palette,
) -> Vec<String> {
    let title = if team.is_empty() {
        q.name.clone()
    } else {
        format!("{} · {}", q.name, team)
    };
    let mut rows = vec![tc::title(&title, w, &p.accent)];
    let label_w = 18usize;
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

    // Linear's own progress, the same figure the team's list shows. Derived
    // here it would disagree with the line the reader just came from.
    field(
        "progress",
        format!("{:.0}%", 100.0 * q.progress),
        tc::meter(q.progress, w.saturating_sub(label_w + 22).clamp(6, 24)),
        if q.progress >= 0.8 { p.ok.as_str() } else { p.txt.as_str() },
    );
    field(
        "status",
        String::new(),
        q.label.clone(),
        p.txt.as_str(),
    );

    let series = |v: &serde_json::Value, key: &str| -> Vec<f64> {
        v[key].as_array().into_iter().flatten().filter_map(|x| x.as_f64()).collect()
    };
    let at = |v: &[f64]| v.last().copied().unwrap_or(0.0);
    if let Some(v) = held.filter(|v| v["_error"].is_null() && !v.is_null()) {
        let scope = series(v, "scopeHistory");
        let done = series(v, "completedScopeHistory");
        let issues = series(v, "issueCountHistory");
        let issues_done = series(v, "completedIssueCountHistory");
        if at(&scope) > 0.0 {
            // Named in points because the percentage above is Linear's
            // own weighting and agrees with neither this nor the issue
            // count. Three different measures unlabelled read as one
            // measure got wrong three times.
            field(
                "scope in points",
                format!("{:.0}", at(&scope)),
                format!("{:.0} done, {:.0} left", at(&done), (at(&scope) - at(&done)).max(0.0)),
                p.txt.as_str(),
            );
        }
        if at(&issues) > 0.0 {
            field(
                "issues",
                format!("{:.0}", at(&issues)),
                format!("{:.0} closed", at(&issues_done)),
                p.dim.as_str(),
            );
        }
        // Scope that arrived after the project opened - the number people
        // mean when they say a project "grew".
        let added = at(&scope) - scope.first().copied().unwrap_or(0.0);
        if added.abs() > 0.05 {
            field(
                "scope added",
                format!("{:+.0}", added),
                "since it started".into(),
                if added > 0.0 { p.warn.as_str() } else { p.ok.as_str() },
            );
        }
    }

    let record = held.unwrap_or(&serde_json::Value::Null);
    let started = day(&text(record, "startedAt"));
    if !started.is_empty() {
        field("started", String::new(), started, p.dim.as_str());
    }
    let finished = day(&text(record, "completedAt"));
    if !finished.is_empty() {
        field("completed", String::new(), finished.clone(), p.ok.as_str());
    }
    // A finished project is not late. It was shown as "overdue by 760d"
    // because the target date is still in the past and nothing was asking
    // whether the work had since landed.
    let done_with = !finished.is_empty() || q.kind == "completed" || q.kind == "canceled";
    if !q.target.is_empty() && done_with {
        field("target was", String::new(), q.target.clone(), p.dim.as_str());
    } else if !q.target.is_empty() {
        // Whether the date is still ahead, said in days rather than left to
        // the reader to work out from today's.
        let left = parse(&format!("{}T00:00:00", q.target))
            .and_then(|d| hours_since(Some(Utc::now().naive_utc()), Some(d)))
            .map(|h| (h / 24.0).ceil() as i64);
        // The label carries the direction so the value stays short enough
        // for the column, the way the cycle screen's "ends in" does.
        let (label, word, colour) = match left {
            Some(d) if d < 0 => ("overdue by", format!("{}d", -d), p.bad.as_str()),
            Some(d) => ("target in", format!("{}d", d), p.txt.as_str()),
            None => ("target", String::new(), p.dim.as_str()),
        };
        field(label, word, q.target.clone(), colour);
    }
    if !q.lead.is_empty() {
        field("lead", String::new(), q.lead.clone(), p.txt.as_str());
    }
    if let Some(v) = held.filter(|v| v["_error"].is_null() && !v.is_null()) {
        let names: Vec<String> = v["members"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| text(m, "name"))
            .filter(|n| !n.is_empty())
            .collect();
        if !names.is_empty() {
            field(
                "members",
                names.len().to_string(),
                names.join(", "),
                p.dim.as_str(),
            );
        }
        let inits: Vec<String> = v["initiatives"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| text(i, "name"))
            .filter(|n| !n.is_empty())
            .collect();
        if !inits.is_empty() {
            field("initiative", String::new(), inits.join(" · "), p.dim.as_str());
        }
    }
    if let Some((age, ident)) = oldest {
        field("oldest open", dur(Some(*age)), ident.clone(), p.warn.as_str());
    }

    // What is open in it, by state, across every team that shares it.
    let open: usize = states.values().sum();
    if open > 0 {
        let legend: Vec<(&str, usize, &str)> = STATE_ORDER
            .iter()
            .map(|st| (state_label(st), states.get(*st).copied().unwrap_or(0), state_colour(st, p)))
            .filter(|x| x.1 > 0)
            .collect();
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── OPEN BY STATE ── ".into()),
                (p.dim.as_str(), plural(open, "issue")),
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
        let mut legend_row: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
        for (label, count, colour) in &legend {
            legend_row.push((colour, "▇ ".into()));
            legend_row.push((p.txt.as_str(), (*label).into()));
            legend_row.push((
                p.dim.as_str(),
                format!(" {} ({:.0}%)   ", count, 100.0 * *count as f64 / open as f64),
            ));
        }
        rows.push(tc::seg(&legend_row, w - 1));
    }

    match held {
        None => {
            rows.push(String::new());
            rows.push(tc::seg(&[(p.dim.as_str(), "  asking Linear for the rest...".into())], w - 1));
            return rows;
        }
        Some(v) if !v["_error"].is_null() => {
            rows.push(String::new());
            rows.push(tc::seg(
                &[
                    (p.bad.as_str(), "  could not read the project: ".into()),
                    (p.dim.as_str(), text(v, "_error")),
                ],
                w - 1,
            ));
            return rows;
        }
        Some(v) if v.is_null() => {
            rows.push(String::new());
            rows.push(tc::seg(&[(p.bad.as_str(), "  Linear returned no such project".into())], w - 1));
            return rows;
        }
        _ => {}
    }
    let v = held.unwrap_or(&serde_json::Value::Null);

    // Milestones, in the order they fall due. Linear returns them in no
    // order at all, and a list of dates out of order reads as noise.
    let mut stones: Vec<(String, String, f64)> = v["projectMilestones"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| {
            (
                text(m, "name"),
                text(m, "targetDate"),
                // Milestones report progress out of a hundred where the
                // project reports it out of one. Fed straight to a meter
                // every one of them would read full.
                m["progress"].as_f64().unwrap_or(0.0) / 100.0,
            )
        })
        .collect();
    // Undated last: they have no place in a sequence of dates.
    stones.sort_by(|a, b| {
        a.1.is_empty().cmp(&b.1.is_empty()).then(a.1.cmp(&b.1)).then(a.0.cmp(&b.0))
    });
    if !stones.is_empty() {
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── MILESTONES ── ".into()),
                (p.dim.as_str(), format!("{}", stones.len())),
            ],
            w - 1,
        ));
        let name_w = stones.iter().map(|(n, _, _)| n.chars().count()).max().unwrap_or(8).max(8);
        let bar_w = (w - 1).saturating_sub(2 + name_w + 2 + 5 + 2 + 10).clamp(6, 30);
        for (name, date, frac) in &stones {
            let colour = if *frac >= 1.0 { p.ok.as_str() } else { p.txt.as_str() };
            rows.push(tc::seg(
                &[
                    (p.txt.as_str(), format!("  {}", tc::pad(name, name_w))),
                    (colour, format!("  {}", tc::meter(*frac, bar_w))),
                    (p.txt.as_str(), format!(" {:>3.0}%", 100.0 * frac)),
                    (p.dim.as_str(), format!("  {}", date)),
                ],
                w - 1,
            ));
        }
    }

    let about = text(v, "description");
    if !about.trim().is_empty() {
        rows.push(String::new());
        rows.push(tc::seg(&[(p.lbl.as_str(), " ── WHAT IT IS FOR ── ".into())], w - 1));
        // Wrapped whole, never cut: this screen scrolls, so there is no
        // width to buy by dropping the end of a sentence.
        for line in wrap(&about, w.saturating_sub(4)) {
            rows.push(tc::seg(&[(p.txt.as_str(), format!("  {}", line))], w - 1));
        }
    }
    rows
}

/// How much a cycle has moved lately, for ranking.
///
/// The burndown arrays already say where the action is: day-over-day
/// movement in completed scope and in scope itself, summed over the tail.
/// A cycle nothing has touched in a week is not interesting however close
/// its deadline, and an empty one scores zero and sinks without a special
/// case. Deadline breaks ties.
fn churn(c: &serde_json::Value) -> (f64, i64) {
    let mut moved = 0.0;
    for series in ["completedScopeHistory", "scopeHistory"] {
        let all: Vec<f64> = c[series]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_f64())
            .collect();
        let tail = &all[all.len().saturating_sub(CHURN_DAYS)..];
        moved += tail.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>();
    }
    let left = match parse(&text(c, "endsAt")) {
        Some(ends) => (ends - Utc::now().naive_utc()).num_days(),
        None => 999,
    };
    (-moved, left)
}

fn last_of(c: &serde_json::Value, key: &str) -> f64 {
    c[key]
        .as_array()
        .and_then(|a| a.last())
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

fn first_of(c: &serde_json::Value, key: &str) -> f64 {
    c[key]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

/// A number as the Python's %g writes it: no trailing zeros on a whole one.
fn tidy(v: f64) -> String {
    if v.fract().abs() < 1e-9 {
        format!("{}", v as i64)
    } else {
        format!("{}", v)
    }
}

fn main() {
    tc::maybe_help(include_str!("linear_help.txt"));
    let cfg = tc::load_config("linear");
    let mut refresh = tc::cfg_f64(&cfg, "refresh", 120.0);
    let exclude: Vec<String> = tc::cfg_strings(&cfg, "exclude_teams", &[]);
    let start_window = tc::cfg_f64(&cfg, "window_days", 14.0) as i64;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut keep: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--refresh" if i + 1 < args.len() => {
                refresh = args[i + 1].parse().unwrap_or(120.0);
                i += 2;
            }
            other if !other.starts_with('-') => {
                keep.push(other.to_uppercase());
                i += 1;
            }
            _ => i += 1,
        }
    }

    let absent = tc::missing(&["curl"]);
    if !absent.is_empty() {
        tc::cannot_start(
            "linear ops",
            &absent,
            &[
                "Everything here comes from Linear's GraphQL API, and curl is",
                "how this reaches it - the same way the other widgets reach",
                "ss, ping and tailscale.",
                "",
                "The key is passed to curl on its standard input rather than",
                "in its arguments, because /proc/<pid>/cmdline is readable by",
                "every user on the machine.",
            ],
            "apt install curl",
        );
        return;
    }

    let p = palette();
    let state = Arc::new(Mutex::new(State {
        window: start_window,
        ..Default::default()
    }));
    let quota = Arc::new(Mutex::new(Quota::default()));
    let days = Arc::new(Mutex::new(start_window));
    let wake = Arc::new((Mutex::new(false), Condvar::new()));

    let (tok, source) = token(&cfg);
    // The poller takes ownership of the key; the project screen fetches on
    // its own thread and needs one too.
    let ui_token = tok.clone();
    let env_name = {
        let name = tc::cfg_str(&cfg, "token_env", "LINEAR_API_KEY");
        if name.is_empty() { "LINEAR_API_KEY".to_string() } else { name }
    };
    let poller = Arc::clone(&state);
    let poller_wake = Arc::clone(&wake);
    let poller_days = Arc::clone(&days);
    let poller_quota = Arc::clone(&quota);
    std::thread::spawn(move || loop {
        if tok.is_empty() {
            if let Ok(mut guard) = poller.lock() {
                guard.err = format!(
                    "no key: set linear.token in config.json or ${}",
                    env_name
                );
            }
        } else {
            let want = poller_days.lock().map(|g| *g).unwrap_or(14);
            if let Err(said) = one_pass(
                &tok,
                source,
                want,
                &keep,
                &exclude,
                &poller,
                &poller_quota,
            ) {
                if let Ok(mut guard) = poller.lock() {
                    guard.err = said;
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
    // Two sections take the arrows, so they need to know which one they
    // are in. Tab moves the focus; the focused heading says so.
    //
    // None is a real state, not a missing value: the board opens with no
    // cursor anywhere, because it is a thing to read before it is a thing
    // to work, and a cursor sitting somewhere you did not put it is a
    // question rather than an answer. It also used to open on the cycles
    // pane while the footer said the arrows scrolled, which was wrong
    // twice over.
    // In the order they are drawn, which is the order the arrows walk them.
    let (cycles_pane, teams_pane, projects_pane) = (0usize, 1usize, 2usize);
    let mut focus: Option<usize> = None;
    let mut sel = [0usize; 3];
    // How long each pane was when it was last drawn. The keys are read
    // before the frame is built, so walking off the end of a pane has to be
    // judged against the length it had a moment ago - which is the length
    // the reader is looking at.
    let mut pane_len = [0usize; 3];
    // Which pane's selection is open on a screen of its own, and how far
    // down it is scrolled.
    let (mut detail, mut dscroll): (Option<usize>, usize) = (None, 0);
    // Which project is under the cursor on a team's screen, and which one
    // is open a level deeper.
    //
    // The deeper one is held by id, not by index: the list re-sorts on
    // every poll - by status, then progress - so a refresh while the screen
    // is up would otherwise swap out the project being read without
    // touching the cursor.
    let mut pick = 0usize;
    let mut deep: Option<String> = None;
    let mut pscroll = 0usize;
    let held: Arc<Mutex<HashMap<String, (serde_json::Value, f64)>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let asking: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let ui_tok = ui_token.clone();
    let ui_quota = Arc::clone(&quota);
    // How long the open screen's own list was when it was last drawn, and
    // what the cursor was on - read by the keys, which run before the frame
    // that answers them is built. A team's screen lists its projects and a
    // cycle's lists its open issues; only one is ever up, so one length and
    // one cursor serve both.
    let mut list_len = 0usize;
    let mut open_project: Option<String> = None;
    let mut copy_url: Option<(String, String)> = None;
    // What the last copy did, and when - shown for a few seconds on the
    // bottom row of whichever screen asked for it.
    let (mut note, mut note_at) = (String::new(), 0.0f64);
    // How far down the board itself is scrolled, and how tall it was when
    // it was last drawn - the keys run before the frame that answers them.
    let mut board = 0usize;
    let mut board_len = 0usize;
    // Which project the board's own list has under its cursor.
    let mut board_project: Option<String> = None;
    let mut tick = 0usize;
    let mut settle_t = 0usize;
    let mut settle_from: Option<(Vec<f64>, Vec<f64>)> = None;

    loop {
        tick += 1;
        for key in keyboard.poll() {
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "r" | "R" => {
                    if let Ok(mut g) = held.lock() {
                        g.clear();
                    }
                    let (lock, cond) = &*wake;
                    if let Ok(mut asked) = lock.lock() {
                        *asked = true;
                        cond.notify_all();
                    }
                }
                "w" | "W" => {
                    if let Ok(mut want) = days.lock() {
                        *want = tc::cycle(WINDOWS, *want);
                    }
                    let (lock, cond) = &*wake;
                    if let Ok(mut asked) = lock.lock() {
                        *asked = true;
                        cond.notify_all();
                    }
                }
                // The rule every widget here with focusable sections
                // follows. tab moves to the next pane and, from the last
                // one, back to no focus at all - it used to toggle between
                // the two for ever, so there was no way to put the cursor
                // away. Empty panes are stepped over: focusing one leaves
                // the arrows moving an index nothing is drawn from, which
                // is a key that does nothing and says nothing.
                "tab" => {
                    focus = tc::next_section(focus, &pane_len);
                    // Back to no focus means back to reading the board,
                    // and the window stays where the cursor left it rather
                    // than jumping to the top.
                    pick = 0;
                }
                // Enter opens whichever pane has the cursor. Without a
                // focused pane there is nothing selected to open, which is
                // the same rule the board's own cursor follows.
                // A team's screen opens one of its projects; the board
                // opens whichever pane has the cursor. Without a focused
                // pane there is nothing selected to open, which is the
                // same rule the board's own cursor follows.
                "right" | "enter" if detail == Some(teams_pane) && deep.is_none() => {
                    if let Some(id) = open_project.clone() {
                        deep = Some(id);
                        pscroll = 0;
                    }
                }
                "right" | "enter" if detail.is_some() => {}
                "right" | "enter" => {
                    if focus.is_some() {
                        detail = focus;
                        dscroll = 0;
                        pick = 0;
                        // The board's project list has no screen of its
                        // own between the board and a project.
                        if focus == Some(projects_pane) {
                            deep = board_project.clone();
                            pscroll = 0;
                            if deep.is_none() {
                                detail = None;
                            }
                        }
                    }
                }
                // Back one level at a time: out of a project to the team
                // that owns it, and only then to the board.
                "left" | "esc" if deep.is_some() => {
                    deep = None;
                    // A project opened from the board's own list has no
                    // team screen behind it to come back to.
                    if detail == Some(projects_pane) {
                        detail = None;
                    }
                }
                "left" | "esc" if detail.is_some() => detail = None,
                // A team's screen hands the arrows to its project list and
                // scrolls itself to follow; every other screen has nothing
                // to select, so they scroll outright.
                // The issue under the cursor, by its address. An issue is
                // a leaf here - there is no screen below it - so what it
                // offers is somewhere to go and read it.
                "c" | "C" if copy_url.is_some() => {
                    let (url, ident) = copy_url.clone().unwrap_or_default();
                    (note, note_at) = if url.is_empty() {
                        ("that issue has no url".to_string(), now())
                    } else if tc::clipboard(&url) {
                        (format!("copied {}", ident), now())
                    } else {
                        // Said out loud: a copy that silently did nothing
                        // is indistinguishable from one that worked until
                        // the paste comes up empty.
                        ("could not reach the clipboard".to_string(), now())
                    };
                }
                "up" | "down" if detail.is_some() && deep.is_none() && list_len > 0 => {
                    if key == "down" {
                        pick = pick.saturating_add(1).min(list_len.saturating_sub(1));
                    } else {
                        pick = pick.saturating_sub(1);
                    }
                }
                "pgup" | "pgdn" if detail.is_some() && deep.is_none() && list_len > 0 => {
                    let page = tc::size().1.saturating_sub(3).max(1);
                    pick = if key == "pgdn" {
                        pick.saturating_add(page).min(list_len.saturating_sub(1))
                    } else {
                        pick.saturating_sub(page)
                    };
                }
                // Walking off either end of a pane leaves it. There is no
                // screen scroll here to hand the arrows to - both panes
                // window themselves to fit - so from nothing focused they
                // step back in at the near end, the same ring latency and
                // link use.
                "up" | "down" if detail.is_some() => {
                    let at = if deep.is_some() { &mut pscroll } else { &mut dscroll };
                    if key == "down" {
                        *at = at.saturating_add(1);
                    } else {
                        *at = at.saturating_sub(1);
                    }
                }
                "pgup" | "pgdn" if detail.is_some() => {
                    let page = tc::size().1.saturating_sub(3).max(1);
                    let at = if deep.is_some() { &mut pscroll } else { &mut dscroll };
                    *at = if key == "pgdn" {
                        at.saturating_add(page)
                    } else {
                        at.saturating_sub(page)
                    };
                }
                "up" | "down" => {
                    let down = key == "down";
                    pick = 0;
                    match focus {
                        Some(here) => {
                            focus = tc::step_across_sections(here, sel[here], &pane_len, down)
                                .map(|(pane, row)| {
                                    sel[pane] = row;
                                    pane
                                });
                        }
                        // Nothing focused: the arrows move the board. The
                        // board is taller than the pane and every section
                        // is drawn whole, so this is the only way to reach
                        // the bottom of it without picking a section first.
                        None => {
                            board = if down {
                                board.saturating_add(1)
                            } else {
                                board.saturating_sub(1)
                            };
                        }
                    }
                }
                "pgup" | "pgdn" => {
                    let page = tc::size().1.saturating_sub(3).max(1);
                    if focus.is_none() {
                        board = if key == "pgdn" {
                            board.saturating_add(page).min(board_len.saturating_sub(1))
                        } else {
                            board.saturating_sub(page)
                        };
                    } else if let Some(here) = focus {
                        // A page through a focused section, clamped to it -
                        // crossing out of a section is what the single
                        // arrows are for.
                        sel[here] = if key == "pgdn" {
                            sel[here].saturating_add(page).min(pane_len[here].saturating_sub(1))
                        } else {
                            sel[here].saturating_sub(page)
                        };
                    }
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let want = days.lock().map(|g| *g).unwrap_or(14);
        let s = match state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        // The counters describe the window they were fetched for, which is
        // not the one the key has just asked for.
        let stale = s.window != want;
        let left = quota.lock().map(|g| g.requests).unwrap_or(None);

        let mut rows = vec![tc::title("linear ops", w, &p.new)];
        // Where the focused section's cursor landed, so the board can be
        // scrolled to keep it on screen.
        let mut cursor: Option<usize> = None;
        let mut head = vec![
            (
                p.dim.as_str(),
                format!(" {} team{}", s.teams.len(), if s.teams.len() == 1 { "" } else { "s" }),
            ),
            (p.dim.as_str(), format!("   updated {} ago", ago(s.fetched))),
        ];
        if let Some(left) = left {
            head.push((
                if left > 500 { p.ok.as_str() } else { p.warn.as_str() },
                format!("   {} req left/hr", left),
            ));
        }
        rows.push(tc::seg(&head, w - 1));
        if !s.err.is_empty() {
            rows.push(tc::seg(&[(p.bad.as_str(), format!(" ! {}", s.err))], w - 1));
        }
        if s.teams.is_empty() {
            rows.push(tc::seg(&[(p.dim.as_str(), " collecting…".into())], w - 1));
            drop(s);
            while rows.len() < h.saturating_sub(1) {
                rows.push(String::new());
            }
            tc::draw(&rows, w, h);
            std::thread::sleep(Duration::from_millis(400));
            continue;
        }

        // How long work takes, across every team. It leads the board: it is
        // the one figure that says whether the machine is getting faster or
        // slower, and it is an aggregate rather than any one team's - which
        // the heading has to say, or it reads as whichever team is selected
        // below.
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── HOW LONG ── ".into()),
                (p.dim.as_str(), "all teams · ".into()),
                (
                    p.dim.as_str(),
                    if stale {
                        "counting…".to_string()
                    } else {
                        format!("median of {} completed in {}d", s.lead.len(), want)
                    },
                ),
            ],
            w - 1,
        ));
        let extreme = |label: &str, pair: &Extreme, colour: &str| -> (String, String, String) {
            if stale {
                return (label.into(), "···".into(), p.dim.clone());
            }
            match pair {
                None => (label.into(), "--".into(), p.dim.clone()),
                Some((hours, ident)) => (
                    label.into(),
                    format!("{} {}", if ident.is_empty() { "?" } else { ident }, dur(Some(*hours))),
                    colour.to_string(),
                ),
            }
        };
        let dimmed = |value: Option<f64>| -> (String, String) {
            if stale {
                ("···".into(), p.dim.clone())
            } else {
                (dur(value), p.txt.clone())
            }
        };
        let (lead_txt, lead_c) = dimmed(median(&s.lead));
        let (cycle_txt, cycle_c) = dimmed(median(&s.cycle_time));
        let cells: Vec<(String, String, String)> = vec![
            ("lead (created→completed)".into(), lead_txt, lead_c),
            ("cycle (started→completed)".into(), cycle_txt, cycle_c),
            extreme("quickest", &s.quickest, &p.ok),
            extreme("slowest", &s.slowest, &p.warn),
            extreme("oldest open", &s.oldest_open, &p.bad),
            extreme("oldest in progress", &s.oldest_wip, &p.warn),
        ];
        let label_w = cells.iter().map(|c| c.0.chars().count()).max().unwrap_or(8);
        // Two columns only when a value still gets room for the longest
        // thing it holds - an identifier and a duration. Cells are a fixed
        // width so a long value cannot push the next column out of line.
        let ncols = if (w - 2) / 2 >= label_w + 3 + 15 { 2 } else { 1 };
        let cw = (w - 2) / ncols;
        let val_w = cw.saturating_sub(label_w + 3).max(6);
        for chunk in cells.chunks(ncols) {
            let mut line: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for (label, value, colour) in chunk {
                line.push((p.dim.as_str(), format!(" {} ", tc::pad(label, label_w))));
                line.push((colour.as_str(), tc::pad(value, val_w)));
            }
            rows.push(tc::seg(&line, w - 1));
        }
        rows.push(String::new());

        let total_open: usize = STATE_ORDER
            .iter()
            .map(|st| s.states.get(*st).copied().unwrap_or(0))
            .sum();
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── OPEN ── ".into()),
                (p.new.as_str(), format!("{}", total_open)),
                (p.dim.as_str(), " issues open".into()),
                (p.dim.as_str(), "   (any age)".into()),
                (
                    p.warn.as_str(),
                    if s.truncated { "   truncated".into() } else { String::new() },
                ),
            ],
            w - 1,
        ));
        if total_open > 0 {
            let parts: Vec<(f64, String)> = STATE_ORDER
                .iter()
                .filter_map(|st| {
                    let n = s.states.get(*st).copied().unwrap_or(0);
                    if n == 0 {
                        return None;
                    }
                    Some((n as f64 / total_open as f64, state_colour(st, &p).to_string()))
                })
                .collect();
            let bar = tc::stacked_bar(&parts, w.saturating_sub(3).max(10));
            let mut line: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for (colour, txt) in &bar {
                line.push((colour.as_str(), txt.clone()));
            }
            rows.push(tc::seg(&line, w - 1));
            let mut key: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for st in STATE_ORDER {
                let n = s.states.get(*st).copied().unwrap_or(0);
                if n == 0 {
                    continue;
                }
                key.push((state_colour(st, &p), "▇ ".into()));
                key.push((p.txt.as_str(), state_label(st).into()));
                key.push((
                    p.dim.as_str(),
                    format!(" {} ({:.0}%)   ", n, 100.0 * n as f64 / total_open as f64),
                ));
            }
            rows.push(tc::seg(&key, w - 1));
        }

        rows.push(String::new());
        let mut ranked_cycles = s.cycles.clone();
        ranked_cycles.sort_by(|a, b| {
            let (am, al) = churn(a);
            let (bm, bl) = churn(b);
            am.total_cmp(&bm).then(al.cmp(&bl))
        });
        pane_len[cycles_pane] = ranked_cycles.len();
        if !ranked_cycles.is_empty() {
            sel[cycles_pane] = sel[cycles_pane].min(ranked_cycles.len() - 1);
        }
        let here_now = focus == Some(cycles_pane);
        rows.push(tc::seg(
            &[
                (
                    if here_now { p.accent.as_str() } else { p.lbl.as_str() },
                    " ── ACTIVE CYCLES ── ".into(),
                ),
                (p.dim.as_str(), format!("{} running", s.cycles.len())),
                (
                    if here_now { p.accent.as_str() } else { p.dim.as_str() },
                    heading_keys(cycles_pane, focus, &pane_len).to_string(),
                ),
            ],
            w - 1,
        ));
        if s.cycles.is_empty() {
            rows.push(tc::seg(
                &[(p.dim.as_str(), "  no cycle is running in any team".into())],
                w - 1,
            ));
        }
        for (ci, c) in ranked_cycles.iter().enumerate() {
            if here_now && ci == sel[cycles_pane] {
                cursor = Some(rows.len());
            }
            let scope = last_of(c, "scopeHistory");
            let done = last_of(c, "completedScopeHistory");
            let opened_at = first_of(c, "scopeHistory");
            let left_days = parse(&text(c, "endsAt"))
                .map(|ends| (ends - Utc::now().naive_utc()).num_days());
            let frac = if scope > 0.0 { done / scope } else { 0.0 };
            let name = format!(
                "{} {}",
                match text(&c["team"], "key") {
                    k if k.is_empty() => "?".to_string(),
                    k => k,
                },
                match text(c, "name") {
                    n if n.is_empty() => format!("Cycle {}", tidy(c["number"].as_f64().unwrap_or(0.0))),
                    n => n,
                }
            );
            let on = focus == Some(cycles_pane) && ci == sel[cycles_pane];
            let tint = if on { tc::bg(38, 56, 76) } else { String::new() };
            let c_of = |colour: &str| {
                let colour = if tint.is_empty() || colour != p.dim {
                    colour
                } else {
                    p.dim_lit.as_str()
                };
                format!("{}{}", tint, colour)
            };
            let hot = tc::heat(frac);
            let mut line = vec![
                (
                    c_of(if on { &p.accent } else { &p.txt }),
                    format!("{}{}", if on { "▸" } else { " " }, tc::pad(&name, 18)),
                ),
                (
                    c_of(&hot),
                    tc::meter(frac, (w.saturating_sub(54)).clamp(8, 28)),
                ),
                (
                    c_of(if scope > 0.0 { &hot } else { &p.dim }),
                    format!(
                        " {:>3}",
                        if scope > 0.0 {
                            format!("{:.0}%", frac * 100.0)
                        } else {
                            "--".into()
                        }
                    ),
                ),
                (
                    c_of(&p.dim),
                    if scope > 0.0 {
                        format!("  {}/{} pts", tidy(done), tidy(scope))
                    } else {
                        "  nothing scoped".into()
                    },
                ),
            ];
            if let Some(days_left) = left_days {
                line.push((
                    c_of(if days_left <= 2 { &p.warn } else { &p.dim }),
                    format!("  {}d left", days_left),
                ));
            }
            // Scope added after the cycle opened is the number that explains
            // a cycle working hard and still slipping.
            if scope > opened_at {
                line.push((c_of(&p.bad), format!("  +{} added", tidy(scope - opened_at))));
            }
            if on {
                line.push((tint.clone(), " ".repeat(w)));
            }
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
        }

        // Arrivals against departures.
        let today = Utc::now().date_naive();
        let mut days_list: Vec<String> = (0..want)
            .rev()
            .map(|n| (today - chrono::Duration::days(n)).format("%Y-%m-%d").to_string())
            .collect();
        let avail = w.saturating_sub(3).max(10);
        if days_list.len() > avail {
            days_list = days_list[days_list.len() - avail..].to_vec();
        }
        let slot = (avail / days_list.len()).max(1);
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
        let made_day: Vec<f64> = days_list
            .iter()
            .map(|d| s.created.get(d).copied().unwrap_or(0) as f64)
            .collect();
        let done_day: Vec<f64> = days_list
            .iter()
            .map(|d| s.completed.get(d).copied().unwrap_or(0) as f64)
            .collect();
        let (up, down) = (spread(&made_day), spread(&done_day));
        let chart_cols = up.len();
        let span_hi = up
            .iter()
            .chain(down.iter())
            .cloned()
            .fold(0.0f64, f64::max)
            .max(1.0);
        rows.push(String::new());
        if stale {
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── ISSUE FLOW ── ".into()),
                    (p.dim.as_str(), format!("counting {}d…", want)),
                ],
                w - 1,
            ));
        } else {
            let span = if days_list.len() < want as usize {
                format!("{}d of {}d", days_list.len(), want)
            } else {
                format!("{}d", days_list.len())
            };
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── ISSUE FLOW ── ".into()),
                    (p.dim.as_str(), format!("{} · ", span)),
                    (
                        p.new.as_str(),
                        format!("▲ {} created", made_day.iter().sum::<f64>() as i64),
                    ),
                    (p.dim.as_str(), " · ".into()),
                    (
                        p.ok.as_str(),
                        format!("▼ {} completed", done_day.iter().sum::<f64>() as i64),
                    ),
                    (p.dim.as_str(), format!("   peak {}/day", span_hi as i64)),
                ],
                w - 1,
            ));
        }
        // While the counters are for a window nobody asked for, the chart
        // dances rather than showing a number that is not the answer; when
        // the real one lands it eases in from where the dance left off.
        let (hu, hd, cu, cd) = if stale {
            let hu = spread(&tc::dance(days_list.len(), tick, 0.0));
            let hd = spread(&tc::dance(days_list.len(), tick, 2.1));
            settle_from = Some((hu.clone(), hd.clone()));
            settle_t = 0;
            (
                hu,
                hd,
                tc::mix(GHOST, NEW_RGB, 0.45),
                tc::mix(GHOST, OK_RGB, 0.45),
            )
        } else {
            let real_u: Vec<f64> = up.iter().map(|v| v / span_hi).collect();
            let real_d: Vec<f64> = down.iter().map(|v| v / span_hi).collect();
            match &settle_from {
                Some((fu, fd)) if settle_t < SETTLE_FRAMES && fu.len() == chart_cols => {
                    settle_t += 1;
                    let q = settle_t as f64 / SETTLE_FRAMES as f64;
                    let q = q * q * (3.0 - 2.0 * q);
                    (
                        fu.iter().zip(&real_u).map(|(a, b)| a + (b - a) * q).collect(),
                        fd.iter().zip(&real_d).map(|(a, b)| a + (b - a) * q).collect(),
                        tc::mix(GHOST, NEW_RGB, 0.45 + 0.55 * q),
                        tc::mix(GHOST, OK_RGB, 0.45 + 0.55 * q),
                    )
                }
                _ => (real_u, real_d, p.new.clone(), p.ok.clone()),
            }
        };
        for line in tc::vbars(
            &hu.iter().map(|v| (*v, cu.clone())).collect::<Vec<_>>(),
            3,
            1.0,
        ) {
            let mut parts: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for (colour, ch) in &line {
                parts.push((colour.as_str(), ch.clone()));
            }
            rows.push(tc::seg(&parts, w - 1));
        }
        rows.push(tc::seg(
            &[(tc::RST, " ".into()), (p.grid.as_str(), "─".repeat(chart_cols))],
            w - 1,
        ));
        for line in tc::vbars_down(
            &hd.iter().map(|v| (*v, cd.clone())).collect::<Vec<_>>(),
            3,
            1.0,
        ) {
            let mut parts: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
            for (colour, ch) in &line {
                parts.push((colour.as_str(), ch.clone()));
            }
            rows.push(tc::seg(&parts, w - 1));
        }
        let left_lbl = format!("{}d ago", days_list.len());
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!(" {}", left_lbl)),
                (
                    p.dim.as_str(),
                    " ".repeat(chart_cols.saturating_sub(left_lbl.chars().count() + 5).max(1)),
                ),
                (p.dim.as_str(), "today".into()),
            ],
            w - 1,
        ));

        rows.push(String::new());
        let mut ranked = s.teams.clone();
        ranked.sort_by(|a, b| {
            let open = |k: &String| {
                s.by_team
                    .get(k)
                    .and_then(|c| c.get("open"))
                    .copied()
                    .unwrap_or(0)
            };
            open(&b.0).cmp(&open(&a.0)).then(a.0.cmp(&b.0))
        });
        pane_len[teams_pane] = ranked.len();
        if !ranked.is_empty() {
            sel[teams_pane] = sel[teams_pane].min(ranked.len() - 1);
        }
        let on_teams = focus == Some(teams_pane);
        rows.push(tc::seg(
            &[
                (
                    if on_teams { p.accent.as_str() } else { p.lbl.as_str() },
                    " ── BY TEAM ──".into(),
                ),
                (
                    if on_teams { p.accent.as_str() } else { p.dim.as_str() },
                    heading_keys(teams_pane, focus, &pane_len).to_string(),
                ),
            ],
            w - 1,
        ));
        rows.push(tc::seg(
            &[(
                p.dim.as_str(),
                tc::pad(
                    &format!(
                        " {:<22}{:>6}{:>7}{:>8}{:>8}",
                        "TEAM",
                        "OPEN",
                        "TRIAGE",
                        "DOING",
                        format!("DONE{}D", want)
                    ),
                    w - 1,
                ),
            )],
            w - 1,
        ));
        for (i, (key, name)) in ranked.iter().enumerate() {
            if on_teams && i == sel[teams_pane] {
                cursor = Some(rows.len());
            }
            let empty = HashMap::new();
            let c = s.by_team.get(key).unwrap_or(&empty);
            let count = |k: &str| c.get(k).copied().unwrap_or(0);
            let here = on_teams && i == sel[teams_pane];
            let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
            let c_of = |colour: &str| {
                let colour = if tint.is_empty() || colour != p.dim {
                    colour
                } else {
                    p.dim_lit.as_str()
                };
                format!("{}{}", tint, colour)
            };
            let mut line = vec![
                (
                    c_of(if here { &p.accent } else { &p.txt }),
                    format!(
                        "{}{}",
                        if here { "▸" } else { " " },
                        tc::pad(&format!("{}  {}", key, name), 22)
                    ),
                ),
                (c_of(&p.new), format!("{:>6}", count("open"))),
                (
                    c_of(if count("triage") > 0 { &p.bad } else { &p.dim }),
                    format!("{:>7}", count("triage")),
                ),
                (
                    c_of(if count("started") > 0 { &p.warn } else { &p.dim }),
                    format!("{:>8}", count("started")),
                ),
                (
                    c_of(if count("done") > 0 { &p.ok } else { &p.dim }),
                    format!("{:>8}", count("done")),
                ),
            ];
            if here {
                line.push((tint.clone(), " ".repeat(w)));
            }
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
        }
        // Every project that is still going, whichever team owns it. The
        // board reaches them without going through a team first, the way
        // it reaches cycles without going through one.
        rows.push(String::new());
        let live: Vec<Proj> = s
            .all_projects
            .iter()
            .filter(|q| q.kind != "completed" && q.kind != "canceled")
            .cloned()
            .collect();
        let finished = s.all_projects.len() - live.len();
        pane_len[projects_pane] = live.len();
        if !live.is_empty() {
            sel[projects_pane] = sel[projects_pane].min(live.len() - 1);
        }
        board_project = live.get(sel[projects_pane]).map(|q| q.id.clone());
        let on_projects = focus == Some(projects_pane);
        rows.push(tc::seg(
            &[
                (
                    if on_projects { p.accent.as_str() } else { p.lbl.as_str() },
                    " ── PROJECTS ── ".into(),
                ),
                // What is not in the list, because a count of the running
                // ones alone reads as a count of all of them.
                (
                    p.dim.as_str(),
                    if finished > 0 {
                        format!("{} running · {} finished, not listed", live.len(), finished)
                    } else {
                        format!("{} running", live.len())
                    },
                ),
                (
                    if on_projects { p.accent.as_str() } else { p.dim.as_str() },
                    heading_keys(projects_pane, focus, &pane_len).to_string(),
                ),
            ],
            w - 1,
        ));
        if live.is_empty() {
            rows.push(tc::seg(
                &[(p.dim.as_str(), "  no project is running in any team".into())],
                w - 1,
            ));
        }
        {
            // Sized the way the team screen sizes its own list: no column
            // capped, the bar taking what is left, and the aside shedding
            // whole facts rather than being cut through the middle.
            let asides: Vec<String> = live
                .iter()
                .map(|q| {
                    let open: usize =
                        s.proj_state.get(&q.id).map(|m| m.values().sum()).unwrap_or(0);
                    if open > 0 {
                        format!("{} open", open)
                    } else {
                        String::new()
                    }
                })
                .collect();
            let widest = |xs: &mut dyn Iterator<Item = usize>| xs.max().unwrap_or(0);
            let team_w = widest(&mut live.iter().map(|q| q.teams.join("/").chars().count())).max(4);
            let name_w = widest(&mut live.iter().map(|q| q.name.chars().count())).max(8);
            let label_w = widest(&mut live.iter().map(|q| q.label.chars().count())).max(4);
            let full = widest(&mut asides.iter().map(|a| a.chars().count()));
            // Something has to go at narrow widths: the longest project
            // name in this workspace is 47 characters, and at eighty
            // columns the name, the team and the status alone do not fit.
            let base = 2 + team_w + 2 + name_w + 5;
            let (label_cost, bar_w, room) = project_columns(w, base, label_w, full);
            for (i, (q, aside)) in live.iter().zip(&asides).enumerate() {
                let here = on_projects && i == sel[projects_pane];
                if here {
                    cursor = Some(rows.len());
                }
                let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
                let c_of = |colour: &str| {
                let colour = if tint.is_empty() || colour != p.dim {
                    colour
                } else {
                    p.dim_lit.as_str()
                };
                format!("{}{}", tint, colour)
            };
                let colour = match q.kind.as_str() {
                    "started" => p.warn.as_str(),
                    _ => p.txt.as_str(),
                };
                let aside = if aside.chars().count() <= room { aside.as_str() } else { "" };
                let aside =
                    if aside.is_empty() { String::new() } else { format!("  {}", aside) };
                let line: Vec<(String, String)> = vec![
                    (
                        c_of(if here { &p.accent } else { &p.dim }),
                        format!(
                            "{} {}",
                            if here { "▸" } else { " " },
                            tc::pad(&q.teams.join("/"), team_w)
                        ),
                    ),
                    (
                        c_of(if here { &p.accent } else { &p.txt }),
                        format!("  {}", tc::pad(&q.name, name_w)),
                    ),
                    (
                        c_of(colour),
                        if label_cost > 0 {
                            format!("  {}", tc::pad(&q.label, label_w))
                        } else {
                            String::new()
                        },
                    ),
                    (
                        c_of(colour),
                        if bar_w > 0 {
                            format!("  {}", tc::meter(q.progress, bar_w))
                        } else {
                            String::new()
                        },
                    ),
                    (c_of(&p.txt), format!(" {:>3.0}%", 100.0 * q.progress)),
                    (c_of(&p.dim), aside),
                ];
                let refs: Vec<(&str, String)> =
                    line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                rows.push(tc::seg(&refs, w - 1));
            }
        }

        // One cycle, one team, or one of that team's projects, each on a
        // screen of its own.
        if let Some(which) = detail {
            let team = ranked
                .get(sel[teams_pane].min(ranked.len().saturating_sub(1)))
                .cloned();
            let none: Vec<Proj> = Vec::new();
            let projects: Vec<Proj> = team
                .as_ref()
                .and_then(|(key, _)| s.projects.get(key))
                .unwrap_or(&none)
                .clone();
            if which == teams_pane {
                list_len = projects.len();
                pick = pick.min(list_len.saturating_sub(1));
                open_project = projects.get(pick).map(|q| q.id.clone());
            }
            // The project being read is found by its id, so a poll that
            // re-sorts the list underneath cannot swap it for its
            // neighbour.
            // Looked up across the whole workspace, because the project
            // may have been opened from the board's list rather than from
            // the team whose screen is behind it.
            let reading = deep
                .as_ref()
                .and_then(|id| s.all_projects.iter().find(|q| &q.id == id).cloned());
            let (body, cursor) = if let Some(q) = reading.as_ref() {
                let empty = HashMap::new();
                let states = s.proj_state.get(&q.id).unwrap_or(&empty);
                let oldest = s.proj_oldest.get(&q.id).cloned();
                let record = held.lock().ok().and_then(|g| g.get(&q.id).map(|(v, _)| v.clone()));
                (
                    project_detail(
                        q,
                        &q.teams.join(" · "),
                        record.as_ref(),
                        states,
                        oldest.as_ref(),
                        w,
                        &p,
                    ),
                    None,
                )
            } else if which == cycles_pane {
                ranked_cycles
                    .get(sel[cycles_pane].min(ranked_cycles.len().saturating_sub(1)))
                    .map(|c| {
                        let id = text(c, "id");
                        let empty = HashMap::new();
                        let none: Vec<Issue> = Vec::new();
                        let states = s.cycle_state.get(&id).unwrap_or(&empty);
                        let issues = s.cycle_issues.get(&id).unwrap_or(&none);
                        list_len = issues.len();
                        copy_url = issues.get(pick.min(issues.len().saturating_sub(1)))
                            .map(|i| (i.url.clone(), i.ident.clone()));
                        cycle_detail(c, states, issues, pick, w, h, &p)
                    })
                    .unwrap_or_default()
            } else {
                team.as_ref()
                    .map(|(key, name)| {
                        let empty = HashMap::new();
                        let counts = s.by_team.get(key).unwrap_or(&empty);
                        let opens = s.proj_open.get(key).unwrap_or(&empty);
                        team_detail(key, name, counts, &projects, opens, pick, s.window, w, h, &p)
                    })
                    .unwrap_or_default()
            };
            drop(s);

            // Ask for the open project's own record, once, and again once
            // it has gone stale - the screen is left up for minutes at a
            // time and a burn-up from ten minutes ago is not this one.
            if let Some(q) = reading.as_ref() {
                let due = held
                    .lock()
                    .ok()
                    .and_then(|g| g.get(&q.id).map(|(_, at)| now() - at > refresh))
                    .unwrap_or(true);
                let mine = asking.lock().map(|g| !g.contains(&q.id)).unwrap_or(false);
                if due && mine && !ui_tok.is_empty() {
                    if let Ok(mut g) = asking.lock() {
                        g.insert(q.id.clone());
                    }
                    let (id, tok, quota) = (q.id.clone(), ui_tok.clone(), Arc::clone(&ui_quota));
                    let (held, asking) = (Arc::clone(&held), Arc::clone(&asking));
                    std::thread::spawn(move || {
                        let got = fetch_project(&id, &tok, &quota);
                        if let Ok(mut g) = held.lock() {
                            g.insert(id.clone(), (got, now()));
                        }
                        if let Ok(mut g) = asking.lock() {
                            g.remove(&id);
                        }
                    });
                }
            }

            if body.is_empty() {
                detail = None;
                deep = None;
            } else {
                // What this screen's own list is called, when it has one:
                // a team's holds its projects and a cycle's its open
                // issues. A screen with an empty list hands the arrows
                // back to plain scroll and says so.
                let listing = if reading.is_some() || list_len == 0 {
                    None
                } else if which == teams_pane {
                    Some(" project")
                } else if which == cycles_pane {
                    Some(" issue")
                } else {
                    None
                };
                let mut hints: Vec<Vec<(&str, String)>> = vec![vec![
                    (p.accent.as_str(), "↑↓".into()),
                    (p.dim.as_str(), listing.unwrap_or(" scroll").to_string()),
                ]];
                // Only a project can be opened; an issue is a leaf, and
                // what a leaf offers here is its address.
                if listing.is_some() && which == teams_pane {
                    hints.push(vec![
                        (p.accent.as_str(), "→/↵".into()),
                        (p.dim.as_str(), " open it".into()),
                    ]);
                }
                if copy_url.is_some() {
                    hints.push(vec![(p.dim.as_str(), "[c]opy url".into())]);
                }
                hints.push(vec![
                    (p.accent.as_str(), "←".into()),
                    (
                        p.dim.as_str(),
                        // Back goes wherever this screen was opened from,
                        // and says which: a project reached through a team
                        // returns to that team, one reached from the
                        // board's own list returns to the board.
                        if reading.is_some() && which == teams_pane {
                            "/esc the team".to_string()
                        } else {
                            "/esc back".to_string()
                        },
                    ),
                ]);
                hints.push(vec![(p.dim.as_str(), "[r]efresh".into())]);
                hints.push(vec![(p.dim.as_str(), "[q]uit".into())]);
                let foot: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
                    .into_iter()
                    .map(|l| format!(" {}", l))
                    .collect();
                let room = h.saturating_sub(foot.len()).max(1);
                let at = if reading.is_some() { &mut pscroll } else { &mut dscroll };
                // The page follows the cursor into the project list, the
                // way netwatch's detail follows one into a section.
                if let Some(row) = cursor {
                    *at = tc::follow(*at, row, room);
                }
                *at = (*at).min(body.len().saturating_sub(room));
                let from = *at;
                let last = (from + room).min(body.len());
                let mut out: Vec<String> = body[from..last].to_vec();
                while out.len() < room {
                    out.push(String::new());
                }
                if !note.is_empty() && now() - note_at < 6.0 {
                    if let Some(row) = out.last_mut() {
                        *row = tc::seg(&[(p.ok.as_str(), format!(" {}", note))], w - 1);
                    }
                }
                out.extend(foot);
                tc::draw(&out, w, h);
                std::thread::sleep(Duration::from_millis(300));
                continue;
            }
        } else {
            drop(s);
            list_len = 0;
            open_project = None;
            copy_url = None;
            deep = None;
        }

        // The board is taller than the pane, so the arrows do one of two
        // things and the footer has to say which. Unfocused they move the
        // whole board; focused they move a cursor through one section, and
        // the board follows.
        let mut hints: Vec<Vec<(&str, String)>> = vec![
            vec![
                (p.accent.as_str(), "↑↓".into()),
                (
                    p.dim.as_str(),
                    if focus.is_some() { " select" } else { " scroll" }.to_string(),
                ),
            ],
            vec![
                (p.accent.as_str(), "tab".into()),
                (
                    p.dim.as_str(),
                    if focus.is_some() { " next section" } else { " into a section" }.to_string(),
                ),
            ],
        ];
        if focus.is_some() {
            hints.push(vec![
                (p.accent.as_str(), "→/↵".into()),
                (p.dim.as_str(), " open it".into()),
            ]);
        }
        hints.push(vec![(p.dim.as_str(), "pgup/pgdn page".into())]);
        hints.push(vec![(p.dim.as_str(), "[w]indow".into())]);
        hints.push(vec![(p.dim.as_str(), "[r]efresh".into())]);
        hints.push(vec![(p.dim.as_str(), "[q]uit".into())]);
        let footer: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
        // The board is longer than most panes are tall, and every section
        // is now drawn whole - so the frame is a window onto it. With a
        // section focused the window chases its cursor; with none, the
        // arrows move the window itself.
        let room = h.saturating_sub(footer.len()).max(1);
        if let Some(at) = cursor {
            board = tc::follow(board, at, room);
        }
        board = board.min(rows.len().saturating_sub(room));
        board_len = rows.len();
        let last = (board + room).min(rows.len());
        let mut out: Vec<String> = rows[board..last].to_vec();
        while out.len() < room {
            out.push(String::new());
        }
        out.extend(footer);
        tc::draw(&out, w, h);
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_project(id: &str, name: &str, label: &str, kind: &str, progress: f64) -> Proj {
        Proj {
            id: id.into(),
            name: name.into(),
            label: label.into(),
            kind: kind.into(),
            progress,
            ..Default::default()
        }
    }

    /// Which screen column a string starts at.
    ///
    /// `str::find` answers in bytes, and the cursor marker is three of them
    /// for one column - so two rows that line up on screen came back two
    /// apart and failed a test about alignment.
    fn col(line: &str, needle: &str) -> usize {
        let at = line.find(needle).unwrap_or_else(|| panic!("{:?} not in {:?}", needle, line));
        line[..at].chars().count()
    }

    /// A team's screen as plain text. `pick` is the project under the
    /// cursor, which every test but the cursor's own leaves at the first.
    fn team_rows(
        counts: &HashMap<String, usize>,
        projects: &[Proj],
        opens: &HashMap<String, usize>,
        w: usize,
    ) -> String {
        let (rows, _) =
            team_detail("ABC", "A Team", counts, projects, opens, 0, 14, w, 40, &palette());
        plain(&rows)
    }

    /// The plain text of a rendered row, with the colour escapes taken out.
    fn plain(rows: &[String]) -> String {
        let joined = rows.join("\n");
        let mut out = String::new();
        let mut chars = joined.chars();
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
    }

    fn an_issue(ident: &str, title: &str, state: &str, age: f64, points: Option<f64>) -> Issue {
        Issue {
            ident: ident.into(),
            title: title.into(),
            url: format!("https://linear.app/x/issue/{}", ident),
            state: state.into(),
            age,
            points,
        }
    }

    /// A cycle carrying the two histories its screen reads.
    fn a_cycle(total: f64, closed: f64) -> serde_json::Value {
        serde_json::json!({
            "number": 7, "name": "", "endsAt": "2026-09-06T00:00:00.000Z",
            "team": { "name": "A Team" },
            "scopeHistory": [10.0, 10.0], "completedScopeHistory": [0.0, 2.0],
            "issueCountHistory": [total, total], "completedIssueCountHistory": [closed, closed],
        })
    }

    #[test]
    fn a_cycle_lists_what_is_open_in_it_and_counts_what_is_not() {
        // The scan behind this fetches open issues only, so a cycle of
        // eight with seven closed has one row - and without saying so, a
        // heading reading "1" under "issues 8" reads as a bug.
        let issues = vec![an_issue("ABC-1", "a thing", "started", 30.0, Some(3.0))];
        let states: HashMap<String, usize> = [("started".to_string(), 1usize)].into_iter().collect();
        let (rows, at) = cycle_detail(&a_cycle(8.0, 7.0), &states, &issues, 0, 110, 40, &palette());
        let out = plain(&rows);
        assert!(out.contains("OPEN IN THIS CYCLE"), "{}", out);
        assert!(out.contains("7 closed, not listed"), "{}", out);
        assert!(out.contains("ABC-1"), "{}", out);
        assert!(out.contains("a thing"), "{}", out);
        // The state's own vocabulary, the same word the board uses.
        assert!(out.contains("in progress"), "{}", out);
        assert!(out.contains("3p"), "{}", out);
        // And a cursor, on the row it named.
        assert!(plain(&[rows[at.unwrap()].clone()]).contains("ABC-1"));
        assert_eq!(out.matches('▸').count(), 1);
    }

    #[test]
    fn a_cycle_with_nothing_open_says_so_and_offers_no_cursor() {
        let (rows, at) =
            cycle_detail(&a_cycle(4.0, 4.0), &HashMap::new(), &[], 0, 110, 40, &palette());
        let out = plain(&rows);
        assert!(out.contains("nothing open in it"), "{}", out);
        // No cursor means the arrows go back to scrolling the screen, and
        // the footer says "scroll" rather than naming a list to walk.
        assert!(at.is_none());
    }

    #[test]
    fn a_long_issue_title_carries_on_underneath_rather_than_being_cut() {
        // The shape that broke it: a title built from a long chain of
        // separated parts, far past any column a list can give it.
        let long = "Check every published rate · northern route · via the interchange \
                    stop · against the operator's own timetable";
        let issues = vec![an_issue("ABC-1", long, "started", 30.0, None)];
        let states: HashMap<String, usize> = [("started".to_string(), 1usize)].into_iter().collect();
        let (rows, _) = cycle_detail(&a_cycle(1.0, 0.0), &states, &issues, 0, 100, 40, &palette());
        let out = plain(&rows);
        // Every word of it is on screen, even though no single row is wide
        // enough to hold the title.
        for word in long.split_whitespace() {
            assert!(out.contains(word), "{:?} missing from:\n{}", word, out);
        }
        // The issue rows fit in the pane. Measured against `w`, not
        // `w - 1`: the heading bar pads to the full width and the rows do
        // not, so a single bound for both would pass on the wrong one.
        assert!(
            out.lines().all(|l| l.chars().count() <= 100),
            "a row ran past the pane: {:?}",
            out.lines().max_by_key(|l| l.chars().count())
        );
        // An issue nobody has pointed says nothing, not "0p".
        assert!(!out.contains("0p"), "{}", out);
    }

    #[test]
    fn a_cycles_issues_put_what_is_moving_first_and_the_oldest_of_those_first() {
        let mut issues = vec![
            an_issue("ABC-3", "newer wip", "started", 10.0, None),
            an_issue("ABC-1", "waiting", "unstarted", 900.0, None),
            an_issue("ABC-2", "older wip", "started", 500.0, None),
            an_issue("ABC-4", "unlooked at", "triage", 999.0, None),
            an_issue("ABC-5", "shelved", "backlog", 999.0, None),
        ];
        issues.sort_by(|a, b| {
            state_rank(&a.state)
                .cmp(&state_rank(&b.state))
                .then(b.age.total_cmp(&a.age))
                .then(a.ident.cmp(&b.ident))
        });
        let order: Vec<&str> = issues.iter().map(|i| i.ident.as_str()).collect();
        assert_eq!(order, ["ABC-2", "ABC-3", "ABC-1", "ABC-5", "ABC-4"]);
    }

    #[test]
    fn a_team_screen_lists_its_projects_with_their_own_progress() {
        let counts: HashMap<String, usize> =
            [("open".to_string(), 9usize), ("started".to_string(), 4)].into_iter().collect();
        let projects = vec![
            a_project("p1", "hallway-lights", "In Progress", "started", 0.5),
            a_project("p2", "old-thing", "Done", "completed", 1.0),
        ];
        let opens: HashMap<String, usize> = [("p1".to_string(), 4usize)].into_iter().collect();
        let out = team_rows(&counts, &projects, &opens, 100);
        assert!(out.contains("PROJECTS"), "{}", out);
        assert!(out.contains("hallway-lights"), "{}", out);
        assert!(out.contains("4 open"), "{}", out);
        // Linear's own figure, not one derived from the open issues: this
        // project has four of nine open and is still shown at 50%.
        assert!(out.contains(" 50%"), "{}", out);
        // A finished project keeps its place and says so, rather than
        // showing a full bar with no explanation.
        assert!(out.contains("Done"), "{}", out);
        assert!(out.contains("100%"), "{}", out);
    }

    #[test]
    fn open_issues_in_no_project_are_counted_out_loud() {
        // Nine open, four in a project: the other five are in none, and
        // without saying so the column below does not reconcile with the
        // team's own open count three lines above it.
        let counts: HashMap<String, usize> = [("open".to_string(), 9usize)].into_iter().collect();
        let projects = vec![a_project("p1", "hallway-lights", "In Progress", "started", 0.5)];
        let opens: HashMap<String, usize> =
            [("p1".to_string(), 4usize), (String::new(), 5)].into_iter().collect();
        let out = team_rows(&counts, &projects, &opens, 100);
        assert!(out.contains("5 open in no project"), "{}", out);

        // With none loose, the aside is not there to be read past.
        let opens: HashMap<String, usize> = [("p1".to_string(), 4usize)].into_iter().collect();
        let out = team_rows(&counts, &projects, &opens, 100);
        assert!(!out.contains("in no project"), "{}", out);
    }

    #[test]
    fn a_long_project_name_is_shown_whole_and_the_bar_gives_way() {
        // The first cut of this capped the name column at 34 characters,
        // and a real project came out as "GCP Optimisations and GKE
        // Migratio" - which reads as a project that does not exist.
        let long = "GCP Optimisations and GKE Migration";
        let counts: HashMap<String, usize> = [("open".to_string(), 2usize)].into_iter().collect();
        let projects = vec![
            a_project("p1", long, "Completed", "completed", 0.87),
            a_project("p2", "short", "In Progress", "started", 0.1),
        ];
        let out =
            team_rows(&counts, &projects, &HashMap::new(), 130);
        assert!(out.contains(long), "{}", out);
        // And the short one still lines up under it.
        let row = out.lines().find(|l| l.contains("short")).unwrap();
        let wide = out.lines().find(|l| l.contains(long)).unwrap();
        assert_eq!(
            col(row, "In Progress"),
            col(wide, "Completed"),
            "status column ragged:\n{}\n{}",
            wide,
            row
        );
    }

    #[test]
    fn a_narrow_pane_drops_whole_facts_off_the_aside_rather_than_half_of_one() {
        let counts: HashMap<String, usize> = [("open".to_string(), 2usize)].into_iter().collect();
        let mut q = a_project("p1", "a-project", "In Progress", "started", 0.5);
        q.target = "2024-07-26".into();
        q.lead = "Wilhelmina".into();
        let opens: HashMap<String, usize> = [("p1".to_string(), 2usize)].into_iter().collect();
        let wide = team_rows(&counts, &[q.clone()], &opens, 130);
        let row = wide.lines().find(|l| l.contains("a-project")).unwrap();
        assert!(row.contains("2 open · due 2024-07-26 · Wilhelmina"), "{}", row);

        // Squeezed, the last fact leaves whole. What is left is still true,
        // and no half-written name is on screen claiming to be someone.
        for w in [58usize, 64, 70, 76, 82] {
            let out = team_rows(&counts, &[q.clone()], &opens, w);
            let row = out.lines().find(|l| l.contains("a-project")).unwrap();
            let aside = row.split("50%").nth(1).unwrap().trim();
            assert!(
                ["", "2 open", "2 open · due 2024-07-26", "2 open · due 2024-07-26 · Wilhelmina"]
                    .contains(&aside),
                "w={} left a part cut in half: {:?}",
                w,
                aside
            );
        }
    }

    #[test]
    fn the_cursor_marks_one_project_and_says_which_row_it_is_on() {
        let counts: HashMap<String, usize> = [("open".to_string(), 3usize)].into_iter().collect();
        let projects = vec![
            a_project("p1", "first", "In Progress", "started", 0.1),
            a_project("p2", "second", "In Progress", "started", 0.2),
            a_project("p3", "third", "In Progress", "started", 0.3),
        ];
        let (rows, at) =
            team_detail("ABC", "A", &counts, &projects, &HashMap::new(), 1, 14, 100, 40, &palette());
        let at = at.expect("a project list has a cursor");
        assert!(plain(&[rows[at].clone()]).contains("second"), "{}", plain(&[rows[at].clone()]));
        // Exactly one marker, so the reader is never asked which of two is
        // the one enter would open.
        assert_eq!(plain(&rows).matches('▸').count(), 1);

        // A cursor past the end lands on the last project rather than
        // falling off it - the list shortens under the cursor whenever a
        // poll lands.
        let (rows, at) =
            team_detail("ABC", "A", &counts, &projects, &HashMap::new(), 99, 14, 100, 40, &palette());
        assert!(plain(&[rows[at.unwrap()].clone()]).contains("third"));
    }

    #[test]
    fn a_project_screen_is_found_by_id_so_a_resort_cannot_swap_it() {
        // This is the whole reason the open project is held by id. The
        // list re-sorts on every poll; holding an index would have left
        // the screen showing whichever project fell into that slot.
        let mut projects = vec![
            a_project("p1", "first", "In Progress", "started", 0.1),
            a_project("p2", "second", "In Progress", "started", 0.2),
        ];
        let open = "p2".to_string();
        let before = projects.iter().find(|q| q.id == open).cloned().unwrap();
        projects.reverse();
        let after = projects.iter().find(|q| q.id == open).cloned().unwrap();
        assert_eq!(before.name, after.name);
        assert_eq!(after.name, "second");
        // And the index that used to point at it now points elsewhere.
        assert_eq!(projects[1].name, "first");
    }

    #[test]
    fn a_project_screen_shows_what_it_has_before_the_rest_arrives() {
        let q = a_project("p1", "hallway-lights", "In Progress", "started", 0.5);
        let states: HashMap<String, usize> =
            [("started".to_string(), 2usize), ("triage".to_string(), 1)].into_iter().collect();
        // Nothing fetched yet: the name, status and progress the list
        // already knew are on the first frame, and the screen says a
        // request is out rather than looking empty.
        let out = plain(&project_detail(&q, "ABC", None, &states, None, 100, &palette()));
        assert!(out.contains("HALLWAY-LIGHTS · ABC"), "{}", out);
        assert!(out.contains(" 50%"), "{}", out);
        assert!(out.contains("In Progress"), "{}", out);
        assert!(out.contains("OPEN BY STATE"), "{}", out);
        assert!(out.contains("asking Linear"), "{}", out);
    }

    #[test]
    fn a_finished_project_is_not_called_late() {
        // It read "overdue by 760d" on a project completed two years ago,
        // because nothing was asking whether the work had since landed.
        let mut q = a_project("p1", "old-thing", "Completed", "completed", 1.0);
        q.target = "2024-07-26".into();
        let record = serde_json::json!({ "completedAt": "2024-08-01T00:00:00.000Z" });
        let out =
            plain(&project_detail(&q, "ABC", Some(&record), &HashMap::new(), None, 100, &palette()));
        assert!(!out.contains("overdue"), "{}", out);
        assert!(out.contains("2024-08-01"), "{}", out);
        assert!(out.contains("target was"), "{}", out);

        // Still running and past its date, it is late and says so.
        let mut q = a_project("p2", "live-thing", "In Progress", "started", 0.4);
        q.target = "2024-07-26".into();
        let out =
            plain(&project_detail(&q, "ABC", Some(&serde_json::json!({})), &HashMap::new(), None, 100, &palette()));
        assert!(out.contains("overdue by"), "{}", out);
    }

    #[test]
    fn a_project_fetch_that_failed_says_so_instead_of_asking_for_ever() {
        // A screen stuck on "loading" is this widget's oldest lesson: a
        // failure nobody records is indistinguishable from a slow answer.
        let q = a_project("p1", "hallway-lights", "In Progress", "started", 0.5);
        let bad = serde_json::json!({ "_error": "HTTP 502 from Linear" });
        let out =
            plain(&project_detail(&q, "ABC", Some(&bad), &HashMap::new(), None, 100, &palette()));
        assert!(out.contains("could not read the project"), "{}", out);
        assert!(out.contains("HTTP 502"), "{}", out);
        assert!(!out.contains("asking Linear"), "{}", out);
    }

    #[test]
    fn milestones_fall_in_date_order_and_their_bars_are_out_of_a_hundred() {
        let q = a_project("p1", "hallway-lights", "In Progress", "started", 0.5);
        // Linear reports a milestone's progress out of 100 and a project's
        // out of 1. Fed straight to the meter, every milestone read full.
        let record = serde_json::json!({
            "projectMilestones": { "nodes": [
                { "name": "later", "targetDate": "2026-09-05", "progress": 0.0 },
                { "name": "undated", "targetDate": null, "progress": 50.0 },
                { "name": "sooner", "targetDate": "2026-09-03", "progress": 67.86 },
            ]},
        });
        let out =
            plain(&project_detail(&q, "ABC", Some(&record), &HashMap::new(), None, 100, &palette()));
        let rows: Vec<&str> = out.lines().collect();
        let seat = |name: &str| rows.iter().position(|l| l.contains(name)).unwrap();
        assert!(seat("sooner") < seat("later"), "{}", out);
        assert!(seat("later") < seat("undated"), "dated milestones come first:\n{}", out);
        // 67.86 out of a hundred, not 6786%.
        let row = rows[seat("sooner")];
        assert!(row.contains(" 68%"), "{}", row);
        assert!(row.contains('░'), "a 68% bar is not full: {}", row);
    }

    #[test]
    fn one_heading_at_a_time_says_tab_and_it_is_the_one_tab_reaches() {
        let lens = [5usize, 14, 34];
        let says = |focus: Option<usize>| -> Vec<&'static str> {
            (0..3).map(|me| heading_keys(me, focus, &lens)).collect()
        };
        // Nothing focused: the first section is where tab goes.
        assert_eq!(says(None), ["   [tab] to focus", "", ""]);
        // Focused: that heading takes the arrows, and the hint moves on to
        // the one the next press actually reaches.
        assert_eq!(says(Some(0)), ["   ↑↓", "   [tab] to focus", ""]);
        assert_eq!(says(Some(1)), ["", "   ↑↓", "   [tab] to focus"]);
        // From the last, tab lets go - so no heading claims it.
        assert_eq!(says(Some(2)), ["", "", "   ↑↓"]);
        // Never two at once, whatever the focus.
        for focus in [None, Some(0), Some(1), Some(2)] {
            assert_eq!(
                says(focus).iter().filter(|h| h.contains("[tab]")).count() <= 1,
                true,
                "two headings claimed tab with focus {:?}",
                focus
            );
        }
        // A section with nothing in it never claims it, because tab steps
        // over it - a hint on a heading no press reaches is the thing this
        // is careful about.
        let gap = [5usize, 0, 34];
        assert_eq!(heading_keys(1, Some(0), &gap), "");
        assert_eq!(heading_keys(2, Some(0), &gap), "   [tab] to focus");
    }

    #[test]
    fn the_board_walks_three_sections_and_lets_go_only_at_the_ends() {
        // Cycles, teams, then the board's own project list, in the order
        // they are drawn. The rule is the same one every widget here with
        // focusable sections follows; this is the third section arriving.
        let lens = [5usize, 14, 34];
        assert_eq!(tc::next_section(None, &lens), Some(0));
        assert_eq!(tc::next_section(Some(0), &lens), Some(1));
        assert_eq!(tc::next_section(Some(1), &lens), Some(2));
        assert_eq!(tc::next_section(Some(2), &lens), None);
        // Down off the last cycle lands on the first team, and off the
        // last team on the first project.
        assert_eq!(tc::step_across_sections(0, 4, &lens, true), Some((1, 0)));
        assert_eq!(tc::step_across_sections(1, 13, &lens, true), Some((2, 0)));
        assert_eq!(tc::step_across_sections(2, 33, &lens, true), None);
        // And back up the same way, into the *last* row of the section
        // above rather than its first.
        assert_eq!(tc::step_across_sections(2, 0, &lens, false), Some((1, 13)));
        assert_eq!(tc::step_across_sections(1, 0, &lens, false), Some((0, 4)));
        assert_eq!(tc::step_across_sections(0, 0, &lens, false), None);
        // A section with nothing in it is stepped over, not landed on.
        let gap = [5usize, 0, 34];
        assert_eq!(tc::step_across_sections(0, 4, &gap, true), Some((2, 0)));
        assert_eq!(tc::next_section(Some(0), &gap), Some(2));
    }

    #[test]
    fn a_project_row_never_overflows_and_never_loses_ground_as_it_widens() {
        // Both bugs this function exists for, stated as properties.
        let (base, label_w, aside) = (2 + 7 + 2 + 47 + 5, 11usize, 8usize);
        let mut had: Option<(bool, bool, bool)> = None;
        for w in 40..200usize {
            let (label_cost, bar, room) = project_columns(w, base, label_w, aside);
            let bar_cost = if bar > 0 { 2 + bar } else { 0 };
            // It fits, once there is room for the name at all. Anything
            // wider than the pane is cut by `seg`, and what `seg` cuts is
            // the percentage - the row's only number.
            if base <= w - 1 {
                let aside_cost = if room > 0 { 2 + room } else { 0 };
                assert!(
                    base + label_cost + bar_cost + aside_cost <= w - 1,
                    "w={} overflows: base {} label {} bar {} aside {}",
                    w, base, label_cost, bar_cost, aside_cost
                );
            } else {
                // Too narrow even for the name: nothing optional is added
                // on top of a row that is already over the edge.
                assert_eq!((label_cost, bar, room), (0, 0, 0), "w={}", w);
            }
            // A wider pane never shows less than a narrower one did.
            let now = (label_cost > 0, bar > 0, room >= aside);
            if let Some(before) = had {
                assert!(
                    now.0 >= before.0 && now.1 >= before.1 && now.2 >= before.2,
                    "w={} lost a column the narrower pane had: {:?} then {:?}",
                    w, before, now
                );
            }
            had = Some(now);
        }
        // And at the ends: nothing optional survives a very narrow pane,
        // everything does in a wide one.
        assert_eq!(project_columns(50, base, label_w, aside), (0, 0, 0));
        let (label_cost, bar, room) = project_columns(190, base, label_w, aside);
        assert!(label_cost > 0 && bar == 30 && room >= aside);
    }


    #[test]
    fn a_team_with_no_projects_says_so_rather_than_showing_an_empty_heading() {
        let counts: HashMap<String, usize> = [("open".to_string(), 3usize)].into_iter().collect();
        let out =
            team_rows(&counts, &[], &HashMap::new(), 100);
        assert!(out.contains("owns no projects"), "{}", out);
    }

    #[test]
    fn running_projects_sort_above_finished_ones() {
        let mut list = vec![
            a_project("p1", "done", "Done", "completed", 1.0),
            a_project("p2", "dropped", "Cancelled", "canceled", 0.2),
            a_project("p3", "running", "In Progress", "started", 0.3),
            a_project("p4", "waiting", "Backlog", "backlog", 0.0),
            // A status this build has never heard of belongs with the live
            // work, not buried under the finished work.
            a_project("p5", "odd", "Shipping", "some_new_type", 0.9),
        ];
        list.sort_by(|a, b| rank(&a.kind).cmp(&rank(&b.kind)).then(a.name.cmp(&b.name)));
        let order: Vec<&str> = list.iter().map(|q| q.name.as_str()).collect();
        assert_eq!(order, ["running", "odd", "waiting", "done", "dropped"]);
    }

    #[test]
    fn a_timestamp_that_is_not_ascii_is_declined_rather_than_fatal() {
        // A full-width digit puts a character boundary inside byte 19.
        // This used to panic in the poll thread, which under the release
        // profile takes the whole widget with it.
        // Byte 19 falls inside this character; an earlier fixture put the
        // wide digit where byte 19 was still a boundary and so passed
        // against the bug. Verified by reverting the fix.
        assert_eq!(parse("2026-08-24T10:00:0\u{ff14} x"), None);
        assert_eq!(parse("\u{4e00}\u{4e00}\u{4e00}\u{4e00}\u{4e00}"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("2026-08-24"), None);
        // And a real one still reads.
        assert!(parse("2026-08-24T10:00:00.000Z").is_some());
    }


    #[test]
    fn a_span_changes_unit_before_it_stops_meaning_anything() {
        assert_eq!(dur(None), "--");
        assert_eq!(dur(Some(0.5)), "30m");
        assert_eq!(dur(Some(3.25)), "3.2h");
        assert_eq!(dur(Some(72.0)), "3.0d");
        // An issue open for 1021.6d is arithmetic; 2.8y is a decision.
        assert_eq!(dur(Some(24.0 * 365.0 * 2.8)), "2.8y");
        // Never zero: something that took forty seconds took a minute, not
        // no time at all.
        assert_eq!(dur(Some(0.001)), "1m");
    }

    #[test]
    fn a_median_takes_the_middle_of_an_even_count() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[5.0]), Some(5.0));
        assert_eq!(median(&[1.0, 3.0]), Some(2.0));
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
    }

    #[test]
    fn a_timestamp_gives_up_its_day_and_its_instant() {
        assert_eq!(day("2026-08-23T04:15:00.000Z"), "2026-08-23");
        assert_eq!(day(""), "");
        let at = parse("2026-08-23T04:15:00.000Z").expect("a Linear timestamp");
        assert_eq!(at.date().to_string(), "2026-08-23");
        // Anything shorter than a whole instant is not one.
        assert!(parse("2026-08-23").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn a_cycle_is_ranked_by_what_moved_lately() {
        let busy: serde_json::Value = serde_json::from_str(
            r#"{"scopeHistory": [10,10,10,10,10,20],
                "completedScopeHistory": [0,1,2,3,4,5], "endsAt": ""}"#,
        )
        .unwrap();
        let quiet: serde_json::Value = serde_json::from_str(
            r#"{"scopeHistory": [10,10,10,10,10,10],
                "completedScopeHistory": [5,5,5,5,5,5], "endsAt": ""}"#,
        )
        .unwrap();
        // Movement is negated so the busiest sorts first.
        assert!(churn(&busy).0 < churn(&quiet).0);
        // A cycle nothing has touched scores nothing at all, whatever its
        // deadline - which is how an empty one sinks without a special case.
        assert_eq!(churn(&quiet).0, 0.0);
        let empty: serde_json::Value = serde_json::from_str(r#"{"endsAt": ""}"#).unwrap();
        assert_eq!(churn(&empty).0, 0.0);
    }

    #[test]
    fn a_whole_number_of_points_loses_its_decimal() {
        assert_eq!(tidy(8.0), "8");
        assert_eq!(tidy(8.5), "8.5");
        assert_eq!(tidy(0.0), "0");
    }

    #[test]
    fn a_key_decides_which_teams_are_counted() {
        // Named teams win outright; otherwise the excluded ones are dropped
        // and everything else is in.
        let of = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };
        assert!(team_wanted(&of(&["TOY"]), &of(&["OPS"]), "TOY"));
        assert!(!team_wanted(&of(&["TOY"]), &of(&[]), "OPS"));
        assert!(team_wanted(&of(&[]), &of(&["OPS"]), "TOY"));
        assert!(!team_wanted(&of(&[]), &of(&["OPS"]), "OPS"));
        // A named team wins even when it is also excluded, which is the
        // branch the test-local copy could never have got wrong.
        assert!(team_wanted(&of(&["OPS"]), &of(&["OPS"]), "OPS"));
    }
}
