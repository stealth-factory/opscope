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

//! Vercel deployments, across every team the token can see.
//!
//! A port of deployments.py. The first widget here to hold a secret: the
//! token comes from the git-ignored config or the environment, never from
//! the source tree, and never reaches a command line.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};
use toys_core as tc;

const API: &str = "https://api.vercel.com";
/// How many build-log lines to ask for. Enough that the tail of a failing
/// build is in there whole, few enough not to wait on a novel.
const EVENT_LIMIT: usize = 200;
/// How long a fetched detail is kept before the open page asks again. A
/// running build's log grows while it is being read.
const DETAIL_TTL: f64 = 60.0;
const FILTERS: &[&str] = &["all", "failed", "production"];
const SPARK: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// The states that mean something is happening right now.
const LIVE: &[&str] = &["BUILDING", "QUEUED", "INITIALIZING"];

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// A Vercel token, and where it came from.
///
/// Deliberately not the Vercel CLI's session: that expires within hours and
/// only the CLI can refresh it, so a panel reading it goes dark overnight.
/// Create one at Account Settings -> Tokens instead.
fn token(cfg: &serde_json::Value) -> (String, &'static str) {
    let from_config = tc::cfg_str(cfg, "token", "");
    if !from_config.is_empty() {
        return (from_config, "config");
    }
    let name = tc::cfg_str(cfg, "token_env", "VERCEL_TOKEN");
    let name = if name.is_empty() { "VERCEL_TOKEN".into() } else { name };
    match std::env::var(&name) {
        Ok(value) if !value.is_empty() => (value, "env"),
        _ => (String::new(), "missing"),
    }
}

