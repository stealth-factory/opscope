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

//! Everything running under a Luvus session, and who needs a human.
//!
//! A Luvus client rather than a general agent monitor. Every figure comes
//! from the session's own Universal Harness Protocol 1.0 answers, reached
//! through the `luvus` CLI: the pane inventory from `uhp snapshot`, the
//! agents from `agent list`, and the two things Herdr has no equivalent
//! for — claimable tasks and the file-path leases they hold.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use opscope_core as tc;

#[path = "parse.rs"]
mod parse;

use parse::{Absence, Agent, Lease, Pane, Snapshot, Task};

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "luvus-panes",
    section: "luvus_panes",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};

/// Seconds before a luvus command is given up on.
///
/// Bounded, because the socket on the other end can stop answering and an
/// unbounded wait would hold the poller there forever with the pane still
/// drawing whatever it last had.
const RUN_TIMEOUT: u64 = 15;

/// Run one luvus command against a named session and hand back its stdout.
fn luvus_text(session: &str, args: &[&str]) -> Result<String, String> {
    let mut argv = vec!["luvus", "--session", session];
    argv.extend_from_slice(args);
    tc::run(&argv, RUN_TIMEOUT)
}

/// Focus a pane. `pane focus` jumps to the pane's workspace and tab too,
/// so there is nothing for a second command to add.
fn focus_pane(session: &str, pane: &str) -> bool {
    luvus_text(session, &["pane", "focus", pane]).is_ok()
}

/// Everything one poll established, each source answering for itself.
///
/// Four `Result`s rather than one shared error, because a `task list` that
/// failed and a session with no tasks in it are opposite readings and the
/// screen has to be able to say which. A single error field would have made
/// the failed one draw as `0 tasks`.
struct State {
    snapshot: Result<Snapshot, String>,
    agents: Result<Vec<Agent>, String>,
    tasks: Result<Vec<Task>, String>,
    leases: Result<Vec<Lease>, String>,
    /// Set when there is no session to read at all, and which of the three
    /// reasons that is.
    absent: Option<Absence>,
    /// Why the poller stopped, when it did. A thread that dies takes its
    /// explanation with it and leaves a board that looks like an empty
    /// session, which is the one thing this widget must never do.
    err: String,
    /// False until the first poll has answered, so "nothing here" is not
    /// drawn over a session nobody has looked at yet.
    read: bool,
}

impl Default for State {
    fn default() -> Self {
        const WAITING: &str = "not read yet";
        State {
            snapshot: Err(WAITING.into()),
            agents: Err(WAITING.into()),
            tasks: Err(WAITING.into()),
            leases: Err(WAITING.into()),
            absent: None,
            err: String::new(),
            read: false,
        }
    }
}

/// What each agent's state was when we first saw it, so a duration can be
/// measured rather than guessed. UHP does not timestamp a state change.
#[derive(Default)]
struct Seen {
    since: HashMap<String, (String, f64, bool)>,
    first_poll: bool,
}

fn poll(state: &Arc<Mutex<State>>, seen: &mut Seen, session: &str) {
    // The snapshot answers first because it is also the liveness probe: if
    // there is no server, nothing else is worth asking and the reason is
    // the same for all four.
    let snapshot = match luvus_text(session, &["uhp", "snapshot"]) {
        Ok(text) => parse::parse_snapshot(&text),
        Err(why) => {
            let absent = parse::parse_failure(&why);
            if let Ok(mut guard) = state.lock() {
                // Every reading goes with the server. Left standing, the
                // pinned header would keep counting agents and panes over
                // the top of "no luvus server" and the rows behind it would
                // still answer to enter - a board that is a screenshot of a
                // session that has stopped existing.
                guard.snapshot = Err(why.clone());
                guard.agents = Err(why.clone());
                guard.tasks = Err(why.clone());
                guard.leases = Err(why);
                guard.absent = Some(absent);
                guard.read = true;
            }
            return;
        }
    };

    let agents = luvus_text(session, &["agent", "list", "--json"])
        .and_then(|text| parse::parse_agents(&text));
    let tasks =
        luvus_text(session, &["task", "list", "--json"]).and_then(|text| parse::parse_tasks(&text));
    let leases = luvus_text(session, &["lease", "list", "--json"])
        .and_then(|text| parse::parse_leases(&text));

    let at = tc::now();
    let agents = agents.map(|listed| {
        let mut listed: Vec<Agent> = listed;
        let mut live = HashSet::new();
        for agent in &mut listed {
            let key = duration_key(&agent.pane, &agent.name);
            let (since, exact) = measure_since(seen, &key, &agent.state, at);
            agent.since = since;
            agent.exact = exact;
            live.insert(key);
        }
        // A successful poll is the only moment it is safe to forget a
        // vanished agent: a failed one must not wipe history and then
        // hand the next success a fresh clock.
        keep_live(seen, &live);
        listed.sort_by(|a, b| {
            parse::rank_of(&a.state)
                .cmp(&parse::rank_of(&b.state))
                .then(b.since.total_cmp(&a.since))
        });
        listed
    });

    if let Ok(mut guard) = state.lock() {
        guard.snapshot = snapshot;
        guard.agents = agents;
        guard.tasks = tasks;
        guard.leases = leases;
        guard.absent = None;
        guard.read = true;
    }
    seen.first_poll = false;
}

