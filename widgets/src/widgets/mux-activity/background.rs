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

//! Claude background agents, the same on every board (OPS-276).
//!
//! A background agent has no pane, so each board draws it as an agent row
//! in a group of its own, `background agents · via jobs`, sorted with the
//! board's other agents - a blocked one is with the agents that need you.
//! What a row says, what `e` explains and what Enter answers is decided
//! here once, so Rex, Herdr and Luvus cannot drift apart.

use opscope_core as tc;
use tc::agent_work::{Job, JobState, Jobs, Table, Work};

/// The group a board files background agents under, in place of a
/// session or workspace. Not a real id any multiplexer hands out.
pub const GROUP: &str = "\u{0}claude-jobs";

/// Enter on a background agent: there is no pane to focus.
pub const NO_PANE: &str = "a background agent has no pane - `claude agents` opens it";

/// The group's heading. Finished jobs are counted here rather than drawn,
/// so the rows under it are never read as every job there is.
pub fn group_label(jobs: &Jobs) -> String {
    let mut label = "background agents · via jobs".to_string();
    if jobs.finished > 0 {
        label.push_str(&format!(" · {} finished not shown", jobs.finished));
    }
    label
}

/// The state word a board ranks and colours by: the reported one, or
/// `unknown` for a job whose file could not be read.
pub fn state_word(job: &Job) -> &'static str {
    job.state.map_or("unknown", JobState::word)
}

/// What the row is called: the job's name, else its id.
pub fn title(job: &Job) -> String {
    if job.name.is_empty() {
        job.short.clone()
    } else {
        job.name.clone()
    }
}

/// The one line that says what it is doing or waiting for.
pub fn message(job: &Job) -> String {
    if job.state == Some(JobState::Blocked) && !job.needs.is_empty() {
        job.needs.clone()
    } else {
        job.detail.clone()
    }
}

/// A warning the row carries: the file it could not read, or a process
/// that is gone while the file still says the agent is busy.
pub fn why(job: &Job) -> String {
    if let Some(why) = &job.unread {
        return format!("{why} ({})", job.dir);
    }
    match (job.running, job.state) {
        (Some(false), Some(JobState::Working | JobState::Blocked)) => {
            "its process is not running; this is the last state it wrote".into()
        }
        (None, Some(JobState::Done)) if !job.unchecked.is_empty() => {
            format!("it says done, but whether its process still runs is unknown: {}", job.unchecked)
        }
        _ => String::new(),
    }
}

/// The facts after the message on a row: the count from `inFlight`.
pub fn facts(job: &Job) -> Vec<String> {
    let mut out = Vec::new();
    let flight = job.in_flight_words();
    if !flight.is_empty() {
        out.push(flight);
    }
    out
}

/// What a row that has no room for prose of its own says under a
/// background agent, wrapped to `w` and never cut: the message and the
/// in-flight count, then any warning (`true`), each indented three.
pub fn note_rows(job: &Job, w: usize) -> Vec<(bool, String)> {
    let said: Vec<String> = [message(job)].into_iter().chain(facts(job)).filter(|s| !s.is_empty()).collect();
    let warn = why(job);
    let mut out = Vec::new();
    for (alarm, text) in [(false, said.join(" · ")), (true, if warn.is_empty() { warn } else { format!("⚠ {warn}") })] {
        if text.is_empty() {
            continue;
        }
        out.extend(tc::wrap_words(&text, w.saturating_sub(4).max(1)).into_iter().map(|l| (alarm, format!("   {l}"))));
    }
    out
}

/// Seconds the state has held at `at`, and whether that is exact or only
/// a lower bound (drawn with `≥`).
pub fn held(job: &Job, at: f64) -> Option<(f64, bool)> {
    job.held_since.map(|(since, exact)| ((at - since).max(0.0), exact))
}

/// `≥3m` or `3m`, or `-` when the file gave no time at all.
pub fn held_words(job: &Job, at: f64, ago: fn(f64) -> String) -> String {
    match held(job, at) {
        Some((secs, exact)) => format!("{}{}", if exact { "" } else { "≥" }, ago(secs)),
        None => "-".into(),
    }
}

/// The work under a background agent: its unfinished subagents from its
/// own `fan`, and the shells under its process in the process table.
pub fn work(job: &Job, table: &Result<Table, String>) -> Work {
    match job.pid {
        Some(pid) => Work::of(table, Some(pid)),
        None => Work::default(),
    }
    .with_fan(&job.fan)
}

