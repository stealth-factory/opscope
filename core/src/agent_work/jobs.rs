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

//! Claude Code background agents (`claude --bg`) and Claude's own pid files
//! (OPS-276).
//!
//! A background agent runs on Claude's daemon, on a terminal no multiplexer
//! pane holds, so no pane status and no process tree under a pane can see
//! it. Claude writes what its `claude agents` dashboard reads instead:
//! `<config dir>/jobs/<short id>/state.json`, a `timeline.jsonl` of state
//! changes beside it, and `<config dir>/sessions/<pid>.json` for every
//! running Claude process, background or not. These are undocumented
//! internal formats, pinned by the fixtures below; see
//! `docs/agent-background-work.md` for the version they were observed on.
//!
//! Parsers are pure and ungated. Acquisition is plain file reads, so a Mac
//! reads the same files; only the pid-reuse check reads `/proc`, and goes
//! without it where there is none.

use super::Table;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The state a background agent reported, verbatim. Not inferred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Blocked,
    Working,
    Done,
    Stopped,
}

impl JobState {
    pub fn word(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Working => "working",
            Self::Done => "done",
            Self::Stopped => "stopped",
        }
    }
    fn rank(self) -> usize {
        match self {
            Self::Blocked => 1,
            Self::Working => 2,
            Self::Done => 3,
            Self::Stopped => 4,
        }
    }
}

/// One subagent or shell a background agent started: an entry in `fan`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FanItem {
    pub id: String,
    /// `agent` or `shell`, as Claude names it.
    pub kind: String,
    pub label: String,
    /// Epoch seconds.
    pub started: Option<f64>,
    /// Epoch seconds; absent while it runs.
    pub done: Option<f64>,
}

/// `inFlight`: what the agent itself counts as still running.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InFlight {
    pub tasks: u64,
    pub queued: u64,
    pub kinds: Vec<String>,
}

/// One background agent, or why its `state.json` could not be read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Job {
    /// The directory name under `jobs/`, which `claude agents` shows.
    pub short: String,
    /// The folder this was read from, for a reader asking where.
    pub dir: String,
    /// `None` exactly when `unread` says why.
    pub state: Option<JobState>,
    pub unread: Option<String>,
    pub name: String,
    pub detail: String,
    /// What it is waiting for, when blocked.
    pub needs: String,
    pub cwd: String,
    /// Every session id this job has gone by: its own, the one it resumed
    /// and the one its running process holds. Any of them can be the id a
    /// hook reported to a multiplexer.
    pub session_ids: Vec<String>,
    pub version: String,
    pub in_flight: Option<InFlight>,
    pub fan: Vec<FanItem>,
    /// Epoch seconds of the last write to `state.json`.
    pub updated: Option<f64>,
    /// Epoch seconds since which `state` has held, and whether that is the
    /// moment it began (`true`) or only a lower bound on how long.
    pub held_since: Option<(f64, bool)>,
    /// Whether a live Claude process holds this job. `None` when that
    /// could not be told: no process table, or a pid file not readable.
    pub running: Option<bool>,
    pub pid: Option<u32>,
}

impl Job {
    /// Whether this job is the one a multiplexer reported under `session`.
    pub fn is_session(&self, session: &str) -> bool {
        !session.is_empty() && self.session_ids.iter().any(|s| s == session)
    }

    /// `inFlight` as a person would read it: `2 tasks, 1 queued`, or
    /// nothing when the agent reported no count.
    pub fn in_flight_words(&self) -> String {
        match &self.in_flight {
            Some(f) if f.queued > 0 => format!("{} in flight, {} queued", f.tasks, f.queued),
            Some(f) => format!("{} in flight", f.tasks),
            None => String::new(),
        }
    }

    fn rank(&self) -> usize {
        self.state.map_or(0, JobState::rank)
    }
}

/// Every background agent worth a row, and what was left out and why.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Jobs {
    /// Unreadable first, then blocked, working, done; longest held first.
    /// A `jobs/` folder that exists and could not be listed is a row too,
    /// with `unread` saying why, so it is never an empty section.
    pub shown: Vec<Job>,
    /// Finished jobs no process holds any more: `done` with nothing
    /// running, and `stopped`. Counted, never silently dropped.
    pub finished: usize,
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}

