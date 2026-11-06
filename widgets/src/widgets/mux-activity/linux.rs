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

//! CPU and memory of the pids Rex reported, on Linux: `/proc/<pid>/stat`.

use std::collections::HashMap;

use super::rex_parse as parse;

/// Ticks and the time they were read, per pid, so a percentage is two
/// readings apart rather than a lifetime average.
#[derive(Default)]
pub struct Usage {
    seen: HashMap<i64, (u64, f64)>,
}

impl Usage {
    /// pid → (cpu %, resident bytes). CPU is `None` on the first reading of
    /// a pid, because one reading is not a rate.
    pub fn sample(&mut self, pids: &[i64], at: f64) -> HashMap<i64, (Option<f64>, u64)> {
        let hz = {
            let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            if hz > 0 { hz as f64 } else { 100.0 }
        };
        let page = {
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            if page > 0 { page as u64 } else { 4096 }
        };
        let mut out = HashMap::new();
        for &pid in pids {
            let Ok(text) = std::fs::read_to_string(format!("/proc/{}/stat", pid)) else {
                continue;
            };
            let Some((ticks, rss)) = parse::parse_proc_stat(&text, page) else {
                continue;
            };
            let cpu = match self.seen.get(&pid) {
                Some((was, when)) if at - when > 0.0 => {
                    Some(ticks.saturating_sub(*was) as f64 / hz / (at - when) * 100.0)
                }
                _ => None,
            };
            self.seen.insert(pid, (ticks, at));
            out.insert(pid, (cpu, rss));
        }
        self.seen.retain(|pid, _| pids.contains(pid));
        out
    }
}

/// Process names from this process's parent upward, nearest first, read
/// from `/proc/<pid>/stat`. Stops at init, at a pid that cannot be read,
/// and after 64 steps whatever happens.
pub fn ancestors() -> Vec<String> {
    let mut out = Vec::new();
    let mut pid = unsafe { libc::getppid() } as i64;
    while pid > 1 && out.len() < 64 {
        let Ok(text) = std::fs::read_to_string(format!("/proc/{}/stat", pid)) else {
            break;
        };
        let Some((name, parent)) = super::detect::parse_stat_parent(&text) else {
            break;
        };
        out.push(name);
        pid = parent;
    }
    out
}
