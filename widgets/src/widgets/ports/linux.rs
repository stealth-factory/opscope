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

//! Where the bytes come from on Linux: `/proc`.
//!
//! Parsers live in `parse.rs` and are compiled everywhere. This file is
//! gated by `cfg(target_os = "linux")` because opening `/proc/net/tcp`
//! is the thing that genuinely cannot exist on the other side.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::time::UNIX_EPOCH;

use super::parse::{parse_proc_net_tcp, parse_proc_stat_zombie};
use super::Found;

/// Every listening TCP socket, with the pid this user can name.
pub fn sockets() -> Result<Vec<Found>, String> {
    let owners = socket_owners();
    let mut out = Vec::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for sock in parse_proc_net_tcp(&text) {
            out.push(Found {
                port: sock.port,
                bind: sock.bind,
                uid: sock.uid,
                pid: owners.get(&sock.inode).copied(),
            });
        }
    }
    Ok(out)
}

/// inode -> pid, for every process this user can read.
///
/// Root's sockets are not readable, so sshd and the like arrive unowned.
/// That is stated on screen rather than papered over.
fn socket_owners() -> HashMap<String, i32> {
    let mut owners = HashMap::new();
    let entries = match std::fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return owners,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let pid: i32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let fds = match std::fs::read_dir(format!("/proc/{}/fd", pid)) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                let target = target.to_string_lossy();
                if let Some(rest) = target.strip_prefix("socket:[") {
                    owners.insert(rest.trim_end_matches(']').to_string(), pid);
                }
            }
        }
    }
    owners
}

pub fn process_info(pid: i32) -> (String, String, Option<f64>) {
    let cmdline = std::fs::read(format!("/proc/{}/cmdline", pid))
        .map(|raw| {
            String::from_utf8_lossy(&raw)
                .replace('\0', " ")
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let cwd = std::fs::read_link(format!("/proc/{}/cwd", pid))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    // The link resolves even when the directory is gone; the kernel just
    // marks it, and that marker is worth keeping.
    let deleted = std::fs::metadata(format!("/proc/{}/cwd", pid)).is_err() && !cwd.is_empty();
    let cwd = if deleted && !cwd.ends_with("(deleted)") {
        format!("{} (deleted)", cwd)
    } else {
        cwd
    };
    let started = std::fs::metadata(format!("/proc/{}", pid))
        .ok()
        .and_then(|m| m.created().or_else(|_| m.modified()).ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64());
    (cmdline, cwd, started)
}

/// Whether this pid is ours to signal.
///
/// `Ok(false)` is somebody else's. `Err` is already gone.
pub fn ours(pid: i32) -> Result<bool, ()> {
    match std::fs::metadata(format!("/proc/{}", pid)) {
        Ok(meta) => Ok(meta.uid() == unsafe { libc::getuid() }),
        Err(_) => Err(()),
    }
}

/// A zombie answers `kill(0)` and is not running.
pub fn is_zombie(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{}/stat", pid))
        .ok()
        .is_some_and(|text| parse_proc_stat_zombie(&text))
}
