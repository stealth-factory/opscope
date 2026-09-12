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

//! Everything that turns a UHP answer into rows.
//!
//! Pure, so it compiles and is tested on every target: the only part of
//! this widget that touches the machine is spawning `luvus`, and that is
//! the same command on Linux and macOS. Nothing here is `cfg`-gated.

use serde_json::Value;

/// Why there is no session to read, told apart so the screen can say which.
///
/// Three answers rather than one, because an empty board is the same
/// picture for all three and the reader cannot tell a missing program from
/// a stopped server from a session with nothing open in it. That
/// indistinguishability is the failure this widget is built to avoid.
#[derive(Clone, Debug, PartialEq)]
pub enum Absence {
    /// No `luvus` on PATH. The shared dependency screen catches this before
    /// the terminal starts, but a program can also go away while we run.
    NoBinary,
    /// `luvus` is here and there is no server answering for this session.
    NoServer,
    /// It answered, and the answer was not one we can use.
    Other(String),
}

/// Which of the three a failed command was.
///
/// Read off the message rather than the exit status, because `luvus` exits
/// 1 for both a stopped server and a bad request, and the difference is the
/// whole point of the type.
pub fn parse_failure(message: &str) -> Absence {
    let said = message.to_ascii_lowercase();
    if said.contains("no luvus server")
        || said.contains("no server running")
        || said.contains("connection refused")
    {
        return Absence::NoServer;
    }
    // The missing program is read off the one shape core produces when a
    // spawn fails - the program name, a colon, and the operating system's
    // reason - and not off the words "not found" wherever they fall. A
    // future `Error: pane not found` is luvus answering, not luvus being
    // absent, and drawing "no luvus on PATH" over it would send the reader
    // to install something they already have.
    if said.starts_with("luvus:") && (said.contains("no such file") || said.contains("os error 2"))
    {
        return Absence::NoBinary;
    }
    // Luvus names its socket in the stopped-server message, and this
    // repository is public. Anything else it says is kept, up to that
    // parenthesis, so a screenshot cannot carry a path out of the machine.
    Absence::Other(sanitize_error(message))
}

/// Drop the socket path Luvus names in a stopped-server message.
///
/// This repository is public and screenshots of the pane are not.
/// Everything up to the parenthesis is kept, so the reason still
/// reaches the row.
pub fn sanitize_error(message: &str) -> String {
    message
        .split(" (socket:")
        .next()
        .unwrap_or(message)
        .trim()
        .to_string()
}

/// The `result` object out of one UHP answer, or why there is none.
///
/// Split from the running of the command, because the running is not where
/// every failure shows: UHP answers a request it cannot serve with an
/// `error` object, and a request that is merely empty with a `result`
/// holding an empty list. Those are opposite readings and they are told
/// apart by shape here or they are not told apart at all.
pub fn parse_result(text: &str) -> Result<Value, String> {
    let parsed: Value =
        serde_json::from_str(text).map_err(|e| format!("unreadable answer: {}", e))?;
    if let Some(said) = parsed.get("error") {
        let message = said["message"].as_str().unwrap_or("").trim();
        return Err(if message.is_empty() {
            format!("luvus said {}", said)
        } else {
            message.to_string()
        });
    }
    match parsed.get("result") {
        Some(Value::Null) | None => Err("luvus answered with no result".into()),
        Some(value) => Ok(value.clone()),
    }
}

fn text_at(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_string()
}

/// A string field that may be a string, a number, or absent.
///
/// Pane, tab and workspace ids arrive as strings in `agent list` and as
/// strings in the snapshot, but task and lease ids have not been seen on a
/// live session at all. Reading a number as a number rather than as nothing
/// costs one branch and saves a row that says a task has no id when it has.
fn id_at(value: &Value, key: &str) -> String {
    match &value[key] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The first of several spellings that is present.
///
/// `task list` and `lease list` are empty on every session this was built
/// against, so the field names are read from the CLI's own vocabulary and
/// more than one plausible spelling is accepted. A row drawn from whichever
/// arrives is honest; a row that insists on one spelling and shows a blank
/// is not.
fn any_of(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        let found = id_at(value, key);
        if !found.is_empty() {
            return found;
        }
    }
    String::new()
}

fn list_at(value: &Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        if let Some(items) = value[*key].as_array() {
            let found: Vec<String> = items
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if !found.is_empty() {
                return found;
            }
        }
    }
    Vec::new()
}

/// The named list in a successful envelope, or why it is not a list.
///
/// An omitted or non-array field is not an empty reading. Zero agents and
/// a response that never named them are opposite answers, and treating
/// the second as the first is how a malformed source draws as a quiet one.
fn array_at<'a>(result: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    match result.get(key) {
        Some(Value::Array(items)) => Ok(items),
        Some(_) => Err(format!("luvus answered with {} that is not a list", key)),
        None => Err(format!("luvus answered with no {}", key)),
    }
}

/// A coding agent under the session, as `agent list` reports it.
///
/// `since` and `exact` are not in the answer: UHP does not timestamp a
/// state change, so how long a state has held is measured here from the
/// first poll that saw it, and `exact` is false for a state that was
/// already in place when the widget started.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Agent {
    pub name: String,
    pub kind: String,
    pub pane: String,
    pub workspace: String,
    pub project: String,
    pub branch: String,
    pub cwd: String,
    pub state: String,
    /// How the *identity* was decided: `process_tree`, `osc_title`, ...
    pub authority: String,
    /// How the *state* was decided: `manifest_rule`, `screen_text`, ...
    pub state_source: String,
    pub worktree: bool,
    pub focused: bool,
    pub since: f64,
    pub exact: bool,
}

