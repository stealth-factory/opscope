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

//! Everything that turns what the Rex server says into panes.
//!
//! Pure, so it compiles and is tested on every target. The only parts of
//! this widget that touch the machine are spawning `rex` and, for CPU and
//! memory, `linux.rs` / `macos.rs`; their text comes here to be read.

use std::collections::HashMap;

use serde_json::Value;

/// A list field out of a Lua answer.
///
/// `rex do` encodes the table a script returns as JSON, and Lua has one
/// table type for both arrays and maps, so an *empty* list arrives as `{}`
/// rather than `[]`: a block with no status reports answers
/// `"records": {}`. Read as an array, that is "not a list" and the whole
/// field would be lost; read here, it is the empty list it is.
fn list(value: &Value) -> &[Value] {
    match value {
        Value::Array(items) => items,
        _ => &[],
    }
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_string()
}

fn opt_text(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Seconds since the epoch out of an RFC 3339 time, as Rex writes them.
///
/// Rex stamps a status report in the server's own offset with nanoseconds
/// (`2026-11-04T04:36:01.674084511+08:00`), and other times in UTC with a
/// `Z`. Both are the same instant arithmetic, and an unreadable stamp is
/// `None` rather than zero: zero would draw as an agent blocked for fifty-six
/// years, which is a number on screen that is not real.
pub fn parse_rfc3339(stamp: &str) -> Option<f64> {
    let at = chrono::DateTime::parse_from_rfc3339(stamp.trim()).ok()?;
    Some(at.timestamp() as f64 + f64::from(at.timestamp_subsec_nanos()) / 1e9)
}

/// One OSC 7501 record, as the `program_status` block method returns it.
///
/// Every field is what the program reported, verbatim; `msg` and `title`
/// arrive already decoded from base64 by the server.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Record {
    /// `None` for the root record, which is the program's own state.
    /// Child ids such as `build/test` are workers under it.
    pub id: Option<String>,
    pub state: String,
    pub kind: String,
    pub progress: Option<u64>,
    /// `effective_app`, which is `app` or the one inherited from the
    /// nearest ancestor record that has one.
    pub app: String,
    pub title: String,
    pub msg: String,
    /// When the server received the report.
    pub updated_at: Option<f64>,
    pub owner_pid: Option<i64>,
    pub owner_name: String,
}

pub fn parse_records(value: &Value) -> Vec<Record> {
    let mut records: Vec<Record> = list(&value["records"])
        .iter()
        .map(|r| Record {
            id: opt_text(r, "id"),
            state: text(r, "state"),
            kind: text(r, "kind"),
            progress: r["progress"].as_u64(),
            app: opt_text(r, "effective_app").unwrap_or_else(|| text(r, "app")),
            title: text(r, "title"),
            msg: text(r, "msg"),
            updated_at: r["updated_at"].as_str().and_then(parse_rfc3339),
            owner_pid: r["owner"]["pid"].as_i64(),
            owner_name: text(&r["owner"], "name"),
        })
        .collect();
    // Root first, then by id, which is the order the method documents and
    // the one a child list reads in.
    records.sort_by(|a, b| a.id.cmp(&b.id));
    records
}

/// The program in the foreground of a terminal block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Foreground {
    pub pid: i64,
    pub name: String,
    pub argv0: String,
    pub path: String,
    pub invoked_path: String,
    pub script: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Process {
    /// The program Rex started in the block - usually the shell.
    pub child_pid: Option<i64>,
    pub child_cwd: String,
    /// The name and argv0 of that program, to tell a shell from a pane
    /// started with a command of its own.
    pub child_name: String,
    pub child_argv0: String,
    pub foreground: Option<Foreground>,
    pub last_exit: Option<i64>,
}

