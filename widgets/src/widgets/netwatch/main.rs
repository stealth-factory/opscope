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

//! Which processes are using the network, how much, and how fast.
//!
//! A port of netwatch.py, reading the same two things: `ss -tine` for the
//! per-socket byte counters and the inode beside them, and /proc/<pid>/fd
//! for the process that owns the inode.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opscope_core as tc;

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "netwatch",
    section: "netwatch",
    legacy_section: None,
    schema: include_str!("settings.json"),
};

const SERIES: usize = 240;

/// A braille cell is two dots wide and four tall, so one character holds
/// eight addressable points. The bit for each is fixed by the encoding.

/// Interfaces that are not the wire. A packet forwarded out of one of these
/// leaves through a real interface as well, and counting both counts it
/// twice.
const VIRTUAL: &[&str] = &[
    "lo", "tailscale0", "docker", "veth", "br-", "virbr", "wg", "tun", "tap", "cni", "flannel",
    "kube",
];

/// Systemd names the slice, not the thing in it.
const SLICES: &[&str] = &["system.slice", "user.slice", "init.scope", "-.slice", "app.slice"];

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

/// Seconds before an external command is given up on, from netwatch.py.
const RUN_TIMEOUT: u64 = 5;

/// Every address this machine answers to.
///
/// A connection to one of them is turned around inside the kernel and never
/// reaches a wire, so it is not traffic leaving the machine even though the
/// address is not loopback.
fn own_addresses() -> Vec<String> {
    let mut found = Vec::new();
    for line in tc::run_quiet(&["ip", "-o", "addr"], RUN_TIMEOUT).lines() {
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
    /// The local port - `ss` column 3, where column 4 is the peer's. Five
    /// sockets to one CDN all read "1.2.3.4:443", and this is the only field
    /// that tells them apart: the peer's port is 443 on every one of them.
    mine: u16,
    cgroup: String,
}

/// Every TCP socket's byte counters, keyed by inode.
///
/// -i for the counters, -e for the inode. Without the inode there is no
/// honest way to reach the process: `ss -p` needs root to name anybody
/// else's, while /proc/<pid>/fd needs nothing to name our own.
fn sockets(external: bool, own: &[String]) -> (HashMap<String, Seen>, String) {
    let text = tc::run_quiet(&["ss", "-tine"], RUN_TIMEOUT);
    if text.is_empty() {
        return (HashMap::new(), "ss would not run".into());
    }
    let mut found = HashMap::new();
    let (mut inode, mut peer, mut port, mut cgroup) = (None, String::new(), 0u16, String::new());
    let mut mine = 0u16;
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
            // Column 3 is our address, column 4 is theirs.
            mine = cols
                .get(3)
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
                mine,
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

/// How the detail screen reports where you are in it, at a width that does
/// not change with the numbers - the footer is packed before the body is
/// measured, so a label that grew by a character could wrap the hints onto
/// another line and leave the body sized for a footer that is not there.
///
/// link.rs carries the same function; TOY-7 tracks folding the pair into
/// opscope-core rather than fixing anything here twice.
fn scroll_label(first: usize, last: usize, total: usize) -> String {
    format!("rows {:>3}-{:>3} of {:>3}", first, last, total)
}

/// How long a rate is averaged over.
///
/// One sample interval is the honest instantaneous rate and an unreadable
/// column: nearly all traffic is bursty, so a process that is steadily busy
/// flickers between a figure and a dash. Averaging over a few seconds is
/// just as true - it is a rate over a stated window - and can actually be
/// read. The header says which window, so the number is not a mystery.
const RATE_WINDOW: f64 = 10.0;

/// Fold one sample into a counter and re-average over the window.
///
/// A macro rather than a trait because processes, connections and endpoints
/// carry the same six fields and want exactly the same arithmetic; the one
/// thing that must not happen is the three drifting apart on what a rate
/// means.
macro_rules! fold {
    ($e:expr, $when:expr, $gap:expr, $up:expr, $down:expr) => {{
        let e = &mut $e;
        e.up += $up;
        e.down += $down;
        e.seen = $when;
        e.alive = true;
        e.recent.push(($when, $gap, $up, $down));
        e.recent.retain(|(t, _, _, _)| $when - t <= RATE_WINDOW);
        // Each delta covers the interval ending at its stamp and lasting
        // its own gap, so the time the window covers is the sum of those
        // gaps - counting each stamp once, because a process's fifteen
        // sockets fold at one stamp and share one interval between them.
        // Dividing instead by the span from oldest stamp to newest counted
        // n deltas over n-1 intervals, and every process, connection and
        // endpoint row read a quarter high at a one-second cadence.
        let mut span = 0.0f64;
        let mut last = f64::NAN;
        let (mut u, mut d) = (0u64, 0u64);
        for (t, gap, up, down) in &e.recent {
            if *t != last {
                span += gap;
                last = *t;
            }
            u += up;
            d += down;
        }
        // A process with fifteen sockets folds fifteen samples at one
        // timestamp, and the window then spans no time at all. Dividing by
        // an epsilon turned five megabytes into a rate of a terabyte a
        // second and pinned the chart's axis there for four minutes. With
        // no elapsed time there is no rate to compute, so the last one
        // stands until the next sample gives the window a width.
        if span > 0.0 {
            e.up_rate = u as f64 / span;
            e.down_rate = d as f64 / span;
        }
    }};
}

/// Drop what has fallen out of the window, and say so when nothing is left.
macro_rules! settle {
    ($e:expr, $when:expr) => {{
        let e = &mut $e;
        e.alive = false;
        e.recent.retain(|(t, _, _, _)| $when - t <= RATE_WINDOW);
        if e.recent.is_empty() {
            e.up_rate = 0.0;
            e.down_rate = 0.0;
        }
    }};
}

/// Keep a series bounded without reallocating the whole thing each sample.
fn trim<T>(series: &mut Vec<T>, most: usize) {
    if series.len() > most {
        let drop = series.len() - most;
        series.drain(..drop);
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
    seen: f64,
    /// (when, up bytes, down bytes) for the last few samples.
    /// (stamp, the gap it covers, bytes up, bytes down)
    recent: Vec<(f64, f64, u64, u64)>,
    /// (down rate, up rate) per sample, for this process's own chart.
    hist: Vec<(f64, f64)>,
    /// (read rate, write rate) per sample, from /proc/<pid>/io.
    ///
    /// Disk was a line of two lifetime totals, which answers "has it ever
    /// touched the disk" and not "is it touching it now" - the question the
    /// network half of this screen exists to answer. Same series, same
    /// chart, so the two can be read against each other.
    disk: Vec<(f64, f64)>,
    /// (when, read bytes, write bytes) at the previous sample. The kernel
    /// reports lifetime counters, so a rate needs the one before.
    io_was: Option<(f64, u64, u64)>,
}

/// One socket, so the detail screen can say which of a process's dozen
/// connections is the one actually moving.
#[derive(Clone, Default)]
struct Conn {
    pid: i32,
    name: String,
    peer: String,
    port: u16,
    /// The local port. See `Seen::mine`.
    mine: u16,
    up: u64,
    down: u64,
    up_rate: f64,
    down_rate: f64,
    alive: bool,
    seen: f64,
    /// (stamp, the gap it covers, bytes up, bytes down)
    recent: Vec<(f64, f64, u64, u64)>,
    /// (down rate, up rate) per sample, so a selected socket can be charted
    /// the way a selected endpoint is.
    hist: Vec<(f64, f64)>,
}

/// The sockets sharing a peer, folded together: a browser opening six
/// connections to one host is one thing being talked to, not six.
#[derive(Clone, Default)]
struct Spot {
    pid: i32,
    name: String,
    peer: String,
    up: u64,
    down: u64,
    up_rate: f64,
    down_rate: f64,
    alive: bool,
    seen: f64,
    ports: BTreeSet<u16>,
    /// (stamp, the gap it covers, bytes up, bytes down)
    recent: Vec<(f64, f64, u64, u64)>,
    hist: Vec<(f64, f64)>,
}

#[derive(Default)]
struct State {
    totals: HashMap<(i32, String), Proc>,
    conns: HashMap<String, Conn>,
    spots: HashMap<(i32, String, String), Spot>,
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
    let stamp = tc::now();
    let own = own_addresses();
    let counters = wire_bytes();
    let (found, err) = sockets(external, &own);
    let owners = if found.is_empty() {
        HashMap::new()
    } else {
        socket_owners()
    };
    absorb(state, stamp, &found, &owners, counters, err);
}

/// Fold one reading into the running totals.
///
/// Split from `sample` so the arithmetic can be exercised without a machine
/// that happens to have the right sockets open on it.
fn absorb(
    state: &mut State,
    stamp: f64,
    found: &HashMap<String, Seen>,
    owners: &HashMap<String, (i32, String)>,
    counters: Option<(u64, u64, Vec<String>)>,
    err: String,
) {
    // A failed `ss` is not a machine with no sockets, and the difference
    // matters more than it looks: this used to say why and then go on to
    // overwrite `state.last` with the empty map it had been handed. The
    // next good poll found no baseline for a socket that had been open for
    // hours, counted its whole lifetime as one interval's traffic, and
    // divided that by the gap since the failed poll. The number that
    // reached the screen was not merely wrong, it was enormous, and it
    // stayed in the totals afterwards.
    //
    // `state.last` and `state.stamp` are one baseline and have to move
    // together: keeping the counters while advancing the clock would divide
    // two intervals of traffic by one interval of time, which is the same
    // bug with a smaller number on it. So a failed reading advances
    // neither - it records why and changes nothing else.
    state.err = err;
    if !state.err.is_empty() {
        return;
    }
    let gap = if state.stamp > 0.0 {
        (stamp - state.stamp).max(1e-6)
    } else {
        0.0
    };

    // A row with nothing left in the window really is idle, and says so.
    for row in state.totals.values_mut() {
        settle!(*row, stamp);
    }
    for conn in state.conns.values_mut() {
        settle!(*conn, stamp);
    }
    for spot in state.spots.values_mut() {
        settle!(*spot, stamp);
    }

    let first = state.stamp == 0.0;
    for (inode, seen) in found {
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
                name: name.clone(),
                ..Default::default()
            });
        row.alive = true;
        row.seen = stamp;
        if gap > 0.0 {
            fold!(*row, stamp, gap, d_sent, d_recv);
        }

        let conn = state.conns.entry(inode.clone()).or_insert_with(|| Conn {
            pid,
            name: name.clone(),
            peer: seen.peer.clone(),
            port: seen.port,
            mine: seen.mine,
            ..Default::default()
        });
        conn.alive = true;
        conn.seen = stamp;
        if gap > 0.0 {
            fold!(*conn, stamp, gap, d_sent, d_recv);
        }

        let spot = state
            .spots
            .entry((pid, name.clone(), seen.peer.clone()))
            .or_insert_with(|| Spot {
                pid,
                name: name.clone(),
                peer: seen.peer.clone(),
                ..Default::default()
            });
        spot.alive = true;
        spot.seen = stamp;
        spot.ports.insert(seen.port);
        if gap > 0.0 {
            fold!(*spot, stamp, gap, d_sent, d_recv);
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
        trim(&mut state.series, SERIES);
        // Every process and every endpoint keeps its own series, so the
        // second screen can draw one of them alone on the same axes.
        for row in state.totals.values_mut() {
            row.hist.push((row.down_rate, row.up_rate));
            trim(&mut row.hist, SERIES);
            // /proc/<pid>/io is one small read beside the /proc/<pid>/fd
            // directory scan this widget already does per process, and it
            // is unreadable for anyone else's, which the empty map covers.
            let (mut read_rate, mut write_rate) = (0.0, 0.0);
            if row.pid != 0 && row.alive {
                let io = proc_io(row.pid);
                let now_read = *io.get("read_bytes").unwrap_or(&0);
                let now_write = *io.get("write_bytes").unwrap_or(&0);
                if !io.is_empty() {
                    if let Some((was_at, was_read, was_write)) = row.io_was {
                        let span = stamp - was_at;
                        if span > 0.0 {
                            read_rate = now_read.saturating_sub(was_read) as f64 / span;
                            write_rate = now_write.saturating_sub(was_write) as f64 / span;
                        }
                    }
                    row.io_was = Some((stamp, now_read, now_write));
                }
            }
            row.disk.push((read_rate, write_rate));
            trim(&mut row.disk, SERIES);
        }
        for conn in state.conns.values_mut() {
            conn.hist.push((conn.down_rate, conn.up_rate));
            trim(&mut conn.hist, SERIES);
        }
        for spot in state.spots.values_mut() {
            spot.hist.push((spot.down_rate, spot.up_rate));
            trim(&mut spot.hist, SERIES);
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

    // A closed connection is worth keeping - it may be the one that did the
    // damage - but not forever. The quiet dead ones go once there are
    // enough of them to matter.
    if state.conns.len() > 400 {
        let mut order: Vec<(String, f64, bool)> = state
            .conns
            .iter()
            .map(|(k, c)| (k.clone(), c.seen, c.alive))
            .collect();
        order.sort_by(|a, b| a.1.total_cmp(&b.1));
        for (inode, _, alive) in order.into_iter().take(100) {
            if !alive {
                state.conns.remove(&inode);
            }
        }
    }
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
    let dot = |x: i64, y: i64, grid: &mut Vec<Vec<u8>>| {
        if x >= 0 && (x as usize) < px_w && y >= 0 && (y as usize) < px_h {
            grid[y as usize / 4][x as usize / 2] |= tc::BRAILLE[y as usize % 4][x as usize % 2];
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

// ── the second screen ────────────────────────────────────────────────
//
// The table answers "what is using the network"; this answers "with what,
// and what is it writing". Everything here comes out of /proc for our own
// processes and out of nothing at all for anybody else's, which is why the
// screen says so rather than showing empty lists.

/// Reverse DNS, off the drawing thread.
///
/// A PTR lookup takes half a second when it works and longer when it does
/// not, which is several frames. The address is shown until a name arrives,
/// and an address that has no name is remembered as having none so it is
/// not asked about again every second.
struct Resolver {
    known: Arc<Mutex<HashMap<String, String>>>,
    wanted: Arc<Mutex<Vec<String>>>,
}

impl Resolver {
    fn new() -> Resolver {
        let known: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let wanted: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let (k, q) = (Arc::clone(&known), Arc::clone(&wanted));
        std::thread::spawn(move || loop {
            let next = q.lock().ok().and_then(|mut g| g.pop());
            let ip = match next {
                Some(ip) => ip,
                None => {
                    std::thread::sleep(Duration::from_millis(300));
                    continue;
                }
            };
            // getent rather than a resolver library: it is glibc's own
            // lookup, so it honours /etc/hosts, nsswitch and the search
            // domains exactly as everything else on the machine does.
            let answer = tc::run_quiet(&["getent", "hosts", &ip], RUN_TIMEOUT);
            let name = answer
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_string();
            if let Ok(mut guard) = k.lock() {
                guard.insert(ip, name);
            }
        });
        Resolver { known, wanted }
    }

    /// A name for an address if one is known, queueing a lookup if not.
    fn name(&self, ip: &str) -> String {
        if let Ok(guard) = self.known.lock() {
            if let Some(found) = guard.get(ip) {
                return found.clone();
            }
        }
        if let Ok(mut queue) = self.wanted.lock() {
            if !queue.iter().any(|q| q == ip) && queue.len() < 64 {
                queue.push(ip.to_string());
            }
        }
        String::new()
    }
}

/// What a port number is conventionally for, from /etc/services.
fn service(port: u16) -> String {
    if port == 0 {
        return String::new();
    }
    let text = match std::fs::read_to_string("/etc/services") {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    let want = format!("{}/tcp", port);
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        let mut cols = line.split_whitespace();
        let (name, entry) = (cols.next(), cols.next());
        if entry == Some(want.as_str()) {
            return name.unwrap_or("").to_string();
        }
    }
    String::new()
}

/// Whether the process still exists.
///
/// Distinct from the `alive` flag on a row, which means "had a socket in
/// the last sample". A long-running server sitting idle has neither traffic
/// nor open connections and has certainly not exited, and saying it had
/// would be worse than saying nothing.
fn running(pid: i32) -> bool {
    pid > 0 && std::path::Path::new(&format!("/proc/{}", pid)).is_dir()
}

/// Disk bytes this process has read and written, from /proc/<pid>/io.
fn proc_io(pid: i32) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(format!("/proc/{}/io", pid)) {
        for line in text.lines() {
            if let Some((key, value)) = line.split_once(':') {
                if let Ok(n) = value.trim().parse() {
                    out.insert(key.trim().to_string(), n);
                }
            }
        }
    }
    out
}

struct OpenFile {
    path: String,
    size: u64,
}

/// Regular files this process has open, largest first.
///
/// A download has to land somewhere, and where it lands is a file getting
/// bigger. This is the closest thing to "which file" that exists outside
/// the encrypted stream - the name of the thing being written, rather than
/// the name of the thing being fetched.
fn open_files(pid: i32) -> Vec<OpenFile> {
    let mut found = Vec::new();
    let dir = match std::fs::read_dir(format!("/proc/{}/fd", pid)) {
        Ok(d) => d,
        Err(_) => return found,
    };
    for entry in dir.flatten() {
        let link = match std::fs::read_link(entry.path()) {
            Ok(l) => l,
            Err(_) => continue,
        };
        let path = link.to_string_lossy().to_string();
        if !path.starts_with('/')
            || path.starts_with("/dev/")
            || path.starts_with("/proc/")
            || path.starts_with("/sys/")
        {
            continue;
        }
        // Through the fd rather than the path: the target may have been
        // unlinked, and a temporary file being written is exactly the case
        // this screen exists for.
        let size = match std::fs::metadata(entry.path()) {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        found.push(OpenFile { path, size });
    }
    found.sort_by(|a, b| b.size.cmp(&a.size));
    found
}

#[derive(Default)]
struct Facts {
    cmdline: String,
    cwd: String,
}

/// Command and directory - what the table has no room for.
fn process_facts(pid: i32) -> Facts {
    let mut facts = Facts::default();
    if let Ok(raw) = std::fs::read(format!("/proc/{}/cmdline", pid)) {
        facts.cmdline = String::from_utf8_lossy(&raw)
            .replace('\0', " ")
            .trim()
            .to_string();
    }
    if let Ok(link) = std::fs::read_link(format!("/proc/{}/cwd", pid)) {
        facts.cwd = link.to_string_lossy().to_string();
    }
    facts
}

/// A path that fits, keeping the end - which is the filename.
fn short(path: &str, room: usize) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = if !home.is_empty() && path.starts_with(&home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    };
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= room || room < 2 {
        return path;
    }
    format!("…{}", chars[chars.len() - (room - 1)..].iter().collect::<String>())
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest: Vec<char> = text.chars().collect();
    while !rest.is_empty() && lines.len() < 3 {
        if rest.len() <= width {
            lines.push(rest.iter().collect());
            break;
        }
        let cut = rest[..(width + 1).min(rest.len())]
            .iter()
            .rposition(|c| *c == ' ')
            .filter(|c| *c > width / 2)
            .unwrap_or(width);
        lines.push(rest[..cut].iter().collect());
        rest = rest[cut..].iter().skip_while(|c| **c == ' ').copied().collect();
    }
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn field_rows(label: &str, value: &str, w: usize, colour: &str, p: &Palette) -> Vec<String> {
    wrap(value, ((w - 3).saturating_sub(10)).max(8))
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            tc::seg(
                &[
                    (
                        p.dim.as_str(),
                        format!("  {}", tc::pad(if i == 0 { label } else { "" }, 10)),
                    ),
                    (colour, line),
                ],
                w - 1,
            )
        })
        .collect()
}

/// A section header that says whether it is the one taking the keys.
fn section_head(
    name: &str,
    count: usize,
    note: &str,
    focused: bool,
    next: bool,
    w: usize,
    p: &Palette,
) -> String {
    tc::seg(
        &[
            (
                if focused { p.accent.as_str() } else { p.lbl.as_str() },
                format!("{}── {} ── ", if focused { " ▏" } else { " " }, name),
            ),
            (
                p.dim.as_str(),
                format!("{} {}{}", count, note, if count == 1 { "" } else { "s" }),
            ),
            // One key, on the one heading it is true of. This used to name
            // a letter per section - [e] and [f] pointing at keys that no
            // longer existed, and the middle one pointing at tab, which was
            // the only one that was ever true. So tab is what is left, and
            // it is shown only on the section tab would actually focus
            // next: it cycles, and a [tab] on all three would promise three
            // sections that one press reaches.
            (
                p.accent.as_str(),
                if next { "   [tab] to focus".to_string() } else { String::new() },
            ),
        ],
        w - 1,
    )
}

/// The line above a chart: what it is, and how far back it reaches.
fn chart_head(len: usize, w: usize, label: &str, interval: f64, p: &Palette) -> String {
    let span = if len > 0 {
        elapsed(len as f64 * interval)
    } else {
        "nothing yet".to_string()
    };
    tc::seg(
        &[
            (p.lbl.as_str(), format!(" ── {} ── ", label)),
            (p.up.as_str(), "↑ tx above".into()),
            (p.dim.as_str(), " · ".into()),
            (p.down.as_str(), "↓ rx below".into()),
            (p.dim.as_str(), format!("  · {} of history", span)),
        ],
        w - 1,
    )
}

/// The three lists each own a flexible first column and a fixed tail. The
/// width lives in a function so the heading and the rows read it from the
/// same place: a heading that has slipped a column is worse than no heading,
/// because it labels the wrong number with confidence.
fn endpoint_host_w(w: usize) -> usize {
    // 44, not 42: the fixed tail is 9 for ports, 10 for rx, 11 for tx and 11
    // for the rate, plus 3 for the selection mark. It was written as 42, so
    // between 59 and 77 columns the host name took two cells the rate needed
    // and seg() clipped "12.4 KB/s" down to "12.4 KB". A rate short of its
    // unit is not a smaller rate, it is a wrong one.
    ((w - 1).saturating_sub(44)).clamp(14, 34)
}

fn connection_host_w(w: usize) -> usize {
    // 37 is the fixed tail exactly: 3 for the mark, 6 for our port, 7 for the
    // state, 10 for rx and 11 for tx. Counted, not estimated - the endpoint
    // list had this written as 42 when it was 44 and quietly clipped the rate.
    ((w - 1).saturating_sub(37)).clamp(14, 38)
}

fn file_path_w(w: usize) -> usize {
    ((w - 1).saturating_sub(30)).max(18)
}

/// The column names above `endpoint_rows`. The leading three cells are the
/// selection mark's column, left blank here.
fn endpoint_head(w: usize, p: &Palette) -> String {
    tc::seg(
        &[(
            p.dim.as_str(),
            format!(
                "   {}{:<9}{:>10}{:>11}{:>11}",
                tc::pad("HOST", endpoint_host_w(w)),
                "PORTS",
                "RX",
                "TX",
                "RATE"
            ),
        )],
        w - 1,
    )
}

/// The column names above `connection_rows`.
fn connection_head(w: usize, p: &Palette) -> String {
    tc::seg(
        &[(
            p.dim.as_str(),
            format!(
                "   {}{:<6}{:<7}{:>10}{:>11}",
                tc::pad("SOCKET", connection_host_w(w)),
                "LOCAL",
                "STATE",
                "RX",
                "TX"
            ),
        )],
        w - 1,
    )
}

/// The column names above `file_rows`.
fn file_head(w: usize, p: &Palette) -> String {
    tc::seg(
        &[(
            p.dim.as_str(),
            format!(
                "   {}{:>10}{:>12}",
                tc::pad("PATH", file_path_w(w)),
                "SIZE",
                "GROWTH"
            ),
        )],
        w - 1,
    )
}

/// Remote hosts, ranked by what they have carried since launch.
fn endpoint_rows(
    spots: &[Spot],
    at: usize,
    focused: bool,
    room: usize,
    w: usize,
    names: &Resolver,
    p: &Palette,
) -> Vec<String> {
    let host_w = endpoint_host_w(w);
    spots
        .iter()
        .take(room.max(1))
        .enumerate()
        .map(|(i, spot)| {
            let here = focused && i == at;
            let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
            let name = {
                let found = names.name(&spot.peer);
                if found.is_empty() { spot.peer.clone() } else { found }
            };
            let ports: Vec<String> = spot
                .ports
                .iter()
                .take(2)
                .map(|port| {
                    let named = service(*port);
                    if named.is_empty() { port.to_string() } else { named }
                })
                .collect();
            let moving = spot.down_rate + spot.up_rate;
            let c = |colour: &str| format!("{}{}", tint, colour);
            tc::seg(
                &[
                    (
                        &c(if here { &p.accent } else { &p.dim }),
                        if here { " ▸ ".into() } else { "   ".into() },
                    ),
                    (
                        &c(if spot.alive { &p.txt } else { &p.dim }),
                        tc::pad(&name.chars().take(host_w - 1).collect::<String>(), host_w),
                    ),
                    (
                        &c(&p.dim),
                        format!("{:<9}", ports.join("/").chars().take(9).collect::<String>()),
                    ),
                    (&c(&p.down), format!("↓{:>9}", units(spot.down as f64))),
                    (&c(&p.up), format!(" ↑{:>9}", units(spot.up as f64))),
                    (
                        &c(if moving > 0.0 { &p.ok } else { &p.dim }),
                        format!("{:>11}", rate(moving)),
                    ),
                    (&tint, if here { " ".repeat(w) } else { String::new() }),
                ],
                w - 1,
            )
        })
        .collect()
}

/// The sockets open right now, which is a different list from the hosts.
fn connection_rows(
    conns: &[Conn],
    at: usize,
    focused: bool,
    room: usize,
    w: usize,
    p: &Palette,
) -> Vec<String> {
    let host_w = connection_host_w(w);
    conns
        .iter()
        .take(room.max(1))
        .enumerate()
        .map(|(i, conn)| {
            let here = focused && i == at;
            let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
            let where_ = format!("{}:{}", conn.peer, conn.port);
            let c = |colour: &str| format!("{}{}", tint, colour);
            tc::seg(
                &[
                    (
                        &c(if here { &p.accent } else { &p.dim }),
                        if here { " ▸ ".into() } else { "   ".into() },
                    ),
                    (
                        &c(if conn.alive { &p.txt } else { &p.dim }),
                        tc::pad(
                            &where_.chars().take(host_w - 1).collect::<String>(),
                            host_w,
                        ),
                    ),
                    // The local port, which ss and netstat both call Local.
                    // Without it five rows to one CDN are five identical
                    // lines, and "why are there so many of these" has no
                    // answer on screen.
                    (
                        &c(&p.dim),
                        format!(
                            "{:<6}",
                            if conn.mine > 0 { conn.mine.to_string() } else { "-".into() }
                        ),
                    ),
                    (
                        &c(if conn.alive { &p.ok } else { &p.dim }),
                        format!("{:<7}", if conn.alive { "open" } else { "closed" }),
                    ),
                    (&c(&p.down), format!("↓{:>9}", units(conn.down as f64))),
                    (&c(&p.up), format!(" ↑{:>9}", units(conn.up as f64))),
                    (&tint, if here { " ".repeat(w) } else { String::new() }),
                ],
                w - 1,
            )
        })
        .collect()
}

/// Open files, with how fast each is growing since this screen opened.
fn file_rows(
    files: &[OpenFile],
    sizes: &HashMap<String, (u64, f64)>,
    at: usize,
    focused: bool,
    room: usize,
    w: usize,
    p: &Palette,
) -> Vec<String> {
    let path_w = file_path_w(w);
    files
        .iter()
        .take(room.max(1))
        .enumerate()
        .map(|(i, item)| {
            let here = focused && i == at;
            let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
            let (grew, span) = match sizes.get(&item.path) {
                Some((was, when)) => (item.size.saturating_sub(*was), tc::now() - when),
                None => (0, 0.0),
            };
            let growth = if grew > 0 && span > 0.0 {
                format!("+{}", rate(grew as f64 / span))
            } else {
                String::new()
            };
            let c = |colour: &str| format!("{}{}", tint, colour);
            tc::seg(
                &[
                    (
                        &c(if here { &p.accent } else { &p.dim }),
                        if here { " ▸ ".into() } else { "   ".into() },
                    ),
                    (&c(&p.txt), tc::pad(&short(&item.path, path_w), path_w)),
                    (&c(&p.dim), format!("{:>10}", units(item.size as f64))),
                    (
                        &c(if grew > 0 { &p.ok } else { &p.dim }),
                        format!("{:>12}", growth),
                    ),
                    (&tint, if here { " ".repeat(w) } else { String::new() }),
                ],
                w - 1,
            )
        })
        .collect()
}

/// One process in full: what it is, who it talks to, what it writes.
#[allow(clippy::too_many_arguments)]
fn detail_rows(
    row: &Proc,
    spots: &[Spot],
    conns: &[Conn],
    files: &[OpenFile],
    sizes: &HashMap<String, (u64, f64)>,
    focus: Option<usize>,
    at: &[usize; 3],
    w: usize,
    h: usize,
    interval: f64,
    names: &Resolver,
    p: &Palette,
) -> (Vec<String>, Option<(usize, usize)>) {
    // Where the cursor ended up in the body, and how many rows it owns - its
    // own row plus the chart drawn under it. The caller cannot work this out:
    // the body is built at whatever height it needs and the selected row's
    // position depends on how many rows every section above it drew.
    let mut cursor: Option<(usize, usize)> = None;
    let facts = process_facts(row.pid);
    let here_now = running(row.pid);
    let total = (row.up + row.down) as f64;
    let mut out = vec![tc::title(
        &format!("{} · pid {}", row.name, row.pid),
        w,
        &p.accent,
    )];
    out.push(tc::seg(
        &[
            (p.txt.as_str(), format!(" {}", units(total))),
            (p.dim.as_str(), " since first seen  ·  ".into()),
            (p.down.as_str(), format!("↓ {}", rate(row.down_rate))),
            (p.dim.as_str(), "  ".into()),
            (p.up.as_str(), format!("↑ {}", rate(row.up_rate))),
        ],
        w - 1,
    ));
    if row.pid == 0 {
        out.push(tc::seg(
            &[(
                p.dim.as_str(),
                " another user's process - named from its control group, \
                 since /proc is closed to us"
                    .into(),
            )],
            w - 1,
        ));
    } else if !here_now {
        out.push(tc::seg(
            &[(
                p.warn.as_str(),
                " this process has exited - its total is kept, and nothing \
                 below is live"
                    .into(),
            )],
            w - 1,
        ));
    } else if !row.alive {
        out.push(tc::seg(
            &[(
                p.dim.as_str(),
                " no connection open at the moment - what is below is the \
                 last that was seen"
                    .into(),
            )],
            w - 1,
        ));
    }
    out.push(String::new());

    // This process's own traffic, on the same chart as the machine's.
    let spare = h.saturating_sub(out.len());
    let graph_h = if spare >= 30 {
        7
    } else if spare >= 24 {
        5
    } else {
        0
    };
    if graph_h > 0 && !row.hist.is_empty() {
        out.push(chart_head(row.hist.len(), w, "THIS PROCESS", interval, p));
        out.extend(chart(&row.hist, w, graph_h, p));
        out.push(String::new());
    }

    if h.saturating_sub(out.len()) >= 12 {
        out.push(tc::seg(&[(p.lbl.as_str(), " ── PROCESS ── ".into())], w - 1));
        let cmd = if facts.cmdline.is_empty() { "?" } else { &facts.cmdline };
        out.extend(field_rows("command", cmd, w, &p.txt, p));
        if !facts.cwd.is_empty() {
            let cwd = short(&facts.cwd, w.saturating_sub(14));
            out.extend(field_rows("directory", &cwd, w, &p.txt, p));
        }
        out.push(String::new());
    }

    // Every list is drawn in full. The three used to share what was left,
    // with the focused one taking the room and the other two collapsing to
    // a single line - so a process with six sockets showed one of them and
    // said "6 sockets" above it, and the only way to see the rest was to
    // remember which key focused that section. The screen is now as tall as
    // it needs to be and the caller scrolls it.
    let counts = [spots.len(), conns.len(), files.len()];
    let shares = [counts[0].max(1), counts[1].max(1), counts[2].max(1)];

    for (which, (name, note)) in [
        ("TALKING TO", "endpoint"),
        ("CONNECTIONS", "socket"),
        ("FILES", "file"),
    ]
    .into_iter()
    .enumerate()
    {
        let focused = focus == Some(which);
        let next = tc::next_section(focus, &counts) == Some(which);
        out.push(section_head(name, counts[which], note, focused, next, w, p));
        let room = shares[which];
        if counts[which] == 0 {
            out.push(tc::seg(&[(p.dim.as_str(), "   none".into())], w - 1));
        } else if which == 0 {
            out.push(endpoint_head(w, p));
            let mut rows = endpoint_rows(spots, at[which], focused, room, w, names, p);
            // The chart belongs under the line it describes, not after the
            // list: with it at the bottom you had to hold which host was
            // highlighted in your head while your eye travelled. Here the
            // answer is where the question is.
            let pick_at = at[which].min(spots.len() - 1);
            if focused {
                let pick = &spots[pick_at];
                let mut tall = 1;
                if !pick.hist.is_empty() && pick_at < rows.len() {
                    let mut under = chart(&pick.hist, w, 4, p);
                    under.push(String::new());
                    tall += under.len();
                    rows.splice(pick_at + 1..pick_at + 1, under);
                }
                cursor = Some((out.len() + pick_at, tall));
            }
            out.extend(rows);
        } else if which == 1 {
            out.push(connection_head(w, p));
            let mut rows = connection_rows(conns, at[which], focused, room, w, p);
            let pick_at = at[which].min(conns.len().saturating_sub(1));
            if focused {
                let mut tall = 1;
                if let Some(pick) = conns.get(pick_at) {
                    if !pick.hist.is_empty() && pick_at < rows.len() {
                        let mut under = chart(&pick.hist, w, 4, p);
                        under.push(String::new());
                        tall += under.len();
                        rows.splice(pick_at + 1..pick_at + 1, under);
                    }
                }
                cursor = Some((out.len() + pick_at, tall));
            }
            out.extend(rows);
        } else {
            out.push(file_head(w, p));
            if focused {
                cursor = Some((out.len() + at[which].min(files.len() - 1), 1));
            }
            out.extend(file_rows(files, sizes, at[which], focused, room, w, p));
        }
        out.push(String::new());
    }

    let io = if here_now {
        proc_io(row.pid)
    } else {
        HashMap::new()
    };
    if !io.is_empty() && h.saturating_sub(out.len()) >= 2 {
        out.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── DISK ── ".into()),
                (
                    p.dim.as_str(),
                    format!(
                        "read {} · written {} since it started",
                        units(*io.get("read_bytes").unwrap_or(&0) as f64),
                        units(*io.get("write_bytes").unwrap_or(&0) as f64)
                    ),
                ),
            ],
            w - 1,
        ));
        // The same chart the network half uses, on the same axes rule:
        // written above the line, read below. Two lifetime totals say
        // whether it has ever touched the disk; this says whether it is
        // touching it now, which is the question the rest of this screen
        // is answering about the network.
        let room = h.saturating_sub(out.len() + 1);
        let disk_h = if room >= 7 { room.min(9) } else { 0 };
        // Drawn whether or not it is moving, like the network chart above:
        // a flat line at zero says "not touching the disk", which is an
        // answer. Hiding it would leave the reader unsure whether the
        // process is quiet or the chart is broken.
        if disk_h > 0 && !row.disk.is_empty() {
            out.extend(chart(&row.disk, w, disk_h, p));
        }
    }
    (out, cursor)
}

/// One block per interval, for a log or a pipe.
fn plain_line(rows: &[Proc], started: f64, live: bool, limit: usize) -> String {
    let mut lines = vec![format!(
        "--- {} elapsed · sorted by {} ---",
        elapsed(tc::now() - started),
        if live { "live" } else { "total" }
    )];
    for row in rows.iter().take(limit) {
        lines.push(format!(
            "{:<22} {:<8} {:>11} {:>11} {:>11} {:>11}",
            row.name,
            if row.pid > 0 {
                row.pid.to_string()
            } else {
                "-".to_string()
            },
            units((row.up + row.down) as f64),
            rate(row.up_rate + row.down_rate),
            rate(row.down_rate),
            rate(row.up_rate)
        ));
    }
    lines.join("\n")
}

/// The table's order, which is also the order Enter indexes into.
fn ordered(state: &Arc<Mutex<State>>, mine: bool, live: bool) -> Vec<Proc> {
    let mut rows: Vec<Proc> = match state.lock() {
        Ok(guard) => guard
            .totals
            .values()
            .filter(|r| !mine || r.pid != 0)
            .cloned()
            .collect(),
        Err(_) => return Vec::new(),
    };
    if live {
        rows.sort_by(|a, b| {
            (b.up_rate + b.down_rate)
                .total_cmp(&(a.up_rate + a.down_rate))
                .then((b.up + b.down).cmp(&(a.up + a.down)))
        });
    } else {
        rows.sort_by(|a, b| {
            (b.up + b.down)
                .cmp(&(a.up + a.down))
                .then((b.up_rate + b.down_rate).total_cmp(&(a.up_rate + a.down_rate)))
        });
    }
    rows
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    // Config first, argv second: `--interval 2` beats a config saying 1,
    // which is the precedence netwatch.py uses. These five were documented
    // in config.example.json and read by nobody.
    let cfg = tc::load_config("netwatch");
    let mut interval = tc::poll_secs(tc::cfg_f64(&cfg, "interval", 1.0), 1.0).max(0.2);
    let mut limit = tc::cfg_usize(&cfg, "limit", 0);
    let mut external = cfg
        .get("external")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let mut mine = cfg.get("mine").and_then(|v| v.as_bool()).unwrap_or(true);
    let mut sort_live = tc::cfg_str(&cfg, "sort", "total") == "live";
    let mut plain = false;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-i" | "--interval" if i + 1 < args.len() => {
                interval = tc::poll_secs(args[i + 1].parse().unwrap_or(1.0), 1.0).max(0.2);
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
            "--external" => {
                external = true;
                i += 1;
            }
            "--all-external" => {
                external = false;
                i += 1;
            }
            "--all-users" => {
                mine = false;
                i += 1;
            }
            "--plain" => {
                plain = true;
                i += 1;
            }
            // Refused rather than ignored: a typo that silently does
            // nothing is worse than one that says so, and the Python has
            // always said so.
            other => {
                eprintln!("unknown option {:?} - try --help", other);
                std::process::exit(2);
            }
        }
    }

    let p = palette();
    let state = Arc::new(Mutex::new(State {
        started: tc::now(),
        ..Default::default()
    }));
    let poller = Arc::clone(&state);
    std::thread::spawn(move || loop {
        // A poller that dies takes its explanation with it, and an empty
        // table looks exactly like a machine with no sockets on it.
        {
            let mut guard = match poller.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                sample(&mut guard, external);
            }))
            .is_err()
            {
                guard.err = "poller stopped - see the pane it was started from".into();
                return;
            }
        }
        std::thread::sleep(Duration::from_secs_f64(interval));
    });

    // One block per interval, for a log or a pipe. Nothing here touches
    // the screen, so it can be redirected to a file and left running.
    if plain {
        loop {
            std::thread::sleep(Duration::from_secs_f64(interval));
            let (started, err) = {
                let guard = match state.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                (guard.started, guard.err.clone())
            };
            if !err.is_empty() {
                eprintln!("{}", err);
            }
            let rows = ordered(&state, mine, sort_live);
            let show = if limit > 0 { limit } else { rows.len() };
            println!("{}", plain_line(&rows, started, sort_live, show));
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut selected = 0usize;
    // Where the process table is scrolled to, and whether a key has just
    // moved the cursor. The wheel writes the first and never the second, so
    // the table stops re-centring on the selection the moment it is turned.
    let (mut scroll, mut moved) = (0usize, false);
    // The second screen: which process, which of its three lists is taking
    // the keys, and where each list is scrolled to.
    let mut detail: Option<(i32, String)> = None;
    // Which section has the cursor, if any. Opens with none: the screen is
    // a thing to read before it is a thing to work, and a cursor sitting
    // somewhere you did not put it is a question rather than an answer.
    let mut focus: Option<usize> = None;
    let mut at = [0usize; 3];
    // How far down the detail screen we are. Clamped against the body when
    // the frame is drawn, because the body's height depends on how many
    // sockets and files the process has right now.
    let mut dscroll = 0usize;
    // How long each section was when it was last drawn. The keys are read
    // before the frame is built, so walking off the end of a list has to be
    // judged against the length it had a moment ago - which is the same
    // length the reader is looking at.
    let mut section_len = [0usize; 3];
    // What each open file measured when this screen opened, so the growth
    // column is over the time you have been looking rather than the life
    // of the file.
    let mut sizes: HashMap<String, (u64, f64)> = HashMap::new();
    let mut notice: Option<(String, String, f64)> = None;
    // What `c` would copy: known while drawing, wanted when the key is hit.
    let mut pending_copy = String::new();
    let names = Resolver::new();

    loop {
        for key in keyboard.poll() {
            if detail.is_some() {
                match key.as_str() {
                    // Left and esc come out; q quits, which is what the
                    // footer beside it says and what q does everywhere
                    // else. backspace is gone: an alias no hint named.
                    "esc" | "left" => {
                        detail = None;
                        sizes.clear();
                    }
                    "q" | "Q" => {
                        keyboard.restore();
                        tc::restore_screen();
                        return;
                    }
                    "r" | "R" => {
                        if let Ok(mut guard) = state.lock() {
                            guard.totals.clear();
                            guard.conns.clear();
                            guard.spots.clear();
                            guard.series.clear();
                            guard.started = tc::now();
                        }
                        detail = None;
                        sizes.clear();
                    }
                    // Focused, up and down walk the three lists as though
                    // they were one: off the bottom of a section is the top
                    // of the next, off the top is the bottom of the one
                    // above. Only the two far ends let go. Unfocused, they
                    // move the screen. Whichever is in front of you is what
                    // they act on, which is the rule the list screen follows.
                    // The view, whatever has the focus - the same arm linear
                    // carries, for the same reason. The arrows below already
                    // do this when nothing is focused; once a section is
                    // picked they belong to the cursor, and scrolling to
                    // look at something must not move what enter opens.
                    "ctrl-y" | "ctrl-e" | "wheel-up" | "wheel-down" => {
                        dscroll = if key == "ctrl-e" || key == "wheel-down" {
                            dscroll.saturating_add(1)
                        } else {
                            dscroll.saturating_sub(1)
                        };
                    }
                    "up" | "k" | "K" => match focus {
                        Some(here) => {
                            focus = tc::step_across_sections(here, at[here], &section_len, false)
                                .map(|(sect, row)| {
                                    at[sect] = row;
                                    sect
                                });
                            moved = true;
                        }
                        None => dscroll = dscroll.saturating_sub(1),
                    },
                    "down" | "j" | "J" => match focus {
                        Some(here) => {
                            focus = tc::step_across_sections(here, at[here], &section_len, true)
                                .map(|(sect, row)| {
                                    at[sect] = row;
                                    sect
                                });
                            moved = true;
                        }
                        None => dscroll = dscroll.saturating_add(1),
                    },
                    // The pane height is read here rather than carried,
                    // because a page is only meaningful against the pane as
                    // it is now and it may have been resized since.
                    "pgup" => {
                        let page = tc::size().1.saturating_sub(4).max(1);
                        dscroll = dscroll.saturating_sub(page);
                    }
                    "pgdn" => {
                        let page = tc::size().1.saturating_sub(4).max(1);
                        dscroll = dscroll.saturating_add(page);
                    }
                    "home" => dscroll = 0,
                    "end" => dscroll = usize::MAX,
                    // tab is the only way between sections. e and f jumped
                    // straight to two of the three, which meant three keys
                    // for one job and a section heading advertising a letter
                    // of its own - and no key at all for the middle one.
                    // Where it goes, and which sections it steps over, is
                    // opscope-core's rule rather than this widget's.
                    "tab" => {
                        focus = tc::next_section(focus, &section_len);
                        moved = true;
                    }
                    "c" | "C" => {
                        if !pending_copy.is_empty() {
                            // The value goes in the message either way: OSC
                            // 52 is refused by some terminals and swallowed
                            // by some multiplexers, and a copy that quietly
                            // did nothing would leave nothing to read.
                            let copied = tc::clipboard(&pending_copy);
                            notice = Some((
                                format!(
                                    "{}{}",
                                    if copied { "copied  " } else { "no clipboard  " },
                                    pending_copy
                                ),
                                if copied { p.ok.clone() } else { p.warn.clone() },
                                tc::now() + 8.0,
                            ));
                        }
                    }
                    _ => {}
                }
                continue;
            }
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "," => {
                    tc::run_settings(&mut keyboard, SETTINGS);
                    continue;
                }
                "1" => sort_live = false,
                "2" => sort_live = true,
                "s" | "S" | "t" | "T" => sort_live = !sort_live,
                "o" | "O" => {
                    mine = !mine;
                    selected = 0;
                    moved = true;
                }
                "up" | "k" | "K" => {
                    selected = selected.saturating_sub(1);
                    moved = true;
                }
                "down" | "j" | "J" => {
                    selected += 1;
                    moved = true;
                }
                // The wheel moves the table and leaves the cursor where it
                // is - selection is the arrows' job, here as everywhere.
                "ctrl-y" | "wheel-up" => scroll = scroll.saturating_sub(1),
                "ctrl-e" | "wheel-down" => scroll = scroll.saturating_add(1),
                "enter" | "right" => {
                    if let Some(pick) = ordered(&state, mine, sort_live).get(selected) {
                        detail = Some((pick.pid, pick.name.clone()));
                        focus = None;
                        at = [0; 3];
                        dscroll = 0;
                        sizes.clear();
                    }
                }
                "r" | "R" => {
                    if let Ok(mut guard) = state.lock() {
                        guard.totals.clear();
                        guard.conns.clear();
                        guard.spots.clear();
                        guard.series.clear();
                        guard.started = tc::now();
                    }
                    selected = 0;
                    moved = true;
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        if notice.as_ref().is_some_and(|n| tc::now() >= n.2) {
            notice = None;
        }

        // One process in full. Its row is looked up fresh each frame so the
        // figures keep moving while the screen is open, and it survives the
        // process exiting - which is often when it is being looked at.
        if let Some((pid, name)) = detail.clone() {
            let (row, spots, conns) = {
                let guard = match state.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                let row = guard.totals.get(&(pid, name.clone())).cloned();
                let mut spots: Vec<Spot> = guard
                    .spots
                    .values()
                    .filter(|s| s.pid == pid && s.name == name)
                    .cloned()
                    .collect();
                spots.sort_by(|a, b| {
                    (b.up + b.down)
                        .cmp(&(a.up + a.down))
                        .then(a.peer.cmp(&b.peer))
                });
                let mut conns: Vec<Conn> = guard
                    .conns
                    .values()
                    .filter(|c| c.pid == pid && c.name == name)
                    .cloned()
                    .collect();
                conns.sort_by(|a, b| {
                    (b.up + b.down)
                        .cmp(&(a.up + a.down))
                        .then(a.peer.cmp(&b.peer))
                });
                (row, spots, conns)
            };
            let row = match row {
                Some(r) => r,
                None => {
                    detail = None;
                    continue;
                }
            };
            let files = if running(pid) { open_files(pid) } else { Vec::new() };
            for file in &files {
                sizes.entry(file.path.clone()).or_insert((file.size, tc::now()));
            }
            let counts = [spots.len(), conns.len(), files.len()];
            section_len = counts;
            if let Some(at_focus) = focus {
                at[at_focus] = at[at_focus].min(counts[at_focus].saturating_sub(1));
            }
            // The selection is known here and the key is pressed elsewhere,
            // so what `c` would copy is recorded while the frame is drawn.
            // Nothing focused means nothing selected, so c has nothing to
            // take. It says so rather than copying whatever happened to be
            // first.
            pending_copy = match focus {
                Some(0) => spots.get(at[0]).map(|s| s.peer.clone()).unwrap_or_default(),
                Some(1) => conns
                    .get(at[1])
                    .map(|c| format!("{}:{}", c.peer, c.port))
                    .unwrap_or_default(),
                Some(_) => files.get(at[2]).map(|f| f.path.clone()).unwrap_or_default(),
                None => String::new(),
            };
            let hints: Vec<Vec<(&str, String)>> = vec![
                vec![
                    (p.accent.as_str(), "↑↓".into()),
                    (
                        p.dim.as_str(),
                        if focus.is_some() { " select" } else { " scroll" }.to_string(),
                    ),
                ],
                vec![
                    (p.accent.as_str(), "tab".into()),
                    (
                        p.dim.as_str(),
                        if focus.is_some() { " next section" } else { " into a section" }.to_string(),
                    ),
                ],
                vec![(p.dim.as_str(), "[c]opy".into())],
                vec![(p.dim.as_str(), "[r]ezero".into())],
                vec![
                    (p.accent.as_str(), "←".into()),
                    (p.dim.as_str(), "/esc back".into()),
                ],
                vec![(p.dim.as_str(), "[q]uit".into())],
            ];
            let mut foot: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
                .into_iter()
                .map(|l| format!(" {}", l))
                .collect();
            if let Some((text, colour, _)) = notice.as_ref() {
                foot = vec![tc::seg(&[(colour.as_str(), format!(" {}", text))], w - 1)];
            }
            let room = h.saturating_sub(foot.len() + 1).max(1);
            // Built at the height it wants rather than the height it has:
            // the lists are drawn in full and the charts get their rows, and
            // what does not fit is scrolled to rather than dropped.
            let natural = room + spots.len() + conns.len() + files.len() + 24;
            let (body, cursor) = detail_rows(
                &row, &spots, &conns, &files, &sizes, focus, &at, w, natural, interval, &names, &p,
            );
            // The title is pinned and everything below it is the window.
            // `cursor` addresses the whole body, so its row shifts by the
            // header before the follow below reads it - miss that and the
            // span it reveals is a row out, but only once scrolled.
            let (head, rest) = body.split_at(1.min(body.len()));
            let room = room.saturating_sub(head.len()).max(1);
            let cursor = cursor.map(|(at, tall)| (at.saturating_sub(head.len()), tall));
            let furthest = rest.len().saturating_sub(room);
            // Only on the frame a key moved the cursor. Chasing it every
            // frame pulls the page back to the selection the instant the
            // wheel moves it, which reads as the wheel doing nothing at
            // all. The chart is why this reveals a span rather than a
            // row: pulling the selected line just into view would leave
            // the four rows it was drawn for still below the edge.
            if moved {
                if let Some((at_row, tall)) = cursor {
                    if at_row < dscroll {
                        dscroll = at_row;
                    } else if at_row + tall > dscroll + room {
                        dscroll = (at_row + tall).saturating_sub(room);
                    }
                }
                moved = false;
            }
            dscroll = dscroll.min(furthest);
            let last = (dscroll + room).min(rest.len());
            let mut shown: Vec<String> = head.to_vec();
            shown.extend_from_slice(&rest[dscroll..last]);
            while shown.len() < room {
                shown.push(String::new());
            }
            // Appending this to an already-packed line overflowed the width
            // and the terminal wrapped it, so the footer's last row was the
            // tail of a number. It is a hint like the others now, packed with
            // them.
            //
            // `room` above reserved one line for it, and one is enough:
            // pack_hints is greedy, so the hints before this one pack the
            // same way whether it is there or not, and it either joins the
            // last line or starts one more. Never two more - which matters,
            // because `last` was measured against `room` and a second extra
            // line would cost a body row the label had already counted.
            if furthest > 0 {
                let mut with_pos = hints.clone();
                with_pos.push(vec![(
                    p.dim.as_str(),
                    scroll_label(dscroll + 1, last, rest.len()),
                )]);
                foot = tc::pack_hints(&with_pos, w - 2, "  ")
                    .into_iter()
                    .map(|l| format!(" {}", l))
                    .collect();
                if let Some((text, colour, _)) = notice.as_ref() {
                    foot = vec![tc::seg(&[(colour.as_str(), format!(" {}", text))], w - 1)];
                }
                while shown.len() + foot.len() < h {
                    shown.push(String::new());
                }
            }
            shown.extend(foot);
            tc::draw(&shown, w, h);
            std::thread::sleep(Duration::from_millis(300));
            continue;
        }

        let rows = ordered(&state, mine, sort_live);
        let guard = match state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if !rows.is_empty() && selected >= rows.len() {
            selected = rows.len() - 1;
        }
        let moving = rows.iter().filter(|r| r.up_rate + r.down_rate > 0.0).count();
        let down: f64 = rows.iter().map(|r| r.down_rate).sum();
        let up: f64 = rows.iter().map(|r| r.up_rate).sum();

        let mut out = vec![tc::title("netwatch", w, &p.accent)];
        // Held open. What this line has to say depends on how many rows fit,
        // which is not known until the chart above the table has been built -
        // but the line is one row tall whatever it ends up saying, so nothing
        // below it moves.
        let count_at = out.len();
        out.push(String::new());
        out.push(tc::seg(
            &[
                (p.dim.as_str(), format!(" {} moving", moving)),
                (p.dim.as_str(), " · ".into()),
                (p.accent.as_str(), elapsed(tc::now() - guard.started)),
                (p.dim.as_str(), " · sorted by ".into()),
                (
                    p.accent.as_str(),
                    if sort_live { "live".into() } else { "total".into() },
                ),
                (
                    p.dim.as_str(),
                    format!("   every {}s · rates over {}s", interval, RATE_WINDOW as i64),
                ),
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
        // The window follows the cursor, as it does in the detail screen and
        // in github's account list. It did not before: the table always drew
        // rows 0..show while the cursor was free to walk to the end of the
        // list, so on any pane too short for every process the cursor left
        // the screen and there was nothing to say where it had gone - and
        // enter still opened whatever it was sitting on, unseen.
        // Only on the frame a key moved the cursor. Re-centring every frame
        // pulls the table back to the selection the instant the wheel moves
        // it, which reads as the wheel doing nothing at all.
        if moved {
            scroll = window_at(selected, show, rows.len());
            moved = false;
        }
        let first = scroll.min(rows.len().saturating_sub(show));
        // And the count says so. "27 processes" above a table of 15 is a
        // partial result presented as a total, which is the one thing a
        // number here must never be.
        let last = (first + show).min(rows.len());
        out[count_at] = tc::seg(
            &[
                (
                    p.dim.as_str(),
                    format!(
                        " {} process{}",
                        rows.len(),
                        if rows.len() == 1 { "" } else { "es" }
                    ),
                ),
                (
                    if rows.len() > show { p.accent.as_str() } else { p.dim.as_str() },
                    if rows.len() > show {
                        format!(" · showing {}-{}", first + 1, last)
                    } else {
                        String::new()
                    },
                ),
            ],
            w - 1,
        );
        if rows.is_empty() {
            out.push(tc::seg(
                &[(
                    p.dim.as_str(),
                    "  Nothing has moved yet. Totals start at zero, so this fills as traffic happens.".into(),
                )],
                w - 1,
            ));
        } else {
            out.extend(table(&rows, w, first, show, selected, &p));
        }

        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
            vec![
                (p.accent.as_str(), "→/↵".into()),
                (p.dim.as_str(), " details".into()),
            ],
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
            vec![(p.dim.as_str(), "[,] settings".into())],
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
/// Where the process table starts so the cursor is on it.
///
/// Centred rather than nudged: the list is re-sorted by rate every couple of
/// seconds, so a row under the cursor moves on its own and a window that
/// only scrolled far enough to admit it would jump about at the edges.
///
/// It used to be copied into the test file below, under a comment saying it
/// had been extracted so the two could not disagree. They could.
fn window_at(selected: usize, show: usize, total: usize) -> usize {
    if total > show {
        selected.saturating_sub(show / 2).min(total - show)
    } else {
        0
    }
}

fn table(
    rows: &[Proc],
    w: usize,
    first: usize,
    limit: usize,
    selected: usize,
    p: &Palette,
) -> Vec<String> {
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

    for (i, row) in rows.iter().skip(first).take(limit).enumerate() {
        let live = row.up_rate + row.down_rate;
        let total = (row.up + row.down) as f64;
        let here = first + i == selected;
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
    fn a_failed_ss_read_does_not_reset_the_counter_baselines() {
        // One socket, open throughout, whose kernel counters only ever
        // climb. `ss` answers, fails, then answers again.
        let socket = |sent, recv| {
            let mut one = HashMap::new();
            one.insert(
                "42".to_string(),
                Seen {
                    sent,
                    recv,
                    peer: "192.0.2.1".into(),
                    port: 443,
                    mine: 51000,
                    cgroup: String::new(),
                },
            );
            one
        };
        let owners = HashMap::new();
        let mut state = State::default();

        // The first reading is a baseline: a socket already open when we
        // started did its megabytes before we were watching.
        absorb(&mut state, 1000.0, &socket(1_000_000, 2_000_000), &owners, None, String::new());
        assert_eq!(state.stamp, 1000.0);
        assert_eq!(state.last.get("42"), Some(&(1_000_000, 2_000_000)));

        // `ss` wedges or will not run. It must say so and leave the
        // baseline alone - both halves of it, the counters and the clock
        // they were read at.
        absorb(&mut state, 1001.0, &HashMap::new(), &owners, None, "ss would not run".into());
        assert_eq!(state.err, "ss would not run");
        assert_eq!(state.last.get("42"), Some(&(1_000_000, 2_000_000)), "baseline cleared");
        assert_eq!(state.stamp, 1000.0, "clock advanced without the counters it pairs with");

        // Two seconds after the last good reading, half a kilobyte up and a
        // kilobyte down. Before the fix this socket had no baseline left,
        // so the whole megabyte of its lifetime counted as traffic in the
        // one second since the failed poll.
        absorb(&mut state, 1002.0, &socket(1_000_500, 2_001_000), &owners, None, String::new());
        assert!(state.err.is_empty(), "a good reading must clear the error: {}", state.err);
        let row = state
            .totals
            .get(&(0, "(unattributed)".to_string()))
            .expect("the socket has a row");
        assert_eq!((row.up, row.down), (500, 1000), "counted a lifetime as one interval");
        // 500 bytes and 1000 bytes over the two seconds since the last
        // reading that had numbers in it.
        assert!((row.up_rate - 250.0).abs() < 1.0, "{}", row.up_rate);
        assert!((row.down_rate - 500.0).abs() < 1.0, "{}", row.down_rate);
    }

    /// Colour is not width: `len()` counts escape bytes, so every column
    /// check below measures the text alone.
    fn bare(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Which cell `token` starts in, counting characters rather than bytes -
    /// the arrows are three bytes each and byte offsets would lie.
    fn col(line: &str, token: &str) -> usize {
        let at = line
            .find(token)
            .unwrap_or_else(|| panic!("{:?} is not in {:?}", token, line));
        line[..at].chars().count()
    }

    fn a_resolver() -> Resolver {
        Resolver {
            known: Arc::new(Mutex::new(HashMap::new())),
            wanted: Arc::new(Mutex::new(Vec::new())),
        }
    }

    // The headings are found by looking at where the rendered row put its
    // columns, not by repeating the arithmetic the row used. A test that
    // recomputes the widths would agree with a heading that had slipped.

    #[test]
    fn the_process_cursor_is_always_on_the_screen() {
        // It was not. The table drew rows 0..show whatever the cursor was
        // doing, so on any pane too short for the whole list the cursor
        // walked off the bottom and vanished - and enter still opened the
        // row it was invisibly sitting on.
        let p = palette();
        for total in [1usize, 5, 15, 26, 200] {
            let rows: Vec<Proc> = (0..total)
                .map(|i| Proc {
                    pid: 1000 + i as i32,
                    name: format!("proc-{}", i),
                    down: 4096,
                    alive: true,
                    ..Default::default()
                })
                .collect();
            for show in [1usize, 3, 7, 15, 40] {
                for selected in 0..total {
                    let first = window_at(selected, show, total);
                    let drawn = table(&rows, 120, first, show, selected, &p);
                    let marked = drawn.iter().filter(|l| bare(l).starts_with('▸')).count();
                    assert_eq!(
                        marked, 1,
                        "total={} show={} selected={}: {} rows carried the cursor",
                        total, show, selected, marked
                    );
                    // and it is the row it claims to be
                    let on = drawn
                        .iter()
                        .find(|l| bare(l).starts_with('▸'))
                        .map(|l| bare(l))
                        .unwrap_or_default();
                    assert!(
                        on.contains(&format!("proc-{} ", selected)),
                        "total={} show={} selected={}: cursor sat on {:?}",
                        total, show, selected, on.trim()
                    );
                }
            }
        }
    }

    #[test]
    fn the_endpoint_heading_sits_over_its_columns() {
        let p = palette();
        let names = a_resolver();
        let mut spot = Spot {
            peer: "192.0.2.7".into(),
            up: 4096,
            down: 8192,
            alive: true,
            ..Default::default()
        };
        spot.ports.insert(9999);
        for w in [60usize, 84, 120, 200] {
            let head = bare(&endpoint_head(w, &p));
            let row = bare(&endpoint_rows(&[spot.clone()], 0, false, 1, w, &names, &p)[0]);
            let (wide, down, up) = (row.chars().count(), col(&row, "↓"), col(&row, "↑"));
            assert_eq!(head.chars().count(), wide, "heading width at w={}", w);
            assert_eq!(col(&head, "HOST"), 3, "host column at w={}", w);
            assert_eq!(col(&head, "PORTS"), down - 9, "ports column at w={}", w);
            assert_eq!(col(&head, "RX") + 2, down + 10, "rx column at w={}", w);
            assert_eq!(col(&head, "TX") + 2, up + 10, "tx column at w={}", w);
            assert_eq!(col(&head, "RATE") + 4, wide, "rate column at w={}", w);
        }
    }

    #[test]
    fn the_connection_heading_sits_over_its_columns() {
        let p = palette();
        let conn = Conn {
            peer: "192.0.2.7".into(),
            port: 443,
            mine: 44672,
            up: 4096,
            down: 8192,
            alive: true,
            ..Default::default()
        };
        for w in [66usize, 84, 120, 200] {
            let head = bare(&connection_head(w, &p));
            let row = bare(&connection_rows(&[conn.clone()], 0, false, 1, w, &p)[0]);
            let (wide, down, up) = (row.chars().count(), col(&row, "↓"), col(&row, "↑"));
            assert_eq!(head.chars().count(), wide, "heading width at w={}", w);
            assert_eq!(col(&head, "SOCKET"), 3, "socket column at w={}", w);
            assert_eq!(col(&head, "STATE"), down - 7, "state column at w={}", w);
            // our port sits between the address and the state, and the row
            // is searched for the port itself rather than for a width
            assert_eq!(col(&head, "LOCAL"), down - 13, "local column at w={}", w);
            assert_eq!(col(&row, "44672"), down - 13, "our port at w={}", w);
            assert_eq!(col(&head, "RX") + 2, down + 10, "rx column at w={}", w);
            assert_eq!(col(&head, "TX") + 2, up + 10, "tx column at w={}", w);
            assert_eq!(up + 10, wide, "the tx column is the last at w={}", w);
        }
    }

    #[test]
    fn the_file_heading_sits_over_its_columns() {
        let p = palette();
        let files = vec![OpenFile {
            path: "/var/log/marker.log".into(),
            size: 8192,
        }];
        let sizes = HashMap::new();
        let shown = units(8192.0);
        for w in [60usize, 84, 120, 200] {
            let head = bare(&file_head(w, &p));
            let row = bare(&file_rows(&files, &sizes, 0, false, 1, w, &p)[0]);
            let wide = row.chars().count();
            assert_eq!(head.chars().count(), wide, "heading width at w={}", w);
            assert_eq!(col(&head, "PATH"), 3, "path column at w={}", w);
            assert_eq!(
                col(&head, "SIZE") + 4,
                col(&row, &shown) + shown.chars().count(),
                "size column at w={}",
                w
            );
            assert_eq!(col(&head, "GROWTH") + 6, wide, "growth column at w={}", w);
        }
    }

    #[test]
    fn a_bursty_process_keeps_a_readable_rate() {
        let mut row = Proc::default();
        // A kilobyte at t=0 and nothing for the next three seconds. The
        // instantaneous rate is zero for most of that; the windowed one
        // stays up, which is the whole point.
        fold!(row, 0.0, 1.0, 0, 1000);
        fold!(row, 1.0, 1.0, 0, 0);
        fold!(row, 2.0, 1.0, 0, 0);
        fold!(row, 3.0, 1.0, 0, 0);
        assert!(row.down_rate > 0.0, "the rate flickered to nothing");
        // A kilobyte spread over the four seconds the window covers.
        assert!((row.down_rate - 250.0).abs() < 1.0, "got {}", row.down_rate);
        assert_eq!(row.down, 1000, "the total is unaffected by smoothing");
    }

    #[test]
    fn many_sockets_at_one_instant_are_not_a_rate() {
        let mut row = Proc::default();
        // One process, fifteen sockets, all folded at the same timestamp -
        // which is every sample, since a sample reads them all at once.
        // The window spans no time, so there is no rate to compute yet.
        for _ in 0..15 {
            fold!(row, 0.0, 0.0, 0, 400_000);
        }
        assert_eq!(row.down, 6_000_000);
        assert_eq!(
            row.down_rate, 0.0,
            "six megabytes in no time at all read as {} B/s",
            row.down_rate
        );
        // A second poll, a second apart, and the fifteen sockets of the
        // first one share the single interval they were read in - they are
        // parallel, not fifteen intervals in a row. Six megabytes over the
        // one second that poll covered. This asserted six megabytes per
        // second across a two-second window before.
        let mut row = Proc::default();
        for _ in 0..15 {
            fold!(row, 0.0, 1.0, 0, 400_000);
        }
        fold!(row, 1.0, 1.0, 0, 0);
        assert!((row.down_rate - 3_000_000.0).abs() < 1.0, "got {}", row.down_rate);
    }

    #[test]
    fn a_rate_is_the_window_it_claims() {
        let mut row = Proc::default();
        // Two kilobytes a second, steadily, for four seconds.
        for i in 0..5 {
            fold!(row, i as f64, 1.0, 0, 2000);
        }
        // Five polls, each covering a second, carrying 2000 bytes each:
        // 10000 bytes over the five seconds they cover, so 2000 a second,
        // which is what was actually sent. This asserted 2500 before - a
        // quarter too high on every row, with the overcount written into
        // the comment as though it were the intent.
        assert!((row.down_rate - 2000.0).abs() < 1.0, "got {}", row.down_rate);
    }

    #[test]
    fn history_older_than_the_window_is_dropped() {
        let mut row = Proc::default();
        fold!(row, 0.0, 1.0, 0, 5000);
        fold!(row, 100.0, 1.0, 0, 1000);
        // The ancient sample is gone, so it cannot prop the rate up.
        assert_eq!(row.recent.len(), 1);
        assert_eq!(row.down, 6000);
    }

    #[test]
    fn units_are_decimal_as_isps_quote_them() {
        assert_eq!(units(1000.0), "1.0 KB");
        assert_eq!(units(2_500_000.0), "2.5 MB");
        assert_eq!(units(3_000_000_000.0), "3.0 GB");
        assert_eq!(units(512.0), "512 B");
    }

    #[test]
    fn traffic_that_never_leaves_is_recognised() {
        let own = vec!["10.0.0.46".to_string(), "100.64.0.102".to_string()];
        assert!(local_peer("127.0.0.1", &own));
        assert!(local_peer("::1", &own));
        // The half that is easy to miss: our own non-loopback address.
        assert!(local_peer("10.0.0.46", &own));
        assert!(local_peer("::ffff:10.0.0.46", &own));
        assert!(!local_peer("10.0.0.99", &own));
    }

    #[test]
    fn only_globally_routable_peers_are_off_box() {
        let own = vec!["10.0.0.46".to_string()];
        assert!(off_box("203.0.113.10", &own));
        assert!(!off_box("10.0.0.5", &own));
        assert!(!off_box("172.16.0.1", &own));
        assert!(!off_box("192.168.1.1", &own));
        assert!(!off_box("100.64.0.102", &own));
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
