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

use std::collections::HashMap;
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
    nodes {{ identifier estimate startedAt createdAt
            state {{ type }} team {{ key }} }}
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
      name number startsAt endsAt progress
      issueCountHistory completedIssueCountHistory
      scopeHistory completedScopeHistory
      team { key name }
    }
    pageInfo { hasNextPage endCursor }
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

/// An issue worth going and looking at: how long, and which one.
type Extreme = Option<(f64, String)>;

#[derive(Default)]
struct State {
    teams: Vec<(String, String)>,
    states: HashMap<String, usize>,
    by_team: HashMap<String, HashMap<String, usize>>,
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
        guard.truncated = capped || cap2 || cap3;
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
    let (cycles_pane, teams_pane) = (0usize, 1usize);
    let mut focus: Option<usize> = None;
    let mut sel = [0usize, 0usize];
    // How long each pane was when it was last drawn. The keys are read
    // before the frame is built, so walking off the end of a pane has to be
    // judged against the length it had a moment ago - which is the length
    // the reader is looking at.
    let mut pane_len = [0usize, 0usize];
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
                "tab" => focus = tc::next_section(focus, &pane_len),
                // Walking off either end of a pane leaves it. There is no
                // screen scroll here to hand the arrows to - both panes
                // window themselves to fit - so from nothing focused they
                // step back in at the near end, the same ring latency and
                // link use.
                "up" | "down" => {
                    let down = key == "down";
                    focus = match focus {
                        Some(here) => tc::step_across_sections(here, sel[here], &pane_len, down)
                            .map(|(pane, row)| {
                                sel[pane] = row;
                                pane
                            }),
                        // Nothing focused, and no screen scroll to hand the
                        // arrows to - both panes window themselves. They
                        // step into the near end of the first pane that has
                        // rows, so the ring closes the way latency's does.
                        None => tc::next_section(None, &pane_len).map(|here| {
                            sel[here] = if down { 0 } else { pane_len[here] - 1 };
                            here
                        }),
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
        let shown = ((h.saturating_sub(rows.len())) / 4).clamp(2, 6);
        let cfirst = if ranked_cycles.len() > shown {
            sel[cycles_pane]
                .saturating_sub(shown / 2)
                .min(ranked_cycles.len() - shown)
        } else {
            0
        };
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
                    if ranked_cycles.len() > shown {
                        format!(
                            "   {}{}-{} of {}",
                            if here_now { "↑↓ " } else { "" },
                            cfirst + 1,
                            (cfirst + shown).min(ranked_cycles.len()),
                            ranked_cycles.len()
                        )
                    } else {
                        String::new()
                    },
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
        for (ci, c) in ranked_cycles.iter().enumerate().skip(cfirst).take(shown) {
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
            let c_of = |colour: &str| format!("{}{}", tint, colour);
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
        let room = h.saturating_sub(5 + rows.len()).max(1);
        let first = if ranked.len() > room {
            sel[teams_pane].saturating_sub(room / 2).min(ranked.len() - room)
        } else {
            0
        };
        let on_teams = focus == Some(teams_pane);
        rows.push(tc::seg(
            &[
                (
                    if on_teams { p.accent.as_str() } else { p.lbl.as_str() },
                    " ── BY TEAM ──".into(),
                ),
                (
                    if on_teams { p.accent.as_str() } else { p.dim.as_str() },
                    if ranked.len() > room {
                        format!(
                            "   {}{}-{} of {}",
                            if on_teams { "↑↓ " } else { "" },
                            first + 1,
                            (first + room).min(ranked.len()),
                            ranked.len()
                        )
                    } else {
                        String::new()
                    },
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
        for (i, (key, name)) in ranked.iter().enumerate().skip(first).take(room) {
            let empty = HashMap::new();
            let c = s.by_team.get(key).unwrap_or(&empty);
            let count = |k: &str| c.get(k).copied().unwrap_or(0);
            let here = on_teams && i == sel[teams_pane];
            let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
            let c_of = |colour: &str| format!("{}{}", tint, colour);
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
        drop(s);

        let hints: Vec<Vec<(&str, String)>> = vec![
            // Not "scroll": nothing on this board scrolls. The arrows move
            // a cursor through whichever pane has the focus, and both panes
            // window themselves around it.
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
            vec![
                (p.accent.as_str(), "tab".into()),
                (
                    p.dim.as_str(),
                    if focus.is_some() { " next pane" } else { " into a pane" }.to_string(),
                ),
            ],
            vec![(p.dim.as_str(), "[w]indow".into())],
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
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