pub fn parse_process_value(value: &Value) -> Process {
    let fg = &value["foreground"];
    Process {
        child_pid: value["child"]["pid"].as_i64(),
        child_cwd: text(&value["child"], "cwd"),
        child_name: text(&value["child"], "name"),
        child_argv0: text(&value["child"], "argv0"),
        foreground: fg["pid"].as_i64().map(|pid| Foreground {
            pid,
            name: text(fg, "name"),
            argv0: text(fg, "argv0"),
            path: text(fg, "path"),
            invoked_path: text(fg, "invoked_path"),
            script: text(fg, "script"),
            cwd: text(fg, "cwd"),
        }),
        last_exit: value["last_exit"]["exit_code"].as_i64(),
    }
}

/// Whether the program Rex started in a block is a shell.
///
/// "At a prompt" is the shell itself in the foreground. A pane started with
/// a command - `rex split -- htop` - has that command as its child, and the
/// child in the foreground there is the command running, not a prompt. A
/// login shell's argv0 starts with `-`, which settles it on its own.
pub fn is_shell(name: &str, argv0: &str) -> bool {
    const SHELLS: &[&str] = &[
        "sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "elvish", "xonsh",
        "pwsh", "ash", "yash", "oksh",
    ];
    if argv0.starts_with('-') {
        return true;
    }
    let argv0 = base_name(argv0.split_whitespace().next().unwrap_or(""));
    SHELLS.contains(&base_name(name)) || SHELLS.contains(&argv0)
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// What a person would call the program in the foreground.
///
/// The order the API documents for display: the script an interpreter was
/// handed, then `argv0`, then the executable's name. `argv0` is what makes
/// `npm exec opscope@latest` read as that rather than as `node`, and the
/// name comes last because it is the least useful: Claude Code's process
/// is named after its version, `2.1.317`.
pub fn command_label(fg: &Foreground) -> String {
    if !fg.script.is_empty() {
        return base_name(&fg.script).to_string();
    }
    let argv0 = fg.argv0.trim_start_matches('-');
    if !argv0.is_empty() {
        // A full path in argv0 says where, not what.
        return if argv0.starts_with('/') {
            base_name(argv0).to_string()
        } else {
            argv0.to_string()
        };
    }
    if !fg.name.is_empty() {
        return fg.name.clone();
    }
    "?".into()
}

/// Every name the foreground program goes by, for matching a catalogue.
///
/// `invoked_path` first: it is the one that said `claude` while `name` said
/// `2.1.317`.
fn identities(fg: &Foreground) -> Vec<String> {
    let mut names = Vec::new();
    for candidate in [
        base_name(&fg.invoked_path),
        base_name(fg.argv0.split_whitespace().next().unwrap_or("")),
        base_name(&fg.path),
        fg.name.as_str(),
        base_name(&fg.script),
    ] {
        let candidate = candidate.trim_start_matches('-').to_ascii_lowercase();
        if !candidate.is_empty() && !names.contains(&candidate) {
            names.push(candidate);
        }
    }
    names
}

/// Whether a reported app name is one of the catalogue's commands.
///
/// Programs report under their product name, which is often the command
/// with a suffix: Claude Code says `claude-code` and its command is
/// `claude`. Exactly those two suffixes, so `claude-code` matches `claude`
/// while a `claudette` would not.
fn app_matches(app: &str, command: &str) -> bool {
    let app = app.to_ascii_lowercase();
    let command = command.to_ascii_lowercase();
    app == command || app == format!("{}-code", command) || app == format!("{}-cli", command)
}

/// How the widget came to call a pane an agent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    /// The program said so, in an OSC 7501 report naming its app.
    Report,
    /// Its foreground process is one of the catalogue's commands.
    Process,
}

impl Source {
    pub fn words(self) -> &'static str {
        match self {
            Source::Report => "id via report",
            Source::Process => "id via process",
        }
    }
}

