// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! BY PROCESS: which readable process is moving bytes, how many processes
//! that covers, and how much of the disks' own figure it explains.
//!
//! A partial list under a total is the founding hazard of this collection,
//! so the table never stands alone. Every frame names how many processes
//! could be read and how many could not, and the line under it ties what
//! the readable ones moved to what the disks saw over a window long enough
//! for the two clocks to agree - `write_bytes` counts when a page is
//! dirtied, the device when the write is submitted - and marks it `≈`.
//!
//! Nothing here opens a file. `linux.rs` hands over its one `/proc` pass,
//! `macos.rs` its `proc_pid_rusage` list; both arrive as [`ProcSample`]s,
//! so the rates, the sentence and the reconciliation are the same code,
//! tested on every target with hand-built samples.

// The `/proc` half (pid samples, hidepid, open files) is only acquired on
// Linux and the rusage half only on macOS; each item's counterpart is
// marked where it is declared. Everything compiles, and is tested, on both.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::{HashMap, VecDeque};

use opscope_core as tc;
use tc::procfs::{OpenFile, PidIo};

use super::writers::{PidSample, Writers, rate, secs, shown_path};
use super::{DiskSort, Palette, View, chunks, human, join_cells, wrap};

/// Item ids for a click, above WAITING's.
pub const PROC_BASE: usize = 5_000_000;

/// A process whose counters this user may not read: another user's on
/// Linux (`/proc/<pid>/io` is ptrace-gated), `EPERM` from
/// `proc_pid_rusage` on macOS (`CHECK_SAME_USER` in xnu).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unreadable;

/// Where the counters came from, which decides the wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `/proc/<pid>/io`: `read_bytes`, and `write_bytes` less
    /// `cancelled_write_bytes`. Compared with the disks' write rate.
    Procfs,
    /// `proc_pid_rusage(RUSAGE_INFO_V2)`: `ri_diskio_bytesread` and
    /// `ri_diskio_byteswritten`. Compared with iostat's read and write
    /// together, which is the only figure iostat gives.
    Rusage,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Procfs => "/proc/<pid>/io",
            Source::Rusage => "proc_pid_rusage",
        }
    }

    pub fn combined(self) -> bool {
        self == Source::Rusage
    }
}

/// One process's lifetime counters as one pass read them.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcSample {
    pub pid: i32,
    pub comm: String,
    /// When the process started, in whatever unit the host gives (clock
    /// ticks since boot, Mach absolute time). Only ever compared for
    /// equality: a pid reused between two passes is a different process,
    /// and its counters are not subtracted from a stranger's.
    pub start: u64,
    /// `(read, write, cancelled)` bytes since the process started. Kept
    /// apart so a poll's net write is the write delta less the cancelled
    /// delta: a lifetime net clamped at zero would hide every write of a
    /// process that once truncated more than it had written. `cancelled`
    /// is 0 where the host has no such counter (macOS).
    pub io: Result<(u64, u64, u64), Unreadable>,
    /// The BY SERVICE row the process's I/O lands in, as drawn (`/` for the
    /// root's own), `""` when the walk did not place it.
    pub cgroup: String,
}

/// What `proc_pid_rusage` returned for one pid: the two disk counters and
/// `ri_proc_start_abstime`, which tells a reused pid from the old one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct Rusage {
    pub read: u64,
    pub write: u64,
    pub start: u64,
}

/// Linux: `read_bytes`, `write_bytes`, and `cancelled_write_bytes` - what
/// was dirtied and then thrown away before it reached the disk (a
/// truncated or deleted file), which `step` takes off the writes.
pub fn io_counts(io: &PidIo) -> (u64, u64, u64) {
    (io.read_bytes, io.write_bytes, io.cancelled_write_bytes)
}

/// The Linux pass as samples. `cgroups` is pid → BY SERVICE row.
pub fn samples_from_pids(samples: &[PidSample], cgroups: &HashMap<i32, String>) -> Vec<ProcSample> {
    samples
        .iter()
        .map(|s| ProcSample {
            pid: s.pid,
            comm: s.comm.clone(),
            start: s.starttime,
            io: s.io.as_ref().map(io_counts).ok_or(Unreadable),
            cgroup: cgroups
                .get(&s.pid)
                .map(|c| shown_path(c))
                .unwrap_or_default(),
        })
        .collect()
}

/// The macOS list as samples. A pid `ps` did not name (it started after
/// `ps` ran, or left before) is drawn as `—` rather than dropped: it was
/// still read, so it still counts.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn samples_from_rusage(
    list: &[(i32, Result<Rusage, Unreadable>)],
    names: &HashMap<i32, String>,
) -> Vec<ProcSample> {
    list.iter()
        .map(|(pid, read)| ProcSample {
            pid: *pid,
            comm: names.get(pid).cloned().unwrap_or_else(|| "—".into()),
            start: read.map(|r| r.start).unwrap_or(0),
            io: read.map(|r| (r.read, r.write, 0)),
            cgroup: String::new(),
        })
        .collect()
}

/// `ps -axo pid=,comm=` as pid → name. The same shape as ports'
/// `parse_ps_processes`: the pid, then everything after it, which may have
/// spaces in it. Apple's `comm` is the executable's path, so the name is
/// what follows the last `/`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_ps_names(text: &str) -> HashMap<i32, String> {
    let mut found = HashMap::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some(pid_end) = line.find(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = line[..pid_end].parse::<i32>() else {
            continue;
        };
        let command = line[pid_end..].trim();
        if command.is_empty() {
            continue;
        }
        let name = command.rsplit('/').next().unwrap_or(command).trim();
        let name = if name.is_empty() { command } else { name };
        found.insert(pid, name.to_string());
    }
    found
}

