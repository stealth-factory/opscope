// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! macOS acquisition for `link`; parsing remains platform-independent.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::parse::{is_nettop_sample_header, parse_nettop_snapshot};
use super::Session;

#[derive(Default)]
struct NettopState {
    latest: String,
    published: Option<Instant>,
    error: String,
}

struct NettopFeed {
    state: Mutex<NettopState>,
    changed: Condvar,
}

static NETTOP: OnceLock<Arc<NettopFeed>> = OnceLock::new();

/// Keep one `nettop` logger alive. A one-shot `nettop -L 1` takes roughly six
/// seconds to exit, longer than this widget's five-second command bound, and
/// a pipe makes the continuous logger buffer several samples together.
/// `script` gives it the terminal it expects, so each CSV sample is flushed
/// at the requested one-second cadence.
fn nettop_feed() -> &'static Arc<NettopFeed> {
    NETTOP.get_or_init(|| {
        let feed = Arc::new(NettopFeed {
            state: Mutex::new(NettopState::default()),
            changed: Condvar::new(),
        });
        let worker = Arc::clone(&feed);
        std::thread::spawn(move || run_nettop(worker));
        feed
    })
}

fn publish_nettop(feed: &NettopFeed, lines: &mut Vec<String>) {
    if lines.is_empty() {
        return;
    }
    let text = lines.join("\n");
    lines.clear();
    if let Ok(mut state) = feed.state.lock() {
        state.latest = text;
        state.published = Some(Instant::now());
        state.error.clear();
        feed.changed.notify_all();
    }
}

fn fail_nettop(feed: &NettopFeed, reason: String) {
    if let Ok(mut state) = feed.state.lock() {
        state.error = reason;
        feed.changed.notify_all();
    }
}

fn drain_stderr(stderr: Option<std::process::ChildStderr>) -> Arc<Mutex<String>> {
    let last = Arc::new(Mutex::new(String::new()));
    let Some(stderr) = stderr else {
        return last;
    };
    let slot = Arc::clone(&last);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            if let Ok(mut guard) = slot.lock() {
                *guard = line;
            }
        }
    });
    last
}

fn with_stderr(reason: String, stderr: &Mutex<String>) -> String {
    match stderr.lock() {
        Ok(line) if !line.is_empty() => format!("{}: {}", reason, line),
        _ => reason,
    }
}

/// Keep reading samples for as long as the widget lives. A child that exits
/// is a source failure, not the end of the session: the next poll would
/// otherwise keep returning that one error and freeze the session list.
fn run_nettop(feed: Arc<NettopFeed>) {
    loop {
        run_nettop_once(&feed);
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn run_nettop_once(feed: &NettopFeed) {
    let mut child = match Command::new("/usr/bin/script")
        .args([
            "-q",
            "/dev/null",
            "/usr/bin/nettop",
            "-m",
            "tcp",
            "-n",
            "-x",
            "-L",
            "0",
            "-s",
            "1",
            "-J",
            "bytes_in,bytes_out,re-tx,rtt_min,rtt_avg,rtt_var",
        ])
        // The logger lives for the whole widget session. If it inherits the
        // pane's stdin it races the UI for key and mouse reports, making
        // selection and scrolling appear to work only at random.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            fail_nettop(feed, format!("nettop TCP metrics unavailable: {}", err));
            return;
        }
    };
    let stderr = drain_stderr(child.stderr.take());
    let Some(stdout) = child.stdout.take() else {
        fail_nettop(
            feed,
            with_stderr("nettop stdout unavailable".into(), &stderr),
        );
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    let mut sample = Vec::new();
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(line) => line.trim_end_matches('\r').to_string(),
            Err(err) => {
                fail_nettop(
                    feed,
                    with_stderr(format!("nettop output unreadable: {}", err), &stderr),
                );
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        };
        if is_nettop_sample_header(&line) {
            publish_nettop(feed, &mut sample);
            sample.push(line);
        } else if !sample.is_empty() {
            sample.push(line);
        }
    }
    publish_nettop(feed, &mut sample);
    let reason = match child.wait() {
        Ok(status) => format!("nettop stopped with {}", status),
        Err(err) => format!("nettop stopped: {}", err),
    };
    fail_nettop(feed, with_stderr(reason, &stderr));
}

fn sample_is_fresh(state: &NettopState) -> bool {
    state
        .published
        .is_some_and(|at| at.elapsed() < Duration::from_secs(10))
        && !state.latest.is_empty()
}

fn latest_snapshot() -> Result<String, String> {
    let feed = nettop_feed();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut state = feed
        .state
        .lock()
        .map_err(|_| "nettop feed lock failed".to_string())?;
    loop {
        if !state.error.is_empty() {
            return Err(state.error.clone());
        }
        if sample_is_fresh(&state) {
            return Ok(state.latest.clone());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err("nettop did not publish a TCP sample within 5s".into());
        }
        let (next, timeout) = feed
            .changed
            .wait_timeout(state, deadline - now)
            .map_err(|_| "nettop feed lock failed".to_string())?;
        state = next;
        if timeout.timed_out() && !sample_is_fresh(&state) && state.error.is_empty() {
            return Err("nettop did not publish a TCP sample within 5s".into());
        }
    }
}

pub fn sessions(named: &[u16]) -> Result<Vec<Session>, String> {
    let text = latest_snapshot()?;
    Ok(parse_nettop_snapshot(&text, named)
        .sessions
        .into_iter()
        .map(|row| Session {
            peer: row.peer,
            ip: row.ip,
            port: row.port,
            rtt: row.rtt,
            jitter: row.jitter,
            floor: row.floor,
            sent: row.sent,
            recv: row.recv,
            delivery: None,
            delivery_unavailable: true,
            loss_unavailable: true,
            raw: row.raw,
            ..Session::default()
        })
        .collect())
}

pub fn source_note() -> &'static str {
    "measured by the kernel via nettop, nothing sent"
}

pub fn empty_note() -> &'static str {
    "Nothing is connected to this machine, or nettop cannot see it."
}
