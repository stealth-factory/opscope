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

//! Everything running in Herdr, across every workspace.
//!
//! A port of herdr-panes.py. A Herdr client rather than a general agent
//! monitor: the inventory and the lifecycle states come from the `herdr`
//! CLI, and any agent kind it recognises appears here with no change to
//! this file.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use toys_core as tc;

/// Worst first: the states that want a human are the reason to look.
const RANK: &[&str] = &["blocked", "done", "working", "idle", "unknown"];
const SPINNER: &[char] = &['◐', '◓', '◑', '◒'];

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn rank_of(state: &str) -> usize {
    RANK.iter().position(|s| *s == state).unwrap_or(9)
}

/// Seconds before a herdr command is given up on, from herdr-panes.py.
const RUN_TIMEOUT: u64 = 15;

/// Run a herdr command for its effect; true when it succeeded.
///
/// Bounded, because the socket on the other end can stop answering and
/// .output() would wait for it forever with the pane still drawing.
fn herdr_action(args: &[&str]) -> bool {
    let mut argv = vec!["herdr"];
    argv.extend_from_slice(args);
    tc::run(&argv, RUN_TIMEOUT).is_ok()
}

/// The `result` object out of one herdr answer, or why there is none.
///
/// Split from the running of the command, because the running is not where
/// the failure shows: herdr answers a request it cannot serve with an
/// `error` object and exit status 0, so a pane that does not exist and a
/// pane with nothing to say arrive as two commands that both succeeded.
/// They are told apart by shape here or they are not told apart at all.
fn result_of(text: &str) -> Result<serde_json::Value, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("unreadable answer: {}", e))?;
    if let Some(said) = parsed.get("error") {
        let message = said["message"].as_str().unwrap_or("").trim();
        return Err(if message.is_empty() {
            format!("herdr said {}", said)
        } else {
            message.to_string()
        });
    }
    match parsed.get("result") {
        Some(serde_json::Value::Null) | None => Err("herdr answered with no result".into()),
        Some(value) => Ok(value.clone()),
    }
}

/// Run a herdr command and hand back its `result`, or why there is none.
fn herdr_result(args: &[&str]) -> Result<serde_json::Value, String> {
    let mut argv = vec!["herdr"];
    argv.extend_from_slice(args);
    result_of(&tc::run(&argv, RUN_TIMEOUT)?)
}

/// The same, for the callers that have nothing to do with the reason.
fn herdr(args: &[&str]) -> Option<serde_json::Value> {
    herdr_result(args).ok()
}

/// Keep the end of a path, marking the cut so it does not read as a name.
fn tail_path(path: &str, n: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= n || n < 2 {
        return path.to_string();
    }
    format!("…{}", chars[chars.len() - (n - 1)..].iter().collect::<String>())
}

fn base_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Readable name for what a pane is running.
///
/// "python3" or "node" says nothing useful, so prefer the script they were
/// handed; otherwise fall back to the executable's own name.
fn command_label(argv: &[String], name: &str) -> String {
    const RUNNERS: &[&str] = &[
        "python", "python3", "node", "ruby", "perl", "bun", "deno", "sh", "bash", "zsh",
    ];
    let Some(first) = argv.first() else {
        return if name.is_empty() { "?".into() } else { name.into() };
    };
    let head = base_name(first);
    let stem = head.split('.').next().unwrap_or(&head);
    if RUNNERS.contains(&stem) && argv.len() > 1 {
        for token in &argv[1..] {
            if !token.starts_with('-') {
                return base_name(token);
            }
        }
    }
    head
}

/// (cpu ticks used, resident bytes) out of one /proc/<pid>/stat line.
///
/// Split from the read so it can be tested on a line rather than on a live
/// process. Its test used to re-implement this parse in the test body and
/// assert on the copy, which meant the shipped one was never run.
fn parse_proc_stat(text: &str) -> Option<(u64, u64)> {
    // The command is in brackets and may itself contain spaces and brackets,
    // so the split starts after the last one rather than at the second field.
    let rest = text.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    let rss: u64 = fields.get(21)?.parse().ok()?;
    Some((utime + stime, rss * 4096))
}

/// (cpu ticks used, resident bytes) for a pid.
fn proc_stats(pid: i32) -> Option<(u64, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    parse_proc_stat(&text)
}

fn clock_ticks() -> f64 {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz > 0 {
        hz as f64
    } else {
        100.0
    }
}

/// A recognised coding agent, and what its process is costing.
#[derive(Clone, Default)]
struct Agent {
    name: String,
    pane_id: String,
    workspace_id: String,
    state: String,
    title: String,
    cwd: String,
    since: f64,
    /// False when the state was already in place at the first poll, so the
    /// duration is only a lower bound and says so.
    exact: bool,
    cpu: Option<f64>,
    rss: Option<u64>,
}

/// The slice of a variable-height list that fits `room` rows and holds the
/// cursor.
///
/// The three lists here do not have one height between them: an agent takes
/// two rows, its second carrying the directory and the pane title, while a
/// process takes one. A window counted in *entries* therefore admits more
/// rows than the pane has, they are cut off the bottom, and the cursor goes
/// with them - which is the bug this is here to fix, arrived at the second
/// time rather than the first.
///
/// `from` is where the window sat last frame. It is honoured when it can be,
/// so the view holds still while the cursor moves inside it, and moves by as
/// little as it takes when the cursor would leave.
fn window_over(
    heights: &[usize],
    at: usize,
    room: usize,
    from: usize,
) -> std::ops::Range<usize> {
    let n = heights.len();
    if n == 0 || room == 0 {
        return 0..0;
    }
    let at = at.min(n - 1);
    // Never start below the cursor: reaching up moves the window to it.
    let mut first = from.min(at);
    loop {
        let (mut used, mut end) = (0usize, first);
        while end < n && used + heights[end] <= room {
            used += heights[end];
            end += 1;
        }
        // A row taller than the whole pane still gets drawn, or the list
        // would be empty and the cursor nowhere.
        if end == first {
            end = first + 1;
        }
        if at < end || first + 1 >= n {
            return first..end;
        }
        first += 1;
    }
}