/// `/proc/self/mountinfo`: whether `/proc` is mounted with a `hidepid`
/// that hides other users' processes from listing (`invisible`, `2`, or
/// `ptraceable`, `4`). `noaccess` (`1`) still lists them, so the total is
/// still every process and the word `visible` would be wrong.
pub fn parse_hidepid(mountinfo: &str) -> bool {
    mountinfo.lines().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(dash) = fields.iter().position(|f| *f == "-") else {
            return false;
        };
        let mount_point = fields.get(4).copied().unwrap_or("");
        let fstype = fields.get(dash + 1).copied().unwrap_or("");
        if mount_point != "/proc" || fstype != "proc" {
            return false;
        }
        let options = fields[5..].iter().chain(fields.get(dash + 3).into_iter());
        options
            .flat_map(|opts| opts.split(','))
            .filter_map(|opt| opt.strip_prefix("hidepid="))
            .any(|v| matches!(v, "2" | "invisible" | "4" | "ptraceable"))
    })
}

/// One process in the table: every readable one, with a rate once it has
/// been seen twice.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcRate {
    pub pid: i32,
    pub comm: String,
    pub cgroup: String,
    /// Bytes per second over the last poll. `None` the first time the pid
    /// is seen: a rate needs two samples.
    pub read_bps: Option<f64>,
    pub write_bps: Option<f64>,
    /// Since the process started.
    pub read_total: u64,
    pub write_total: u64,
}

impl ProcRate {
    fn moved(&self) -> bool {
        self.read_bps.unwrap_or(0.0) > 0.0 || self.write_bps.unwrap_or(0.0) > 0.0
    }
}

/// A process this user cannot read the counters of. Its name and cgroup
/// are still readable, so it is still a row under `a`.
#[derive(Clone, Debug, PartialEq)]
pub struct Locked {
    pub pid: i32,
    pub comm: String,
    pub cgroup: String,
}

/// The by-process half of a poll.
#[derive(Clone, Debug, PartialEq)]
pub struct Procs {
    pub source: Source,
    pub rows: Vec<ProcRate>,
    pub locked: Vec<Locked>,
    /// Seconds since the previous pass.
    pub elapsed: Option<f64>,
    /// Running as root: every process is readable, and none is "yours".
    pub root: bool,
    /// `/proc` hides other users' processes: the total is what was visible.
    pub visible: bool,
    /// Bytes the readable processes moved over the poll, in the unit the
    /// reconciliation compares: written on Linux, read and written on macOS.
    pub moved: Option<f64>,
    /// Filled by the poller once the disks are measured.
    pub window: Option<Window>,
}

/// What is kept from one pass to the next: one previous reading per pid.
#[derive(Default)]
pub struct ProcMemory {
    prev: HashMap<i32, (u64, u64, u64, u64)>,
    at: Option<f64>,
}

/// One pass turned into rates. A pid that was not in the previous pass has
/// no rate yet; one that has gone is dropped, never drawn as negative; a
/// pid whose start time changed is a reused pid and starts again.
pub fn step(
    memory: &mut ProcMemory,
    samples: Vec<ProcSample>,
    now: f64,
    root: bool,
    visible: bool,
    source: Source,
) -> Procs {
    let elapsed = memory.at.filter(|at| now > *at).map(|at| now - at);
    let mut rows = Vec::new();
    let mut locked = Vec::new();
    let mut next = HashMap::with_capacity(samples.len());
    let mut moved = 0.0;
    for s in samples {
        match s.io {
            Ok((read, written, cancelled)) => {
                // Identity is the start time; each counter is then read on
                // its own. The poll's write is its write delta less its
                // cancelled delta, so a poll that truncated more dirty data
                // than it wrote is 0, and its reads still count.
                let delta = elapsed.and_then(|e| {
                    let &(start, r0, w0, c0) = memory.prev.get(&s.pid)?;
                    (start == s.start && read >= r0).then(|| {
                        let w = written
                            .saturating_sub(w0)
                            .saturating_sub(cancelled.saturating_sub(c0));
                        ((read - r0) as f64, w as f64, e)
                    })
                });
                let write = written.saturating_sub(cancelled);
                if let Some((r, w, _)) = delta {
                    moved += if source.combined() { r + w } else { w };
                }
                next.insert(s.pid, (s.start, read, written, cancelled));
                rows.push(ProcRate {
                    pid: s.pid,
                    comm: s.comm,
                    cgroup: s.cgroup,
                    read_bps: delta.map(|(r, _, e)| r / e),
                    write_bps: delta.map(|(_, w, e)| w / e),
                    read_total: read,
                    write_total: write,
                });
            }
            Err(Unreadable) => locked.push(Locked {
                pid: s.pid,
                comm: s.comm,
                cgroup: s.cgroup,
            }),
        }
    }
    memory.prev = next;
    memory.at = Some(now);
    Procs {
        source,
        rows,
        locked,
        elapsed,
        root,
        visible,
        moved: elapsed.map(|_| moved),
        window: None,
    }
}

/// `412 of 598 processes readable (yours) · the other 186 need root`.
/// Drawn every frame, even at `598 of 598`: the number is the proof.
pub fn coverage(procs: &Procs) -> String {
    let readable = procs.rows.len();
    let total = readable + procs.locked.len();
    let other = procs.locked.len();
    let mut line = format!(
        "{readable} of {total} {}processes readable",
        if procs.visible { "visible " } else { "" }
    );
    if !procs.root {
        line.push_str(" (yours)");
    }
    match other {
        0 => {}
        1 => line.push_str(" · the other 1 needs root"),
        n => line.push_str(&format!(" · the other {n} need root")),
    }
    if procs.visible {
        line.push_str(" · hidepid keeps other users' processes out of /proc");
    }
    line
}

