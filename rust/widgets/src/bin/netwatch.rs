// terminal-toys - small dependency-free terminal widgets
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

//! Which processes are using the network, how much, and how fast.
//!
//! A port of netwatch.py, reading the same two things: `ss -tine` for the
//! per-socket byte counters and the inode beside them, and /proc/<pid>/fd
//! for the process that owns the inode.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use toys_core as tc;

const SERIES: usize = 240;

/// A braille cell is two dots wide and four tall, so one character holds
/// eight addressable points. The bit for each is fixed by the encoding.
const BRAILLE: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// Interfaces that are not the wire. A packet forwarded out of one of these
/// leaves through a real interface as well, and counting both counts it
/// twice.
const VIRTUAL: &[&str] = &[
    "lo", "tailscale0", "docker", "veth", "br-", "virbr", "wg", "tun", "tap", "cni", "flannel",
    "kube",
];

/// Systemd names the slice, not the thing in it.
const SLICES: &[&str] = &["system.slice", "user.slice", "init.scope", "-.slice", "app.slice"];

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Decimal units, as network equipment and ISPs quote them.
fn units(n: f64) -> String {
    for (suffix, scale) in [("GB", 1e9), ("MB", 1e6), ("KB", 1e3)] {
        if n >= scale {
            return format!("{:.1} {}", n / scale, suffix);
        }
    }
    format!("{} B", n as i64)
}

fn rate(n: f64) -> String {
    if n > 0.0 {
        format!("{}/s", units(n))
    } else {
        "-".into()
    }
}

fn elapsed(seconds: f64) -> String {
    let s = seconds as i64;
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    }
}

fn run(args: &[&str]) -> String {
    match std::process::Command::new(args[0]).args(&args[1..]).output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        _ => String::new(),
    }
}

/// Every address this machine answers to.
///
/// A connection to one of them is turned around inside the kernel and never
/// reaches a wire, so it is not traffic leaving the machine even though the
/// address is not loopback.
fn own_addresses() -> Vec<String> {
    let mut found = Vec::new();
    for line in run(&["ip", "-o", "addr"]).lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if let Some(at) = cols.iter().position(|c| *c == "inet" || *c == "inet6") {
            if let Some(addr) = cols.get(at + 1) {
                found.push(addr.split('/').next().unwrap_or(addr).to_string());
            }
        }
    }
    found
}

/// Whether this traffic never leaves the machine.
fn local_peer(host: &str, own: &[String]) -> bool {
    if host.starts_with("127.") || host == "::1" || host.is_empty() || host == "*" {
        return true;
    }
    let bare = host.strip_prefix("::ffff:").unwrap_or(host);
    own.iter().any(|a| a == bare)
}

/// Whether a peer is out on the internet rather than nearby.
fn off_box(host: &str, own: &[String]) -> bool {
    if local_peer(host, own) {
        return false;
    }
    let h = host.strip_prefix("::ffff:").unwrap_or(host);
    if h.starts_with("10.") || h.starts_with("192.168.") || h.starts_with("169.254.") {
        return false;
    }
    if h.starts_with("172.") {
        if let Some(second) = h.split('.').nth(1).and_then(|s| s.parse::<u16>().ok()) {
            if (16..=31).contains(&second) {
                return false;
            }
        }
    }
    if let Some(second) = h.strip_prefix("100.").and_then(|r| r.split('.').next()) {
        if let Ok(n) = second.parse::<u16>() {
            if (64..=127).contains(&n) {
                return false;
            }
        }
    }
    !(h.starts_with("fd7a:115c:a1e0")
        || h.starts_with("fe80:")
        || h.starts_with("fc")
        || h.starts_with("fd"))
}

#[derive(Clone)]
struct Seen {
    sent: u64,
    recv: u64,
    peer: String,
    port: u16,
    cgroup: String,
}