/// Every agent in `luvus agent list --json`.
///
/// `agent list` is what decides who is an agent, and the snapshot is not.
/// Every pane in a snapshot carries an `agent` field, and a plain shell at
/// a prompt arrives as `agent: "bash"` with `agent_authority:
/// "command_fallback"` — so reading the snapshot for agents turns every
/// idle shell into an idle agent.
pub fn parse_agents(text: &str) -> Result<Vec<Agent>, String> {
    let result = parse_result(text)?;
    let mut found = Vec::new();
    for entry in array_at(&result, "agents")? {
        let kind = text_at(entry, "agent");
        let named = text_at(entry, "name");
        found.push(Agent {
            name: if named.is_empty() {
                kind.clone()
            } else {
                named
            },
            kind,
            pane: id_at(entry, "pane"),
            workspace: text_at(entry, "workspace_name"),
            project: text_at(entry, "project"),
            branch: text_at(entry, "branch"),
            cwd: text_at(entry, "cwd"),
            state: match entry["status"].as_str() {
                Some(s) if !s.is_empty() => s.to_string(),
                // The protocol declares four states. A fifth answer, or
                // none, is not one of them and must not be filed as idle.
                _ => "unknown".to_string(),
            },
            authority: text_at(entry, "authority"),
            state_source: text_at(entry, "state_source"),
            worktree: entry["worktree"].as_bool().unwrap_or(false),
            focused: entry["focused"].as_bool().unwrap_or(false),
            since: 0.0,
            exact: false,
        });
    }
    Ok(found)
}

/// One pane of the session, from the snapshot.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Pane {
    pub pane_id: String,
    pub workspace: String,
    pub branch: String,
    pub tab: String,
    pub kind: String,
    pub cwd: String,
    /// What luvus says is in the pane — `bash`, `npm`, an agent's name.
    pub command: String,
    /// One of the protocol's four states, or empty when it reported none.
    pub status: String,
    pub authority: String,
    pub focused: bool,
}

/// One workspace of the session, as the snapshot describes it.
///
/// `index` is what the per-workspace calls take: `git status` and
/// `worktree list` both answer for one workspace and reject a directory
/// that is not a checkout, so the index has to survive the snapshot or
/// every one of those calls needs a second round trip to find it.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Workspace {
    pub index: String,
    pub name: String,
    pub cwd: String,
    pub branch: String,
    pub active: bool,
}

/// A whole session, as one `luvus uhp snapshot` describes it.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Snapshot {
    pub session: String,
    pub protocol: String,
    pub sequence: u64,
    pub spaces: Vec<Workspace>,
    pub panes: Vec<Pane>,
}

/// The session out of one `luvus uhp snapshot`.
pub fn parse_snapshot(text: &str) -> Result<Snapshot, String> {
    let result = parse_result(text)?;
    let protocol = match (&result["protocol"]["major"], &result["protocol"]["minor"]) {
        (Value::Number(a), Value::Number(b)) => format!("{}.{}", a, b),
        _ => String::new(),
    };
    let mut panes = Vec::new();
    let mut spaces = Vec::new();
    let listed = array_at(&result, "workspaces")?;
    for workspace in listed {
        let name = text_at(workspace, "name");
        let branch = text_at(workspace, "branch");
        spaces.push(Workspace {
            index: id_at(workspace, "index"),
            name: name.clone(),
            cwd: text_at(workspace, "cwd"),
            branch: branch.clone(),
            active: workspace["active"].as_bool().unwrap_or(false),
        });
        for tab in workspace["tabs"].as_array().into_iter().flatten() {
            let tab_id = id_at(tab, "index");
            for pane in tab["panes"].as_array().into_iter().flatten() {
                panes.push(Pane {
                    pane_id: id_at(pane, "pane_id"),
                    workspace: name.clone(),
                    branch: branch.clone(),
                    tab: tab_id.clone(),
                    kind: text_at(pane, "kind"),
                    cwd: text_at(pane, "cwd"),
                    command: text_at(pane, "agent"),
                    status: text_at(pane, "agent_status"),
                    authority: text_at(pane, "agent_authority"),
                    focused: pane["focused"].as_bool().unwrap_or(false),
                });
            }
        }
    }
    Ok(Snapshot {
        session: text_at(&result, "session"),
        protocol,
        sequence: result["event_sequence"].as_u64().unwrap_or(0),
        spaces,
        panes,
    })
}

/// A claimable unit of work, from `luvus task list --json`.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: String,
    pub holder: String,
    pub pane: String,
    pub paths: Vec<String>,
}

/// Every task in `luvus task list --json`.
///
/// An empty list is a reading — most sessions coordinate nothing — and the
/// caller must draw it as *no tasks*. Only a `Err` here is a failure.
pub fn parse_tasks(text: &str) -> Result<Vec<Task>, String> {
    let result = parse_result(text)?;
    let mut found = Vec::new();
    for entry in array_at(&result, "tasks")? {
        found.push(Task {
            id: any_of(entry, &["id", "task", "task_id"]),
            title: any_of(entry, &["title", "name", "description"]),
            status: any_of(entry, &["status", "state"]),
            holder: any_of(entry, &["agent", "assignee", "claimed_by", "owner"]),
            pane: any_of(entry, &["pane", "pane_id"]),
            paths: list_at(entry, &["paths", "globs"]),
        });
    }
    Ok(found)
}