/// Take out every job a multiplexer already shows under one of
/// `sessions`, returning them so their work can join that agent's row.
/// One agent is drawn once, whichever source saw it first.
pub fn take_reported(jobs: &mut Jobs, sessions: &[String]) -> Vec<Job> {
    let (taken, kept): (Vec<Job>, Vec<Job>) = std::mem::take(&mut jobs.shown)
        .into_iter()
        .partition(|j| sessions.iter().any(|s| j.is_session(s)));
    jobs.shown = kept;
    taken
}

/// The `e` screen for a background agent, as `(colour kind, text)` lines:
/// `lbl`, `txt` and `dim`, or a state word to colour by. Every `fan` entry
/// is listed, finished or not, with its age.
pub fn explain_lines(job: &Job, at: f64, ago: fn(f64) -> String) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut put = |kind: &str, text: String| out.push((kind.to_string(), text));
    put("lbl", " ── BACKGROUND AGENT ──".into());
    let field = |name: &str, value: &str| format!("   {:<10} {}", name, value);
    put("txt", field("name", &title(job)));
    put(state_word(job), field("state", &format!("{} for {}", state_word(job), held_words(job, at, ago))));
    if let Some(why) = &job.unread {
        put("unknown", field("unread", why));
    }
    if !job.detail.is_empty() {
        put("txt", field("detail", &job.detail));
    }
    if !job.needs.is_empty() {
        put("blocked", field("needs", &job.needs));
    }
    if !job.cwd.is_empty() {
        put("txt", field("directory", &job.cwd));
    }
    put("txt", field("job", &job.short));
    for id in &job.session_ids {
        put("dim", field("session", id));
    }
    let process = match (job.running, job.pid) {
        (Some(true), Some(pid)) => format!("pid {pid}, running"),
        (Some(false), _) => "no running Claude process holds this job".into(),
        _ if !job.unchecked.is_empty() => format!("could not tell whether a process holds it: {}", job.unchecked),
        _ => "could not tell whether a process holds it".into(),
    };
    put("txt", field("process", &process));
    if let Some(f) = &job.in_flight {
        let kinds = if f.kinds.is_empty() { String::new() } else { format!(" ({})", f.kinds.join(", ")) };
        put("txt", field("in flight", &format!("{}{kinds}", job.in_flight_words())));
    }
    if !job.version.is_empty() {
        put("dim", field("claude", &job.version));
    }
    put("", String::new());
    put("lbl", format!(" ── WHAT IT STARTED · {} ──", job.fan.len()));
    if job.fan.is_empty() {
        put("dim", "   nothing listed".into());
    }
    for f in &job.fan {
        let label = if f.label.is_empty() { &f.id } else { &f.label };
        let age = f.started.map(|s| format!("started {} ago", ago(at - s))).unwrap_or_default();
        let (kind, finish) = match f.done {
            Some(d) => ("dim", format!("done {} ago", ago(at - d))),
            // Shells never get a finish written; that is not proof one runs.
            None if f.kind == "shell" => ("dim", "no finish recorded".to_string()),
            None => ("working", "no finish recorded".to_string()),
        };
        let what = if f.kind.is_empty() { "task" } else { f.kind.as_str() };
        put(kind, format!("   {} {label}  {age} · {finish}", tc::pad(what, 6)));
    }
    put("", String::new());
    put(
        "dim",
        format!(
            " Read from {}/state.json, which Claude Code writes for its `claude agents` dashboard. \
             An undocumented format: a change in Claude can make this screen wrong.",
            job.dir
        ),
    );
    out
}