/// Every TCP socket's byte counters, keyed by inode.
///
/// -i for the counters, -e for the inode. Without the inode there is no
/// honest way to reach the process: `ss -p` needs root to name anybody
/// else's, while /proc/<pid>/fd needs nothing to name our own.
fn sockets(external: bool, own: &[String]) -> (HashMap<String, Seen>, String) {
    let text = run(&["ss", "-tine"]);
    if text.is_empty() {
        return (HashMap::new(), "ss would not run".into());
    }
    let mut found = HashMap::new();
    let (mut inode, mut peer, mut port, mut cgroup) = (None, String::new(), 0u16, String::new());
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            continue;
        }
        // A socket is two lines: the addresses and inode, then the counters
        // on an indented continuation. Neither is usable without the other.
        if !line.starts_with(' ') && !line.starts_with('\t') {
            let cols: Vec<&str> = line.split_whitespace().collect();
            peer = cols
                .get(4)
                .and_then(|a| a.rsplit_once(':'))
                .map(|(h, _)| h.trim_matches(|c| c == '[' || c == ']').to_string())
                .unwrap_or_default();
            port = cols
                .get(4)
                .and_then(|a| a.rsplit_once(':'))
                .and_then(|(_, p)| p.parse().ok())
                .unwrap_or(0);
            inode = field(line, "ino:").filter(|v| v != "0");
            cgroup = field(line, "cgroup:").unwrap_or_default();
            continue;
        }
        let id = match inode.take() {
            Some(id) => id,
            None => continue,
        };
        if local_peer(&peer, own) || (external && !off_box(&peer, own)) {
            continue;
        }
        found.insert(
            id,
            Seen {
                sent: field(line, "bytes_sent:")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
                recv: field(line, "bytes_received:")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
                peer: peer.clone(),
                port,
                cgroup: cgroup.clone(),
            },
        );
    }
    (found, String::new())
}

/// The value after `key:` on a line, up to the next space.
fn field(line: &str, key: &str) -> Option<String> {
    let at = line.find(key)? + key.len();
    let rest = &line[at..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let value = &rest[..end];
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Who owns a socket, from the cgroup the kernel already reports.
///
/// Another user's /proc is closed, but `ss` prints the control group for
/// every socket regardless, and on a systemd machine that names the unit.
fn unit_name(cgroup: &str) -> String {
    for part in cgroup.trim_matches('/').split('/').rev() {
        if part.is_empty() || SLICES.contains(&part) {
            continue;
        }
        let mut name = part;
        for suffix in [".service", ".scope", ".slice"] {
            if let Some(base) = name.strip_suffix(suffix) {
                name = base;
                break;
            }
        }
        // A login session is a person, not a program.
        if name.starts_with("session-") || name.starts_with("user-") {
            continue;
        }
        return name.to_string();
    }
    String::new()
}

/// inode -> (pid, name), for every process this user can read.
fn socket_owners() -> HashMap<String, (i32, String)> {
    let mut owners = HashMap::new();
    let entries = match std::fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return owners,
    };
    for entry in entries.flatten() {
        let pid: i32 = match entry.file_name().to_string_lossy().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let fds = match std::fs::read_dir(format!("/proc/{}/fd", pid)) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let mut name = String::new();
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                let target = target.to_string_lossy();
                if let Some(rest) = target.strip_prefix("socket:[") {
                    if name.is_empty() {
                        name = process_name(pid);
                    }
                    owners.insert(rest.trim_end_matches(']').to_string(), (pid, name.clone()));
                }
            }
        }
    }
    owners
}

