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

use parse::{
    Absence, Agent, Explanation, GitState, Lease, NextTask, Pane, Resumable, Snapshot, Task,
    Worktree,
};

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
    tc::run(&argv, RUN_TIMEOUT).map_err(|why| parse::sanitize_error(&why))
}

/// Focus a pane. `pane focus` jumps to the pane's workspace and tab too,
/// so there is nothing for a second command to add.
fn focus_pane(session: &str, pane: &str) -> bool {
    luvus_text(session, &["pane", "focus", pane]).is_ok()
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
    // the same for every reading.
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

    let agents = luvus_text(session, &["agent", "list", "--json"])
        .and_then(|text| parse::parse_agents(&text));
    let tasks =
        luvus_text(session, &["task", "list", "--json"]).and_then(|text| parse::parse_tasks(&text));
    let leases = luvus_text(session, &["lease", "list", "--json"])
        .and_then(|text| parse::parse_leases(&text));
    let next =
        luvus_text(session, &["task", "next"]).and_then(|text| parse::parse_next_task(&text));
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
        guard.next = next;
        guard.sessions = sessions;
        guard.git = git;
        guard.worktrees = worktrees;
        guard.absent = None;
        guard.read = true;
    }
    seen.first_poll = false;
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
) {
    std::thread::spawn(move || {
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

/// The count a heading may print. A failed source has no number.
fn shown_count<T>(reading: &Result<Vec<T>, String>) -> String {
    match reading {
        Ok(rows) => rows.len().to_string(),
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
        let (snapshot, agents, tasks, leases, absent, err, read, next, sessions, git, worktrees) =
            match state.lock() {
                Ok(g) => (
                    g.snapshot.clone(),
                    g.agents.clone(),
                    g.tasks.clone(),
                    g.leases.clone(),
                    g.absent.clone(),
                    g.err.clone(),
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

        rows_now = agent_rows
            .iter()
            .cloned()
            .map(Row::Agent)
            .chain(task_rows.iter().cloned().map(Row::Task))
            .chain(lease_rows.iter().cloned().map(Row::Lease))
            .chain(resumable_rows.iter().cloned().map(Row::Resumable))
            .chain(busy.iter().cloned().map(Row::Pane))
            .chain(resting.iter().filter(|_| show_idle).cloned().map(Row::Pane))
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

        let idle_listed = show_idle && !resting.is_empty();
        sections = [
            (0, !agent_rows.is_empty()),
            (agent_rows.len(), !task_rows.is_empty()),
            (agent_rows.len() + task_rows.len(), !lease_rows.is_empty()),
            (
                agent_rows.len() + task_rows.len() + lease_rows.len(),
                !resumable_rows.is_empty(),
            ),
            (
                agent_rows.len() + task_rows.len() + lease_rows.len() + resumable_rows.len(),
                !busy.is_empty(),
            ),
            (
                agent_rows.len()
                    + task_rows.len()
                    + lease_rows.len()
                    + resumable_rows.len()
                    + busy.len(),
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
        let hidden_idle = idle_filter(show_idle, resting.len());
        // `others`, not `panes`: a pane holding a recognised agent is in
        // AGENTS rather than in this list, and counting it here would
        // report it as something the filter had hidden.
        // Held out of `head` until after the prune: that loop pops from
        // the bottom, and this row would be the first content it took.
        let filter_line = tc::filter_row(busy.len(), others.len(), &hidden_idle).map(|said| {
            tc::seg(
                &[(p.dim.as_str(), format!(" {}", said))],
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
                "AGENT",
                "STATE",
                "FOR",
                "WORKSPACE",
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
                let refs: Vec<(&str, String)> = detail
                    .iter()
                    .map(|(c, t)| (c.as_str(), t.clone()))
                    .collect();
                body.push(tc::seg(&refs, w.saturating_sub(1)));
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
            }

            // ---- TASKS ----
            body.push(String::new());
            body.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── TASKS ── ".into()),
                    (p.dim.as_str(), shown_count(&tasks)),
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
            for t in &task_rows {
                let here = at == selected_row;
                let start = body.len();
                let tint = if here {
                    tc::bg(38, 56, 76)
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
                        if bad {
                            p.unknown.as_str()
                        } else {
                            p.dim.as_str()
                        },
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
                let tint = if here {
                    tc::bg(38, 56, 76)
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
                        if bad {
                            p.unknown.as_str()
                        } else {
                            p.dim.as_str()
                        },
                        said,
                    )],
                    w.saturating_sub(1),
                ));
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
                    (p.dim.as_str(), shown_count(&sessions)),
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
            for r in &resumable_rows {
                let here = at == selected_row;
                let start = body.len();
                let tint = if here {
                    tc::bg(38, 56, 76)
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
                        colour
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
                        format!(" {}", parse::tail_path(&parse::homely(&r.cwd), 34)),
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
                            &format!(
                                " {:<12} {:<8} {:<14} {:<20}",
                                "IN THE PANE", "STATE", "WORKSPACE", "DIRECTORY"
                            ),
                            w.saturating_sub(1),
                        ),
                    )],
                    w.saturating_sub(1),
                ));
            }
            for n in &busy {
                let here = at == selected_row;
                let start = body.len();
                let tint = if here {
                    tc::bg(38, 56, 76)
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
                            tc::pad(if unstated_here { "no state" } else { &n.status }, 8)
                        ),
                    ),
                    (c(&p.accent), format!(" {}", tc::pad(&n.workspace, 14))),
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
                        if bad {
                            p.unknown.as_str()
                        } else {
                            p.dim.as_str()
                        },
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
                            format!(
                                "{} pane{} at a prompt",
                                resting.len(),
                                plural(resting.len())
                            ),
                        ),
                    ],
                    w.saturating_sub(1),
                ));
                for n in &resting {
                    let here = at == selected_row;
                    let start = body.len();
                    let tint = if here {
                        tc::bg(38, 56, 76)
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
        rows.extend(footer);
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_failed_source_has_no_count_to_print() {
        let empty: Result<Vec<u8>, String> = Ok(Vec::new());
        assert_eq!(shown_count(&empty), "0");
        let listed: Result<Vec<u8>, String> = Ok(vec![1, 2]);
        assert_eq!(shown_count(&listed), "2");
        let failed: Result<Vec<u8>, String> = Err("timeout".into());
        assert_eq!(shown_count(&failed), "unread");
        assert_ne!(shown_count(&failed), "0");
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
