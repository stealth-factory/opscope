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
    if said.starts_with("luvus:")
        && (said.contains("no such file") || said.contains("os error 2"))
    {
        return Absence::NoBinary;
    }
    // Luvus names its socket in the stopped-server message, and this
    // repository is public. Anything else it says is kept, up to that
    // parenthesis, so a screenshot cannot carry a path out of the machine.
    let said = message.split(" (socket:").next().unwrap_or(message);
    Absence::Other(said.trim().to_string())
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
    for entry in result["agents"].as_array().into_iter().flatten() {
        let kind = text_at(entry, "agent");
        let named = text_at(entry, "name");
        found.push(Agent {
            name: if named.is_empty() { kind.clone() } else { named },
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

/// A whole session, as one `luvus uhp snapshot` describes it.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Snapshot {
    pub session: String,
    pub protocol: String,
    pub sequence: u64,
    pub workspaces: usize,
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
    let listed = result["workspaces"].as_array().cloned().unwrap_or_default();
    for workspace in &listed {
        let name = text_at(workspace, "name");
        let branch = text_at(workspace, "branch");
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
        workspaces: listed.len(),
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
    for entry in result["tasks"].as_array().into_iter().flatten() {
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
    for entry in result["leases"].as_array().into_iter().flatten() {
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
    format!("…{}", chars[chars.len() - (n - 1)..].iter().collect::<String>())
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
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        return path.to_string();
    }
    let projects = format!("{}/projects/", home);
    if let Some(rest) = path.strip_prefix(&projects) {
        return rest.to_string();
    }
    match path.strip_prefix(&home) {
        Some(rest) if rest.is_empty() => "~".to_string(),
        Some(rest) => format!("~{}", rest),
        None => path.to_string(),
    }
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
        assert_eq!(snapshot.workspaces, 2);
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
        assert_eq!(snapshot.workspaces, 0);
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
    }

    #[test]
    fn the_ordering_puts_the_ones_wanting_a_human_first() {
        let mut states = vec!["idle", "working", "done", "blocked", "sideways"];
        states.sort_by_key(|s| rank_of(s));
        assert_eq!(states, vec!["blocked", "done", "working", "idle", "sideways"]);
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
}
