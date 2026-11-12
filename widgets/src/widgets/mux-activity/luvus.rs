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

//! The Luvus backend of `mux-activity`, which was luvus-panes.
//!
//! Everything running under a Luvus session, and who needs a human.
//!
//! A Luvus client rather than a general agent monitor. Every figure comes
//! from the session's own Universal Harness Protocol 1.0 answers, reached
//! through the `luvus` CLI: the pane inventory from `uhp snapshot`, the
//! agents from `agent list`, and the two things Herdr has no equivalent
//! for — claimable tasks and the file-path leases they hold.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opscope_core as tc;

use super::luvus_events as events;
use super::luvus_parse as parse;
use super::{process_filter, refresh_wake};

use parse::{
    Absence, Agent, Explanation, GitState, Lease, NextTask, Pane, Resumable, Snapshot, Task,
    Worktree,
};


/// Seconds before a luvus command is given up on.
///
/// Bounded, because the socket on the other end can stop answering and an
/// unbounded wait would hold the poller there forever with the pane still
/// drawing whatever it last had.
const RUN_TIMEOUT: u64 = 15;
const EVENT_WAIT_GRACE: u64 = 3;

// Test stand-in for `luvus_text`. A cursor written by hand never reaches
// the failure path, and there is no live session to fail against. This
// stays unset outside tests. It cannot sit behind `cfg(test)`: the widget
// checks keep only the source before the first one, so an early gate would
// hide the rest of this file from them.
thread_local! {
    static LUVUS_TEXT_OVERRIDE: std::cell::RefCell<
        Option<fn(&str, &[&str]) -> Result<String, String>>,
    > = const { std::cell::RefCell::new(None) };
}

/// Run one luvus command against a named session and hand back its stdout.
fn luvus_text(session: &str, args: &[&str]) -> Result<String, String> {
    if let Some(answer) =
        LUVUS_TEXT_OVERRIDE.with(|slot| slot.borrow().map(|hook| hook(session, args)))
    {
        return answer;
    }
    let mut argv = vec!["luvus", "--session", session];
    argv.extend_from_slice(args);
    let out = tc::run_full(&argv, RUN_TIMEOUT).map_err(|why| parse::sanitize_error(&why))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        return Ok(stdout);
    }
    // Luvus puts its reason in a JSON error on stdout and exits 1; the exit
    // status alone said "luvus exited exit status: 1" and nothing else.
    let why = parse::parse_command_error(&stdout, &String::from_utf8_lossy(&out.stderr))
        .unwrap_or_else(|| format!("luvus exited {}", out.status));
    Err(parse::sanitize_error(&why))
}

/// A list command's JSON answer, on whichever Luvus is installed.
///
/// Luvus 0.14 prints JSON on its own and refuses `--json` on `agent list`
/// ("unexpected agent list argument --json"); earlier releases printed a
/// table unless asked for JSON. So ask plainly first, and only when the
/// answer is not a JSON object ask again with `--json`. Current Luvus costs
/// one call, an older one two, and neither is told it is wrong.
fn luvus_json(session: &str, args: &[&str]) -> Result<String, String> {
    let plain = luvus_text(session, args);
    if plain.as_deref().is_ok_and(|text| text.trim_start().starts_with('{')) {
        return plain;
    }
    let mut flagged = args.to_vec();
    flagged.push("--json");
    match luvus_text(session, &flagged) {
        Ok(text) => Ok(text),
        // The flag was not the answer either: say what the plain call
        // said, since that is the form current Luvus expects.
        Err(_) if plain.is_err() => plain,
        Err(why) => Err(why),
    }
}

/// Focus a pane. `pane focus` jumps to the pane's workspace and tab too,
/// so there is nothing for a second command to add.
fn focus_pane(session: &str, pane: &str) -> bool {
    luvus_text(session, &["pane", "focus", pane]).is_ok()
}

fn local_processes() -> Result<String, String> {
    tc::run(
        &["ps", "-axww", "-o", "pid=,ppid=,comm=,args="],
        RUN_TIMEOUT,
    )
}

/// Everything one poll established, each source answering for itself.
///
/// Eight `Result`s rather than one shared error, because a `task list` that
/// failed and a session with no tasks in it are opposite readings and the
/// screen has to be able to say which. A single error field would have made
/// the failed one draw as `0 tasks`.
struct State {
    snapshot: Result<Snapshot, String>,
    agents: Result<Vec<Agent>, String>,
    tasks: Result<Vec<Task>, String>,
    leases: Result<Vec<Lease>, String>,
    /// What is claimable right now, which `task list` cannot say: a list
    /// with three tasks in it and nothing ready are both non-empty.
    next: Result<NextTask, String>,
    /// Every resumable session on the machine, each marked with whether
    /// its directory is one this session has open.
    sessions: Result<Vec<Resumable>, String>,
    /// The focused workspace's checkout, and every checkout of its repo.
    ///
    /// One workspace, not all of them: the CLI's `git status` and
    /// `worktree list` take no workspace argument and answer for whichever
    /// workspace the session is focused on. The screen names that
    /// workspace rather than implying the figures cover the session.
    git: Result<GitState, String>,
    worktrees: Result<Vec<Worktree>, String>,
    /// Set when there is no session to read at all, and which of the three
    /// reasons that is.
    absent: Option<Absence>,
    /// Why the poller stopped, when it did. A thread that dies takes its
    /// explanation with it and leaves a board that looks like an empty
    /// session, which is the one thing this widget must never do.
    err: String,
    /// Event delivery is independent of the authoritative sources above:
    /// a successful snapshot must not imply that the waiter is healthy.
    watcher_err: String,
    /// Incremented only when an authoritative snapshot supplies a valid
    /// event cursor. Waiters discard results from an older epoch.
    snapshot_epoch: u64,
    /// Last successful snapshot cursor, retained while later snapshot reads
    /// fail so an in-flight waiter cannot mistake an old result for current.
    event_cursor: Option<u64>,
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
            next: Err(WAITING.into()),
            sessions: Err(WAITING.into()),
            git: Err(WAITING.into()),
            worktrees: Err(WAITING.into()),
            absent: None,
            err: String::new(),
            watcher_err: String::new(),
            snapshot_epoch: 0,
            event_cursor: None,
            read: false,
        }
    }
}

/// What each agent's state was when we first saw it, so a duration can be
/// measured rather than guessed. UHP does not timestamp a state change.
#[derive(Default)]
struct Seen {
    logs: tc::agent_work::LogReader,
    since: HashMap<String, (String, f64, bool)>,
    first_poll: bool,
}

/// A resumable ID is exact only when the server ties it to this pane.
/// A cwd-only resumable entry does not identify this process or the newest
/// session. With no binding the log reader chooses its own labelled guess.
fn log_session_hint<'a>(agent: &'a Agent, sessions: &'a [parse::Resumable]) -> Option<(&'a str, bool)> {
    if !agent.session_id.is_empty() { return Some((&agent.session_id, false)); }
    if let Some(s) = sessions.iter().find(|s| !s.pane.is_empty() && s.pane == agent.pane
        && s.kind == agent.kind && !s.session_id.is_empty()) {
        return Some((&s.session_id, false));
    }
    None
}

fn poll(state: &Arc<Mutex<State>>, seen: &mut Seen, session: &str) {
    // The snapshot answers first because it is also the liveness probe: if
    // there is no server, nothing else is worth asking and the reason is
    // the same for every reading.
    let mut snapshot = match luvus_text(session, &["uhp", "snapshot"]) {
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
                guard.leases = Err(why.clone());
                guard.next = Err(why.clone());
                guard.sessions = Err(why.clone());
                guard.git = Err(why.clone());
                guard.worktrees = Err(why);
                guard.absent = Some(absent);
                guard.read = true;
            }
            return;
        }
    };

    let mut processes = None;
    if let Ok(snapshot) = &mut snapshot {
        // One bounded local scan per poll, not one subprocess per pane. UHP
        // provides root PIDs; full command lines never travel over its wire.
        let read = local_processes();
        parse::mark_opscope_processes(snapshot, read.as_deref().map_err(String::from));
        processes = Some(read.and_then(|text| tc::agent_work::parse_process_table(&text)));
    }
    // The same scan answers what each agent left running: the pane's root
    // process is its shell, the agent is under it, and its shells under that.
    let roots: HashMap<String, u32> = snapshot
        .as_ref()
        .map(|s| {
            s.panes
                .iter()
                .filter_map(|pane| Some((pane.pane_id.clone(), pane.root_pid?)))
                .collect()
        })
        .unwrap_or_default();

    let agents = luvus_json(session, &["agent", "list"]).and_then(|text| parse::parse_agents(&text));
    let tasks = luvus_json(session, &["task", "list"]).and_then(|text| parse::parse_tasks(&text));
    let leases = luvus_json(session, &["lease", "list"]).and_then(|text| parse::parse_leases(&text));
    // Worked out from the list, never asked for: `task next` claims.
    let next = tasks.as_ref().map(|t| parse::next_ready(t)).map_err(|why| why.clone());
    // Marked against the session's own workspaces, because `agent sessions`
    // answers for the whole machine: nine of ten on the box this was built
    // against were in directories no open workspace covers. A snapshot
    // that did not come back is not an empty list — that would mark every
    // session "not open here" when membership was never established.
    let open: Option<Vec<String>> = snapshot
        .as_ref()
        .ok()
        .map(|s| s.spaces.iter().map(|w| w.cwd.clone()).collect());
    let sessions = luvus_text(session, &["agent", "sessions"])
        .and_then(|text| parse::parse_sessions(&text, open.as_deref()));
    let git =
        luvus_text(session, &["git", "status"]).and_then(|text| parse::parse_git_status(&text));
    let worktrees =
        luvus_text(session, &["worktree", "list"]).and_then(|text| parse::parse_worktrees(&text));

    let at = tc::now();
    let agents = agents.map(|listed| {
        let mut listed: Vec<Agent> = listed;
        let mut live = HashSet::new();
        for agent in &mut listed {
            let key = duration_key(&agent.pane, &agent.name);
            let (since, exact) = measure_since(seen, &key, &agent.state, at);
            agent.since = since;
            agent.exact = exact;
            // With no root known the agent cannot be found, but a table
            // that did not come back still says so on its row.
            if let Some(table) = &processes {
                let pid = roots
                    .get(&agent.pane)
                    .and_then(|&root| table.as_ref().ok().and_then(|t| t.agent_below(root, &agent.kind)));
                agent.work = tc::agent_work::Work::of(table, pid);
            }
            let hint = log_session_hint(agent, sessions.as_ref().ok().map(Vec::as_slice).unwrap_or(&[]));
            let logs = seen.logs.read(&agent.kind, &agent.cwd, hint.map(|(id, _)| id))
                .map(|mut logs| {
                    logs.guessed |= hint.is_some_and(|(_, guessed)| guessed);
                    logs
                });
            agent.work = std::mem::take(&mut agent.work).with_logs(logs);
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
        // Drawn, walked by the keys and indexed by the cursor in this
        // order, so the grouping happens here and nowhere else.
        by_workspace(listed)
    });

    let event_sequence = snapshot.as_ref().ok().map(|snapshot| snapshot.sequence);
    if let Ok(mut guard) = state.lock() {
        guard.snapshot = snapshot;
        guard.agents = agents;
        guard.tasks = tasks;
        guard.leases = leases;
        guard.next = next;
        guard.sessions = sessions;
        guard.git = git;
        guard.worktrees = worktrees;
        guard.absent = None;
        guard.read = true;
        if let Some(sequence) = event_sequence {
            guard.snapshot_epoch = guard.snapshot_epoch.wrapping_add(1);
            guard.event_cursor = Some(sequence);
        }
    }
    seen.first_poll = false;
}

