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

//! Everything running under a Rex server, across every session.
//!
//! A Rex client, read-only, and honest about where each fact came from.
//! State is what a program reported with OSC 7501 and nothing else; the
//! foreground process says what a pane is running and whether it sits at a
//! prompt; a title is shown as a title. One `rex do` script reads the whole
//! server on one connection, and `rex events` wakes it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opscope_core as tc;

#[path = "../../process_filter.rs"]
mod process_filter;

#[path = "parse.rs"]
mod parse;

#[path = "events.rs"]
mod events;

#[path = "../refresh_wake.rs"]
mod refresh_wake;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod host;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod host;

use parse::{Absence, Foreground, Record, Source};

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "rex-panes",
    section: "rex_panes",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};

/// The read, run inside the `rex` CLI on one connection.
const GATHER_LUA: &str = include_str!("gather.lua");

/// The API revision this widget was written against. A server on another
/// revision is still read - a method it lacks fails per block, visibly -
/// but the reader is told, once, why a pane might say so.
const API_REVISION_BUILT_AGAINST: u64 = 10;

/// Seconds before a rex command is given up on.
const RUN_TIMEOUT: u64 = 8;

const DEFAULT_AGENTS: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "copilot",
    "cursor-agent",
    "aider",
    "opencode",
    "pi",
    "goose",
    "amp",
    "grok",
    "droid",
    "kiro",
    "cline",
    "crush",
    "qwen",
    "kimi",
    "devin",
    "jules",
];

/// Worst first: the states that want a human are the reason to look.
/// `attention` is not a protocol state - it is a program that rang the bell
/// or sent a notification and has not been looked at since.
const RANK: &[&str] = &[
    "blocked",
    "error",
    "done",
    "attention",
    "working",
    "idle",
    "unknown",
];

fn rank_of(state: &str) -> usize {
    RANK.iter().position(|s| *s == state).unwrap_or(RANK.len())
}

/// A pane's foreground, from the widget's side of the probe.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Doing {
    Running,
    Prompt,
    /// The `process` method did not answer. The default, because a pane
    /// nobody managed to look at has told us nothing.
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Section {
    Agents,
    Processes,
    Idle,
}

/// One block, with everything the screen says about it.
#[derive(Clone, Debug, Default)]
struct Pane {
    session_id: String,
    session_label: String,
    session_ord: usize,
    window_ord: Option<usize>,
    window_label: String,
    /// `Some(minimised)` when the block sits in a floating layer.
    floating: Option<bool>,
    block_id: String,
    label: String,
    creator: String,
    detached: bool,
    remote: bool,
    focused: bool,
    zoomed: bool,
    child_pid: Option<i64>,
    fg: Option<Foreground>,
    child_cwd: String,
    last_exit: Option<i64>,
    title: String,
    records: Vec<Record>,
    doing: Doing,
    /// Why a method failed for this block, when one did.
    why: String,
    /// `program_status` itself failed, so an empty `records` is not an
    /// answer: the program may be blocked and nobody could ask.
    status_unread: bool,
    agent: Option<(String, Source)>,
    is_opscope: bool,
    cpu: Option<f64>,
    rss: Option<u64>,
    /// When the widget first saw this foreground, for panes with no report.
    seen_since: f64,
    seen_exact: bool,
    attention: Option<(f64, String)>,
}

impl Pane {
    fn root(&self) -> Option<&Record> {
        self.records.iter().find(|r| r.id.is_none())
    }

    fn children(&self) -> impl Iterator<Item = &Record> {
        self.records.iter().filter(|r| r.id.is_some())
    }

    /// The state word drawn on the row: the root record's, verbatim, or
    /// `unknown` when the program has reported nothing. Never read off a
    /// title glyph or the screen.
    fn state(&self) -> String {
        match self.root() {
            Some(r) if !r.state.is_empty() => r.state.clone(),
            _ => "unknown".into(),
        }
    }

    /// Where it sorts. The worst record of any - a blocked worker under an
    /// idle parent is still a human needed - and a bell outranks a program
    /// that is only working.
    fn rank(&self) -> usize {
        let mut best = rank_of(&self.state());
        for r in self.children() {
            best = best.min(rank_of(&r.state));
        }
        if self.attention.is_some() {
            best = best.min(rank_of("attention"));
        }
        best
    }

    fn wants_a_human(&self) -> bool {
        self.records
            .iter()
            .any(|r| matches!(r.state.as_str(), "blocked" | "error" | "done"))
            || self.attention.is_some()
    }

    fn section(&self) -> Section {
        if self.agent.is_some() {
            Section::Agents
        } else if self.doing == Doing::Prompt && self.records.is_empty() && !self.status_unread {
            Section::Idle
        } else {
            Section::Processes
        }
    }

    fn command(&self) -> String {
        if self.creator != TERMINAL {
            return format!("{} block", base_creator(&self.creator));
        }
        self.fg
            .as_ref()
            .map(parse::command_label)
            .unwrap_or_default()
    }

    fn cwd(&self) -> String {
        match &self.fg {
            Some(fg) if !fg.cwd.is_empty() => fg.cwd.clone(),
            _ => self.child_cwd.clone(),
        }
    }

    /// `work:2`, the session and the window's tab position - what `rex -s
    /// work -w 2` means - or what stands in for a window when there is none.
    fn place(&self) -> String {
        format!("{}:{}", self.session_label, self.window())
    }

    /// The window's tab position on its own - what a row under its
    /// session's sub-heading needs, since the heading already names it.
    fn window(&self) -> String {
        if self.detached {
            "detached".to_string()
        } else {
            match (self.window_ord, self.floating) {
                (Some(n), Some(true)) => format!("{}·float·min", n),
                (Some(n), Some(false)) => format!("{}·float", n),
                (Some(n), None) => n.to_string(),
                (None, _) => "?".into(),
            }
        }
    }

    /// A record whose reporter is neither in front nor the shell has
    /// outlived the program that made it. The protocol keeps `done` and
    /// `error` past exit on purpose, so it is drawn, and said.
    fn reporter_gone(&self, r: &Record) -> bool {
        match r.owner_pid {
            Some(pid) => {
                Some(pid) != self.fg.as_ref().map(|f| f.pid) && Some(pid) != self.child_pid
            }
            None => false,
        }
    }

    fn short_id(&self) -> String {
        let id = self.block_id.rsplit(':').next().unwrap_or(&self.block_id);
        id.chars()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

const TERMINAL: &str = "com.superlogical.terminal";

fn base_creator(creator: &str) -> &str {
    creator.rsplit('.').next().unwrap_or(creator)
}

#[derive(Clone, Debug, Default)]
struct Board {
    panes: Vec<Pane>,
    /// Sessions that could not be read, and why.
    unread_sessions: Vec<(String, String)>,
    sessions: usize,
    apps: usize,
    /// Session ids some app is attached to.
    app_sessions: Vec<String>,
    rex_version: String,
    api_revision: u64,
    local_usage: bool,
}

#[derive(Default)]
pub struct State {
    board: Option<Board>,
    absence: Option<Absence>,
    /// Why the poller stopped, when it did.
    err: String,
    watcher_err: String,
    /// Bells and notifications, newest per block: (when, what it said).
    pub attention: HashMap<String, (f64, String)>,
    restarted_at: Option<f64>,
}

fn panic_reason(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&'static str>()
                .map(|message| message.to_string())
        })
        .unwrap_or_else(|| "non-string panic payload".into())
}

/// `rex`, with the flags every call here carries.
///
/// `--autostart=false` on every one: the CLI starts a server when none is
/// running, and a status pane that conjures the thing it reports on would
/// report a server nobody asked for.
fn rex_argv<'a>(server: &'a str, rest: &[&'a str]) -> Vec<&'a str> {
    let mut argv = vec!["rex", "--autostart=false"];
    if !server.is_empty() {
        argv.extend_from_slice(&["-S", server]);
    }
    argv.extend_from_slice(rest);
    argv
}

/// Run a rex command; its stdout, or why not in the words rex used.
fn rex(server: &str, rest: &[&str]) -> Result<String, Absence> {
    let argv = rex_argv(server, rest);
    match tc::run_full(&argv, RUN_TIMEOUT) {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).to_string()),
        Ok(out) => Err(parse::parse_failure(&String::from_utf8_lossy(&out.stderr))),
        Err(why) => Err(Absence::Other(why)),
    }
}

/// What carries over between reads: when each foreground was first seen.
#[derive(Default)]
struct Seen {
    since: HashMap<String, (i64, f64, bool)>,
    first: bool,
    instance: String,
}

/// Whether the pids Rex reports are pids on this machine.
///
/// Only when the widget talks to the local socket. A `server` set in config,
/// or a `REX_SERVER` naming an address, can be another computer, and its
/// pid 4012 is not ours.
fn talks_to_local_server(server: &str) -> bool {
    if !server.is_empty() {
        return false;
    }
    match std::env::var("REX_SERVER") {
        Ok(v) => v.is_empty() || v.starts_with("unix:"),
        Err(_) => true,
    }
}