/// The agent a block holds, if it holds one, and how that is known.
///
/// A report beats the process: a program that names itself is the
/// authority on what it is, and the catalogue is only a list of commands.
pub fn classify_agent(
    records: &[Record],
    fg: Option<&Foreground>,
    catalogue: &[String],
) -> Option<(String, Source)> {
    for record in records {
        if catalogue.iter().any(|c| app_matches(&record.app, c)) {
            return Some((record.app.clone(), Source::Report));
        }
    }
    let fg = fg?;
    let names = identities(fg);
    for command in catalogue {
        let command = command.to_ascii_lowercase();
        if names.iter().any(|n| *n == command) {
            return Some((command, Source::Process));
        }
    }
    None
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Window {
    pub window_id: String,
    pub label: String,
    /// One-based, in the order `session.view` lists windows - which is
    /// the tab order in the app and what `rex -w N` means.
    pub ordinal: usize,
    pub focused_block: String,
    pub zoomed_block: String,
    /// Blocks in a floating layer, and whether that layer is minimised.
    pub floating: HashMap<String, bool>,
}

fn walk_layout(node: &Value, out: &mut Vec<String>) {
    if let Some(id) = node["block_id"].as_str() {
        out.push(id.to_string());
    }
    for half in ["before", "after"] {
        if node["split"][half].is_object() {
            walk_layout(&node["split"][half], out);
        }
    }
    if node["layout"].is_object() {
        walk_layout(&node["layout"], out);
    }
}

pub fn parse_view_value(view: &Value) -> (String, Vec<Window>) {
    let active = text(view, "active_window_id");
    let windows = list(&view["windows"])
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let mut floating = HashMap::new();
            for layer in list(&w["layers"]) {
                if text(layer, "kind") != "floating" {
                    continue;
                }
                let minimised = text(layer, "state") == "minimized";
                let mut ids = Vec::new();
                walk_layout(layer, &mut ids);
                for b in list(&layer["blocks"]) {
                    if let Some(id) = b["block_id"].as_str() {
                        ids.push(id.to_string());
                    }
                }
                for id in ids {
                    floating.insert(id, minimised);
                }
            }
            Window {
                window_id: text(w, "window_id"),
                label: text(w, "label"),
                ordinal: i + 1,
                focused_block: text(w, "focused_block_id"),
                zoomed_block: text(w, "zoomed_block_id"),
                floating,
            }
        })
        .collect();
    (active, windows)
}