/// An RFC 3339 timestamp as epoch seconds.
pub fn parse_iso_seconds(s: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.timestamp_millis() as f64 / 1000.0)
}

fn millis(v: &Value) -> Option<f64> {
    v.as_f64().map(|ms| ms / 1000.0)
}

/// `jobs/<short>/state.json`. An unrecognised `state` is an error, not a
/// guess: the dashboard's words are the only ones drawn.
pub fn parse_job_state(short: &str, text_in: &str) -> Result<Job, String> {
    let v: Value =
        serde_json::from_str(text_in).map_err(|e| format!("state.json is not JSON: {e}"))?;
    if !v.is_object() {
        return Err("state.json is not an object".into());
    }
    let state = match v["state"].as_str() {
        Some("working") => JobState::Working,
        Some("blocked") => JobState::Blocked,
        Some("done") => JobState::Done,
        Some("stopped") => JobState::Stopped,
        Some(other) => return Err(format!("state.json has an unrecognised state `{other}`")),
        None => return Err("state.json has no state".into()),
    };
    let mut session_ids = Vec::new();
    for key in ["sessionId", "resumeSessionId"] {
        let id = text(&v, key);
        if !id.is_empty() && !session_ids.contains(&id) {
            session_ids.push(id);
        }
    }
    let in_flight = v["inFlight"].is_object().then(|| InFlight {
        tasks: v["inFlight"]["tasks"].as_u64().unwrap_or(0),
        queued: v["inFlight"]["queued"].as_u64().unwrap_or(0),
        kinds: v["inFlight"]["kinds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|k| k.as_str().map(str::to_owned))
            .collect(),
    });
    let fan = v["fan"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| FanItem {
            id: text(f, "id"),
            kind: text(f, "kind"),
            label: text(f, "label"),
            started: millis(&f["startedAt"]),
            done: millis(&f["doneAt"]),
        })
        .collect();
    let updated = parse_iso_seconds(&text(&v, "updatedAt"));
    Ok(Job {
        short: short.to_string(),
        state: Some(state),
        name: text(&v, "name"),
        detail: text(&v, "detail"),
        needs: text(&v, "needs"),
        cwd: text(&v, "cwd"),
        session_ids,
        version: text(&v, "cliVersion"),
        in_flight,
        fan,
        updated,
        // The last write is after the state began, so it is a lower bound
        // until the timeline says when it began.
        held_since: updated.map(|at| (at, false)),
        ..Default::default()
    })
}

/// When `state` began, from the tail of `timeline.jsonl`: the first record
/// of the run of `state` that the file ends with. `whole` says the text is
/// the entire file, so a run that starts on its first line began then;
/// otherwise the first line is a fragment and a run reaching back to it is
/// only a lower bound. A timeline that ends on a different state than
/// `state.json` holds says nothing, and gives `None`.
pub fn parse_job_timeline(text_in: &str, state: &str, whole: bool) -> Option<(f64, bool)> {
    let mut lines = text_in.lines();
    if !whole {
        lines.next();
    }
    let mut run: Option<f64> = None;
    let mut broke = false;
    let mut last = String::new();
    for line in lines {
        // A record the writer is still finishing is not one yet.
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let (Some(s), Some(at)) = (v["state"].as_str(), v["at"].as_str().and_then(parse_iso_seconds))
        else {
            continue;
        };
        if s == state {
            if last != state {
                broke = !last.is_empty();
                run = Some(at);
            }
        } else {
            run = None;
        }
        last = s.to_string();
    }
    if last != state {
        return None;
    }
    run.map(|at| (at, broke || whole))
}

/// `sessions/<pid>.json`, the file every running Claude process keeps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClaudeSession {
    pub pid: u32,
    pub session_id: String,
    pub cwd: String,
    /// `busy`, `idle` or `shell`, as Claude reports it.
    pub status: String,
    /// `interactive` or `bg`.
    pub kind: String,
    /// The job's short id, for a background agent.
    pub job_id: String,
    /// The process start time Claude recorded, to tell a reused pid.
    pub proc_start: String,
}