// ── the 60-second reconciliation ─────────────────────────────────────

/// The readable processes' bytes and the disks' bytes, summed over the
/// polls in the ring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub seconds: f64,
    pub procs: f64,
    pub device: f64,
}

/// Per-poll sums covering at most `reconcile_seconds` of measured time.
/// Bounded by age, not count: a poll takes longer than the refresh when
/// its scan is slow, so `reconcile_seconds / refresh` polls would reach
/// further back than the setting says. The newest poll is kept even when
/// it alone is longer. A poll where either side has no figure is not
/// entered at all, so both sides always cover the same polls.
#[derive(Clone, Debug)]
pub struct Ring {
    entries: VecDeque<Window>,
    seconds: f64,
}

impl Ring {
    pub fn new(seconds: f64) -> Ring {
        Ring {
            entries: VecDeque::new(),
            seconds: if seconds.is_finite() { seconds.max(0.0) } else { 0.0 },
        }
    }

    pub fn push(&mut self, seconds: f64, procs: f64, device: f64) {
        if !(seconds > 0.0) {
            return;
        }
        self.entries.push_back(Window {
            seconds,
            procs: procs.max(0.0),
            device: device.max(0.0),
        });
        let mut total: f64 = self.entries.iter().map(|e| e.seconds).sum();
        while self.entries.len() > 1 && total > self.seconds + 1e-9 {
            if let Some(old) = self.entries.pop_front() {
                total -= old.seconds;
            }
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn window(&self) -> Option<Window> {
        if self.entries.is_empty() {
            return None;
        }
        Some(self.entries.iter().fold(
            Window {
                seconds: 0.0,
                procs: 0.0,
                device: 0.0,
            },
            |sum, e| Window {
                seconds: sum.seconds + e.seconds,
                procs: sum.procs + e.procs,
                device: sum.device + e.device,
            },
        ))
    }
}

/// The line under the table.
///
/// `≈` always: `write_bytes` counts a page when it is dirtied and the
/// device when the write is submitted, which can be thirty seconds later,
/// so even over the window the two are only near each other. The share can
/// pass 100% for the same reason, and the clause says why. `disks` names
/// what the device side sums (Linux), and macOS says `combined`.
pub fn reconcile_line(window: Option<&Window>, source: Source, disks: &[String]) -> String {
    let Some(w) = window.filter(|w| w.seconds > 0.0) else {
        return "readable processes account for ≈ … of the disks · measuring".into();
    };
    let p = w.procs / w.seconds;
    let d = w.device / w.seconds;
    let (ps, ds) = (rate(p), rate(d));
    let share = if d > 0.0 && ds != rate(0.0) {
        format!(" ({:.0}%)", (p / d * 100.0).round())
    } else {
        String::new()
    };
    let over = format!("over the last {}", secs(w.seconds));
    match source {
        Source::Procfs => {
            // The disk total names its disks above the chart; past three,
            // pointing at that list reads better than repeating it.
            let to = match disks.len() {
                0 => String::new(),
                1..=3 => format!(" written to {}", disks.join(", ")),
                n => format!(" written to the {n} disks in the total above"),
            };
            format!(
                "readable processes account for ≈ {ps} of {ds}{share}{to} {over} · write_bytes counts at dirtying, devices at submission"
            )
        }
        Source::Rusage => format!(
            "readable processes account for ≈ {ps} of {ds} combined{share} {over} · read and written together, as iostat reports them · bytes handed to the disk layer"
        ),
    }
}

// ── the process detail ───────────────────────────────────────────────

/// How an open file's size moved since the previous detail sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    /// The first sample of this file on this screen.
    First,
    Grew(u64, f64),
    Shrank(u64, f64),
    Same(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileRow {
    pub path: String,
    pub size: u64,
    pub change: Change,
}

/// The previous detail sample's sizes, for one pid.
#[derive(Clone, Debug, Default)]
pub struct FileMemory {
    pid: i32,
    at: f64,
    sizes: HashMap<String, u64>,
}

/// Open files with growth since the previous sample of the same pid. A
/// file open on two descriptors is one row. Biggest first.
pub fn file_growth(
    memory: &mut Option<FileMemory>,
    pid: i32,
    files: &[OpenFile],
    now: f64,
) -> Vec<FileRow> {
    let prev = memory.as_ref().filter(|m| m.pid == pid && now > m.at);
    let mut seen: HashMap<String, u64> = HashMap::new();
    let mut rows: Vec<FileRow> = Vec::new();
    for file in files {
        if seen.contains_key(&file.path) {
            continue;
        }
        seen.insert(file.path.clone(), file.size);
        let change = match prev.and_then(|m| m.sizes.get(&file.path).map(|&was| (was, now - m.at)))
        {
            None => Change::First,
            Some((was, span)) if file.size > was => Change::Grew(file.size - was, span),
            Some((was, span)) if file.size < was => Change::Shrank(was - file.size, span),
            Some((_, span)) => Change::Same(span),
        };
        rows.push(FileRow {
            path: file.path.clone(),
            size: file.size,
            change,
        });
    }
    rows.sort_by(|a, b| b.size.cmp(&a.size).then(a.path.cmp(&b.path)));
    *memory = Some(FileMemory {
        pid,
        at: now,
        sizes: seen,
    });
    rows
}

/// `+12M in 2s`.
pub fn change_text(change: &Change) -> String {
    match change {
        Change::First => "first sample".into(),
        Change::Grew(n, span) => format!("+{} in {}", human(*n), secs(*span)),
        Change::Shrank(n, span) => format!("−{} in {}", human(*n), secs(*span)),
        Change::Same(span) => format!("unchanged in {}", secs(*span)),
    }
}

/// What the poller read for the process whose detail is open, every poll.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcDetail {
    pub pid: i32,
    /// `stat`'s state letter; `None` where the host has none to give.
    pub state: Option<char>,
    pub cmdline: String,
    pub cwd: String,
    /// `None` on a host where open files are not read (macOS).
    pub files: Option<Result<Vec<FileRow>, String>>,
}

// ── what the section draws ───────────────────────────────────────────

const PID_W: usize = 9; // marker and up to seven digits
const COMM_MIN: usize = 7;
const RATE_W: usize = 7;
const CGROUP_MIN: usize = 10;

struct Cols {
    comm: usize,
    read: bool,
    cgroup: Option<usize>,
}

/// Drop order as the pane narrows: CGROUP, then READ, then COMMAND
/// narrows and wraps. PID and WRITE stay.
fn columns(width: usize, comm_want: usize, cgroup_want: usize) -> Cols {
    let comm_want = comm_want.max(COMM_MIN);
    for &(read, cgroup) in &[(true, true), (true, false), (false, false)] {
        let mut need = PID_W + 1 + COMM_MIN + 1 + RATE_W;
        if read {
            need += 1 + RATE_W;
        }
        let cg = cgroup.then(|| CGROUP_MIN.min(cgroup_want.max(6)));
        if let Some(c) = cg {
            need += 1 + c;
        }
        if need > width {
            continue;
        }
        let mut spare = width - need;
        let comm = COMM_MIN + spare.min(comm_want - COMM_MIN);
        spare -= comm - COMM_MIN;
        let cgroup = cg.map(|c| c + spare.min(cgroup_want.saturating_sub(c)));
        return Cols { comm, read, cgroup };
    }
    Cols {
        comm: width.saturating_sub(PID_W + 2 + RATE_W).max(3),
        read: false,
        cgroup: None,
    }
}

/// What the table lists, in the order drawn: readable rows, then (under
/// `a`) the ones that need root. Without `a`, only processes that moved
/// bytes over the poll, cut to `top`.
pub fn ordered<'a>(
    procs: &'a Procs,
    sort: DiskSort,
    all: bool,
    top: usize,
) -> (Vec<&'a ProcRate>, Vec<&'a Locked>) {
    let mut rows: Vec<&ProcRate> = procs.rows.iter().filter(|r| all || r.moved()).collect();
    let key =
        |r: &ProcRate, write: bool| if write { r.write_bps } else { r.read_bps }.unwrap_or(-1.0);
    rows.sort_by(|a, b| match sort {
        DiskSort::Write => key(b, true)
            .total_cmp(&key(a, true))
            .then(a.pid.cmp(&b.pid)),
        DiskSort::Read => key(b, false)
            .total_cmp(&key(a, false))
            .then(a.pid.cmp(&b.pid)),
        DiskSort::Name => a.comm.cmp(&b.comm).then(a.pid.cmp(&b.pid)),
    });
    if !all {
        rows.truncate(top.max(1));
        return (rows, Vec::new());
    }
    let mut locked: Vec<&Locked> = procs.locked.iter().collect();
    locked.sort_by(|a, b| match sort {
        DiskSort::Name => a.comm.cmp(&b.comm).then(a.pid.cmp(&b.pid)),
        _ => a.pid.cmp(&b.pid),
    });
    (rows, locked)
}

/// The `[a]` hint on BY PROCESS, naming the state the next press moves to.
pub fn all_hint(all: bool, top: usize) -> String {
    if all {
        format!("[a] show top {top}")
    } else {
        "[a] show all processes".into()
    }
}

fn push_lines(rows: &mut Vec<String>, text: &str, ink: &str, width: usize) {
    for line in wrap(text, width.saturating_sub(1)) {
        rows.push(tc::seg(&[(ink, format!(" {line}"))], width));
    }
}

fn shown_cgroup(cgroup: &str) -> String {
    if cgroup.is_empty() {
        "—".into()
    } else {
        cgroup.to_string()
    }
}

/// What BY PROCESS drew: the row keys (pids) in order, how many rows `a`
/// would show in all, and whether the cursor stands on the subset line
/// because no row was drawn.
#[derive(Clone, Debug, Default)]
pub struct Painted {
    pub keys: Vec<String>,
    pub total: usize,
    pub anchor: bool,
}

impl Painted {
    /// What the section counts as for the cursor: its rows, or the one
    /// subset line when it has none, so it can always take focus.
    pub fn len(&self) -> usize {
        self.keys.len().max(usize::from(self.anchor))
    }
}

/// BY PROCESS on the list screen. On Linux it sits between BY SERVICE and
/// WAITING ON DISK; on macOS it is the whole section.
pub fn paint(
    view: &View,
    writers: &Writers,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    placed: &mut Vec<(usize, usize)>,
) -> Painted {
    let Some(procs) = writers.procs.as_ref() else {
        return Painted::default();
    };
    let sorted = format!("every disk · sorted: {}", view.proc_sort.label(false));
    let heading = match procs.source {
        Source::Procfs => format!(
            "BY PROCESS  {} · {sorted} · {}",
            procs.source.label(),
            coverage(procs)
        ),
        Source::Rusage => format!("{sorted} · {}", coverage(procs)),
    };
    if procs.source == Source::Procfs {
        rows.push(String::new());
    }
    push_lines(rows, &heading, &p.lbl, width);

    let (list, locked) = ordered(procs, view.proc_sort, view.proc_all, view.top_writers);
    let readable = procs.rows.len();
    let active = procs.rows.iter().filter(|r| r.moved()).count();
    let window = procs.elapsed.map(secs);
    let subset = match (&window, view.proc_all) {
        (None, false) => "measuring · a rate needs two samples".to_string(),
        (Some(e), false) if active == 0 => {
            format!("none of the {readable} readable moved bytes over the last {e}")
        }
        (Some(e), false) => {
            let base = format!("{active} of {readable} readable moved bytes over the last {e}");
            if list.len() < active {
                format!("top {} shown · {base}", list.len())
            } else {
                base
            }
        }
        (_, true) => format!(
            "every process: the {readable} readable, then the {} that need root",
            procs.locked.len()
        ),
    };
    // With no row to stand on, the line saying why there is none is where
    // the cursor goes, so BY PROCESS can still take focus and `a` can still
    // reach the full list from it.
    let anchor = list.is_empty() && locked.is_empty();
    if anchor {
        let here = view.focus == 4;
        let ink = if here {
            format!("{}{}", tc::tint(tc::Role::Slot), p.lit(&p.accent))
        } else {
            p.dim.clone()
        };
        let from = rows.len();
        push_lines(rows, &subset, &ink, width);
        placed.extend((from..rows.len()).map(|at| (at, PROC_BASE)));
    } else {
        push_lines(rows, &subset, &p.dim, width);
    }

    let comm_want = list
        .iter()
        .map(|r| tc::display_width(&r.comm))
        .chain(locked.iter().map(|l| tc::display_width(&l.comm)))
        .max()
        .unwrap_or(0);
    let cgroup_want = list
        .iter()
        .map(|r| tc::display_width(&shown_cgroup(&r.cgroup)))
        .chain(
            locked
                .iter()
                .map(|l| tc::display_width(&shown_cgroup(&l.cgroup))),
        )
        .max()
        .unwrap_or(0);
    let mut cols = columns(width, comm_want, cgroup_want);
    if procs.source == Source::Rusage {
        // macOS has no cgroups: the column is not there to drop, and its
        // room goes to the command.
        if let Some(c) = cols.cgroup.take() {
            cols.comm += (1 + c).min(comm_want.saturating_sub(cols.comm));
        }
    }
    let mut keys = Vec::new();
    if !list.is_empty() || !locked.is_empty() {
        rows.push(header(&cols, width, p));
    }
    for r in &list {
        let key = r.pid.to_string();
        let here = view.focus == 4 && view.proc_cursor == key;
        let from = rows.len();
        let cells = Cells {
            pid: r.pid,
            comm: &r.comm,
            write: r.write_bps.map(rate).unwrap_or_else(|| "—".into()),
            read: r.read_bps.map(rate).unwrap_or_else(|| "—".into()),
            cgroup: shown_cgroup(&r.cgroup),
        };
        rows.extend(row(&cells, &cols, width, here, p));
        placed.extend((from..rows.len()).map(|at| (at, PROC_BASE + keys.len())));
        keys.push(key);
    }
    if !locked.is_empty() {
        push_lines(
            rows,
            &format!(
                "{} need root · their I/O is not readable as you",
                locked.len()
            ),
            &p.dim,
            width,
        );
    }
    for l in &locked {
        let key = l.pid.to_string();
        let here = view.focus == 4 && view.proc_cursor == key;
        let from = rows.len();
        let cells = Cells {
            pid: l.pid,
            comm: &l.comm,
            write: "—".into(),
            read: "—".into(),
            cgroup: shown_cgroup(&l.cgroup),
        };
        rows.extend(row(&cells, &cols, width, here, p));
        placed.extend((from..rows.len()).map(|at| (at, PROC_BASE + keys.len())));
        keys.push(key);
    }
    let disks = view
        .board
        .map(|b| b.total_names.clone())
        .unwrap_or_default();
    push_lines(
        rows,
        &reconcile_line(procs.window.as_ref(), procs.source, &disks),
        &p.txt,
        width,
    );
    Painted {
        keys,
        total: readable + procs.locked.len(),
        anchor,
    }
}

struct Cells<'a> {
    pid: i32,
    comm: &'a str,
    write: String,
    read: String,
    cgroup: String,
}