fn build(g: &parse::Gather, catalogue: &[String], seen: &mut Seen, local: bool, at: f64) -> Board {
    let mut panes = Vec::new();
    let mut unread = Vec::new();
    for s in &g.sessions {
        if let Some(why) = &s.error {
            unread.push((s.label.clone(), why.clone()));
        }
        let active = s.windows.iter().find(|w| w.window_id == s.active_window);
        for b in &s.blocks {
            let window = s.windows.iter().find(|w| w.window_id == b.window_id);
            let mut pane = Pane {
                session_id: s.session_id.clone(),
                session_label: s.label.clone(),
                session_ord: s.ordinal,
                window_ord: window.map(|w| w.ordinal),
                window_label: window.map(|w| w.label.clone()).unwrap_or_default(),
                floating: window.and_then(|w| w.floating.get(&b.block_id).copied()),
                block_id: b.block_id.clone(),
                label: b.label.clone(),
                creator: b.creator.clone(),
                detached: b.detached,
                remote: !b.host_id.is_empty() && b.host_id != g.server.host_id,
                focused: active.is_some_and(|w| w.focused_block == b.block_id),
                zoomed: window.is_some_and(|w| w.zoomed_block == b.block_id),
                ..Default::default()
            };
            let mut why = Vec::new();
            if b.creator != TERMINAL {
                pane.doing = Doing::Running;
            } else {
                match &b.process {
                    Some(Ok(p)) => {
                        pane.child_pid = p.child_pid;
                        pane.child_cwd = p.child_cwd.clone();
                        pane.last_exit = p.last_exit;
                        pane.fg = p.foreground.clone();
                        pane.doing = match (&p.foreground, p.child_pid) {
                            (Some(fg), Some(child))
                                if fg.pid == child
                                    && parse::is_shell(&p.child_name, &p.child_argv0) =>
                            {
                                Doing::Prompt
                            }
                            (Some(_), _) => Doing::Running,
                            (None, _) => {
                                why.push("no foreground process reported".to_string());
                                Doing::Unknown
                            }
                        };
                    }
                    Some(Err(e)) => why.push(e.clone()),
                    None => why.push("process was not read".into()),
                }
                match &b.title {
                    Some(Ok(t)) => pane.title = t.clone(),
                    Some(Err(e)) => why.push(format!("title: {}", e)),
                    None => {}
                }
                match &b.records {
                    Some(Ok(r)) => pane.records = r.clone(),
                    Some(Err(e)) => {
                        pane.status_unread = true;
                        why.push(format!("program_status: {}", e));
                    }
                    None => {}
                }
            }
            pane.why = why.join(" · ");
            pane.agent = parse::classify_agent(&pane.records, pane.fg.as_ref(), catalogue);
            pane.is_opscope = pane.fg.as_ref().is_some_and(|fg| {
                process_filter::matches(&fg.name, &format!("{} {}", fg.argv0, fg.invoked_path))
            });
            // How long this foreground has been in front, timed by the
            // widget - a lower bound when it was already there at the first
            // read. Reported states carry the server's own timestamp and do
            // not use this.
            let pid = pane.fg.as_ref().map(|f| f.pid).unwrap_or(0);
            let entry = seen
                .since
                .entry(pane.block_id.clone())
                .or_insert((pid, at, !seen.first));
            if entry.0 != pid {
                *entry = (pid, at, true);
            }
            pane.seen_since = entry.1;
            pane.seen_exact = entry.2;
            pane.remote = pane.remote || !local;
            panes.push(pane);
        }
    }
    let live: Vec<String> = panes.iter().map(|p| p.block_id.clone()).collect();
    seen.since.retain(|id, _| live.contains(id));
    seen.first = false;
    let apps: Vec<&parse::Client> = g.clients.iter().filter(|c| c.kind == "app").collect();
    Board {
        panes,
        unread_sessions: unread,
        sessions: g.sessions.len(),
        apps: apps.len(),
        app_sessions: apps
            .iter()
            .flat_map(|c| c.session_ids.iter().cloned())
            .collect(),
        rex_version: g.server.rex_version.clone(),
        api_revision: g.server.api_revision,
        local_usage: local,
    }
}

/// Worst first, then the one waiting longest, then where it is.
fn order(panes: &mut [Pane]) {
    panes.sort_by(|a, b| {
        a.rank()
            .cmp(&b.rank())
            .then_with(|| {
                let at = |p: &Pane| p.root().and_then(|r| r.updated_at).unwrap_or(f64::MAX);
                at(a).total_cmp(&at(b))
            })
            .then(a.session_ord.cmp(&b.session_ord))
            .then(a.window_ord.cmp(&b.window_ord))
    });
}

fn poll(
    state: &Arc<Mutex<State>>,
    server: &str,
    catalogue: &[String],
    seen: &mut Seen,
    usage: &mut host::Usage,
) {
    let read = rex(server, &["--timeout", "5s", "do", "-e", GATHER_LUA]);
    let gather = match read.and_then(|text| parse::parse_gather(&text).map_err(Absence::Other)) {
        Ok(g) => g,
        Err(absence) => {
            if let Ok(mut guard) = state.lock() {
                // Rows from a server that has stopped answering are rows
                // about the past drawn as the present, so they go.
                guard.board = None;
                guard.absence = Some(absence);
            }
            return;
        }
    };
    let at = tc::now();
    let restarted = !seen.instance.is_empty() && seen.instance != gather.server.instance_id;
    if restarted {
        // Every duration held is about a server that no longer exists.
        seen.since.clear();
        seen.first = true;
    }
    seen.instance = gather.server.instance_id.clone();
    let local = talks_to_local_server(server);
    let mut board = build(&gather, catalogue, seen, local, at);
    if local {
        let pids: Vec<i64> = board
            .panes
            .iter()
            .filter(|p| !p.remote && p.doing == Doing::Running)
            .filter_map(|p| p.fg.as_ref().map(|f| f.pid))
            .collect();
        let read = usage.sample(&pids, at);
        for pane in &mut board.panes {
            if let Some((cpu, rss)) = pane.fg.as_ref().and_then(|f| read.get(&f.pid)) {
                pane.cpu = *cpu;
                pane.rss = Some(*rss);
            }
        }
    }
    tc::notices::when(board.api_revision > API_REVISION_BUILT_AGAINST, || {
        tc::notices::Notice::info(
            "rex-api-newer",
            "rex: the server's API is newer than this widget was written against",
        )
    });
    tc::notices::when(board.api_revision < API_REVISION_BUILT_AGAINST, || {
        tc::notices::Notice::warn(
            "rex-api-older",
            "rex: the server's API is older than this widget was written against - a pane may say a method is missing",
        )
    });
    if let Ok(mut guard) = state.lock() {
        answer_bells(&mut guard.attention, &mut board.panes, &board.app_sessions);
        order(&mut board.panes);
        guard.board = Some(board);
        guard.absence = None;
        if restarted {
            guard.restarted_at = Some(at);
        }
    }
}

fn ago(seconds: f64) -> String {
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

fn mem(bytes: Option<u64>) -> String {
    let Some(bytes) = bytes else {
        return "   --".to_string();
    };
    let mut value = bytes as f64;
    for unit in ["B", "K", "M", "G"] {
        if value < 1024.0 {
            return format!("{:>4.0}{}", value, unit);
        }
        value /= 1024.0;
    }
    format!("{:>4.1}T", value)
}

fn percent(cpu: Option<f64>) -> String {
    match cpu {
        Some(v) => format!("{:>4.0}%", v),
        None => format!("{:>5}", "-"),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Keep the end of a path, marking the cut so it does not read as a name.
/// herdr-panes' rule: in a directory the end is the part that says where.
fn tail_path(path: &str, n: usize) -> String {
    if tc::display_width(path) <= n || n < 2 {
        return path.to_string();
    }
    let mut start = path.len();
    for (at, _) in path.char_indices().rev() {
        if tc::display_width(&path[at..]) > n - 1 {
            break;
        }
        start = at;
    }
    format!("…{}", &path[start..])
}

/// The home-relative form of a directory, which is how a person names it.
fn homely(path: &str) -> String {
    let path = path
        .strip_prefix("file://")
        .map_or(path, |rest| rest.find('/').map_or(rest, |at| &rest[at..]));
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        return path.to_string();
    }
    tilde(path, &home)
}

/// `path` with `home` written as `~`, only where `home` ends at a directory
/// boundary: `/home/al` is not the start of `/home/alice`.
fn tilde(path: &str, home: &str) -> String {
    match path.strip_prefix(home) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{}", rest),
        _ => path.to_string(),
    }
}

struct Palette {
    blocked: String,
    blocked_lit: String,
    done: String,
    working: String,
    idle: String,
    idle_lit: String,
    unknown: String,
    unknown_lit: String,
    dim: String,
    /// `dim` lifted until it clears AA on every tint here; swapped in by
    /// the tint closure, never at a call site.
    dim_lit: String,
    txt: String,
    lbl: String,
    accent: String,
    proc: String,
    idle_c: String,
    idle_c_lit: String,
}

fn palette() -> Palette {
    Palette {
        blocked: tc::rgb(255, 105, 115),
        blocked_lit: tc::rgb(255, 128, 136),
        done: tc::rgb(90, 240, 160),
        working: tc::rgb(255, 200, 90),
        idle: tc::rgb(128, 148, 172),
        idle_lit: tc::rgb(152, 168, 188),
        unknown: tc::rgb(150, 150, 165),
        unknown_lit: tc::rgb(165, 165, 178),
        dim: tc::rgb(127, 147, 172),
        dim_lit: tc::rgb(140, 170, 195),
        txt: tc::rgb(225, 235, 245),
        lbl: tc::rgb(130, 165, 200),
        accent: tc::rgb(150, 210, 255),
        proc: tc::rgb(170, 190, 215),
        idle_c: tc::rgb(122, 138, 160),
        idle_c_lit: tc::rgb(155, 167, 184),
    }
}

fn colour_of<'a>(state: &str, p: &'a Palette) -> &'a str {
    match state {
        "blocked" | "error" => &p.blocked,
        "done" => &p.done,
        "working" | "attention" => &p.working,
        "idle" => &p.idle,
        _ => &p.unknown,
    }
}

fn mark_of(state: &str, tick: usize) -> char {
    match state {
        "blocked" => '⚠',
        "error" => '✗',
        "done" => '✓',
        "working" => tc::SPINNER[tick % tc::SPINNER.len()],
        "idle" => '·',
        _ => '?',
    }
}

/// The facts under a pane's directory-and-title row, as phrases.
///
/// Herdr's rows stop at the title; Rex knows more, and it goes here rather
/// than into columns: what a blocked program is waiting on, how far along it
/// is, the message it reported. Wrapped at word boundaries by the caller,
/// never cut - a blocked agent's message is the half of the row worth
/// reading.
fn detail_phrases(pane: &Pane, at: f64) -> Vec<String> {
    let mut out = Vec::new();
    let root = pane.root();
    if let Some(r) = root {
        if !r.app.is_empty() && pane.agent.is_none() {
            out.push(format!("reported by {}", r.app));
        }
        if !r.kind.is_empty() {
            out.push(r.kind.clone());
        }
        if let Some(n) = r.progress {
            out.push(format!("{}%", n));
        }
        if !r.msg.is_empty() {
            out.push(r.msg.clone());
        }
        if pane.agent.is_none() {
            if let Some(u) = r.updated_at {
                out.push(format!("for {}", ago(at - u)));
            }
        }
        if pane.reporter_gone(r) {
            out.push("reporter not in foreground".into());
        }
    } else if pane.agent.is_some() && !pane.status_unread {
        // Said on the row, because an unknown beside a known looks like the
        // same kind of answer and is not one. A status that could not be
        // read is not this - its reason is drawn on its own row instead.
        out.push("no status reported".into());
    }
    if let Some((when, said)) = &pane.attention {
        out.push(format!(
            "asked for attention {} ago: {}",
            ago(at - when),
            said
        ));
    }
    out
}