/// A method that failed for one block, carried as the server said it.
pub type Read<T> = Result<T, String>;

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub block_id: String,
    pub label: String,
    pub creator: String,
    pub host_id: String,
    pub detached: bool,
    pub window_id: String,
    pub process: Option<Read<Process>>,
    pub title: Option<Read<String>>,
    pub records: Option<Read<Vec<Record>>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub session_id: String,
    pub label: String,
    pub ordinal: usize,
    pub active_window: String,
    pub windows: Vec<Window>,
    /// Why the session could not be read, when it could not.
    pub error: Option<String>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Client {
    pub kind: String,
    pub platform: String,
    pub session_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Server {
    pub host_id: String,
    pub instance_id: String,
    pub rex_version: String,
    /// Absent means revision 0, as the API documents.
    pub api_revision: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gather {
    pub sessions: Vec<Session>,
    pub clients: Vec<Client>,
    pub server: Server,
}

fn method<T>(row: &Value, name: &str, read: impl Fn(&Value) -> T) -> Option<Read<T>> {
    if let Some(why) = row[format!("{}_error", name)].as_str() {
        return Some(Err(why.to_string()));
    }
    if row[name].is_null() {
        return None;
    }
    Some(Ok(read(&row[name])))
}

/// Everything one `rex do` read returned, as panes the screen can draw.
///
/// `gather.lua` is what produced it; see there for the shape.
pub fn parse_gather(input: &str) -> Result<Gather, String> {
    let value: Value = serde_json::from_str(input.trim())
        .map_err(|e| format!("rex do answered with something that is not JSON: {}", e))?;
    if !value.is_object() {
        return Err("rex do answered with something that is not a table".into());
    }
    let mut attach_errors: HashMap<String, String> = HashMap::new();
    for e in list(&value["errors"]) {
        attach_errors.insert(text(e, "session_id"), text(e, "attach_error"));
    }
    let sessions = list(&value["sessions"])
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let session_id = text(s, "session_id");
            let (active_window, windows) = parse_view_value(&s["view"]);
            let error = attach_errors
                .get(&session_id)
                .cloned()
                .or_else(|| opt_text(s, "view_error"))
                .or_else(|| opt_text(s, "blocks_error"));
            let blocks = list(&s["blocks"])
                .iter()
                .map(|row| {
                    let b = &row["block"];
                    Block {
                        block_id: text(b, "block_id"),
                        label: text(b, "label"),
                        creator: text(b, "creator_name"),
                        host_id: text(b, "host_id"),
                        detached: text(b, "placement") == "detached",
                        window_id: text(b, "window_id"),
                        process: method(row, "process", parse_process_value),
                        title: method(row, "title", |t| text(t, "title")),
                        records: method(row, "program_status", parse_records),
                    }
                })
                .collect();
            Session {
                label: text(s, "label"),
                session_id,
                ordinal: i + 1,
                active_window,
                windows,
                error,
                blocks,
            }
        })
        .collect();
    let clients = list(&value["clients"]["clients"])
        .iter()
        .map(|c| Client {
            kind: text(&c["info"], "kind"),
            platform: text(&c["info"], "platform"),
            session_ids: list(&c["session_ids"])
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
        })
        .collect();
    let server = Server {
        host_id: text(&value["server"], "host_id"),
        instance_id: text(&value["server"], "instance_id"),
        rex_version: text(&value["version"], "rex_version"),
        api_revision: value["version"]["api_revision"]
            .as_u64()
            .or_else(|| value["server"]["api_revision"].as_u64())
            .unwrap_or(0),
    };
    Ok(Gather {
        sessions,
        clients,
        server,
    })
}

/// Why a `rex` command found no server to read, told apart.
#[derive(Clone, Debug, PartialEq)]
pub enum Absence {
    /// Nothing is listening on the local socket, and the widget will not
    /// start a server to answer itself.
    NoServer,
    /// A named server did not answer.
    Unreachable,
    /// A block or session went away between the read and the key.
    Gone,
    /// Anything else, in the words `rex` used.
    Other(String),
}

/// What a failed `rex` command's stderr says, with the parts that do not
/// belong on a shared screen taken out.
///
/// The local socket path carries the user's uid, and an unreachable host's
/// error spells out every address probed; neither is shown, because this
/// pane gets screenshotted and the reader needs the kind of failure, not
/// the plumbing.
pub fn parse_failure(stderr: &str) -> Absence {
    let said = stderr.trim();
    let lower = said.to_ascii_lowercase();
    if lower.contains("autostart is disabled") || lower.contains("no rex server is running") {
        Absence::NoServer
    } else if lower.contains("connecting to rex server") {
        Absence::Unreachable
    } else if lower.contains("not found") {
        Absence::Gone
    } else {
        let first = said
            .lines()
            .next()
            .unwrap_or("")
            .trim_start_matches("Error: ");
        Absence::Other(first.chars().take(160).collect())
    }
}

/// What one line of `rex events --all --json` asks of the widget.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    /// Something a full read would show differently: read again.
    Wake,
    /// A program asked for a human with a bell or a desktop notification.
    /// No method returns this afterwards, so it is the one payload kept.
    Attention { block_id: String, said: String },
    /// Nothing the screen draws depends on it.
    Ignore,
    /// Not an event: `rex events` said something else and is ending.
    Ended(String),
}