/// A reservation over file paths held by an unfinished task.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Lease {
    pub id: String,
    pub task: String,
    pub holder: String,
    pub pane: String,
    pub paths: Vec<String>,
}

/// Every lease in `luvus lease list --json`.
pub fn parse_leases(text: &str) -> Result<Vec<Lease>, String> {
    let result = parse_result(text)?;
    let mut found = Vec::new();
    for entry in array_at(&result, "leases")? {
        found.push(Lease {
            id: any_of(entry, &["id", "lease", "lease_id"]),
            task: any_of(entry, &["task", "task_id"]),
            holder: any_of(entry, &["agent", "holder", "owner"]),
            pane: any_of(entry, &["pane", "pane_id"]),
            paths: list_at(entry, &["paths", "globs"]),
        });
    }
    Ok(found)
}

/// Worst first: the states that want a human are the reason to look.
///
/// `blocked` above `done` and not the other way round: blocked is waiting
/// on you right now, while done is waiting to be noticed.
pub const RANK: &[&str] = &["blocked", "done", "working", "idle", "unknown"];

pub fn rank_of(state: &str) -> usize {
    RANK.iter().position(|s| *s == state).unwrap_or(9)
}

/// Keep the end of a path, marking the cut so it does not read as a name.
pub fn tail_path(path: &str, n: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= n || n < 2 {
        return path.to_string();
    }
    format!(
        "…{}",
        chars[chars.len() - (n - 1)..].iter().collect::<String>()
    )
}

/// A duration as this widget says it.
///
/// Between an hour and a day it carries the minutes too: an agent blocked
/// for "3h" and one blocked for "3h58m" are the same number of hours and a
/// very different amount of ignoring.
pub fn ago(seconds: f64) -> String {
    let s = seconds.max(0.0) as i64;
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h{:02}m", s / 3600, s % 3600 / 60)
    } else {
        format!("{}d", s / 86400)
    }
}

/// The home-relative form of a directory, which is how a person names it.
pub fn homely(path: &str) -> String {
    shorten_home(path, &std::env::var("HOME").unwrap_or_default())
}

/// Whether `cwd` is the workspace root or a directory under it.
///
/// Component-aware, so a sibling whose name merely starts with the
/// workspace path is not claimed as inside it. Agents are often started
/// in a crate under the repo, and exact equality would call those away.
pub fn cwd_is_inside(cwd: &str, root: &str) -> bool {
    if root.is_empty() {
        return false;
    }
    let cwd = std::path::Path::new(cwd);
    let root = std::path::Path::new(root);
    cwd == root || cwd.starts_with(root)
}

/// HOME is replaced only on a path-component boundary, so a sibling whose
/// name merely starts with the home path is not claimed as under it.
fn shorten_home(path: &str, home: &str) -> String {
    if home.is_empty() {
        return path.to_string();
    }
    let path = std::path::Path::new(path);
    let home = std::path::Path::new(home);
    if let Ok(rest) = path.strip_prefix(home.join("projects")) {
        if !rest.as_os_str().is_empty() {
            return rest.to_string_lossy().into_owned();
        }
    }
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// What `task next` answers: the one task an agent could claim right now.
///
/// `task list` says what exists; this says what is not waiting on anything
/// else. Both can be non-empty at once with nothing claimable, and a header
/// that showed only a count would read as though something were ready.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct NextTask {
    pub id: String,
    pub title: String,
    /// Luvus answered `type: "none"` — nothing is ready. An answer, not an
    /// absence, and the only shape this has been seen in on a live session.
    pub none: bool,
}

/// The claimable task out of `luvus task next`.
pub fn parse_next_task(text: &str) -> Result<NextTask, String> {
    let result = parse_result(text)?;
    if text_at(&result, "type") == "none" {
        return Ok(NextTask {
            none: true,
            ..Default::default()
        });
    }
    // Never seen populated on a live session, so the same several-spellings
    // rule the task list uses applies here.
    let task = if result["task"].is_object() {
        &result["task"]
    } else {
        &result
    };
    Ok(NextTask {
        id: any_of(task, &["id", "task", "task_id"]),
        title: any_of(task, &["title", "name", "description"]),
        none: false,
    })
}

/// One checkout of a repository, as `worktree list` reports it.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Worktree {
    pub path: String,
    pub branch: String,
    pub head: String,
    /// The primary checkout. The others are linked worktrees, where `.git`
    /// is a file rather than a directory.
    pub main: bool,
}

/// Every checkout of the workspace's repository.
///
/// An empty list is a reading: a workspace whose directory is not a
/// repository has no worktrees, and that is not a failure. The caller tells
/// the two apart with [`is_not_a_repo`].
pub fn parse_worktrees(text: &str) -> Result<Vec<Worktree>, String> {
    let result = parse_result(text)?;
    let mut found = Vec::new();
    for entry in array_at(&result, "worktrees")? {
        found.push(Worktree {
            path: text_at(entry, "path"),
            branch: text_at(entry, "branch"),
            head: text_at(entry, "head"),
            main: entry["main"].as_bool().unwrap_or(false),
        });
    }
    Ok(found)
}

/// An agent with a session on this machine that no pane is holding.
///
/// `agent sessions` answers for the whole machine, not for the session:
/// nine of the ten on the box this was written against were in directories
/// no open workspace covers. `mission.snapshot` reports only the ones
/// inside an open workspace, which is why the two counts differ — they
/// answer different questions rather than disagreeing. Both numbers are
/// kept so the screen can say which it means.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Resumable {
    pub kind: String,
    pub cwd: String,
    pub session_id: String,
    /// Whether the directory is one the session currently has open.
    ///
    /// `None` when the workspace list was never established: a failed
    /// snapshot is not an empty list of workspaces, and treating it as
    /// one would mark every session "not open here".
    pub in_workspace: Option<bool>,
}