/// What a section heading adds when the window does not hold all of it.
///
/// `first` is where the section starts in the flat list the three of them
/// make together, and `len` is how many rows it has. Nothing when the whole
/// section is on screen - a range on a section you can see all of is noise.
fn showing(window: &std::ops::Range<usize>, first: usize, len: usize) -> String {
    if len == 0 {
        return String::new();
    }
    let from = window.start.max(first);
    let to = window.end.min(first + len);
    if from <= first && to >= first + len {
        String::new()
    } else if to <= from {
        // Scrolled clean past it. Said out loud, because a heading with a
        // count and no rows under it otherwise reads as a section that has
        // failed to load rather than one you have scrolled away from.
        "  · none on screen".to_string()
    } else {
        format!("  · showing {}-{}", from - first + 1, to - first)
    }
}

/// What a pane with no agent in it is doing.
///
/// Three states rather than a bool, because the third one is real: a pane
/// whose probe failed is neither running nor resting, and filing it as
/// either says something true has been established when nothing has.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Doing {
    Running,
    Prompt,
    /// `pane process-info` did not answer. The default, because a pane
    /// nobody has managed to look at has told us nothing.
    #[default]
    Unknown,
}

/// Unreadable first, then what is running, then the prompts: the row that
/// wants looking at is the one where the widget cannot say.
fn doing_rank(doing: Doing) -> usize {
    match doing {
        Doing::Unknown => 0,
        Doing::Running => 1,
        Doing::Prompt => 2,
    }
}

/// A pane with no agent in it: running something, at a prompt, or unread.
#[derive(Clone, Default)]
struct Panel {
    pane_id: String,
    tab_id: String,
    workspace_id: String,
    command: String,
    cwd: String,
    doing: Doing,
    /// Why the probe failed, when it did. Carried rather than counted: a
    /// number of panes nobody could read is a smaller answer than "pane
    /// not found" or "herdr did not answer in 15s".
    why: String,
    cpu: Option<f64>,
    rss: Option<u64>,
}

#[derive(Default)]
struct State {
    agents: Vec<Agent>,
    panels: Vec<Panel>,
    labels: HashMap<String, String>,
    err: String,
}

/// What each pane's state was when we first saw it, so a duration can be
/// measured rather than guessed.
#[derive(Default)]
struct Seen {
    since: HashMap<String, (String, f64, bool)>,
    cpu: HashMap<i32, (u64, f64)>,
    first_poll: bool,
}

/// Fold a fresh /proc reading into the CPU history and return the percentage.
fn cpu_of(seen: &mut Seen, pid: i32, at: f64, hz: f64) -> (Option<f64>, Option<u64>) {
    let Some((ticks, rss)) = proc_stats(pid) else {
        return (None, None);
    };
    // A percentage needs two readings; the first visit only records one.
    let cpu = match seen.cpu.get(&pid) {
        Some((was, when)) if at - when > 0.0 => {
            Some((ticks.saturating_sub(*was)) as f64 / hz / (at - when) * 100.0)
        }
        _ => None,
    };
    seen.cpu.insert(pid, (ticks, at));
    (cpu, Some(rss))
}

fn text_at(value: &serde_json::Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_string()
}

/// What a pane's foreground process is, or why we could not tell.
///
/// Three answers rather than two, and that is the whole point of the type.
/// A pane at its shell prompt and a probe that failed used to arrive here
/// as the same `None`, and the caller wrote that down as idle - so a herdr
/// that had stopped answering turned a board full of working panes into a
/// board full of resting ones, with nothing on screen saying otherwise.
enum Front {
    /// Running something: its pid, argv, name and directory.
    Running(i32, Vec<String>, String, String),
    /// At its shell prompt - the foreground pid is the shell's own.
    Prompt,
    /// The probe did not answer, so which of the two this is nobody knows.
    Unknown(String),
}