fn event_baseline(state: &Arc<Mutex<State>>) -> Option<(u64, u64)> {
    let guard = state.lock().ok()?;
    Some((guard.snapshot_epoch, guard.event_cursor?))
}

fn watcher_error(state: &Arc<Mutex<State>>, error: impl Into<String>) {
    if let Ok(mut guard) = state.lock() {
        guard.watcher_err = error.into();
    }
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
        watcher_error(
            state,
            format!("event watcher panicked: {}", panic_reason(payload.as_ref())),
        );
    }
}

fn recover_event_baseline(
    state: &Arc<Mutex<State>>,
    wake: &refresh_wake::Wake,
    stop: &AtomicBool,
    old_epoch: u64,
) -> Option<(u64, u64)> {
    refresh_wake::request(wake);
    while !stop.load(Ordering::Relaxed) {
        if let Some(baseline) = event_baseline(state) {
            if baseline.0 != old_epoch {
                return Some(baseline);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn event_watcher(
    state: Arc<Mutex<State>>,
    wake: refresh_wake::Wake,
    session: String,
    refresh: f64,
    stop: Arc<AtomicBool>,
) {
    let timeout_s = (refresh.floor() as u64).clamp(1, 30);
    let execution_timeout = timeout_s + EVENT_WAIT_GRACE;
    let args = ["luvus", "--session", &session, "uhp", "proxy"];
    let mut baseline = None;
    let mut backoff = Duration::from_secs(1);

    while !stop.load(Ordering::Relaxed) {
        if let Some(current) = event_baseline(&state) {
            if baseline.is_none_or(|previous: (u64, u64)| previous.0 != current.0) {
                baseline = Some(current);
            }
        }
        let Some((wait_epoch, cursor)) = baseline else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };

        // One status-only wait prioritizes blocked-agent latency; other
        // session changes are covered by the independent refresh ceiling.
        let request = events::wait_request(cursor, timeout_s);
        let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tc::run_input(&args, &request, execution_timeout)
        }));
        let response = match response {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                watcher_error(&state, parse::sanitize_error(&error));
                baseline = recover_event_baseline(&state, &wake, &stop, wait_epoch);
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(8));
                continue;
            }
            Err(payload) => {
                watcher_error(
                    &state,
                    format!("event watcher panicked: {}", panic_reason(payload.as_ref())),
                );
                baseline = recover_event_baseline(&state, &wake, &stop, wait_epoch);
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(8));
                continue;
            }
        };

        let result = parse::parse_wait_response(&response, cursor);
        let current = event_baseline(&state).unwrap_or((wait_epoch, cursor));
        if let Some((next_baseline, should_wake)) =
            events::reconcile_result(&result, wait_epoch, cursor, current)
        {
            if should_wake {
                refresh_wake::request(&wake);
            }
            baseline = Some(next_baseline);
            if let Ok(mut guard) = state.lock() {
                guard.watcher_err.clear();
            }
            backoff = Duration::from_secs(1);
            continue;
        }

        if let Err(error) = result {
            watcher_error(&state, error);
            baseline = recover_event_baseline(&state, &wake, &stop, wait_epoch);
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(8));
        }
    }
}

/// One agent's evidence, fetched when asked for rather than every poll.
///
/// Two calls, kept apart, because they fail for different reasons and the
/// screen says which: an agent the server cannot explain is a different
/// answer from a pane it cannot read.
struct Detail {
    pane: String,
    what: String,
    explain: Result<Explanation, String>,
    screen: Result<String, String>,
    /// False until the worker that fetched this has answered, so the
    /// panel can open on the keystroke rather than after both calls
    /// return. An empty panel and a panel still loading are opposite
    /// readings of the same screen.
    ready: bool,
}

/// Ask the server about one agent. Bounded like every other call.
///
/// Done on the keystroke rather than in the poller: it is one agent's
/// evidence asked for once, and putting it in the four-second round would
/// make every refresh pay for a panel nobody has open.
fn explain_agent(session: &str, pane: &str, what: &str) -> Detail {
    Detail {
        pane: pane.to_string(),
        what: what.to_string(),
        explain: luvus_text(session, &["agent", "explain", pane])
            .and_then(|t| parse::parse_explanation(&t)),
        screen: luvus_text(
            session,
            &[
                "agent", "read", pane, "--lines", "60", "--source", "visible",
            ],
        )
        .and_then(|t| parse::parse_screen(&t)),
        ready: true,
    }
}

/// Fetch one agent's evidence off the input loop.
///
/// `luvus_text` waits up to `RUN_TIMEOUT` per call, and two of those in
/// sequence would freeze redraw and keys — including `esc` and `q` — for
/// half a minute if the socket stopped answering. The panel opens at
/// once; this thread publishes the finished `Detail` when both calls
/// return. A panic is recorded as a failed read rather than leaving the
/// panel on "reading…" forever.
fn ask_explain(
    session: String,
    pane: String,
    what: String,
    gen: u64,
    inbox: Arc<Mutex<Option<(u64, Detail)>>>,
    activity: tc::Refresh,
) {
    std::thread::spawn(move || {
        let _work = activity.start();
        let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            explain_agent(&session, &pane, &what)
        }));
        let detail = match step {
            Ok(d) => d,
            Err(_) => Detail {
                pane,
                what,
                explain: Err("explain stopped - see the pane it was started from".into()),
                screen: Err("explain stopped - see the pane it was started from".into()),
                ready: true,
            },
        };
        if let Ok(mut guard) = inbox.lock() {
            // A slower worker from an older panel must not replace a
            // newer worker's finished read, or the open panel stays on
            // "reading…" after the current result has already arrived.
            if guard.as_ref().is_none_or(|(had, _)| *had < gen) {
                *guard = Some((gen, detail));
            }
        }
    });
}

/// Where a row points, so `↵` knows which pane to focus.
#[derive(Clone)]
enum Row {
    Agent(Agent),
    Task(Task),
    Lease(Lease),
    Pane(Pane),
    /// A session no pane is holding. It has nowhere to jump to, which is
    /// the same shape as an unclaimed task and is answered the same way.
    Resumable(Resumable),
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
            Row::Resumable(_) => String::new(),
        }
    }

    fn what(&self) -> String {
        match self {
            Row::Agent(a) => a.name.clone(),
            Row::Task(t) => t.id.clone(),
            Row::Lease(l) => l.id.clone(),
            Row::Pane(p) => p.command.clone(),
            Row::Resumable(r) => r.kind.clone(),
        }
    }

    /// What the `/` filter matches the row on: the words it shows - agent
    /// or command, workspace, directory, branch, title and what it reported.
    fn search_fields(&self) -> Vec<String> {
        let mut out = match self {
            Row::Agent(a) => {
                let mut out = vec![
                    a.name.clone(),
                    a.kind.clone(),
                    a.state.clone(),
                    a.workspace.clone(),
                    a.branch.clone(),
                    a.project.clone(),
                    parse::homely(&a.cwd),
                    a.state_source.clone(),
                ];
                out.extend(a.work.rows(240).into_iter().map(|(_, text)| text.trim().to_string()));
                out
            }
            Row::Task(t) => vec![t.id.clone(), t.status.clone(), t.title.clone(), t.holder.clone()],
            Row::Lease(l) => {
                let mut out = vec![l.task.clone(), l.holder.clone()];
                out.extend(l.paths.iter().cloned());
                out
            }
            Row::Pane(n) => vec![
                n.command.clone(),
                if n.status.is_empty() { "no state".into() } else { n.status.clone() },
                n.workspace.clone(),
                n.branch.clone(),
                parse::homely(&n.cwd),
            ],
            // Both wordings of the membership, since the pane's width
            // picks which one is drawn.
            Row::Resumable(r) => vec![
                r.kind.clone(),
                parse::homely(&r.cwd),
                membership_label(r.in_workspace, true).into(),
                membership_label(r.in_workspace, false).into(),
            ],
        };
        out.retain(|s| !s.is_empty());
        out
    }

    /// Whether two rows are the same entry, across a rebuild of the list.
    fn same(&self, other: &Row) -> bool {
        match (self, other) {
            (Row::Agent(a), Row::Agent(b)) => a.pane == b.pane && a.name == b.name,
            (Row::Task(a), Row::Task(b)) => a.id == b.id,
            (Row::Lease(a), Row::Lease(b)) => a.id == b.id && a.task == b.task,
            (Row::Pane(a), Row::Pane(b)) => a.pane_id == b.pane_id,
            (Row::Resumable(a), Row::Resumable(b)) => {
                a.session_id == b.session_id && a.kind == b.kind && a.cwd == b.cwd
            }
            _ => false,
        }
    }
}