/// Every resumable session, marked with whether the session has its
/// directory open.
///
/// `open` is the workspace cwd list when the snapshot answered, and
/// `None` when it did not. Taking it as an argument rather than reading
/// the workspaces here keeps this pure and lets the marking be tested
/// without a server.
pub fn parse_sessions(text: &str, open: Option<&[String]>) -> Result<Vec<Resumable>, String> {
    let result = parse_result(text)?;
    let mut found = Vec::new();
    for entry in array_at(&result, "sessions")? {
        let cwd = text_at(entry, "cwd");
        found.push(Resumable {
            kind: text_at(entry, "agent"),
            in_workspace: open.map(|ws| ws.iter().any(|w| cwd_is_inside(&cwd, w))),
            cwd,
            session_id: any_of(entry, &["session_id", "session", "id"]),
        });
    }
    Ok(found)
}

/// What one workspace's checkout looks like right now.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct GitState {
    pub branch: String,
    pub upstream: String,
    pub ahead: u64,
    pub behind: u64,
    /// Staged, unstaged and untracked entries added together. Untracked
    /// arrives as a directory when the whole directory is untracked, so
    /// this counts entries git reported and not files on disk — which is
    /// why the screen says `entries` and never `files`.
    pub dirty: usize,
    pub stashes: usize,
}

/// The checkout state out of `luvus git status`.
pub fn parse_git_status(text: &str) -> Result<GitState, String> {
    let result = parse_result(text)?;
    // Lengths, not unique paths: a path that is staged and then edited
    // again is two entries git reported, and the screen says entries.
    // An omitted or non-array list is not an empty one — that is how a
    // partial payload would draw as a cleaner tree than it is.
    let staged = array_at(&result, "staged")?;
    let unstaged = array_at(&result, "unstaged")?;
    let untracked = array_at(&result, "untracked")?;
    let stashes = array_at(&result, "stashes")?;
    Ok(GitState {
        branch: text_at(&result, "branch"),
        upstream: text_at(&result, "upstream"),
        ahead: result["ahead"].as_u64().unwrap_or(0),
        behind: result["behind"].as_u64().unwrap_or(0),
        dirty: staged.len() + unstaged.len() + untracked.len(),
        stashes: stashes.len(),
    })
}

/// Whether a git failure is a permanent property of the directory rather
/// than a reading that failed this time.
///
/// Three shapes wear the same `git_error` code and they are not the same
/// news: a workspace that is not a repository at all will never be one and
/// should draw a dash, a repository whose index was locked mid-poll should
/// draw the failure on its own row and try again, and a clean repository
/// reporting zeroes is an answer. Only the first is settled, so only the
/// first is cached.
pub fn is_not_a_repo(message: &str) -> bool {
    let said = message.to_ascii_lowercase();
    said.contains("not a git repository") || said.contains("not a working tree")
}

/// How the server decided an agent's identity and state, and what it thinks
/// the agent is waiting for.
///
/// The AGENTS row already carries the two one-word authorities. This is the
/// rest of the same answer, and `blocked_hint` is the part the pane has
/// never been able to show: a blocked row says that it is blocked and never
/// what it is blocked on, which is the one question that sends the reader
/// out of the widget.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Explanation {
    pub kind: String,
    pub status: String,
    /// The server can still reach the pane. False is worth drawing: an
    /// agent it cannot reach is not an agent that is idle.
    pub available: bool,
    /// The integration holding authority over this state, when one does.
    /// Empty when the state was inferred rather than reported, which is a
    /// weaker claim and says so.
    pub authority: String,
    pub identity_source: String,
    pub identity_confidence: String,
    pub state_source: String,
    pub state_confidence: String,
    /// Where the rule matched - `title`, `body`. Empty when the state did
    /// not come from reading the screen at all.
    pub rule_region: String,
    pub rule_priority: i64,
    /// What the agent is waiting for, in the server's words. Empty when it
    /// is not waiting, and empty is not the same as "nothing is wrong".
    pub blocked_hint: String,
}

/// What a pane currently shows, out of `luvus agent read`.
pub fn parse_screen(text: &str) -> Result<String, String> {
    let result = parse_result(text)?;
    match result.get("text") {
        Some(Value::String(t)) => Ok(t.clone()),
        // An answer with no text is not a blank screen. The caller draws
        // the difference, so it has to survive the parse.
        _ => Err("luvus answered with no screen text".into()),
    }
}

