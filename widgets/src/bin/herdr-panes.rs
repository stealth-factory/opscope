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

/// Run a herdr command and hand back the `result` object it printed.
fn herdr(args: &[&str]) -> Option<serde_json::Value> {
    let mut argv = vec!["herdr"];
    argv.extend_from_slice(args);
    let text = tc::run(&argv, RUN_TIMEOUT).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
    match parsed.get("result") {
        Some(serde_json::Value::Null) | None => None,
        Some(value) => Some(value.clone()),
    }
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

/// A pane with no agent in it: either running something, or at a prompt.
#[derive(Clone, Default)]
struct Panel {
    pane_id: String,
    tab_id: String,
    workspace_id: String,
    command: String,
    cwd: String,
    idle: bool,
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

/// The foreground process of a pane, if it is running one.
///
/// A pane sitting at its shell prompt has nothing to report, and the test
/// for that is that the foreground pid is the shell's own.
fn foreground(pane_id: &str) -> Option<(i32, Vec<String>, String, String)> {
    let info = herdr(&["pane", "process-info", "--pane", pane_id])?;
    let process = &info["process_info"];
    let front = process["foreground_processes"].as_array()?.first()?.clone();
    let pid = front["pid"].as_i64()? as i32;
    let busy = process["shell_pid"].as_i64() != Some(pid as i64);
    if !busy {
        return None;
    }
    let argv = front["argv"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Some((pid, argv, text_at(&front, "name"), text_at(&front, "cwd")))
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
            Some((pid, _, _, _)) => cpu_of(seen, pid, at, hz),
            None => (None, None),
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
                Some((pid, _, _, _)) => cpu_of(seen, *pid, at, hz),
                None => (None, None),
            };
            let (command, cwd) = match &front {
                Some((_, argv, name, cwd)) => (
                    command_label(argv, name),
                    if cwd.is_empty() { text_at(pane, "cwd") } else { cwd.clone() },
                ),
                None => (String::new(), text_at(pane, "cwd")),
            };
            panels.push(Panel {
                tab_id: text_at(pane, "tab_id"),
                workspace_id: text_at(pane, "workspace_id"),
                idle: front.is_none(),
                pane_id,
                command,
                cwd,
                cpu,
                rss,
            });
        }
    }
    // Busy first, and the busiest of those first: the point of the section
    // is what is costing something.
    // Idle last, and that ordering is load-bearing twice over. The screen
    // draws `running` then `resting`, and both the cursor and the window are
    // indices into `agents ++ panels` - so the two agree only because
    // `panels` is already running-then-resting. Reorder this and the cursor
    // silently marks one pane while enter switches to another.
    panels.sort_by(|a, b| {
        a.idle
            .cmp(&b.idle)
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
    done: String,
    working: String,
    idle: String,
    unknown: String,
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
    txt: String,
    lbl: String,
    accent: String,
    proc: String,
    idle_c: String,
}

fn palette() -> Palette {
    Palette {
        blocked: tc::rgb(255, 105, 115),
        done: tc::rgb(90, 240, 160),
        working: tc::rgb(255, 200, 90),
        idle: tc::rgb(128, 148, 172),
        unknown: tc::rgb(150, 150, 165),
        dim: tc::rgb(127, 147, 172),
        dim_lit: tc::rgb(140, 170, 195),
        txt: tc::rgb(225, 235, 245),
        lbl: tc::rgb(130, 165, 200),
        accent: tc::rgb(150, 210, 255),
        proc: tc::rgb(170, 190, 215),
        idle_c: tc::rgb(122, 138, 160),
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
        let running: Vec<&Panel> = panels.iter().filter(|n| !n.idle).collect();
        let resting: Vec<&Panel> = panels.iter().filter(|n| n.idle).collect();
        rows_now = agents
            .iter()
            .cloned()
            .map(Row::Agent)
            .chain(
                panels
                    .iter()
                    .filter(|n| show_idle || !n.idle)
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
            + usize::from(agents.is_empty())          // the line that stands in for a list
            + usize::from(running.is_empty())
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
                let colour = if tint.is_empty() || colour != p.dim {
                    colour
                } else {
                    p.dim_lit.as_str()
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
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── PROCESSES ── ".into()),
                (
                    p.dim.as_str(),
                    format!(
                        "{} pane{} running something",
                        running.len(),
                        plural(running.len())
                    ),
                ),
                (p.dim.as_str(), showing(&window, agents.len(), running.len())),
            ],
            w - 1,
        ));
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
        for (j, n) in running.iter().enumerate() {
            if !window.contains(&(agents.len() + j)) {
                continue;
            }
            let here = agents.len() + j == selected;
            let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
            let c = |colour: &str| {
                let colour = if tint.is_empty() || colour != p.dim {
                    colour
                } else {
                    p.dim_lit.as_str()
                };
                format!("{}{}", tint, colour)
            };
            let heat = match n.cpu {
                Some(v) if v > 0.0 => tc::heat((v / 100.0).min(1.0)),
                _ => p.dim.clone(),
            };
            let mut line = vec![
                (c(&p.proc), format!("{}▪ ", if here { "▸" } else { " " })),
                (
                    c(&p.txt),
                    tc::pad(if n.command.is_empty() { "?" } else { &n.command }, 20),
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
        if running.is_empty() {
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
                        showing(&window, agents.len() + running.len(), resting.len()),
                    ),
                ],
                w - 1,
            ));
            for (j, n) in resting.iter().enumerate() {
                if !window.contains(&(agents.len() + running.len() + j)) {
                    continue;
                }
                let here = agents.len() + running.len() + j == selected;
                let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
                let c = |colour: &str| {
                let colour = if tint.is_empty() || colour != p.dim {
                    colour
                } else {
                    p.dim_lit.as_str()
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
}