/// What one `pane process-info` answer says the pane is doing.
///
/// Split from the request so all three answers can be tested on a value
/// rather than on a live Herdr, the way `parse_proc_stat` is.
fn classify(info: &serde_json::Value) -> Front {
    let process = &info["process_info"];
    let Some(front) = process["foreground_processes"]
        .as_array()
        .and_then(|a| a.first())
    else {
        return Front::Unknown("no foreground process reported".into());
    };
    let Some(pid) = front["pid"].as_i64() else {
        return Front::Unknown("foreground process has no pid".into());
    };
    // The shell's own pid in the foreground is the prompt being what is in
    // the foreground. An absent `shell_pid` is not that - it says nothing -
    // so it goes on as running rather than resting, which is the direction
    // that cannot repeat the bug this type exists for.
    if process["shell_pid"].as_i64() == Some(pid) {
        return Front::Prompt;
    }
    let argv = front["argv"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Front::Running(
        pid as i32,
        argv,
        text_at(front, "name"),
        text_at(front, "cwd"),
    )
}

/// The foreground process of a pane, the prompt, or the failed probe.
fn foreground(pane_id: &str) -> Front {
    match herdr_result(&["pane", "process-info", "--pane", pane_id]) {
        Ok(info) => classify(&info),
        Err(why) => Front::Unknown(why),
    }
}

fn poll(state: &Arc<Mutex<State>>, seen: &mut Seen, hz: f64) {
    let mut labels = HashMap::new();
    if let Some(res) = herdr(&["workspace", "list"]) {
        for w in res["workspaces"].as_array().into_iter().flatten() {
            labels.insert(text_at(w, "workspace_id"), text_at(w, "label"));
        }
    }
    let listed = match herdr(&["agent", "list"]) {
        Some(res) => res,
        None => {
            if let Ok(mut guard) = state.lock() {
                guard.err = "herdr CLI unavailable (is HERDR_ENV set?)".into();
            }
            return;
        }
    };

    let at = now();
    let mut agents = Vec::new();
    for entry in listed["agents"].as_array().into_iter().flatten() {
        let pane_id = text_at(entry, "pane_id");
        let state_name = match entry["agent_status"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => "unknown".to_string(),
        };
        let was = seen.since.get(&pane_id);
        if was.is_none_or(|(had, _, _)| *had != state_name) {
            // A state already in place when we started is only a lower
            // bound - we did not see it begin.
            seen.since
                .insert(pane_id.clone(), (state_name.clone(), at, !seen.first_poll));
        }
        let (_, began, exact) = seen.since[&pane_id].clone();
        let (cpu, rss) = match foreground(&pane_id) {
            Front::Running(pid, _, _, _) => cpu_of(seen, pid, at, hz),
            // An agent's state comes from `agent list`, not from the probe,
            // so a failed probe costs the row its CPU and memory and
            // nothing else - and those already draw as `-` and `--` when
            // there is no reading, which there is not.
            Front::Prompt | Front::Unknown(_) => (None, None),
        };
        agents.push(Agent {
            name: text_at(entry, "agent"),
            workspace_id: text_at(entry, "workspace_id"),
            title: text_at(entry, "terminal_title_stripped"),
            cwd: text_at(entry, "cwd"),
            state: state_name,
            since: at - began,
            exact,
            cpu,
            rss,
            pane_id,
        });
    }
    agents.sort_by(|a, b| {
        rank_of(&a.state)
            .cmp(&rank_of(&b.state))
            .then(b.since.total_cmp(&a.since))
    });

    let mut panels = Vec::new();
    if let Some(listing) = herdr(&["pane", "list"]) {
        for pane in listing["panes"].as_array().into_iter().flatten() {
            if pane.get("agent").is_some_and(|a| !a.is_null()) {
                continue;
            }
            let pane_id = text_at(pane, "pane_id");
            let front = foreground(&pane_id);
            let (cpu, rss) = match &front {
                Front::Running(pid, _, _, _) => cpu_of(seen, *pid, at, hz),
                Front::Prompt | Front::Unknown(_) => (None, None),
            };
            let (doing, command, cwd, why) = match front {
                Front::Running(_, argv, name, cwd) => (
                    Doing::Running,
                    command_label(&argv, &name),
                    if cwd.is_empty() { text_at(pane, "cwd") } else { cwd },
                    String::new(),
                ),
                Front::Prompt => (
                    Doing::Prompt,
                    String::new(),
                    text_at(pane, "cwd"),
                    String::new(),
                ),
                // The pane's own directory still comes from `pane list`, so
                // an unread pane is not a blank row - it is a row that says
                // where it is and that nobody could see into it.
                Front::Unknown(why) => (
                    Doing::Unknown,
                    String::new(),
                    text_at(pane, "cwd"),
                    why,
                ),
            };
            panels.push(Panel {
                tab_id: text_at(pane, "tab_id"),
                workspace_id: text_at(pane, "workspace_id"),
                doing,
                why,
                pane_id,
                command,
                cwd,
                cpu,
                rss,
            });
        }
    }
    // Unread first, then busy, and the busiest of those first: the point of
    // the section is what is costing something, and above that, what the
    // widget could not find out at all.
    // Idle last, and that ordering is load-bearing twice over. The screen
    // draws `busy` then `resting`, and both the cursor and the window are
    // indices into `agents ++ panels` - so the two agree only because
    // `panels` is already unread-then-running-then-resting. Reorder this and
    // the cursor silently marks one pane while enter switches to another.
    panels.sort_by(|a, b| {
        doing_rank(a.doing)
            .cmp(&doing_rank(b.doing))
            .then(b.cpu.unwrap_or(0.0).total_cmp(&a.cpu.unwrap_or(0.0)))
    });

    if let Ok(mut guard) = state.lock() {
        guard.agents = agents;
        guard.panels = panels;
        guard.labels = labels;
        guard.err.clear();
    }
    seen.first_poll = false;
}

/// A duration as this widget says it, which is not how the others do.
///
/// Between an hour and a day it carries the minutes too: an agent blocked
/// for "3h" and one blocked for "3h58m" are the same number of hours and a
/// very different amount of ignoring.
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

/// Resident memory in five cells, whatever there is to say.
///
/// herdr-panes.py returns five characters for a reading and six for an
/// absent one, so a process /proc would not name pushes the workspace
/// column one cell right of every other row. The header allots five.
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
        None => "   -".to_string(),
    }
}

/// Where a row points, so Enter knows what to focus.
#[derive(Clone)]
enum Row {
    Agent(Agent),
    Process(Panel),
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
        "blocked" => &p.blocked,
        "done" => &p.done,
        "working" => &p.working,
        "idle" => &p.idle,
        _ => &p.unknown,
    }
}

fn mark_of(state: &str, tick: usize) -> char {
    match state {
        "blocked" => '⚠',
        "done" => '✓',
        "working" => SPINNER[tick % SPINNER.len()],
        "idle" => '·',
        _ => '?',
    }
}

/// The home-relative form of a directory, which is how a person names it.
fn homely(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        return path.to_string();
    }
    let projects = format!("{}/projects/", home);
    if let Some(rest) = path.strip_prefix(&projects) {
        return rest.to_string();
    }
    match path.strip_prefix(&home) {
        Some(rest) => format!("~{}", rest),
        None => path.to_string(),
    }
}