/// One agent's evidence, out of `luvus agent explain`.
pub fn parse_explanation(text: &str) -> Result<Explanation, String> {
    let result = parse_result(text)?;
    let identity = &result["identity"];
    let state = &result["state_evidence"];
    Ok(Explanation {
        kind: text_at(&result, "agent"),
        status: text_at(&result, "status"),
        // Absent is not false: a server that did not say is not a server
        // saying the pane is gone. Only an explicit false draws as gone.
        available: result["available"].as_bool().unwrap_or(true),
        authority: text_at(&result, "authority"),
        identity_source: text_at(identity, "source"),
        identity_confidence: text_at(identity, "confidence"),
        state_source: text_at(state, "source"),
        state_confidence: text_at(state, "confidence"),
        rule_region: text_at(state, "rule_region"),
        rule_priority: state["rule_priority"].as_i64().unwrap_or(0),
        blocked_hint: any_of(state, &["blocked_hint", "hint", "reason"]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped from a live `luvus uhp snapshot`, with the paths replaced.
    /// A fixture naming a real directory is a fixture that leaks one.
    const SNAPSHOT: &str = r#"{
      "id": "1",
      "result": {
        "event_sequence": 2094,
        "protocol": { "major": 1, "minor": 0, "name": "luvus-uhp" },
        "session": "default",
        "type": "session_snapshot",
        "workspaces": [
          {
            "active": false, "branch": null, "cwd": "/srv/home", "index": 1,
            "name": "home", "pinned": false,
            "tabs": [{ "active": true, "index": 1, "kind": "panes", "panes": [
              { "agent": "bash", "agent_authority": "command_fallback",
                "agent_status": "idle", "content_revision": 3, "cwd": "/srv/home",
                "focused": false, "kind": "terminal", "pane_id": "1",
                "root_process": { "pid": 101 } },
              { "agent": "bash", "agent_authority": "command_fallback",
                "agent_status": "working", "content_revision": 9, "cwd": "/srv/home",
                "focused": false, "kind": "terminal", "pane_id": "5",
                "root_process": { "pid": 105 } }
            ]}]
          },
          {
            "active": true, "branch": "feature/thing", "cwd": "/srv/work/thing",
            "index": 2, "name": "thing", "pinned": false,
            "tabs": [{ "active": true, "index": 1, "kind": "panes", "panes": [
              { "agent": "claude", "agent_authority": "process_tree",
                "agent_status": "working", "content_revision": 41,
                "cwd": "/srv/work/thing", "focused": true, "kind": "terminal",
                "pane_id": "2", "root_process": { "pid": 102 } }
            ]}]
          }
        ]
      }
    }"#;

    const AGENTS: &str = r#"{
      "id": "1",
      "result": {
        "agents": [
          { "agent": "claude", "authority": "process_tree",
            "branch": "feature/thing", "cwd": "/srv/work/thing", "focused": true,
            "name": null, "pane": "2", "project": "thing",
            "repo": "/srv/work/thing/.git", "session": null,
            "state_source": "manifest_rule", "status": "working", "tab": "1",
            "workspace": "1", "workspace_name": "thing", "worktree": false }
        ],
        "revision": 2094,
        "type": "agent_list"
      }
    }"#;

    #[test]
    fn the_envelope_is_unwrapped_and_an_error_object_is_not_an_empty_list() {
        assert!(parse_result(r#"{"id":"1","result":{"tasks":[]}}"#).is_ok());
        // Exit status 0 with an error object inside: the shape the whole
        // split exists for.
        assert_eq!(
            parse_result(r#"{"id":"1","error":{"message":"unknown method"}}"#),
            Err("unknown method".to_string())
        );
        assert!(parse_result(r#"{"id":"1","result":null}"#).is_err());
        assert!(parse_result("not json at all").is_err());
    }

    #[test]
    fn an_empty_list_parses_as_a_reading_rather_than_a_failure() {
        let empty = r#"{"id":"1","result":{"tasks":[],"revision":7,"type":"task_list"}}"#;
        assert_eq!(parse_tasks(empty), Ok(Vec::new()));
        let empty = r#"{"id":"1","result":{"leases":[],"revision":7,"type":"lease_list"}}"#;
        assert_eq!(parse_leases(empty), Ok(Vec::new()));
        // And a failure is still a failure, which is the other half.
        assert!(parse_tasks(r#"{"id":"1","error":{"message":"nope"}}"#).is_err());
    }

    #[test]
    fn a_missing_list_is_not_an_empty_reading() {
        // A successful envelope that never named the collection is not
        // the same as one that named it and put nothing in it.
        assert!(parse_agents(r#"{"id":"1","result":{"revision":1}}"#).is_err());
        assert!(parse_agents(r#"{"id":"1","result":{"agents":{}}}"#).is_err());
        assert!(parse_tasks(r#"{"id":"1","result":{"type":"task_list"}}"#).is_err());
        assert!(parse_leases(r#"{"id":"1","result":{"leases":null}}"#).is_err());
        assert!(parse_snapshot(r#"{"id":"1","result":{"session":"x"}}"#).is_err());
        assert_eq!(
            parse_agents(r#"{"id":"1","result":{"agents":[]}}"#),
            Ok(Vec::new())
        );
    }

    #[test]
    fn homely_only_shortens_on_a_directory_boundary() {
        assert_eq!(shorten_home("/home/alice/proj", "/home/alice"), "~/proj");
        assert_eq!(shorten_home("/home/alice", "/home/alice"), "~");
        assert_eq!(
            shorten_home("/home/alice-other/proj", "/home/alice"),
            "/home/alice-other/proj"
        );
        assert_eq!(
            shorten_home("/home/alice/projects/opscope", "/home/alice"),
            "opscope"
        );
        assert_eq!(
            shorten_home("/home/alice/projects-other/x", "/home/alice"),
            "~/projects-other/x"
        );
    }

    #[test]
    fn a_crate_under_the_workspace_is_inside_it() {
        assert!(cwd_is_inside("/srv/work/thing/crate", "/srv/work/thing"));
        assert!(cwd_is_inside("/srv/work/thing", "/srv/work/thing"));
        assert!(cwd_is_inside("/srv/work/thing/", "/srv/work/thing"));
        // A sibling whose name merely starts with the workspace path is
        // not inside it — string prefix would get this wrong.
        assert!(!cwd_is_inside("/srv/work/thing-other", "/srv/work/thing"));
        assert!(!cwd_is_inside("/srv/work/thing", ""));
    }

    #[test]
    fn a_shell_at_a_prompt_is_not_an_agent() {
        // Every pane carries an `agent` field, and two of the three panes in
        // the snapshot hold a plain shell. `agent list` names one agent, and
        // that is the number the AGENTS section must show.
        let agents = parse_agents(AGENTS).expect("agents");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].kind, "claude");
        assert_eq!(agents[0].pane, "2");
        // `name` is null on the wire and the kind stands in for it.
        assert_eq!(agents[0].name, "claude");
        assert_eq!(agents[0].state_source, "manifest_rule");
        assert_eq!(agents[0].authority, "process_tree");

        let snapshot = parse_snapshot(SNAPSHOT).expect("snapshot");
        assert_eq!(snapshot.panes.len(), 3);
        assert_eq!(
            snapshot
                .panes
                .iter()
                .filter(|p| p.command == "bash")
                .count(),
            2
        );
    }

    #[test]
    fn a_pane_running_something_is_not_a_pane_at_a_prompt() {
        let snapshot = parse_snapshot(SNAPSHOT).expect("snapshot");
        let five = snapshot.panes.iter().find(|p| p.pane_id == "5").unwrap();
        assert_eq!(five.status, "working");
        assert_eq!(five.authority, "command_fallback");
        let one = snapshot.panes.iter().find(|p| p.pane_id == "1").unwrap();
        assert_eq!(one.status, "idle");
    }

    #[test]
    fn the_snapshot_carries_the_session_it_describes() {
        let snapshot = parse_snapshot(SNAPSHOT).expect("snapshot");
        assert_eq!(snapshot.session, "default");
        assert_eq!(snapshot.protocol, "1.0");
        assert_eq!(snapshot.sequence, 2094);
        assert_eq!(snapshot.spaces.len(), 2);
        assert_eq!(snapshot.spaces[1].index, "2");
        assert_eq!(snapshot.spaces[1].cwd, "/srv/work/thing");
        assert!(snapshot.spaces[1].active);
        // The workspace's branch reaches every pane in it, so a pane row can
        // say which checkout it is looking at without a second call.
        let two = snapshot.panes.iter().find(|p| p.pane_id == "2").unwrap();
        assert_eq!(two.branch, "feature/thing");
        assert!(two.focused);
    }

    #[test]
    fn an_empty_session_is_not_a_broken_one() {
        let bare = r#"{"id":"1","result":{"session":"scratch","event_sequence":1,
          "protocol":{"major":1,"minor":0},"workspaces":[]}}"#;
        let snapshot = parse_snapshot(bare).expect("snapshot");
        assert_eq!(snapshot.spaces.len(), 0);
        assert!(snapshot.panes.is_empty());
        assert_eq!(snapshot.session, "scratch");
    }

    #[test]
    fn the_three_absences_are_told_apart() {
        // The live wording, with the socket path this widget never prints.
        assert_eq!(
            parse_failure("Error: no luvus server running (socket: /srv/run/luvus.sock)"),
            Absence::NoServer
        );
        assert_eq!(
            parse_failure("luvus: No such file or directory (os error 2)"),
            Absence::NoBinary
        );
        assert_eq!(
            parse_failure("luvus did not answer in 15s"),
            Absence::Other("luvus did not answer in 15s".into())
        );
        // Luvus answering that it cannot find something is luvus being
        // present, and must not be read as luvus being absent.
        assert_eq!(
            parse_failure("Error: pane not found"),
            Absence::Other("Error: pane not found".into())
        );
        // And whatever it says, the socket path never reaches a row.
        assert_eq!(
            parse_failure("Error: something else (socket: /srv/run/luvus.sock)"),
            Absence::Other("Error: something else".into())
        );
        assert_eq!(
            sanitize_error("Error: no luvus server running (socket: /srv/run/luvus.sock)"),
            "Error: no luvus server running"
        );
        // Already clean stays clean, so a second pass cannot invent a reason.
        assert_eq!(
            sanitize_error("luvus did not answer in 15s"),
            "luvus did not answer in 15s"
        );
    }

    #[test]
    fn the_ordering_puts_the_ones_wanting_a_human_first() {
        let mut states = vec!["idle", "working", "done", "blocked", "sideways"];
        states.sort_by_key(|s| rank_of(s));
        assert_eq!(
            states,
            vec!["blocked", "done", "working", "idle", "sideways"]
        );
    }

    #[test]
    fn a_duration_keeps_the_minutes_where_they_change_the_reading() {
        assert_eq!(ago(0.0), "0s");
        assert_eq!(ago(59.4), "59s");
        assert_eq!(ago(600.0), "10m");
        assert_eq!(ago(3600.0 * 3.0 + 58.0 * 60.0), "3h58m");
        assert_eq!(ago(86400.0 * 2.0), "2d");
    }

    #[test]
    fn a_cut_path_says_it_was_cut() {
        assert_eq!(tail_path("short", 10), "short");
        // Five cells asked for, five cells given: the mark costs one of them.
        assert_eq!(tail_path("abcdefghij", 5), "…ghij");
        assert_eq!(tail_path("abc", 1), "abc");
    }

    #[test]
    fn task_and_lease_rows_read_whichever_spelling_arrives() {
        let tasks = parse_tasks(
            r#"{"id":"1","result":{"tasks":[
              {"id":"t1","title":"Port the parser","status":"claimed",
               "agent":"claude","pane":"2","paths":["core/**"]},
              {"task_id":7,"name":"Second spelling","state":"ready"}
            ]}}"#,
        )
        .expect("tasks");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].title, "Port the parser");
        assert_eq!(tasks[0].paths, vec!["core/**".to_string()]);
        assert_eq!(tasks[1].id, "7");
        assert_eq!(tasks[1].title, "Second spelling");
        assert_eq!(tasks[1].status, "ready");

        let leases = parse_leases(
            r#"{"id":"1","result":{"leases":[
              {"id":"l1","task":"t1","paths":["core/**","docs/*.md"],"agent":"claude"}
            ]}}"#,
        )
        .expect("leases");
        assert_eq!(leases[0].paths.len(), 2);
        assert_eq!(leases[0].task, "t1");
    }

    /// Shaped from a live `luvus worktree list`, with the paths replaced.
    /// A fixture naming a real directory is a fixture that leaks one.
    const WORKTREES: &str = r#"{
      "id": "1",
      "result": {
        "revision": 44, "type": "worktree_list",
        "worktrees": [
          { "branch": "main", "head": "aaaa111", "main": true,
            "path": "/srv/work/thing" },
          { "branch": "feature/thing", "head": "bbbb222", "main": false,
            "path": "/srv/work/thing/.worktrees/one" }
        ]
      }
    }"#;

    const SESSIONS: &str = r#"{
      "id": "1",
      "result": {
        "revision": 44, "type": "agent_sessions",
        "sessions": [
          { "agent": "claude", "cwd": "/srv/work/thing", "session_id": "s-1" },
          { "agent": "codex",  "cwd": "/srv/work/other", "session_id": "s-2" },
          { "agent": "claude", "cwd": "/srv/work/gone",  "session_id": "s-3" }
        ]
      }
    }"#;

    const GIT: &str = r#"{
      "id": "1",
      "result": {
        "ahead": 1, "behind": 2, "branch": "feature/thing", "revision": 44,
        "staged": [{ "code": "M", "path": "a.rs" }],
        "stashes": ["stash@{0}: wip"],
        "type": "git_status",
        "unstaged": [{ "code": "M", "path": "b.rs" }, { "code": "M", "path": "c.rs" }],
        "untracked": ["d/"],
        "upstream": "origin/feature/thing"
      }
    }"#;

    #[test]
    fn a_worktree_list_names_every_checkout_and_which_is_primary() {
        let found = parse_worktrees(WORKTREES).expect("worktrees");
        assert_eq!(found.len(), 2);
        assert!(found[0].main);
        assert!(!found[1].main);
        assert_eq!(found[1].branch, "feature/thing");
        // Two agents on one repo in different checkouts are the reading
        // this exists for, so the branch has to survive the parse.
        assert_eq!(found[0].branch, "main");
    }

    #[test]
    fn a_dirty_count_adds_the_three_lists_git_reports_separately() {
        let state = parse_git_status(GIT).expect("git status");
        assert_eq!(state.branch, "feature/thing");
        assert_eq!(state.upstream, "origin/feature/thing");
        assert_eq!(state.ahead, 1);
        assert_eq!(state.behind, 2);
        // One staged, two unstaged, one untracked. Counting only `unstaged`
        // would call a tree with four changes in it two.
        assert_eq!(state.dirty, 4);
        assert_eq!(state.stashes, 1);
        // A clean tree reports the lists as empty rather than omitting
        // them, and that is zero rather than unknown.
        let clean = r#"{"id":"1","result":{"ahead":0,"behind":0,"branch":"main",
          "staged":[],"stashes":[],"unstaged":[],"untracked":[],"type":"git_status"}}"#;
        let clean = parse_git_status(clean).expect("clean");
        assert_eq!(clean.dirty, 0);
        assert_eq!(clean.stashes, 0);
        // The same path in staged and unstaged is two entries git
        // reported, not one file counted twice.
        let both = r#"{"id":"1","result":{"ahead":0,"behind":0,"branch":"main",
          "staged":[{"code":"M","path":"a.rs"}],"stashes":[],
          "unstaged":[{"code":"M","path":"a.rs"}],"untracked":[],"type":"git_status"}}"#;
        assert_eq!(parse_git_status(both).expect("both").dirty, 2);
        // An omitted list is not an empty one: a partial payload must
        // not draw as a cleaner tree than the source could support.
        assert!(parse_git_status(
            r#"{"id":"1","result":{"ahead":0,"behind":0,"branch":"main",
              "unstaged":[],"untracked":[],"stashes":[],"type":"git_status"}}"#
        )
        .is_err());
        assert!(parse_git_status(
            r#"{"id":"1","result":{"ahead":0,"behind":0,"branch":"main",
              "staged":"nope","unstaged":[],"untracked":[],"stashes":[],"type":"git_status"}}"#
        )
        .is_err());
    }

    #[test]
    fn a_workspace_that_is_not_a_repository_is_not_a_failed_reading() {
        // Both wear `git_error`. Only one of them will still be true next
        // poll, and drawing the settled one as a failure would put a red
        // row on a home directory for ever.
        assert!(is_not_a_repo(
            "fatal: not a git repository (or any of the parent directories): .git"
        ));
        assert!(!is_not_a_repo(
            "fatal: Unable to create index.lock: File exists"
        ));
        assert!(!is_not_a_repo("no luvus server"));
    }

    #[test]
    fn a_resumable_session_knows_whether_its_directory_is_open() {
        // Ten sessions and one open workspace was the live reading: the
        // count that matters is not the length of the list.
        let open = vec!["/srv/work/thing".to_string()];
        let found = parse_sessions(SESSIONS, Some(&open)).expect("sessions");
        assert_eq!(found.len(), 3);
        assert_eq!(
            found
                .iter()
                .filter(|s| s.in_workspace == Some(true))
                .count(),
            1
        );
        assert_eq!(found[0].in_workspace, Some(true));
        assert_eq!(found[1].in_workspace, Some(false));
        assert_eq!(found[0].kind, "claude");
        assert_eq!(found[2].session_id, "s-3");
        // A crate under an open workspace is inside it, not away.
        let nested = r#"{"id":"1","result":{"sessions":[
          {"agent":"claude","cwd":"/srv/work/thing/crate","session_id":"s-n"}]}}"#;
        let under = parse_sessions(nested, Some(&open)).expect("nested");
        assert_eq!(under[0].in_workspace, Some(true));
        // No workspaces open at all is zero inside, not zero sessions.
        let none = parse_sessions(SESSIONS, Some(&[])).expect("sessions");
        assert_eq!(none.len(), 3);
        assert_eq!(
            none.iter().filter(|s| s.in_workspace == Some(true)).count(),
            0
        );
        // A snapshot that did not come back is not an empty workspace
        // list: membership is unknown rather than "not open here".
        let unread = parse_sessions(SESSIONS, None).expect("unread");
        assert_eq!(unread.len(), 3);
        assert!(unread.iter().all(|s| s.in_workspace.is_none()));
    }

    #[test]
    fn nothing_ready_to_claim_is_an_answer() {
        let none = r#"{"id":"1","result":{"message":"no ready tasks","revision":7,"type":"none"}}"#;
        let next = parse_next_task(none).expect("next");
        assert!(next.none);
        assert!(next.id.is_empty());
        // And a real one is read whichever spelling arrives, as the task
        // list is, because neither has been seen on a live session.
        let ready = r#"{"id":"1","result":{"type":"task","id":"t-1","title":"Port it"}}"#;
        let next = parse_next_task(ready).expect("next");
        assert!(!next.none);
        assert_eq!(next.id, "t-1");
        assert_eq!(next.title, "Port it");
        let nested =
            r#"{"id":"1","result":{"type":"task","task":{"task_id":"t-2","name":"Other"}}}"#;
        let next = parse_next_task(nested).expect("next");
        assert_eq!(next.id, "t-2");
        assert_eq!(next.title, "Other");
        assert!(parse_next_task(r#"{"id":"1","error":{"message":"nope"}}"#).is_err());
    }

    /// Shaped from a live `luvus agent explain`, with the paths replaced.
    const EXPLAIN: &str = r#"{
      "id": "1",
      "result": {
        "agent": "claude", "authority": null, "available": true,
        "identity": { "confidence": "authoritative", "source": "process_tree" },
        "pane": "2", "revision": 202138, "session": null,
        "state_evidence": {
          "blocked_hint": null, "confidence": "high", "rule_priority": 120,
          "rule_region": "title", "source": "manifest_rule"
        },
        "status": "working", "type": "agent_explanation"
      }
    }"#;

    #[test]
    fn an_explanation_carries_the_evidence_the_row_has_no_room_for() {
        let e = parse_explanation(EXPLAIN).expect("explanation");
        assert_eq!(e.kind, "claude");
        assert_eq!(e.status, "working");
        assert!(e.available);
        assert_eq!(e.identity_source, "process_tree");
        assert_eq!(e.identity_confidence, "authoritative");
        assert_eq!(e.state_source, "manifest_rule");
        assert_eq!(e.state_confidence, "high");
        assert_eq!(e.rule_region, "title");
        assert_eq!(e.rule_priority, 120);
        // Not blocked, so nothing to say about why - and a null hint must
        // arrive as empty rather than as the four characters "null".
        assert_eq!(e.blocked_hint, "");
        // No integration is holding this state; it was inferred. The screen
        // says so rather than leaving the field looking answered.
        assert_eq!(e.authority, "");
    }

    #[test]
    fn a_blocked_agent_says_what_it_is_waiting_for() {
        let blocked = r#"{"id":"1","result":{
          "agent":"codex","authority":"integration_report","available":true,
          "identity":{"confidence":"authoritative","source":"integration_report"},
          "state_evidence":{"blocked_hint":"approve edit to src/main.rs?",
            "confidence":"high","rule_priority":200,"rule_region":"body",
            "source":"integration_report"},
          "status":"blocked","type":"agent_explanation"}}"#;
        let e = parse_explanation(blocked).expect("explanation");
        assert_eq!(e.status, "blocked");
        assert_eq!(e.blocked_hint, "approve edit to src/main.rs?");
        assert_eq!(e.authority, "integration_report");
    }

    #[test]
    fn a_pane_the_server_cannot_reach_is_not_an_idle_one() {
        let gone = r#"{"id":"1","result":{"agent":"claude","available":false,
          "identity":{},"state_evidence":{},"status":"unknown",
          "type":"agent_explanation"}}"#;
        let e = parse_explanation(gone).expect("explanation");
        assert!(!e.available);
        // A server that simply did not mention it is not a server saying
        // the pane is gone, so an absent field stays available.
        let quiet = r#"{"id":"1","result":{"agent":"claude","identity":{},
          "state_evidence":{},"status":"idle","type":"agent_explanation"}}"#;
        assert!(parse_explanation(quiet).expect("explanation").available);
        assert!(parse_explanation(r#"{"id":"1","error":{"message":"no such agent"}}"#).is_err());
    }
}