/// Where the cursor lands once the list is rebuilt. The cursor is a row,
/// not a position: a refresh that puts a newly blocked agent above it must
/// not move `↵` onto somebody else. A row that has gone leaves the index
/// where it was, clamped by the caller - unless the `/` filter just hid it,
/// and then the cursor goes to the first match.
fn reselect(before: &[Row], selected: usize, after: &[Row], filter_changed: bool) -> usize {
    super::search::reselect(before, selected, after, filter_changed, |a, b| a.same(b))
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
    /// the repository asks for against the tint as well as the background.
    /// This is the same role lifted — `ink_on(Muted, true)` — used *only*
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

/// Break a line at cell boundaries without collapsing spaces.
///
/// `wrap_words` is the right tool for prose, and the wrong one for a
/// pane's screen: a prompt's leading spaces are the indent, and dropping
/// them would make two different lines look the same. A word wider than
/// the pane is still broken rather than handed to `seg` to clip.
fn wrap_cells(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = 0usize;
        for (at, ch) in rest.char_indices() {
            let next = at + ch.len_utf8();
            if tc::display_width(&rest[..next]) <= width {
                end = next;
            } else {
                break;
            }
        }
        if end == 0 {
            let ch = rest.chars().next().unwrap();
            out.push(ch.to_string());
            rest = &rest[ch.len_utf8()..];
            continue;
        }
        out.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
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
fn window_from(
    total: usize,
    want: std::ops::Range<usize>,
    room: usize,
    from: usize,
    chase: bool,
) -> usize {
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

fn pane_summary(total: usize, hidden_opscope: usize, resting: usize, show_idle: bool) -> String {
    let hidden_idle = if show_idle { 0 } else { resting };
    let visible = total - hidden_opscope - hidden_idle;
    format!(
        " · {} {}pane{}",
        visible,
        if hidden_opscope + hidden_idle > 0 {
            "visible "
        } else {
            ""
        },
        plural(visible)
    )
}

/// The filter `[i]` applies, named, or nothing when it is holding nothing.
///
/// Hiding the idle panes is a filter like any other and an unstated one
/// leaves a short list looking like a quiet session. It is only a filter
/// while there is something behind it: with no pane at a prompt, hidden and
/// shown are the same screen, and a line saying `0 idle panes hidden` would
/// be a filter announcing itself for nothing.
fn idle_filter(show_idle: bool, resting: usize) -> Vec<String> {
    if show_idle || resting == 0 {
        return Vec::new();
    }
    vec![format!("{} idle pane{} hidden", resting, plural(resting))]
}

/// Give a short pane its body back without dropping an applied filter.
///
/// The header pops from the bottom until the body has [`HEAD_FLOOR`] rows
/// to scroll. The idle-filter line is pushed last-but-the-blank, so that
/// loop would take the blank and then the filter — a filtered list with
/// nothing saying so, which is the quiet-session reading the line exists
/// to prevent. The filter is held out of the prune and put back after.
fn prune_head(
    mut head: Vec<String>,
    filter: Option<String>,
    footer_len: usize,
    h: usize,
) -> Vec<String> {
    const HEAD_FLOOR: usize = 3;
    let keep = usize::from(filter.is_some());
    while head.len() > 2 && h.saturating_sub(head.len() + keep + footer_len + 1) < HEAD_FLOOR {
        head.pop();
    }
    if let Some(line) = filter {
        if head.last().is_some_and(String::is_empty) {
            let blank = head.pop().expect("last row is the trailing blank");
            head.push(line);
            head.push(blank);
        } else {
            head.push(line);
        }
    }
    head
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

/// The count a heading may print: `3 of 14` while the `/` filter holds
/// some back. A failed source has no number.
fn shown_count<T>(reading: &Result<Vec<T>, String>, shown: usize, filtering: bool) -> String {
    match reading {
        Ok(rows) => super::search::count(shown, rows.len(), filtering),
        Err(_) => "unread".into(),
    }
}

/// Away first: a session left in a directory nobody has open is the one
/// you are least likely to remember. Unknown membership is not "away".
fn membership_rank(inside: Option<bool>) -> u8 {
    match inside {
        Some(false) => 0,
        None => 1,
        Some(true) => 2,
    }
}

/// What a resumable row says about workspace membership.
///
/// Narrow panes get a compact word; colour alone is not a label. Wide
/// panes keep the longer sentence.
/// The widest of some values, in cells.
fn widest<S: AsRef<str>>(values: impl Iterator<Item = S>) -> usize {
    values
        .map(|v| tc::display_width(v.as_ref()))
        .max()
        .unwrap_or(0)
}

/// What the BRANCH column says for an agent: its branch, or the project
/// when it is not on one.
fn branch_of(a: &Agent) -> String {
    if a.branch.is_empty() {
        a.project.clone()
    } else {
        a.branch.clone()
    }
}

/// How wide each text column is drawn this frame.
///
/// The text columns grow with the pane. Their old fixed widths are the
/// minimums, so nothing moves at the `wide` threshold, and whatever the
/// fixed cells leave goes to the column still cutting something: a
/// workspace stayed cut at 14 cells on a 200-column pane with the rest
/// blank. A row's last column (BRANCH, DIRECTORY) is never padded, but it
/// takes its share here so the columns before it do not starve it.
#[derive(Debug, PartialEq)]
struct Columns {
    name_w: usize,
    agent_workspace_w: usize,
    command_w: usize,
    busy_workspace_w: usize,
    dir_w: usize,
    idle_workspace_w: usize,
}

impl Columns {
    /// `want` is the widest value this frame of: agent name, agent
    /// workspace, branch, pane command, pane workspace, pane directory,
    /// idle directory, idle workspace.
    fn fit(w: usize, wide: bool, want: [usize; 8]) -> Columns {
        let row = w.saturating_sub(1);
        // An agent row: `▸`, the mark and a space, the name, STATE and FOR,
        // the space before the workspace; the space before BRANCH when wide.
        let agent = if wide {
            tc::fit_columns(
                row.saturating_sub(3 + 9 + 7 + 1 + 1),
                &[(8, want[0]), (14, want[1]), (0, want[2])],
            )
        } else {
            tc::fit_columns(
                row.saturating_sub(3 + 9 + 7 + 1),
                &[(8, want[0]), (14, want[1])],
            )
        };
        // A pane row: `▸`, the mark and a space, the command, STATE, the
        // space before the workspace; the space before the directory when
        // wide.
        let busy = if wide {
            tc::fit_columns(
                row.saturating_sub(3 + 9 + 1 + 1),
                &[(12, want[3]), (14, want[4]), (0, want[5])],
            )
        } else {
            tc::fit_columns(
                row.saturating_sub(3 + 9 + 1),
                &[(12, want[3]), (14, want[4])],
            )
        };
        // An idle row: `▸`, the mark and a space, the directory and a
        // space, then the workspace.
        let idle = tc::fit_columns(row.saturating_sub(3 + 1), &[(30, want[6]), (14, want[7])]);
        Columns {
            name_w: agent[0],
            agent_workspace_w: agent[1],
            command_w: busy[0],
            busy_workspace_w: busy[1],
            dir_w: idle[0],
            idle_workspace_w: idle[1],
        }
    }
}

/// Agents regrouped by workspace, without losing who needs you: each
/// workspace's agents together in the order they already had, worst first,
/// and the workspaces in the order their most urgent agent came - which,
/// for a list already sorted worst first, is the order each first appears.
fn by_workspace(agents: Vec<Agent>) -> Vec<Agent> {
    let mut groups: Vec<Vec<Agent>> = Vec::new();
    for agent in agents {
        match groups.iter_mut().find(|g| g[0].workspace == agent.workspace) {
            Some(g) => g.push(agent),
            None => groups.push(vec![agent]),
        }
    }
    groups.into_iter().flatten().collect()
}

/// A workspace as its heading names it; Luvus can report an agent in none.
fn workspace_name(workspace: &str) -> &str {
    if workspace.is_empty() {
        "no workspace"
    } else {
        workspace
    }
}

fn membership_label(inside: Option<bool>, wide: bool) -> &'static str {
    match (inside, wide) {
        (Some(true), true) => "in an open workspace",
        (Some(false), true) => "not open here",
        (None, true) => "workspace unread",
        (Some(true), false) => "open",
        (Some(false), false) => "away",
        (None, false) => "?",
    }
}

/// The checkout line under the session. Each source speaks for itself.
///
/// A git failure, a worktree failure, and a directory that is not a
/// repository are different sentences. Omitting the row would make an
/// unread source look like a checkout with nothing to say.
fn checkout_line<'a>(
    git: &'a Result<GitState, String>,
    worktrees: &'a Result<Vec<Worktree>, String>,
    focused: &str,
    p: &'a Palette,
) -> Vec<(&'a str, String)> {
    match git {
        Ok(g) => {
            let mut said = vec![(
                p.dim.as_str(),
                if focused.is_empty() {
                    format!(" ⑂ on {}", g.branch)
                } else {
                    format!(" ⑂ {} on {}", focused, g.branch)
                },
            )];
            if g.behind > 0 {
                said.push((p.blocked.as_str(), format!(" · {} behind", g.behind)));
            }
            if g.ahead > 0 {
                said.push((p.done.as_str(), format!(" · {} ahead", g.ahead)));
            }
            if g.dirty > 0 {
                // Entries, not files: git reports a wholly untracked
                // directory as one entry and the count would be a lie.
                said.push((
                    p.working.as_str(),
                    format!(
                        " · {} entr{} changed",
                        g.dirty,
                        if g.dirty == 1 { "y" } else { "ies" }
                    ),
                ));
            }
            if g.stashes > 0 {
                said.push((p.dim.as_str(), format!(" · {} stashed", g.stashes)));
            }
            match worktrees {
                Ok(trees) if trees.len() > 1 => {
                    said.push((p.dim.as_str(), format!(" · {} worktrees", trees.len())));
                }
                Ok(_) => {}
                Err(why) => {
                    said.push((p.unknown.as_str(), format!(" · worktrees unread — {}", why)));
                }
            }
            said
        }
        Err(why) if parse::is_not_a_repo(why) => {
            vec![(
                p.dim.as_str(),
                if focused.is_empty() {
                    " ⑂ not a repository".into()
                } else {
                    format!(" ⑂ {} — not a repository", focused)
                },
            )]
        }
        Err(why) => {
            let mut said = vec![(
                p.unknown.as_str(),
                if focused.is_empty() {
                    format!(" ⑂ checkout unread — {}", why)
                } else {
                    format!(" ⑂ {} · checkout unread — {}", focused, why)
                },
            )];
            match worktrees {
                Ok(trees) if trees.len() > 1 => {
                    said.push((p.dim.as_str(), format!(" · {} worktrees", trees.len())));
                }
                Ok(_) => {}
                Err(wwhy) if parse::is_not_a_repo(wwhy) => {}
                Err(wwhy) => {
                    said.push((
                        p.unknown.as_str(),
                        format!(" · worktrees unread — {}", wwhy),
                    ));
                }
            }
            said
        }
    }
}

/// Run the board against a Luvus session until `q`. `why` is the line
/// under the title saying what chose Luvus; `comma` opens the shared
/// settings screen.
pub fn watch(cfg: &serde_json::Value, args: &[String], why: &str, comma: &dyn Fn(&mut tc::Keyboard)) {
    let session = tc::cfg_str(cfg, "luvus_session", "default");
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
    let polled_session = session.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let poller_stop = Arc::clone(&stop);
    std::thread::spawn(move || {
        let mut seen = Seen {
            first_poll: true,
            ..Default::default()
        };
        loop {
            let work = poller_activity.start();
            // A poller that dies takes its explanation with it, and an
            // empty board looks exactly like a session with nothing in it.
            let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                poll(&poller, &mut seen, &polled_session)
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
    let watcher_session = session.clone();
    std::thread::spawn(move || {
        let loop_state = Arc::clone(&watcher_state);
        run_watcher_catching_panic(&watcher_state, move || {
            event_watcher(
                loop_state,
                watcher_wake,
                watcher_session,
                refresh,
                watcher_stop,
            )
        });
    });

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut show_idle = true;
    let mut opscope_filter = process_filter::Filter::default();
    let mut search = super::search::Search::default();
    let mut selected = 0usize;
    // How far down the body, in drawn rows, the window has scrolled. Kept
    // across frames so the view holds still while the cursor moves inside
    // it, and only moves when the cursor would leave it.
    let mut scroll = 0usize;
    // Where each entry's rows landed on the frame now on screen, how many
    // rows the header keeps above the window, and where the window starts.
    // A click is answered against the frame the reader was looking at.
    let (mut placed, mut list_head, mut list_scroll): (Vec<(usize, usize)>, usize, usize) =
        (Vec::new(), 0, 0);
    // Set by whichever key moved the cursor, and cleared once the window
    // has been asked to hold it. Without it the window chases the cursor
    // every frame and drags itself back from wherever the wheel put it.
    let mut moved = false;
    let mut note: Option<(String, bool, f64)> = None;
    // The evidence panel, when one is open. Read-only, and it replaces the
    // body rather than covering it, so nothing is hidden behind it.
    let mut detail: Option<Detail> = None;
    // A generation so a late worker cannot write into a panel that has
    // already been closed, or into a newer one opened for a different
    // agent. Incremented on every open and every close.
    let mut detail_gen = 0u64;
    let inbox: Arc<Mutex<Option<(u64, Detail)>>> = Arc::new(Mutex::new(None));
    let mut rows_now: Vec<Row> = Vec::new();
    // Where each section starts in `rows_now`, read as one flat list, with
    // the empty ones left out. Written by the frame and read by tab on the
    // frame after, which is the same one-frame lag `rows_now` already has.
    let mut sections: Vec<usize> = Vec::new();

    loop {
        let tick = tc::animation_tick();
        // The query line takes `!` as a character, not the notices screen.
        if search.typing {
            keyboard.typing();
        }
        let mut keys = keyboard.poll();
        // A click on another row moves the cursor there; a click on the row
        // it is already on becomes `enter`, which is the key the footer
        // names for opening one. Rewritten before the match rather than
        // acted on here, so the arm below does the opening and this cannot
        // drift from what the keyboard does.
        if let Some(at) = tc::rows_clicked(
            &mut keys,
            Some(selected),
            list_head,
            list_scroll,
            &placed,
            None,
        ) {
            selected = at;
            moved = true;
        }
        for key in keys {
            if search.takes(&key) {
                search.key(&key);
                moved = true;
                continue;
            }
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
                "o" | "O" => {
                    if detail.is_none() {
                        opscope_filter.toggle(&mut selected);
                        moved = true;
                    }
                }
                "i" | "I" => {
                    if detail.is_none() {
                        show_idle = !show_idle;
                        selected = 0;
                        moved = true;
                    }
                }
                // The evidence behind the selected agent's row. Read-only:
                // it says what the agent is waiting for and never answers
                // it. `esc` and a second press both close it.
                "e" | "E" => {
                    if detail.is_some() {
                        detail = None;
                        detail_gen += 1;
                        scroll = 0;
                        moved = true;
                    } else if let Some(Row::Agent(agent)) =
                        rows_now.get(selected.min(rows_now.len().saturating_sub(1)))
                    {
                        let pane = agent.pane.clone();
                        if pane.is_empty() {
                            note = Some((
                                format!("! {} is not in a pane to explain", agent.name),
                                false,
                                tc::now() + 3.0,
                            ));
                        } else {
                            detail_gen += 1;
                            detail = Some(Detail {
                                pane: pane.clone(),
                                what: agent.name.clone(),
                                explain: Err(String::new()),
                                screen: Err(String::new()),
                                ready: false,
                            });
                            scroll = 0;
                            ask_explain(
                                session.clone(),
                                pane,
                                agent.name.clone(),
                                detail_gen,
                                Arc::clone(&inbox),
                                activity.clone(),
                            );
                        }
                    } else if let Some(row) =
                        rows_now.get(selected.min(rows_now.len().saturating_sub(1)))
                    {
                        note = Some((
                            format!("! {} is not an agent to explain", row.what()),
                            false,
                            tc::now() + 3.0,
                        ));
                    }
                }
                // The evidence panel keeps `esc` while it is open; on the
                // board it clears the filter.
                "esc" if detail.is_none() && search.active() => {
                    search.clear();
                    moved = true;
                }
                "/" if detail.is_none() => search.open(),
                "esc" => {
                    detail = None;
                    detail_gen += 1;
                    scroll = 0;
                    moved = true;
                }
                "up" | "k" | "K" => {
                    if detail.is_none() {
                        selected = selected.saturating_sub(1);
                        moved = true;
                    }
                }
                "down" | "j" | "J" => {
                    if detail.is_none() {
                        selected += 1;
                        moved = true;
                    }
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
                    if detail.is_none() {
                        if let Some(next) = sections
                            .iter()
                            .find(|&&at| at > selected)
                            .or_else(|| sections.first())
                        {
                            selected = *next;
                            moved = true;
                        }
                    }
                }
                "home" => {
                    if detail.is_none() {
                        selected = 0;
                        moved = true;
                    }
                }
                "end" => {
                    if detail.is_none() {
                        selected = rows_now.len().saturating_sub(1);
                        moved = true;
                    }
                }
                "enter" | "f" | "F" => {
                    // While the panel is open it names one pane, and that
                    // is the one `↵` focuses — not whichever row a hidden
                    // selection or a refresh reordering would now pick.
                    let (pane, what) = if let Some(d) = &detail {
                        (d.pane.clone(), d.what.clone())
                    } else if let Some(row) =
                        rows_now.get(selected.min(rows_now.len().saturating_sub(1)))
                    {
                        (row.pane(), row.what())
                    } else {
                        (String::new(), String::new())
                    };
                    if !what.is_empty() || !pane.is_empty() {
                        note = Some(if pane.is_empty() {
                            // A task nobody has claimed is in no pane, and
                            // focusing something else would be a lie about
                            // where the work is.
                            (
                                format!("! {} is not in a pane to jump to", what),
                                false,
                                tc::now() + 3.0,
                            )
                        } else if focus_pane(&session, &pane) {
                            (format!("→ focused pane {}", pane), true, tc::now() + 3.0)
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
        if let Ok(mut guard) = inbox.lock() {
            if let Some((gen, arrived)) = guard.take() {
                if gen == detail_gen && detail.as_ref().is_some_and(|d| !d.ready) {
                    detail = Some(arrived);
                }
            }
        }

        let (w, h) = tc::size();
        let (
            snapshot,
            agents,
            tasks,
            leases,
            absent,
            err,
            watcher_err,
            read,
            next,
            sessions,
            git,
            worktrees,
        ) = match state.lock() {
            Ok(g) => (
                g.snapshot.clone(),
                g.agents.clone(),
                g.tasks.clone(),
                g.leases.clone(),
                g.absent.clone(),
                g.err.clone(),
                g.watcher_err.clone(),
                g.read,
                g.next.clone(),
                g.sessions.clone(),
                g.git.clone(),
                g.worktrees.clone(),
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
        // Worst first here too: a session left in a directory nobody has
        // open is the one you are least likely to remember.
        let mut resumable_rows: Vec<Resumable> = sessions.clone().unwrap_or_default();
        resumable_rows.sort_by(|a, b| {
            membership_rank(a.in_workspace)
                .cmp(&membership_rank(b.in_workspace))
                .then(a.kind.cmp(&b.kind))
                .then(a.cwd.cmp(&b.cwd))
        });
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
        let total_others = others.len();
        let hidden_opscope = others
            .iter()
            .filter(|n| !opscope_filter.visible(n.is_opscope))
            .count();
        others.retain(|n| opscope_filter.visible(n.is_opscope));
        // Unstated first, then busy, then the prompts: the row that wants
        // looking at is the one where luvus could not say.
        others.sort_by_key(|n| match n.status.as_str() {
            "" => 0,
            "idle" => 2,
            _ => 1,
        });
        let busy: Vec<Pane> = others
            .iter()
            .filter(|n| n.status != "idle")
            .cloned()
            .collect();
        let resting: Vec<Pane> = others
            .iter()
            .filter(|n| n.status == "idle")
            .cloned()
            .collect();
        let unstated = busy.iter().filter(|n| n.status.is_empty()).count();
        let running = busy.len() - unstated;

        // The `/` filter only includes or excludes: each section keeps the
        // order above, so a blocked agent stays above an idle one that
        // matched better. The unfiltered lists still feed the summary.
        let filtering = search.active();
        let keep = |row: Row| search.matches(&row.search_fields());
        let agents_shown: Vec<Agent> =
            agent_rows.iter().filter(|a| keep(Row::Agent((*a).clone()))).cloned().collect();
        let tasks_shown: Vec<Task> =
            task_rows.iter().filter(|t| keep(Row::Task((*t).clone()))).cloned().collect();
        let leases_shown: Vec<Lease> =
            lease_rows.iter().filter(|l| keep(Row::Lease((*l).clone()))).cloned().collect();
        let resumable_shown: Vec<Resumable> = resumable_rows
            .iter()
            .filter(|r| keep(Row::Resumable((*r).clone())))
            .cloned()
            .collect();
        let busy_shown: Vec<Pane> =
            busy.iter().filter(|n| keep(Row::Pane((*n).clone()))).cloned().collect();
        let resting_shown: Vec<Pane> =
            resting.iter().filter(|n| keep(Row::Pane((*n).clone()))).cloned().collect();

        let before = std::mem::take(&mut rows_now);
        rows_now = agents_shown
            .iter()
            .cloned()
            .map(Row::Agent)
            .chain(tasks_shown.iter().cloned().map(Row::Task))
            .chain(leases_shown.iter().cloned().map(Row::Lease))
            .chain(resumable_shown.iter().cloned().map(Row::Resumable))
            .chain(busy_shown.iter().cloned().map(Row::Pane))
            .chain(resting_shown.iter().filter(|_| show_idle).cloned().map(Row::Pane))
            .collect();
        selected = reselect(&before, selected, &rows_now, search.take_changed());
        if !rows_now.is_empty() && selected >= rows_now.len() {
            selected = rows_now.len() - 1;
        }
        if note
            .as_ref()
            .is_some_and(|(_, _, until)| tc::now() >= *until)
        {
            note = None;
        }

        let idle_listed = show_idle && !resting.is_empty();
        sections = [
            (0, !agents_shown.is_empty()),
            (agents_shown.len(), !tasks_shown.is_empty()),
            (agents_shown.len() + tasks_shown.len(), !leases_shown.is_empty()),
            (
                agents_shown.len() + tasks_shown.len() + leases_shown.len(),
                !resumable_shown.is_empty(),
            ),
            (
                agents_shown.len() + tasks_shown.len() + leases_shown.len() + resumable_shown.len(),
                !busy_shown.is_empty(),
            ),
            (
                agents_shown.len()
                    + tasks_shown.len()
                    + leases_shown.len()
                    + resumable_shown.len()
                    + busy_shown.len(),
                idle_listed && !resting_shown.is_empty(),
            ),
        ]
        .into_iter()
        .filter_map(|(at, live)| live.then_some(at))
        .collect();

        // ---- the pinned header ----
        let mut head = vec![tc::title_busy(
            "mux activity · luvus",
            w,
            &p.accent,
            activity.tick(tc::animation_tick()),
        )];
        head.extend(super::mux_rows(why, &p.dim, w));
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
            pane_summary(panes.len(), hidden_opscope, resting.len(), show_idle),
        ));
        for state_name in ["blocked", "done", "working", "idle"] {
            if let Some(n) = counts.get(state_name) {
                summary.push((
                    colour_of(state_name, &p),
                    format!("   {} {}", n, state_name),
                ));
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
                            s.spaces.len(),
                            plural(s.spaces.len()),
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
        if !watcher_err.is_empty() {
            head.push(tc::seg(
                &[(
                    p.blocked.as_str(),
                    format!(" ! event wait: {}", watcher_err),
                )],
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
        // The checkout the session is looking at. One workspace, named, and
        // not a total: the CLI's `git status` and `worktree list` take no
        // workspace argument, so a figure here covers the focused workspace
        // and saying otherwise would make four numbers into a claim about
        // the session none of them support. Each Result is drawn even when
        // it failed: omitting the row would make an unread source look like
        // a checkout with nothing to say.
        let focused = snapshot
            .as_ref()
            .ok()
            .and_then(|s| s.spaces.iter().find(|x| x.active))
            .map(|x| x.name.clone())
            .unwrap_or_default();
        head.push(tc::seg(
            &checkout_line(&git, &worktrees, &focused, &p),
            w.saturating_sub(1),
        ));
        // Hiding the idle panes is a filter, and an unstated filter leaves
        // a short list looking like a quiet session. It goes in the pinned
        // header so it cannot scroll away from the list it qualifies, and
        // it is drawn only while it is holding something back.
        let mut hidden_idle = idle_filter(show_idle, resting.len());
        hidden_idle.extend(opscope_filter.description(hidden_opscope));
        if !opscope_filter.show {
            if let Ok(s) = &snapshot {
                if !s.process_error.is_empty() {
                    hidden_idle.push(format!("! {}", s.process_error));
                }
            }
        }
        // `others`, not `mux-activity`: a pane holding a recognised agent is in
        // AGENTS rather than in this list, and counting it here would
        // report it as something the filter had hidden.
        // Held out of `head` until after the prune: that loop pops from
        // the bottom, and this row would be the first content it took.
        // The `/` filter reaches every section, so while it holds the count
        // is every row listed against every row there is, not panes alone.
        hidden_idle.extend(search.filter());
        let board = agent_rows.len()
            + task_rows.len()
            + lease_rows.len()
            + resumable_rows.len()
            + total_others;
        let (shown, of) = if filtering {
            (rows_now.len(), board)
        } else {
            (busy.len() + if show_idle { resting.len() } else { 0 }, total_others)
        };
        let filter_line = tc::filter_row(shown, of, &hidden_idle).map(|said| {
            tc::seg(
                &[(p.dim.as_str(), format!(" {}", said))],
                w.saturating_sub(1),
            )
        });
        head.push(String::new());

        // ---- the footer, built before the body ----
        // It wraps, so how many rows it takes depends on the width, and the
        // body cannot know its own budget until that is settled.
        let mut hints: Vec<Vec<(&str, String)>> = search.hints(&p.dim);
        hints.extend(vec![
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
                (p.dim.as_str(), " focus".into()),
            ],
            vec![
                (p.accent.as_str(), "tab".into()),
                (p.dim.as_str(), " section".into()),
            ],
            vec![(p.dim.as_str(), "[e]xplain".into())],
            // What the next press does, not what is in force: `[i]dle`
            // alone said neither, and the hint beside it on the wall reads
            // as a state. What is in force is the count line in the
            // header, which says how many the filter is holding back.
            vec![(
                p.dim.as_str(),
                format!("[i]dle {}", if show_idle { "hide" } else { "show" }),
            )],
            vec![(p.dim.as_str(), "[r]efresh".into())],
        ]);
        hints.push(vec![(p.dim.as_str(), "[,] settings".into())]);
        hints.push(vec![(p.dim.as_str(), tc::PALETTE_HINT.into())]);
        hints.push(vec![(p.dim.as_str(), tc::THEME_HINT.into())]);
        hints.push(vec![(p.dim.as_str(), "[q]uit".into())]);
        let hints = if search.typing { search.typing_hints(&p.accent, &p.dim, w.saturating_sub(2)) } else { hints };
        let packed = tc::pack_hints_placed(&hints, w.saturating_sub(2), "  ");
        let footer: Vec<String> = packed.lines.iter().map(|l| format!(" {}", l)).collect();

        // ---- the body, built at whatever height it needs ----
        let mut body: Vec<String> = Vec::new();
        let mut plots = Vec::new();
        let total: usize = counts.values().sum();
        if total > 0 {
            let mut states: Vec<_> = counts.iter().collect();
            states.sort_by_key(|(state, _)| **state);
            let parts: Vec<_> = states.into_iter().map(|(state, n)|
                (*n as f64 / total as f64, colour_of(state, &p).to_string())).collect();
            let width = w.saturating_sub(2);
            plots.push(tc::Plot::stacked_bar(1, body.len(), width, &parts));
            let bar = tc::stacked_bar(&parts, width);
            let mut line = vec![(tc::RST, " ".into())];
            line.extend(bar.iter().map(|(c, t)| (c.as_str(), t.clone())));
            body.push(tc::seg(&line, w.saturating_sub(1)));
        }
        // Where each entry's rows sit in `body`, so the window can hold the
        // selected one without counting rows a second time and disagreeing.
        let mut spans: Vec<std::ops::Range<usize>> = Vec::new();

        if let Some(d) = &detail {
            // Replaces the body rather than covering it: a panel drawn over
            // the rows would hide however many it covered, and a section
            // that is not drawn looks exactly like a section with nothing
            // in it. Nothing is selectable here, so `spans` stays empty and
            // the window scrolls on `scroll` alone.
            body.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── EXPLAIN ── ".into()),
                    (p.txt.as_str(), d.what.clone()),
                    (p.dim.as_str(), format!(" · pane {}", d.pane)),
                ],
                w.saturating_sub(1),
            ));
            body.push(String::new());
            if let Some(explanation) = agents.as_ref().ok().and_then(|agents|
                agents.iter().find(|a| a.pane == d.pane)).and_then(|a| a.work.logs.explanation()) {
                for text in tc::wrap_words(&format!("   log source: {explanation}"), w.saturating_sub(1)) {
                    body.push(tc::seg(&[(p.dim.as_str(), text)], w.saturating_sub(1)));
                }
            }
            if !d.ready {
                body.push(tc::seg(
                    &[(p.dim.as_str(), "   reading the evidence…".into())],
                    w.saturating_sub(1),
                ));
            } else {
                match &d.explain {
                    Ok(e) => {
                        // The hint first and in the warning colour, because it
                        // is the answer to the question that opened this panel.
                        if !e.blocked_hint.is_empty() {
                            // Wrapped, not clipped: this sentence is the
                            // reason the panel exists, and `seg` would drop
                            // the suffix that names the approval or the path.
                            let prefix = " ⚠ waiting on  ";
                            let budget = w
                                .saturating_sub(1)
                                .saturating_sub(tc::display_width(prefix))
                                .max(1);
                            for (i, line) in tc::wrap_words(&e.blocked_hint, budget)
                                .into_iter()
                                .enumerate()
                            {
                                let lead = if i == 0 {
                                    prefix.to_string()
                                } else {
                                    " ".repeat(tc::display_width(prefix))
                                };
                                body.push(tc::seg(
                                    &[(p.blocked.as_str(), lead), (p.txt.as_str(), line)],
                                    w.saturating_sub(1),
                                ));
                            }
                        } else if e.status == "blocked" {
                            // Blocked with nothing to say about why is its own
                            // reading, and drawing nothing would look like a
                            // panel that failed to load.
                            body.push(tc::seg(
                                &[(
                                    p.unknown.as_str(),
                                    " ⚠ waiting on  luvus did not say what for".into(),
                                )],
                                w.saturating_sub(1),
                            ));
                        }
                        let mut row = |label: &str, value: String, colour: &str| {
                            body.push(tc::seg(
                                &[
                                    (p.dim.as_str(), format!("   {}", tc::pad(label, 12))),
                                    (colour, value),
                                ],
                                w.saturating_sub(1),
                            ));
                        };
                        row(
                            "state",
                            format!(
                                "{} · via {}{}",
                                e.status,
                                if e.state_source.is_empty() {
                                    "—"
                                } else {
                                    &e.state_source
                                },
                                if e.state_confidence.is_empty() {
                                    String::new()
                                } else {
                                    format!(" · {} confidence", e.state_confidence)
                                }
                            ),
                            colour_of(&e.status, &p),
                        );
                        row(
                            "identity",
                            format!(
                                "{} · via {}{}",
                                if e.kind.is_empty() { "—" } else { &e.kind },
                                if e.identity_source.is_empty() {
                                    "—"
                                } else {
                                    &e.identity_source
                                },
                                if e.identity_confidence.is_empty() {
                                    String::new()
                                } else {
                                    format!(" · {}", e.identity_confidence)
                                }
                            ),
                            p.txt.as_str(),
                        );
                        if !e.rule_region.is_empty() {
                            row(
                                "rule",
                                format!(
                                    "matched in the {}, priority {}",
                                    e.rule_region, e.rule_priority
                                ),
                                p.dim.as_str(),
                            );
                        }
                        // An integration reporting a state and a rule guessing
                        // at one are different strengths of claim, and the row
                        // that says "none" is the weaker one saying so.
                        row(
                            "authority",
                            if e.authority.is_empty() {
                                "none — the state was inferred, not reported".to_string()
                            } else {
                                e.authority.clone()
                            },
                            if e.authority.is_empty() {
                                p.dim.as_str()
                            } else {
                                p.idle_c.as_str()
                            },
                        );
                        if !e.available {
                            row(
                                "reachable",
                                "no — the server cannot reach this pane".to_string(),
                                p.blocked.as_str(),
                            );
                        }
                    }
                    Err(why) => {
                        let budget = w.saturating_sub(1).max(1);
                        for line in tc::wrap_words(&format!(" ! agent explain: {}", why), budget) {
                            body.push(tc::seg(&[(p.unknown.as_str(), line)], w.saturating_sub(1)));
                        }
                    }
                }
                body.push(String::new());
                body.push(tc::seg(
                    &[(p.lbl.as_str(), " ── WHAT THE PANE SHOWS ── ".into())],
                    w.saturating_sub(1),
                ));
                match &d.screen {
                    Ok(text) => {
                        // Each pane line can be wider than this widget. Wrap
                        // at cell boundaries so a prompt or an error on the
                        // right of the pane is still here to read; `seg`
                        // would drop that suffix and this view has no other
                        // way to show it.
                        let budget = w.saturating_sub(2).max(1);
                        for line in text.lines() {
                            for piece in wrap_cells(line, budget) {
                                body.push(tc::seg(
                                    &[(p.dim.as_str(), format!(" {}", piece))],
                                    w.saturating_sub(1),
                                ));
                            }
                        }
                    }
                    Err(why) => {
                        let budget = w.saturating_sub(1).max(1);
                        for line in tc::wrap_words(&format!(" ! agent read: {}", why), budget) {
                            body.push(tc::seg(&[(p.unknown.as_str(), line)], w.saturating_sub(1)));
                        }
                    }
                }
            }
            body.push(String::new());
            body.push(tc::seg(
                &[(
                    p.dim.as_str(),
                    " esc closes this and puts the sections back".into(),
                )],
                w.saturating_sub(1),
            ));
        } else if let Some(why) = &absent {
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
                && busy.is_empty()
                && hidden_opscope == 0;
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

            let nothing = |of: usize| {
                tc::seg(
                    &[(
                        p.dim.as_str(),
                        format!(
                            "   none of {} match{}",
                            of,
                            search.filter().map(|f| format!(" {}", f)).unwrap_or_default()
                        ),
                    )],
                    w.saturating_sub(1),
                )
            };
            // A query that leaves nothing on the whole board says so,
            // rather than drawing six empty sections - but only when every
            // reading answered. A failed one draws as an empty list, so the
            // sections stay up to say which could not be read.
            let all_read = snapshot.is_ok()
                && agents.is_ok()
                && tasks.is_ok()
                && leases.is_ok()
                && sessions.is_ok();
            let emptied = if filtering && all_read && rows_now.is_empty() {
                tc::filtered_to_nothing(board, &hidden_idle)
            } else {
                None
            };
            if let Some(said) = emptied {
                for line in tc::wrap_words(&said, w.saturating_sub(3)) {
                    body.push(tc::seg(&[(p.dim.as_str(), format!("  {}", line))], w.saturating_sub(1)));
                }
            } else {
            // ---- AGENTS ----
            body.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── AGENTS ── ".into()),
                    (p.dim.as_str(), shown_count(&agents, agents_shown.len(), filtering)),
                ],
                w.saturating_sub(1),
            ));
            let Columns {
                name_w,
                agent_workspace_w,
                command_w,
                busy_workspace_w,
                dir_w,
                idle_workspace_w,
            } = Columns::fit(
                w,
                wide,
                [
                    widest(agent_rows.iter().map(|a| &a.name)),
                    widest(agent_rows.iter().map(|a| &a.workspace)),
                    // The worktree mark rides after the branch.
                    widest(agent_rows.iter().map(branch_of)) + 2,
                    widest(busy.iter().map(|n| &n.command)).max(1),
                    widest(busy.iter().map(|n| &n.workspace)),
                    widest(busy.iter().map(|n| parse::homely(&n.cwd))),
                    widest(resting.iter().map(|n| parse::homely(&n.cwd))),
                    widest(resting.iter().map(|n| &n.workspace)),
                ],
            );
            // AGENT sits over the mark as well as the name, hence the two.
            let mut columns = format!(
                " {:<nw$} {:<8} {:<6} {}",
                "AGENT",
                "STATE",
                "FOR",
                tc::pad("WORKSPACE", agent_workspace_w),
                nw = name_w + 2
            );
            if wide {
                columns += " BRANCH";
            }
            body.push(tc::seg(
                &[(p.dim.as_str(), tc::pad(&columns, w.saturating_sub(1)))],
                w.saturating_sub(1),
            ));
            let grouped = agents_shown
                .iter()
                .any(|a| a.workspace != agents_shown[0].workspace);
            for (i, a) in agents_shown.iter().enumerate() {
                // A gap between two agents, and where a workspace begins
                // its heading and rule - only when there are workspaces to
                // tell apart. Above `start`, so a click there is no row's.
                if i > 0 {
                    body.push(String::new());
                }
                if grouped && (i == 0 || agents_shown[i - 1].workspace != a.workspace) {
                    let n = agents_shown.iter().filter(|o| o.workspace == a.workspace).count();
                    let at_row = body.len();
                    body.extend(super::group_heading(
                        workspace_name(&a.workspace),
                        n,
                        w,
                        (&p.accent, &p.dim, &p.rule),
                        at_row,
                        &mut plots,
                    ));
                }
                let here = at == selected_row;
                let start = body.len();
                let colour = colour_of(&a.state, &p);
                // Blocked keeps a tint of its own even unselected: it is the
                // one row that has to be visible without being looked for.
                // Done does not - a green ground read as a second state
                // beside the one the row names.
                let loud = a.state == "blocked" || a.state == "done";
                let tint = if here {
                    tc::tint(tc::Role::Slot)
                } else if a.state == "blocked" {
                    tc::tint_toward(tc::Role::Red)
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
                        p.on_tint(colour)
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
                    (
                        c(&p.accent),
                        format!(" {}", tc::pad(&a.workspace, agent_workspace_w)),
                    ),
                ];
                if wide {
                    let branch = branch_of(a);
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
                let state_via = format!(
                    "state via {}",
                    if a.state_source.is_empty() {
                        "—"
                    } else {
                        &a.state_source
                    }
                );
                let id_via = if wide {
                    format!(
                        " · id via {}",
                        if a.authority.is_empty() {
                            "—"
                        } else {
                            &a.authority
                        }
                    )
                } else {
                    String::new()
                };
                // The directory takes what the provenance leaves, and never
                // less than the 34 it always had.
                let path_w = w
                    .saturating_sub(1 + 3 + 2)
                    .saturating_sub(tc::display_width(&state_via) + tc::display_width(&id_via))
                    .max(34);
                let mut detail = vec![
                    (
                        c(if loud || here { &p.txt } else { &p.dim }),
                        format!("   {}  ", parse::tail_path(&parse::homely(&a.cwd), path_w)),
                    ),
                    (c(&p.dim), state_via),
                ];
                if wide {
                    detail.push((c(&p.dim), id_via));
                }
                if loud || here {
                    detail.push((tint.clone(), " ".repeat(w)));
                }
                let refs: Vec<(&str, String)> = detail
                    .iter()
                    .map(|(c, t)| (c.as_str(), t.clone()))
                    .collect();
                body.push(tc::seg(&refs, w.saturating_sub(1)));
                for (unread, text) in a.work.rows(w.saturating_sub(1)) {
                    let mut line = vec![(c(if unread { &p.unknown } else { &p.working }), text)];
                    if loud || here {
                        line.push((tint.clone(), " ".repeat(w)));
                    }
                    let refs: Vec<(&str, String)> =
                        line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                    body.push(tc::seg(&refs, w.saturating_sub(1)));
                }
                spans.push(start..body.len());
                at += 1;
            }
            if agent_rows.is_empty() {
                let (said, bad) = empty_or_why(&agents, "no agents under this session");
                body.push(tc::seg(
                    &[(
                        if bad {
                            p.unknown.as_str()
                        } else {
                            p.dim.as_str()
                        },
                        said,
                    )],
                    w.saturating_sub(1),
                ));
            } else if agents_shown.is_empty() {
                body.push(nothing(agent_rows.len()));
            }

            // ---- TASKS ----
            body.push(String::new());
            body.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── TASKS ── ".into()),
                    (p.dim.as_str(), shown_count(&tasks, tasks_shown.len(), filtering)),
                    // A list with tasks in it and nothing claimable are
                    // different sentences, and the count alone reads as
                    // though something were ready to pick up.
                    match &next {
                        Ok(n) if n.none && !task_rows.is_empty() => {
                            (p.dim.as_str(), "   none ready to claim".to_string())
                        }
                        Ok(n) if !n.none && !n.id.is_empty() => {
                            (p.done.as_str(), format!("   {} ready to claim", n.id))
                        }
                        Err(why) if !why.starts_with("not read") => {
                            (p.unknown.as_str(), format!("   next: {}", why))
                        }
                        _ => (p.dim.as_str(), String::new()),
                    },
                ],
                w.saturating_sub(1),
            ));
            for t in &tasks_shown {
                let here = at == selected_row;
                let start = body.len();
                let tint = if here {
                    tc::tint(tc::Role::Slot)
                } else {
                    String::new()
                };
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
                        p.on_tint(colour)
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
                        if bad {
                            p.unknown.as_str()
                        } else {
                            p.dim.as_str()
                        },
                        said,
                    )],
                    w.saturating_sub(1),
                ));
            } else if tasks_shown.is_empty() {
                body.push(nothing(task_rows.len()));
            }

            // ---- LEASES ----
            body.push(String::new());
            body.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── LEASES ── ".into()),
                    (p.dim.as_str(), shown_count(&leases, leases_shown.len(), filtering)),
                ],
                w.saturating_sub(1),
            ));
            for l in &leases_shown {
                let here = at == selected_row;
                let start = body.len();
                let tint = if here {
                    tc::tint(tc::Role::Slot)
                } else {
                    String::new()
                };
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
                        p.on_tint(colour)
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
                        if bad {
                            p.unknown.as_str()
                        } else {
                            p.dim.as_str()
                        },
                        said,
                    )],
                    w.saturating_sub(1),
                ));
            } else if leases_shown.is_empty() {
                body.push(nothing(lease_rows.len()));
            }

            // ---- RESUMABLE ----
            // Sessions with no pane holding them. `agent sessions` answers
            // for the machine and not for this session, so the heading says
            // how many are in a workspace this session has open rather than
            // presenting the whole count as though it were the session's.
            body.push(String::new());
            let inside = resumable_rows
                .iter()
                .filter(|r| r.in_workspace == Some(true))
                .count();
            let membership_unread = resumable_rows.iter().any(|r| r.in_workspace.is_none());
            body.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── RESUMABLE ── ".into()),
                    (p.dim.as_str(), shown_count(&sessions, resumable_shown.len(), filtering)),
                    (
                        if membership_unread {
                            p.unknown.as_str()
                        } else {
                            p.dim.as_str()
                        },
                        if resumable_rows.is_empty() {
                            String::new()
                        } else if membership_unread {
                            "   cannot say which are in an open workspace".into()
                        } else {
                            format!("   {} in an open workspace", inside)
                        },
                    ),
                ],
                w.saturating_sub(1),
            ));
            for r in &resumable_shown {
                let here = at == selected_row;
                let start = body.len();
                let tint = if here {
                    tc::tint(tc::Role::Slot)
                } else {
                    String::new()
                };
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
                        p.on_tint(colour)
                    };
                    format!("{}{}", tint, colour)
                };
                let mut line = vec![
                    (c(&p.accent), format!("{}◇ ", if here { "▸" } else { " " })),
                    (c(&p.txt), tc::pad(&r.kind, 8)),
                    (
                        c(if r.in_workspace == Some(true) {
                            &p.idle_c
                        } else if r.in_workspace.is_none() {
                            &p.unknown
                        } else {
                            &p.dim
                        }),
                        format!(
                            " {}",
                            parse::tail_path(
                                &parse::homely(&r.cwd),
                                // What the kind and the membership leave,
                                // and never less than the 34 it always had.
                                w.saturating_sub(1 + 3 + 8 + 1 + 2)
                                    .saturating_sub(
                                        membership_label(r.in_workspace, wide).chars().count()
                                    )
                                    .max(34),
                            )
                        ),
                    ),
                    (
                        c(if r.in_workspace == Some(true) {
                            &p.idle_c
                        } else if r.in_workspace.is_none() {
                            &p.unknown
                        } else {
                            &p.dim
                        }),
                        format!("  {}", membership_label(r.in_workspace, wide)),
                    ),
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
            if resumable_rows.is_empty() {
                let (said, bad) =
                    empty_or_why(&sessions, "no resumable sessions — every agent has a pane");
                body.push(tc::seg(
                    &[(
                        if bad {
                            p.unknown.as_str()
                        } else {
                            p.dim.as_str()
                        },
                        said,
                    )],
                    w.saturating_sub(1),
                ));
            } else if resumable_shown.is_empty() {
                body.push(nothing(resumable_rows.len()));
            }

            // ---- PANES ----
            body.push(String::new());
            let mut heading = vec![
                (p.lbl.as_str(), " ── PANES ── ".into()),
                (
                    p.dim.as_str(),
                    // Unstated panes are counted apart, as unfiltered: they
                    // are listed here but not known to be off a prompt.
                    if filtering {
                        let running_shown =
                            busy_shown.iter().filter(|n| !n.status.is_empty()).count();
                        format!(
                            "{} pane{} not at a prompt",
                            super::search::count(running_shown, running, true),
                            plural(running)
                        )
                    } else {
                        format!("{} pane{} not at a prompt", running, plural(running))
                    },
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
            if wide && !busy_shown.is_empty() {
                body.push(tc::seg(
                    &[(
                        p.dim.as_str(),
                        tc::pad(
                            &format!(
                                " {:<cw$} {:<8} {} {}",
                                "IN THE PANE",
                                "STATE",
                                tc::pad("WORKSPACE", busy_workspace_w),
                                "DIRECTORY",
                                cw = command_w + 2
                            ),
                            w.saturating_sub(1),
                        ),
                    )],
                    w.saturating_sub(1),
                ));
            }
            for n in &busy_shown {
                let here = at == selected_row;
                let start = body.len();
                let tint = if here {
                    tc::tint(tc::Role::Slot)
                } else {
                    String::new()
                };
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
                        p.on_tint(colour)
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
                            command_w,
                        ),
                    ),
                    (
                        c(colour_of(&n.status, &p)),
                        format!(
                            " {}",
                            tc::pad(if unstated_here { "no state" } else { &n.status }, 8)
                        ),
                    ),
                    (
                        c(&p.accent),
                        format!(" {}", tc::pad(&n.workspace, busy_workspace_w)),
                    ),
                ];
                if wide {
                    line.push((c(&p.dim), format!(" {}", parse::homely(&n.cwd))));
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
                let empty = process_filter::empty_message(
                    hidden_opscope,
                    resting.len(),
                    if panes.is_empty() {
                        "no panes in this session"
                    } else {
                        "every other pane is idle at a prompt"
                    },
                );
                let (said, bad) = empty_or_why(&snapshot.clone().map(|s| s.panes), &empty);
                body.push(tc::seg(
                    &[(
                        if bad {
                            p.unknown.as_str()
                        } else {
                            p.dim.as_str()
                        },
                        said,
                    )],
                    w.saturating_sub(1),
                ));
            } else if busy_shown.is_empty() {
                body.push(nothing(busy.len()));
            }

            // ---- IDLE ----
            if idle_listed {
                body.push(String::new());
                body.push(tc::seg(
                    &[
                        (p.lbl.as_str(), " ── IDLE ── ".into()),
                        (
                            p.dim.as_str(),
                            format!(
                                "{} pane{} at a prompt",
                                super::search::count(resting_shown.len(), resting.len(), filtering),
                                plural(resting.len())
                            ),
                        ),
                    ],
                    w.saturating_sub(1),
                ));
                if resting_shown.is_empty() {
                    body.push(nothing(resting.len()));
                }
                for n in &resting_shown {
                    let here = at == selected_row;
                    let start = body.len();
                    let tint = if here {
                        tc::tint(tc::Role::Slot)
                    } else {
                        String::new()
                    };
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
                            p.on_tint(colour)
                        };
                        format!("{}{}", tint, colour)
                    };
                    let mut line = vec![
                        (c(&p.idle_c), format!("{}▫ ", if here { "▸" } else { " " })),
                        (
                            c(&p.idle_c),
                            tc::pad(&parse::tail_path(&parse::homely(&n.cwd), dir_w), dir_w + 1),
                        ),
                        (c(&p.accent), tc::pad(&n.workspace, idle_workspace_w)),
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
        }

        // ---- the window onto it ----
        // The header is pinned, but not at the price of the body: on a pane
        // eight rows tall a five-row header and a four-row footer leave
        // nothing, and a body with no rows in it reads as a session with
        // nothing in it. So the header gives its last lines back, from the
        // bottom, until there is something to scroll. The title and the
        // counts never go: those two are the widget.
        let head = prune_head(head, filter_line, footer.len(), h);
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
        // The spans this widget already keeps - one per entry, recorded
        // while the body was built so the window could be brought to the
        // selected one - are exactly what a click needs read the other way
        // round. One entry per row it covers, so a click on any line of a
        // multi-line entry picks that entry.
        //
        // Shifted by the header, because this widget keeps `head` and `body`
        // in two vecs rather than splitting one - so a span is an index into
        // `body` while a click is a row of `head` ++ `body[window]`. Without
        // the shift every click lands however many rows the header is tall
        // further down the list, and the first few entries cannot be reached
        // at all.
        let head_len = head.len();
        (placed, list_head, list_scroll) = (
            spans
                .iter()
                .enumerate()
                .flat_map(|(i, span)| span.clone().map(move |row| (row + head_len, i)))
                .collect(),
            head_len,
            window.start,
        );
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
                    if *ok {
                        p.done.as_str()
                    } else {
                        p.blocked.as_str()
                    },
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
        // Where the footer lands on the frame, after the padding that pushes
        // it to the bottom, and one column in because that is the indent
        // above. Registered every frame: the footer moves when the pane
        // resizes and wraps onto a second line when it narrows, and a
        // placement kept from an older frame sends whatever key used to be
        // under the pointer.
        let foot_top = rows.len();
        rows.extend(footer);
        let plots: Vec<_> = plots.into_iter().filter_map(|mut p| {
            p.y += head_len;
            p.in_viewport(start, head_len, room)
        }).collect();
        tc::draw_plots(&rows, w, h, &plots);
        keyboard.footer_at(&packed, foot_top, 1);
        keyboard.wait(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_id_is_exact_only_when_bound_to_the_pane() {
        let mut agent = Agent { kind: "claude".into(), cwd: "/srv/repo".into(), pane: "p1".into(), ..Default::default() };
        let a = parse::Resumable { kind: "claude".into(), cwd: "/srv/repo".into(), session_id: "one".into(), ..Default::default() };
        assert_eq!(log_session_hint(&agent, std::slice::from_ref(&a)), None);
        let mut b = a.clone(); b.session_id = "two".into();
        assert_eq!(log_session_hint(&agent, &[a.clone(), b.clone()]), None);
        b.pane = "p1".into();
        assert_eq!(log_session_hint(&agent, &[a, b]), Some(("two", false)));
        agent.session_id = "bound".into();
        assert_eq!(log_session_hint(&agent, &[]), Some(("bound", false)));
    }

    #[test]
    fn agents_group_by_workspace_with_the_most_urgent_workspace_first() {
        let agent = |pane: &str, workspace: &str| Agent {
            pane: pane.into(),
            workspace: workspace.into(),
            ..Agent::default()
        };
        // Already worst-first, as the poll sorts them before grouping.
        let sorted = vec![agent("1", "api"), agent("2", "site"), agent("3", "api"), agent("4", "")];
        let panes: Vec<String> = by_workspace(sorted).into_iter().map(|a| a.pane).collect();
        assert_eq!(panes, ["1", "3", "2", "4"]);
        assert_eq!(workspace_name(""), "no workspace");
        assert_eq!(workspace_name("api"), "api");
    }

    thread_local! {
        static SNAPSHOT_READS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    /// Clears the command stand-in even when the test panics, so a later
    /// test on this thread cannot answer `luvus` from the fixture.
    struct ClearLuvusOverride;

    impl Drop for ClearLuvusOverride {
        fn drop(&mut self) {
            LUVUS_TEXT_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
        }
    }

    fn scripted_luvus(_session: &str, args: &[&str]) -> Result<String, String> {
        if args == ["uhp", "snapshot"] {
            let attempt = SNAPSHOT_READS.with(|reads| {
                let attempt = reads.get();
                reads.set(attempt + 1);
                attempt
            });
            return if attempt == 0 {
                Ok(
                    r#"{"result":{"event_sequence":61,"session":"fixture","workspaces":[]}}"#
                        .into(),
                )
            } else {
                Err("connection refused".into())
            };
        }
        Err("fixture command has no answer".into())
    }

    const AGENTS: &str = r#"{"id":"1","result":{"agents":[{"agent":"claude","status":"done","pane":"2","workspace_name":"projects"}],"revision":300,"type":"agent_list"}}"#;

    /// Luvus 0.14: JSON by default, and `--json` refused outright.
    fn luvus_0_14(_session: &str, args: &[&str]) -> Result<String, String> {
        match args {
            ["agent", "list"] => Ok(AGENTS.into()),
            ["agent", "list", "--json"] => {
                Err("Error: unexpected agent list argument --json, Try luvus help agent.".into())
            }
            _ => Err("fixture command has no answer".into()),
        }
    }

    /// Before 0.14: a table unless asked for JSON.
    fn luvus_before_0_14(_session: &str, args: &[&str]) -> Result<String, String> {
        match args {
            ["agent", "list"] => Ok("PANE  AGENT   STATUS\n2     claude  done\n".into()),
            ["agent", "list", "--json"] => Ok(AGENTS.into()),
            _ => Err("fixture command has no answer".into()),
        }
    }

    #[test]
    fn agents_are_read_from_luvus_before_and_after_it_dropped_the_json_flag() {
        for (hook, which) in [
            (luvus_0_14 as fn(&str, &[&str]) -> Result<String, String>, "0.14"),
            (luvus_before_0_14, "before 0.14"),
        ] {
            LUVUS_TEXT_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(hook));
            let _clear = ClearLuvusOverride;
            let agents = luvus_json("fixture", &["agent", "list"])
                .and_then(|text| parse::parse_agents(&text))
                .unwrap_or_else(|why| panic!("{which}: {why}"));
            assert_eq!(agents.len(), 1, "{which}");
            assert_eq!(agents[0].state, "done", "{which}");
        }
    }

    #[test]
    fn a_list_neither_form_can_read_says_what_the_plain_call_said() {
        fn down(_session: &str, args: &[&str]) -> Result<String, String> {
            Err(if args.contains(&"--json") { "flag refused" } else { "no luvus server running" }.into())
        }
        LUVUS_TEXT_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(down));
        let _clear = ClearLuvusOverride;
        assert_eq!(luvus_json("fixture", &["agent", "list"]), Err("no luvus server running".into()));
    }

    #[test]
    fn latest_successful_event_baseline_survives_a_failed_snapshot() {
        SNAPSHOT_READS.with(|reads| reads.set(0));
        LUVUS_TEXT_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(scripted_luvus));
        let _clear = ClearLuvusOverride;
        let state = Arc::new(Mutex::new(State::default()));
        let mut seen = Seen::default();

        poll(&state, &mut seen, "fixture");
        assert_eq!(
            event_baseline(&state),
            Some((1, 61)),
            "a parsed snapshot is what publishes the cursor"
        );
        assert!(state.lock().unwrap().snapshot.is_ok());

        poll(&state, &mut seen, "fixture");
        let guard = state.lock().unwrap();
        assert!(
            guard.snapshot.is_err(),
            "the second read has to fail or the cursor was never at risk"
        );
        assert_eq!(guard.snapshot_epoch, 1);
        assert_eq!(guard.event_cursor, Some(61));
        drop(guard);
        assert_eq!(event_baseline(&state), Some((1, 61)));
        assert_eq!(SNAPSHOT_READS.with(|reads| reads.get()), 2);
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
    fn local_process_scan_works_on_the_current_host() {
        if !tc::missing(&["ps"]).is_empty() {
            return; // Optional at runtime; fixture tests cover unavailable scans.
        }
        let table = local_processes().expect("ps should supply the local process table");
        assert!(parse::parse_process_flags(&table).is_ok());
    }

    #[test]
    fn pane_summary_counts_both_filters_without_double_counting() {
        // Four panes: one Opscope, two idle, and one busy. `resting` is
        // counted after removing Opscope panes, as it is in the live view.
        assert_eq!(pane_summary(4, 1, 2, false), " · 1 visible pane");
        assert_eq!(pane_summary(4, 1, 2, true), " · 3 visible panes");
        assert_eq!(pane_summary(4, 0, 2, false), " · 2 visible panes");
        assert_eq!(pane_summary(4, 0, 2, true), " · 4 panes");
        assert_eq!(pane_summary(1, 1, 0, false), " · 0 visible panes");
    }

    #[test]
    fn hiding_the_idle_panes_is_a_filter_the_pane_states() {
        assert!(idle_filter(true, 4).is_empty(), "nothing is hidden");
        assert!(
            idle_filter(false, 0).is_empty(),
            "a filter holding nothing back is not worth a row"
        );
        assert_eq!(idle_filter(false, 4), vec!["4 idle panes hidden"]);
        assert_eq!(idle_filter(false, 1), vec!["1 idle pane hidden"]);
        let said = tc::filter_row(5, 9, &idle_filter(false, 4)).expect("a filter is on");
        assert_eq!(said, "5 of 9 shown · 4 idle panes hidden");
    }

    #[test]
    fn a_short_pane_keeps_the_idle_filter() {
        // Title and counts never go; everything after can. On a pane that
        // has to give the body three rows, the old pop would take the
        // blank and then the filter — the list stays filtered and nothing
        // says so.
        let head = vec![
            "title".into(),
            "counts".into(),
            "session".into(),
            "waiting".into(),
            "checkout".into(),
            String::new(),
        ];
        let filter = Some(" 5 of 9 shown · 4 idle panes hidden".into());
        // 6 header + 2 footer + 1 note = 9; FLOOR 3 wants h >= 12 to skip
        // the prune. Ten rows is the short pane Codex named.
        let kept = prune_head(head, filter, 2, 10);
        assert!(
            kept.iter().any(|row| row.contains("idle panes hidden")),
            "the filter was pruned: {kept:?}"
        );
        assert_eq!(kept[0], "title");
        assert_eq!(kept[1], "counts");
        // Without a filter the same pane still gives lines back.
        let bare = vec![
            "title".into(),
            "counts".into(),
            "session".into(),
            "checkout".into(),
            String::new(),
        ];
        let pruned = prune_head(bare, None, 2, 10);
        assert!(
            pruned.len() < 5,
            "an unfiltered header still prunes: {pruned:?}"
        );
        assert!(!pruned.iter().any(|row| row.contains("idle")));
    }

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
    fn a_wide_pane_shows_the_whole_workspace() {
        // The workspace was cut at 14 cells however wide the pane: one of
        // 30 on a 200-column pane lost half of itself beside blank space.
        let c = Columns::fit(200, true, [10, 30, 20, 16, 30, 40, 50, 30]);
        assert_eq!((c.name_w, c.agent_workspace_w), (10, 30));
        assert_eq!((c.command_w, c.busy_workspace_w), (16, 30));
        assert_eq!((c.dir_w, c.idle_workspace_w), (50, 30));
    }

    #[test]
    fn no_row_grows_past_the_pane_it_was_fitted_to() {
        // Each row's fixed cells plus its padded text columns, against the
        // width they were fitted for, from the threshold up. The last
        // column of a row is unpadded and `seg` clips it at the edge.
        let long = [60, 80, 60, 60, 80, 90, 90, 80];
        for w in 76..=240 {
            let c = Columns::fit(w, true, long);
            assert!(
                3 + c.name_w + 16 + 1 + c.agent_workspace_w <= w - 1,
                "agent row at {}",
                w
            );
            assert!(
                3 + c.command_w + 9 + 1 + c.busy_workspace_w <= w - 1,
                "pane row at {}",
                w
            );
            assert!(
                3 + c.dir_w + 1 + c.idle_workspace_w <= w - 1,
                "idle row at {}",
                w
            );
        }
        // At the threshold the old widths still stand.
        let at = Columns::fit(76, true, long);
        assert!(at.name_w >= 8 && at.agent_workspace_w >= 14 && at.command_w >= 12);
    }

    #[test]
    fn a_pane_line_wider_than_the_widget_wraps_without_losing_spaces() {
        assert_eq!(wrap_cells("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        // Leading spaces are the indent; collapsing them would make two
        // different prompt lines look the same.
        assert_eq!(wrap_cells("  keep", 4), vec!["  ke", "ep"]);
        assert_eq!(wrap_cells("", 4), vec![""]);
        // Width zero still has to say something: dropping the text is
        // indistinguishable from a blank pane.
        assert_eq!(wrap_cells("left intact", 0), vec!["left intact"]);
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
    fn the_filter_matches_what_each_row_shows() {
        let agent = Row::Agent(Agent {
            name: "claude".into(),
            workspace: "api".into(),
            branch: "feature/login".into(),
            cwd: "/srv/api-server".into(),
            ..Agent::default()
        });
        let fields = agent.search_fields();
        assert!(super::super::search::matches("fl", &fields), "{fields:?}");
        assert!(super::super::search::matches("api", &fields));
        assert!(!fields.iter().any(String::is_empty));
        let task = Row::Task(Task { id: "T-7".into(), title: "Fix the login page".into(), ..Task::default() });
        assert!(super::super::search::matches("login", &task.search_fields()));
        let pane = Row::Pane(Pane { command: "npm".into(), ..Pane::default() });
        assert!(super::super::search::matches("no state", &pane.search_fields()));
        assert!(!super::super::search::matches("zzz", &pane.search_fields()));
    }

    #[test]
    fn the_cursor_follows_its_row_across_a_rebuild() {
        let pane = |id: &str| Row::Pane(Pane { pane_id: id.into(), ..Pane::default() });
        let before = vec![pane("a"), pane("b")];
        let after = vec![pane("new"), pane("a"), pane("b")];
        assert_eq!(reselect(&before, 1, &after, false), 2, "still on b");
        let gone = vec![pane("a")];
        assert_eq!(reselect(&before, 1, &gone, false), 1, "b went: left for the clamp");
        assert_eq!(reselect(&before, 1, &gone, true), 0, "the filter hid b: first match");
        // The same id in a different section is a different row.
        let task = Row::Task(Task { id: "b".into(), ..Task::default() });
        assert_eq!(reselect(&before, 1, &[task], true), 0);
    }

    #[test]
    fn a_failed_source_has_no_count_to_print() {
        let empty: Result<Vec<u8>, String> = Ok(Vec::new());
        assert_eq!(shown_count(&empty, 0, false), "0");
        let listed: Result<Vec<u8>, String> = Ok(vec![1, 2]);
        assert_eq!(shown_count(&listed, 2, false), "2");
        assert_eq!(shown_count(&listed, 1, true), "1 of 2");
        let failed: Result<Vec<u8>, String> = Err("timeout".into());
        assert_eq!(shown_count(&failed, 0, false), "unread");
        assert_ne!(shown_count(&failed, 0, true), "0");
    }

    fn said_of(line: &[(&str, String)]) -> String {
        line.iter().map(|(_, t)| t.as_str()).collect()
    }

    #[test]
    fn a_failed_checkout_is_a_row_not_a_silence() {
        let p = palette();
        let git: Result<GitState, String> = Err("luvus did not answer in 15s".into());
        let trees: Result<Vec<Worktree>, String> = Ok(Vec::new());
        let said = said_of(&checkout_line(&git, &trees, "opscope", &p));
        assert!(said.contains("checkout unread"));
        assert!(said.contains("did not answer"));
        assert!(said.contains("opscope"));
        let trees: Result<Vec<Worktree>, String> = Err("luvus did not answer in 15s".into());
        let said = said_of(&checkout_line(&git, &trees, "opscope", &p));
        assert!(said.contains("worktrees unread"));
    }

    #[test]
    fn a_directory_that_is_not_a_repository_is_a_dash() {
        let p = palette();
        let git: Result<GitState, String> =
            Err("fatal: not a git repository (or any of the parent directories): .git".into());
        let trees: Result<Vec<Worktree>, String> = Err("fatal: not a git repository".into());
        let said = said_of(&checkout_line(&git, &trees, "home", &p));
        assert!(said.contains("not a repository"));
        assert!(!said.contains("unread"));
    }

    #[test]
    fn a_worktree_failure_stays_on_the_checkout_line() {
        let p = palette();
        let git = Ok(GitState {
            branch: "main".into(),
            ..Default::default()
        });
        let trees: Result<Vec<Worktree>, String> = Err("luvus did not answer in 15s".into());
        let said = said_of(&checkout_line(&git, &trees, "opscope", &p));
        assert!(said.contains("worktrees unread"));
        assert!(said.contains("on main"));
        assert!(said.contains("opscope"));
    }

    #[test]
    fn a_narrow_row_still_says_whether_the_workspace_is_open() {
        assert_eq!(membership_label(Some(true), false), "open");
        assert_eq!(membership_label(Some(false), false), "away");
        assert_eq!(membership_label(None, false), "?");
        assert_eq!(membership_label(Some(true), true), "in an open workspace");
        assert_eq!(membership_label(None, true), "workspace unread");
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