/// A section's panes regrouped by session, without losing who needs you.
///
/// Each session's panes stay together, in the order they already had -
/// worst first. The sessions are ordered by their most urgent pane, then by
/// Rex's own session order, so a blocked agent still puts its session at
/// the top of the section rather than waiting behind a quiet one that
/// happens to sort first by name.
fn by_session(panes: Vec<&Pane>) -> Vec<&Pane> {
    let mut groups: Vec<(usize, usize, Vec<&Pane>)> = Vec::new();
    for pane in panes {
        match groups
            .iter_mut()
            .find(|(_, _, g)| g[0].session_id == pane.session_id)
        {
            Some((worst, _, g)) => {
                *worst = (*worst).min(pane.rank());
                g.push(pane);
            }
            None => groups.push((pane.rank(), pane.session_ord, vec![pane])),
        }
    }
    groups.sort_by_key(|(worst, ord, _)| (*worst, *ord));
    groups.into_iter().flat_map(|(_, _, g)| g).collect()
}

/// One frame's worth of a pane: its rows, each already coloured.
///
/// The same grammar as herdr-panes, so the two read alike side by side: an
/// agent is its state row and then its directory and title; a process is one
/// row; an idle shell is its directory. What only Rex knows - the reported
/// kind, progress and message, and child records - follows as indented rows.
fn render_pane(
    pane: &Pane,
    here: bool,
    w: usize,
    cols: &Columns,
    p: &Palette,
    tick: usize,
    at: f64,
) -> Vec<String> {
    let state = pane.state();
    let loud = matches!(state.as_str(), "blocked" | "error" | "done");
    // Blocked and done keep a tint of their own even unselected: the whole
    // point is that they are visible without being looked for.
    let tint = if here {
        tc::bg(38, 56, 76)
    } else if matches!(state.as_str(), "blocked" | "error") {
        tc::bg(46, 26, 30)
    } else if state == "done" {
        tc::bg(22, 46, 34)
    } else {
        String::new()
    };
    let c = |colour: &str| {
        // Any colour that would not clear AA on this tint is swapped for
        // its lighter twin, measured rather than guessed.
        let colour = if tint.is_empty() {
            colour
        } else if colour == p.dim {
            p.dim_lit.as_str()
        } else if colour == p.idle {
            p.idle_lit.as_str()
        } else if colour == p.unknown {
            p.unknown_lit.as_str()
        } else if colour == p.blocked {
            p.blocked_lit.as_str()
        } else if colour == p.idle_c {
            p.idle_c_lit.as_str()
        } else {
            colour
        };
        format!("{}{}", tint, colour)
    };
    let fill = |line: &mut Vec<(String, String)>| {
        if !tint.is_empty() {
            line.push((tint.clone(), " ".repeat(w)));
        }
    };
    let finish = |line: Vec<(String, String)>| {
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        tc::seg(&refs, w.saturating_sub(1))
    };
    // Indented prose under a row, wrapped to the pane, tinted with it.
    let prose = |rows: &mut Vec<String>, colour: &str, text: &str| {
        for line in tc::wrap_words(text, w.saturating_sub(5)) {
            let mut l = vec![(c(colour), format!("   {}", line))];
            fill(&mut l);
            rows.push(finish(l));
        }
    };
    let cursor = if here { "▸" } else { " " };
    // `heat_on`, because these cells go through the tint closure and the
    // plain ramp's hot end cannot be read on any of the three tints.
    let heat = match pane.cpu {
        Some(v) if v > 0.0 => tc::heat_on((v / 100.0).min(1.0), !tint.is_empty()),
        _ => p.dim.clone(),
    };
    let usage = |line: &mut Vec<(String, String)>| {
        if pane.remote && cols.cpu {
            line.push((c(&p.dim), format!("{:>5}", "—")));
            if cols.wide {
                line.push((c(&p.dim), format!(" {:>5}", "—")));
            }
        } else if cols.cpu {
            line.push((c(&heat), percent(pane.cpu)));
            if cols.wide {
                line.push((c(&p.dim), format!(" {}", mem(pane.rss))));
            }
        }
    };
    let body = if loud || here { &p.txt } else { &p.dim };
    let mut rows = Vec::new();
    let children = |rows: &mut Vec<String>| {
        for r in pane.children() {
            let mut words = vec![r.id.clone().unwrap_or_default(), r.state.clone()];
            if !r.msg.is_empty() {
                words.push(r.msg.clone());
            }
            if let Some(u) = r.updated_at {
                words.push(ago(at - u));
            }
            for (i, text) in tc::wrap_words(&words.join("  "), w.saturating_sub(7))
                .into_iter()
                .enumerate()
            {
                let mut line = vec![(
                    c(colour_of(&r.state, p)),
                    format!("   {} {}", if i == 0 { "├" } else { " " }, text),
                )];
                fill(&mut line);
                rows.push(finish(line));
            }
        }
    };
    match pane.section() {
        Section::Agents => {
            let colour = colour_of(&state, p);
            let name = pane
                .agent
                .as_ref()
                .map(|(n, _)| n.clone())
                .unwrap_or_default();
            // Reported states carry the server's timestamp and are exact;
            // the rest are timed here, and say ≥ when they were already in
            // place at the first read.
            let for_cell = match pane.root().and_then(|r| r.updated_at) {
                Some(u) => ago(at - u),
                None => format!(
                    "{}{}",
                    if pane.seen_exact { "" } else { "≥" },
                    ago(at - pane.seen_since)
                ),
            };
            let state_cell = if loud {
                state.to_uppercase()
            } else {
                state.clone()
            };
            // Below about twenty-five columns the mark, a six-cell name,
            // STATE and FOR do not fit on one row; the state and its
            // duration go under the name rather than being cut off it.
            let one_row = 3 + cols.name + 9 + 7 <= w.saturating_sub(1);
            let mut line = vec![(
                c(colour),
                format!(
                    "{}{} {}",
                    cursor,
                    mark_of(&state, tick),
                    tc::pad(&name, cols.name)
                ),
            )];
            if one_row {
                line.push((c(colour), format!(" {:<8}", state_cell)));
                line.push((c(&p.dim), format!(" {:<6}", for_cell)));
            }
            usage(&mut line);
            if cols.wide {
                line.push((
                    c(&p.accent),
                    format!(" {}", tc::pad(&pane.window(), cols.place)),
                ));
            }
            fill(&mut line);
            rows.push(finish(line));
            if !one_row {
                prose(&mut rows, colour, &format!("{}  {}", state_cell, for_cell));
            }
            // The second row is herdr's: where it is, and what its title says.
            let title = pane.title.trim();
            let dir = homely(&pane.cwd());
            let mut line = vec![(c(&p.dim), format!("   {}  ", dir))];
            let room = w
                .saturating_sub(1)
                .checked_sub(tc::display_width(&line[0].1));
            match room {
                Some(room) if tc::display_width(title) <= room => {
                    line.push((c(body), title.to_string()));
                    fill(&mut line);
                    rows.push(finish(line));
                }
                Some(_) => {
                    fill(&mut line);
                    rows.push(finish(line));
                    prose(&mut rows, body, title);
                }
                // A directory wider than the pane wraps whole, so its
                // distinguishing tail is still on screen.
                None => {
                    prose(&mut rows, &p.dim, &dir);
                    if !title.is_empty() {
                        prose(&mut rows, body, title);
                    }
                }
            }
            if !pane.why.is_empty() {
                prose(&mut rows, &p.unknown, &format!("⚠ {}", pane.why));
            }
            let facts = detail_phrases(pane, at);
            if !facts.is_empty() {
                prose(&mut rows, body, &facts.join(" · "));
            }
            children(&mut rows);
        }
        Section::Processes => {
            let unreadable = pane.doing == Doing::Unknown && pane.creator == TERMINAL;
            let command = if unreadable {
                "could not be read".to_string()
            } else {
                let c = pane.command();
                if c.is_empty() {
                    "?".into()
                } else {
                    c
                }
            };
            // An unread pane says so in words where a command would go. A
            // bare "?" is what a running pane with unparseable argv shows,
            // and the two are not the same thing at all.
            let fits = tc::display_width(&command) <= cols.command;
            // Too long for its column: the head of it stays on the row, and
            // the rest wraps underneath at its own spaces, under where it
            // started. Never cut, and never a row with an empty command.
            let pieces = if fits {
                Vec::new()
            } else {
                tc::wrap_words(&command, cols.command.max(1))
            };
            let mut line = vec![
                (
                    c(if unreadable { &p.unknown } else { &p.proc }),
                    format!("{}{} ", cursor, if unreadable { '⚠' } else { '▪' }),
                ),
                (
                    c(if unreadable { &p.unknown } else { &p.txt }),
                    tc::pad(
                        if fits {
                            &command
                        } else {
                            pieces.first().map_or("", |s| s.as_str())
                        },
                        cols.command,
                    ),
                ),
            ];
            if cols.states {
                line.push((
                    c(colour_of(&state, p)),
                    format!(
                        " {:<8}",
                        if pane.root().is_some() {
                            state.clone()
                        } else {
                            String::new()
                        }
                    ),
                ));
            }
            line.push((String::new(), " ".into()));
            usage(&mut line);
            if cols.wide {
                line.push((
                    c(&p.accent),
                    format!(" {}", tc::pad(&pane.window(), cols.place)),
                ));
            }
            fill(&mut line);
            rows.push(finish(line));
            for piece in pieces.iter().skip(1) {
                let mut l = vec![(c(&p.txt), format!("   {}", piece))];
                fill(&mut l);
                rows.push(finish(l));
            }
            if unreadable || !pane.why.is_empty() {
                let why = if pane.why.is_empty() {
                    "no reason given"
                } else {
                    &pane.why
                };
                prose(&mut rows, &p.unknown, &format!("⚠ {}", why));
            }
            let facts = detail_phrases(pane, at);
            if !facts.is_empty() {
                prose(&mut rows, body, &facts.join(" · "));
            }
            children(&mut rows);
        }
        Section::Idle => {
            let mut line = vec![
                (c(&p.idle_c), format!("{}▫ ", cursor)),
                (
                    c(&p.idle_c),
                    tc::pad(&tail_path(&homely(&pane.cwd()), cols.dir), cols.dir + 1),
                ),
                (
                    c(&p.accent),
                    if cols.wide {
                        pane.window()
                    } else {
                        String::new()
                    },
                ),
            ];
            fill(&mut line);
            rows.push(finish(line));
        }
    }
    rows
}

/// How wide each text column is drawn this frame.
///
/// The text columns grow with the pane, herdr's way: their fixed widths are
/// the minimums and whatever the numbers leave goes to the column still
/// cutting something.
struct Columns {
    wide: bool,
    /// Whether any process carries a reported state, so the column is drawn.
    states: bool,
    /// Whether CPU fits beside the name. Below about thirty columns it does
    /// not, and the column goes whole - head included - rather than `C`.
    cpu: bool,
    /// More than one session on the board, so where a pane is says something.
    sessions: bool,
    name: usize,
    command: usize,
    place: usize,
    dir: usize,
}

