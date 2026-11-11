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

//! The Herdr backend of `mux-activity`, which was herdr-panes.
//!
//! Everything running in Herdr, across every workspace.
//!
//! A port of herdr-panes.py. A Herdr client rather than a general agent
//! monitor: the inventory and the lifecycle states come from the `herdr`
//! CLI, and any agent kind it recognises appears here with no change to
//! this file.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opscope_core as tc;

use super::herdr_events as events;
use super::herdr_parse as parse;
use super::{process_filter, refresh_wake};


/// Worst first: the states that want a human are the reason to look.
const RANK: &[&str] = &["blocked", "done", "working", "idle", "unknown"];

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

/// Bring an agent's pane to the front of the attached Herdr window.
///
/// `agent focus` alone is not enough. Herdr 0.9.0 gave each attached client
/// its own view, and on that release `agent focus` moved the server's focus
/// without moving any client: the command succeeded, the pane said
/// "focused", and the window stayed where it was. 0.9.1 fixed it upstream,
/// but a client update leaves the running server alone, so a 0.9.0 server
/// can outlive the upgrade by days. `tab focus` does move clients on every
/// release, so it goes first to bring the tab into view, and `agent focus`
/// follows to pick the agent's pane inside it and mark it seen.
///
/// `cached_tab` is the tab on the row, from the last poll. An agent can
/// move in that interval. Focusing the tab it left, on 0.9.0, leaves the
/// client there while both commands succeed. The tab is read again from a
/// fresh `agent list`; the row's id is only the fallback when that read
/// fails or no longer names this pane.
fn focus_agent(cached_tab: &str, pane_id: &str) -> bool {
    let tab_id = tab_for_focus(herdr(&["agent", "list"]).as_ref(), pane_id, cached_tab);
    // An agent list without a tab id is not one this was written against;
    // fall through to the plain command rather than refusing.
    let tab = tab_id.is_empty() || herdr_action(&["tab", "focus", &tab_id]);
    tab && herdr_action(&["agent", "focus", pane_id])
}

/// Which tab to focus for `pane_id`.
///
/// A list that names the agent wins, including a blank tab: that is the
/// list saying there is nothing to focus, and a cached id would send the
/// client somewhere the agent is not. `None` — the list failed, or it does
/// not name this pane — keeps `cached`, which is the last place the poll
/// saw it.
fn tab_for_focus(listed: Option<&serde_json::Value>, pane_id: &str, cached: &str) -> String {
    listed
        .and_then(|list| tab_holding(list, pane_id))
        .unwrap_or_else(|| cached.to_string())
}

/// The tab `pane_id` is on, in an `agent list` result.
///
/// `Some("")` means the list names the agent and gives it no tab. `None`
/// means the list does not name it.
fn tab_holding(listed: &serde_json::Value, pane_id: &str) -> Option<String> {
    listed
        .get("agents")?
        .as_array()?
        .iter()
        .find(|entry| text_at(entry, "pane_id") == pane_id)
        .map(|entry| text_at(entry, "tab_id"))
}

/// The `result` object out of one herdr answer, or why there is none.
///
/// Split from the running of the command, because the running is not where
/// the failure shows: herdr answers a request it cannot serve with an
/// `error` object and exit status 0, so a pane that does not exist and a
/// pane with nothing to say arrive as two commands that both succeeded.
/// They are told apart by shape here or they are not told apart at all.
fn result_of(text: &str) -> Result<serde_json::Value, String> {
    parse::parse_result(text)
}

pub fn herdr_text(args: &[&str]) -> Result<String, String> {
    let mut argv = vec!["herdr"];
    argv.extend_from_slice(args);
    tc::run(&argv, RUN_TIMEOUT)
}

/// Run a herdr command and hand back its `result`, or why there is none.
fn herdr_result(args: &[&str]) -> Result<serde_json::Value, String> {
    result_of(&herdr_text(args)?)
}

/// The same, for the callers that have nothing to do with the reason.
fn herdr(args: &[&str]) -> Option<serde_json::Value> {
    herdr_result(args).ok()
}

/// How wide each text column is drawn this frame.
///
/// The text columns grow with the pane. Their old fixed widths are the
/// minimums, so nothing moves at the `wide` threshold, and whatever the
/// numbers leave goes to the column still cutting something: a workspace
/// label stayed cut at 18 cells on a 200-column pane with the rest blank.
#[derive(Debug, PartialEq)]
struct Columns {
    name_w: usize,
    agent_place_w: usize,
    command_w: usize,
    busy_place_w: usize,
    dir_w: usize,
    idle_place_w: usize,
}

impl Columns {
    /// `want` is the widest value of each column this frame, in the order
    /// the fields are declared.
    fn fit(w: usize, wide: bool, want: [usize; 6]) -> Columns {
        let row = w.saturating_sub(1);
        // An agent row: `▸`, the mark and a space, the name, then STATE, FOR
        // and CPU; MEM and the space before the place when wide.
        let (name_w, agent_place_w) = if wide {
            let fit = tc::fit_columns(
                row.saturating_sub(3 + 9 + 7 + 5 + 6 + 1),
                &[(6, want[0]), (18, want[1])],
            );
            (fit[0], fit[1])
        } else {
            (
                tc::fit_columns(row.saturating_sub(3 + 9 + 7 + 5), &[(6, want[0])])[0],
                0,
            )
        };
        // A process row: `▸`, the mark and a space, the command, CPU; MEM
        // and the space before the place when wide.
        let (command_w, busy_place_w) = if wide {
            let fit = tc::fit_columns(
                row.saturating_sub(3 + 5 + 6 + 1),
                &[(20, want[2]), (18, want[3])],
            );
            (fit[0], fit[1])
        } else {
            (
                tc::fit_columns(row.saturating_sub(3 + 5), &[(20, want[2])])[0],
                0,
            )
        };
        // An idle row: `▸`, the mark and a space, the directory and a space,
        // then the place.
        let idle = tc::fit_columns(row.saturating_sub(3 + 1), &[(26, want[4]), (18, want[5])]);
        Columns {
            name_w,
            agent_place_w,
            command_w,
            busy_place_w,
            dir_w: idle[0],
            idle_place_w: idle[1],
        }
    }
}