fn api(path: &str, tok: &str) -> Result<serde_json::Value, String> {
    let url = format!("{}{}", API, path);
    let body = tc::get(&url, &[("Authorization", &format!("Bearer {}", tok))], 25)?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

/// Every team the token can see, so deployments are not just personal.
///
/// An empty list on failure is deliberate: the personal scope still works,
/// and a widget that refused to start because one endpoint was down would
/// be worse than one showing fewer rows.
fn discover_teams(tok: &str) -> Vec<String> {
    let Ok(res) = api("/v2/teams", tok) else {
        return Vec::new();
    };
    res["teams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["id"].as_str().map(String::from))
        .collect()
}

/// Per-deployment detail: why it failed, timings, regions, aliases.
///
/// The list endpoint carries none of this, so it is fetched when the info
/// view opens rather than for two hundred deployments nobody asked about.
fn fetch_detail(uid: &str, team: &str, tok: &str) -> serde_json::Value {
    let mut path = format!("/v13/deployments/{}", uid);
    if !team.is_empty() {
        path += &format!("?teamId={}", team);
    }
    let mut got = match api(&path, tok) {
        Ok(value) => value,
        Err(e) => serde_json::json!({ "_error": e }),
    };
    // The build log, on the same trip. errorMessage says a build failed and
    // names a code; the log says which line of somebody's config did it,
    // which is the thing you would otherwise open a browser for.
    if let Some(map) = got.as_object_mut() {
        map.insert("_events".into(), fetch_events(uid, team, tok));
        map.insert("_fetched_at".into(), serde_json::json!(now()));
    }
    got
}

/// The build log for one deployment, newest last.
///
/// Capped on the way in: a long build runs to thousands of lines and the
/// pane shows a few dozen, so asking for all of them would spend the wait
/// on text nobody sees. The tail is the useful end - a build says why it
/// failed on its way out, not on its way in.
fn fetch_events(uid: &str, team: &str, tok: &str) -> serde_json::Value {
    let mut path = format!("/v3/deployments/{}/events?limit={}", uid, EVENT_LIMIT);
    if !team.is_empty() {
        path += &format!("&teamId={}", team);
    }
    match api(&path, tok) {
        Ok(value) => value,
        Err(e) => serde_json::json!({ "_error": e }),
    }
}

#[derive(Default)]
struct State {
    deployments: Vec<serde_json::Value>,
    err: String,
    fetched: f64,
}

fn text(value: &serde_json::Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_string()
}

fn ms_at(value: &serde_json::Value, key: &str) -> Option<f64> {
    value[key].as_f64()
}

/// How long ago, on this widget's own scale.
fn age(ms: Option<f64>) -> String {
    let Some(ms) = ms else {
        return "--".to_string();
    };
    let s = (now() - ms / 1000.0).max(0.0);
    if s < 90.0 {
        format!("{}s", s as i64)
    } else if s < 5400.0 {
        format!("{}m", (s / 60.0) as i64)
    } else if s < 172_800.0 {
        format!("{}h", (s / 3600.0) as i64)
    } else {
        format!("{}d", (s / 86400.0) as i64)
    }
}

fn dur(seconds: Option<f64>) -> String {
    let Some(s) = seconds else {
        return "  --  ".to_string();
    };
    if s < 60.0 {
        format!("{:>5.0}s", s)
    } else {
        format!("{}m{:02}s", (s / 60.0) as i64, (s as i64) % 60)
    }
}

/// How long a build took, or has been taking.
fn build_seconds(d: &serde_json::Value) -> Option<f64> {
    let building = ms_at(d, "buildingAt")?;
    if let Some(ready) = ms_at(d, "ready") {
        return Some((ready - building) / 1000.0);
    }
    if LIVE.contains(&text(d, "state").as_str()) {
        return Some(now() - building / 1000.0);
    }
    None
}

fn wrap(t: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = t.chars().collect();
    if chars.is_empty() || width == 0 {
        return vec![String::new()];
    }
    chars
        .chunks(width)
        .map(|c| c.iter().collect())
        .collect()
}

fn when(ms: Option<f64>) -> String {
    let Some(ms) = ms else {
        return String::new();
    };
    match Local.timestamp_opt((ms / 1000.0) as i64, 0).single() {
        Some(at) => at.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

/// Everything worth copying out of a deployment.
fn copy_items(dep: &serde_json::Value, detail: Option<&serde_json::Value>) -> Vec<(String, String)> {
    let meta = &dep["meta"];
    let mut items = Vec::new();
    let mut push = |label: &str, value: String| {
        if !value.is_empty() {
            items.push((label.to_string(), value));
        }
    };
    push("Deployment dashboard", text(dep, "inspectorUrl"));
    let branch_alias = text(meta, "branchAlias");
    if !branch_alias.is_empty() {
        push("Branch preview", format!("https://{}", branch_alias));
    }
    let url = text(dep, "url");
    if !url.is_empty() {
        push("Commit preview", format!("https://{}", url));
    }
    let (pr, org, repo) = (
        text(meta, "githubPrId"),
        text(meta, "githubOrg"),
        text(meta, "githubRepo"),
    );
    if !pr.is_empty() && !org.is_empty() && !repo.is_empty() {
        push(
            "Pull request",
            format!("https://github.com/{}/{}/pull/{}", org, repo, pr),
        );
    }
    push("Commit SHA", text(meta, "githubCommitSha"));
    push("Branch name", text(meta, "githubCommitRef"));
    if let Some(d) = detail {
        push("Error message", text(d, "errorMessage"));
    }
    items
}

/// Progressive disclosure: spend extra width on more content, not padding.
///
/// Under 66 columns only the essentials fit. Above that the commit SHA and
/// branch appear. From 110 the metadata and commit subject share one line,
/// so twice as many deployments are visible in the same height.
struct Columns {
    detail: bool,
    single: bool,
    project: usize,
    branch: usize,
}

fn columns(w: usize) -> Columns {
    Columns {
        detail: w >= 66,
        single: w >= 110,
        project: if w < 80 {
            12
        } else if w < 110 {
            16
        } else {
            20
        },
        branch: (w / 5).clamp(12, 34),
    }
}

/// Deployments per time bucket, coloured by the worst outcome in it.
fn activity(deps: &[serde_json::Value], w: usize, hours: f64, p: &Palette) -> (String, usize) {
    let cols = w.saturating_sub(2).max(10);
    let at = now() * 1000.0;
    let span = hours * 3_600_000.0;
    let mut buckets: Vec<Vec<&serde_json::Value>> = vec![Vec::new(); cols];
    for d in deps {
        let off = at - ms_at(d, "created").unwrap_or(at);
        if (0.0..span).contains(&off) {
            let slot = cols - 1 - (off / span * cols as f64) as usize;
            buckets[slot.min(cols - 1)].push(d);
        }
    }
    let peak = buckets.iter().map(|b| b.len()).max().unwrap_or(0);
    if peak == 0 {
        return (
            tc::seg(
                &[(
                    p.dim.as_str(),
                    format!(" no deployments in the last {}h", hours as i64),
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
        let states: HashSet<String> = bucket.iter().map(|d| text(d, "state")).collect();
        let colour = if states.contains("ERROR") {
            &p.error
        } else if LIVE.iter().any(|s| states.contains(*s)) {
            &p.build
        } else {
            &p.ready
        };
        let level = ((bucket.len() as f64 / peak as f64) * 7.99) as usize;
        parts.push((colour.as_str(), SPARK[level.min(7)].to_string()));
    }
    (tc::seg(&parts, w - 1), peak)
}

struct Palette {
    ready: String,
    build: String,
    error: String,
    queue: String,
    cancel: String,
    dim: String,
    grid: String,
    msg: String,
    url: String,
    hint: String,
    txt: String,
    lbl: String,
    prod: String,
    sha: String,
    branch: String,
    accent: String,
}

fn palette() -> Palette {
    Palette {
        ready: tc::rgb(80, 235, 150),
        build: tc::rgb(255, 200, 90),
        error: tc::rgb(255, 95, 105),
        queue: tc::rgb(120, 160, 220),
        cancel: tc::rgb(140, 145, 160),
        dim: tc::rgb(127, 147, 172),
        grid: tc::rgb(71, 91, 116),
        msg: tc::rgb(158, 174, 196),
        url: tc::rgb(130, 200, 255),
        hint: tc::rgb(126, 148, 173),
        txt: tc::rgb(225, 235, 245),
        lbl: tc::rgb(130, 165, 200),
        prod: tc::rgb(120, 180, 255),
        sha: tc::rgb(190, 170, 255),
        branch: tc::rgb(150, 210, 255),
        accent: tc::rgb(150, 210, 255),
    }
}

fn state_colour<'a>(state: &str, p: &'a Palette) -> &'a str {
    match state {
        "READY" => &p.ready,
        "BUILDING" => &p.build,
        // Initializing is before the build starts, which is why
        // deployments.py groups it with the queue rather than the build.
        // Grouping it with BUILDING put the two panes on different colours
        // for the same state.
        "INITIALIZING" => &p.queue,
        "ERROR" => &p.error,
        "QUEUED" => &p.queue,
        "CANCELED" => &p.cancel,
        _ => &p.dim,
    }
}

/// Title case, which is all the Python's `.title()` is doing to a state.
fn titled(state: &str) -> String {
    let mut chars = state.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// One deployment in full: state, timings, why it failed, and what to copy.

/// The build log, newest last, with what the build wrote to stderr picked
/// out of what it wrote to stdout.
///
/// Vercel returns the lines oldest-first and a build explains itself on the
/// way out, so the tail is what matters - the last error before it gave up.
/// Long lines wrap rather than clip: a stack trace cut at the pane edge is
/// the half without the path in it.
fn build_log(detail: &serde_json::Value, w: usize, p: &Palette) -> Vec<String> {
    let events = &detail["_events"];
    if let Some(err) = events.get("_error").and_then(|e| e.as_str()) {
        return vec![
            String::new(),
            tc::seg(&[(p.lbl.as_str(), " ── BUILD LOG ──".into())], w - 1),
            tc::seg(&[(p.dim.as_str(), format!("  {}", err))], w - 1),
        ];
    }
    let Some(lines) = events.as_array() else {
        return Vec::new();
    };
    if lines.is_empty() {
        return Vec::new();
    }
    let mut rows = vec![
        String::new(),
        tc::seg(
            &[
                (p.lbl.as_str(), " ── BUILD LOG ──".into()),
                (
                    p.dim.as_str(),
                    format!(" {} line{}", lines.len(), if lines.len() == 1 { "" } else { "s" }),
                ),
            ],
            w - 1,
        ),
    ];
    for event in lines {
        let said = text(event, "text");
        let said = said.trim_end();
        if said.is_empty() {
            continue;
        }
        let colour = if text(event, "type") == "stderr" {
            p.error.as_str()
        } else {
            p.dim.as_str()
        };
        for line in wrap(said, w.saturating_sub(4).max(10)) {
            rows.push(tc::seg(&[(colour, format!("  {}", line))], w - 1));
        }
    }
    rows
}

fn info_overlay(
    dep: &serde_json::Value,
    detail: Option<&serde_json::Value>,
    w: usize,
    h: usize,
    note: &str,
    p: &Palette,
) -> Vec<String> {
    let meta = &dep["meta"];
    let empty = serde_json::Value::Null;
    let d = detail.unwrap_or(&empty);
    let mut rows = vec![tc::title("deployment", w, &p.prod)];

    macro_rules! field {
        ($label:expr, $value:expr, $colour:expr) => {{
            let value: String = $value;
            if !value.is_empty() {
                rows.push(tc::seg(
                    &[
                        (p.dim.as_str(), format!(" {:<11}", $label)),
                        ($colour, value),
                    ],
                    w - 1,
                ));
            }
        }};
    }

    let state = text(dep, "state");
    field!("project", text(dep, "name"), p.accent.as_str());
    let step = text(d, "errorStep");
    field!(
        "state",
        format!(
            "{}{}",
            titled(&state),
            if step.is_empty() {
                String::new()
            } else {
                format!("  ({})", step)
            }
        ),
        state_colour(&state, p)
    );
    let production = text(dep, "target") == "production";
    field!(
        "target",
        if production { "production" } else { "preview" }.to_string(),
        if production { p.prod.as_str() } else { p.dim.as_str() }
    );
    field!(
        "created",
        when(ms_at(dep, "created").or_else(|| ms_at(dep, "createdAt"))),
        p.txt.as_str()
    );
    if let Some(secs) = build_seconds(dep) {
        let queued = match (ms_at(dep, "createdAt"), ms_at(dep, "buildingAt")) {
            (Some(made), Some(built)) => (built - made) / 1000.0,
            _ => 0.0,
        };
        field!(
            "build",
            format!(
                "{}{}",
                dur(Some(secs)).trim(),
                if queued > 0.5 {
                    format!("   queued {:.0}s", queued)
                } else {
                    String::new()
                }
            ),
            p.txt.as_str()
        );
    }
    let regions: Vec<String> = d["regions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str().map(String::from))
        .collect();
    if !regions.is_empty() {
        let plan = text(d, "plan");
        field!(
            "regions",
            format!(
                "{}{}",
                regions.join(", "),
                if plan.is_empty() {
                    String::new()
                } else {
                    format!("   plan {}", plan)
                }
            ),
            p.txt.as_str()
        );
    }
    let aliases = d["alias"].as_array().map(|a| a.len()).unwrap_or(0);
    if aliases > 0 {
        field!("aliases", format!("{} assigned", aliases), p.dim.as_str());
    }

    let (message, code) = (text(d, "errorMessage"), text(d, "errorCode"));
    if !message.is_empty() || !code.is_empty() {
        rows.push(String::new());
        rows.push(tc::seg(&[(p.lbl.as_str(), " ── WHY IT FAILED ──".into())], w - 1));
        if !code.is_empty() {
            rows.push(tc::seg(&[(p.error.as_str(), format!("  {}", code))], w - 1));
        }
        for line in wrap(&message, w.saturating_sub(4).max(10)) {
            rows.push(tc::seg(&[(p.msg.as_str(), format!("  {}", line))], w - 1));
        }
        let link = text(d, "errorLink");
        if !link.is_empty() {
            rows.push(tc::seg(&[(p.url.as_str(), format!("  {}", link))], w - 1));
        }
    } else if !text(d, "_error").is_empty() {
        rows.push(String::new());
        rows.push(tc::seg(
            &[(
                p.error.as_str(),
                format!(" detail unavailable: {}", text(d, "_error")),
            )],
            w - 1,
        ));
    } else if d.is_null() {
        rows.push(String::new());
        rows.push(tc::seg(&[(p.dim.as_str(), " loading detail…".into())], w - 1));
    }

    let sha = text(meta, "githubCommitSha");
    if !sha.is_empty() {
        rows.push(String::new());
        rows.push(tc::seg(&[(p.lbl.as_str(), " ── COMMIT ──".into())], w - 1));
        rows.push(tc::seg(
            &[
                (p.sha.as_str(), format!("  {}", &sha[..sha.len().min(7)])),
                (
                    p.branch.as_str(),
                    format!("  {}", text(meta, "githubCommitRef")),
                ),
            ],
            w - 1,
        ));
        let subject = text(meta, "githubCommitMessage");
        let subject = subject.lines().next().unwrap_or("");
        for line in wrap(subject, w.saturating_sub(4).max(10)) {
            rows.push(tc::seg(&[(p.msg.as_str(), format!("  {}", line))], w - 1));
        }
        let who = match text(meta, "githubCommitAuthorName") {
            name if !name.is_empty() => name,
            _ => text(meta, "githubCommitAuthorLogin"),
        };
        if !who.is_empty() {
            rows.push(tc::seg(&[(p.dim.as_str(), format!("  by {}", who))], w - 1));
        }
    }

    // The build log, last. It is the longest thing on the screen and the
    // one most often scrolled to, so everything that fits in a line of its
    // own goes above it.
    if let Some(detail) = detail {
        rows.extend(build_log(detail, w, &p));
    }

    // The copy list is its own page now, reached with c. It was a section
    // at the bottom of this one, which meant the numbers you press were
    // usually scrolled off the screen by the build log above them.
    let pairs: Vec<(String, String)> = Vec::new();
    if !pairs.is_empty() {
        rows.push(String::new());
        rows.push(tc::seg(&[(p.lbl.as_str(), " ── COPY ──".into())], w - 1));
        for (i, (label, value)) in pairs.iter().enumerate() {
            let room = w.saturating_sub(28);
            let short = if value.chars().count() <= room {
                value.clone()
            } else {
                format!(
                    "{}…",
                    value.chars().take(w.saturating_sub(31)).collect::<String>()
                )
            };
            rows.push(tc::seg(
                &[
                    (p.ready.as_str(), format!("  [{}] ", i + 1)),
                    (p.txt.as_str(), format!("{:<21} ", label)),
                    (p.url.as_str(), short),
                ],
                w - 1,
            ));
        }
    }

    let _ = (h, note);
    rows
}

/// The copy list on a page of its own.
///
/// It used to sit under the detail, which was fine until the build log went
/// in above it: the numbers you press were then reliably scrolled off the
/// bottom of the screen you were pressing them from.
fn copy_overlay(
    dep: &serde_json::Value,
    detail: Option<&serde_json::Value>,
    w: usize,
    h: usize,
    note: &str,
    p: &Palette,
) -> Vec<String> {
    let pairs = copy_items(dep, detail);
    let mut rows = vec![tc::title("copy", w, &p.prod)];
    rows.push(String::new());
    if pairs.is_empty() {
        rows.push(tc::seg(
            &[(p.dim.as_str(), "  nothing here to copy yet".into())],
            w - 1,
        ));
    }
    for (i, (label, value)) in pairs.iter().enumerate() {
        let room = w.saturating_sub(28);
        let short = if value.chars().count() <= room {
            value.clone()
        } else {
            format!("{}…", value.chars().take(room.saturating_sub(1)).collect::<String>())
        };
        rows.push(tc::seg(
            &[
                (p.ready.as_str(), format!("  [{}] ", i + 1)),
                (p.txt.as_str(), format!("{:<21} ", label)),
                (p.url.as_str(), short),
            ],
            w - 1,
        ));
    }
    while rows.len() < h.saturating_sub(2) {
        rows.push(String::new());
    }
    rows.push(tc::seg(
        &[(
            p.hint.as_str(),
            if pairs.is_empty() {
                " ← or esc to close".to_string()
            } else {
                format!(" press 1-{} to copy · ← or esc to close", pairs.len())
            },
        )],
        w - 1,
    ));
    rows.push(if note.is_empty() {
        String::new()
    } else {
        tc::seg(&[(p.ready.as_str(), format!(" {}", note))], w - 1)
    });
    rows
}

fn main() {
    tc::maybe_help(include_str!("deployments_help.txt"));
    let cfg = tc::load_config("deployments");
    let mut refresh = tc::cfg_f64(&cfg, "refresh", 15.0);
    let limit = tc::cfg_usize(&cfg, "limit", 100);
    let mut teams = tc::cfg_strings(&cfg, "teams", &[]);
    let configured: Vec<String> = tc::cfg_strings(&cfg, "projects", &[]);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut named: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--refresh" if i + 1 < args.len() => {
                refresh = args[i + 1].parse::<f64>().unwrap_or(15.0).max(5.0);
                i += 2;
            }
            "-t" | "--team" if i + 1 < args.len() => {
                teams.push(args[i + 1].clone());
                i += 2;
            }
            other if !other.starts_with('-') => {
                named.push(other.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }
    let projects: HashSet<String> = if named.is_empty() {
        configured.into_iter().collect()
    } else {
        named.into_iter().collect()
    };

    let absent = tc::missing(&["curl"]);
    if !absent.is_empty() {
        tc::cannot_start(
            "vercel deployments",
            &absent,
            &[
                "Everything here comes from Vercel's HTTP API, and curl is how",
                "this reaches it - the same way the other widgets reach ss,",
                "ping and tailscale.",
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
    let (tok, source) = token(&cfg);
    let env_name = {
        let name = tc::cfg_str(&cfg, "token_env", "VERCEL_TOKEN");
        if name.is_empty() { "VERCEL_TOKEN".to_string() } else { name }
    };
    if !tok.is_empty() && teams.is_empty() {
        teams = discover_teams(&tok);
    }

    let state = Arc::new(Mutex::new(State::default()));
    let wake = Arc::new((Mutex::new(false), Condvar::new()));
    let poller = Arc::clone(&state);
    let poller_wake = Arc::clone(&wake);
    let poll_teams = teams.clone();
    let poll_projects = projects.clone();
    let poll_token = tok.clone();
    let poll_env = env_name.clone();
    std::thread::spawn(move || loop {
        if poll_token.is_empty() {
            if let Ok(mut guard) = poller.lock() {
                guard.err = format!(
                    "no token: set deployments.token in config.json, or ${}",
                    poll_env
                );
            }
        } else {
            let mut out: Vec<serde_json::Value> = Vec::new();
            // A file others can read is worth saying out loud, since this
            // is the widget that put a token in it.
            let mut err = if source == "config" {
                tc::config_token_warning().unwrap_or_default()
            } else {
                String::new()
            };
            let scopes: Vec<String> = if poll_teams.is_empty() {
                vec![String::new()]
            } else {
                poll_teams.clone()
            };
            for team in &scopes {
                let mut path = format!("/v6/deployments?limit={}", limit);
                if !team.is_empty() {
                    path += &format!("&teamId={}", team);
                }
                match api(&path, &poll_token) {
                    Ok(res) => {
                        for d in res["deployments"].as_array().into_iter().flatten() {
                            let mut d = d.clone();
                            // Carried so the detail request knows its scope.
                            d["_team"] = serde_json::Value::String(team.clone());
                            out.push(d);
                        }
                    }
                    Err(said) => {
                        err = if said.contains("401") || said.contains("403") {
                            format!("{} (token expired? make a new one)", said)
                        } else {
                            said
                        };
                    }
                }
            }
            if !poll_projects.is_empty() {
                out.retain(|d| poll_projects.contains(&text(d, "name")));
            }
            out.sort_by(|a, b| {
                ms_at(b, "created")
                    .unwrap_or(0.0)
                    .total_cmp(&ms_at(a, "created").unwrap_or(0.0))
            });
            if let Ok(mut guard) = poller.lock() {
                // A failed round keeps the last good list rather than
                // blanking the board: stale rows with a message beside them
                // say more than an empty screen does.
                if !out.is_empty() || err.is_empty() {
                    guard.deployments = out;
                    guard.fetched = now();
                }
                guard.err = err;
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
    let details: Arc<Mutex<HashMap<String, serde_json::Value>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let fetching: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut filter = 0usize;
    let mut only: Option<String> = None;
    let (mut tick, mut selected, mut scroll) = (0usize, 0usize, 0usize);
    let mut overlay = false;
    // The copy list is a page of its own, opened from the detail with c.
    let mut copying = false;
    // How far down the detail is scrolled. The build log makes it taller
    // than any pane, so it has to move.
    let mut oscroll = 0usize;
    let mut note: (String, f64) = (String::new(), 0.0);
    let mut visible = 1usize;
    let mut shown: Vec<serde_json::Value> = Vec::new();

    loop {
        tick += 1;
        for key in keyboard.poll() {
            if overlay {
                match key.as_str() {
                    // Left and esc come out. q quits outright, which is
                    // what the footer has always promised and what it
                    // does from the list - it used to close the overlay
                    // instead, so the key disagreed with its own hint.
                    "left" | "esc" => {
                        if copying {
                            copying = false;
                        } else {
                            overlay = false;
                        }
                    }
                    "c" | "C" if !copying => copying = true,
                    "up" | "k" | "K" if !copying => oscroll = oscroll.saturating_sub(1),
                    "down" | "j" | "J" if !copying => oscroll = oscroll.saturating_add(1),
                    "pgup" if !copying => {
                        let page = tc::size().1.saturating_sub(3).max(1);
                        oscroll = oscroll.saturating_sub(page);
                    }
                    "pgdn" if !copying => {
                        let page = tc::size().1.saturating_sub(3).max(1);
                        oscroll = oscroll.saturating_add(page);
                    }
                    "home" if !copying => oscroll = 0,
                    "end" if !copying => oscroll = usize::MAX,
                    // The detail is a live thing too - a running build's log
                    // grows while you read it. r drops what was fetched so
                    // the next frame asks again.
                    "r" | "R" if !copying => {
                        if let Some(chosen) = shown.get(selected.min(shown.len().saturating_sub(1)))
                        {
                            let uid = text(chosen, "uid");
                            if let Ok(mut g) = details.lock() {
                                g.remove(&uid);
                            }
                        }
                    }
                    "q" | "Q" => {
                        keyboard.restore();
                        tc::restore_screen();
                        return;
                    }
                    digit if digit.len() == 1 && digit.chars().all(|c| c.is_ascii_digit()) => {
                        if let Some(chosen) = shown.get(selected.min(shown.len().saturating_sub(1)))
                        {
                            let uid = text(chosen, "uid");
                            let held = details.lock().ok().and_then(|g| g.get(&uid).cloned());
                            let pairs = copy_items(chosen, held.as_ref());
                            let at = digit.parse::<usize>().unwrap_or(0);
                            if at >= 1 && at <= pairs.len() {
                                let (label, value) = &pairs[at - 1];
                                note = (
                                    if tc::clipboard(value) {
                                        format!("✓ copied {}", label.to_lowercase())
                                    } else {
                                        "! no clipboard; select the text with the mouse".into()
                                    },
                                    now() + 3.0,
                                );
                            }
                        }
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
                "f" | "F" => {
                    filter = (filter + 1) % FILTERS.len();
                    selected = 0;
                }
                "up" => selected = selected.saturating_sub(1),
                "down" => selected += 1,
                "pgup" => selected = selected.saturating_sub(visible),
                "pgdn" => selected += visible,
                "home" => selected = 0,
                "end" => selected = shown.len().saturating_sub(1),
                "right" | "enter" => {
                    if !shown.is_empty() {
                        overlay = true;
                        copying = false;
                        oscroll = 0;
                        note = (String::new(), 0.0);
                    }
                }
                "p" | "P" => {
                    let names: Vec<String> = {
                        let guard = match state.lock() {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                        let mut seen: Vec<String> = guard
                            .deployments
                            .iter()
                            .map(|d| text(d, "name"))
                            .filter(|n| !n.is_empty())
                            .collect();
                        seen.sort();
                        seen.dedup();
                        seen
                    };
                    // Cycles through every project and back to no filter,
                    // so the key always has somewhere to go.
                    only = match &only {
                        None => names.first().cloned(),
                        Some(current) => match names.iter().position(|n| n == current) {
                            Some(at) if at + 1 < names.len() => Some(names[at + 1].clone()),
                            Some(_) => None,
                            None => names.first().cloned(),
                        },
                    };
                    selected = 0;
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let (deps, err, fetched) = match state.lock() {
            Ok(g) => (g.deployments.clone(), g.err.clone(), g.fetched),
            Err(_) => return,
        };
        if !note.0.is_empty() && now() > note.1 {
            note = (String::new(), 0.0);
        }
        shown = deps.clone();
        if let Some(name) = &only {
            shown.retain(|d| text(d, "name") == *name);
        }
        match FILTERS[filter] {
            "failed" => shown.retain(|d| {
                let s = text(d, "state");
                s == "ERROR" || s == "CANCELED"
            }),
            "production" => shown.retain(|d| text(d, "target") == "production"),
            _ => {}
        }
        if !shown.is_empty() && selected >= shown.len() {
            selected = shown.len() - 1;
        }

        if overlay && !shown.is_empty() {
            let chosen = shown[selected].clone();
            let uid = text(&chosen, "uid");
            let mut held = details.lock().ok().and_then(|g| g.get(&uid).cloned());
            // A build that is still running writes more log while you read
            // it, so what was fetched when the page opened stops being the
            // whole story. Dropped after DETAIL_TTL so the next frame asks
            // again; a finished deployment is refetched too, which costs one
            // request a minute and keeps the rule simple.
            if held
                .as_ref()
                .map(|v| now() - v["_fetched_at"].as_f64().unwrap_or(0.0) > DETAIL_TTL)
                .unwrap_or(false)
            {
                if let Ok(mut g) = details.lock() {
                    g.remove(&uid);
                }
                held = None;
            }
            if held.is_none() && !uid.is_empty() {
                let start = fetching
                    .lock()
                    .map(|mut g| g.insert(uid.clone()))
                    .unwrap_or(false);
                if start {
                    let (details, fetching) = (Arc::clone(&details), Arc::clone(&fetching));
                    let (uid, team, tok) = (uid.clone(), text(&chosen, "_team"), tok.clone());
                    std::thread::spawn(move || {
                        let got = fetch_detail(&uid, &team, &tok);
                        if let Ok(mut g) = details.lock() {
                            g.insert(uid.clone(), got);
                        }
                        if let Ok(mut g) = fetching.lock() {
                            g.remove(&uid);
                        }
                    });
                }
            }
            let rows = if copying {
                copy_overlay(&chosen, held.as_ref(), w, h, &note.0, &p)
            } else {
                let body = info_overlay(&chosen, held.as_ref(), w, h, &note.0, &p);
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
                                " ↑↓ scroll {}-{} of {} · [c]opy · [r]efresh · ← or esc to close",
                                oscroll + 1,
                                last,
                                body.len()
                            )
                        } else {
                            " [c]opy · [r]efresh · ← or esc to close".to_string()
                        },
                    )],
                    w - 1,
                ));
                out.push(if note.0.is_empty() {
                    String::new()
                } else {
                    tc::seg(&[(p.ready.as_str(), format!(" {}", note.0))], w - 1)
                });
                out
            };
            tc::draw(&rows, w, h);
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        let mut states: HashMap<String, usize> = HashMap::new();
        for d in &deps {
            *states.entry(text(d, "state")).or_insert(0) += 1;
        }
        let seen_projects: HashSet<String> = deps.iter().map(|d| text(d, "name")).collect();
        let live: usize = LIVE
            .iter()
            .map(|s| states.get(*s).copied().unwrap_or(0))
            .sum();

        let mut rows = vec![tc::title("vercel deployments", w, &p.prod)];
        let mut head = vec![
            (p.dim.as_str(), format!(" {} deploys", deps.len())),
            (p.dim.as_str(), format!(" · {} proj", seen_projects.len())),
            (
                p.ready.as_str(),
                format!("  {} ready", states.get("READY").copied().unwrap_or(0)),
            ),
        ];
        if let Some(n) = states.get("ERROR") {
            head.push((p.error.as_str(), format!("  {} error", n)));
        }
        if live > 0 {
            head.push((
                p.build.as_str(),
                format!("  {} {} building", SPINNER[tick % SPINNER.len()], live),
            ));
        }
        head.push((
            p.dim.as_str(),
            format!(
                "   {} ago",
                if fetched > 0.0 {
                    age(Some(fetched * 1000.0))
                } else {
                    "--".into()
                }
            ),
        ));
        rows.push(tc::seg(&head, w - 1));
        if !err.is_empty() {
            rows.push(tc::seg(&[(p.error.as_str(), format!(" ! {}", err))], w - 1));
        }
        let mut bits: Vec<String> = Vec::new();
        if FILTERS[filter] != "all" {
            bits.push(FILTERS[filter].to_string());
        }
        if let Some(name) = &only {
            bits.push(name.clone());
        }
        if !bits.is_empty() {
            rows.push(tc::seg(
                &[(p.build.as_str(), format!(" filter: {}", bits.join(" + ")))],
                w - 1,
            ));
        }
        rows.push(String::new());

        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── ACTIVITY ── ".into()),
                (p.dim.as_str(), "deploys/hour, last 48h".into()),
            ],
            w - 1,
        ));
        let (chart, peak) = activity(&deps, w, 48.0, &p);
        rows.push(chart);
        if peak > 0 {
            rows.push(tc::seg(
                &[
                    (p.dim.as_str(), " 48h ago".into()),
                    (p.dim.as_str(), " ".repeat(w.saturating_sub(22).max(1))),
                    (p.dim.as_str(), format!("peak {}/h", peak)),
                ],
                w - 1,
            ));
        }

        let mut durs: Vec<f64> = deps.iter().filter_map(build_seconds).collect();
        durs.sort_by(f64::total_cmp);
        if !durs.is_empty() {
            let med = durs[durs.len() / 2];
            let p95 = durs[(durs.len() - 1).min((durs.len() as f64 * 0.95) as usize)];
            rows.push(String::new());
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── BUILD TIME ── ".into()),
                    (p.dim.as_str(), "median ".into()),
                    (p.txt.as_str(), dur(Some(med))),
                    (p.dim.as_str(), "  p95 ".into()),
                    (p.txt.as_str(), dur(Some(p95))),
                    (p.dim.as_str(), "  max ".into()),
                    (p.txt.as_str(), dur(durs.last().copied())),
                ],
                w - 1,
            ));
            let recent: Vec<f64> = deps
                .iter()
                .take(w.saturating_sub(2).max(10))
                .filter_map(build_seconds)
                .collect::<Vec<f64>>()
                .into_iter()
                .rev()
                .collect();
            if !recent.is_empty() {
                let hi = recent.iter().cloned().fold(0.0f64, f64::max).max(1e-9);
                let spark: String = recent
                    .iter()
                    .map(|x| SPARK[(((x / hi) * 7.99) as usize).min(7)])
                    .collect();
                rows.push(tc::seg(&[(p.ready.as_str(), format!(" {}", spark))], w - 1));
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
        visible = (h.saturating_sub(rows.len() + 1) / per_item).max(1);
        scroll = scroll.min(shown.len().saturating_sub(visible));
        if selected < scroll {
            scroll = selected;
        } else if selected >= scroll + visible {
            scroll = selected - visible + 1;
        }
        for (i, d) in shown.iter().enumerate().skip(scroll).take(visible) {
            if rows.len() >= h.saturating_sub(1) {
                break;
            }
            let here = i == selected;
            let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
            let meta = &d["meta"];
            let state = text(d, "state");
            let colour = state_colour(&state, &p);
            let mark = if LIVE.contains(&state.as_str()) {
                SPINNER[tick % SPINNER.len()]
            } else if state == "READY" {
                '●'
            } else if state == "ERROR" {
                '✖'
            } else {
                '○'
            };
            let subject = text(meta, "githubCommitMessage");
            let subject = subject.lines().next().unwrap_or("").to_string();
            let c = |colour: &str| format!("{}{}", tint, colour);
            let mut line = vec![
                (
                    c(colour),
                    format!(
                        "{}{} {:<9}",
                        if here { "▸" } else { " " },
                        mark,
                        titled(&state)
                    ),
                ),
                (c(&p.txt), tc::pad(&text(d, "name"), cols.project)),
                (c(&p.dim), dur(build_seconds(d))),
                (c(&p.dim), format!(" {:>4}", age(ms_at(d, "created")))),
            ];
            if text(d, "target") == "production" {
                line.push((c(&p.prod), "  PROD".into()));
            } else if cols.detail {
                line.push((c(&p.dim), "  prev".into()));
            }
            if cols.detail {
                let sha = text(meta, "githubCommitSha");
                line.push((
                    c(&p.sha),
                    format!("  {}", &sha[..sha.len().min(7)]),
                ));
                line.push((
                    c(&p.branch),
                    format!(" {}", tc::pad(&text(meta, "githubCommitRef"), cols.branch)),
                ));
            }
            if cols.single {
                line.push((c(if here { &p.txt } else { &p.msg }), format!(" {}", subject)));
            }
            if here {
                line.push((tint.clone(), " ".repeat(w)));
            }
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
            if !cols.single && rows.len() < h.saturating_sub(1) {
                rows.push(tc::seg(
                    &[
                        (&c(if here { &p.txt } else { &p.msg }), format!("   {}", subject)),
                        (&tint, if here { " ".repeat(w) } else { String::new() }),
                    ],
                    w - 1,
                ));
            }
        }
        if shown.is_empty() {
            rows.push(tc::seg(
                &[(p.dim.as_str(), "   (nothing matches the current filter)".into())],
                w - 1,
            ));
        }

        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
            vec![
                (p.accent.as_str(), "→/↵".into()),
                (p.dim.as_str(), " details".into()),
            ],
            vec![(p.dim.as_str(), "[f]ilter".into())],
            vec![(p.dim.as_str(), "[p]roject".into())],
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The line that says why a build failed is the one worth finding, and
    /// it arrives on stderr among dozens of stdout lines that all look the
    /// same. Vercel marks them; this checks we keep the mark.
    #[test]
    fn the_build_log_picks_stderr_out_of_stdout() {
        let p = palette();
        let detail = serde_json::json!({
            "_events": [
                {"type": "stdout", "text": "Running build in Washington, D.C."},
                {"type": "stdout", "text": "Build machine configuration: 4 cores"},
                {"type": "stderr", "text": "Error: No Next.js version detected."},
                {"type": "stdout", "text": ""},
            ]
        });
        let rows = build_log(&detail, 90, &p);
        let joined = rows.join("\n");
        assert!(joined.contains("BUILD LOG"), "no heading");
        assert!(joined.contains("Error: No Next.js version"), "the failing line is missing");
        assert!(joined.contains("Running build in"), "the ordinary lines are missing");

        let of = |needle: &str| {
            rows.iter()
                .find(|r| r.contains(needle))
                .unwrap_or_else(|| panic!("{:?} not drawn", needle))
        };
        assert!(
            of("Error: No Next.js").starts_with(p.error.as_str()),
            "stderr was not marked"
        );
        assert!(
            of("Running build in").starts_with(p.dim.as_str()),
            "stdout was marked as an error"
        );
        // A blank line from the build is not a row on the screen.
        assert_eq!(joined.matches("  \n").count(), 0);
        // The count in the heading is the lines the build produced, blank
        // ones included - it is what the API returned, not what we drew.
        assert!(of("BUILD LOG").contains("4 lines"), "{}", of("BUILD LOG"));
    }

    #[test]
    fn a_log_that_would_not_load_says_so_rather_than_vanishing() {
        let p = palette();
        let detail = serde_json::json!({ "_events": { "_error": "curl exited 22" } });
        let rows = build_log(&detail, 90, &p);
        let joined = rows.join("\n");
        assert!(joined.contains("BUILD LOG"), "the section disappeared");
        assert!(joined.contains("curl exited 22"), "the reason disappeared");
        // And a deployment with no events at all draws nothing, rather than
        // an empty heading promising a log that is not coming.
        assert!(build_log(&serde_json::json!({}), 90, &p).is_empty());
        assert!(build_log(&serde_json::json!({"_events": []}), 90, &p).is_empty());
    }


    #[test]
    fn a_token_prefers_the_config_then_the_environment() {
        let cfg: serde_json::Value =
            serde_json::from_str(r#"{"token": "from-config", "token_env": "NOPE_TOKEN"}"#).unwrap();
        assert_eq!(token(&cfg), ("from-config".to_string(), "config"));
        // An empty token in the config is not a token.
        let bare: serde_json::Value = serde_json::from_str(r#"{"token": ""}"#).unwrap();
        assert_eq!(token(&bare).1, "missing");
    }

    #[test]
    fn a_build_time_covers_the_one_still_running() {
        // Finished: the gap between the two stamps.
        let done: serde_json::Value =
            serde_json::from_str(r#"{"state": "READY", "buildingAt": 1000, "ready": 46000}"#)
                .unwrap();
        assert_eq!(build_seconds(&done), Some(45.0));
        // Never started: nothing to report rather than zero.
        let queued: serde_json::Value = serde_json::from_str(r#"{"state": "QUEUED"}"#).unwrap();
        assert_eq!(build_seconds(&queued), None);
        // Finished but with no start stamp is also nothing, not a huge number.
        let odd: serde_json::Value =
            serde_json::from_str(r#"{"state": "READY", "ready": 46000}"#).unwrap();
        assert_eq!(build_seconds(&odd), None);
    }

    #[test]
    fn a_duration_keeps_its_column_width() {
        assert_eq!(dur(None), "  --  ");
        assert_eq!(dur(Some(45.0)), "   45s");
        assert_eq!(dur(Some(45.0)).chars().count(), 6);
        assert_eq!(dur(None).chars().count(), 6);
        // Past a minute it changes shape, as the Python does.
        assert_eq!(dur(Some(125.0)), "2m05s");
    }

    #[test]
    fn the_copy_sheet_lists_only_what_exists() {
        let dep: serde_json::Value = serde_json::from_str(
            r#"{"inspectorUrl": "https://vercel.com/x", "url": "abc.vercel.app",
                "meta": {"githubCommitSha": "0123456789", "githubOrg": "o",
                         "githubRepo": "r", "githubPrId": "7"}}"#,
        )
        .unwrap();
        let items = copy_items(&dep, None);
        let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Deployment dashboard",
                "Commit preview",
                "Pull request",
                "Commit SHA"
            ]
        );
        // A pull request needs all three parts, not just the number.
        assert_eq!(items[2].1, "https://github.com/o/r/pull/7");
        // No branch alias, so no branch preview - an empty row would be a
        // key that copies nothing.
        assert!(!labels.contains(&"Branch preview"));
    }

    #[test]
    fn width_buys_content_rather_than_padding() {
        assert!(!columns(60).detail);
        assert!(columns(66).detail);
        assert!(!columns(100).single);
        assert!(columns(110).single);
        // The project column grows in steps and the branch scales, but
        // neither runs away with a very wide pane.
        assert_eq!(columns(70).project, 12);
        assert_eq!(columns(200).project, 20);
        assert_eq!(columns(200).branch, 34);
        assert_eq!(columns(40).branch, 12);
    }

    #[test]
    fn a_state_reads_as_a_word() {
        assert_eq!(titled("READY"), "Ready");
        assert_eq!(titled("INITIALIZING"), "Initializing");
        assert_eq!(titled(""), "");
    }
}