impl Columns {
    fn fit(w: usize, panes: &[&Pane]) -> Columns {
        let wide = w >= 66;
        let widest = |f: &dyn Fn(&Pane) -> usize, sec: Section| -> usize {
            panes
                .iter()
                .filter(|p| p.section() == sec)
                .map(|p| f(p))
                .max()
                .unwrap_or(0)
        };
        let states = panes
            .iter()
            .any(|p| p.section() == Section::Processes && p.root().is_some());
        let name_want = widest(
            &|p| tc::display_width(&p.agent.as_ref().map(|a| a.0.clone()).unwrap_or_default()),
            Section::Agents,
        );
        let command_want = widest(&|p| tc::display_width(&p.command()), Section::Processes)
            .max(tc::display_width("could not be read"));
        let place_want = panes
            .iter()
            .map(|p| tc::display_width(&p.window()))
            .chain(std::iter::once(tc::display_width("WINDOW")))
            .max()
            .unwrap_or(0);
        let dir_want = widest(&|p| tc::display_width(&homely(&p.cwd())), Section::Idle);
        let row = w.saturating_sub(1);
        let state_w = if states { 9 } else { 0 };
        let sessions = {
            let mut ids: Vec<&str> = panes.iter().map(|p| p.session_id.as_str()).collect();
            ids.sort_unstable();
            ids.dedup();
            ids.len() > 1
        };
        // CPU earns its five cells once the name and the state still fit
        // beside it: the mark, a six-cell name, STATE and FOR come first.
        let cpu = wide || row >= 3 + 6 + 9 + 7 + 5;
        let cpu_w = if cpu { 5 } else { 0 };
        // An agent row: `▸`, the mark and a space, the name, STATE, FOR and
        // CPU; MEM and the place when wide. A process row: the same three,
        // the command, an optional STATE, CPU; MEM and the place when wide.
        let (name, command, place) = if wide {
            let agent = tc::fit_columns(
                row.saturating_sub(3 + 9 + 7 + 5 + 6 + 1),
                &[(6, name_want), (12, place_want)],
            );
            let process = tc::fit_columns(
                row.saturating_sub(3 + state_w + 1 + 5 + 6 + 1),
                &[(20, command_want), (12, place_want)],
            );
            (agent[0], process[0], agent[1].min(process[1]))
        } else {
            (
                tc::fit_columns(row.saturating_sub(3 + 9 + 7 + cpu_w), &[(6, name_want)])[0],
                tc::fit_columns(
                    row.saturating_sub(3 + state_w + 1 + cpu_w),
                    &[(6, command_want)],
                )[0],
                place_want,
            )
        };
        let place_cells = if wide { 1 + place_want } else { 0 };
        let dir = tc::fit_columns(row.saturating_sub(3 + place_cells), &[(6, dir_want)])[0];
        Columns {
            wide,
            states,
            cpu,
            sessions,
            name,
            command,
            place,
            dir,
        }
    }
}

/// The detail screen's text for one pane: every record, every identity.
/// Hand each pane the bell it rang, unless it has been answered - dropping
/// those, and any for a pane that has gone.
///
/// A bell is answered once someone has looked: the pane is focused in a
/// session a Rex app is showing, or the program has reported anything newer.
/// Focus alone is not looking - a session nobody has attached keeps its last
/// focused block forever, and a bell from that one is exactly what this
/// board is for.
fn answer_bells(
    attention: &mut HashMap<String, (f64, String)>,
    panes: &mut [Pane],
    shown: &[String],
) {
    for pane in panes.iter_mut() {
        if let Some((when, said)) = attention.get(&pane.block_id).cloned() {
            let newer = pane
                .records
                .iter()
                .any(|r| r.updated_at.is_some_and(|u| u > when));
            if (pane.focused && shown.contains(&pane.session_id)) || newer {
                attention.remove(&pane.block_id);
            } else {
                pane.attention = Some((when, said));
            }
        }
    }
    attention.retain(|id, _| panes.iter().any(|p| &p.block_id == id));
}

/// Where the cursor lands once the list is rebuilt. The cursor is a pane,
/// not a position: a refresh that puts a newly blocked agent above it must
/// not move `↵` onto somebody else. A pane that has gone leaves the index
/// where it was, clamped by the caller.
fn reselect(before: &[Pane], selected: usize, after: &[Pane]) -> usize {
    before
        .get(selected)
        .and_then(|held| after.iter().position(|n| n.block_id == held.block_id))
        .unwrap_or(selected)
}

/// The server, as the pane may name it: never the setting itself, which
/// can be an address, because the pane gets screenshotted.
fn server_words(server: &str) -> &'static str {
    if server.is_empty() {
        "local server"
    } else {
        "server set in ,"
    }
}