fn main() {
    tc::maybe_help(include_str!("herdr-panes_help.txt"));
    // The section is spelled with an underscore while everything else about
    // this widget is hyphenated. A mismatched key is read as absent rather
    // than as an error, so it is worth saying out loud.
    let cfg = tc::load_config("herdr_panes");
    let mut refresh = tc::cfg_f64(&cfg, "refresh", 4.0);
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() >= 2 && (args[0] == "-n" || args[0] == "--refresh") {
        refresh = args[1].parse::<f64>().unwrap_or(4.0).max(1.0);
    }

    let absent = tc::missing(&["herdr"]);
    if !absent.is_empty() {
        tc::cannot_start(
            "herdr panes",
            &absent,
            &[
                "This reads a running Herdr session through its own CLI: the",
                "workspaces, the panes in them, and which agent is in which.",
                "There is no other source for any of it.",
                "",
                "If Herdr is installed but not on PATH, this widget will find",
                "it as soon as the shell can.",
            ],
            "see https://herdr.dev",
        );
        return;
    }

    let p = palette();
    let state = Arc::new(Mutex::new(State::default()));
    let wake = Arc::new((Mutex::new(false), Condvar::new()));
    let poller = Arc::clone(&state);
    let poller_wake = Arc::clone(&wake);
    std::thread::spawn(move || {
        let hz = clock_ticks();
        let mut seen = Seen {
            first_poll: true,
            ..Default::default()
        };
        loop {
            // A poller that dies takes its explanation with it, and an empty
            // board looks exactly like a Herdr with nothing running in it.
            let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                poll(&poller, &mut seen, hz)
            }));
            if step.is_err() {
                if let Ok(mut guard) = poller.lock() {
                    guard.err = "poller stopped - see the pane it was started from".into();
                }
                return;
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
    let (mut show_labels, mut show_idle) = (true, true);
    let (mut selected, mut tick) = (0usize, 0usize);
    // How far down the three lists, read as one, the window has scrolled.
    // Kept across frames so the view holds still while the cursor moves
    // inside it, and only moves when the cursor would leave it.
    let mut scroll = 0usize;
    let mut note: Option<(String, bool, f64)> = None;
    let mut rows_now: Vec<Row> = Vec::new();

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
                "l" | "L" => show_labels = !show_labels,
                "i" | "I" => {
                    show_idle = !show_idle;
                    selected = 0;
                }
                "up" => selected = selected.saturating_sub(1),
                "down" => selected += 1,
                "home" => selected = 0,
                "end" => selected = rows_now.len().saturating_sub(1),
                "enter" | "f" | "F" => {
                    if let Some(row) = rows_now.get(selected.min(rows_now.len().saturating_sub(1)))
                    {
                        let (ok, what, pane) = match row {
                            Row::Agent(a) => (
                                herdr_action(&["agent", "focus", &a.pane_id]),
                                a.name.clone(),
                                a.pane_id.clone(),
                            ),
                            // A pane has no focus-by-id, but a tab tiles its
                            // panes, so focusing the tab brings it into view.
                            Row::Process(n) => (
                                herdr_action(&["tab", "focus", &n.tab_id]),
                                n.command.clone(),
                                n.pane_id.clone(),
                            ),
                        };
                        note = Some((
                            if ok {
                                format!("→ focused {} in {}", what, pane)
                            } else {
                                format!("! could not focus {}", pane)
                            },
                            ok,
                            now() + 3.0,
                        ));
                    }
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let (agents, panels, labels, err) = match state.lock() {
            Ok(g) => (
                g.agents.clone(),
                g.panels.clone(),
                g.labels.clone(),
                g.err.clone(),
            ),
            Err(_) => return,
        };
        // Everything that is not known to be at a prompt shares the
        // PROCESSES section, unread panes at the top of it. They are counted
        // apart in the heading, because "running something" is a claim and
        // the whole point of the unread ones is that the claim cannot be
        // made. [i] hides only the panes we know are resting: hiding one we
        // could not read would be the old bug wearing the new type.
        let busy: Vec<&Panel> = panels.iter().filter(|n| n.doing != Doing::Prompt).collect();
        let resting: Vec<&Panel> = panels.iter().filter(|n| n.doing == Doing::Prompt).collect();
        let unread: Vec<&&Panel> = busy.iter().filter(|n| n.doing == Doing::Unknown).collect();
        let running = busy.len() - unread.len();
        rows_now = agents
            .iter()
            .cloned()
            .map(Row::Agent)
            .chain(
                panels
                    .iter()
                    .filter(|n| show_idle || n.doing != Doing::Prompt)
                    .cloned()
                    .map(Row::Process),
            )
            .collect();
        if !rows_now.is_empty() && selected >= rows_now.len() {
            selected = rows_now.len() - 1;
        }
        if note.as_ref().is_some_and(|(_, _, until)| now() >= *until) {
            note = None;
        }

        let mut counts: HashMap<&str, usize> = HashMap::new();
        for a in &agents {
            *counts.entry(a.state.as_str()).or_insert(0) += 1;
        }
        let places: std::collections::HashSet<&str> =
            agents.iter().map(|a| a.workspace_id.as_str()).collect();

        let mut rows = vec![tc::title("herdr panes", w, &p.accent)];
        let mut summary = vec![
            (
                p.dim.as_str(),
                format!(" {} agent{}", agents.len(), plural(agents.len())),
            ),
            (
                p.dim.as_str(),
                format!(" · {} workspace{}", places.len(), plural(places.len())),
            ),
        ];
        for state_name in ["blocked", "done", "working", "idle"] {
            if let Some(n) = counts.get(state_name) {
                summary.push((colour_of(state_name, &p), format!("   {} {}", n, state_name)));
            }
        }
        rows.push(tc::seg(&summary, w - 1));
        if !err.is_empty() {
            rows.push(tc::seg(&[(p.blocked.as_str(), format!(" ! {}", err))], w - 1));
        }

        let wants = counts.get("blocked").copied().unwrap_or(0)
            + counts.get("done").copied().unwrap_or(0);
        rows.push(if wants > 0 {
            tc::seg(
                &[(
                    if counts.contains_key("blocked") {
                        p.blocked.as_str()
                    } else {
                        p.done.as_str()
                    },
                    format!(" ▸ {} agent{} waiting for you", wants, plural(wants)),
                )],
                w - 1,
            )
        } else {
            tc::seg(&[(p.dim.as_str(), " nothing waiting on you".into())], w - 1)
        });
        rows.push(String::new());

        let wide = w >= 66;

        // The footer must always be the last visible line, so it is built
        // before the body rather than after it: it wraps, so how many rows
        // it takes depends on the width, and the body cannot know its own
        // budget until that is settled. Each section budgeting for itself is
        // what drifted before, and left the footer written past the bottom.
        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
            vec![
                (p.accent.as_str(), "↵".into()),
                (p.dim.as_str(), " switch to this pane".into()),
            ],
            vec![(p.dim.as_str(), "[i]dle".into())],
            vec![(p.dim.as_str(), "[l]abels".into())],
            vec![(p.dim.as_str(), "[r]efresh".into())],
            vec![(p.dim.as_str(), "[q]uit".into())],
        ];
        let footer: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();

        // One window over the three lists read as one, which is the order
        // `rows_now` is in and the order the keys walk. The cursor used to
        // be clamped against the whole list while each section stopped at a
        // row budget of its own, so on any pane too short for everything the
        // cursor walked past the last drawn row and vanished - and enter
        // still acted on whatever it was invisibly sitting on.
        let idle_listed = show_idle && !resting.is_empty();
        // Every heading is drawn, always: AGENTS and its column head, a
        // blank and PROCESSES, and the same again for IDLE when there is an
        // idle section at all. TOY-34's rule survives that way rather than
        // by rationing - the heading and its count are never what gets cut.
        // Everything that is not an entry row: the pinned header already
        // pushed, each section's heading and column head, the note line and
        // the footer. Counted rather than estimated, because one row short
        // is one entry admitted that the truncate below then cuts - and the
        // row it cuts is the last one, which is where the cursor is when you
        // have just pressed end.
        let chrome = rows.len()
            + 2                                       // AGENTS, and its column head
            + 2 + usize::from(wide)                   // blank, PROCESSES, its column head
            + usize::from(!unread.is_empty())         // why they could not be read
            + usize::from(agents.is_empty())          // the line that stands in for a list
            + usize::from(busy.is_empty())
            + if idle_listed { 2 } else { 0 }         // blank, IDLE
            + footer.len()
            + 1;                                      // the note line
        let room = h.saturating_sub(chrome).max(1);
        // An agent takes two rows and a pane takes one, in the order the
        // keys walk them.
        let heights: Vec<usize> = std::iter::repeat(2)
            .take(agents.len())
            .chain(std::iter::repeat(1).take(rows_now.len() - agents.len()))
            .collect();
        let window = window_over(&heights, selected, room, scroll);
        scroll = window.start;

        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── AGENTS ── ".into()),
                (p.dim.as_str(), format!("{}", agents.len())),
                (p.dim.as_str(), showing(&window, 0, agents.len())),
            ],
            w - 1,
        ));
        let mut head = format!(" {:<8} {:<8} {:<6} {:<5}", "AGENT", "STATE", "FOR", "CPU");
        if wide {
            head += &format!(" {:<5} {:<18}", "MEM", "WORKSPACE");
        }
        rows.push(tc::seg(&[(p.dim.as_str(), tc::pad(&head, w - 1))], w - 1));

        for (i, a) in agents.iter().enumerate() {
            if !window.contains(&i) {
                continue;
            }
            let here = i == selected;
            let colour = colour_of(&a.state, &p);
            // Blocked and done keep a tint of their own even unselected: the
            // whole point is that they are visible without being looked for.
            let loud = a.state == "blocked" || a.state == "done";
            let tint = if here {
                tc::bg(38, 56, 76)
            } else if a.state == "blocked" {
                tc::bg(46, 26, 30)
            } else if a.state == "done" {
                tc::bg(22, 46, 34)
            } else {
                String::new()
            };
            let c = |colour: &str| {
                // Any colour that would not clear AA on this tint is swapped
                // for its lighter twin. `dim` was measured first; a review
                // found the others after the first fix shipped saying it was
                // done, so they are here by measurement rather than by guess.
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
            let name: String = a.name.chars().take(6).collect();
            let state_cell = if loud {
                a.state.to_uppercase()
            } else {
                a.state.clone()
            };
            let heat = match a.cpu {
                Some(v) if v > 0.0 => tc::heat((v / 100.0).min(1.0)),
                _ => p.dim.clone(),
            };
            let mut line = vec![
                (
                    c(colour),
                    format!(
                        "{}{} {:<6}",
                        if here { "▸" } else { " " },
                        mark_of(&a.state, tick),
                        name
                    ),
                ),
                (c(colour), format!(" {:<8}", state_cell)),
                (
                    c(&p.dim),
                    format!(" {:<6}", format!("{}{}", if a.exact { "" } else { "≥" }, ago(a.since))),
                ),
                (c(&heat), percent(a.cpu)),
            ];
            if wide {
                let place = if show_labels {
                    let label = labels.get(&a.workspace_id).cloned().unwrap_or_default();
                    if label.is_empty() { a.workspace_id.clone() } else { label }
                } else {
                    a.pane_id.clone()
                };
                line.push((c(&p.dim), format!(" {}", mem(a.rss))));
                line.push((c(&p.accent), format!(" {}", tc::pad(&place, 18))));
            }
            if loud || here {
                line.push((tint.clone(), " ".repeat(w)));
            }
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
            if rows.len() < h.saturating_sub(1) {
                let body = if loud || here { &p.txt } else { &p.dim };
                rows.push(tc::seg(
                    &[
                        (&c(&p.dim), format!("   {}  ", homely(&a.cwd))),
                        (&c(body), a.title.trim().to_string()),
                        (
                            &tint,
                            if loud || here { " ".repeat(w) } else { String::new() },
                        ),
                    ],
                    w - 1,
                ));
            }
        }
        if agents.is_empty() && err.is_empty() {
            rows.push(tc::seg(&[(p.dim.as_str(), "   no agents running".into())], w - 1));
        }

        rows.push(String::new());
        let mut heading = vec![
            (p.lbl.as_str(), " ── PROCESSES ── ".into()),
            (
                p.dim.as_str(),
                format!("{} pane{} running something", running, plural(running)),
            ),
        ];
        if !unread.is_empty() {
            heading.push((
                p.unknown.as_str(),
                format!("  · {} could not be read", unread.len()),
            ));
        }
        heading.push((p.dim.as_str(), showing(&window, agents.len(), busy.len())));
        rows.push(tc::seg(&heading, w - 1));
        if wide {
            rows.push(tc::seg(
                &[(
                    p.dim.as_str(),
                    tc::pad(
                        &format!(" {:<20} {:<5} {:<5} {:<18}", "COMMAND", "CPU", "MEM", "WORKSPACE"),
                        w - 1,
                    ),
                )],
                w - 1,
            ));
        }
        // The reason, on a line of its own rather than after the count in
        // the heading: on an eighty-column pane the heading runs out of room
        // exactly where the reason starts, and the reason is the half worth
        // keeping. They fail one reason at a time - the socket, or a pane
        // going away between the listing and the probe - so the first one
        // speaks for all of them.
        if let Some(n) = unread.first() {
            let why = if n.why.is_empty() { "no reason given" } else { &n.why };
            rows.push(tc::seg(
                &[(p.unknown.as_str(), format!("   ⚠ {}", why))],
                w - 1,
            ));
        }
        for (j, n) in busy.iter().enumerate() {
            if !window.contains(&(agents.len() + j)) {
                continue;
            }
            let here = agents.len() + j == selected;
            let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
            let c = |colour: &str| {
                // Any colour that would not clear AA on this tint is swapped
                // for its lighter twin. `dim` was measured first; a review
                // found the others after the first fix shipped saying it was
                // done, so they are here by measurement rather than by guess.
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
            let heat = match n.cpu {
                Some(v) if v > 0.0 => tc::heat((v / 100.0).min(1.0)),
                _ => p.dim.clone(),
            };
            // An unread pane says so in words where a command would go. A
            // bare "?" is what a running pane with unparseable argv shows,
            // and the two are not the same thing at all.
            let unreadable = n.doing == Doing::Unknown;
            let mut line = vec![
                (
                    c(if unreadable { &p.unknown } else { &p.proc }),
                    format!(
                        "{}{} ",
                        if here { "▸" } else { " " },
                        if unreadable { '⚠' } else { '▪' }
                    ),
                ),
                (
                    c(if unreadable { &p.unknown } else { &p.txt }),
                    tc::pad(
                        if unreadable {
                            "could not be read"
                        } else if n.command.is_empty() {
                            "?"
                        } else {
                            &n.command
                        },
                        20,
                    ),
                ),
                (c(&heat), percent(n.cpu)),
            ];
            if wide {
                let place = if show_labels {
                    let label = labels.get(&n.workspace_id).cloned().unwrap_or_default();
                    if label.is_empty() { n.workspace_id.clone() } else { label }
                } else {
                    n.pane_id.clone()
                };
                line.push((c(&p.dim), format!(" {}", mem(n.rss))));
                line.push((c(&p.accent), format!(" {}", tc::pad(&place, 18))));
            }
            if here {
                line.push((tint.clone(), " ".repeat(w)));
            }
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
        }
        // True only when nothing was unread either: an empty section with a
        // pane the probe failed on is not a Herdr where everything rests.
        if busy.is_empty() {
            rows.push(tc::seg(
                &[(p.dim.as_str(), "   every other pane is idle at a prompt".into())],
                w - 1,
            ));
        }

        // The idle section's heading is drawn whenever there is an idle
        // section at all, and it is never what gets cut - that was TOY-34:
        // dropping it silently left the footer offering [i]dle with nothing
        // behind it. Rationing rows for it is no longer how that is kept.
        // The window bounds the entries above, so the heading always fits,
        // and it says how many are on screen when not all of them are.
        if idle_listed {
            rows.push(String::new());
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── IDLE ── ".into()),
                    (
                        p.dim.as_str(),
                        format!("{} pane{} at a prompt", resting.len(), plural(resting.len())),
                    ),
                    (
                        p.dim.as_str(),
                        showing(&window, agents.len() + busy.len(), resting.len()),
                    ),
                ],
                w - 1,
            ));
            for (j, n) in resting.iter().enumerate() {
                if !window.contains(&(agents.len() + busy.len() + j)) {
                    continue;
                }
                let here = agents.len() + busy.len() + j == selected;
                let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
                let c = |colour: &str| {
                // Any colour that would not clear AA on this tint is swapped
                // for its lighter twin. `dim` was measured first; a review
                // found the others after the first fix shipped saying it was
                // done, so they are here by measurement rather than by guess.
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
                let place = if show_labels {
                    let label = labels.get(&n.workspace_id).cloned().unwrap_or_default();
                    if label.is_empty() { n.workspace_id.clone() } else { label }
                } else {
                    n.pane_id.clone()
                };
                let mut line = vec![
                    (c(&p.idle_c), format!("{}▫ ", if here { "▸" } else { " " })),
                    (c(&p.idle_c), tc::pad(&tail_path(&homely(&n.cwd), 26), 27)),
                    (c(&p.accent), tc::pad(&place, 18)),
                ];
                if here {
                    line.push((tint.clone(), " ".repeat(w)));
                }
                let refs: Vec<(&str, String)> =
                    line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                rows.push(tc::seg(&refs, w - 1));
            }
        }

        let reserve = footer.len() + 1; // +1 for the note line
        rows.truncate(h.saturating_sub(reserve));
        while rows.len() < h.saturating_sub(reserve) {
            rows.push(String::new());
        }
        rows.push(match note.as_ref() {
            Some((text, ok, _)) => tc::seg(
                &[(
                    if *ok { p.done.as_str() } else { p.blocked.as_str() },
                    format!(" {}", text),
                )],
                w - 1,
            ),
            None => String::new(),
        });
        rows.extend(footer);
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_holds_the_cursor_whatever_the_rows_cost() {
        // Fifteen agents at two rows each, then twenty-eight panes at one:
        // the shape that broke it. A window counted in entries admits more
        // rows than the pane has, they are cut off the bottom, and the
        // cursor goes with them.
        let heights: Vec<usize> = std::iter::repeat(2)
            .take(15)
            .chain(std::iter::repeat(1).take(28))
            .collect();
        for room in [1usize, 4, 12, 30, 200] {
            for at in 0..heights.len() {
                for from in [0usize, 5, 14, 30, 42] {
                    let w = window_over(&heights, at, room, from);
                    assert!(w.contains(&at), "room={} at={} from={} gave {:?}", room, at, from, w);
                    // And it fits, unless one entry alone is taller than the
                    // pane - in which case it is drawn anyway, because the
                    // alternative is drawing nothing.
                    let used: usize = heights[w.clone()].iter().sum();
                    assert!(
                        used <= room || w.len() == 1,
                        "room={} at={} from={} drew {} rows in {:?}",
                        room, at, from, used, w
                    );
                }
            }
        }
    }

    #[test]
    fn the_window_holds_still_while_the_cursor_moves_inside_it() {
        let heights = vec![1usize; 40];
        // Already on screen: the view does not jump under the reader.
        assert_eq!(window_over(&heights, 12, 10, 8), 8..18);
        assert_eq!(window_over(&heights, 8, 10, 8), 8..18);
        assert_eq!(window_over(&heights, 17, 10, 8), 8..18);
        // Off the bottom: it moves by exactly enough.
        assert_eq!(window_over(&heights, 18, 10, 8), 9..19);
        // Off the top: it moves to the cursor rather than past it.
        assert_eq!(window_over(&heights, 3, 10, 8), 3..13);
        // An empty list has no window at all.
        assert_eq!(window_over(&[], 0, 10, 0), 0..0);
    }

    #[test]
    fn a_heading_says_its_range_only_when_the_section_is_cut() {
        // Thirteen agents, then fifteen running panes, then fifteen idle.
        let (agents, running, resting) = (13usize, 15usize, 15usize);
        let (a, r, i) = (0, agents, agents + running);

        // A window holding everything says nothing about ranges.
        let all = 0..43;
        assert_eq!(showing(&all, a, agents), "");
        assert_eq!(showing(&all, r, running), "");
        assert_eq!(showing(&all, i, resting), "");

        // A window over the middle: agents cut short, running cut at both
        // ends, idle not reached.
        let mid = 6..20;
        assert_eq!(showing(&mid, a, agents), "  · showing 7-13");
        assert_eq!(showing(&mid, r, running), "  · showing 1-7");
        assert_eq!(showing(&mid, i, resting), "  · none on screen");

        // And past the end: the sections above say so rather than showing a
        // count with no rows under it.
        let low = 30..43;
        assert_eq!(showing(&low, a, agents), "  · none on screen");
        assert_eq!(showing(&low, i, resting), "  · showing 3-15");

        // An empty section has no range to give.
        assert_eq!(showing(&mid, r, 0), "");
    }

    #[test]
    fn the_idle_section_is_never_silently_absent() {
        // TOY-34's rule, which this rewrite had to keep: dropping the idle
        // section silently left the footer offering [i]dle with nothing
        // behind it.
        //
        // It used to be kept by rationing rows - the section was granted a
        // heading only if the lists above had left room. It is now kept by
        // construction: the window bounds how many entry rows the lists
        // above can take, so the heading always fits, and when the window
        // has scrolled past the idle panes the heading says that rather
        // than nothing.
        for start in 0..40usize {
            for room in 1..12usize {
                let window = start..start + room;
                let said = showing(&window, 28, 15);
                assert!(
                    !said.is_empty() || (window.start <= 28 && window.end >= 43),
                    "window {:?} says nothing about a section it does not hold",
                    window
                );
            }
        }
    }


    #[test]
    fn a_runner_gives_way_to_the_script_it_was_handed() {
        // "python3" and "node" say nothing about what a pane is doing.
        let argv = |s: &str| -> Vec<String> {
            s.split_whitespace().map(String::from).collect()
        };
        assert_eq!(
            command_label(&argv("/usr/bin/python3 /home/w/toys/netwatch.py"), ""),
            "netwatch.py"
        );
        // Flags are skipped to reach the script behind them.
        assert_eq!(
            command_label(&argv("node --inspect /srv/app/server.js"), ""),
            "server.js"
        );
        // Anything that is not a runner is its own answer.
        assert_eq!(command_label(&argv("/usr/bin/htop"), ""), "htop");
        // A runner with nothing after it is still the runner.
        assert_eq!(command_label(&argv("bash"), ""), "bash");
        // No argv at all falls back to the name the kernel reports.
        assert_eq!(command_label(&[], "cargo"), "cargo");
        assert_eq!(command_label(&[], ""), "?");
    }

    #[test]
    fn a_cut_path_says_that_it_was_cut() {
        assert_eq!(tail_path("/home/w/projects/toys", 40), "/home/w/projects/toys");
        // The end is kept, because that is the part that names the thing.
        assert_eq!(tail_path("/home/w/projects/toys", 10), "…ects/toys");
        assert_eq!(tail_path("/home/w/projects/toys", 10).chars().count(), 10);
    }

    #[test]
    fn a_duration_carries_minutes_between_an_hour_and_a_day() {
        assert_eq!(ago(45.0), "45s");
        assert_eq!(ago(600.0), "10m");
        // Three hours and one minute is not the same as three hours, and an
        // agent blocked that long is the reason this widget exists.
        assert_eq!(ago(3660.0), "1h01m");
        assert_eq!(ago(14_280.0), "3h58m");
        assert_eq!(ago(90_000.0), "1d");
        assert_eq!(ago(-5.0), "0s");
    }

    #[test]
    fn memory_keeps_its_column_width() {
        // Five, matching the header - the Python gives six to the absent
        // case, which is what this caught.
        for value in [Some(0), Some(4096), Some(1_600_000_000), None] {
            assert_eq!(mem(value).chars().count(), 5, "{:?}", value);
        }
        assert_eq!(mem(Some(512)), " 512B");
        assert_eq!(mem(Some(1024 * 1024 * 3)), "   3M");
        assert_eq!(mem(None), "   --");
    }

    #[test]
    fn the_states_that_want_a_human_sort_first() {
        assert!(rank_of("blocked") < rank_of("done"));
        assert!(rank_of("done") < rank_of("working"));
        assert!(rank_of("working") < rank_of("idle"));
        assert!(rank_of("idle") < rank_of("unknown"));
        // A state Herdr grows later sorts last rather than crashing.
        assert!(rank_of("reticulating") > rank_of("unknown"));
    }

    #[test]
    fn a_proc_stat_line_gives_up_its_cpu_and_memory() {
        // The command sits in brackets and can contain spaces and brackets
        // of its own, which is why the fields are counted from the last one.
        let line = format!(
            "42 (my (odd) proc) S 1 42 42 0 -1 4194304 100 0 0 0 {} {} 0 0 20 0 8 0 900 0 {} 0",
            310, 90, 4096
        );
        let (ticks, rss) = parse_proc_stat(&line).expect("a well-formed line should parse");
        assert_eq!(ticks, 400, "utime and stime are summed");
        assert_eq!(rss, 4096 * 4096, "rss is in pages, reported in bytes");
        // A truncated line has no fields to find and must not be guessed at.
        assert_eq!(parse_proc_stat("42 (short) S 1 2 3"), None);
    }

    #[test]
    fn a_failed_command_is_told_from_a_quiet_one() {
        // What herdr answers a request it cannot serve, verbatim in shape:
        // an `error` object, no `result`, and exit status 0. Nothing about
        // having run the command says it did not work, so a reader looking
        // only for `result` cannot tell this from a pane with nothing to
        // report - which is how a failed probe used to become "idle".
        let failed = r#"{"error":{"code":"pane_not_found","message":"pane not found"},"id":"p"}"#;
        assert_eq!(result_of(failed).unwrap_err(), "pane not found");
        // An error with no message still has to say something.
        assert!(!result_of(r#"{"error":{"code":"busy"}}"#)
            .unwrap_err()
            .is_empty());
        // Output that is not JSON at all is a failure, not a silence.
        assert!(result_of("herdr: no server on this socket").is_err());
        // A null result is nothing, and says so rather than handing it on.
        assert!(result_of(r#"{"id":"p","result":null}"#).is_err());
        // And a result that is there arrives whole.
        let answered = r#"{"id":"p","result":{"process_info":{"pane_id":"w1:p1"}}}"#;
        assert_eq!(
            result_of(answered).unwrap()["process_info"]["pane_id"],
            "w1:p1"
        );
    }

    #[test]
    fn a_pane_at_its_prompt_and_a_pane_nobody_could_read_are_not_one_answer() {
        let info = |front: serde_json::Value, shell: i64| {
            serde_json::json!({
                "process_info": {"foreground_processes": [front], "shell_pid": shell}
            })
        };
        let shell = serde_json::json!({
            "pid": 200, "argv": ["/bin/bash"], "name": "bash", "cwd": "/w"
        });
        // The foreground pid is the shell's own: the prompt is what is in
        // front. This is the only thing that means idle.
        assert!(matches!(classify(&info(shell, 200)), Front::Prompt));

        let build = serde_json::json!({
            "pid": 311, "argv": ["/usr/bin/python3", "/w/build.py"],
            "name": "python3", "cwd": "/w"
        });
        match classify(&info(build, 200)) {
            Front::Running(pid, argv, name, cwd) => {
                assert_eq!(pid, 311);
                assert_eq!(command_label(&argv, &name), "build.py");
                assert_eq!(cwd, "/w");
            }
            _ => panic!("a pane running something is running something"),
        }

        // An answer with nothing readable in it is neither of those. It used
        // to fall through to the same `None` as the prompt, and the pane
        // joined IDLE with no sign that anything had gone wrong.
        assert!(matches!(classify(&serde_json::json!({})), Front::Unknown(_)));
        assert!(matches!(
            classify(&info(serde_json::json!({"argv": ["sh"]}), 200)),
            Front::Unknown(_)
        ));

        // An absent shell_pid says nothing, so it cannot say "prompt".
        let alone = serde_json::json!({
            "process_info": {"foreground_processes": [{"pid": 7, "name": "vi"}]}
        });
        assert!(matches!(classify(&alone), Front::Running(7, _, _, _)));
    }

    #[test]
    fn the_unread_panes_sort_above_the_busy_ones() {
        // The section draws in this order and the cursor indexes it, so the
        // rank is what keeps the two agreeing - and it puts the row the
        // widget could not answer for at the top, where a failure belongs.
        assert!(doing_rank(Doing::Unknown) < doing_rank(Doing::Running));
        assert!(doing_rank(Doing::Running) < doing_rank(Doing::Prompt));
        // A pane nobody has looked at yet is unread, not resting.
        assert_eq!(Doing::default(), Doing::Unknown);
    }
}