/// Read one event line.
///
/// Only the type, the block event's name and its block id are read. A
/// `size_changed` payload carries the owning client's principal - a login
/// name and a device - and nothing here has any business holding it.
pub fn parse_event_line(line: &str) -> StreamEvent {
    let line = line.trim();
    if line.is_empty() {
        return StreamEvent::Ignore;
    }
    if !line.starts_with('{') {
        return StreamEvent::Ended(line.trim_start_matches("Error: ").to_string());
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return StreamEvent::Ignore;
    };
    let top = value["type"].as_str().unwrap_or("");
    if top != "session_event" {
        // Server-level: a session made or destroyed changes the board.
        return match top {
            "session_created"
            | "session_destroyed"
            | "session_removed"
            | "session_label_changed" => StreamEvent::Wake,
            _ => StreamEvent::Ignore,
        };
    }
    let event = &value["payload"]["event"];
    match event["type"].as_str().unwrap_or("") {
        "block_event" => {
            let payload = &event["payload"];
            match payload["name"].as_str().unwrap_or("") {
                "program_status_changed"
                | "program_status_removed"
                | "process_changed"
                | "child_exited"
                | "title_changed"
                | "pwd_changed" => StreamEvent::Wake,
                name @ ("bell" | "desktop_notification") => {
                    let inner = &payload["payload"];
                    let said = [text(inner, "title"), text(inner, "body")]
                        .into_iter()
                        .filter(|s| !s.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    StreamEvent::Attention {
                        block_id: text(payload, "block_id"),
                        said: if said.is_empty() && name == "bell" {
                            "rang the bell".into()
                        } else if said.is_empty() {
                            "sent a notification".into()
                        } else {
                            said
                        },
                    }
                }
                _ => StreamEvent::Ignore,
            }
        }
        "session_view_changed"
        | "block_created"
        | "block_closed"
        | "block_removed"
        | "window_created"
        | "window_closed"
        | "window_label_changed"
        | "block_label_changed"
        | "session_label_changed"
        | "active_window_changed" => StreamEvent::Wake,
        _ => StreamEvent::Ignore,
    }
}

/// (cpu ticks used, resident bytes) out of one /proc/<pid>/stat line.
///
/// The command sits in brackets and may itself contain spaces and
/// brackets, so the split starts after the last one.
pub fn parse_proc_stat(text: &str, page: u64) -> Option<(u64, u64)> {
    let rest = text.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    let rss: u64 = fields.get(21)?.parse().ok()?;
    Some((utime + stime, rss * page))
}

/// `ps -o pid=,pcpu=,rss=` lines as pid → (cpu %, resident bytes).
///
/// macOS's `pcpu` is the kernel's own decaying recent average, not a
/// lifetime mean, which is why it can be shown as it is. `rss` is KiB.
///
/// Only `macos.rs` calls it. It is compiled and tested everywhere all the
/// same, which is the point of keeping parsers out of the platform files.
#[allow(dead_code)]
pub fn parse_ps(text: &str) -> HashMap<i64, (f64, u64)> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(cpu), Some(rss)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(pid), Ok(cpu), Ok(rss)) = (pid.parse(), cpu.parse(), rss.parse::<u64>()) else {
            continue;
        };
        out.insert(pid, (cpu, rss * 1024));
    }
    out
}