pub fn parse_claude_session(text_in: &str) -> Result<ClaudeSession, String> {
    let v: Value =
        serde_json::from_str(text_in).map_err(|e| format!("Claude pid file is not JSON: {e}"))?;
    let pid = v["pid"]
        .as_u64()
        .and_then(|p| u32::try_from(p).ok())
        .ok_or("Claude pid file has no pid")?;
    let session_id = text(&v, "sessionId");
    if session_id.is_empty() {
        return Err("Claude pid file has no sessionId".into());
    }
    Ok(ClaudeSession {
        pid,
        session_id,
        cwd: text(&v, "cwd"),
        status: text(&v, "status"),
        kind: text(&v, "kind"),
        job_id: text(&v, "jobId"),
        proc_start: text(&v, "procStart"),
    })
}

/// Whether `pid` is still the process that wrote the file. Where `/proc`
/// gives a start time it must match; elsewhere the process table, which
/// already named the pid a running Claude, is all there is.
pub(super) fn same_process(session: &ClaudeSession) -> bool {
    if session.proc_start.is_empty() {
        return true;
    }
    match i32::try_from(session.pid).ok().map(crate::procfs::read_stat) {
        Some(Ok(stat)) => stat.starttime.to_string() == session.proc_start,
        _ => true,
    }
}

/// Every pid file in `<root>/sessions`, with the ones that did not parse.
pub(super) fn read_sessions(root: &Path) -> Result<(Vec<ClaudeSession>, usize), String> {
    let dir = root.join("sessions");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut out = Vec::new();
    let mut bad = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| parse_claude_session(&t))
        {
            Ok(s) => out.push(s),
            Err(_) => bad += 1,
        }
    }
    Ok((out, bad))
}

/// The last `n` bytes of a file and whether that was all of it.
fn tail(path: &Path, n: u64) -> std::io::Result<(String, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(n);
    f.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    Ok((String::from_utf8_lossy(&bytes).to_string(), start == 0))
}

const TIMELINE_TAIL: u64 = 64 * 1024;

/// Every background agent under these config dirs. `table` is the process
/// table the frame already read, used to tell a running job from one whose
/// process has gone.
pub fn read_jobs(roots: &[PathBuf], table: Option<&Result<Table, String>>) -> Jobs {
    let mut all = Vec::new();
    let mut out = Jobs::default();
    for root in roots {
        let dir = root.join("jobs");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // No folder: no background agent was ever started from here.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                all.push(Job {
                    short: "jobs".into(),
                    dir: dir.display().to_string(),
                    unread: Some(format!("the jobs folder could not be listed: {e}")),
                    ..Default::default()
                });
                continue;
            }
        };
        // Unreadable pid files leave whether a job runs unknown, not false.
        let sessions = read_sessions(root);
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let short = entry.file_name().to_string_lossy().to_string();
            let state = path.join("state.json");
            // A job folder with no state yet holds only Claude's scratch
            // space; it has never run, so it is not a job to report.
            if !state.exists() {
                continue;
            }
            // The last write, as a lower bound on how long whatever the
            // file says - or that it cannot be read - has been so.
            let written = std::fs::metadata(&state)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| (d.as_secs_f64(), false));
            let mut job = match std::fs::read_to_string(&state)
                .map_err(|e| format!("state.json could not be read: {e}"))
                .and_then(|t| parse_job_state(&short, &t))
            {
                Ok(job) => Job {
                    dir: path.display().to_string(),
                    held_since: job.held_since.or(written),
                    ..job
                },
                Err(why) => {
                    let dir = path.display().to_string();
                    all.push(Job { short, dir, unread: Some(why), held_since: written, ..Default::default() });
                    continue;
                }
            };
            if let Some(word) = job.state.map(JobState::word) {
                if let Ok((text_in, whole)) = tail(&path.join("timeline.jsonl"), TIMELINE_TAIL) {
                    if let Some(held) = parse_job_timeline(&text_in, word, whole) {
                        job.held_since = Some(held);
                    }
                }
            }
            bind_process(&mut job, &sessions, table);
            all.push(job);
        }
    }
    for job in all {
        let keep = match job.state {
            None | Some(JobState::Blocked | JobState::Working) => true,
            Some(JobState::Done) => job.running == Some(true),
            Some(JobState::Stopped) => false,
        };
        if keep {
            out.shown.push(job);
        } else {
            out.finished += 1;
        }
    }
    out.shown.sort_by(|a, b| {
        a.rank().cmp(&b.rank()).then_with(|| {
            let at = |j: &Job| j.held_since.map_or(f64::MAX, |h| h.0);
            at(a).total_cmp(&at(b))
        })
    });
    out
}