/// [`explain_lines`] drawn `w` columns wide, each line in the colour
/// `colour` gives its kind. A line that fits is kept as it is, since its
/// field names are aligned by runs of spaces; a longer one wraps under its
/// own indent rather than being cut.
pub fn explain_rows(lines: &[(String, String)], w: usize, colour: &dyn Fn(&str) -> String) -> Vec<String> {
    let mut out = Vec::new();
    for (kind, text) in lines {
        let colour = colour(kind);
        let wrapped = if tc::display_width(text) < w.saturating_sub(1) {
            vec![text.clone()]
        } else {
            tc::wrap_words(text, w.saturating_sub(6).max(1))
                .into_iter()
                .enumerate()
                .map(|(i, l)| format!("{}{l}", if i == 0 { "   " } else { "     " }))
                .collect()
        };
        for line in wrapped {
            out.push(tc::seg(&[(colour.as_str(), line)], w.saturating_sub(1)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tc::agent_work::{parse_job_state, FanItem};

    fn ago(s: f64) -> String {
        format!("{}s", s as i64)
    }

    fn job(state: &str) -> Job {
        let mut job = parse_job_state(
            "abcd1234",
            &format!(r#"{{"state":"{state}","name":"web","detail":"Running tests","needs":"Approve?","sessionId":"s1","updatedAt":"2026-11-12T00:00:00Z","inFlight":{{"tasks":1,"queued":0,"kinds":["local_agent"]}}}}"#),
        )
        .unwrap();
        job.fan = vec![
            FanItem { id: "a1".into(), kind: "agent".into(), label: "Explore".into(), started: Some(100.0), done: None },
            FanItem { id: "a2".into(), kind: "agent".into(), label: "Review".into(), started: Some(100.0), done: Some(150.0) },
            FanItem { id: "b1".into(), kind: "shell".into(), label: "cargo test".into(), started: Some(100.0), done: None },
        ];
        job
    }

    #[test]
    fn a_blocked_job_says_what_it_needs_and_a_working_one_what_it_does() {
        assert_eq!(message(&job("blocked")), "Approve?");
        assert_eq!(message(&job("working")), "Running tests");
        assert_eq!(state_word(&job("blocked")), "blocked");
        assert_eq!(facts(&job("working")), ["1 in flight"]);
    }

    #[test]
    fn the_explain_screen_lists_every_fan_item_with_its_age() {
        let lines = explain_lines(&job("working"), 200.0, ago);
        let text: Vec<&str> = lines.iter().map(|(_, t)| t.as_str()).collect();
        assert!(text.iter().any(|t| t.contains("Explore  started 100s ago · no finish recorded")));
        assert!(text.iter().any(|t| t.contains("Review  started 100s ago · done 50s ago")));
        assert!(text.iter().any(|t| t.contains("cargo test")));
        assert!(text.iter().any(|t| t.contains("WHAT IT STARTED · 3")));
    }

    #[test]
    fn a_job_whose_process_is_gone_says_so_and_an_unread_one_says_why() {
        let mut gone = job("working");
        gone.running = Some(false);
        assert!(why(&gone).contains("not running"));
        let mut done = job("done");
        done.running = Some(false);
        assert_eq!(why(&done), "");
        // Done, but nobody could check its process: kept, and says why.
        done.running = None;
        done.unchecked = "no process table was read".into();
        assert!(why(&done).contains("unknown: no process table was read"));
        let unread = Job { short: "x".into(), dir: "/c/jobs/x".into(), unread: Some("state.json is not JSON".into()), ..Default::default() };
        assert_eq!(state_word(&unread), "unknown");
        assert!(why(&unread).contains("not JSON") && why(&unread).contains("/c/jobs/x"));
    }

    #[test]
    fn a_job_a_multiplexer_already_reports_is_taken_out_once() {
        let mut jobs = Jobs { shown: vec![job("working"), Job { short: "other".into(), ..job("blocked") }], finished: 0 };
        jobs.shown[1].session_ids = vec!["s2".into()];
        let taken = take_reported(&mut jobs, &["s1".to_string()]);
        assert_eq!(taken.len(), 1);
        assert_eq!(jobs.shown.len(), 1);
        assert_eq!(jobs.shown[0].short, "other");
    }

    #[test]
    fn no_explain_or_note_row_grows_past_its_pane_at_any_width() {
        let mut long = job("blocked");
        long.running = Some(false);
        long.detail = "a detail with a-very-long-unbroken-path/that/keeps/on/going/past/any/pane".into();
        let lines = explain_lines(&long, 200.0, ago);
        for w in 8..=120 {
            for row in explain_rows(&lines, w, &|_| String::new()) {
                assert!(tc::display_width(&row) <= w, "{w}: {row:?}");
            }
            for (_, row) in note_rows(&long, w) {
                assert!(tc::display_width(&row) <= w.max(5), "{w}: {row:?}");
            }
        }
    }

    #[test]
    fn finished_jobs_are_counted_in_the_heading() {
        assert_eq!(group_label(&Jobs::default()), "background agents · via jobs");
        let jobs = Jobs { finished: 3, ..Default::default() };
        assert_eq!(group_label(&jobs), "background agents · via jobs · 3 finished not shown");
    }

    #[test]
    fn the_work_under_a_job_is_its_unfinished_subagents() {
        let rows = work(&job("working"), &Err("no ps".into())).rows(80);
        let text: Vec<&str> = rows.iter().map(|(_, r)| r.as_str()).collect();
        assert_eq!(text, ["   ├ Explore working · via jobs"]);
    }
}