/// Keep the end of a path, marking the cut so it does not read as a name.
fn tail_path(path: &str, n: usize) -> String {
    // Cells, not characters: a directory named in CJK is two cells a
    // character, and counting characters let it run past the column into
    // whatever the row says after it.
    if opscope_core::display_width(path) <= n || n < 2 {
        return path.to_string();
    }
    let mut start = path.len();
    for (at, _) in path.char_indices().rev() {
        if opscope_core::display_width(&path[at..]) > n - 1 {
            break;
        }
        start = at;
    }
    format!("…{}", &path[start..])
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
        return if name.is_empty() {
            "?".into()
        } else {
            name.into()
        };
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

/// (cpu ticks used, resident bytes) for a pid, from `/proc/<pid>/stat`
/// through `tc::procfs`, which counts resident pages at this kernel's page
/// size rather than at 4096.
fn proc_stats(pid: i32) -> Option<(u64, u64)> {
    let stat = tc::procfs::read_stat(pid).ok()?;
    Some((stat.utime + stat.stime, tc::procfs::rss_bytes(stat.rss_pages)))
}

/// A recognised coding agent, and what its process is costing.
#[derive(Clone, Default)]
pub(crate) struct Agent {
    name: String,
    pane_id: String,
    tab_id: String,
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
    /// Shells the agent started that are still running, one row each under
    /// the agent. Herdr carries no child records, so this is the process
    /// table's answer and the row says so.
    work: tc::agent_work::Work,
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
///
/// `chase` is false on a frame the wheel moved the view rather than a key
/// moving the cursor. Then `from` stands as given and the cursor is allowed
/// to scroll out of sight - it is still where enter will act, and the next
/// arrow press brings the window back to it.
fn window_over(
    heights: &[usize],
    at: usize,
    room: usize,
    from: usize,
    chase: bool,
) -> std::ops::Range<usize> {
    let n = heights.len();
    if n == 0 || room == 0 {
        return 0..0;
    }
    let at = at.min(n - 1);
    if !chase {
        // The last start worth having is the one whose window still ends on
        // the last entry; past it the view would scroll into blank space.
        let (mut last, mut used) = (n, 0usize);
        while last > 0 && used + heights[last - 1] <= room {
            used += heights[last - 1];
            last -= 1;
        }
        // An entry taller than the pane cannot join `last`, so `last`
        // stays `n`. Without the clamp, a wheel that has already reached
        // the end then produces `n..n` and the list vanishes while the
        // hidden selection stays actionable.
        let first = from.min(last.min(n - 1));
        let (mut used, mut end) = (0usize, first);
        while end < n && used + heights[end] <= room {
            used += heights[end];
            end += 1;
        }
        return first..end.max(first + 1).min(n);
    }
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
pub(crate) struct Panel {
    pane_id: String,
    tab_id: String,
    workspace_id: String,
    command: String,
    is_opscope: bool,
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
pub(crate) struct State {
    pub(crate) agents: Vec<Agent>,
    pub(crate) panels: Vec<Panel>,
    pub(crate) labels: HashMap<String, String>,
    pub(crate) err: String,
    pub(crate) watcher_err: String,
    pub(crate) inventory_err: String,
    pub(crate) pane_ids: Vec<String>,
    pub(crate) inventory_generation: u64,
    pub(crate) inventory_ready: bool,
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

fn run_watcher_catching_panic(state: &Arc<Mutex<State>>, watch: impl FnOnce()) {
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(watch)) {
        if let Ok(mut guard) = state.lock() {
            guard.watcher_err =
                format!("event watcher panicked: {}", panic_reason(payload.as_ref()));
        }
    }
}

fn update_inventory(state: &mut State, pane_ids: Option<Vec<String>>, inventory_err: String) {
    if let Some(pane_ids) = pane_ids {
        if !state.inventory_ready || state.pane_ids != pane_ids {
            state.inventory_generation = state.inventory_generation.wrapping_add(1);
        }
        state.pane_ids = pane_ids;
        state.inventory_ready = true;
        state.inventory_err.clear();
    } else {
        state.inventory_err = inventory_err;
    }
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
/// rather than on a live Herdr, the way `tc::procfs::parse_pid_stat` is.
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

fn front_is_opscope(front: &Front) -> bool {
    matches!(front, Front::Running(_, argv, name, _) if process_filter::matches(name, &argv.join(" ")))
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
    // Herdr's own order, for workspaces whose most urgent agents tie.
    let mut order: Vec<String> = Vec::new();
    if let Some(res) = herdr(&["workspace", "list"]) {
        for w in res["workspaces"].as_array().into_iter().flatten() {
            order.push(text_at(w, "workspace_id"));
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

    let pane_listing = herdr_text(&["pane", "list"]);
    let (pane_ids, inventory_err) = match &pane_listing {
        Ok(text) => match parse::parse_pane_ids(text) {
            Ok(ids) => (Some(ids), String::new()),
            Err(why) => (None, why.clone()),
        },
        Err(why) => (None, why.clone()),
    };

    let at = tc::now();
    let mut agents = Vec::new();
    // One process table for every agent, read only when there are agents.
    let table = if listed["agents"].as_array().is_some_and(|a| !a.is_empty()) {
        Some(tc::agent_work::read_process_table())
    } else {
        None
    };
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
        let front = foreground(&pane_id);
        let (cpu, rss) = match &front {
            Front::Running(pid, _, _, _) => cpu_of(seen, *pid, at, hz),
            // An agent's state comes from `agent list`, not from the probe,
            // so a failed probe costs the row its CPU and memory and
            // nothing else - and those already draw as `-` and `--` when
            // there is no reading, which there is not.
            Front::Prompt | Front::Unknown(_) => (None, None),
        };
        let work = match (&front, &table) {
            (Front::Running(pid, _, _, _), Some(table)) => {
                // The first foreground process may be a launcher or a job
                // rather than the agent, so the agent is looked for by name.
                let name = text_at(entry, "agent");
                let pid = u32::try_from(*pid).ok();
                let agent = match table {
                    Ok(t) => pid.map(|pid| t.agent_at(pid, &name)),
                    Err(_) => pid,
                };
                tc::agent_work::Work::of(table, agent)
            }
            _ => tc::agent_work::Work::default(),
        };
        agents.push(Agent {
            name: text_at(entry, "agent"),
            workspace_id: text_at(entry, "workspace_id"),
            tab_id: text_at(entry, "tab_id"),
            title: text_at(entry, "terminal_title_stripped"),
            cwd: text_at(entry, "cwd"),
            state: state_name,
            since: at - began,
            exact,
            cpu,
            rss,
            work,
            pane_id,
        });
    }
    agents.sort_by(|a, b| {
        rank_of(&a.state)
            .cmp(&rank_of(&b.state))
            .then(b.since.total_cmp(&a.since))
    });
    // Drawn, walked by the keys and indexed by the cursor in this order, so
    // the grouping happens here and nowhere else.
    let agents = by_workspace(agents, &order);

    let mut panels = Vec::new();
    if let Some(listing) = pane_listing
        .as_ref()
        .ok()
        .and_then(|text| result_of(text).ok())
    {
        for pane in listing["panes"].as_array().into_iter().flatten() {
            if pane.get("agent").is_some_and(|a| !a.is_null()) {
                continue;
            }
            let pane_id = text_at(pane, "pane_id");
            let front = foreground(&pane_id);
            let is_opscope = front_is_opscope(&front);
            let (cpu, rss) = match &front {
                Front::Running(pid, _, _, _) => cpu_of(seen, *pid, at, hz),
                Front::Prompt | Front::Unknown(_) => (None, None),
            };
            let (doing, command, cwd, why) = match front {
                Front::Running(_, argv, name, cwd) => (
                    Doing::Running,
                    command_label(&argv, &name),
                    if cwd.is_empty() {
                        text_at(pane, "cwd")
                    } else {
                        cwd
                    },
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
                Front::Unknown(why) => (Doing::Unknown, String::new(), text_at(pane, "cwd"), why),
            };
            panels.push(Panel {
                tab_id: text_at(pane, "tab_id"),
                workspace_id: text_at(pane, "workspace_id"),
                doing,
                is_opscope,
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
        update_inventory(&mut guard, pane_ids, inventory_err);
    }
    seen.first_poll = false;
}

/// Agents regrouped by workspace, without losing who needs you.
///
/// Each workspace's agents stay together in the order they already had -
/// worst first. Workspaces are ordered by their most urgent agent, then by
/// Herdr's own workspace order, so a blocked agent still puts its workspace
/// at the top rather than waiting behind a quiet one that is listed first.
/// Rex's `by_session`, for Herdr's workspaces.
fn by_workspace(agents: Vec<Agent>, order: &[String]) -> Vec<Agent> {
    let mut groups: Vec<(usize, usize, Vec<Agent>)> = Vec::new();
    for agent in agents {
        let rank = rank_of(&agent.state);
        match groups
            .iter_mut()
            .find(|(_, _, g)| g[0].workspace_id == agent.workspace_id)
        {
            Some((worst, _, g)) => {
                *worst = (*worst).min(rank);
                g.push(agent);
            }
            None => {
                let at = order
                    .iter()
                    .position(|id| *id == agent.workspace_id)
                    .unwrap_or(usize::MAX);
                groups.push((rank, at, vec![agent]));
            }
        }
    }
    // Stable, so workspaces Herdr did not list keep the order they arrived in.
    groups.sort_by_key(|(worst, at, _)| (*worst, *at));
    groups.into_iter().flat_map(|(_, _, g)| g).collect()
}

/// The rows under an agent in a window `room` rows tall and `width`
/// columns wide: every job, wrapped, while they fit beside the agent's own
/// two rows; else the whole jobs that fit and a last row that counts the
/// rest. The window never splits an entry, so this keeps one agent from
/// outgrowing it and hiding jobs it cannot scroll to. Each row says whether
/// it is the explanation of an unread table, which is drawn dim.
fn job_lines(work: &tc::agent_work::Work, width: usize, room: usize) -> Vec<(bool, String)> {
    let max = room.saturating_sub(2).max(1);
    let all = work.rows(width);
    if all.len() <= max {
        return all;
    }
    let mut out = Vec::new();
    let mut shown = 0;
    for job in &work.tasks {
        let rows = job.rows(width);
        if out.len() + rows.len() > max - 1 {
            break;
        }
        out.extend(rows.into_iter().map(|row| (false, row)));
        shown += 1;
    }
    out.push((false, format!("   ├ +{} more  running", work.tasks.len() - shown)));
    out
}

/// What goes above agent `i`: nothing for the section's first agent when
/// there is one workspace, a gap between two agents of one workspace, and
/// a gap and the workspace's heading where a workspace begins. A heading
/// only when there is more than one workspace to tell apart - with one, the
/// heading would name the only thing there is.
///
/// The rows it returns are what the window counts, so `y` decides only
/// where the heading's rule is placed, not how many rows there are.
fn lead_rows(
    agents: &[Agent],
    i: usize,
    w: usize,
    name_of: &dyn Fn(&str) -> String,
    p: &Palette,
    y: usize,
    plots: &mut Vec<tc::Plot>,
) -> Vec<String> {
    let grouped = agents
        .iter()
        .any(|a| a.workspace_id != agents[0].workspace_id);
    let starts = i == 0 || agents[i - 1].workspace_id != agents[i].workspace_id;
    let mut out = Vec::new();
    if i > 0 {
        out.push(String::new());
    }
    if grouped && starts {
        let n = agents
            .iter()
            .filter(|a| a.workspace_id == agents[i].workspace_id)
            .count();
        let at = y + out.len();
        out.extend(super::group_heading(
            &name_of(&agents[i].workspace_id),
            n,
            w,
            (&p.accent, &p.dim, &p.rule),
            at,
            plots,
        ));
    }
    out
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
/// Five for a reading and five for an absent one. A six-wide dash pushed
/// the workspace column one cell right of every other row. The header
/// allots five.
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
    /// `dim` once measured 3.81 against the selected-row tint, under the 4.5
    /// CLAUDE.md asks for against the tint as well as the background. This is
    /// the same role lifted - `ink_on(Muted, true)` - and it is used *only*
    /// where a tint is on, so an untinted row is exactly the colour it always
    /// was. Not quite the same as
    /// "unselected": a blocked row is tinted whether or not it is selected,
    /// and gets the lighter colours too.
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
    /// The rule under a workspace's sub-heading: decoration, never text.
    rule: String,
    /// The lifted form of every other role this palette draws in, for text
    /// on a selected or tinted row: the named `_lit` twins above are swapped
    /// in by the tint closure first, and anything else it is handed goes
    /// through [`Palette::on_tint`].
    lifts: Vec<(String, String)>,
}

impl Palette {
    /// `colour` as it should be drawn on a tinted row: its role's lifted
    /// form when it is one of this palette's, otherwise `colour` itself.
    fn on_tint<'a>(&'a self, colour: &'a str) -> &'a str {
        self.lifts
            .iter()
            .find(|(plain, _)| plain == colour)
            .map(|(_, lifted)| lifted.as_str())
            .unwrap_or(colour)
    }
}

fn palette() -> Palette {
    Palette {
        blocked: tc::ink(tc::Role::Red),
        blocked_lit: tc::ink_on(tc::Role::Red, true),
        done: tc::ink(tc::Role::Green),
        working: tc::ink(tc::Role::Yellow),
        idle: tc::ink(tc::Role::Muted),
        idle_lit: tc::ink_on(tc::Role::Muted, true),
        unknown: tc::ink(tc::Role::Muted),
        unknown_lit: tc::ink_on(tc::Role::Muted, true),
        dim: tc::ink(tc::Role::Muted),
        dim_lit: tc::ink_on(tc::Role::Muted, true),
        txt: tc::ink(tc::Role::Fg),
        lbl: tc::ink(tc::Role::Muted),
        accent: tc::ink(tc::Role::Accent),
        proc: tc::ink(tc::Role::Muted),
        idle_c: tc::ink(tc::Role::Muted),
        idle_c_lit: tc::ink_on(tc::Role::Muted, true),
        rule: tc::ink(tc::Role::Border),
        lifts: [tc::Role::Green, tc::Role::Yellow, tc::Role::Fg, tc::Role::Accent]
            .into_iter()
            .map(|role| (tc::ink(role), tc::ink_on(role, true)))
            .collect(),
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
        "working" => tc::SPINNER[tick % tc::SPINNER.len()],
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

/// Run the board against the Herdr session until `q`. `why` is the line
/// under the title saying what chose Herdr; `comma` opens the shared
/// settings screen.
pub fn watch(cfg: &serde_json::Value, args: &[String], why: &str, comma: &dyn Fn(&mut tc::Keyboard)) {
    let mut refresh = tc::poll_secs(tc::cfg_f64(cfg, "refresh", 4.0), 4.0);
    if let Some(at) = args.iter().position(|a| a == "-n" || a == "--refresh") {
        if let Some(n) = args.get(at + 1) {
            refresh = tc::poll_secs(n.parse().unwrap_or(4.0), 4.0).max(1.0);
        }
    }

    let p = palette();
    let state = Arc::new(Mutex::new(State::default()));
    let activity = tc::Refresh::default();
    let wake = refresh_wake::new();
    let poller = Arc::clone(&state);
    let poller_wake = Arc::clone(&wake);
    let poller_activity = activity.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let poller_stop = Arc::clone(&stop);
    std::thread::spawn(move || {
        let hz = tc::procfs::clock_ticks();
        let mut seen = Seen {
            first_poll: true,
            ..Default::default()
        };
        loop {
            let work = poller_activity.start();
            // A poller that dies takes its explanation with it, and an empty
            // board looks exactly like a Herdr with nothing running in it.
            let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                poll(&poller, &mut seen, hz)
            }));
            if let Err(payload) = step {
                if let Ok(mut guard) = poller.lock() {
                    guard.err = format!("poller stopped: {}", panic_reason(payload.as_ref()));
                }
                return;
            }
            drop(work);
            if poller_stop.load(Ordering::Relaxed) {
                return;
            }
            if refresh_wake::wait_for(&poller_wake, Duration::from_secs_f64(refresh)).is_err() {
                if let Ok(mut guard) = poller.lock() {
                    guard.err = "poller wake signal failed".into();
                }
                return;
            }
        }
    });
    let watcher_state = Arc::clone(&state);
    let watcher_wake = Arc::clone(&wake);
    let watcher_stop = Arc::clone(&stop);
    std::thread::spawn(move || {
        let loop_state = Arc::clone(&watcher_state);
        run_watcher_catching_panic(&watcher_state, move || {
            events::watch(loop_state, watcher_wake, refresh, watcher_stop)
        });
    });

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let (mut show_labels, mut show_idle) = (true, true);
    let mut opscope_filter = process_filter::Filter::default();
    let mut selected = 0usize;
    // Where each pane's rows landed on the frame now on screen. The three
    // sections share one index - agents, then busy, then the rest - which
    // is what the arrows walk, so a click lands in the same space and
    // needs nothing extra to say which section it was in.
    let mut placed: Vec<(usize, usize)> = Vec::new();
    // How far down the three lists, read as one, the window has scrolled.
    // Kept across frames so the view holds still while the cursor moves
    // inside it, and only moves when the cursor would leave it.
    let mut scroll = 0usize;
    // Set by whichever key moved the cursor, and cleared once the window has
    // been asked to hold it. Without it the window chases the cursor every
    // frame and drags itself straight back from wherever the wheel put it.
    let mut moved = false;
    let mut note: Option<(String, bool, f64)> = None;
    let mut rows_now: Vec<Row> = Vec::new();
    // Where each section starts in `rows_now`, read as one flat list, with
    // the empty ones left out. Written by the frame, because that is where
    // the three lists are split, and read by tab on the frame after - which
    // is the same one-frame lag `rows_now` itself already has.
    let mut sections: Vec<usize> = Vec::new();

    loop {
        let tick = tc::animation_tick();
        // A click on another row moves the cursor there; a click on the row
        // it is already on becomes `enter`, which is the key the footer
        // names for opening one. Rewritten before the match rather than
        // acted on here, so the arm below does the opening and this cannot
        // drift from what the keyboard does.
        // The list windows itself - rows outside `window_over` are never
        // built - so the frame is the body and there is nothing to subtract.
        let mut keys = keyboard.poll();
        if let Some(at) = tc::rows_clicked(&mut keys, Some(selected), 0, 0, &placed, None) {
            selected = at;
        }
        for key in keys {
            match key.as_str() {
                "q" | "Q" => {
                    stop.store(true, Ordering::Relaxed);
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "," => {
                    comma(&mut keyboard);
                    continue;
                }
                "r" | "R" => {
                    refresh_wake::request(&wake);
                }
                "l" | "L" => show_labels = !show_labels,
                "o" | "O" => {
                    opscope_filter.toggle(&mut selected);
                    moved = true;
                }
                "i" | "I" => {
                    show_idle = !show_idle;
                    selected = 0;
                    moved = true;
                }
                "up" => {
                    selected = selected.saturating_sub(1);
                    moved = true;
                }
                "down" => {
                    selected += 1;
                    moved = true;
                }
                // The wheel moves the view and nothing else. Selection is
                // the arrows' job, here as everywhere in the collection.
                "ctrl-y" | "wheel-up" => scroll = scroll.saturating_sub(1),
                "ctrl-e" | "wheel-down" => scroll = scroll.saturating_add(1),
                // Tab walks the section heads rather than the rows: on a
                // Herdr with twenty idle panes, reaching PROCESSES with the
                // arrows is thirty presses. Wraps, so it never dead-ends.
                "tab" => {
                    if let Some(next) = sections
                        .iter()
                        .find(|&&at| at > selected)
                        .or_else(|| sections.first())
                    {
                        selected = *next;
                        moved = true;
                    }
                }
                "home" => {
                    selected = 0;
                    moved = true;
                }
                "end" => {
                    selected = rows_now.len().saturating_sub(1);
                    moved = true;
                }
                "enter" | "f" | "F" => {
                    if let Some(row) = rows_now.get(selected.min(rows_now.len().saturating_sub(1)))
                    {
                        let (ok, what, pane) = match row {
                            Row::Agent(a) => (
                                focus_agent(&a.tab_id, &a.pane_id),
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
                            tc::now() + 3.0,
                        ));
                    }
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let (agents, panels, labels, err, watcher_err, inventory_err) = match state.lock() {
            Ok(g) => (
                g.agents.clone(),
                g.panels.clone(),
                g.labels.clone(),
                g.err.clone(),
                g.watcher_err.clone(),
                g.inventory_err.clone(),
            ),
            Err(_) => return,
        };
        // Everything that is not known to be at a prompt shares the
        // PROCESSES section, unread panes at the top of it. They are counted
        // apart in the heading, because "running something" is a claim and
        // the whole point of the unread ones is that the claim cannot be
        // made. [i] hides only the panes we know are resting: hiding one we
        // could not read would be the old bug wearing the new type.
        let total_panels = panels.len();
        let hidden_opscope = panels
            .iter()
            .filter(|n| !opscope_filter.visible(n.is_opscope))
            .count();
        let panels: Vec<Panel> = panels
            .into_iter()
            .filter(|n| opscope_filter.visible(n.is_opscope))
            .collect();
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
        if note
            .as_ref()
            .is_some_and(|(_, _, until)| tc::now() >= *until)
        {
            note = None;
        }

        let mut counts: HashMap<&str, usize> = HashMap::new();
        for a in &agents {
            *counts.entry(a.state.as_str()).or_insert(0) += 1;
        }
        let places: std::collections::HashSet<&str> =
            agents.iter().map(|a| a.workspace_id.as_str()).collect();

        let mut plots = Vec::new();
        let mut rows = vec![tc::title_busy(
            "mux activity · herdr",
            w,
            &p.accent,
            activity.tick(tc::animation_tick()),
        )];
        rows.extend(super::mux_rows(why, &p.dim, w));
        // The span each pane covers, taken from where its rows started and
        // ended: a pane is one row, or several once its detail is drawn.
        let mut rows_at: Vec<(usize, usize)> = Vec::new();
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
                summary.push((
                    colour_of(state_name, &p),
                    format!("   {} {}", n, state_name),
                ));
            }
        }
        rows.push(tc::seg(&summary, w - 1));
        if !agents.is_empty() {
            let mut states: Vec<_> = counts.iter().collect();
            states.sort_by_key(|(state, _)| **state);
            let parts: Vec<_> = states.into_iter().map(|(state, n)|
                (*n as f64 / agents.len() as f64, colour_of(state, &p).to_string())).collect();
            let width = w.saturating_sub(2);
            plots.push(tc::Plot::stacked_bar(1, rows.len(), width, &parts));
            let bar = tc::stacked_bar(&parts, width);
            let mut line = vec![(tc::RST, " ".into())];
            line.extend(bar.iter().map(|(c, t)| (c.as_str(), t.clone())));
            rows.push(tc::seg(&line, w.saturating_sub(1)));
        }
        if !err.is_empty() {
            rows.push(tc::seg(
                &[(p.blocked.as_str(), format!(" ! {}", err))],
                w - 1,
            ));
        }
        if !inventory_err.is_empty() {
            rows.push(tc::seg(
                &[(
                    p.blocked.as_str(),
                    format!(" ! pane inventory: {}", inventory_err),
                )],
                w - 1,
            ));
        }
        if !watcher_err.is_empty() {
            rows.push(tc::seg(
                &[(
                    p.blocked.as_str(),
                    format!(" ! event stream: {}", watcher_err),
                )],
                w - 1,
            ));
        }

        let wants =
            counts.get("blocked").copied().unwrap_or(0) + counts.get("done").copied().unwrap_or(0);
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
        // Hiding the idle panes is a filter, and an unstated filter leaves
        // a short list looking like a quiet Herdr. Drawn only when it is
        // holding something back: with nothing hidden there is nothing to
        // say, and saying it anyway is a row that never comes off.
        let mut hidden_idle = idle_filter(show_idle, resting.len());
        hidden_idle.extend(opscope_filter.description(hidden_opscope));
        let shown = busy.len() + if show_idle { resting.len() } else { 0 };
        if let Some(said) = tc::filter_row(shown, total_panels, &hidden_idle) {
            rows.push(tc::seg(&[(p.dim.as_str(), format!(" {}", said))], w - 1));
        }
        rows.push(String::new());

        let wide = w >= 66;

        // The footer must always be the last visible line, so it is built
        // before the body rather than after it: it wraps, so how many rows
        // it takes depends on the width, and the body cannot know its own
        // budget until that is settled. Each section budgeting for itself is
        // what drifted before, and left the footer written past the bottom.
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
            // What the next press does, not what is in force: `[i]dle`
            // alone said neither, and a reader coming from the pane beside
            // it would have read a state. What is in force is the count
            // line above, which says how many the filter is holding back.
            vec![(
                p.dim.as_str(),
                format!("[i]dle {}", if show_idle { "hide" } else { "show" }),
            )],
            // A column rather than rows, so no body line - the same call
            // `[t]stats` got - but the wording is the wording: a bare
            // `[l]abels` beside `[i]dle hide` is the same shape saying
            // nothing.
            vec![(
                p.dim.as_str(),
                format!("[l]abels {}", if show_labels { "hide" } else { "show" }),
            )],
            vec![(p.dim.as_str(), "[r]efresh".into())],
        ];
        hints.push(vec![(p.dim.as_str(), "[,] settings".into())]);
        hints.push(vec![(p.dim.as_str(), tc::PALETTE_HINT.into())]);
        hints.push(vec![(p.dim.as_str(), tc::THEME_HINT.into())]);
        hints.push(vec![(p.dim.as_str(), "[q]uit".into())]);
        let packed = tc::pack_hints_placed(&hints, w - 2, "  ");
        let footer: Vec<String> = packed.lines.iter().map(|l| format!(" {}", l)).collect();

        // One window over the three lists read as one, which is the order
        // `rows_now` is in and the order the keys walk. The cursor used to
        // be clamped against the whole list while each section stopped at a
        // row budget of its own, so on any pane too short for everything the
        // cursor walked past the last drawn row and vanished - and enter
        // still acted on whatever it was invisibly sitting on.
        let idle_listed = show_idle && !resting.is_empty();
        // In the order `rows_now` is in: agents, then the panes that are
        // doing something, then the ones at a prompt. A section with no
        // rows is not somewhere tab can land, so it is left out.
        sections = [
            (0, !agents.is_empty()),
            (agents.len(), !busy.is_empty()),
            (agents.len() + busy.len(), idle_listed),
        ]
        .into_iter()
        .filter_map(|(at, live)| live.then_some(at))
        .collect();
        // Every heading is drawn, always: AGENTS and its column head, a
        // blank and PROCESSES, and the same again for IDLE when there is an
        // idle section at all. OPS-34's rule survives that way rather than
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
            + 1; // the note line
        let room = h.saturating_sub(chrome).max(1);
        // A workspace's heading names it by its label, whatever `l` says
        // the rows show: the heading is where the label is for.
        let name_of = |workspace_id: &str| -> String {
            match labels.get(workspace_id) {
                Some(label) if !label.is_empty() => label.clone(),
                _ => workspace_id.to_string(),
            }
        };
        // An agent takes two rows, plus the gap or workspace heading above
        // it, and a pane takes one, in the order the keys walk them. The
        // rows above an agent count as its own, so the window keeps them
        // with the agent they introduce. On a pane with too little room for
        // an agent and what goes above it, the rows above are left off and
        // the agent takes its two, as it did before there were headings:
        // the window cannot split an entry, so an entry taller than the
        // room is admitted whole and the truncate below cuts the section
        // headings after it instead.
        let lead_fits = |lead: usize| 2 + lead <= room;
        let heights: Vec<usize> = (0..agents.len())
            .map(|i| {
                let lead = lead_rows(&agents, i, w, &name_of, &p, 0, &mut Vec::new()).len();
                let own = 2 + job_lines(&agents[i].work, w.saturating_sub(1), room).len();
                if lead_fits(lead) { own + lead } else { own }
            })
            .chain(std::iter::repeat(1).take(rows_now.len() - agents.len()))
            .collect();
        let window = window_over(&heights, selected, room, scroll, moved);
        scroll = window.start;
        moved = false;

        // Where a row points: the workspace's label when labels are shown
        // and it has one, else the bare id.
        let place_of = |workspace_id: &str, pane_id: &str| -> String {
            if show_labels {
                let label = labels.get(workspace_id).cloned().unwrap_or_default();
                if label.is_empty() {
                    workspace_id.to_string()
                } else {
                    label
                }
            } else {
                pane_id.to_string()
            }
        };
        let widest = |it: &mut dyn Iterator<Item = String>| -> usize {
            it.map(|t| tc::display_width(&t)).max().unwrap_or(0)
        };
        let cols = Columns::fit(
            w,
            wide,
            [
                widest(&mut agents.iter().map(|a| a.name.clone())),
                widest(&mut agents.iter().map(|a| place_of(&a.workspace_id, &a.pane_id))),
                widest(&mut busy.iter().map(|n| n.command.clone()))
                    .max(tc::display_width("could not be read")),
                widest(&mut busy.iter().map(|n| place_of(&n.workspace_id, &n.pane_id))),
                widest(&mut resting.iter().map(|n| homely(&n.cwd))),
                widest(
                    &mut resting
                        .iter()
                        .map(|n| place_of(&n.workspace_id, &n.pane_id)),
                ),
            ],
        );
        let Columns {
            name_w,
            agent_place_w,
            command_w,
            busy_place_w,
            dir_w,
            idle_place_w,
        } = cols;

        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── AGENTS ── ".into()),
                (p.dim.as_str(), format!("{}", agents.len())),
                (p.dim.as_str(), showing(&window, 0, agents.len())),
            ],
            w - 1,
        ));
        // AGENT sits over the mark as well as the name, hence the two.
        let mut head = format!(
            " {:<nw$} {:<8} {:<6} {:<5}",
            "AGENT",
            "STATE",
            "FOR",
            "CPU",
            nw = name_w + 2
        );
        if wide {
            head += &format!(" {:<5} {}", "MEM", "WORKSPACE");
        }
        rows.push(tc::seg(&[(p.dim.as_str(), tc::pad(&head, w - 1))], w - 1));

        for (i, a) in agents.iter().enumerate() {
            if !window.contains(&i) {
                continue;
            }
            let mut rule = Vec::new();
            let lead = lead_rows(&agents, i, w, &name_of, &p, rows.len(), &mut rule);
            if lead_fits(lead.len()) {
                rows.extend(lead);
                plots.extend(rule);
            }
            let from = rows.len();
            let here = i == selected;
            let colour = colour_of(&a.state, &p);
            // Blocked keeps a tint of its own even unselected: it is the one
            // row that has to be visible without being looked for. Done
            // does not - a green ground read as a second state beside the
            // one the row names.
            let loud = a.state == "blocked" || a.state == "done";
            let tint = if here {
                tc::tint(tc::Role::Slot)
            } else if a.state == "blocked" {
                tc::tint_toward(tc::Role::Red)
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
                    p.on_tint(colour)
                };
                format!("{}{}", tint, colour)
            };
            let name = tc::pad(&a.name, name_w);
            let state_cell = if loud {
                a.state.to_uppercase()
            } else {
                a.state.clone()
            };
            // `heat_on`, because this cell goes through the tint closure and
            // the plain ramp's hot end cannot be read on any of the three
            // tints above: 3.18 on the selection blue, 4.35 on blocked, 3.85
            // on done. A pane at 100% CPU is the row you selected it to read.
            let heat = match a.cpu {
                Some(v) if v > 0.0 => tc::heat_on((v / 100.0).min(1.0), !tint.is_empty()),
                _ => p.dim.clone(),
            };
            let mut line = vec![
                (
                    c(colour),
                    format!(
                        "{}{} {}",
                        if here { "▸" } else { " " },
                        mark_of(&a.state, tick),
                        name
                    ),
                ),
                (c(colour), format!(" {:<8}", state_cell)),
                (
                    c(&p.dim),
                    format!(
                        " {:<6}",
                        format!("{}{}", if a.exact { "" } else { "≥" }, ago(a.since))
                    ),
                ),
                (c(&heat), percent(a.cpu)),
            ];
            if wide {
                let place = place_of(&a.workspace_id, &a.pane_id);
                line.push((c(&p.dim), format!(" {}", mem(a.rss))));
                line.push((c(&p.accent), format!(" {}", tc::pad(&place, agent_place_w))));
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
                            if loud || here {
                                " ".repeat(w)
                            } else {
                                String::new()
                            },
                        ),
                    ],
                    w - 1,
                ));
            }
            // The window counts these rows in the agent's height, so
            // they are the same ones `job_lines` gave it.
            for (unread, line) in job_lines(&a.work, w.saturating_sub(1), room) {
                rows.push(tc::seg(
                    &[
                        (&c(if unread { &p.unknown } else { &p.working }), line),
                        (
                            &tint,
                            if loud || here {
                                " ".repeat(w)
                            } else {
                                String::new()
                            },
                        ),
                    ],
                    w - 1,
                ));
            }
            rows_at.extend((from..rows.len()).map(|row| (row, i)));
        }
        if agents.is_empty() && err.is_empty() {
            rows.push(tc::seg(
                &[(p.dim.as_str(), "   no agents running".into())],
                w - 1,
            ));
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
                        &format!(
                            " {:<cw$} {:<5} {:<5} {}",
                            "COMMAND",
                            "CPU",
                            "MEM",
                            "WORKSPACE",
                            cw = command_w + 1
                        ),
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
            let why = if n.why.is_empty() {
                "no reason given"
            } else {
                &n.why
            };
            rows.push(tc::seg(
                &[(p.unknown.as_str(), format!("   ⚠ {}", why))],
                w - 1,
            ));
        }
        for (j, n) in busy.iter().enumerate() {
            if !window.contains(&(agents.len() + j)) {
                continue;
            }
            let from = rows.len();
            let here = agents.len() + j == selected;
            let tint = if here {
                tc::tint(tc::Role::Slot)
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
                    p.on_tint(colour)
                };
                format!("{}{}", tint, colour)
            };
            // The lifted ramp, for the same reason as the agent table above:
            // a tinted row is where the number is least readable and most
            // looked at.
            let heat = match n.cpu {
                Some(v) if v > 0.0 => tc::heat_on((v / 100.0).min(1.0), !tint.is_empty()),
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
                        command_w,
                    ),
                ),
                (c(&heat), percent(n.cpu)),
            ];
            if wide {
                let place = place_of(&n.workspace_id, &n.pane_id);
                line.push((c(&p.dim), format!(" {}", mem(n.rss))));
                line.push((c(&p.accent), format!(" {}", tc::pad(&place, busy_place_w))));
            }
            if here {
                line.push((tint.clone(), " ".repeat(w)));
            }
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
            rows_at.extend((from..rows.len()).map(|row| (row, agents.len() + j)));
        }
        // True only when nothing was unread either: an empty section with a
        // pane the probe failed on is not a Herdr where everything rests.
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
        }

        // The idle section's heading is drawn whenever there is an idle
        // section at all, and it is never what gets cut - that was OPS-34:
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
                        format!(
                            "{} pane{} at a prompt",
                            resting.len(),
                            plural(resting.len())
                        ),
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
                let from = rows.len();
                let here = agents.len() + busy.len() + j == selected;
                let tint = if here {
                    tc::tint(tc::Role::Slot)
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
                        p.on_tint(colour)
                    };
                    format!("{}{}", tint, colour)
                };
                let place = place_of(&n.workspace_id, &n.pane_id);
                let mut line = vec![
                    (c(&p.idle_c), format!("{}▫ ", if here { "▸" } else { " " })),
                    (
                        c(&p.idle_c),
                        tc::pad(&tail_path(&homely(&n.cwd), dir_w), dir_w + 1),
                    ),
                    (c(&p.accent), tc::pad(&place, idle_place_w)),
                ];
                if here {
                    line.push((tint.clone(), " ".repeat(w)));
                }
                let refs: Vec<(&str, String)> =
                    line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                rows.push(tc::seg(&refs, w - 1));
                rows_at.extend((from..rows.len()).map(|row| (row, agents.len() + busy.len() + j)));
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
                    if *ok {
                        p.done.as_str()
                    } else {
                        p.blocked.as_str()
                    },
                    format!(" {}", text),
                )],
                w - 1,
            ),
            None => String::new(),
        });
        // Where the footer lands on the frame, after the padding that pushes
        // it to the bottom, and one column in because that is the indent
        // above. Registered every frame: the footer moves when the pane
        // resizes and wraps onto a second line when it narrows, and a
        // placement kept from an older frame sends whatever key used to be
        // under the pointer.
        placed = rows_at;
        let body_end = rows.len();
        let foot_top = body_end;
        rows.extend(footer);
        let plots: Vec<_> = plots.into_iter().filter_map(|p| p.in_viewport(0, 1, body_end.saturating_sub(2))).collect();
        tc::draw_plots(&rows, w, h, &plots);
        keyboard.footer_at(&packed, foot_top, 1);
        keyboard.wait(Duration::from_millis(250));
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// The filter `[i]` applies, named, or nothing when it is holding nothing.
///
/// Hiding the idle panes is a filter like any other and an unstated one
/// leaves a short list looking like a quiet Herdr. It is only a filter
/// while there is something behind it: with no pane at a prompt, hidden and
/// shown are the same screen, and a line saying `0 idle panes hidden` would
/// be a filter announcing itself for nothing.
fn idle_filter(show_idle: bool, resting: usize) -> Vec<String> {
    if show_idle || resting == 0 {
        return Vec::new();
    }
    vec![format!("{} idle pane{} hidden", resting, plural(resting))]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(pane: &str, workspace: &str, state: &str) -> Agent {
        Agent {
            name: "claude".into(),
            pane_id: pane.into(),
            tab_id: String::new(),
            workspace_id: workspace.into(),
            state: state.into(),
            title: String::new(),
            cwd: String::new(),
            since: 0.0,
            exact: true,
            cpu: None,
            rss: None,
            work: Default::default(),
        }
    }

    #[test]
    fn jobs_that_outgrow_the_window_end_in_a_count_of_the_rest() {
        let job = |label: &str| tc::agent_work::Task {
            pid: 1,
            label: label.into(),
            via: tc::agent_work::Via::Process,
        };
        let work = tc::agent_work::Work {
            tasks: vec![job("npm test"), job("cargo build"), job("tail -f x.log")],
            unread: None,
        };
        // Room for all three beside the agent's two rows.
        assert_eq!(job_lines(&work, 60, 5).len(), 3);
        // Room for two job rows: one job, then a row counting the other two.
        let lines = job_lines(&work, 60, 4);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].1.contains("npm test"));
        assert!(lines[1].1.contains("+2 more"));
    }

    #[test]
    fn agents_group_by_workspace_with_the_most_urgent_workspace_first() {
        // Already worst-first, as poll sorts them before grouping.
        let sorted = vec![
            agent("p1", "w2", "blocked"),
            agent("p2", "w1", "done"),
            agent("p3", "w2", "idle"),
            agent("p4", "w1", "working"),
            agent("p5", "w3", "done"),
        ];
        let order = ["w1".to_string(), "w2".to_string(), "w3".to_string()];
        let panes: Vec<String> = by_workspace(sorted, &order)
            .into_iter()
            .map(|a| a.pane_id)
            .collect();
        // w2 holds the blocked agent, so it leads; w1 and w3 tie on done
        // and keep Herdr's order. Inside each, the order it already had.
        assert_eq!(panes, ["p1", "p3", "p2", "p4", "p5"]);

        // Workspaces Herdr did not list come after the listed ones of the
        // same urgency, in the order they arrived.
        let sorted = vec![
            agent("p1", "x2", "idle"),
            agent("p2", "w1", "idle"),
            agent("p3", "x1", "idle"),
            agent("p4", "x2", "idle"),
        ];
        let order = ["w1".to_string()];
        let panes: Vec<String> = by_workspace(sorted, &order)
            .into_iter()
            .map(|a| a.pane_id)
            .collect();
        assert_eq!(panes, ["p2", "p1", "p4", "p3"]);
    }

    #[test]
    fn a_workspace_heading_only_when_there_are_workspaces_to_tell_apart() {
        let p = palette();
        let name = |id: &str| id.to_string();
        let one = vec![agent("p1", "w1", "idle"), agent("p2", "w1", "idle")];
        assert!(lead_rows(&one, 0, 80, &name, &p, 0, &mut Vec::new()).is_empty());
        // Between two agents of one workspace: a gap, and no line.
        assert_eq!(lead_rows(&one, 1, 80, &name, &p, 0, &mut Vec::new()), [""]);

        let two = vec![agent("p1", "w1", "idle"), agent("p2", "w2", "idle")];
        let mut plots = Vec::new();
        assert_eq!(lead_rows(&two, 0, 80, &name, &p, 0, &mut plots).len(), 1);
        let second = lead_rows(&two, 1, 80, &name, &p, 10, &mut plots);
        assert_eq!(second.len(), 2, "a gap, then the heading");
        assert!(second[1].contains("w2") && second[1].contains('─'));
        // One rule per heading, each placed on its heading's row.
        assert_eq!(plots.len(), 2);
    }

    #[test]
    fn watcher_panic_reason_is_visible_in_state() {
        let state = Arc::new(Mutex::new(State::default()));
        run_watcher_catching_panic(&state, || panic!("fixture watcher failure"));
        assert!(state
            .lock()
            .unwrap()
            .watcher_err
            .contains("fixture watcher failure"));
    }

    #[test]
    fn pane_inventory_renews_filters_only_when_ids_change() {
        let mut state = State::default();
        update_inventory(
            &mut state,
            Some(vec!["p1".into(), "p2".into()]),
            String::new(),
        );
        assert_eq!(state.inventory_generation, 1);
        update_inventory(
            &mut state,
            Some(vec!["p1".into(), "p2".into()]),
            String::new(),
        );
        assert_eq!(state.inventory_generation, 1);
        update_inventory(&mut state, Some(vec!["p2".into()]), String::new());
        assert_eq!(state.inventory_generation, 2);
        update_inventory(&mut state, None, "pane list failed".into());
        assert_eq!(state.inventory_generation, 2);
        assert_eq!(state.inventory_err, "pane list failed");
        assert_eq!(state.pane_ids, vec!["p2"]);
    }

    #[test]
    fn opscope_matches_name_or_full_command_without_hiding_other_processes() {
        for (name, argv, expected) in [
            ("opscope-widget", vec!["python", "monitor.py"], true),
            (
                "python",
                vec!["python", "/opt/opscope/widgets/monitor.py"],
                true,
            ),
            ("OpScOpE", vec![], true),
            ("sh", vec!["sh", "-c", "OpScOpE dashboard"], true),
            ("python", vec!["python", "monitor.py"], false),
            ("", vec![], false),
        ] {
            let info = serde_json::json!({"process_info": {"shell_pid": 1,
                "foreground_processes": [{"pid": 2, "name": name, "argv": argv}]}});
            let front = classify(&info);
            assert_eq!(front_is_opscope(&front), expected, "{name}");
            let mut filter = process_filter::Filter::default();
            assert_eq!(filter.visible(front_is_opscope(&front)), !expected);
            let mut selected = 4;
            filter.toggle(&mut selected);
            assert!(filter.visible(front_is_opscope(&front)));
            filter.toggle(&mut selected);
            assert_eq!(filter.visible(front_is_opscope(&front)), !expected);
        }
        assert!(!front_is_opscope(&Front::Prompt));
        assert!(!front_is_opscope(&Front::Unknown("unavailable".into())));
    }

    #[test]
    fn hiding_the_idle_panes_is_a_filter_the_pane_states() {
        assert!(idle_filter(true, 3).is_empty(), "nothing is hidden");
        assert!(
            idle_filter(false, 0).is_empty(),
            "a filter holding nothing back is not worth a row"
        );
        assert_eq!(idle_filter(false, 3), vec!["3 idle panes hidden"]);
        assert_eq!(idle_filter(false, 1), vec!["1 idle pane hidden"]);
        // And the row it produces says the count is a subset.
        let said = tc::filter_row(6, 9, &idle_filter(false, 3)).expect("a filter is on");
        assert_eq!(said, "6 of 9 shown · 3 idle panes hidden");
    }

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
                    let w = window_over(&heights, at, room, from, true);
                    assert!(
                        w.contains(&at),
                        "room={} at={} from={} gave {:?}",
                        room,
                        at,
                        from,
                        w
                    );
                    // And it fits, unless one entry alone is taller than the
                    // pane - in which case it is drawn anyway, because the
                    // alternative is drawing nothing.
                    let used: usize = heights[w.clone()].iter().sum();
                    assert!(
                        used <= room || w.len() == 1,
                        "room={} at={} from={} drew {} rows in {:?}",
                        room,
                        at,
                        from,
                        used,
                        w
                    );
                }
            }
        }
    }

    #[test]
    fn the_window_holds_still_while_the_cursor_moves_inside_it() {
        let heights = vec![1usize; 40];
        // Already on screen: the view does not jump under the reader.
        assert_eq!(window_over(&heights, 12, 10, 8, true), 8..18);
        assert_eq!(window_over(&heights, 8, 10, 8, true), 8..18);
        assert_eq!(window_over(&heights, 17, 10, 8, true), 8..18);
        // Off the bottom: it moves by exactly enough.
        assert_eq!(window_over(&heights, 18, 10, 8, true), 9..19);
        // Off the top: it moves to the cursor rather than past it.
        assert_eq!(window_over(&heights, 3, 10, 8, true), 3..13);
        // An empty list has no window at all.
        assert_eq!(window_over(&[], 0, 10, 0, true), 0..0);
    }

    #[test]
    fn the_wheel_moves_the_window_off_the_cursor_and_stops_at_the_end() {
        let heights = vec![1usize; 40];
        // Not chasing: the window sits where it was put, cursor or no cursor.
        assert_eq!(window_over(&heights, 0, 10, 8, false), 8..18);
        assert_eq!(window_over(&heights, 39, 10, 0, false), 0..10);
        // And it stops once the last entry is on screen rather than
        // scrolling into blank space below it.
        assert_eq!(window_over(&heights, 0, 10, 100, false), 30..40);
        // Two-row entries are counted in rows, not entries: ten rows holds
        // five of them.
        let tall = vec![2usize; 20];
        assert_eq!(window_over(&tall, 0, 10, 100, false), 15..20);
        // A last entry taller than the pane is still a legal start. The
        // loop that finds `last` cannot include it, so without the clamp
        // `from` at the end produced n..n and every row disappeared.
        let last_tall = vec![1usize, 1, 2];
        assert_eq!(window_over(&last_tall, 0, 1, 2, false), 2..3);
        assert_eq!(window_over(&last_tall, 0, 1, 100, false), 2..3);
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
        // OPS-34's rule, which this rewrite had to keep: dropping the idle
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
        let argv = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
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
        assert_eq!(
            tail_path("/home/w/projects/toys", 40),
            "/home/w/projects/toys"
        );
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
        // Five, matching the header. An absent reading used to take six,
        // which is what this caught.
        for value in [Some(0), Some(4096), Some(1_600_000_000), None] {
            assert_eq!(mem(value).chars().count(), 5, "{:?}", value);
        }
        assert_eq!(mem(Some(512)), " 512B");
        assert_eq!(mem(Some(1024 * 1024 * 3)), "   3M");
        assert_eq!(mem(None), "   --");
    }

    #[test]
    fn a_wide_pane_shows_the_whole_workspace_label() {
        // The place was cut at 18 cells however wide the pane: a label of
        // 40 on a 200-column pane lost half of itself beside blank space.
        let cols = Columns::fit(200, true, [12, 40, 30, 40, 50, 40]);
        assert_eq!((cols.name_w, cols.agent_place_w), (12, 40));
        assert_eq!((cols.command_w, cols.busy_place_w), (30, 40));
        assert_eq!((cols.dir_w, cols.idle_place_w), (50, 40));
    }

    #[test]
    fn no_row_grows_past_the_pane_it_was_fitted_to() {
        // Each row's fixed cells plus its text columns, against the width
        // they were fitted for, from the threshold up. Below the minimums
        // `seg` clips as it always did, so the check starts where they fit.
        let long = [60, 80, 60, 80, 90, 80];
        for w in 66..=240 {
            let c = Columns::fit(w, true, long);
            assert!(
                3 + c.name_w + 21 + 7 + c.agent_place_w <= w - 1,
                "agent row at {}",
                w
            );
            assert!(
                3 + c.command_w + 5 + 7 + c.busy_place_w <= w - 1,
                "process row at {}",
                w
            );
            assert!(
                3 + c.dir_w + 1 + c.idle_place_w <= w - 1,
                "idle row at {}",
                w
            );
        }
        // At the threshold the old widths still stand, so nothing jumps
        // when the pane crosses it.
        let at = Columns::fit(66, true, long);
        assert!(at.name_w >= 6 && at.agent_place_w >= 18 && at.command_w >= 20);
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
        let stat = tc::procfs::parse_pid_stat(&line).expect("a well-formed line should parse");
        assert_eq!(stat.utime + stat.stime, 400, "utime and stime are summed");
        assert_eq!(stat.rss_pages, 4096, "rss is in pages, turned into bytes by rss_bytes");
        // A truncated line has no fields to find and must not be guessed at.
        assert_eq!(tc::procfs::parse_pid_stat("42 (short) S 1 2 3"), None);
    }

    #[test]
    fn the_tab_focused_is_the_one_the_agent_is_on_now() {
        let listed = serde_json::json!({
            "agents": [
                {"pane_id": "w1:p1", "tab_id": "w1:t1"},
                {"pane_id": "w1:p2", "tab_id": "w1:t9"}
            ]
        });
        // The row still says the tab from the last poll. The list says the
        // agent has moved, and that is the tab the client has to follow.
        assert_eq!(tab_for_focus(Some(&listed), "w1:p2", "w1:t2"), "w1:t9");
        // A list that fails, or that no longer names the pane, has nothing
        // newer to offer. The poll's tab is what is left.
        assert_eq!(tab_for_focus(None, "w1:p2", "w1:t2"), "w1:t2");
        assert_eq!(tab_for_focus(Some(&listed), "w1:p9", "w1:t2"), "w1:t2");
        assert_eq!(
            tab_for_focus(Some(&serde_json::json!({})), "w1:p2", "w1:t2"),
            "w1:t2"
        );
        // Named, with no tab: that is an answer. Falling back to the cached
        // id would focus a tab the list says the agent is not on.
        let bare = serde_json::json!({"agents": [{"pane_id": "w1:p2"}]});
        assert_eq!(tab_for_focus(Some(&bare), "w1:p2", "w1:t2"), "");
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
        assert!(matches!(
            classify(&serde_json::json!({})),
            Front::Unknown(_)
        ));
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