fn bind_process(
    job: &mut Job,
    sessions: &Result<(Vec<ClaudeSession>, usize), String>,
    table: Option<&Result<Table, String>>,
) {
    let Ok((sessions, bad)) = sessions else {
        return;
    };
    let Some(Ok(table)) = table else {
        return;
    };
    let live = sessions.iter().find(|s| {
        s.job_id == job.short && table.get(s.pid).is_some() && same_process(s)
    });
    match live {
        Some(s) => {
            job.running = Some(true);
            job.pid = Some(s.pid);
            if !job.session_ids.contains(&s.session_id) {
                job.session_ids.push(s.session_id.clone());
            }
        }
        // A pid file that did not parse may be this job's.
        None if *bad > 0 => job.running = None,
        None => job.running = Some(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKING: &str = include_str!("fixtures/jobs/working.json");
    const BLOCKED: &str = include_str!("fixtures/jobs/blocked.json");
    const DONE: &str = include_str!("fixtures/jobs/done.json");
    const STOPPED: &str = include_str!("fixtures/jobs/stopped.json");
    const TIMELINE: &str = include_str!("fixtures/jobs/timeline.jsonl");
    const SESSION: &str = include_str!("fixtures/jobs/session.json");

    #[test]
    fn a_working_job_keeps_its_fan_and_every_session_id() {
        let job = parse_job_state("aaaa1111", WORKING).unwrap();
        assert_eq!(job.state, Some(JobState::Working));
        assert_eq!(job.name, "opscope");
        assert_eq!(job.detail, "Running cargo test");
        assert_eq!(job.cwd, "/srv/repo");
        assert_eq!(job.version, "2.1.322");
        assert!(job.is_session("aaaa1111-0000-4000-8000-000000000001"));
        assert!(job.is_session("bbbb2222-0000-4000-8000-000000000002"));
        assert!(!job.is_session(""));
        assert_eq!(job.in_flight_words(), "2 in flight");
        assert_eq!(job.fan.len(), 3);
        assert_eq!(job.fan[0].kind, "agent");
        assert_eq!(job.fan[0].label, "Explore the parser");
        assert_eq!(job.fan[0].started, Some(1_794_000_000.0));
        assert_eq!(job.fan[0].done, Some(1_794_000_100.0));
        assert_eq!(job.fan[1].done, None);
        assert_eq!(job.held_since.map(|h| h.1), Some(false));
    }

    #[test]
    fn blocked_done_and_stopped_parse_as_reported() {
        let blocked = parse_job_state("b", BLOCKED).unwrap();
        assert_eq!(blocked.state, Some(JobState::Blocked));
        assert_eq!(blocked.needs, "Should I do that next?");
        assert_eq!(blocked.in_flight_words(), "2 in flight, 1 queued");
        assert_eq!(parse_job_state("d", DONE).unwrap().state, Some(JobState::Done));
        let stopped = parse_job_state("s", STOPPED).unwrap();
        assert_eq!(stopped.state, Some(JobState::Stopped));
        // No inFlight is no count, not a count of zero.
        assert_eq!(stopped.in_flight, None);
        assert_eq!(stopped.in_flight_words(), "");
        assert!(stopped.fan.is_empty());
    }

    #[test]
    fn a_corrupt_or_unrecognised_state_is_an_error_not_a_guess() {
        assert!(parse_job_state("x", "{\"state\": \"wor").unwrap_err().contains("not JSON"));
        assert!(parse_job_state("x", "[]").unwrap_err().contains("not an object"));
        assert!(parse_job_state("x", "{}").unwrap_err().contains("no state"));
        let odd = parse_job_state("x", r#"{"state":"napping"}"#).unwrap_err();
        assert!(odd.contains("`napping`"), "{odd}");
    }

    #[test]
    fn the_timeline_dates_the_run_the_file_ends_with() {
        let at = parse_iso_seconds("2026-11-12T02:05:55.061Z").unwrap();
        assert_eq!(parse_job_timeline(TIMELINE, "working", true), Some((at, true)));
        // Ending on another state than state.json holds says nothing.
        assert_eq!(parse_job_timeline(TIMELINE, "blocked", true), None);
    }

    #[test]
    fn a_run_reaching_back_to_a_cut_first_line_is_a_lower_bound() {
        let lines: Vec<&str> = TIMELINE.lines().collect();
        let first = parse_iso_seconds("2026-11-12T02:05:55.061Z").unwrap();
        // Cut so the fragment is the `done` record: the run still has a
        // different state before it inside the text.
        let cut = format!("garbage}}\n{}\n", lines[lines.len() - 3..].join("\n"));
        assert_eq!(parse_job_timeline(&cut, "working", false), Some((first, true)));
        // Cut so only working records follow the fragment.
        let cut = format!("garbage}}\n{}\n", lines[lines.len() - 2..].join("\n"));
        assert_eq!(parse_job_timeline(&cut, "working", false), Some((first, false)));
        // A record still being written is skipped, not an error.
        let partial = format!("{TIMELINE}{{\"at\":\"2026-11");
        assert_eq!(parse_job_timeline(&partial, "working", true), Some((first, true)));
    }

    #[test]
    fn a_pid_file_names_its_session_and_job() {
        let s = parse_claude_session(SESSION).unwrap();
        assert_eq!(s.pid, 4242);
        assert_eq!(s.session_id, "bbbb2222-0000-4000-8000-000000000002");
        assert_eq!(s.cwd, "/srv/repo/.claude/worktrees/feature");
        assert_eq!(s.status, "busy");
        assert_eq!(s.kind, "bg");
        assert_eq!(s.job_id, "aaaa1111");
        assert!(parse_claude_session(r#"{"pid":1}"#).unwrap_err().contains("sessionId"));
        assert!(parse_claude_session("{").is_err());
    }

    fn table(text_in: &str) -> Result<Table, String> {
        super::super::parse_process_table(text_in)
    }

    #[test]
    fn jobs_sort_by_who_needs_you_and_finished_ones_are_counted() {
        let dir = std::env::temp_dir().join(format!("opscope-jobs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let put = |rel: &str, body: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        put("jobs/aaaa1111/state.json", WORKING);
        put("jobs/aaaa1111/timeline.jsonl", TIMELINE);
        put("jobs/bbbb/state.json", BLOCKED);
        put("jobs/dddd/state.json", DONE);
        put("jobs/ssss/state.json", STOPPED);
        put("jobs/cccc/state.json", "{\"state\":");
        std::fs::create_dir_all(dir.join("jobs/eeee/tmp")).unwrap();
        put("jobs/pins.json", "[]");
        put("sessions/4242.json", SESSION);
        let ps = table("4242 1 claude claude bg-spare\n");
        let jobs = read_jobs(std::slice::from_ref(&dir), Some(&ps));
        let order: Vec<&str> = jobs.shown.iter().map(|j| j.short.as_str()).collect();
        assert_eq!(order, ["cccc", "bbbb", "aaaa1111"]);
        assert!(jobs.shown[0].unread.as_deref().unwrap().contains("not JSON"));
        let working = &jobs.shown[2];
        assert_eq!(working.running, Some(true));
        assert_eq!(working.pid, Some(4242));
        assert_eq!(working.held_since.map(|h| h.1), Some(true));
        // Blocked with no process holding it is still drawn, and says so.
        assert_eq!(jobs.shown[1].running, Some(false));
        // done (no process) and stopped; the folder with no state is no job.
        assert_eq!(jobs.finished, 2);
        // Without a process table nobody can say whether one runs.
        let blind = read_jobs(std::slice::from_ref(&dir), None);
        assert!(blind.shown.iter().all(|j| j.running.is_none()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_jobs_folder_is_no_jobs_rather_than_an_error() {
        let jobs = read_jobs(&[PathBuf::from("/nonexistent/opscope-claude")], None);
        assert_eq!(jobs, Jobs::default());
    }
}