/// What to call a process, preferring something a person would recognise.
///
/// /proc/<pid>/comm is the kernel's answer and usually right, but some
/// binaries are versioned - .../claude/versions/2.1.233 reports itself as
/// "2.1.233", which is true and useless.
fn process_name(pid: i32) -> String {
    let comm = std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if comm.chars().any(|c| c.is_ascii_alphabetic()) {
        return comm;
    }
    const GENERIC: &[&str] = &[
        "versions", "bin", "sbin", "libexec", "node_modules", "dist", "build", "lib", "share",
        "local", "current", "releases",
    ];
    let argv0 = std::fs::read(format!("/proc/{}/cmdline", pid))
        .map(|raw| {
            String::from_utf8_lossy(&raw)
                .split('\0')
                .next()
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_default();
    for part in argv0.split('/').rev() {
        if part.chars().any(|c| c.is_ascii_alphabetic())
            && !GENERIC.contains(&part.to_lowercase().as_str())
        {
            return part.to_string();
        }
    }
    if comm.is_empty() {
        "?".into()
    } else {
        comm
    }
}

/// Bytes in and out of this machine's real interfaces.
///
/// The kernel counts these whatever produced them, which is the point: a
/// packet this machine routes rather than terminates never touches a
/// socket. On an exit node that is most of the traffic.
fn wire_bytes() -> Option<(u64, u64, Vec<String>)> {
    let text = std::fs::read_to_string("/proc/net/dev").ok()?;
    let (mut rx, mut tx, mut names) = (0u64, 0u64, Vec::new());
    for line in text.lines().skip(2) {
        let (name, rest) = line.split_once(':')?;
        let name = name.trim();
        if VIRTUAL.iter().any(|v| name.starts_with(v)) {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 9 {
            continue;
        }
        rx += fields[0].parse::<u64>().unwrap_or(0);
        tx += fields[8].parse::<u64>().unwrap_or(0);
        names.push(name.to_string());
    }
    Some((rx, tx, names))
}

/// Which interfaces are being counted, for the end of the line.
fn wire_label(names: &[String]) -> String {
    match names.len() {
        0 => String::new(),
        1..=3 => names.join(", "),
        n => format!("{} of them", n),
    }
}

#[derive(Clone, Default)]
struct Proc {
    pid: i32,
    name: String,
    up: u64,
    down: u64,
    up_rate: f64,
    down_rate: f64,
    alive: bool,
}

#[derive(Default)]
struct State {
    totals: HashMap<(i32, String), Proc>,
    last: HashMap<String, (u64, u64)>,
    series: Vec<(f64, f64, f64, f64)>,
    stamp: f64,
    started: f64,
    wire: Option<(u64, u64)>,
    wire_rate: (f64, f64),
    wire_names: Vec<String>,
    err: String,
}

fn sample(state: &mut State, external: bool) {
    let stamp = now();
    let own = own_addresses();
    let counters = wire_bytes();
    let (found, err) = sockets(external, &own);
    let owners = if found.is_empty() {
        HashMap::new()
    } else {
        socket_owners()
    };
    let gap = if state.stamp > 0.0 {
        (stamp - state.stamp).max(1e-6)
    } else {
        0.0
    };
    state.err = err;

    for row in state.totals.values_mut() {
        row.up_rate = 0.0;
        row.down_rate = 0.0;
        row.alive = false;
    }

    let first = state.stamp == 0.0;
    for (inode, seen) in &found {
        let was = state.last.get(inode).copied();
        // A socket opened since the last sample started at zero when it was
        // created, so all of its counters are traffic that happened while
        // we were watching. Only sockets already open at the first sample
        // are zeroed - the difference is a connection that opens and closes
        // inside one interval, whose bytes would otherwise never count.
        let (d_sent, d_recv) = if first {
            (0, 0)
        } else {
            match was {
                None => (seen.sent, seen.recv),
                // A reused inode reads lower than it did; subtracting would
                // underflow, so the new socket's own figures are the delta.
                Some((s, r)) if seen.sent < s || seen.recv < r => (seen.sent, seen.recv),
                Some((s, r)) => (seen.sent - s, seen.recv - r),
            }
        };
        let (pid, name) = match owners.get(inode) {
            Some((pid, name)) => (*pid, name.clone()),
            None => {
                let unit = unit_name(&seen.cgroup);
                (
                    0,
                    if unit.is_empty() {
                        "(unattributed)".into()
                    } else {
                        unit
                    },
                )
            }
        };
        let row = state
            .totals
            .entry((pid, name.clone()))
            .or_insert_with(|| Proc {
                pid,
                name,
                ..Default::default()
            });
        row.alive = true;
        row.up += d_sent;
        row.down += d_recv;
        if gap > 0.0 {
            row.up_rate += d_sent as f64 / gap;
            row.down_rate += d_recv as f64 / gap;
        }
    }

    // Both totals are recorded every sample, so the filter is a display
    // choice: o flips instantly and the chart redraws over history it
    // already had rather than starting again.
    if gap > 0.0 {
        let mine_down: f64 = state
            .totals
            .values()
            .filter(|r| r.pid != 0)
            .map(|r| r.down_rate)
            .sum();
        let mine_up: f64 = state
            .totals
            .values()
            .filter(|r| r.pid != 0)
            .map(|r| r.up_rate)
            .sum();
        let all_down: f64 = state.totals.values().map(|r| r.down_rate).sum();
        let all_up: f64 = state.totals.values().map(|r| r.up_rate).sum();
        state.series.push((mine_down, mine_up, all_down, all_up));
        if state.series.len() > SERIES {
            let drop = state.series.len() - SERIES;
            state.series.drain(..drop);
        }
    }

    if let Some((rx, tx, names)) = counters {
        if let Some((was_rx, was_tx)) = state.wire {
            if gap > 0.0 {
                state.wire_rate = (
                    rx.saturating_sub(was_rx) as f64 / gap,
                    tx.saturating_sub(was_tx) as f64 / gap,
                );
            }
        }
        state.wire = Some((rx, tx));
        state.wire_names = names;
    }

    state.last = found.iter().map(|(k, v)| (k.clone(), (v.sent, v.recv))).collect();
    state.stamp = stamp;
}

/// Plot a series on a dot canvas eight times finer than the cells.
///
/// Two dots per column and four per row, which is the difference between a
/// line that steps between character rows and one that reads as a curve.
fn braille_canvas(values: &[f64], peak: f64, cols: usize, rows: usize, inverted: bool) -> Vec<Vec<u8>> {
    let (px_w, px_h) = (cols * 2, rows * 4);
    let mut grid = vec![vec![0u8; cols]; rows];
    let vals: Vec<f64> = values.iter().rev().take(px_w).rev().copied().collect();
    if vals.is_empty() {
        return grid;
    }
    let point = |i: usize| -> (i64, i64) {
        let x = if vals.len() == 1 {
            px_w as i64 - 1
        } else {
            ((i as f64) * (px_w as f64 - 1.0) / (vals.len() as f64 - 1.0)).round() as i64
        };
        let scaled = if peak > 0.0 {
            (vals[i] / peak).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let magnitude = (scaled * (px_h as f64 - 1.0)).round() as i64;
        (x, if inverted { magnitude } else { px_h as i64 - 1 - magnitude })
    };
    let mut dot = |x: i64, y: i64, grid: &mut Vec<Vec<u8>>| {
        if x >= 0 && (x as usize) < px_w && y >= 0 && (y as usize) < px_h {
            grid[y as usize / 4][x as usize / 2] |= BRAILLE[y as usize % 4][x as usize % 2];
        }
    };
    if vals.len() == 1 {
        if vals[0] > 0.0 {
            let (x, y) = point(0);
            dot(x, y, &mut grid);
        }
        return grid;
    }
    for i in 1..vals.len() {
        // An idle stretch draws nothing at all rather than a flat line
        // pinned to the axis, which would read as activity at zero.
        if vals[i - 1] == 0.0 && vals[i] == 0.0 {
            continue;
        }
        let (mut x0, mut y0) = point(i - 1);
        let (x1, y1) = point(i);
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            dot(x0, y0, &mut grid);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let twice = 2 * err;
            if twice >= dy {
                err += dy;
                x0 += sx;
            }
            if twice <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }
    grid
}

fn braille_row(masks: &[u8], colour: &str) -> Vec<(String, String)> {
    masks
        .iter()
        .map(|m| {
            (
                colour.to_string(),
                if *m == 0 {
                    " ".to_string()
                } else {
                    char::from_u32(0x2800 + *m as u32).unwrap_or(' ').to_string()
                },
            )
        })
        .collect()
}

fn main() {
    tc::maybe_help(include_str!("netwatch_help.txt"));
    let mut interval = 1.0f64;
    let mut limit = 0usize;
    let mut external = true;
    let mut mine = true;
    let mut sort_live = false;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-i" | "--interval" if i + 1 < args.len() => {
                interval = args[i + 1].parse::<f64>().unwrap_or(1.0).max(0.2);
                i += 2;
            }
            "-n" | "--limit" if i + 1 < args.len() => {
                limit = args[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            "--sort" if i + 1 < args.len() => {
                sort_live = args[i + 1] == "live";
                i += 2;
            }
            "--all-external" => {
                external = false;
                i += 1;
            }
            "--all-users" => {
                mine = false;
                i += 1;
            }
            "-V" | "--version" => {
                println!("netwatch 1.1");
                return;
            }
            _ => i += 1,
        }
    }

    let p = palette();
    let state = Arc::new(Mutex::new(State {
        started: now(),
        ..Default::default()
    }));
    let poller = Arc::clone(&state);
    std::thread::spawn(move || loop {
        {
            let mut guard = match poller.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            sample(&mut guard, external);
        }
        std::thread::sleep(Duration::from_secs_f64(interval));
    });

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut selected = 0usize;

    loop {
        for key in keyboard.poll() {
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "1" => sort_live = false,
                "2" => sort_live = true,
                "s" | "S" | "t" | "T" => sort_live = !sort_live,
                "o" | "O" => {
                    mine = !mine;
                    selected = 0;
                }
                "up" | "k" | "K" => selected = selected.saturating_sub(1),
                "down" | "j" | "J" => selected += 1,
                "r" | "R" => {
                    if let Ok(mut guard) = state.lock() {
                        guard.totals.clear();
                        guard.series.clear();
                        guard.started = now();
                    }
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let guard = match state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let mut rows: Vec<Proc> = guard
            .totals
            .values()
            .filter(|r| !mine || r.pid != 0)
            .cloned()
            .collect();
        if sort_live {
            rows.sort_by(|a, b| {
                (b.up_rate + b.down_rate)
                    .partial_cmp(&(a.up_rate + a.down_rate))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then((b.up + b.down).cmp(&(a.up + a.down)))
            });
        } else {
            rows.sort_by(|a, b| {
                (b.up + b.down).cmp(&(a.up + a.down)).then(
                    (b.up_rate + b.down_rate)
                        .partial_cmp(&(a.up_rate + a.down_rate))
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
            });
        }
        if !rows.is_empty() && selected >= rows.len() {
            selected = rows.len() - 1;
        }
        let moving = rows.iter().filter(|r| r.up_rate + r.down_rate > 0.0).count();
        let down: f64 = rows.iter().map(|r| r.down_rate).sum();
        let up: f64 = rows.iter().map(|r| r.up_rate).sum();

        let mut out = vec![tc::title("netwatch", w, &p.accent)];
        out.push(tc::seg(
            &[
                (
                    p.dim.as_str(),
                    format!(
                        " {} process{}",
                        rows.len(),
                        if rows.len() == 1 { "" } else { "es" }
                    ),
                ),
                (p.dim.as_str(), format!(" · {} moving", moving)),
                (p.dim.as_str(), " · ".into()),
                (p.accent.as_str(), elapsed(now() - guard.started)),
                (p.dim.as_str(), " · sorted by ".into()),
                (
                    p.accent.as_str(),
                    if sort_live { "live".into() } else { "total".into() },
                ),
                (p.dim.as_str(), format!("   every {}s", interval)),
            ],
            w - 1,
        ));
        out.push(tc::seg(
            &[
                (
                    p.dim.as_str(),
                    if mine {
                        " TCP only · ".into()
                    } else {
                        " TCP only · every user · ".into()
                    },
                ),
                (p.down.as_str(), format!("↓ {}", rate(down))),
                (p.dim.as_str(), "  ".into()),
                (p.up.as_str(), format!("↑ {}", rate(up))),
                (
                    p.dim.as_str(),
                    if external {
                        "  · internet only".into()
                    } else {
                        "  · everything off-box".into()
                    },
                ),
            ],
            w - 1,
        ));

        // What the interfaces actually moved, against what the sockets can
        // explain. On a router the two differ by most of the traffic.
        let (wire_rx, wire_tx) = guard.wire_rate;
        let wire = wire_rx + wire_tx;
        if wire > 0.0 {
            let share = ((down + up) / wire).min(1.0);
            let mut said = vec![
                (p.lbl.as_str(), " interfaces".to_string()),
                (p.dim.as_str(), " · ".into()),
                (p.down.as_str(), format!("↓ {}", rate(wire_rx))),
                (p.dim.as_str(), "  ".into()),
                (p.up.as_str(), format!("↑ {}", rate(wire_tx))),
                (p.dim.as_str(), "  · ".into()),
                (
                    if share >= 0.9 { &p.dim } else { &p.warn },
                    format!("{:.0}% of it has a socket", share * 100.0),
                ),
            ];
            let which = wire_label(&guard.wire_names);
            let used: usize = said.iter().map(|(_, t)| t.chars().count()).sum();
            if !which.is_empty() && used + which.chars().count() + 4 <= w - 1 {
                said.push((p.grid.as_str(), format!("  · {}", which)));
            }
            out.push(tc::seg(&said, w - 1));
        }
        if !guard.err.is_empty() {
            out.push(tc::seg(&[(p.bad.as_str(), format!(" ! {}", guard.err))], w - 1));
        }
        out.push(String::new());

        // The chart takes a share of the pane and the list takes the rest,
        // but the list is the point: below a certain height there is no
        // chart at all rather than two rows of neither.
        let spare = h.saturating_sub(out.len() + 4);
        let graph_h = if spare >= 20 {
            9
        } else if spare >= 15 {
            7
        } else if spare >= 11 {
            5
        } else {
            0
        };
        let series: Vec<(f64, f64)> = guard
            .series
            .iter()
            .map(|s| if mine { (s.0, s.1) } else { (s.2, s.3) })
            .collect();
        if graph_h > 0 && !series.is_empty() {
            out.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── PROCESS WATCH ── ".into()),
                    (p.up.as_str(), "↑ tx above".into()),
                    (p.dim.as_str(), " · ".into()),
                    (p.down.as_str(), "↓ rx below".into()),
                    (
                        p.dim.as_str(),
                        format!("  · {} of history", elapsed(series.len() as f64 * interval)),
                    ),
                ],
                w - 1,
            ));
            out.extend(chart(&series, w, graph_h, &p));
            out.push(String::new());
        }

        let room = h.saturating_sub(out.len() + 3).max(1);
        let show = if limit > 0 { limit.min(room) } else { room };
        if rows.is_empty() {
            out.push(tc::seg(
                &[(
                    p.dim.as_str(),
                    "  Nothing has moved yet. Totals start at zero, so this fills as traffic happens.".into(),
                )],
                w - 1,
            ));
        } else {
            out.extend(table(&rows, w, show, selected, &p));
        }

        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
            vec![(p.accent.as_str(), "↵".into()), (p.dim.as_str(), " details".into())],
            vec![(
                if sort_live { &p.dim } else { &p.accent },
                "[1] total".into(),
            )],
            vec![(
                if sort_live { &p.accent } else { &p.dim },
                "[2] live".into(),
            )],
            vec![(
                p.dim.as_str(),
                format!("[o]{} others", if mine { "show" } else { "hide" }),
            )],
            vec![(p.dim.as_str(), "[r]ezero".into())],
            vec![(p.dim.as_str(), "[q]uit".into())],
        ];
        drop(guard);
        let foot: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
        while out.len() < h.saturating_sub(foot.len()) {
            out.push(String::new());
        }
        out.extend(foot);
        tc::draw(&out, w, h);
        std::thread::sleep(Duration::from_millis(300).min(Duration::from_secs_f64(interval)));
    }
}

/// Sent above the line, received below it, newest on the right.
///
/// That way round because of the arrows: ↑ means upload and ↓ means
/// download, so upload has to be the half that goes up.
fn chart(series: &[(f64, f64)], w: usize, h: usize, p: &Palette) -> Vec<String> {
    let canvas = h.saturating_sub(3).max(2);
    let up_h = (canvas / 2).max(1);
    let down_h = canvas.saturating_sub(up_h).max(1);
    let plot = w.saturating_sub(18).max(12);
    let window: Vec<(f64, f64)> = series.iter().rev().take(plot * 2).rev().copied().collect();
    let rx: Vec<f64> = window.iter().map(|s| s.0).collect();
    let tx: Vec<f64> = window.iter().map(|s| s.1).collect();
    let rx_peak = rx.iter().cloned().fold(0.0f64, f64::max).max(1.0);
    let tx_peak = tx.iter().cloned().fold(0.0f64, f64::max).max(1.0);
    // Each label carries its own direction and neither is signed: rx is not
    // negative traffic, it is simply the half drawn downward.
    let up_label = format!("↑ {}", rate(tx_peak));
    let down_label = format!("↓ {}", rate(rx_peak));
    let lab = up_label
        .chars()
        .count()
        .max(down_label.chars().count())
        .clamp(9, 16);
    let plot = w.saturating_sub(lab + 4).max(12);

    let mut out = Vec::new();
    out.push(tc::seg(
        &[
            (p.up.as_str(), format!("{:>1$} ", up_label, lab)),
            (p.grid.as_str(), format!("┌{}┐", "─".repeat(plot))),
        ],
        w - 1,
    ));
    for masks in braille_canvas(&tx, tx_peak, plot, up_h, false) {
        let mut line = vec![
            (p.dim.clone(), " ".repeat(lab + 1)),
            (p.grid.clone(), "│".into()),
        ];
        line.extend(braille_row(&masks, &p.up));
        line.push((p.grid.clone(), "│".into()));
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        out.push(tc::seg(&refs, w - 1));
    }
    out.push(tc::seg(
        &[
            (p.dim.as_str(), format!("{:>1$} ", "0", lab)),
            (p.grid.as_str(), format!("├{}┤", "─".repeat(plot))),
        ],
        w - 1,
    ));
    for masks in braille_canvas(&rx, rx_peak, plot, down_h, true) {
        let mut line = vec![
            (p.dim.clone(), " ".repeat(lab + 1)),
            (p.grid.clone(), "│".into()),
        ];
        line.extend(braille_row(&masks, &p.down));
        line.push((p.grid.clone(), "│".into()));
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        out.push(tc::seg(&refs, w - 1));
    }
    out.push(tc::seg(
        &[
            (p.down.as_str(), format!("{:>1$} ", down_label, lab)),
            (p.grid.as_str(), format!("└{}┘", "─".repeat(plot))),
        ],
        w - 1,
    ));
    out
}

/// The process table, dropping columns rather than clipping them.
fn table(rows: &[Proc], w: usize, limit: usize, selected: usize, p: &Palette) -> Vec<String> {
    let avail = (w - 1).saturating_sub(2 + 8 + 11);
    let wide = avail >= 10 + 11 + 22;
    let mid = avail >= 10 + 11;
    let name_w = avail
        .saturating_sub(if wide { 33 } else if mid { 11 } else { 0 })
        .clamp(8, 26);

    let mut head = vec![
        (p.dim.as_str(), format!("  {}", tc::pad("PROCESS", name_w))),
        (p.dim.as_str(), format!("{:<8}", "PID")),
        (p.dim.as_str(), format!("{:>11}", "TOTAL")),
    ];
    if mid {
        head.push((p.dim.as_str(), format!("{:>11}", "NOW")));
    }
    if wide {
        head.push((p.dim.as_str(), format!("{:>11}", "DOWN")));
        head.push((p.dim.as_str(), format!("{:>11}", "UP")));
    }
    let mut out = vec![tc::seg(&head, w - 1)];

    for (i, row) in rows.iter().take(limit).enumerate() {
        let live = row.up_rate + row.down_rate;
        let total = (row.up + row.down) as f64;
        let here = i == selected;
        let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
        let name_c = format!(
            "{}{}",
            tint,
            if here {
                &p.accent
            } else if row.alive {
                &p.txt
            } else {
                &p.dim
            }
        );
        let pid_c = format!("{}{}", tint, p.dim);
        let total_c = format!("{}{}", tint, if total > 0.0 { &p.txt } else { &p.dim });
        let live_c = format!("{}{}", tint, if live > 0.0 { &p.ok } else { &p.dim });
        let down_c = format!("{}{}", tint, if row.down_rate > 0.0 { &p.down } else { &p.dim });
        let up_c = format!("{}{}", tint, if row.up_rate > 0.0 { &p.up } else { &p.dim });
        let name: String = row.name.chars().take(name_w.saturating_sub(2)).collect();
        let mut line = vec![
            (
                name_c.as_str(),
                format!("{} {}", if here { "▸" } else { " " }, tc::pad(&name, name_w - 1)),
            ),
            (
                pid_c.as_str(),
                format!("{:<8}", if row.pid > 0 { row.pid.to_string() } else { "-".into() }),
            ),
            (total_c.as_str(), format!("{:>11}", units(total))),
        ];
        if mid {
            line.push((live_c.as_str(), format!("{:>11}", rate(live))));
        }
        if wide {
            line.push((down_c.as_str(), format!("{:>11}", rate(row.down_rate))));
            line.push((up_c.as_str(), format!("{:>11}", rate(row.up_rate))));
        }
        if here {
            line.push((tint.as_str(), " ".repeat(w)));
        }
        out.push(tc::seg(&line, w - 1));
    }
    out
}

struct Palette {
    ok: String,
    warn: String,
    bad: String,
    dim: String,
    grid: String,
    txt: String,
    lbl: String,
    accent: String,
    down: String,
    up: String,
}

fn palette() -> Palette {
    Palette {
        ok: tc::rgb(90, 240, 160),
        warn: tc::rgb(255, 200, 90),
        bad: tc::rgb(255, 100, 110),
        dim: tc::rgb(127, 147, 172),
        grid: tc::rgb(60, 78, 98),
        txt: tc::rgb(225, 235, 245),
        lbl: tc::rgb(130, 165, 200),
        accent: tc::rgb(150, 210, 255),
        down: tc::rgb(120, 200, 255),
        up: tc::rgb(255, 170, 120),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_are_decimal_as_isps_quote_them() {
        assert_eq!(units(1000.0), "1.0 KB");
        assert_eq!(units(2_500_000.0), "2.5 MB");
        assert_eq!(units(3_000_000_000.0), "3.0 GB");
        assert_eq!(units(512.0), "512 B");
    }

    #[test]
    fn traffic_that_never_leaves_is_recognised() {
        let own = vec!["10.240.0.46".to_string(), "100.89.99.102".to_string()];
        assert!(local_peer("127.0.0.1", &own));
        assert!(local_peer("::1", &own));
        // The half that is easy to miss: our own non-loopback address.
        assert!(local_peer("10.240.0.46", &own));
        assert!(local_peer("::ffff:10.240.0.46", &own));
        assert!(!local_peer("10.240.0.99", &own));
    }

    #[test]
    fn only_globally_routable_peers_are_off_box() {
        let own = vec!["10.240.0.46".to_string()];
        assert!(off_box("160.79.104.10", &own));
        assert!(!off_box("10.0.0.5", &own));
        assert!(!off_box("172.16.0.1", &own));
        assert!(!off_box("192.168.1.1", &own));
        assert!(!off_box("100.89.99.102", &own));
        assert!(!off_box("127.0.0.1", &own));
        // 172.32 is outside the private range and really is out there.
        assert!(off_box("172.32.0.1", &own));
    }

    #[test]
    fn a_cgroup_names_the_daemon() {
        assert_eq!(unit_name("/system.slice/tailscaled.service"), "tailscaled");
        assert_eq!(
            unit_name("/system.slice/google-guest-agent.service"),
            "google-guest-agent"
        );
        // A login session says who is logged in, not what opened the socket.
        assert_eq!(unit_name("/user.slice/user-1002.slice/session-1.scope"), "");
        assert_eq!(unit_name("/"), "");
    }

    #[test]
    fn ss_fields_are_read_off_the_line() {
        let line = "\t ts sack cubic bytes_sent:1669 bytes_acked:1670 bytes_received:11469 segs_out:272";
        assert_eq!(field(line, "bytes_sent:"), Some("1669".into()));
        assert_eq!(field(line, "bytes_received:"), Some("11469".into()));
        assert_eq!(field(line, "nothing:"), None);
    }

    #[test]
    fn virtual_interfaces_are_not_the_wire() {
        // Counting a tunnel as well as the card counts a forwarded packet
        // twice, which is the whole reason for the exclusion list.
        for name in ["lo", "tailscale0", "docker0", "veth1234"] {
            assert!(VIRTUAL.iter().any(|v| name.starts_with(v)), "{}", name);
        }
        for name in ["ens4", "eth0", "wlan0"] {
            assert!(!VIRTUAL.iter().any(|v| name.starts_with(v)), "{}", name);
        }
    }

    #[test]
    fn an_idle_series_draws_nothing() {
        let grid = braille_canvas(&[0.0, 0.0, 0.0, 0.0], 1.0, 10, 2, false);
        assert!(grid.iter().all(|row| row.iter().all(|c| *c == 0)));
    }

    #[test]
    fn a_spike_puts_dots_on_the_canvas() {
        let grid = braille_canvas(&[0.0, 5.0, 0.0], 5.0, 10, 2, false);
        assert!(grid.iter().any(|row| row.iter().any(|c| *c != 0)));
    }

    #[test]
    fn the_interface_label_names_few_and_counts_many() {
        assert_eq!(wire_label(&["ens4".into()]), "ens4");
        assert_eq!(wire_label(&["ens4".into(), "eth1".into()]), "ens4, eth1");
        let many: Vec<String> = (0..5).map(|i| format!("eth{}", i)).collect();
        assert_eq!(wire_label(&many), "5 of them");
    }
}
