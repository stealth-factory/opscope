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

//! CPU and memory of the pids Rex reported, on macOS: one `ps` for all.

use std::collections::HashMap;

use opscope_core as tc;

use super::rex_parse as parse;

#[derive(Default)]
pub struct Usage;

impl Usage {
    /// pid → (cpu %, resident bytes). macOS's `pcpu` is already a recent
    /// rate, so there is no first reading to wait for.
    pub fn sample(&mut self, pids: &[i64], _at: f64) -> HashMap<i64, (Option<f64>, u64)> {
        if pids.is_empty() {
            return HashMap::new();
        }
        let list = pids
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",");
        // `run_full`, not `run`: ps exits 1 when any one of the pids has
        // gone, and still prints every one that has not.
        let Ok(out) = tc::run_full(&["ps", "-o", "pid=,pcpu=,rss=", "-p", &list], 5) else {
            return HashMap::new();
        };
        parse::parse_ps(&String::from_utf8_lossy(&out.stdout))
            .into_iter()
            .map(|(pid, (cpu, rss))| (pid, (Some(cpu), rss)))
            .collect()
    }
}

/// Process names from this process's parent upward, nearest first: one
/// `ps` a step, which is a handful at start-up and never again. Stops at
/// launchd, at a pid `ps` will not describe, and after 64 steps.
pub fn ancestors() -> Vec<String> {
    let mut out = Vec::new();
    let mut pid = unsafe { libc::getppid() } as i64;
    while pid > 1 && out.len() < 64 {
        let Ok(text) = tc::run(&["ps", "-o", "ppid=,comm=", "-p", &pid.to_string()], 5) else {
            break;
        };
        let Some((name, parent)) = super::detect::parse_ps_parent(&text) else {
            break;
        };
        out.push(name);
        pid = parent;
    }
    out
}