/// The screen text `rex capture --trim` printed, as the last `n` lines
/// that say anything.
pub fn parse_capture(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.trim().is_empty())
        .collect();
    let from = lines.len().saturating_sub(n);
    lines[from..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GATHER: &str = include_str!("fixtures/rex/gather.json");
    const GATHER_RECORDS: &str = include_str!("fixtures/rex/gather_records.json");
    const EVENTS: &str = include_str!("fixtures/rex/events.ndjson");

    fn catalogue() -> Vec<String> {
        ["claude", "codex"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_lua_empty_table_is_an_empty_list_not_a_missing_one() {
        let g = parse_gather(GATHER).unwrap();
        let block = &g.sessions[0].blocks[0];
        assert_eq!(block.records, Some(Ok(Vec::new())));
        let empty =
            r#"{"sessions":{},"errors":{},"clients":{"clients":{}},"server":{},"version":{}}"#;
        let g = parse_gather(empty).unwrap();
        assert!(g.sessions.is_empty());
        assert!(g.clients.is_empty());
        assert_eq!(g.server.api_revision, 0);
    }

    #[test]
    fn the_whole_server_reads_in_session_and_window_order() {
        let g = parse_gather(GATHER).unwrap();
        assert_eq!(g.sessions.len(), 1);
        let s = &g.sessions[0];
        assert_eq!(s.label, "work");
        assert_eq!(s.ordinal, 1);
        assert_eq!(s.windows.len(), 1);
        assert_eq!(s.windows[0].ordinal, 1);
        assert!(!s.windows[0].focused_block.is_empty());
        assert_eq!(s.blocks.len(), 5);
        assert_eq!(g.server.api_revision, 10);
        assert!(!g.server.instance_id.is_empty());
        assert!(g.clients.iter().any(|c| c.kind == "app"));
    }

    #[test]
    fn real_reports_carry_state_kind_progress_message_and_children() {
        let g = parse_gather(GATHER_RECORDS).unwrap();
        let blocks = &g.sessions[0].blocks;
        let records: Vec<&Vec<Record>> = blocks
            .iter()
            .filter_map(|b| match &b.records {
                Some(Ok(r)) if !r.is_empty() => Some(r),
                _ => None,
            })
            .collect();
        assert_eq!(records.len(), 2);
        let blocked = records.iter().find(|r| r[0].state == "blocked").unwrap();
        assert_eq!(blocked[0].id, None);
        assert_eq!(blocked[0].kind, "permission");
        assert_eq!(blocked[0].progress, Some(40));
        assert_eq!(blocked[0].msg, "approve: cargo test");
        assert_eq!(blocked[1].id.as_deref(), Some("build/test"));
        assert_eq!(blocked[1].state, "done");
        let claude = records.iter().find(|r| r[0].app == "claude-code").unwrap();
        assert_eq!(claude[0].state, "done");
        assert_eq!(claude[0].msg, "replied 'ok' as requested");
        assert!(claude[0].updated_at.unwrap() > 1.7e9);
    }

    #[test]
    fn an_offset_time_and_a_utc_time_are_the_same_arithmetic() {
        let a = parse_rfc3339("2026-11-04T04:36:01.5+08:00").unwrap();
        let b = parse_rfc3339("2026-11-03T20:36:01.5Z").unwrap();
        assert!((a - b).abs() < 1e-6);
        assert_eq!(parse_rfc3339("yesterday"), None);
    }

    #[test]
    fn claude_is_found_by_its_invoked_path_when_its_name_is_a_version() {
        let fg = Foreground {
            pid: 2,
            name: "2.1.317".into(),
            argv0: "claude".into(),
            invoked_path: "/home/user/.local/bin/claude".into(),
            path: "/home/user/.local/share/claude/versions/2.1.317".into(),
            ..Default::default()
        };
        assert_eq!(
            classify_agent(&[], Some(&fg), &catalogue()),
            Some(("claude".into(), Source::Process))
        );
        assert_eq!(command_label(&fg), "claude");
    }

    #[test]
    fn a_report_beats_the_process_and_names_the_product() {
        let record = Record {
            app: "claude-code".into(),
            state: "idle".into(),
            ..Default::default()
        };
        let fg = Foreground {
            pid: 2,
            argv0: "claude".into(),
            ..Default::default()
        };
        assert_eq!(
            classify_agent(&[record], Some(&fg), &catalogue()),
            Some(("claude-code".into(), Source::Report))
        );
        assert!(!app_matches("claudette", "claude"));
        assert!(app_matches("Codex-CLI", "codex"));
    }

    #[test]
    fn a_reporter_outside_the_catalogue_is_not_an_agent() {
        let record = Record {
            app: "terraform".into(),
            state: "blocked".into(),
            ..Default::default()
        };
        assert_eq!(classify_agent(&[record], None, &catalogue()), None);
    }

    #[test]
    fn argv0_names_an_npm_exec_and_a_login_shell_loses_its_dash() {
        let npm = Foreground {
            pid: 3,
            name: "node".into(),
            argv0: "npm exec opscope@latest".into(),
            ..Default::default()
        };
        assert_eq!(command_label(&npm), "npm exec opscope@latest");
        let shell = Foreground {
            pid: 3,
            name: "bash".into(),
            argv0: "-bash".into(),
            ..Default::default()
        };
        assert_eq!(command_label(&shell), "bash");
        let script = Foreground {
            argv0: "python3".into(),
            script: "/opt/tools/serve.py".into(),
            ..Default::default()
        };
        assert_eq!(command_label(&script), "serve.py");
    }

    #[test]
    fn a_method_that_failed_keeps_its_reason() {
        let input = r#"{"sessions":[{"session_id":"session:x","label":"s","view":{"windows":{}},
            "blocks":[{"block":{"block_id":"block:y","creator_name":"com.superlogical.terminal"},
            "process_error":"block y has no method \"process\""}]}],"errors":{}}"#;
        let g = parse_gather(input).unwrap();
        let b = &g.sessions[0].blocks[0];
        assert!(matches!(&b.process, Some(Err(why)) if why.contains("no method")));
        assert_eq!(b.records, None);
    }

    #[test]
    fn an_attach_failure_is_the_sessions_error() {
        let input = r#"{"sessions":[{"session_id":"session:x","label":"s","blocks":{}}],
            "errors":[{"session_id":"session:x","attach_error":"code=conflict"}]}"#;
        let g = parse_gather(input).unwrap();
        assert_eq!(g.sessions[0].error.as_deref(), Some("code=conflict"));
    }

    #[test]
    fn a_floating_layer_marks_its_blocks_and_whether_it_is_minimised() {
        let view: Value = serde_json::from_str(
            r#"{"active_window_id":"window:a","windows":[{"window_id":"window:a","label":"one",
            "layers":[{"kind":"tiled","layout":{"block_id":"block:1"}},
                      {"kind":"floating","state":"minimized","layout":{"block_id":"block:2"}}]}]}"#,
        )
        .unwrap();
        let (active, windows) = parse_view_value(&view);
        assert_eq!(active, "window:a");
        assert_eq!(windows[0].floating.get("block:2"), Some(&true));
        assert_eq!(windows[0].floating.get("block:1"), None);
    }

    #[test]
    fn failures_are_told_apart_and_never_show_the_socket_path() {
        assert_eq!(
            parse_failure(
                "Error: no Rex server is running at /run/user/1000/rex/server.sock, and autostart is disabled"
            ),
            Absence::NoServer
        );
        assert_eq!(
            parse_failure(
                "Error: connecting to Rex server: probing https://desk: dial tcp: lookup desk"
            ),
            Absence::Unreachable
        );
        assert_eq!(
            parse_failure("Error: block target \"block:1\" not found: session scope"),
            Absence::Gone
        );
        assert_eq!(
            parse_failure("Error: attempt to index a nil value\nstack traceback"),
            Absence::Other("attempt to index a nil value".into())
        );
    }

    #[test]
    fn every_captured_event_lands_in_its_class() {
        let kinds: Vec<StreamEvent> = EVENTS.lines().map(parse_event_line).collect();
        assert!(kinds.iter().any(|k| *k == StreamEvent::Wake));
        assert!(kinds.iter().any(|k| *k == StreamEvent::Ignore));
        assert_eq!(
            parse_event_line("Error: interrupted"),
            StreamEvent::Ended("interrupted".into())
        );
        // Owner changes and resizes happen on every focus move; waking on
        // them would turn one click into twenty reads.
        let owner = r#"{"type":"session_event","payload":{"session_id":"s","event":{"type":"block_owner_changed","payload":{"block_id":"b"}}}}"#;
        assert_eq!(parse_event_line(owner), StreamEvent::Ignore);
        let size = r#"{"type":"session_event","payload":{"session_id":"s","event":{"type":"block_event","payload":{"block_id":"b","name":"size_changed","payload":{"owner":{"principal":{"login":"someone"}}}}}}}"#;
        assert_eq!(parse_event_line(size), StreamEvent::Ignore);
    }

    #[test]
    fn a_status_report_wakes_a_read_and_a_bell_is_kept_with_its_words() {
        let status = r#"{"type":"session_event","payload":{"session_id":"s","event":{"type":"block_event","payload":{"block_id":"b","name":"program_status_changed","payload":{"id":"","reason":"report"}}}}}"#;
        assert_eq!(parse_event_line(status), StreamEvent::Wake);
        let note = r#"{"type":"session_event","payload":{"session_id":"s","event":{"type":"block_event","payload":{"block_id":"block:9","name":"desktop_notification","payload":{"title":"Claude Code","body":"needs your permission"}}}}}"#;
        assert_eq!(
            parse_event_line(note),
            StreamEvent::Attention {
                block_id: "block:9".into(),
                said: "Claude Code · needs your permission".into()
            }
        );
        let bell = r#"{"type":"session_event","payload":{"session_id":"s","event":{"type":"block_event","payload":{"block_id":"block:9","name":"bell","payload":{}}}}}"#;
        assert_eq!(
            parse_event_line(bell),
            StreamEvent::Attention {
                block_id: "block:9".into(),
                said: "rang the bell".into()
            }
        );
    }

    #[test]
    fn proc_stat_and_ps_read_the_same_two_numbers() {
        let line = "1234 (my (odd) prog) S 1 1234 1234 0 -1 4194304 100 0 0 0 250 50 0 0 20 0 1 0 100 1000000 300 18446744073709551615";
        assert_eq!(parse_proc_stat(line, 4096), Some((300, 300 * 4096)));
        let ps = "  1234   12.5  20480\n  99 0.0 4\nnot a line\n";
        let read = parse_ps(ps);
        assert_eq!(read.get(&1234), Some(&(12.5, 20480 * 1024)));
        assert_eq!(read.get(&99), Some(&(0.0, 4096)));
        assert_eq!(read.len(), 2);
    }

    #[test]
    fn a_capture_keeps_the_last_lines_that_say_something() {
        let text = "one\n\n  \ntwo  \nthree\n\n";
        assert_eq!(parse_capture(text, 2), vec!["two", "three"]);
        assert_eq!(parse_capture(text, 10), vec!["one", "two", "three"]);
    }

    #[test]
    fn a_pane_started_with_a_command_is_not_a_shell() {
        assert!(is_shell("bash", "-bash"));
        assert!(is_shell("zsh", "zsh"));
        assert!(is_shell("whatever", "-fish"));
        assert!(!is_shell("rex-panes", "/opt/opscope/rex-panes"));
        assert!(!is_shell("htop", "htop"));
    }

    #[test]
    fn a_shell_at_its_prompt_is_its_own_foreground() {
        let value: Value = serde_json::from_str(r#"{"child":{"pid":10,"cwd":"/home/user"},"foreground":{"pid":10,"name":"bash","argv0":"-bash"},"last_exit":null}"#).unwrap();
        let process = parse_process_value(&value);
        assert_eq!(process.child_pid, Some(10));
        assert_eq!(process.foreground.unwrap().pid, 10);
        assert_eq!(process.last_exit, None);
        let records = parse_records(&serde_json::from_str(r#"{"records":{}}"#).unwrap());
        assert!(records.is_empty());
    }
}
