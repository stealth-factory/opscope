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

//! Where the bytes come from on macOS: `lsof` and `ps`.
//!
//! Parsers live in `parse.rs` and are compiled everywhere. This file is
//! gated by `cfg(target_os = "macos")` because spawning `lsof` is the
//! thing Linux does not need. `lsof` itself is a tool, so its absence is
//! a runtime `missing()` check in `main`, not a compile-time one.

use std::collections::HashMap;

use opscope_core as tc;

use super::parse::{
    parse_lsof_cwds, parse_lsof_listen, parse_nettop_connections, parse_ps_processes,
    parse_ps_state_zombie, parse_ps_uid, Counters,
};
use super::{Found, RUN_TIMEOUT};

/// Every listening TCP socket, with the pid `lsof` named beside it.
///
/// A failed or timed-out `lsof` is an error, not an empty table: empty is
/// what "nothing is listening" looks like.
pub fn sockets() -> Result<Vec<Found>, String> {
    let text = tc::run(
        &["lsof", "-nP", "-iTCP", "-sTCP:LISTEN", "-Fpcunt"],
        RUN_TIMEOUT,
    )?;
    Ok(parse_lsof_listen(&text)
        .into_iter()
        .map(|sock| Found {
            port: sock.port,
            bind: sock.bind,
            uid: sock.uid,
            pid: sock.pid,
        })
        .collect())
}

pub fn process_infos(pids: &[i32]) -> Result<HashMap<i32, (String, String, Option<f64>)>, String> {
    if pids.is_empty() {
        return Ok(HashMap::new());
    }
    let listed = pids
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    // One ps and one lsof per scan, rather than three subprocesses for every
    // listener. Besides being faster, the batch gives every uptime the same
    // timestamp and keeps a large machine from looking empty during startup.
    let processes = parse_ps_processes(&tc::run(
        &["ps", "-www", "-o", "pid=,etime=,args=", "-p", &listed],
        RUN_TIMEOUT,
    )?);
    let cwds = parse_lsof_cwds(&tc::run(
        &["lsof", "-a", "-p", &listed, "-d", "cwd", "-Fpn"],
        RUN_TIMEOUT,
    )?);
    let now = tc::now();
    Ok(processes
        .into_iter()
        .map(|process| {
            let cwd = cwds.get(&process.pid).cloned().unwrap_or_default();
            let started = process.elapsed.map(|elapsed| now - elapsed);
            (process.pid, (process.command, cwd, started))
        })
        .collect())
}

pub fn traffic_available() -> bool {
    std::path::Path::new("/usr/bin/nettop").is_file()
}

pub fn traffic_unavailable() -> &'static str {
    "no traffic · needs nettop"
}

pub fn traffic_counters() -> Result<HashMap<String, Counters>, String> {
    let text = tc::run(
        &[
            "/usr/bin/nettop",
            "-m",
            "tcp",
            "-n",
            "-x",
            "-L",
            "1",
            "-J",
            "bytes_in,bytes_out",
        ],
        RUN_TIMEOUT,
    )?;
    Ok(parse_nettop_connections(&text)
        .into_iter()
        .map(|connection| {
            (
                connection.id,
                Counters {
                    port: connection.port,
                    sent: connection.sent,
                    recv: connection.recv,
                },
            )
        })
        .collect())
}

/// Whether this pid is ours to signal.
///
/// `Ok(false)` is somebody else's. `Err` is already gone.
pub fn ours(pid: i32) -> Result<bool, ()> {
    let uid = parse_ps_uid(&tc::run_quiet(
        &["ps", "-o", "uid=", "-p", &pid.to_string()],
        RUN_TIMEOUT,
    ))
    .ok_or(())?;
    Ok(uid == unsafe { libc::getuid() })
}

/// A zombie answers `kill(0)` and is not running.
pub fn is_zombie(pid: i32) -> bool {
    parse_ps_state_zombie(&tc::run_quiet(
        &["ps", "-o", "state=", "-p", &pid.to_string()],
        RUN_TIMEOUT,
    ))
}