/// Where a row points, so `↵` knows which pane to focus.
#[derive(Clone)]
enum Row {
    Agent(Agent),
    Task(Task),
    Lease(Lease),
    Pane(Pane),
}

impl Row {
    /// The pane this row is about, when it is about one. A task nobody has
    /// claimed names no pane, and `↵` says so rather than focusing
    /// something arbitrary.
    fn pane(&self) -> String {
        match self {
            Row::Agent(a) => a.pane.clone(),
            Row::Task(t) => t.pane.clone(),
            Row::Lease(l) => l.pane.clone(),
            Row::Pane(p) => p.pane_id.clone(),
        }
    }

    fn what(&self) -> String {
        match self {
            Row::Agent(a) => a.name.clone(),
            Row::Task(t) => t.id.clone(),
            Row::Lease(l) => l.id.clone(),
            Row::Pane(p) => p.command.clone(),
        }
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
    /// A colour to draw over the selected-row tint.
    ///
    /// `dim` measures 3.81 against `bg(38, 56, 76)`, under the 4.5 the
    /// repository asks for against the tint as well as the background.
    /// This is the same grey lifted until it clears — 4.94 — used *only*
    /// where a tint is on, so an untinted row is the colour it always was.
    /// The substitution happens inside the closure that composes the tint
    /// rather than at each call site, because most sites reach `dim`
    /// through a condition that has nothing to do with selection.
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
        "working" => tc::SPINNER[tick % tc::SPINNER.len()],
        "idle" => '·',
        _ => '?',
    }
}

