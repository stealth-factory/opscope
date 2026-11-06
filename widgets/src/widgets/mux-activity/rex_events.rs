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

//! The live half: `rex events --all --json`, which wakes a read.
//!
//! Events wake reads; they never patch rows. Both sibling widgets settled on
//! that, and for the same reason: an event is a hint that something moved,
//! and the read is what says where it moved to. The one payload kept is a
//! bell or a desktop notification, because no method returns it afterwards.

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::rex::State;
use super::rex_parse::{self as parse, StreamEvent};
use super::refresh_wake;

/// Seconds to wait before each reconnect, then the last one for good.
const BACKOFF: &[u64] = &[1, 2, 5, 10];

fn sleep_or_stop(stop: &AtomicBool, seconds: u64) {
    let until = std::time::Instant::now() + Duration::from_secs(seconds);
    while std::time::Instant::now() < until {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn set_err(state: &Arc<Mutex<State>>, why: String) {
    if let Ok(mut guard) = state.lock() {
        guard.watcher_err = why;
    }
}

/// Follow the server's events until told to stop, reconnecting as needed.
///
/// `child` holds the running `rex events` pid so quitting can end it; a
/// widget that leaves it behind leaves a client attached to the server.
pub fn watch(
    state: Arc<Mutex<State>>,
    wake: refresh_wake::Wake,
    server: String,
    stop: Arc<AtomicBool>,
    child: Arc<AtomicI32>,
) {
    let mut attempt = 0usize;
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let mut command = Command::new("rex");
        command.arg("--autostart=false");
        if !server.is_empty() {
            command.args(["-S", &server]);
        }
        command
            .args(["events", "--all", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let why = match command.spawn() {
            Err(e) => format!("could not start rex events: {}", e),
            Ok(mut proc) => {
                child.store(proc.id() as i32, Ordering::Relaxed);
                let mut last = String::new();
                let mut heard = false;
                if let Some(out) = proc.stdout.take() {
                    for line in BufReader::new(out).lines() {
                        let Ok(line) = line else { break };
                        if !heard {
                            // A stream that answered is a stream that works:
                            // clear the reason and catch up on whatever
                            // happened while it was away.
                            heard = true;
                            attempt = 0;
                            set_err(&state, String::new());
                            refresh_wake::request(&wake);
                        }
                        match parse::parse_event_line(&line) {
                            StreamEvent::Wake => refresh_wake::request(&wake),
                            StreamEvent::Attention { block_id, said } => {
                                if let Ok(mut guard) = state.lock() {
                                    guard
                                        .attention
                                        .insert(block_id, (opscope_core::now(), said));
                                }
                                refresh_wake::request(&wake);
                            }
                            StreamEvent::Ended(said) => last = said,
                            StreamEvent::Ignore => {}
                        }
                    }
                }
                let mut stderr = String::new();
                if let Some(mut err) = proc.stderr.take() {
                    let _ = err.read_to_string(&mut stderr);
                }
                let status = proc.wait();
                child.store(0, Ordering::Relaxed);
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let said = if !last.is_empty() {
                    last
                } else {
                    match parse::parse_failure(&stderr) {
                        parse::Absence::NoServer => "no Rex server is running".into(),
                        parse::Absence::Unreachable => "the server did not answer".into(),
                        parse::Absence::Gone => "the server went away".into(),
                        parse::Absence::Other(text) if !text.is_empty() => text,
                        parse::Absence::Other(_) => match status {
                            Ok(s) => format!("rex events exited ({})", s),
                            Err(e) => format!("rex events: {}", e),
                        },
                    }
                };
                format!("stream ended: {}", said)
            }
        };
        let wait = BACKOFF[attempt.min(BACKOFF.len() - 1)];
        attempt += 1;
        set_err(
            &state,
            format!(
                "{} · reconnecting in {}s · rows are still read on the refresh timer",
                why, wait
            ),
        );
        // Whatever ended the stream may also have changed the board.
        refresh_wake::request(&wake);
        sleep_or_stop(&stop, wait);
    }
}