fn header(cols: &Cols, width: usize, p: &Palette) -> String {
    let mut cells = vec![
        tc::pad("  PID", PID_W),
        tc::pad("COMMAND", cols.comm),
        format!("{:>w$}", "WRITE", w = RATE_W),
    ];
    if cols.read {
        cells.push(format!("{:>w$}", "READ", w = RATE_W));
    }
    if let Some(w) = cols.cgroup {
        cells.push(tc::pad("CGROUP", w));
    }
    tc::seg(
        &[(p.dim.as_str(), cells.join(" ").trim_end().to_string())],
        width,
    )
}

fn row(cells: &Cells, cols: &Cols, width: usize, here: bool, p: &Palette) -> Vec<String> {
    let tint = if here {
        tc::tint(tc::Role::Slot)
    } else {
        String::new()
    };
    let c = |colour: &str| {
        if tint.is_empty() {
            format!("{}{}", tint, colour)
        } else {
            format!("{}{}", tint, p.lit(colour))
        }
    };
    let comm = chunks(cells.comm, cols.comm);
    let cgroup = cols
        .cgroup
        .map(|room| chunks(&cells.cgroup, room))
        .unwrap_or_default();
    let lines = comm.len().max(cgroup.len()).max(1);
    let mut out = Vec::new();
    for line in 0..lines {
        let first = line == 0;
        let marker = if first && here { "▸ " } else { "  " };
        let pid = if first {
            cells.pid.to_string()
        } else {
            String::new()
        };
        let ink = if here && first {
            p.accent.as_str()
        } else {
            p.txt.as_str()
        };
        let mut out_cells = vec![
            (c(ink), tc::pad(&format!("{marker}{pid}"), PID_W)),
            (
                c(p.txt.as_str()),
                tc::pad(comm.get(line).map(String::as_str).unwrap_or(""), cols.comm),
            ),
            (
                c(p.txt.as_str()),
                format!(
                    "{:>w$}",
                    if first { cells.write.as_str() } else { "" },
                    w = RATE_W
                ),
            ),
        ];
        if cols.read {
            out_cells.push((
                c(p.txt.as_str()),
                format!(
                    "{:>w$}",
                    if first { cells.read.as_str() } else { "" },
                    w = RATE_W
                ),
            ));
        }
        if let Some(room) = cols.cgroup {
            out_cells.push((
                c(p.dim.as_str()),
                tc::pad(cgroup.get(line).map(String::as_str).unwrap_or(""), room),
            ));
        }
        out.push(join_cells(&out_cells, width, here, &tint));
    }
    out
}