/// Where the window over the body should start.
///
/// The body is built at whatever height it needs and the pane is a window
/// onto it, so this works in *drawn rows* rather than in entries — the
/// sections have headings and column heads between them and an agent takes
/// two rows where a pane takes one, so an entry count is not a row count
/// and a window measured in entries admits more rows than the pane has.
///
/// `want` is the row span of the selected entry. `chase` is false on a
/// frame the wheel moved the view rather than a key moving the cursor:
/// then `from` stands as given and the cursor may scroll out of sight. It
/// is still where `↵` acts, and the next arrow brings the window back.
fn window_from(total: usize, want: std::ops::Range<usize>, room: usize, from: usize, chase: bool) -> usize {
    let last = total.saturating_sub(room);
    let mut start = from.min(last);
    if chase {
        if want.start < start {
            start = want.start;
        } else if want.end > start + room {
            // An entry taller than the whole window still gets its top
            // shown, rather than its bottom with the name scrolled off.
            start = want.end.saturating_sub(room).min(want.start);
        }
    }
    start.min(last)
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// The one line that stands in for a section with nothing in it.
///
/// An empty reading and a failed one are different sentences, and this is
/// the only place either is written: zero tasks is the normal state of most
/// sessions and has to read as an answer, while a `task list` that did not
/// come back has to read as a question nobody answered.
fn empty_or_why<T>(reading: &Result<Vec<T>, String>, empty: &str) -> (String, bool) {
    match reading {
        Ok(_) => (format!("   {}", empty), false),
        Err(why) => (format!("   ⚠ could not be read — {}", why), true),
    }
}

/// Who this duration belongs to: the pane, and the agent in it.
///
/// Pane id alone is empty when `agent list` omits `pane`, and two agents
/// without one would then share a clock. A replacement in the same pane
/// keeps the old start time if only the pane is the key.
fn duration_key(pane: &str, name: &str) -> String {
    format!("{}\0{}", pane, name)
}

/// How long `state` has been held, measured from the first poll that saw it.
///
/// A state already in place when we started is only a lower bound — we
/// did not see it begin.
fn measure_since(seen: &mut Seen, key: &str, state: &str, at: f64) -> (f64, bool) {
    if seen.since.get(key).is_none_or(|(had, _, _)| *had != state) {
        seen.since
            .insert(key.to_string(), (state.to_string(), at, !seen.first_poll));
    }
    let (_, began, exact) = seen.since[key].clone();
    (at - began, exact)
}

fn keep_live(seen: &mut Seen, live: &HashSet<String>) {
    seen.since.retain(|k, _| live.contains(k));
}

/// The count a heading may print. A failed source has no number.
fn shown_count<T>(reading: &Result<Vec<T>, String>) -> String {
    match reading {
        Ok(rows) => rows.len().to_string(),
        Err(_) => "unread".into(),
    }
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    if !tc::dependencies_available(
        "luvus-panes",
        include_str!("dependencies.json"),
        Some(SETTINGS),
    ) {
        return;
    }
    // The section is spelled with an underscore while everything else about
    // this widget is hyphenated. A mismatched key is read as absent rather
    // than as an error, so it is worth saying out loud.
    let cfg = tc::load_config("luvus_panes");
    let session = tc::cfg_str(&cfg, "session", "default");
    let mut refresh = tc::poll_secs(tc::cfg_f64(&cfg, "refresh", 4.0), 4.0);
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() >= 2 && (args[0] == "-n" || args[0] == "--refresh") {
        refresh = tc::poll_secs(args[1].parse().unwrap_or(4.0), 4.0).max(1.0);
    }

    let p = palette();
    let state = Arc::new(Mutex::new(State::default()));
    let wake = Arc::new((Mutex::new(false), Condvar::new()));
    let poller = Arc::clone(&state);
    let poller_wake = Arc::clone(&wake);
    let polled_session = session.clone();
    std::thread::spawn(move || {
        let mut seen = Seen {
            first_poll: true,
            ..Default::default()
        };
        loop {
            // A poller that dies takes its explanation with it, and an
            // empty board looks exactly like a session with nothing in it.
            let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                poll(&poller, &mut seen, &polled_session)
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
    let mut show_idle = true;
    let (mut selected, mut tick) = (0usize, 0usize);
    // How far down the body, in drawn rows, the window has scrolled. Kept
    // across frames so the view holds still while the cursor moves inside
    // it, and only moves when the cursor would leave it.
    let mut scroll = 0usize;
    // Set by whichever key moved the cursor, and cleared once the window
    // has been asked to hold it. Without it the window chases the cursor
    // every frame and drags itself back from wherever the wheel put it.
    let mut moved = false;
    let mut note: Option<(String, bool, f64)> = None;
    let mut rows_now: Vec<Row> = Vec::new();
    // Where each section starts in `rows_now`, read as one flat list, with
    // the empty ones left out. Written by the frame and read by tab on the
    // frame after, which is the same one-frame lag `rows_now` already has.
    let mut sections: Vec<usize> = Vec::new();

    loop {
        tick += 1;
        for key in keyboard.poll() {
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "," => {
                    tc::run_settings(&mut keyboard, SETTINGS);
                    continue;
                }
                "r" | "R" => {
                    let (lock, cond) = &*wake;
                    if let Ok(mut asked) = lock.lock() {
                        *asked = true;
                        cond.notify_all();
                    }
                }
                "i" | "I" => {
                    show_idle = !show_idle;
                    selected = 0;
                    moved = true;
                }
                "up" | "k" | "K" => {
                    selected = selected.saturating_sub(1);
                    moved = true;
                }
                "down" | "j" | "J" => {
                    selected += 1;
                    moved = true;
                }
                // The wheel moves the view and nothing else. Selection is
                // the arrows' job, here as everywhere in the collection.
                "ctrl-y" | "wheel-up" => scroll = scroll.saturating_sub(1),
                "ctrl-e" | "wheel-down" => scroll = scroll.saturating_add(1),
                "pgup" => scroll = scroll.saturating_sub(tc::size().1.saturating_sub(4).max(1)),
                "pgdn" => scroll = scroll.saturating_add(tc::size().1.saturating_sub(4).max(1)),
                // Tab walks the section heads rather than the rows: with a
                // dozen panes listed, reaching LEASES with the arrows is a
                // lot of presses. Wraps, so it never dead-ends.
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
                        let pane = row.pane();
                        note = Some(if pane.is_empty() {
                            // A task nobody has claimed is in no pane, and
                            // focusing something else would be a lie about
                            // where the work is.
                            (
                                format!("! {} is not in a pane to jump to", row.what()),
                                false,
                                tc::now() + 3.0,
                            )
                        } else if focus_pane(&session, &pane) {
                            (
                                format!("→ focused pane {}", pane),
                                true,
                                tc::now() + 3.0,
                            )
                        } else {
                            (
                                format!("! could not focus pane {}", pane),
                                false,
                                tc::now() + 3.0,
                            )
                        });
                    }
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let (snapshot, agents, tasks, leases, absent, err, read) = match state.lock() {
            Ok(g) => (
                g.snapshot.clone(),
                g.agents.clone(),
                g.tasks.clone(),
                g.leases.clone(),
                g.absent.clone(),
                g.err.clone(),
                g.read,
            ),
            Err(_) => return,
        };
        let wide = w >= 76;

        // Rows for drawing. A failed source stays a failed source: these
        // vectors are empty, but the Result beside them is what the
        // header and the empty-section lines read, so a timeout cannot
        // draw as zero.
        let agent_rows: Vec<Agent> = agents.clone().unwrap_or_default();
        let task_rows: Vec<Task> = tasks.clone().unwrap_or_default();
        let lease_rows: Vec<Lease> = leases.clone().unwrap_or_default();
        let panes: Vec<Pane> = match &snapshot {
            Ok(s) => s.panes.clone(),
            Err(_) => Vec::new(),
        };
        // A pane holding a recognised agent is already in AGENTS with more
        // to say about it, so PANES is what is left. The join is by pane
        // id, which is what `agent list` and the snapshot agree on.
        let claimed: std::collections::HashSet<String> =
            agent_rows.iter().map(|a| a.pane.clone()).collect();
        let mut others: Vec<Pane> = panes
            .iter()
            .filter(|n| !claimed.contains(&n.pane_id))
            .cloned()
            .collect();
        // Unstated first, then busy, then the prompts: the row that wants
        // looking at is the one where luvus could not say.
        others.sort_by_key(|n| match n.status.as_str() {
            "" => 0,
            "idle" => 2,
            _ => 1,
        });
        let busy: Vec<Pane> = others.iter().filter(|n| n.status != "idle").cloned().collect();
        let resting: Vec<Pane> = others.iter().filter(|n| n.status == "idle").cloned().collect();
        let unstated = busy.iter().filter(|n| n.status.is_empty()).count();
        let running = busy.len() - unstated;

        rows_now = agent_rows
            .iter()
            .cloned()
            .map(Row::Agent)
            .chain(task_rows.iter().cloned().map(Row::Task))
            .chain(lease_rows.iter().cloned().map(Row::Lease))
            .chain(busy.iter().cloned().map(Row::Pane))
            .chain(
                resting
                    .iter()
                    .filter(|_| show_idle)
                    .cloned()
                    .map(Row::Pane),
            )
            .collect();
        if !rows_now.is_empty() && selected >= rows_now.len() {
            selected = rows_now.len() - 1;
        }
        if note.as_ref().is_some_and(|(_, _, until)| tc::now() >= *until) {
            note = None;
        }

        let idle_listed = show_idle && !resting.is_empty();
        sections = [
            (0, !agent_rows.is_empty()),
            (agent_rows.len(), !task_rows.is_empty()),
            (agent_rows.len() + task_rows.len(), !lease_rows.is_empty()),
            (
                agent_rows.len() + task_rows.len() + lease_rows.len(),
                !busy.is_empty(),
            ),
            (
                agent_rows.len() + task_rows.len() + lease_rows.len() + busy.len(),
                idle_listed,
            ),
        ]
        .into_iter()
        .filter_map(|(at, live)| live.then_some(at))
        .collect();

        // ---- the pinned header ----
        let mut head = vec![tc::title("luvus panes", w, &p.accent)];
        let mut counts: HashMap<&str, usize> = HashMap::new();
        if let Ok(listed) = &agents {
            for a in listed {
                *counts.entry(a.state.as_str()).or_insert(0) += 1;
            }
        }
        let mut summary = Vec::new();
        match &agents {
            Ok(listed) => summary.push((
                p.dim.as_str(),
                format!(" {} agent{}", listed.len(), plural(listed.len())),
            )),
            Err(_) => summary.push((p.unknown.as_str(), " agents unread".into())),
        }
        summary.push((
            p.dim.as_str(),
            format!(" · {} pane{}", panes.len(), plural(panes.len())),
        ));
        for state_name in ["blocked", "done", "working", "idle"] {
            if let Some(n) = counts.get(state_name) {
                summary.push((colour_of(state_name, &p), format!("   {} {}", n, state_name)));
            }
        }
        head.push(tc::seg(&summary, w.saturating_sub(1)));
        // The session line is the provenance: which server answered, which
        // protocol it speaks, and how far its event sequence has got. On a
        // machine with more than one session it is the first thing to check.
        head.push(match &snapshot {
            Ok(s) => tc::seg(
                &[
                    (
                        p.dim.as_str(),
                        format!(
                            " session {} · uhp {} · {} workspace{} · seq {}",
                            s.session,
                            s.protocol,
                            s.workspaces,
                            plural(s.workspaces),
                            s.sequence
                        ),
                    ),
                    // Which pane the session itself is focused on, so the
                    // reader can tell the row they are standing in from the
                    // rows they would have to jump to.
                    (
                        p.dim.as_str(),
                        match s.panes.iter().find(|n| n.focused) {
                            Some(n) => format!(" · focused on pane {}", n.pane_id),
                            None => String::new(),
                        },
                    ),
                ],
                w.saturating_sub(1),
            ),
            Err(_) => tc::seg(
                &[(p.dim.as_str(), format!(" session {}", session))],
                w.saturating_sub(1),
            ),
        });
        if !err.is_empty() {
            head.push(tc::seg(
                &[(p.blocked.as_str(), format!(" ! {}", err))],
                w.saturating_sub(1),
            ));
        }
        let wants =
            counts.get("blocked").copied().unwrap_or(0) + counts.get("done").copied().unwrap_or(0);
        head.push(if agents.is_err() {
            tc::seg(
                &[(
                    p.unknown.as_str(),
                    " cannot say who is waiting — agents unread".into(),
                )],
                w.saturating_sub(1),
            )
        } else if wants > 0 {
            tc::seg(
                &[(
                    if counts.contains_key("blocked") {
                        p.blocked.as_str()
                    } else {
                        p.done.as_str()
                    },
                    format!(" ▸ {} agent{} waiting for you", wants, plural(wants)),
                )],
                w.saturating_sub(1),
            )
        } else {
            tc::seg(
                &[(p.dim.as_str(), " nothing waiting on you".into())],
                w.saturating_sub(1),
            )
        });
        head.push(String::new());

        // ---- the footer, built before the body ----
        // It wraps, so how many rows it takes depends on the width, and the
        // body cannot know its own budget until that is settled.
        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![
                (p.accent.as_str(), "↑↓".into()),
                (p.dim.as_str(), " select".into()),
            ],
            vec![
                (p.accent.as_str(), "↵".into()),
                (p.dim.as_str(), " focus".into()),
            ],
            vec![
                (p.accent.as_str(), "tab".into()),
                (p.dim.as_str(), " section".into()),
            ],
            vec![(p.dim.as_str(), "[i]dle".into())],
            vec![(p.dim.as_str(), "[r]efresh".into())],
            vec![(p.dim.as_str(), "[,] settings".into())],
            vec![(p.dim.as_str(), "[q]uit".into())],
        ];
        let footer: Vec<String> = tc::pack_hints(&hints, w.saturating_sub(2), "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();

        // ---- the body, built at whatever height it needs ----
        let mut body: Vec<String> = Vec::new();
        // Where each entry's rows sit in `body`, so the window can hold the
        // selected one without counting rows a second time and disagreeing.
        let mut spans: Vec<std::ops::Range<usize>> = Vec::new();

        if let Some(why) = &absent {
            // One of three sentences, never a blank board. The distinction
            // is the whole reason this widget declares luvus a dependency
            // and probes at run time as well.
            let (mark, said, next) = match why {
                Absence::NoBinary => (
                    "no luvus on PATH",
                    "the luvus command went away while this was running".to_string(),
                    "install Luvus from https://luvus.dev".to_string(),
                ),
                Absence::NoServer => (
                    "no luvus server",
                    format!("nothing is serving the session named {}", session),
                    "start one with `luvus`, or name another session with `,`".to_string(),
                ),
                Absence::Other(message) => (
                    "luvus could not be read",
                    message.clone(),
                    "the reason above is what luvus itself said".to_string(),
                ),
            };
            body.push(tc::seg(
                &[(p.blocked.as_str(), format!(" ⚠ {}", mark))],
                w.saturating_sub(1),
            ));
            body.push(tc::seg(
                &[(p.txt.as_str(), format!("   {}", said))],
                w.saturating_sub(1),
            ));
            body.push(tc::seg(
                &[(p.dim.as_str(), format!("   {}", next))],
                w.saturating_sub(1),
            ));
        } else if !read {
            body.push(tc::seg(
                &[(p.dim.as_str(), "   reading the session…".into())],
                w.saturating_sub(1),
            ));
        } else {
            let selected_row = selected;
            let mut at = 0usize; // entry index across all five sections

            // The third of the three absences, and the one that has no
            // error behind it: the server answered, and there is nothing
            // coordinated under it and no pane doing work. Idle shells at
            // a prompt are not that work — they are listed under IDLE —
            // but a busy non-agent pane is, and calling the session empty
            // while PANES names it would be two readings of the same screen.
            let nothing_under_it = snapshot.is_ok()
                && agents.as_ref().is_ok_and(|a| a.is_empty())
                && tasks.as_ref().is_ok_and(|t| t.is_empty())
                && leases.as_ref().is_ok_and(|l| l.is_empty())
                && busy.is_empty();
            if nothing_under_it {
                let (said, next) = if panes.is_empty() {
                    (
                        "the server answered; it holds no workspaces yet".to_string(),
                        "open one with `luvus workspace open <path>`".to_string(),
                    )
                } else {
                    (
                        format!(
                            "the server answered; {} pane{}, and no agents, tasks or leases",
                            panes.len(),
                            plural(panes.len())
                        ),
                        "start one with `luvus agent start`".to_string(),
                    )
                };
                body.push(tc::seg(
                    &[(
                        p.working.as_str(),
                        format!(" ▪ session {} is open and empty", session),
                    )],
                    w.saturating_sub(1),
                ));
                body.push(tc::seg(
                    &[(p.dim.as_str(), format!("   {}", said))],
                    w.saturating_sub(1),
                ));
                body.push(tc::seg(
                    &[(p.dim.as_str(), format!("   {}", next))],
                    w.saturating_sub(1),
                ));
                body.push(String::new());
            }

            // ---- AGENTS ----
            body.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── AGENTS ── ".into()),
                    (p.dim.as_str(), shown_count(&agents)),
                ],
                w.saturating_sub(1),
            ));
            let name_w = agent_rows
                .iter()
                .map(|a| tc::display_width(&a.name))
                .max()
                .unwrap_or(5)
                .max(8);
            let mut columns = format!(
                " {:<name_w$} {:<8} {:<6} {:<14}",
                "AGENT", "STATE", "FOR", "WORKSPACE",
                name_w = name_w
            );
            if wide {
                columns += " BRANCH";
            }
            body.push(tc::seg(
                &[(p.dim.as_str(), tc::pad(&columns, w.saturating_sub(1)))],
                w.saturating_sub(1),
            ));
            for a in &agent_rows {
                let here = at == selected_row;
                let start = body.len();
                let colour = colour_of(&a.state, &p);
                // Blocked and done keep a tint of their own even unselected:
                // the whole point is that they are visible without being
                // looked for.
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
                    // Any colour that would not clear AA on this tint is
                    // swapped for its lighter twin, inside the closure
                    // rather than at each call site.
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
                let state_cell = if loud {
                    a.state.to_uppercase()
                } else {
                    a.state.clone()
                };
                let mut line = vec![
                    (
                        c(colour),
                        format!(
                            "{}{} {}",
                            if here { "▸" } else { " " },
                            mark_of(&a.state, tick),
                            tc::pad(&a.name, name_w)
                        ),
                    ),
                    (c(colour), format!(" {:<8}", state_cell)),
                    (
                        c(&p.dim),
                        format!(
                            " {:<6}",
                            format!("{}{}", if a.exact { "" } else { "≥" }, parse::ago(a.since))
                        ),
                    ),
                    (c(&p.accent), format!(" {}", tc::pad(&a.workspace, 14))),
                ];
                if wide {
                    let branch = if a.branch.is_empty() {
                        a.project.clone()
                    } else {
                        a.branch.clone()
                    };
                    line.push((
                        c(&p.dim),
                        format!(" {}{}", branch, if a.worktree { " ⑂" } else { "" }),
                    ));
                }
                if loud || here {
                    line.push((tint.clone(), " ".repeat(w)));
                }
                let refs: Vec<(&str, String)> =
                    line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                body.push(tc::seg(&refs, w.saturating_sub(1)));
                // The second row carries where the agent is and how much
                // the state above is worth: a state read off screen text is
                // a weaker claim than one an integration reported, and the
                // screen must not flatten the two into one word.
                let mut detail = vec![
                    (
                        c(if loud || here { &p.txt } else { &p.dim }),
                        format!("   {}  ", parse::tail_path(&parse::homely(&a.cwd), 34)),
                    ),
                    (
                        c(&p.dim),
                        format!(
                            "state via {}",
                            if a.state_source.is_empty() {
                                "—"
                            } else {
                                &a.state_source
                            }
                        ),
                    ),
                ];
                if wide {
                    detail.push((
                        c(&p.dim),
                        format!(
                            " · id via {}",
                            if a.authority.is_empty() {
                                "—"
                            } else {
                                &a.authority
                            }
                        ),
                    ));
                }
                if loud || here {
                    detail.push((tint.clone(), " ".repeat(w)));
                }
                let refs: Vec<(&str, String)> =
                    detail.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                body.push(tc::seg(&refs, w.saturating_sub(1)));
                spans.push(start..body.len());
                at += 1;
            }
            if agent_rows.is_empty() {
                let (said, bad) = empty_or_why(&agents, "no agents under this session");
                body.push(tc::seg(
                    &[(
                        if bad { p.unknown.as_str() } else { p.dim.as_str() },
                        said,
                    )],
                    w.saturating_sub(1),
                ));
            }

            // ---- TASKS ----
            body.push(String::new());
            body.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── TASKS ── ".into()),
                    (p.dim.as_str(), shown_count(&tasks)),
                ],
                w.saturating_sub(1),
            ));
            for t in &task_rows {
                let here = at == selected_row;
                let start = body.len();
                let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
                let c = |colour: &str| {
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
                let mut line = vec![
                    (c(&p.accent), format!("{}◆ ", if here { "▸" } else { " " })),
                    (c(&p.dim), tc::pad(&t.id, 10)),
                    (c(&p.working), format!(" {}", tc::pad(&t.status, 10))),
                    (c(&p.txt), format!(" {}", t.title)),
                ];
                if wide && !t.holder.is_empty() {
                    line.push((c(&p.dim), format!("  ({})", t.holder)));
                }
                if here {
                    line.push((tint.clone(), " ".repeat(w)));
                }
                let refs: Vec<(&str, String)> =
                    line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                body.push(tc::seg(&refs, w.saturating_sub(1)));
                spans.push(start..body.len());
                at += 1;
            }
            if task_rows.is_empty() {
                // Zero tasks is the normal state of most sessions and has
                // to read as an answer, not as a source that failed.
                let (said, bad) = empty_or_why(&tasks, "no tasks — nothing is being coordinated");
                body.push(tc::seg(
                    &[(
                        if bad { p.unknown.as_str() } else { p.dim.as_str() },
                        said,
                    )],
                    w.saturating_sub(1),
                ));
            }

            // ---- LEASES ----
            body.push(String::new());
            body.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── LEASES ── ".into()),
                    (p.dim.as_str(), shown_count(&leases)),
                ],
                w.saturating_sub(1),
            ));
            for l in &lease_rows {
                let here = at == selected_row;
                let start = body.len();
                let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
                let c = |colour: &str| {
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
                let mut line = vec![
                    (c(&p.accent), format!("{}⛒ ", if here { "▸" } else { " " })),
                    (c(&p.dim), tc::pad(&l.task, 10)),
                    (c(&p.txt), format!(" {}", l.paths.join(" "))),
                ];
                if wide && !l.holder.is_empty() {
                    line.push((c(&p.dim), format!("  ({})", l.holder)));
                }
                if here {
                    line.push((tint.clone(), " ".repeat(w)));
                }
                let refs: Vec<(&str, String)> =
                    line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                body.push(tc::seg(&refs, w.saturating_sub(1)));
                spans.push(start..body.len());
                at += 1;
            }
            if lease_rows.is_empty() {
                let (said, bad) = empty_or_why(&leases, "no leases — no paths are reserved");
                body.push(tc::seg(
                    &[(
                        if bad { p.unknown.as_str() } else { p.dim.as_str() },
                        said,
                    )],
                    w.saturating_sub(1),
                ));
            }

            // ---- PANES ----
            body.push(String::new());
            let mut heading = vec![
                (p.lbl.as_str(), " ── PANES ── ".into()),
                (
                    p.dim.as_str(),
                    format!("{} pane{} not at a prompt", running, plural(running)),
                ),
            ];
            if unstated > 0 {
                heading.push((
                    p.unknown.as_str(),
                    format!("  · {} luvus reports no state for", unstated),
                ));
            }
            body.push(tc::seg(&heading, w.saturating_sub(1)));
            // A column head over no rows names columns that are not there.
            // The heading and its count still stand, because those are the
            // reading; the head is only a label for rows.
            if wide && !busy.is_empty() {
                body.push(tc::seg(
                    &[(
                        p.dim.as_str(),
                        tc::pad(
                            &format!(" {:<12} {:<8} {:<14} {:<20}", "IN THE PANE", "STATE", "WORKSPACE", "DIRECTORY"),
                            w.saturating_sub(1),
                        ),
                    )],
                    w.saturating_sub(1),
                ));
            }
            for n in &busy {
                let here = at == selected_row;
                let start = body.len();
                let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
                let c = |colour: &str| {
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
                let unstated_here = n.status.is_empty();
                let mut line = vec![
                    (
                        c(if unstated_here { &p.unknown } else { &p.proc }),
                        format!(
                            "{}{} ",
                            if here { "▸" } else { " " },
                            if unstated_here { '⚠' } else { '▪' }
                        ),
                    ),
                    (
                        c(if unstated_here { &p.unknown } else { &p.txt }),
                        tc::pad(
                            if n.command.is_empty() {
                                "?"
                            } else {
                                &n.command
                            },
                            12,
                        ),
                    ),
                    (
                        c(colour_of(&n.status, &p)),
                        format!(
                            " {}",
                            tc::pad(
                                if unstated_here { "no state" } else { &n.status },
                                8
                            )
                        ),
                    ),
                    (c(&p.accent), format!(" {}", tc::pad(&n.workspace, 14))),
                ];
                if wide {
                    line.push((
                        c(&p.dim),
                        format!(" {}", parse::homely(&n.cwd)),
                    ));
                }
                if here {
                    line.push((tint.clone(), " ".repeat(w)));
                }
                let refs: Vec<(&str, String)> =
                    line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                body.push(tc::seg(&refs, w.saturating_sub(1)));
                spans.push(start..body.len());
                at += 1;
            }
            if busy.is_empty() {
                // Two different readings share this row: a session whose
                // every pane is resting, and a session with no panes at
                // all. Saying the first about the second would be a claim
                // about panes that do not exist.
                let (said, bad) = empty_or_why(
                    &snapshot.clone().map(|s| s.panes),
                    if panes.is_empty() {
                        "no panes in this session"
                    } else {
                        "every other pane is idle at a prompt"
                    },
                );
                body.push(tc::seg(
                    &[(
                        if bad { p.unknown.as_str() } else { p.dim.as_str() },
                        said,
                    )],
                    w.saturating_sub(1),
                ));
            }

            // ---- IDLE ----
            if idle_listed {
                body.push(String::new());
                body.push(tc::seg(
                    &[
                        (p.lbl.as_str(), " ── IDLE ── ".into()),
                        (
                            p.dim.as_str(),
                            format!("{} pane{} at a prompt", resting.len(), plural(resting.len())),
                        ),
                    ],
                    w.saturating_sub(1),
                ));
                for n in &resting {
                    let here = at == selected_row;
                    let start = body.len();
                    let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
                    let c = |colour: &str| {
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
                    let mut line = vec![
                        (c(&p.idle_c), format!("{}▫ ", if here { "▸" } else { " " })),
                        (
                            c(&p.idle_c),
                            tc::pad(&parse::tail_path(&parse::homely(&n.cwd), 30), 31),
                        ),
                        (c(&p.accent), tc::pad(&n.workspace, 14)),
                    ];
                    if here {
                        line.push((tint.clone(), " ".repeat(w)));
                    }
                    let refs: Vec<(&str, String)> =
                        line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                    body.push(tc::seg(&refs, w.saturating_sub(1)));
                    spans.push(start..body.len());
                    at += 1;
                }
            }
        }

        // ---- the window onto it ----
        // The header is pinned, but not at the price of the body: on a pane
        // eight rows tall a five-row header and a four-row footer leave
        // nothing, and a body with no rows in it reads as a session with
        // nothing in it. So the header gives its last lines back, from the
        // bottom, until there is something to scroll. The title and the
        // counts never go: those two are the widget.
        let mut head = head;
        const FLOOR: usize = 3;
        while head.len() > 2 && h.saturating_sub(head.len() + footer.len() + 1) < FLOOR {
            head.pop();
        }
        let reserve = head.len() + footer.len() + 1; // +1 for the note line
        let room = h.saturating_sub(reserve).max(1);
        let want = spans
            .get(selected.min(spans.len().saturating_sub(1)))
            .cloned()
            .unwrap_or(0..0);
        let start = window_from(body.len(), want, room, scroll, moved);
        scroll = start;
        moved = false;
        let window = start..(start + room).min(body.len());

        // The headings are drawn inside the body and scroll with it, so the
        // count of what is on screen is added after the window is known.
        let mut rows = head;
        rows.extend(body[window.clone()].iter().cloned());
        // Said in the footer note line rather than beside every heading,
        // because the headings have already been rendered. What matters is
        // that the reader can tell a section scrolled past from one that is
        // empty, and the headings themselves scroll with their rows.
        while rows.len() < h.saturating_sub(footer.len() + 1) {
            rows.push(String::new());
        }
        rows.truncate(h.saturating_sub(footer.len() + 1));
        rows.push(match note.as_ref() {
            Some((text, ok, _)) => tc::seg(
                &[(
                    if *ok { p.done.as_str() } else { p.blocked.as_str() },
                    format!(" {}", text),
                )],
                w.saturating_sub(1),
            ),
            None if body.len() > room => tc::seg(
                &[(
                    p.dim.as_str(),
                    format!(
                        " rows {}-{} of {}",
                        window.start + 1,
                        window.end,
                        body.len()
                    ),
                )],
                w.saturating_sub(1),
            ),
            None => String::new(),
        });
        rows.extend(footer);
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_holds_the_selected_entry_whatever_it_costs() {
        // A body of 200 rows and an entry two rows tall anywhere in it: the
        // window has to contain both of its rows on a frame a key moved the
        // cursor, at every room and from wherever the view last sat.
        for room in [1usize, 2, 5, 17, 400] {
            for top in (0..199).step_by(7) {
                for from in [0usize, 3, 90, 199, 500] {
                    let want = top..top + 2;
                    let start = window_from(200, want.clone(), room, from, true);
                    let end = (start + room).min(200);
                    assert!(
                        start <= want.start && (want.end <= end || room < 2),
                        "room={} want={:?} from={} gave {}..{}",
                        room,
                        want,
                        from,
                        start,
                        end
                    );
                }
            }
        }
    }

    #[test]
    fn the_wheel_leaves_the_cursor_where_it_was() {
        // chase=false: the view stands where the wheel put it even when the
        // selected entry is nowhere near it. Clamped to the last full
        // window, so it never scrolls into blank space.
        assert_eq!(window_from(100, 0..2, 10, 40, false), 40);
        assert_eq!(window_from(100, 0..2, 10, 200, false), 90);
        // And the next arrow press brings it straight back.
        assert_eq!(window_from(100, 0..2, 10, 40, true), 0);
    }

    #[test]
    fn the_window_holds_still_while_the_cursor_moves_inside_it() {
        assert_eq!(window_from(100, 12..13, 10, 8, true), 8);
        assert_eq!(window_from(100, 8..9, 10, 8, true), 8);
        assert_eq!(window_from(100, 17..18, 10, 8, true), 8);
        // Off the bottom by one: it moves by exactly one.
        assert_eq!(window_from(100, 18..19, 10, 8, true), 9);
        // Off the top: it moves to the cursor rather than past it.
        assert_eq!(window_from(100, 3..4, 10, 8, true), 3);
    }

    #[test]
    fn a_body_shorter_than_the_pane_starts_at_the_top() {
        assert_eq!(window_from(4, 0..1, 20, 0, true), 0);
        assert_eq!(window_from(4, 0..1, 20, 9, false), 0);
        assert_eq!(window_from(0, 0..0, 20, 5, true), 0);
    }

    #[test]
    fn an_empty_reading_and_a_failed_one_are_different_sentences() {
        let empty: Result<Vec<u8>, String> = Ok(Vec::new());
        let (said, bad) = empty_or_why(&empty, "no tasks");
        assert!(said.contains("no tasks"));
        assert!(!bad);
        let failed: Result<Vec<u8>, String> = Err("luvus did not answer in 15s".into());
        let (said, bad) = empty_or_why(&failed, "no tasks");
        assert!(said.contains("could not be read"));
        assert!(said.contains("did not answer"));
        assert!(!said.contains("no tasks"));
        assert!(bad);
    }

    #[test]
    fn a_failed_source_has_no_count_to_print() {
        let empty: Result<Vec<u8>, String> = Ok(Vec::new());
        assert_eq!(shown_count(&empty), "0");
        let listed: Result<Vec<u8>, String> = Ok(vec![1, 2]);
        assert_eq!(shown_count(&listed), "2");
        let failed: Result<Vec<u8>, String> = Err("timeout".into());
        assert_eq!(shown_count(&failed), "unread");
        assert_ne!(shown_count(&failed), "0");
    }

    #[test]
    fn duration_follows_the_agent_not_only_the_pane() {
        let mut seen = Seen {
            first_poll: false,
            ..Default::default()
        };
        let a = duration_key("2", "claude");
        let b = duration_key("2", "codex");
        let empty_one = duration_key("", "claude");
        let empty_two = duration_key("", "codex");
        let (first, exact) = measure_since(&mut seen, &a, "working", 10.0);
        assert_eq!(first, 0.0);
        assert!(exact);
        let (later, _) = measure_since(&mut seen, &a, "working", 18.0);
        assert_eq!(later, 8.0);
        // A different agent in the same pane starts its own clock.
        let (other, _) = measure_since(&mut seen, &b, "working", 18.0);
        assert_eq!(other, 0.0);
        // Agents whose pane id never arrived stay distinct from each other.
        measure_since(&mut seen, &empty_one, "idle", 20.0);
        measure_since(&mut seen, &empty_two, "working", 20.0);
        let (still, _) = measure_since(&mut seen, &empty_one, "idle", 25.0);
        assert_eq!(still, 5.0);
        // A vanished agent is forgotten, so a later return is not hours old.
        let mut live = HashSet::new();
        live.insert(b.clone());
        keep_live(&mut seen, &live);
        let (returned, _) = measure_since(&mut seen, &a, "working", 100.0);
        assert_eq!(returned, 0.0);
    }

}