fn explain_lines(pane: &Pane, board: &Board, server: &str, at: f64) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let heading = |s: &str| ("lbl".to_string(), format!(" ── {} ──", s));
    out.push(heading("PANE"));
    out.push((
        "txt".into(),
        format!(
            " {} · block …{} · session {} · window {}{}",
            if pane.label.is_empty() {
                "unlabelled"
            } else {
                &pane.label
            },
            pane.short_id(),
            pane.session_label,
            pane.window_ord
                .map(|n| n.to_string())
                .unwrap_or_else(|| "none".into()),
            if pane.window_label.is_empty() {
                String::new()
            } else {
                format!(" ({})", pane.window_label)
            }
        ),
    ));
    let mut facts = Vec::new();
    if pane.detached {
        facts.push("detached: in no window".to_string());
    }
    if let Some(min) = pane.floating {
        facts.push(if min {
            "floating, minimised".into()
        } else {
            "floating".into()
        });
    }
    if pane.focused {
        facts.push("focused in its session".into());
    }
    if pane.zoomed {
        facts.push("zoomed".into());
    }
    facts.push(if pane.remote {
        "on another host, or a server this widget cannot see the processes of".into()
    } else {
        "on this machine".into()
    });
    if !board.app_sessions.contains(&pane.session_id) {
        facts.push("no Rex app is showing this session".into());
    }
    out.push(("dim".into(), format!(" {}", facts.join(" · "))));
    out.push((String::new(), String::new()));
    out.push(heading("PROGRAM"));
    match &pane.fg {
        Some(fg) => {
            out.push(("txt".into(), format!(" {}", parse::command_label(fg))));
            for (k, v) in [
                ("name", &fg.name),
                ("argv0", &fg.argv0),
                ("invoked", &fg.invoked_path),
                ("script", &fg.script),
                ("cwd", &fg.cwd),
            ] {
                if !v.is_empty() {
                    out.push(("dim".into(), format!("   {:<8} {}", k, homely(v))));
                }
            }
            out.push((
                "dim".into(),
                format!(
                    "   {:<8} {}",
                    "front",
                    if pane.doing == Doing::Prompt {
                        "the shell, at its prompt"
                    } else {
                        "running"
                    }
                ),
            ));
        }
        None => out.push((
            "unknown".into(),
            format!(
                " no foreground program: {}",
                if pane.why.is_empty() {
                    "none reported"
                } else {
                    &pane.why
                }
            ),
        )),
    }
    if let Some(code) = pane.last_exit {
        out.push(("dim".into(), format!("   last exit {}", code)));
    }
    if !pane.title.is_empty() {
        out.push(("dim".into(), format!("   title    {}", pane.title)));
    }
    if let Some((_, source)) = &pane.agent {
        out.push(("dim".into(), format!("   agent    {}", source.words())));
    }
    out.push((String::new(), String::new()));
    out.push(heading("STATUS REPORTS (OSC 7501)"));
    if pane.status_unread {
        out.push((
            "unknown".into(),
            format!(" could not be read: {}", pane.why),
        ));
    } else if pane.records.is_empty() {
        out.push((
            "dim".into(),
            " none - this program has not reported a status, so none is shown".into(),
        ));
    }
    for r in &pane.records {
        out.push((
            r.state.clone(),
            format!(
                " {} {}{}",
                r.id.as_deref().unwrap_or("(root)"),
                r.state,
                r.updated_at
                    .map(|u| format!(" · {} ago", ago(at - u)))
                    .unwrap_or_default()
            ),
        ));
        for (k, v) in [
            ("app", r.app.clone()),
            ("kind", r.kind.clone()),
            (
                "progress",
                r.progress.map(|n| format!("{}%", n)).unwrap_or_default(),
            ),
            ("title", r.title.clone()),
            ("msg", r.msg.clone()),
            (
                "reporter",
                match r.owner_pid {
                    Some(pid) => format!(
                        "{} (pid {}){}",
                        r.owner_name,
                        pid,
                        if pane.reporter_gone(r) {
                            " · not in foreground"
                        } else {
                            ""
                        }
                    ),
                    None => String::new(),
                },
            ),
        ] {
            if !v.is_empty() {
                out.push(("dim".into(), format!("   {:<9} {}", k, v)));
            }
        }
    }
    if let Some((when, said)) = &pane.attention {
        out.push((
            "working".into(),
            format!(" asked for attention {} ago: {}", ago(at - when), said),
        ));
    }
    out.push((String::new(), String::new()));
    out.push(heading("SERVER"));
    out.push((
        "dim".into(),
        format!(
            " {} · rex {} · api {} · {} Rex app{} attached",
            server_words(server),
            if board.rex_version.is_empty() {
                "?"
            } else {
                &board.rex_version
            },
            board.api_revision,
            board.apps,
            plural(board.apps)
        ),
    ));
    out
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    if !tc::dependencies_available(
        "rex-panes",
        include_str!("dependencies.json"),
        Some(SETTINGS),
    ) {
        return;
    }
    let cfg = tc::load_config("rex_panes");
    let mut refresh = tc::poll_secs(tc::cfg_f64(&cfg, "refresh", 4.0), 4.0);
    let server = tc::cfg_str(&cfg, "server", "");
    let catalogue = tc::cfg_strings(&cfg, "agents", DEFAULT_AGENTS);
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() >= 2 && (args[0] == "-n" || args[0] == "--refresh") {
        refresh = tc::poll_secs(args[1].parse().unwrap_or(4.0), 4.0).max(1.0);
    }

    let p = palette();
    let state = Arc::new(Mutex::new(State::default()));
    let activity = tc::Refresh::default();
    let wake = refresh_wake::new();
    let stop = Arc::new(AtomicBool::new(false));
    let events_pid = Arc::new(AtomicI32::new(0));
    {
        let state = Arc::clone(&state);
        let wake = Arc::clone(&wake);
        let activity = activity.clone();
        let stop = Arc::clone(&stop);
        let server = server.clone();
        std::thread::spawn(move || {
            let mut seen = Seen {
                first: true,
                ..Default::default()
            };
            let mut usage = host::Usage::default();
            loop {
                let work = activity.start();
                // A poller that dies takes its explanation with it, and an
                // empty board looks exactly like a server with nothing open.
                let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    poll(&state, &server, &catalogue, &mut seen, &mut usage)
                }));
                if let Err(payload) = step {
                    if let Ok(mut guard) = state.lock() {
                        guard.err = format!("poller stopped: {}", panic_reason(payload.as_ref()));
                    }
                    return;
                }
                drop(work);
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                // A focus change arrives as a burst of events; one read
                // after the burst says everything the burst did.
                std::thread::sleep(Duration::from_millis(250));
                if refresh_wake::wait_for(&wake, Duration::from_secs_f64(refresh)).is_err() {
                    if let Ok(mut guard) = state.lock() {
                        guard.err = "poller wake signal failed".into();
                    }
                    return;
                }
            }
        });
    }
    {
        let watcher_state = Arc::clone(&state);
        let wake = Arc::clone(&wake);
        let stop = Arc::clone(&stop);
        let server = server.clone();
        let pid = Arc::clone(&events_pid);
        std::thread::spawn(move || {
            let loop_state = Arc::clone(&watcher_state);
            let watched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                events::watch(loop_state, wake, server, stop, pid)
            }));
            if let Err(payload) = watched {
                if let Ok(mut guard) = watcher_state.lock() {
                    guard.watcher_err =
                        format!("event watcher stopped: {}", panic_reason(payload.as_ref()));
                }
            }
        });
    }

    // The explain screen's screen capture, read off the input loop.
    let capture: Arc<Mutex<Option<(String, Result<Vec<String>, String>)>>> =
        Arc::new(Mutex::new(None));
    // What focus or zoom answered, also off the input loop: on a slow or
    // remote server either can take the whole run timeout, and the pane
    // must keep drawing and reading keys while it does.
    let acted: Arc<Mutex<Option<(String, bool, f64)>>> = Arc::new(Mutex::new(None));
    let act = |what: &'static str, pane: Pane, board: Option<Board>| {
        let (acted, wake, server) = (Arc::clone(&acted), Arc::clone(&wake), server.clone());
        std::thread::spawn(move || {
            let answer = if what == "zoom" {
                zoom(&server, &pane, tc::now())
            } else {
                focus(&server, &pane, board.as_ref(), tc::now())
            };
            if let Ok(mut slot) = acted.lock() {
                *slot = Some(answer);
            }
            refresh_wake::request(&wake);
        });
    };

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut show_idle = true;
    let mut opscope_filter = process_filter::Filter::default();
    let mut selected = 0usize;
    let mut placed: Vec<(usize, usize)> = Vec::new();
    let mut scroll = 0usize;
    let mut moved = false;
    let mut note: Option<(String, bool, f64)> = None;
    let mut panes_now: Vec<Pane> = Vec::new();
    let mut sections: Vec<usize> = Vec::new();
    let mut head_rows = 0usize;
    // The block the explain screen is open on, and how far it is scrolled.
    let mut explaining: Option<String> = None;
    let mut explain_scroll = 0usize;
    let mut page = 10usize;

    let quit = |keyboard: &mut tc::Keyboard| {
        stop.store(true, Ordering::Relaxed);
        let pid = events_pid.load(Ordering::Relaxed);
        if pid > 0 {
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        keyboard.restore();
        tc::restore_screen();
    };

    loop {
        let tick = tc::animation_tick();
        let at = tc::now();
        let mut keys = keyboard.poll();
        if explaining.is_none() {
            if let Some(i) =
                tc::rows_clicked(&mut keys, Some(selected), head_rows, scroll, &placed, None)
            {
                selected = i;
                moved = true;
            }
        }
        let current = panes_now
            .get(selected.min(panes_now.len().saturating_sub(1)))
            .cloned();
        let board_now = state.lock().ok().and_then(|g| g.board.clone());
        for key in keys {
            match key.as_str() {
                "q" | "Q" => {
                    quit(&mut keyboard);
                    return;
                }
                "," => {
                    tc::run_settings(&mut keyboard, SETTINGS);
                    continue;
                }
                "r" | "R" => refresh_wake::request(&wake),
                "esc" if explaining.is_some() => explaining = None,
                "e" | "E" => {
                    if explaining.is_some() {
                        explaining = None;
                    } else if let Some(pane) = &current {
                        explaining = Some(pane.block_id.clone());
                        explain_scroll = 0;
                        if let Ok(mut slot) = capture.lock() {
                            *slot = None;
                        }
                        if pane.creator == TERMINAL {
                            let capture = Arc::clone(&capture);
                            let (server, session, block) = (
                                server.clone(),
                                pane.session_id.clone(),
                                pane.block_id.clone(),
                            );
                            std::thread::spawn(move || {
                                let read = rex(
                                    &server,
                                    &["capture", "-s", &session, "-b", &block, "--trim"],
                                )
                                .map(|text| parse::parse_capture(&text, 40))
                                .map_err(|why| match why {
                                    Absence::Gone => "the pane closed".to_string(),
                                    Absence::NoServer => "no Rex server is running".to_string(),
                                    Absence::Unreachable => "the server did not answer".to_string(),
                                    Absence::Other(text) => text,
                                });
                                if let Ok(mut slot) = capture.lock() {
                                    *slot = Some((block, read));
                                }
                            });
                        }
                    }
                }
                "i" | "I" if explaining.is_none() => {
                    show_idle = !show_idle;
                    selected = 0;
                    moved = true;
                }
                "o" | "O" if explaining.is_none() => {
                    opscope_filter.toggle(&mut selected);
                    moved = true;
                }
                "up" | "k" => {
                    if explaining.is_some() {
                        explain_scroll = explain_scroll.saturating_sub(1);
                    } else {
                        selected = selected.saturating_sub(1);
                        moved = true;
                    }
                }
                "down" | "j" => {
                    if explaining.is_some() {
                        explain_scroll += 1;
                    } else {
                        selected += 1;
                        moved = true;
                    }
                }
                // The wheel moves the view and nothing else.
                "ctrl-y" | "wheel-up" => {
                    if explaining.is_some() {
                        explain_scroll = explain_scroll.saturating_sub(1);
                    } else {
                        scroll = scroll.saturating_sub(1);
                    }
                }
                "ctrl-e" | "wheel-down" => {
                    if explaining.is_some() {
                        explain_scroll += 1;
                    } else {
                        scroll += 1;
                    }
                }
                "pgup" => {
                    if explaining.is_some() {
                        explain_scroll = explain_scroll.saturating_sub(page);
                    } else {
                        scroll = scroll.saturating_sub(page);
                    }
                }
                "pgdn" => {
                    if explaining.is_some() {
                        explain_scroll += page;
                    } else {
                        scroll += page;
                    }
                }
                "tab" if explaining.is_none() => {
                    if let Some(next) = sections
                        .iter()
                        .find(|&&s| s > selected)
                        .or_else(|| sections.first())
                    {
                        selected = *next;
                        moved = true;
                    }
                }
                "home" if explaining.is_none() => {
                    selected = 0;
                    moved = true;
                }
                "end" if explaining.is_none() => {
                    selected = panes_now.len().saturating_sub(1);
                    moved = true;
                }
                "enter" | "f" | "F" => {
                    let target = match &explaining {
                        Some(id) => panes_now.iter().find(|p| &p.block_id == id).cloned(),
                        None => current.clone(),
                    };
                    if let Some(pane) = target {
                        note = Some(("focusing…".into(), true, at + 10.0));
                        act("focus", pane, board_now.clone());
                    }
                }
                "z" | "Z" => {
                    let target = match &explaining {
                        Some(id) => panes_now.iter().find(|p| &p.block_id == id).cloned(),
                        None => current.clone(),
                    };
                    if let Some(pane) = target {
                        note = Some(("zooming…".into(), true, at + 10.0));
                        act("zoom", pane, None);
                    }
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        page = h.saturating_sub(6).max(1);
        let (board, absence, err, watcher_err, restarted_at) = match state.lock() {
            Ok(g) => (
                g.board.clone(),
                g.absence.clone(),
                g.err.clone(),
                g.watcher_err.clone(),
                g.restarted_at,
            ),
            Err(_) => return,
        };
        if let Some(answer) = acted.lock().ok().and_then(|mut slot| slot.take()) {
            note = Some(answer);
        }
        if note.as_ref().is_some_and(|(_, _, until)| at >= *until) {
            note = None;
        }

        // ── the explain screen ──
        if let Some(id) = explaining.clone() {
            let Some(pane) = board
                .as_ref()
                .and_then(|b| b.panes.iter().find(|p| p.block_id == id).cloned())
            else {
                explaining = None;
                note = Some(("that pane closed".into(), false, at + 3.0));
                continue;
            };
            let board = board.clone().unwrap_or_default();
            let mut rows = vec![tc::title_busy(
                "rex panes · explain",
                w,
                &p.accent,
                activity.tick(tick),
            )];
            let head = rows.len();
            let mut body: Vec<String> = Vec::new();
            for (kind, text) in explain_lines(&pane, &board, &server, at) {
                let colour = match kind.as_str() {
                    "lbl" => p.lbl.clone(),
                    "txt" => p.txt.clone(),
                    "" => String::new(),
                    "dim" => p.dim.clone(),
                    other => colour_of(other, &p).to_string(),
                };
                // Wrapped only when it does not fit: `wrap_words` collapses
                // runs of spaces, and the field names here are aligned by them.
                let lines = if tc::display_width(&text) < w.saturating_sub(1) {
                    vec![text]
                } else {
                    tc::wrap_words(&text, w.saturating_sub(4))
                        .into_iter()
                        .enumerate()
                        .map(|(i, l)| {
                            if i == 0 {
                                format!(" {}", l)
                            } else {
                                format!("   {}", l)
                            }
                        })
                        .collect()
                };
                for line in lines {
                    body.push(tc::seg(&[(colour.as_str(), line)], w.saturating_sub(1)));
                }
            }
            body.push(String::new());
            body.push(tc::seg(
                &[(p.lbl.as_str(), " ── WHAT THE PANE SHOWS ──".into())],
                w - 1,
            ));
            let read = capture.lock().ok().and_then(|slot| slot.clone());
            match read {
                _ if pane.creator != TERMINAL => body.push(tc::seg(
                    &[(
                        p.dim.as_str(),
                        " not a terminal block: there is no screen to read".into(),
                    )],
                    w - 1,
                )),
                Some((block, Ok(lines))) if block == id => {
                    if lines.is_empty() {
                        body.push(tc::seg(
                            &[(p.dim.as_str(), " the screen is blank".into())],
                            w - 1,
                        ));
                    }
                    for line in lines {
                        // Plain text, already stripped of the program's own
                        // escapes by `capture`; clipped rather than wrapped,
                        // because it is a picture of a screen.
                        body.push(tc::seg(&[(p.proc.as_str(), format!(" {}", line))], w - 1));
                    }
                }
                Some((block, Err(why))) if block == id => body.push(tc::seg(
                    &[(
                        p.blocked.as_str(),
                        format!(" ⚠ could not read the screen: {}", why),
                    )],
                    w - 1,
                )),
                _ => body.push(tc::seg(&[(p.dim.as_str(), " reading…".into())], w - 1)),
            }
            let hints: Vec<Vec<(&str, String)>> = vec![
                vec![
                    (p.accent.as_str(), "↵".into()),
                    (p.dim.as_str(), " focus".into()),
                ],
                vec![(
                    p.dim.as_str(),
                    if pane.zoomed {
                        "[z] unzoom".into()
                    } else {
                        "[z]oom".into()
                    },
                )],
                vec![(p.dim.as_str(), "[e] back".into())],
                vec![(p.dim.as_str(), "esc back".into())],
                vec![(p.dim.as_str(), "[q]uit".into())],
            ];
            let packed = tc::pack_hints_placed(&hints, w.saturating_sub(2), "  ");
            let footer: Vec<String> = packed.lines.iter().map(|l| format!(" {}", l)).collect();
            let room = h.saturating_sub(head + footer.len() + 1).max(1);
            explain_scroll = explain_scroll.min(body.len().saturating_sub(room));
            rows.extend(body.iter().skip(explain_scroll).take(room).cloned());
            while rows.len() < h.saturating_sub(footer.len() + 1) {
                rows.push(String::new());
            }
            rows.push(note_row(&note, &p, w));
            let foot_top = rows.len();
            rows.extend(footer);
            placed.clear();
            tc::draw(&rows, w, h);
            keyboard.footer_at(&packed, foot_top, 1);
            keyboard.wait(Duration::from_millis(250));
            continue;
        }

        // ── the board ──
        let mut rows = vec![tc::title_busy(
            "rex panes",
            w,
            &p.accent,
            activity.tick(tick),
        )];
        let mut plots = Vec::new();
        let mut all = board.as_ref().map(|b| b.panes.clone()).unwrap_or_default();
        let hidden_opscope = all
            .iter()
            .filter(|n| !opscope_filter.visible(n.is_opscope))
            .count();
        let total = all.len();
        all.retain(|n| opscope_filter.visible(n.is_opscope));
        let agents: Vec<&Pane> = all
            .iter()
            .filter(|n| n.section() == Section::Agents)
            .collect();
        let busy: Vec<&Pane> = all
            .iter()
            .filter(|n| n.section() == Section::Processes)
            .collect();
        let resting: Vec<&Pane> = all
            .iter()
            .filter(|n| n.section() == Section::Idle)
            .collect();
        // Grouped by session inside each section, when there is more than
        // one: the flat index the keys walk is this order, so the cursor and
        // the sub-headings cannot disagree.
        let agents = by_session(agents);
        let busy = by_session(busy);
        let resting = by_session(resting);
        let before = std::mem::take(&mut panes_now);
        panes_now = agents
            .iter()
            .chain(busy.iter())
            .chain(resting.iter().filter(|_| show_idle))
            .map(|n| (*n).clone())
            .collect();
        selected = reselect(&before, selected, &panes_now);
        if !panes_now.is_empty() && selected >= panes_now.len() {
            selected = panes_now.len() - 1;
        }

        if let Some(b) = &board {
            let mut counts: HashMap<String, usize> = HashMap::new();
            for a in &agents {
                *counts.entry(a.state()).or_insert(0) += 1;
            }
            let mut summary: Vec<(&str, String)> = vec![
                (
                    p.dim.as_str(),
                    format!("{} agent{} ·", agents.len(), plural(agents.len())),
                ),
                (
                    p.dim.as_str(),
                    // With one session there are no sub-headings to name it,
                    // so the count line does.
                    match (
                        b.sessions,
                        agents.first().or(busy.first()).or(resting.first()),
                    ) {
                        (1, Some(pane)) => format!("session {} ·", pane.session_label),
                        (n, _) => format!("{} session{} ·", n, plural(n)),
                    },
                ),
                (p.dim.as_str(), format!("{} pane{}", total, plural(total))),
            ];
            for name in ["blocked", "error", "done", "working", "idle", "unknown"] {
                if let Some(n) = counts.get(name) {
                    summary.push((colour_of(name, &p), format!("  {} {}", n, name)));
                }
            }
            rows.extend(flow(&summary, w));
            // The state mix as one bar, herdr's: drawn as text, and laid
            // over with a graphic where the terminal can show one. Only
            // while the header is pinned - a plot is placed by frame row,
            // and a header that scrolls has no fixed row to place it on.
            if !agents.is_empty() {
                let mut states: Vec<_> = counts.iter().collect();
                states.sort_by_key(|(state, _)| rank_of(state));
                let parts: Vec<(f64, String)> = states
                    .into_iter()
                    .map(|(state, n)| {
                        (
                            *n as f64 / agents.len() as f64,
                            colour_of(state, &p).to_string(),
                        )
                    })
                    .collect();
                let width = w.saturating_sub(2);
                if h >= 20 {
                    plots.push(tc::Plot::stacked_bar(1, rows.len(), width, &parts));
                }
                let bar = tc::stacked_bar(&parts, width);
                let mut line = vec![(tc::RST, " ".to_string())];
                line.extend(bar.iter().map(|(c, t)| (c.as_str(), t.clone())));
                rows.push(tc::seg(&line, w.saturating_sub(1)));
            }
            let waiting = all.iter().filter(|n| n.wants_a_human()).count();
            if waiting > 0 {
                let blocked = all.iter().any(|n| {
                    n.records
                        .iter()
                        .any(|r| matches!(r.state.as_str(), "blocked" | "error"))
                });
                let colour = if blocked {
                    p.blocked.as_str()
                } else {
                    p.done.as_str()
                };
                rows.extend(flow(
                    &[(
                        colour,
                        format!("▸ {} pane{} waiting for you", waiting, plural(waiting)),
                    )],
                    w,
                ));
            } else {
                rows.extend(flow(
                    &[(p.dim.as_str(), "nothing waiting on you".into())],
                    w,
                ));
            }
        }
        if !err.is_empty() {
            rows.extend(flow(&[(p.blocked.as_str(), format!("! {}", err))], w));
        }
        if !watcher_err.is_empty() {
            rows.extend(flow(
                &[(p.blocked.as_str(), format!("! events: {}", watcher_err))],
                w,
            ));
        }
        if let Some(when) = restarted_at.filter(|when| at - when < 60.0) {
            rows.extend(flow(
                &[(
                    p.dim.as_str(),
                    format!("the server restarted {} ago", ago(at - when)),
                )],
                w,
            ));
        }
        let mut filters = Vec::new();
        if !show_idle && !resting.is_empty() {
            filters.push(format!(
                "{} idle pane{} hidden",
                resting.len(),
                plural(resting.len())
            ));
        }
        filters.extend(opscope_filter.description(hidden_opscope));
        if let Some(said) = tc::filter_row(panes_now.len(), total, &filters) {
            rows.extend(flow(&[(p.dim.as_str(), said)], w));
        }
        // The header is pinned when the pane has room for it and for a body
        // under it. On a short pane only the title stays, and the counts
        // scroll with everything else - a header that leaves one row of body
        // is a pane that shows its own summary and none of what it sums.
        head_rows = if h >= 20 { rows.len() } else { 1 };
        rows.push(String::new());

        let mut rows_at: Vec<(usize, usize)> = Vec::new();
        let mut cursor_span: Option<(usize, usize)> = None;
        sections.clear();

        match (&board, &absence) {
            (None, Some(absence)) => {
                let lines: Vec<(&str, String)> = match absence {
                    Absence::NoServer => vec![
                        (p.blocked.as_str(), "⚠ no Rex server".into()),
                        (p.dim.as_str(), "nothing is listening on this machine's Rex socket, and this widget never starts a server itself".into()),
                        (p.dim.as_str(), "start Rex, or name another server with ,".into()),
                    ],
                    Absence::Unreachable => vec![
                        (p.blocked.as_str(), format!("⚠ the {} did not answer", server_words(&server))),
                        (p.dim.as_str(), "check the name with rex hosts, or change it with ,".into()),
                    ],
                    Absence::Gone => vec![(p.blocked.as_str(), "⚠ a session went away during the read".into())],
                    Absence::Other(why) => vec![(p.blocked.as_str(), format!("⚠ rex do failed: {}", why))],
                };
                for (colour, text) in lines {
                    for line in tc::wrap_words(&text, w.saturating_sub(3)) {
                        rows.push(tc::seg(&[(colour, format!("  {}", line))], w - 1));
                    }
                }
            }
            (None, None) => {
                rows.push(tc::seg(
                    &[(p.dim.as_str(), "  reading the Rex server…".into())],
                    w - 1,
                ));
            }
            (Some(b), _) => {
                for (label, why) in &b.unread_sessions {
                    for line in tc::wrap_words(
                        &format!("⚠ session {} could not be read: {}", label, why),
                        w.saturating_sub(3),
                    ) {
                        rows.push(tc::seg(
                            &[(p.unknown.as_str(), format!(" {}", line))],
                            w - 1,
                        ));
                    }
                }
                let listed: Vec<&Pane> = panes_now.iter().collect();
                let cols = Columns::fit(w, &listed);
                let mut index = 0usize;
                // A session's sub-heading goes above its first pane in each
                // section, and only when the board holds more than one
                // session: with one, the count line has already named it.
                // Decided from the section's own list, so each section
                // starts its own run of sub-headings.
                let mut emit = |rows: &mut Vec<String>, pane: &Pane, index: usize, of: &[&Pane]| {
                    let first = of.iter().position(|o| o.session_id == pane.session_id)
                        == of.iter().position(|o| o.block_id == pane.block_id);
                    if cols.sessions && first {
                        let n = of
                            .iter()
                            .filter(|o| o.session_id == pane.session_id)
                            .count();
                        rows.extend(flow(
                            &[
                                (p.accent.as_str(), format!("  ╶ {}", pane.session_label)),
                                (p.dim.as_str(), format!("· {}", n)),
                            ],
                            w,
                        ));
                    }
                    let from = rows.len();
                    rows.extend(render_pane(pane, index == selected, w, &cols, &p, tick, at));
                    rows_at.extend((from..rows.len()).map(|row| (row, index)));
                    if index == selected {
                        cursor_span = Some((from, rows.len()));
                    }
                };

                rows.extend(flow(
                    &[
                        (p.lbl.as_str(), "── AGENTS ──".into()),
                        (p.dim.as_str(), format!("{}", agents.len())),
                    ],
                    w,
                ));
                if agents.is_empty() {
                    rows.push(tc::seg(
                        &[(p.dim.as_str(), "   no coding agent in any pane".into())],
                        w - 1,
                    ));
                } else {
                    sections.push(index);
                    // AGENT sits over the mark as well as the name, hence
                    // the two; MEM and SESSION only once there is room.
                    let mut head = format!(
                        " {:<nw$} {:<8} {:<6}{}",
                        "AGENT",
                        "STATE",
                        "FOR",
                        if cols.cpu { " CPU  " } else { "" },
                        nw = cols.name + 2
                    );
                    if cols.wide {
                        head += &format!(" {:<5} {}", "MEM", "WINDOW");
                    }
                    rows.push(tc::seg(&[(p.dim.as_str(), tc::pad(&head, w - 1))], w - 1));
                    for pane in &agents {
                        emit(&mut rows, pane, index, &agents);
                        index += 1;
                    }
                }

                rows.push(String::new());
                let unread = busy
                    .iter()
                    .filter(|n| n.doing == Doing::Unknown && n.creator == TERMINAL)
                    .count();
                let mut heading = vec![
                    (p.lbl.as_str(), "── PROCESSES ──".into()),
                    (
                        p.dim.as_str(),
                        // The same count in fewer words when the pane is
                        // narrow, so the heading stays one line: a heading
                        // whose count wraps under it reads as two headings.
                        if w < 50 {
                            format!("{} running", busy.len() - unread)
                        } else {
                            format!(
                                "{} pane{} running something",
                                busy.len() - unread,
                                plural(busy.len() - unread)
                            )
                        },
                    ),
                ];
                if unread > 0 {
                    heading.push((
                        p.unknown.as_str(),
                        if w < 50 {
                            format!("· {} unread", unread)
                        } else {
                            format!("· {} could not be read", unread)
                        },
                    ));
                }
                if !b.local_usage {
                    heading.push((
                        p.dim.as_str(),
                        " · cpu/mem: the server is not on this machine".into(),
                    ));
                }
                rows.extend(flow(&heading, w));
                if busy.is_empty() {
                    rows.push(tc::seg(
                        &[(
                            p.dim.as_str(),
                            format!(
                                "   {}",
                                process_filter::empty_message(
                                    hidden_opscope,
                                    resting.len(),
                                    "every other pane is idle at a prompt"
                                )
                            ),
                        )],
                        w - 1,
                    ));
                } else {
                    sections.push(index);
                    if cols.wide {
                        let head = format!(
                            " {:<cw$} {}{:<5} {:<5} {}",
                            "COMMAND",
                            if cols.states {
                                format!("{:<9}", "STATE")
                            } else {
                                String::new()
                            },
                            "CPU",
                            "MEM",
                            "WINDOW",
                            cw = cols.command + 2
                        );
                        rows.push(tc::seg(&[(p.dim.as_str(), tc::pad(&head, w - 1))], w - 1));
                    }
                    for pane in &busy {
                        emit(&mut rows, pane, index, &busy);
                        index += 1;
                    }
                }

                if show_idle && !resting.is_empty() {
                    rows.push(String::new());
                    rows.extend(flow(
                        &[
                            (p.lbl.as_str(), "── IDLE ──".into()),
                            (
                                p.dim.as_str(),
                                if w < 50 {
                                    format!("{} at a prompt", resting.len())
                                } else {
                                    format!(
                                        "{} pane{} at a prompt",
                                        resting.len(),
                                        plural(resting.len())
                                    )
                                },
                            ),
                        ],
                        w,
                    ));
                    sections.push(index);
                    for pane in &resting {
                        emit(&mut rows, pane, index, &resting);
                        index += 1;
                    }
                }
            }
        }

        // The footer is built before the window, because it wraps and the
        // body's room depends on how many rows it took.
        let zoomed = current.as_ref().is_some_and(|c| c.zoomed);
        // herdr-panes' order, so the two footers read alike side by side;
        // zoom and explain are Rex's own and sit after the shared ones.
        let mut hints: Vec<Vec<(&str, String)>> = vec![
            vec![(
                p.dim.as_str(),
                format!(
                    "[o]{} Opscope",
                    if opscope_filter.show { "hide" } else { "show" }
                ),
            )],
            vec![
                (p.accent.as_str(), "↑↓".into()),
                (p.dim.as_str(), " select".into()),
            ],
            vec![
                (p.accent.as_str(), "↵".into()),
                (p.dim.as_str(), " switch to this pane".into()),
            ],
            vec![
                (p.accent.as_str(), "tab".into()),
                (p.dim.as_str(), " section".into()),
            ],
            vec![(
                p.dim.as_str(),
                format!("[i]dle {}", if show_idle { "hide" } else { "show" }),
            )],
            vec![(
                p.dim.as_str(),
                if zoomed {
                    "[z] unzoom".into()
                } else {
                    "[z]oom".into()
                },
            )],
            vec![(p.dim.as_str(), "[e]xplain".into())],
            vec![(p.dim.as_str(), "[r]efresh".into())],
        ];
        hints.push(vec![(p.dim.as_str(), "[,] settings".into())]);
        hints.push(vec![(p.dim.as_str(), "[q]uit".into())]);
        let packed = tc::pack_hints_placed(&hints, w.saturating_sub(2), "  ");
        let footer: Vec<String> = packed.lines.iter().map(|l| format!(" {}", l)).collect();

        // A pane too short is a pane you scroll: the body is built whole
        // and shown through a window under the pinned header.
        let room = h.saturating_sub(head_rows + footer.len() + 1).max(1);
        let body_len = rows.len() - head_rows;
        if moved {
            if let Some((from, to)) = cursor_span {
                let (from, to) = (from - head_rows, to - head_rows);
                if from < scroll {
                    scroll = from;
                } else if to > scroll + room {
                    scroll = to.saturating_sub(room).min(from);
                }
            }
        }
        scroll = scroll.min(body_len.saturating_sub(room));
        moved = false;
        let mut frame: Vec<String> = rows[..head_rows].to_vec();
        frame.extend(rows[head_rows..].iter().skip(scroll).take(room).cloned());
        while frame.len() < h.saturating_sub(footer.len() + 1) {
            frame.push(String::new());
        }
        frame.push(note_row(&note, &p, w));
        let foot_top = frame.len();
        frame.extend(footer);
        // The placements index the vec the frame is windowed from, so a
        // click resolves through `item_at` with the same head and scroll.
        placed = rows_at;
        // The bar sits in the pinned header, so its frame row is its row.
        let plots: Vec<_> = plots
            .into_iter()
            .filter_map(|plot| plot.in_viewport(0, 0, head_rows))
            .collect();
        tc::draw_plots(&frame, w, h, &plots);
        keyboard.footer_at(&packed, foot_top, 1);
        keyboard.wait(Duration::from_millis(250));
    }
}

/// Coloured phrases packed onto as many rows as they need, a space apart.
///
/// A phrase is never split across rows unless it alone is wider than the
/// pane, and then it is wrapped at its own spaces. Clipping a header line
/// at the pane edge is the cut this collection does not make: `5 pane` and
/// `panes runn` are numbers and words that are not on the server.
fn flow(parts: &[(&str, String)], w: usize) -> Vec<String> {
    let room = w.saturating_sub(2).max(1);
    let mut rows = Vec::new();
    let mut line: Vec<(&str, String)> = Vec::new();
    let mut used = 0usize;
    let push = |line: &mut Vec<(&str, String)>, used: &mut usize, rows: &mut Vec<String>| {
        if !line.is_empty() {
            let mut out = vec![("", " ".to_string())];
            out.append(line);
            rows.push(tc::seg(&out, w.saturating_sub(1)));
        }
        *used = 0;
    };
    for (colour, text) in parts {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let wide = tc::display_width(text);
        let gap = usize::from(used > 0);
        if used + gap + wide > room {
            push(&mut line, &mut used, &mut rows);
        }
        if wide > room {
            for piece in tc::wrap_words(text, room) {
                rows.push(tc::seg(
                    &[("", " ".to_string()), (colour, piece)],
                    w.saturating_sub(1),
                ));
            }
            continue;
        }
        let gap = usize::from(used > 0);
        line.push((
            colour,
            format!("{}{}", if gap == 1 { " " } else { "" }, text),
        ));
        used += gap + wide;
    }
    push(&mut line, &mut used, &mut rows);
    rows
}

fn note_row(note: &Option<(String, bool, f64)>, p: &Palette, w: usize) -> String {
    match note {
        Some((text, ok, _)) => tc::seg(
            &[(
                if *ok {
                    p.done.as_str()
                } else {
                    p.blocked.as_str()
                },
                format!(" {}", text),
            )],
            w.saturating_sub(1),
        ),
        None => String::new(),
    }
}

/// Bring a pane to the front, on the server, and say what that did.
///
/// Focus is session state in Rex - the window becomes active and the block
/// focused - so it works from a headless server with no app attached. What
/// it cannot do is make an app *show* that session; that is a client action
/// behind Remote Control, and this widget does not attempt it. So when no
/// app is attached to the session, the note says so rather than claiming
/// something moved on a screen.
fn focus(server: &str, pane: &Pane, board: Option<&Board>, at: f64) -> (String, bool, f64) {
    if pane.detached {
        return (
            "detached · rex cannot focus a block that is in no window".into(),
            false,
            at + 4.0,
        );
    }
    let what = if pane.label.is_empty() {
        pane.short_id()
    } else {
        pane.label.clone()
    };
    match rex(
        server,
        &["block", "focus", "-s", &pane.session_id, &pane.block_id],
    ) {
        Ok(_) => {
            let shown = board.is_some_and(|b| b.app_sessions.contains(&pane.session_id));
            (
                if shown {
                    format!("focused {} in {}", what, pane.place())
                } else {
                    format!(
                        "focused on the server · no app is showing session {} - pick it in Rex",
                        pane.session_label
                    )
                },
                true,
                at + 4.0,
            )
        }
        Err(Absence::Gone) => ("that pane closed · refreshing".into(), false, at + 3.0),
        Err(Absence::NoServer) => ("no Rex server is running".into(), false, at + 3.0),
        Err(Absence::Unreachable) => ("the server did not answer".into(), false, at + 3.0),
        Err(Absence::Other(why)) => (format!("! could not focus: {}", why), false, at + 4.0),
    }
}

/// Zoom a pane to fill its window, or undo it. Rex zooms by focusing too.
fn zoom(server: &str, pane: &Pane, at: f64) -> (String, bool, f64) {
    if pane.detached {
        return (
            "detached · a block in no window has nothing to fill".into(),
            false,
            at + 4.0,
        );
    }
    let flag = if pane.zoomed { "--off" } else { "--on" };
    match rex(
        server,
        &["zoom", "-s", &pane.session_id, "-b", &pane.block_id, flag],
    ) {
        Ok(_) => (
            if pane.zoomed {
                format!("unzoomed {}", pane.place())
            } else {
                format!("zoomed {}", pane.place())
            },
            true,
            at + 3.0,
        ),
        Err(Absence::Gone) => ("that pane closed · refreshing".into(), false, at + 3.0),
        Err(Absence::NoServer) => ("no Rex server is running".into(), false, at + 3.0),
        Err(Absence::Unreachable) => ("the server did not answer".into(), false, at + 3.0),
        Err(Absence::Other(why)) => (format!("! could not zoom: {}", why), false, at + 4.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GATHER: &str = include_str!("fixtures/gather.json");
    const GATHER_RECORDS: &str = include_str!("fixtures/gather_records.json");

    fn catalogue() -> Vec<String> {
        DEFAULT_AGENTS.iter().map(|s| s.to_string()).collect()
    }

    fn board(text: &str) -> Board {
        let g = parse::parse_gather(text).unwrap();
        let mut seen = Seen {
            first: true,
            ..Default::default()
        };
        let mut b = build(&g, &catalogue(), &mut seen, true, 2.0e9);
        order(&mut b.panes);
        b
    }

    #[test]
    fn the_live_server_sorts_into_three_sections() {
        let b = board(GATHER);
        let agents: Vec<&Pane> = b
            .panes
            .iter()
            .filter(|p| p.section() == Section::Agents)
            .collect();
        assert_eq!(
            agents.len(),
            1,
            "the claude pane is an agent by its process"
        );
        assert_eq!(agents[0].agent.as_ref().unwrap().1, Source::Process);
        // It reported nothing, so its state is unknown - never idle.
        assert_eq!(agents[0].state(), "unknown");
        let opscope = b.panes.iter().filter(|p| p.is_opscope).count();
        assert_eq!(opscope, 4, "four panes run npm exec opscope@latest");
        assert!(b.panes.iter().all(|p| p.doing != Doing::Unknown));
        assert!(b.apps >= 1, "the fixture has an app attached");
    }

    #[test]
    fn a_blocked_report_outranks_a_finished_agent_and_its_children_are_kept() {
        let b = board(GATHER_RECORDS);
        let first = &b.panes[0];
        assert_eq!(first.state(), "blocked");
        assert_eq!(
            first.section(),
            Section::Processes,
            "terraform is not an agent"
        );
        assert_eq!(first.children().count(), 1);
        let claude = b
            .panes
            .iter()
            .find(|p| p.agent.is_some())
            .expect("the claude REPL is an agent");
        assert_eq!(claude.state(), "done");
        assert_eq!(claude.agent.as_ref().unwrap().1, Source::Report);
        assert!(claude.wants_a_human());
    }

    fn pane_in(session: &str, ord: usize, block: &str, state: Option<&str>) -> Pane {
        Pane {
            session_id: format!("session:{}", session),
            session_label: session.into(),
            session_ord: ord,
            block_id: format!("block:{}", block),
            records: state
                .map(|s| {
                    vec![Record {
                        state: s.into(),
                        ..Default::default()
                    }]
                })
                .unwrap_or_default(),
            ..Default::default()
        }
    }

    #[test]
    fn a_session_with_a_blocked_pane_comes_first_and_stays_together() {
        let quiet_a = pane_in("alpha", 1, "a1", Some("idle"));
        let blocked = pane_in("beta", 2, "b1", Some("blocked"));
        let quiet_b = pane_in("beta", 2, "b2", Some("working"));
        let quiet_c = pane_in("alpha", 1, "a2", None);
        // Already worst-first, as `order` leaves them: blocked, working,
        // idle, unknown - with the two sessions interleaved.
        let grouped = by_session(vec![&blocked, &quiet_b, &quiet_a, &quiet_c]);
        let ids: Vec<&str> = grouped.iter().map(|p| p.block_id.as_str()).collect();
        assert_eq!(ids, ["block:b1", "block:b2", "block:a1", "block:a2"]);
        // Two quiet sessions keep Rex's own order.
        let x = pane_in("x", 2, "x1", Some("idle"));
        let y = pane_in("y", 1, "y1", Some("idle"));
        let grouped = by_session(vec![&x, &y]);
        assert_eq!(grouped[0].session_label, "y");
    }

    #[test]
    fn the_column_says_the_window_once_the_heading_says_the_session() {
        let mut pane = pane_in("work", 1, "w1", None);
        pane.window_ord = Some(2);
        assert_eq!(pane.place(), "work:2");
        assert_eq!(pane.window(), "2");
        pane.floating = Some(true);
        assert_eq!(pane.window(), "2·float·min");
        pane.detached = true;
        assert_eq!(pane.window(), "detached");
    }

    #[test]
    fn rank_puts_people_first_and_unknown_last() {
        for pair in RANK.windows(2) {
            assert!(rank_of(pair[0]) < rank_of(pair[1]));
        }
        assert!(rank_of("anything else") > rank_of("unknown"));
    }

    #[test]
    fn a_record_from_a_program_no_longer_in_front_says_so() {
        let mut pane = Pane {
            child_pid: Some(10),
            fg: Some(Foreground {
                pid: 10,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mine = Record {
            owner_pid: Some(10),
            state: "done".into(),
            ..Default::default()
        };
        let gone = Record {
            owner_pid: Some(77),
            state: "done".into(),
            ..Default::default()
        };
        assert!(!pane.reporter_gone(&mine));
        assert!(pane.reporter_gone(&gone));
        pane.records = vec![gone];
        assert!(detail_phrases(&pane, 0.0)
            .iter()
            .any(|s| s.contains("not in foreground")));
    }

    #[test]
    fn every_row_fits_its_pane_at_every_width() {
        for fixture in [GATHER, GATHER_RECORDS] {
            let b = board(fixture);
            let p = palette();
            for w in 20..=140 {
                let listed: Vec<&Pane> = b.panes.iter().collect();
                let cols = Columns::fit(w, &listed);
                for (i, pane) in b.panes.iter().enumerate() {
                    for row in render_pane(pane, i == 0, w, &cols, &p, 0, 2.0e9) {
                        assert!(
                            tc::display_width(&row) < w,
                            "w={} row {:?} is {} wide",
                            w,
                            row,
                            tc::display_width(&row)
                        );
                    }
                }
            }
        }
    }

    fn unread_status(agent: bool) -> Pane {
        Pane {
            block_id: "b1".into(),
            session_id: "s1".into(),
            creator: TERMINAL.into(),
            doing: Doing::Prompt,
            status_unread: true,
            why: "program_status: timed out".into(),
            agent: agent.then(|| ("claude".to_string(), Source::Process)),
            ..Default::default()
        }
    }

    fn drawn(pane: &Pane, w: usize) -> String {
        let cols = Columns::fit(w, &[pane]);
        render_pane(pane, false, w, &cols, &palette(), 0, 2.0e9).join("\n")
    }

    #[test]
    fn a_status_that_could_not_be_read_is_said_and_never_resting() {
        let shell = unread_status(false);
        assert_eq!(
            shell.section(),
            Section::Processes,
            "a shell whose status read failed is not known to be idle"
        );
        assert!(drawn(&shell, 80).contains("program_status: timed out"));
        let agent = unread_status(true);
        let screen = drawn(&agent, 80);
        assert!(screen.contains("program_status: timed out"), "{}", screen);
        assert!(
            !screen.contains("no status reported"),
            "a failed read is not an empty answer"
        );
        let board = Board::default();
        assert!(explain_lines(&agent, &board, "", 2.0e9)
            .iter()
            .any(|(_, t)| t.contains("could not be read: program_status: timed out")));
    }

    #[test]
    fn a_bell_in_a_session_nobody_is_showing_stays_until_looked_at() {
        let mut pane = unread_status(true);
        pane.focused = true;
        let mut attention = HashMap::new();
        attention.insert("b1".to_string(), (1.0e9, "bell".to_string()));
        let mut panes = vec![pane];
        answer_bells(&mut attention, &mut panes, &[]);
        assert!(
            panes[0].attention.is_some(),
            "stored focus in an unattached session is not somebody looking"
        );
        let mut panes = vec![Pane {
            attention: None,
            ..panes[0].clone()
        }];
        answer_bells(&mut attention, &mut panes, &["s1".to_string()]);
        assert!(panes[0].attention.is_none());
        assert!(
            attention.is_empty(),
            "answered once a Rex app shows it focused"
        );
    }

    #[test]
    fn a_refresh_keeps_the_cursor_on_the_same_pane() {
        let named = |id: &str| Pane {
            block_id: id.into(),
            ..Default::default()
        };
        let before = vec![named("a"), named("b")];
        let after = vec![named("new"), named("a"), named("b")];
        assert_eq!(reselect(&before, 1, &after), 2, "still on b");
        let gone = vec![named("a")];
        assert_eq!(reselect(&before, 1, &gone), 1, "b went: left for the clamp");
    }

    #[test]
    fn a_narrow_agent_row_keeps_its_duration_and_its_directory() {
        let mut pane = unread_status(true);
        pane.status_unread = false;
        pane.why.clear();
        pane.seen_exact = true;
        pane.seen_since = 2.0e9 - 120.0;
        pane.child_cwd = "/srv/a-rather-long/path/that-ends/in-the-part-that-matters".into();
        for w in 20..=30 {
            let screen = drawn(&pane, w);
            assert!(screen.contains("2m"), "w={} lost FOR:\n{}", w, screen);
            // Escapes out, then the rows joined back into the text they were
            // wrapped from: a hard-wrapped path breaks mid-word.
            let mut plain = String::new();
            let mut esc = false;
            for ch in screen.chars() {
                match ch {
                    '\x1b' => esc = true,
                    'm' if esc => esc = false,
                    _ if esc => {}
                    _ => plain.push(ch),
                }
            }
            let flat: String = plain.split('\n').map(str::trim).collect();
            assert!(
                flat.contains("matters"),
                "w={} cut the directory's tail:\n{}",
                w,
                screen
            );
        }
    }

    #[test]
    fn home_is_shortened_only_at_a_directory_boundary() {
        assert_eq!(tilde("/home/al/src", "/home/al"), "~/src");
        assert_eq!(tilde("/home/al", "/home/al"), "~");
        assert_eq!(tilde("/home/alice/src", "/home/al"), "/home/alice/src");
    }

    #[test]
    fn an_unread_cpu_is_as_wide_as_a_read_one() {
        assert_eq!(
            tc::display_width(&percent(None)),
            tc::display_width(&percent(Some(12.0)))
        );
    }

    #[test]
    fn the_embedded_read_only_script_is_the_one_shipped() {
        assert!(GATHER_LUA.contains("session.list"));
        assert!(GATHER_LUA.contains("program_status"));
    }

    #[test]
    fn only_the_local_socket_has_pids_of_ours() {
        assert!(!talks_to_local_server("desk"));
    }
}