/// The open-file rows of the process detail: the path, then its size and
/// how it moved, on the same line when they fit and under it when not.
pub fn paint_files(files: &[FileRow], width: usize, p: &Palette, rows: &mut Vec<String>) {
    for f in files {
        let figures = format!("{:>5}  {}", human(f.size), change_text(&f.change));
        let ink = match f.change {
            Change::Grew(..) => p.accent.as_str(),
            _ => p.txt.as_str(),
        };
        let path_w = tc::display_width(&f.path);
        let fig_w = tc::display_width(&figures);
        if 1 + path_w + 2 + fig_w <= width {
            let gap = width - 1 - path_w - fig_w;
            rows.push(tc::seg(
                &[
                    (p.txt.as_str(), format!(" {}", f.path)),
                    (ink, format!("{}{}", " ".repeat(gap.min(width)), figures)),
                ],
                width,
            ));
            continue;
        }
        for piece in chunks(&f.path, width.saturating_sub(1).max(1)) {
            rows.push(tc::seg(&[(p.txt.as_str(), format!(" {piece}"))], width));
        }
        if 3 + fig_w <= width {
            rows.push(tc::seg(&[(ink, format!("   {figures}"))], width));
        } else {
            push_lines(rows, &figures, ink, width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io(read: u64, write: u64, cancelled: u64) -> PidIo {
        PidIo {
            read_bytes: read,
            write_bytes: write,
            cancelled_write_bytes: cancelled,
            ..PidIo::default()
        }
    }

    fn pid_sample(pid: i32, start: u64, io: Option<PidIo>) -> PidSample {
        PidSample {
            pid,
            comm: format!("example-{pid}"),
            state: 'S',
            ppid: 1,
            starttime: start,
            blkio_ticks: None,
            io,
            cgroup: None,
        }
    }

    fn cgroups() -> HashMap<i32, String> {
        [
            (100, "user.slice".to_string()),
            (200, "system.slice/example-db.service".to_string()),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn rates_come_from_two_samples_with_cancelled_bytes_taken_out() {
        let mut memory = ProcMemory::default();
        let first = vec![
            pid_sample(100, 5, Some(io(1_000, 10_000, 2_000))),
            pid_sample(200, 6, None),
        ];
        let p = step(
            &mut memory,
            samples_from_pids(&first, &cgroups()),
            10.0,
            false,
            false,
            Source::Procfs,
        );
        assert_eq!(p.elapsed, None);
        assert_eq!(p.moved, None);
        assert_eq!(p.rows[0].write_bps, None, "a rate needs two samples");
        // Two seconds later: 4 MB more dirtied, 1 MB of it thrown away.
        let next = vec![
            pid_sample(100, 5, Some(io(3_000, 4_010_000, 1_002_000))),
            pid_sample(200, 6, None),
        ];
        let p = step(
            &mut memory,
            samples_from_pids(&next, &cgroups()),
            12.0,
            false,
            false,
            Source::Procfs,
        );
        let row = &p.rows[0];
        assert_eq!(row.write_bps, Some(1_500_000.0), "(4 MB - 1 MB) over 2 s");
        assert_eq!(row.read_bps, Some(1_000.0));
        assert_eq!(row.write_total, 3_008_000);
        assert_eq!(p.moved, Some(3_000_000.0), "written bytes on Linux");
        assert_eq!(row.cgroup, "user.slice");
        // A process whose io cannot be read still shows its cgroup.
        assert_eq!(
            p.locked,
            vec![Locked {
                pid: 200,
                comm: "example-200".into(),
                cgroup: "system.slice/example-db.service".into()
            }]
        );
    }

    #[test]
    fn cancelling_more_than_it_wrote_keeps_the_read_rate() {
        let mut memory = ProcMemory::default();
        let first = vec![pid_sample(100, 5, Some(io(1_000, 10_000, 0)))];
        let _ = step(
            &mut memory,
            samples_from_pids(&first, &HashMap::new()),
            0.0,
            false,
            false,
            Source::Procfs,
        );
        // Truncated 8 KB of dirty data while writing 1 KB and reading 4 KB.
        let next = vec![pid_sample(100, 5, Some(io(5_000, 11_000, 8_000)))];
        let p = step(
            &mut memory,
            samples_from_pids(&next, &HashMap::new()),
            2.0,
            false,
            false,
            Source::Procfs,
        );
        assert_eq!(p.rows[0].read_bps, Some(2_000.0), "reads still count");
        assert_eq!(p.rows[0].write_bps, Some(0.0), "nothing written net");
        assert_eq!(p.moved, Some(0.0));
        // Its lifetime cancelled bytes now pass its lifetime writes. The
        // next 4 KB it writes are still 4 KB, not lost under that.
        let after = vec![pid_sample(100, 5, Some(io(5_000, 15_000, 8_000)))];
        let p = step(
            &mut memory,
            samples_from_pids(&after, &HashMap::new()),
            4.0,
            false,
            false,
            Source::Procfs,
        );
        assert_eq!(p.rows[0].write_bps, Some(2_000.0));
        assert_eq!(p.moved, Some(4_000.0));
        let mut lifetime = ProcMemory::default();
        let _ = step(
            &mut lifetime,
            samples_from_pids(&[pid_sample(7, 1, Some(io(0, 4_096, 16_781_312)))], &HashMap::new()),
            0.0,
            false,
            false,
            Source::Procfs,
        );
        let p = step(
            &mut lifetime,
            samples_from_pids(&[pid_sample(7, 1, Some(io(0, 8_192, 16_781_312)))], &HashMap::new()),
            1.0,
            false,
            false,
            Source::Procfs,
        );
        assert_eq!(p.rows[0].write_bps, Some(4_096.0));
        assert_eq!(p.rows[0].write_total, 0, "the lifetime net is still floored");
    }

    #[test]
    fn an_exited_pid_is_dropped_and_a_reused_one_starts_again() {
        let mut memory = ProcMemory::default();
        let first = vec![
            pid_sample(100, 5, Some(io(0, 5_000, 0))),
            pid_sample(300, 9, Some(io(0, 9_000, 0))),
        ];
        let _ = step(
            &mut memory,
            samples_from_pids(&first, &HashMap::new()),
            0.0,
            false,
            false,
            Source::Procfs,
        );
        // 300 exited; 100 is a new process under the same pid, with a
        // smaller counter. Neither is a negative rate.
        let next = vec![pid_sample(100, 77, Some(io(0, 1_000, 0)))];
        let p = step(
            &mut memory,
            samples_from_pids(&next, &HashMap::new()),
            2.0,
            false,
            false,
            Source::Procfs,
        );
        assert_eq!(p.rows.len(), 1, "the exited pid is gone");
        assert_eq!(p.rows[0].write_bps, None, "a reused pid has no rate yet");
        assert_eq!(p.moved, Some(0.0));
        assert!(p.rows.iter().all(|r| r.write_bps.unwrap_or(0.0) >= 0.0));
    }

    fn mixed(readable: usize, locked: usize, root: bool, visible: bool) -> Procs {
        Procs {
            source: Source::Procfs,
            rows: (0..readable)
                .map(|i| ProcRate {
                    pid: 1000 + i as i32,
                    comm: "example-worker".into(),
                    cgroup: String::new(),
                    read_bps: None,
                    write_bps: None,
                    read_total: 0,
                    write_total: 0,
                })
                .collect(),
            locked: (0..locked)
                .map(|i| Locked {
                    pid: 2000 + i as i32,
                    comm: "example-daemon".into(),
                    cgroup: String::new(),
                })
                .collect(),
            elapsed: Some(2.0),
            root,
            visible,
            moved: Some(0.0),
            window: None,
        }
    }

    #[test]
    fn the_coverage_sentence_for_mixed_hidepid_and_full_sets() {
        assert_eq!(
            coverage(&mixed(412, 186, false, false)),
            "412 of 598 processes readable (yours) · the other 186 need root"
        );
        assert_eq!(
            coverage(&mixed(40, 0, false, true)),
            "40 of 40 visible processes readable (yours) · hidepid keeps other users' processes out of /proc"
        );
        assert_eq!(
            coverage(&mixed(598, 0, true, false)),
            "598 of 598 processes readable",
            "drawn even when everything is readable"
        );
        assert_eq!(
            coverage(&mixed(3, 1, false, false)),
            "3 of 4 processes readable (yours) · the other 1 needs root"
        );
    }

    #[test]
    fn hidepid_is_read_from_the_proc_mount() {
        let line = |opts: &str| {
            format!(
                "22 26 0:21 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc {opts}\n"
            )
        };
        assert!(parse_hidepid(&line("rw,hidepid=invisible")));
        assert!(parse_hidepid(&line("rw,hidepid=2")));
        assert!(!parse_hidepid(&line("rw,hidepid=noaccess")), "still listed");
        assert!(!parse_hidepid(&line("rw")));
        assert!(!parse_hidepid(
            "30 22 0:5 / /mnt/elsewhere rw - proc proc rw,hidepid=2\n"
        ));
    }

    #[test]
    fn the_window_is_a_bounded_ring_and_reads_approximately() {
        let mut ring = Ring::new(60.0);
        assert_eq!(
            reconcile_line(ring.window().as_ref(), Source::Procfs, &[]),
            "readable processes account for ≈ … of the disks · measuring"
        );
        let mib = 1024.0 * 1024.0;
        // 40 polls: only the last 30 count. Each poll the processes
        // dirtied 30 MiB and the disk took 376 MiB.
        for i in 0..40 {
            let procs = if i < 10 { 999.0 * mib } else { 30.0 * mib };
            ring.push(2.0, procs, 376.0 * mib);
        }
        assert_eq!(ring.len(), 30);
        let w = ring.window().unwrap();
        assert_eq!(w.seconds, 60.0);
        let line = reconcile_line(Some(&w), Source::Procfs, &["sda".into(), "sdb".into()]);
        assert_eq!(
            line,
            "readable processes account for ≈ 15M/s of 188M/s (8%) written to sda, sdb over the last 60s · write_bytes counts at dirtying, devices at submission"
        );
        ring.push(0.0, 1.0, 1.0);
        assert_eq!(ring.len(), 30, "a poll with no elapsed time is not entered");
        let mac = reconcile_line(Some(&w), Source::Rusage, &[]);
        assert!(mac.starts_with("readable processes account for ≈ 15M/s of 188M/s combined (8%)"));
        assert!(mac.contains("bytes handed to the disk layer"));
        let idle = Window {
            seconds: 60.0,
            procs: 0.0,
            device: 0.0,
        };
        assert!(!reconcile_line(Some(&idle), Source::Procfs, &[]).contains('%'));
        // Polls that ran long: the window is 60 s of measured time, not
        // 30 polls.
        let mut slow = Ring::new(60.0);
        for _ in 0..40 {
            slow.push(2.5, 1.0, 1.0);
        }
        assert_eq!(slow.len(), 24);
        assert_eq!(slow.window().unwrap().seconds, 60.0);
        // A single poll longer than the window is still kept.
        slow.push(90.0, 1.0, 1.0);
        assert_eq!(slow.len(), 1);
        assert_eq!(Ring::new(f64::NAN).len(), 0);
    }

    #[test]
    fn rusage_rows_render_the_same_sentence_and_eperm_is_unreadable() {
        let names = parse_ps_names(
            "  1 /sbin/example-init\n 501 /Applications/Example App.app/Contents/MacOS/Example App\n 502 example-sync\n",
        );
        assert_eq!(names.get(&501).map(String::as_str), Some("Example App"));
        assert_eq!(names.get(&1).map(String::as_str), Some("example-init"));
        let first: Vec<(i32, Result<Rusage, Unreadable>)> = vec![
            (1, Err(Unreadable)),
            (
                501,
                Ok(Rusage {
                    read: 100,
                    write: 1_000,
                    start: 4,
                }),
            ),
            (
                502,
                Ok(Rusage {
                    read: 0,
                    write: 0,
                    start: 4,
                }),
            ),
            (
                503,
                Ok(Rusage {
                    read: 0,
                    write: 0,
                    start: 4,
                }),
            ),
        ];
        let mut memory = ProcMemory::default();
        let _ = step(
            &mut memory,
            samples_from_rusage(&first, &names),
            0.0,
            false,
            false,
            Source::Rusage,
        );
        let mut next = first.clone();
        next[1].1 = Ok(Rusage {
            read: 2_100,
            write: 5_000,
            start: 4,
        });
        let p = step(
            &mut memory,
            samples_from_rusage(&next, &names),
            2.0,
            false,
            false,
            Source::Rusage,
        );
        assert_eq!(
            coverage(&p),
            "3 of 4 processes readable (yours) · the other 1 needs root"
        );
        assert_eq!(p.moved, Some(6_000.0), "read and write together on macOS");
        let app = p.rows.iter().find(|r| r.pid == 501).unwrap();
        assert_eq!(
            (app.read_bps, app.write_bps),
            (Some(1_000.0), Some(2_000.0))
        );
        assert_eq!(
            p.rows.iter().find(|r| r.pid == 503).unwrap().comm,
            "—",
            "a pid ps did not name still counts"
        );
        let (list, _) = ordered(&p, DiskSort::Write, false, 8);
        assert_eq!(
            list.len(),
            1,
            "idle readable processes are counted, not listed"
        );
    }

    #[test]
    fn open_file_growth_from_two_samples() {
        let file = |path: &str, size: u64| OpenFile {
            path: path.into(),
            size,
        };
        let mut memory = None;
        let first = file_growth(
            &mut memory,
            42,
            &[
                file("/tmp/example/out.bin", 1 << 20),
                file("/tmp/example/log", 10),
            ],
            100.0,
        );
        assert!(first.iter().all(|f| f.change == Change::First));
        let next = file_growth(
            &mut memory,
            42,
            &[
                file("/tmp/example/out.bin", 13 << 20),
                file("/tmp/example/out.bin", 13 << 20),
                file("/tmp/example/log", 4),
                file("/tmp/example/new", 0),
            ],
            102.0,
        );
        assert_eq!(next.len(), 3, "one row per path");
        assert_eq!(next[0].change, Change::Grew(12 << 20, 2.0));
        assert_eq!(change_text(&next[0].change), "+12M in 2s");
        let log = next.iter().find(|f| f.path.ends_with("log")).unwrap();
        assert_eq!(change_text(&log.change), "−6B in 2s");
        let new = next.iter().find(|f| f.path.ends_with("new")).unwrap();
        assert_eq!(new.change, Change::First);
        // Another pid starts over.
        let other = file_growth(&mut memory, 43, &[file("/tmp/example/out.bin", 1)], 104.0);
        assert_eq!(other[0].change, Change::First);
    }

    #[test]
    fn columns_drop_cgroup_then_read() {
        let wide = columns(140, 14, 30);
        assert!(wide.read && wide.cgroup.is_some());
        let mid = columns(40, 14, 30);
        assert!(mid.read && mid.cgroup.is_none(), "CGROUP leaves first");
        let narrow = columns(28, 14, 30);
        assert!(!narrow.read && narrow.cgroup.is_none());
        assert!(PID_W + 1 + narrow.comm + 1 + RATE_W <= 28);
    }
}
