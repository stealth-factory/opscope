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

use opscope_core as tc;

use super::parse::{
    parse_lsof_cwd, parse_lsof_listen, parse_ps_etimes, parse_ps_state_zombie, parse_ps_uid,
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

pub fn process_info(pid: i32) -> (String, String, Option<f64>) {
    let pid_s = pid.to_string();
    let cmdline = tc::run_quiet(&["ps", "-www", "-o", "args=", "-p", &pid_s], RUN_TIMEOUT)
        .trim()
        .to_string();
    let cwd = parse_lsof_cwd(&tc::run_quiet(
        &["lsof", "-a", "-p", &pid_s, "-d", "cwd", "-Fn"],
        RUN_TIMEOUT,
    ));
    // Apple's ps has `etime` (`[[dd-]hh:]mm:ss`), not the procps `etimes`
    // seconds column. Asking for the Linux name produces no parseable
    // stdout and every row would show `--` for uptime.
    let started = parse_ps_etimes(&tc::run_quiet(
        &["ps", "-o", "etime=", "-p", &pid_s],
        RUN_TIMEOUT,
    ))
    .map(|elapsed| tc::now() - elapsed);
    (cmdline, cwd, started)
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
