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
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── AGENTS ── ".into()),
                (p.dim.as_str(), format!("{}", agents.len())),
            ],
            w - 1,
        ));
        let mut head = format!(" {:<8} {:<8} {:<6} {:<5}", "AGENT", "STATE", "FOR", "CPU");
        if wide {
            head += &format!(" {:<5} {:<18}", "MEM", "WORKSPACE");
        }
        rows.push(tc::seg(&[(p.dim.as_str(), tc::pad(&head, w - 1))], w - 1));

        for (i, a) in agents.iter().enumerate() {
            if rows.len() >= h.saturating_sub(6) {
                break;
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
            let c = |colour: &str| format!("{}{}", tint, colour);
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
        // The idle section is claimed before the running list spends the
        // pane, not after. It used to be filled up to the same h-2 the
        // idle loop then checked, so on a busy machine the heading was
        // pushed past the bottom and truncated - while the header counted
        // the panes and the footer offered the key to reveal them.
        let idle_room = if show_idle && !resting.is_empty() { 3 } else { 0 };
        let running_budget = h.saturating_sub(2 + idle_room);
        for (j, n) in running.iter().enumerate() {
            if rows.len() >= running_budget {
                break;
            }
            let here = agents.len() + j == selected;
            let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
            let c = |colour: &str| format!("{}{}", tint, colour);
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

        // Two lines for the blank and the heading, one for a pane. Below
        // that the section cannot be drawn, but the count still can: one
        // line saying how many are there and that the pane is too short to
        // list them, rather than nothing at all under a footer still
        // offering the key.
        if show_idle
            && !resting.is_empty()
            && rows.len() + 3 > h.saturating_sub(2)
            && rows.len() < h.saturating_sub(2)
        {
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── IDLE ── ".into()),
                    (
                        p.dim.as_str(),
                        format!(
                            "{} pane{} at a prompt, too short to list",
                            resting.len(),
                            plural(resting.len())
                        ),
                    ),
                ],
                w - 1,
            ));
        }
        if show_idle && !resting.is_empty() && rows.len() + 3 <= h.saturating_sub(2) {
            rows.push(String::new());
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── IDLE ── ".into()),
                    (
                        p.dim.as_str(),
                        format!("{} pane{} at a prompt", resting.len(), plural(resting.len())),
                    ),
                ],
                w - 1,
            ));
            for (j, n) in resting.iter().enumerate() {
                if rows.len() >= h.saturating_sub(2) {
                    break;
                }
                let here = agents.len() + running.len() + j == selected;
                let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
                let c = |colour: &str| format!("{}{}", tint, colour);
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

        // The footer must always be the last visible line, so the body is
        // clamped to the space left for it rather than each section
        // budgeting for itself - that drifted, and the footer ended up
        // written past the bottom row.
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
