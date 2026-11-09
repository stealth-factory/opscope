// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! WHO IS WRITING: which cgroup is putting bytes on the selected disk, tied
//! back to the disk's own counters, and which processes are waiting on it.
//!
//! Nothing here opens a file. `linux.rs` walks `/sys/fs/cgroup` once per
//! poll and passes `/proc` once; [`step`] turns two of those into rates,
//! and the painters below draw them. So the arithmetic and the wording run
//! on every target and are tested with hand-built trees. BY PROCESS, which
//! sits between BY SERVICE and WAITING, is `by_process.rs`; a Mac build
//! receives a [`Writers`] holding only that ([`by_process_only`]) and
//! draws nothing else here.
//!
//! Attribution is core's [`cgroup::attribute`]: deepest io-enabled level,
//! children subtracted. It is run over a tree of *deltas* rather than over
//! two trees separately, because a hierarchical counter keeps a removed
//! child's bytes: diffing two attributions would hand a parent every byte
//! a short-lived child ever wrote, in the sample after that child went.

// Only `linux.rs` builds a full `Writers`. Everything here still compiles,
// and its tests run, on every target; on a Mac the cgroup half is unused.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use opscope_core as tc;
use tc::cgroup::{self, Attributed, IoStat, Reading, State, Tree};
use tc::procfs::PidIo;

use super::by_process::{self, ProcDetail, Procs};
use super::{
    chunks, human, join_cells, ordered_disks, parse, push_group, push_note, push_part, wrap,
    DiskViewRow, Palette, ShownRate, View, PART_INDENT,
};

/// Rows WAITING ON DISK shows before `+N more`.
pub const WAIT_TOP: usize = 5;
/// Item ids for a click, above the disk and pressure ids in `main.rs`.
pub const SERVICE_BASE: usize = 3_000_000;
pub const WAIT_BASE: usize = 4_000_000;
/// The `+N more` row's cursor key. A pid is never this.
pub const MORE: &str = "more";

/// What one `/proc` pass learned about one process.
#[derive(Clone, Debug)]
pub struct PidSample {
    pub pid: i32,
    /// `stat`'s field 2, which is the same 15 bytes `/proc/<pid>/comm` holds.
    pub comm: String,
    pub state: char,
    pub ppid: i32,
    /// Field 22. A pid reused between two passes has a different one, so
    /// its wait ticks are not subtracted from a stranger's.
    pub starttime: u64,
    /// Field 42, `delayacct_blkio_ticks`. `None` on a kernel too old to
    /// print it.
    pub blkio_ticks: Option<u64>,
    /// `/proc/<pid>/io` when this user may read it. BY PROCESS diffs this
    /// same reading against the previous pass's.
    pub io: Option<PidIo>,
    /// Read only for a process in D state that the cgroup walk did not
    /// place (one that started after the walk).
    pub cgroup: Option<String>,
}

/// The by-service half, or the reason there is none.
#[derive(Clone, Debug)]
pub enum Services {
    /// One walk so far; a rate needs two.
    Measuring,
    Table(Table),
    V1,
    /// The line drawn in place of the table.
    NotEnabled(String),
    Unreadable(String),
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    /// Every io-enabled cgroup's own bytes over the interval, per `MAJ:MIN`.
    pub own: Vec<Attributed>,
    pub elapsed: f64,
    pub depth: String,
    pub container: bool,
    /// The pids whose I/O lands in each row: its own, and those of any
    /// descendant without `io` that rolls up into it.
    pub members: BTreeMap<String, Vec<i32>>,
    /// Controllers enabled on each row's cgroup.
    pub enabled: BTreeMap<String, Vec<String>>,
    /// How many descendant cgroups without `io` roll up into each row.
    pub rolled: BTreeMap<String, usize>,
}

/// One process in D state.
#[derive(Clone, Debug, PartialEq)]
pub struct Waiter {
    pub pid: i32,
    pub comm: String,
    /// A kernel thread: `kthreadd` itself or a child of it.
    pub kernel: bool,
    /// Seconds spent waiting on block I/O over the last interval, from the
    /// field-42 delta. `None` with wait times off, or on the first sample
    /// of this pid.
    pub waited: Option<f64>,
    /// Relative cgroup path, `""` for the root.
    pub cgroup: String,
}

/// What a poll hands the drawing thread.
#[derive(Clone, Debug)]
pub struct Writers {
    pub services: Services,
    /// Pids whose `io` this user can read.
    pub readable: BTreeSet<i32>,
    /// `comm` of every pid the pass saw, for the service detail's list.
    pub names: HashMap<i32, String>,
    /// When each pid the pass saw started, in the host's unit. An open
    /// detail compares it, so a reused pid is not drawn as the one opened.
    pub starts: HashMap<i32, u64>,
    /// Every process in D state, kernel threads included.
    pub waiters: Vec<Waiter>,
    /// `task_delayacct` is 1 and the kernel prints field 42.
    pub wait_times: bool,
    /// Seconds between this pass and the previous one.
    pub wait_elapsed: Option<f64>,
    /// `/proc` could not be listed.
    pub pid_err: String,
    /// `io.pressure` of the cgroup whose detail is open.
    pub pressure: Option<(String, Result<parse::Pressure, String>)>,
    /// BY PROCESS, from the same pass.
    pub procs: Option<Procs>,
    /// What was read this poll for the process whose detail is open.
    pub detail: Option<ProcDetail>,
    /// macOS: BY PROCESS is the whole section. There is no BY SERVICE and
    /// no WAITING ON DISK there, and no placeholder for either.
    pub by_process_only: bool,
}

/// What the open detail screen asks the poller to read with its pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wanted {
    /// The cgroup whose `io.pressure` to read.
    pub cgroup: Option<String>,
    /// The process whose command line, working directory and open files to
    /// read, every poll.
    pub pid: Option<i32>,
}

/// The section on a host that has only BY PROCESS (macOS).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn by_process_only(
    procs: Procs,
    starts: HashMap<i32, u64>,
    pid_err: String,
    detail: Option<ProcDetail>,
) -> Writers {
    Writers {
        starts,
        services: Services::Measuring,
        readable: procs.rows.iter().map(|r| r.pid).collect(),
        names: procs
            .rows
            .iter()
            .map(|r| (r.pid, r.comm.clone()))
            .chain(procs.locked.iter().map(|l| (l.pid, l.comm.clone())))
            .collect(),
        waiters: Vec::new(),
        wait_times: false,
        wait_elapsed: None,
        pid_err,
        pressure: None,
        procs: Some(procs),
        detail,
        by_process_only: true,
    }
}

/// The BY SERVICE row each listed pid's I/O lands in: its own cgroup when
/// that has `io`, otherwise the nearest ancestor that does. Empty without
/// a tree.
pub fn pid_rows(state: &State) -> HashMap<i32, String> {
    let (State::Ok(tree) | State::Container(tree)) = state else {
        return HashMap::new();
    };
    let owner = owners(tree);
    let mut out = HashMap::new();
    for node in &tree.nodes {
        let Some(row) = owner.get(&node.path) else {
            continue;
        };
        for pid in node.procs.iter().flatten() {
            out.insert(*pid, row.clone());
        }
    }
    out
}

/// Everything kept from one poll to the next: the previous tree and the
/// previous wait ticks per pid. Nothing else grows.
#[derive(Default)]
pub struct Memory {
    tree: Option<(Tree, f64)>,
    ticks: HashMap<i32, (u64, u64)>,
    ticks_at: Option<f64>,
}

/// One poll's walk and pass, turned into what the section draws.
///
/// `now` is monotonic seconds; `hz` is clock ticks per second. A
/// `task_delayacct` of 0, or a kernel that does not print field 42, is
/// wait times off.
pub fn step(
    memory: &mut Memory,
    state: State,
    samples: Vec<PidSample>,
    pid_err: String,
    delayacct: bool,
    now: f64,
    hz: f64,
) -> Writers {
    let named = services_for(&state);
    let (tree, container) = match state {
        State::Ok(tree) => (Some(tree), false),
        State::Container(tree) => (Some(tree), true),
        _ => (None, false),
    };
    let placed = tree.as_ref().map(placed_pids).unwrap_or_default();
    let services = match (&tree, named) {
        (Some(next), _) => match memory.tree.take() {
            Some((prev, at)) if now > at => {
                Services::Table(table(&prev, next, now - at, container))
            }
            _ => Services::Measuring,
        },
        (None, Some(named)) => named,
        (None, None) => Services::Measuring,
    };
    memory.tree = tree.map(|tree| (tree, now));

    let wait_times = delayacct && samples.iter().any(|s| s.blkio_ticks.is_some());
    let elapsed = memory.ticks_at.filter(|at| now > *at).map(|at| now - at);
    let waiters = waiters(&samples, &memory.ticks, elapsed, hz, wait_times, &placed);
    memory.ticks = samples
        .iter()
        .filter_map(|s| Some((s.pid, (s.starttime, s.blkio_ticks?))))
        .collect();
    memory.ticks_at = Some(now);
    Writers {
        services,
        readable: samples
            .iter()
            .filter(|s| s.io.is_some())
            .map(|s| s.pid)
            .collect(),
        names: samples.iter().map(|s| (s.pid, s.comm.clone())).collect(),
        starts: samples.iter().map(|s| (s.pid, s.starttime)).collect(),
        waiters,
        wait_times,
        wait_elapsed: elapsed,
        pid_err,
        pressure: None,
        procs: None,
        detail: None,
        by_process_only: false,
    }
}

/// The named states, from a walk that returned no tree.
fn services_for(state: &State) -> Option<Services> {
    match state {
        State::V1 => Some(Services::V1),
        State::NotEnabled(_) => Some(Services::NotEnabled(format!(
            "io accounting is off everywhere · {} enables it per unit",
            state
                .remedy()
                .unwrap_or_else(|| "systemctl set-property <unit> IOAccounting=yes".into())
        ))),
        State::Unreadable(why) => Some(Services::Unreadable(why.clone())),
        State::Ok(_) | State::Container(_) => None,
    }
}

/// `next - prev` per cgroup and device, as a tree [`cgroup::attribute`] can
/// read. A cgroup the previous walk did not have is new since then and
/// started from zero, so its whole count is this interval's. A counter that
/// went backwards is a cgroup removed and made again under the same name,
/// and the same applies. A cgroup that was there without an `io` reading
/// and has one now gets no reading: its count could be from any time.
pub fn delta_tree(prev: &Tree, next: &Tree) -> Tree {
    let before: HashMap<&str, &cgroup::Node> =
        prev.nodes.iter().map(|n| (n.path.as_str(), n)).collect();
    let nodes = next
        .nodes
        .iter()
        .map(|node| {
            let io = match (before.get(node.path.as_str()), &node.io) {
                (_, None) => None,
                (None, Some(lines)) => Some(lines.clone()),
                (Some(old), Some(lines)) => match &old.io {
                    None => None,
                    Some(old_lines) => Some(
                        lines
                            .iter()
                            .map(|line| match old_lines.iter().find(|o| o.dev == line.dev) {
                                Some(o) if line.rbytes >= o.rbytes && line.wbytes >= o.wbytes => {
                                    IoStat {
                                        dev: line.dev.clone(),
                                        rbytes: line.rbytes - o.rbytes,
                                        wbytes: line.wbytes - o.wbytes,
                                        rios: line.rios.saturating_sub(o.rios),
                                        wios: line.wios.saturating_sub(o.wios),
                                        dbytes: line.dbytes.saturating_sub(o.dbytes),
                                        dios: line.dios.saturating_sub(o.dios),
                                    }
                                }
                                _ => line.clone(),
                            })
                            .collect(),
                    ),
                },
            };
            cgroup::Node {
                path: node.path.clone(),
                enabled: node.enabled.clone(),
                io,
                cpu: None,
                memory: None,
                procs: node.procs.clone(),
            }
        })
        .collect();
    Tree { nodes }
}

fn has_io(node: &cgroup::Node) -> bool {
    node.enabled.iter().any(|c| c == "io")
}

fn parent(path: &str) -> Option<&str> {
    if path.is_empty() {
        None
    } else {
        Some(path.rsplit_once('/').map(|(p, _)| p).unwrap_or(""))
    }
}

/// The io-enabled cgroup each cgroup's I/O lands in: itself when it has
/// `io`, otherwise the nearest ancestor that does.
fn owners(tree: &Tree) -> HashMap<String, String> {
    let by_path: HashMap<&str, &cgroup::Node> =
        tree.nodes.iter().map(|n| (n.path.as_str(), n)).collect();
    let mut out = HashMap::new();
    for node in &tree.nodes {
        let mut at = Some(node.path.as_str());
        while let Some(path) = at {
            match by_path.get(path) {
                Some(n) if has_io(n) => {
                    out.insert(node.path.clone(), path.to_string());
                    break;
                }
                _ => at = parent(path),
            }
        }
    }
    out
}

fn table(prev: &Tree, next: &Tree, elapsed: f64, container: bool) -> Table {
    let delta = delta_tree(prev, next);
    let own = cgroup::attribute(&delta, "io");
    let owner = owners(next);
    let mut members: BTreeMap<String, Vec<i32>> = BTreeMap::new();
    let mut rolled: BTreeMap<String, usize> = BTreeMap::new();
    for node in &next.nodes {
        let Some(row) = owner.get(&node.path) else {
            continue;
        };
        if row != &node.path {
            *rolled.entry(row.clone()).or_default() += 1;
        }
        if let Some(procs) = &node.procs {
            members
                .entry(row.clone())
                .or_default()
                .extend(procs.iter().copied());
        }
    }
    for pids in members.values_mut() {
        pids.sort_unstable();
        pids.dedup();
    }
    let enabled = next
        .nodes
        .iter()
        .filter(|n| has_io(n))
        .map(|n| (n.path.clone(), n.enabled.clone()))
        .collect();
    Table {
        own,
        elapsed,
        depth: cgroup::depth_sentence(next, "io"),
        container,
        members,
        enabled,
        rolled,
    }
}

/// Every pid the walk listed, and the cgroup it sits in.
fn placed_pids(tree: &Tree) -> HashMap<i32, String> {
    let mut out = HashMap::new();
    for node in &tree.nodes {
        for pid in node.procs.iter().flatten() {
            out.insert(*pid, node.path.clone());
        }
    }
    out
}

/// Every process in D state now, kernel threads included, with the time it
/// waited on block I/O since the previous pass when wait times are on.
pub fn waiters(
    samples: &[PidSample],
    prev: &HashMap<i32, (u64, u64)>,
    elapsed: Option<f64>,
    hz: f64,
    wait_times: bool,
    placed: &HashMap<i32, String>,
) -> Vec<Waiter> {
    samples
        .iter()
        .filter(|s| s.state == 'D')
        .map(|s| {
            let waited = if wait_times && hz > 0.0 && elapsed.is_some() {
                match (prev.get(&s.pid), s.blkio_ticks) {
                    (Some(&(start, before)), Some(now))
                        if start == s.starttime && now >= before =>
                    {
                        Some((now - before) as f64 / hz)
                    }
                    _ => None,
                }
            } else {
                None
            };
            let cgroup = placed
                .get(&s.pid)
                .cloned()
                .or_else(|| {
                    s.cgroup
                        .as_ref()
                        .map(|c| c.trim_start_matches('/').to_string())
                })
                .unwrap_or_default();
            Waiter {
                pid: s.pid,
                comm: s.comm.clone(),
                kernel: s.pid == 2 || s.ppid == 2,
                waited,
                cgroup,
            }
        })
        .collect()
}

/// `/proc/sys/kernel/task_delayacct`: anything but a non-zero number is off.
pub fn parse_task_delayacct(text: &str) -> bool {
    text.trim().parse::<u64>().is_ok_and(|n| n != 0)
}

// ── what the section draws ───────────────────────────────────────────

/// One row of BY SERVICE for the selected disk.
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceRow {
    pub path: String,
    pub write_bps: f64,
    pub read_bps: f64,
    pub procs: usize,
    pub readable: usize,
    /// The walk read this cgroup below its children (it is not atomic, and
    /// the root counts completions while its children count submissions).
    /// The figure is the floor, zero.
    pub inconsistent: bool,
}

/// Every cgroup that moved bytes on `devs` over the interval: the disk and
/// its partitions. Idle cgroups are not rows.
pub fn rows_for(
    table: &Table,
    devs: &BTreeSet<String>,
    readable: &BTreeSet<i32>,
) -> Vec<ServiceRow> {
    let elapsed = table.elapsed.max(f64::MIN_POSITIVE);
    table
        .own
        .iter()
        .filter_map(|row| {
            let Reading::Io(per_dev) = &row.own else {
                return None;
            };
            let (mut wbytes, mut rbytes) = (0u64, 0u64);
            for (dev, stat) in per_dev {
                if devs.contains(dev) {
                    wbytes = wbytes.saturating_add(stat.wbytes);
                    rbytes = rbytes.saturating_add(stat.rbytes);
                }
            }
            if wbytes == 0 && rbytes == 0 {
                return None;
            }
            let members = table.members.get(&row.path);
            let procs = members.map_or(0, Vec::len);
            let readable = members.map_or(0, |pids| {
                pids.iter().filter(|p| readable.contains(p)).count()
            });
            Some(ServiceRow {
                path: row.path.clone(),
                write_bps: wbytes as f64 / elapsed,
                read_bps: rbytes as f64 / elapsed,
                procs,
                readable,
                inconsistent: row.inconsistent,
            })
        })
        .collect()
}

/// How far the cgroup sum may run above the device before it is called
/// dirtying running ahead rather than two reads a moment apart.
const AHEAD: f64 = 0.02;

pub fn rate(bps: f64) -> String {
    format!("{}/s", human(bps.max(0.0).round() as u64))
}

/// The line under the table that ties the cgroups to the device.
///
/// Like with like: both figures are writes over the same poll. A shortfall
/// is named whenever the two figures read differently; one that rounds away
/// at the precision drawn (`18M/s of 18M/s`) is not. A sum more than [`AHEAD`] above the
/// device is never drawn as `accounted` - it says so, and says why. Under
/// that, the excess is the two sources being read a moment apart (the
/// device's counters, then the cgroup tree), and the line says that instead
/// of claiming a cause it cannot see.
pub fn reconcile(sum: f64, device: f64) -> String {
    let (s, d) = (rate(sum), rate(device));
    if sum > device && s != d {
        if sum - device > device * AHEAD {
            return format!(
                "cgroups report {s} against {d} at the device · dirtying runs ahead of submission"
            );
        }
        return format!(
            "accounted: {s} of {d} · within 2%: the device and the cgroups are read a moment apart"
        );
    }
    let rest = device - sum;
    if rest > 0.0 && s != d {
        return format!(
            "accounted: {s} of {d} · the rest, {}, is writeback on filesystems without cgroup writeback, or root's own",
            rate(rest)
        );
    }
    format!("accounted: {s} of {d}")
}

/// `/` for the root, the relative path otherwise.
pub fn shown_path(path: &str) -> String {
    if path.is_empty() {
        "/".into()
    } else {
        path.to_string()
    }
}

pub fn procs_text(procs: usize, readable: usize, full: bool) -> String {
    if !full || procs == 0 {
        return procs.to_string();
    }
    match (procs, readable) {
        (1, 0) => "1 · not readable".into(),
        (1, _) => "1 · readable".into(),
        (n, 0) => format!("{n} · none readable as you"),
        (n, r) if r >= n => format!("{n} · all readable"),
        (n, r) => format!("{n} · {r} readable"),
    }
}

/// `1.8s of 2s`.
pub fn waited_text(waited: Option<f64>, elapsed: Option<f64>) -> String {
    match (waited, elapsed) {
        (Some(w), Some(e)) => format!("{} of {}", secs(w), secs(e)),
        _ => "measuring".into(),
    }
}

pub fn secs(s: f64) -> String {
    let tenths = (s * 10.0).round() / 10.0;
    if (tenths - tenths.round()).abs() < 1e-9 {
        format!("{:.0}s", tenths)
    } else {
        format!("{:.1}s", tenths)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitSort {
    Waited,
    Pid,
}

impl WaitSort {
    pub fn next(self) -> Self {
        match self {
            Self::Waited => Self::Pid,
            Self::Pid => Self::Waited,
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Waited => "[s] sort by pid",
            Self::Pid => "[s] sort by waited",
        }
    }
}

/// The `[a]` hint, naming the state the next press moves to.
pub fn all_hint(all: bool, top: usize) -> String {
    if all {
        format!("[a] show top {top}")
    } else {
        "[a] show all cgroups".into()
    }
}

/// D-state processes in the order drawn, and how many kernel threads the
/// setting hid. With wait times off the order is pid: a sample, not a rank.
pub fn shown_waiters(
    writers: &Writers,
    show_kernel: bool,
    sort: WaitSort,
) -> (Vec<&Waiter>, usize) {
    let hidden = if show_kernel {
        0
    } else {
        writers.waiters.iter().filter(|w| w.kernel).count()
    };
    let mut shown: Vec<&Waiter> = writers
        .waiters
        .iter()
        .filter(|w| show_kernel || !w.kernel)
        .collect();
    if writers.wait_times && sort == WaitSort::Waited {
        shown.sort_by(|a, b| {
            b.waited
                .unwrap_or(-1.0)
                .total_cmp(&a.waited.unwrap_or(-1.0))
                .then(a.pid.cmp(&b.pid))
        });
    } else {
        shown.sort_by_key(|w| w.pid);
    }
    (shown, hidden)
}

/// The disk the section describes: the one the DISKS cursor was put on,
/// or until then the disk the table shows first - which, sorted by write,
/// is the busiest.
pub fn selected_disk<'a>(view: &View<'a>) -> Option<&'a DiskViewRow> {
    let board = view.board?;
    let order = ordered_disks(board, view.disk_sort, view.show_idle, view.hide_devices);
    order
        .iter()
        .map(|&i| &board.rows[i])
        .find(|row| view.disk_chosen && row.disk.name == view.disk_cursor)
        .or_else(|| order.first().map(|&i| &board.rows[i]))
}

/// The disk's own number and its partitions'. A dm or md device on top of
/// this disk is a disk row of its own, with its own `diskstats`, and the
/// kernel already charges the I/O it passes down to this disk's number, so
/// adding the upper device here would count those bytes twice.
pub fn devs_of(row: &DiskViewRow) -> BTreeSet<String> {
    std::iter::once(row.disk.dev.clone())
        .chain(row.disk.partitions.iter().map(|p| p.dev.clone()))
        .filter(|d| !d.is_empty())
        .collect()
}

/// Indexes into `rows` in the order drawn, cut to `top_writers` unless `a`
/// asked for every one.
pub fn ordered_rows(view: &View, rows: &[ServiceRow]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    use super::DiskSort;
    order.sort_by(|&a, &b| {
        let (l, r) = (&rows[a], &rows[b]);
        match view.svc_sort {
            DiskSort::Write => r
                .write_bps
                .total_cmp(&l.write_bps)
                .then(l.path.cmp(&r.path)),
            DiskSort::Read => r.read_bps.total_cmp(&l.read_bps).then(l.path.cmp(&r.path)),
            DiskSort::Name => l.path.cmp(&r.path),
        }
    });
    if !view.svc_all {
        order.truncate(view.top_writers.max(1));
    }
    order
}

const NAME_MIN: usize = 10;
/// What the name column asks for before the PROCS wording is kept long.
const NAME_ROOMY: usize = 24;
const RATE_W: usize = 7;
const SHARE_W: usize = 5;

struct SvcCols {
    name: usize,
    read: bool,
    share: bool,
    procs: bool,
    full: bool,
    procs_w: usize,
}

/// Drop order as the pane narrows: who can be read in PROCS, then PROCS,
/// then SHARE, then READ. The cgroup and its write rate stay, and a path
/// longer than its column wraps.
fn svc_columns(width: usize, longest: usize, full_w: usize, count_w: usize) -> SvcCols {
    let want = (longest + 2).max(NAME_MIN);
    // First pass: the name column holds a typical cgroup path whole
    // (`user.slice`, a `system.slice/<unit>` of moderate length) before
    // the PROCS wording is kept long. Without it a 60-column pane kept
    // `543 · 534 readable` and broke every name into nine-cell pieces.
    let roomy = want.min(NAME_ROOMY);
    for &(read, share, procs, full) in &[
        (true, true, true, true),
        (true, true, true, false),
        (true, true, false, false),
    ] {
        let procs_w = if full { full_w } else { count_w }.max(5);
        let mut need = roomy + 1 + RATE_W;
        if read {
            need += 1 + RATE_W;
        }
        if share {
            need += 1 + SHARE_W;
        }
        if procs {
            need += 1 + procs_w;
        }
        if need <= width {
            return SvcCols {
                name: (roomy + width - need).min(want),
                read,
                share,
                procs,
                full,
                procs_w,
            };
        }
    }
    for &(read, share, procs, full) in &[
        (true, true, true, true),
        (true, true, true, false),
        (true, true, false, false),
        (true, false, false, false),
        (false, false, false, false),
    ] {
        let procs_w = if full { full_w } else { count_w }.max(5);
        let mut need = NAME_MIN + 1 + RATE_W;
        if read {
            need += 1 + RATE_W;
        }
        if share {
            need += 1 + SHARE_W;
        }
        if procs {
            need += 1 + procs_w;
        }
        if need <= width {
            return SvcCols {
                name: (NAME_MIN + width - need).min(want),
                read,
                share,
                procs,
                full,
                procs_w,
            };
        }
    }
    SvcCols {
        name: width.saturating_sub(1 + RATE_W).max(3),
        read: false,
        share: false,
        procs: false,
        full: false,
        procs_w: 5,
    }
}

const PID_W: usize = 9; // marker and up to seven digits
const COMM_MIN: usize = 7;
const STATE_W: usize = 5;
const CGROUP_MIN: usize = 16;

struct WaitCols {
    comm: usize,
    state: bool,
    waited: Option<usize>,
    cgroup: Option<usize>,
}

/// Drop order as the pane narrows: CGROUP, then STATE (always `D` here),
/// then COMMAND narrows and wraps. PID and WAITED stay.
fn wait_columns(
    width: usize,
    comm_want: usize,
    cgroup_want: usize,
    waited: Option<(usize, usize)>,
) -> WaitCols {
    let comm_want = comm_want.max(COMM_MIN);
    for &(state, cgroup) in &[(true, true), (true, false), (false, false)] {
        // Waited: (cell width, header width). The header's window is
        // dropped before the column is.
        for &wide in &[true, false] {
            let waited_w =
                waited.map(|(cell, head)| if wide { cell.max(head) } else { cell.max(6) });
            let mut need = PID_W + 1 + COMM_MIN;
            if state {
                need += 1 + STATE_W;
            }
            if let Some(w) = waited_w {
                need += 1 + w;
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
            return WaitCols {
                comm,
                state,
                waited: waited_w,
                cgroup,
            };
        }
    }
    WaitCols {
        comm: width.saturating_sub(PID_W + 1).max(3),
        state: false,
        waited: waited.map(|(cell, _)| cell),
        cgroup: None,
    }
}

fn push_lines(rows: &mut Vec<String>, text: &str, ink: &str, width: usize) {
    for line in wrap(text, width.saturating_sub(1)) {
        rows.push(tc::seg(&[(ink, format!(" {line}"))], width));
    }
}

/// What the section drew that the main loop steers by.
#[derive(Clone, Debug, Default)]
pub struct Painted {
    /// BY SERVICE rows in the order drawn, by cgroup path.
    pub svc_keys: Vec<String>,
    /// WAITING rows in the order drawn, by pid, then [`MORE`].
    pub wait_keys: Vec<String>,
    /// BY SERVICE rows before the top-N cut.
    pub svc_total: usize,
    /// BY PROCESS rows in the order drawn, by pid.
    pub proc_keys: Vec<String>,
    /// Every row BY PROCESS has under `a`: readable and not.
    pub proc_total: usize,
    /// BY PROCESS's length for the cursor: its rows, or 1 for the subset
    /// line when it drew none, so it takes focus on an idle host too.
    pub proc_len: usize,
}

/// The WHO IS WRITING section on the list screen.
pub fn paint_section(
    view: &View,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    placed: &mut Vec<(usize, usize)>,
) -> Painted {
    let Some(writers) = view.writers else {
        return Painted::default();
    };
    if writers.by_process_only {
        let source = writers
            .procs
            .as_ref()
            .map(|p| p.source.label())
            .unwrap_or("proc_pid_rusage");
        push_group("WHO IS WRITING", &format!("by process · {source}"), width, p, rows);
        push_note(&writers.pid_err, &p.accent, width, rows);
        let procs = by_process::paint(view, writers, width, p, rows, placed);
        return Painted {
            proc_len: procs.len(),
            proc_keys: procs.keys,
            proc_total: procs.total,
            ..Painted::default()
        };
    }
    let disk = selected_disk(view);
    let elapsed = match &writers.services {
        Services::Table(table) => Some(table.elapsed),
        _ => writers.wait_elapsed,
    };
    let mut heading = String::new();
    match disk {
        Some(row) => {
            heading.push_str(&row.disk.name);
            if let Some(e) = elapsed {
                heading.push_str(&format!(" · last {}", secs(e)));
            }
            match &row.rate {
                ShownRate::Linux(r) => heading.push_str(&format!(
                    " · {} written · {} read",
                    rate(r.write_bps),
                    rate(r.read_bps)
                )),
                ShownRate::Gap => heading.push_str(" · device counter reset —"),
                _ => heading.push_str(" · device measuring"),
            }
        }
        None => heading.push_str("no disk to describe"),
    }
    push_group("WHO IS WRITING", &heading, width, p, rows);
    let (svc_keys, svc_total) = paint_services(view, writers, disk, width, p, rows, placed);
    let procs = by_process::paint(view, writers, width, p, rows, placed);
    let wait_keys = paint_waiting(view, writers, width, p, rows, placed);
    Painted {
        svc_keys,
        wait_keys,
        svc_total,
        proc_len: procs.len(),
        proc_keys: procs.keys,
        proc_total: procs.total,
    }
}

fn paint_services(
    view: &View,
    writers: &Writers,
    disk: Option<&DiskViewRow>,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    placed: &mut Vec<(usize, usize)>,
) -> (Vec<String>, usize) {
    let focused = view.focus == 3;
    let table = match &writers.services {
        Services::Table(table) => table,
        other => {
            let line = match other {
                Services::Measuring => "cgroup v2 io.stat · measuring".to_string(),
                Services::V1 => "by service needs cgroup v2".to_string(),
                Services::NotEnabled(line) => line.clone(),
                Services::Unreadable(why) => format!("cgroup tree unreadable: {why}"),
                Services::Table(_) => unreachable!(),
            };
            push_part("BY SERVICE", "", focused, width, p, rows);
            push_note(&line, &p.txt, width, rows);
            return (Vec::new(), 0);
        }
    };
    push_part(
        "BY SERVICE",
        &format!("sorted: {}", view.svc_sort.label(false)),
        focused,
        width,
        p,
        rows,
    );
    let mut note = format!("cgroup v2 io.stat · complete · {}", table.depth);
    if table.container {
        note.push_str(" · rows are this container's cgroups; the device total is the host's");
    }
    push_note(&note, &p.dim, width, rows);
    let Some(disk) = disk else {
        return (Vec::new(), 0);
    };
    let devs = devs_of(disk);
    if devs.is_empty() {
        push_note(
            &format!("no device number for {}", disk.disk.name),
            &p.txt,
            width,
            rows,
        );
        return (Vec::new(), 0);
    }
    let all = rows_for(table, &devs, &writers.readable);
    let order = ordered_rows(view, &all);
    if order.len() < all.len() {
        let label = format!("top {} by {}", order.len(), view.svc_sort.label(false));
        if let Some(line) = tc::filter_row(order.len(), all.len(), &[label]) {
            push_note(&line, &p.dim, width, rows);
        }
    }
    let sum_write: f64 = all.iter().map(|r| r.write_bps).sum();
    let longest = order
        .iter()
        .map(|&i| tc::display_width(&shown_path(&all[i].path)))
        .max()
        .unwrap_or(6);
    let full_w = order
        .iter()
        .map(|&i| tc::display_width(&procs_text(all[i].procs, all[i].readable, true)))
        .max()
        .unwrap_or(0);
    let count_w = order
        .iter()
        .map(|&i| all[i].procs.to_string().len())
        .max()
        .unwrap_or(0);
    let cols = svc_columns(width, longest, full_w, count_w);
    let mut head = vec![
        tc::pad(&format!("{PART_INDENT}CGROUP"), cols.name),
        format!("{:>w$}", "WRITE", w = RATE_W),
    ];
    if cols.read {
        head.push(format!("{:>w$}", "READ", w = RATE_W));
    }
    if cols.share {
        head.push(format!("{:>w$}", "SHARE", w = SHARE_W));
    }
    if cols.procs {
        head.push(tc::pad("PROCS", cols.procs_w));
    }
    rows.push(tc::seg(
        &[(p.dim.as_str(), head.join(" ").trim_end().to_string())],
        width,
    ));
    let mut keys = Vec::new();
    for (item, &index) in order.iter().enumerate() {
        let row = &all[index];
        let here = view.focus == 3 && view.svc_cursor == row.path;
        let from = rows.len();
        rows.extend(service_row(row, sum_write, &cols, width, here, p));
        placed.extend((from..rows.len()).map(|at| (at, SERVICE_BASE + item)));
        keys.push(row.path.clone());
    }
    let device = match &disk.rate {
        ShownRate::Linux(r) => Some(r.write_bps),
        _ => None,
    };
    let line = match device {
        Some(device) => reconcile(sum_write, device),
        None => format!(
            "accounted: {} · the device rate is not measured yet",
            rate(sum_write)
        ),
    };
    push_note(&line, &p.txt, width, rows);
    let floored = all
        .iter()
        .filter(|r| r.inconsistent && !r.path.is_empty())
        .count();
    if floored > 0 {
        push_note(
            &format!("{floored} read below their children this sample (the walk is not atomic) and show 0"),
            &p.dim,
            width,
            rows,
        );
    }
    (keys, all.len())
}

fn service_row(
    row: &ServiceRow,
    sum_write: f64,
    cols: &SvcCols,
    width: usize,
    here: bool,
    p: &Palette,
) -> Vec<String> {
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
    let room = cols.name.saturating_sub(2).max(1);
    let pieces = chunks(&shown_path(&row.path), room);
    let mut out = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        let marker = if index == 0 && here { "▸ " } else { "  " };
        let name = tc::pad(&format!("{marker}{}", tc::pad(piece, room)), cols.name);
        if index > 0 {
            out.push(join_cells(&[(c(p.txt.as_str()), name)], width, here, &tint));
            continue;
        }
        let ink = if here {
            p.accent.as_str()
        } else {
            p.txt.as_str()
        };
        let mut cells = vec![(c(ink), name)];
        cells.push((
            c(p.txt.as_str()),
            format!("{:>w$}", rate(row.write_bps), w = RATE_W),
        ));
        if cols.read {
            cells.push((
                c(p.txt.as_str()),
                format!("{:>w$}", rate(row.read_bps), w = RATE_W),
            ));
        }
        if cols.share {
            let share = if sum_write > 0.0 {
                format!(
                    "{:.0}%",
                    (row.write_bps / sum_write * 100.0)
                        .round()
                        .clamp(0.0, 100.0)
                )
            } else {
                "—".into()
            };
            cells.push((c(p.txt.as_str()), format!("{:>w$}", share, w = SHARE_W)));
        }
        if cols.procs {
            cells.push((
                c(p.dim.as_str()),
                procs_text(row.procs, row.readable, cols.full),
            ));
        }
        out.push(join_cells(&cells, width, here, &tint));
    }
    out
}

fn paint_waiting(
    view: &View,
    writers: &Writers,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    placed: &mut Vec<(usize, usize)>,
) -> Vec<String> {
    let (shown, hidden) = shown_waiters(writers, view.show_kernel, view.wait_sort);
    rows.push(String::new());
    let (summary, note) = waiting_heading(writers, &shown, hidden, view.wait_sort);
    push_part("WAITING ON DISK", &summary, view.focus == 5, width, p, rows);
    push_note(&note, &p.dim, width, rows);
    push_note(&writers.pid_err, &p.accent, width, rows);
    if shown.is_empty() {
        return Vec::new();
    }
    let top: Vec<&Waiter> = shown.iter().take(WAIT_TOP).copied().collect();
    let cols = waiter_columns(writers, &top, width);
    rows.push(waiter_header(writers, &cols, width, p));
    let mut keys = Vec::new();
    for (item, w) in top.iter().enumerate() {
        let key = w.pid.to_string();
        let here = view.focus == 5 && view.wait_cursor == key;
        let from = rows.len();
        rows.extend(waiter_row(w, writers, &cols, width, here, p));
        placed.extend((from..rows.len()).map(|at| (at, WAIT_BASE + item)));
        keys.push(key);
    }
    if shown.len() > top.len() {
        let here = view.focus == 5 && view.wait_cursor == MORE;
        let tint = if here {
            tc::tint(tc::Role::Slot)
        } else {
            String::new()
        };
        let ink = if here {
            format!("{}{}", tint, p.lit(&p.accent))
        } else {
            p.txt.clone()
        };
        let marker = if here { "▸ " } else { "  " };
        let text = format!("{marker}+{} more  [↵] list all", shown.len() - top.len());
        let from = rows.len();
        rows.push(join_cells(&[(ink, text)], width, here, &tint));
        placed.extend((from..rows.len()).map(|at| (at, WAIT_BASE + keys.len())));
        keys.push(MORE.into());
    }
    keys
}

/// WAITING ON DISK's heading summary (how many, in what order) and the
/// note under it (whether wait times are measured, what is hidden).
fn waiting_heading(
    writers: &Writers,
    shown: &[&Waiter],
    hidden: usize,
    sort: WaitSort,
) -> (String, String) {
    let total = writers.waiters.len();
    let mut summary = if total == 0 {
        "none in D state now".to_string()
    } else {
        format!("{total} in D state now")
    };
    let mut notes: Vec<String> = Vec::new();
    if writers.wait_times {
        notes.push("wait times on (task_delayacct=1)".into());
        if !shown.is_empty() {
            summary.push_str(match sort {
                WaitSort::Waited => " · by time waited",
                WaitSort::Pid => " · in pid order",
            });
        }
    } else {
        notes.push("wait times off (needs root: sysctl kernel.task_delayacct=1)".into());
        if shown.len() > WAIT_TOP {
            summary.push_str(" · the first by pid, a sample");
        }
    }
    if hidden > 0 {
        notes.push(format!(
            "{hidden} kernel thread{} hidden",
            if hidden == 1 { "" } else { "s" }
        ));
    }
    (summary, notes.join(" · "))
}

fn waiter_columns(writers: &Writers, list: &[&Waiter], width: usize) -> WaitCols {
    let comm_want = list
        .iter()
        .map(|w| tc::display_width(&w.comm))
        .max()
        .unwrap_or(0);
    let cgroup_want = list
        .iter()
        .map(|w| tc::display_width(&shown_path(&w.cgroup)))
        .max()
        .unwrap_or(0);
    let waited = writers.wait_times.then(|| {
        let cell = list
            .iter()
            .map(|w| waited_text(w.waited, writers.wait_elapsed).chars().count())
            .max()
            .unwrap_or(9);
        (cell, waited_header(writers, true).chars().count())
    });
    wait_columns(width, comm_want, cgroup_want, waited)
}

fn waited_header(writers: &Writers, wide: bool) -> String {
    match (wide, writers.wait_elapsed) {
        (true, Some(e)) => format!("WAITED (last {})", secs(e)),
        _ => "WAITED".into(),
    }
}

fn waiter_header(writers: &Writers, cols: &WaitCols, width: usize, p: &Palette) -> String {
    let mut cells = vec![tc::pad("  PID", PID_W), tc::pad("COMMAND", cols.comm)];
    if cols.state {
        cells.push(tc::pad("STATE", STATE_W));
    }
    if let Some(w) = cols.waited {
        let wide = waited_header(writers, true);
        let label = if wide.chars().count() <= w {
            wide
        } else {
            waited_header(writers, false)
        };
        cells.push(tc::pad(&label, w));
    }
    if let Some(w) = cols.cgroup {
        cells.push(tc::pad("CGROUP", w));
    }
    tc::seg(
        &[(p.dim.as_str(), cells.join(" ").trim_end().to_string())],
        width,
    )
}

fn waiter_row(
    w: &Waiter,
    writers: &Writers,
    cols: &WaitCols,
    width: usize,
    here: bool,
    p: &Palette,
) -> Vec<String> {
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
    let comm = chunks(&w.comm, cols.comm);
    let cgroup = cols
        .cgroup
        .map(|room| chunks(&shown_path(&w.cgroup), room))
        .unwrap_or_default();
    let lines = comm.len().max(cgroup.len()).max(1);
    let mut out = Vec::new();
    for line in 0..lines {
        let first = line == 0;
        let marker = if first && here { "▸ " } else { "  " };
        let pid = if first {
            w.pid.to_string()
        } else {
            String::new()
        };
        let ink = if here && first {
            p.accent.as_str()
        } else {
            p.txt.as_str()
        };
        let mut cells = vec![
            (c(ink), tc::pad(&format!("{marker}{pid}"), PID_W)),
            (
                c(p.txt.as_str()),
                tc::pad(comm.get(line).map(String::as_str).unwrap_or(""), cols.comm),
            ),
        ];
        if cols.state {
            cells.push((
                c(p.txt.as_str()),
                tc::pad(if first { "D" } else { "" }, STATE_W),
            ));
        }
        if let Some(width) = cols.waited {
            let text = if first {
                waited_text(w.waited, writers.wait_elapsed)
            } else {
                String::new()
            };
            cells.push((c(p.txt.as_str()), tc::pad(&text, width)));
        }
        if let Some(room) = cols.cgroup {
            cells.push((
                c(p.dim.as_str()),
                tc::pad(cgroup.get(line).map(String::as_str).unwrap_or(""), room),
            ));
        }
        out.push(join_cells(&cells, width, here, &tint));
    }
    out
}

/// `↵` on `+N more`: everyone in D, in the section's order, no cap.
pub fn paint_waiters(view: &View, width: usize, p: &Palette, rows: &mut Vec<String>) {
    let Some(writers) = view.writers else {
        push_lines(rows, "WAITING ON DISK  not sampled yet", &p.txt, width);
        return;
    };
    let (shown, hidden) = shown_waiters(writers, view.show_kernel, view.wait_sort);
    let (summary, note) = waiting_heading(writers, &shown, hidden, view.wait_sort);
    push_part("WAITING ON DISK", &summary, false, width, p, rows);
    push_note(&note, &p.dim, width, rows);
    if shown.is_empty() {
        push_note("nothing is in D state now", &p.txt, width, rows);
        return;
    }
    let cols = waiter_columns(writers, &shown, width);
    rows.push(waiter_header(writers, &cols, width, p));
    for w in shown {
        rows.extend(waiter_row(w, writers, &cols, width, false, p));
    }
}

/// What `↵` on a WAITING or BY PROCESS row opened: the pid, and what was
/// known about it then. The poller reads it again every poll from then on.
#[derive(Clone, Debug, Default)]
pub struct Opened {
    pub pid: i32,
    pub comm: String,
    /// WAITING: the process's own cgroup. BY PROCESS: the BY SERVICE row
    /// its I/O lands in, as drawn. Empty when the walk did not place it.
    pub cgroup: String,
    pub cmdline: String,
    pub cwd: String,
    /// Opened from WAITING ON DISK, so the state line talks about D.
    pub from_wait: bool,
    /// The pid's start time when it was opened. `None` when the pass gave
    /// none, and then only the pid is followed.
    pub start: Option<u64>,
}

/// The title of the process detail: `disk · <comm> · pid <n>` when the top
/// row has the room, `disk · pid <n>` when only that fits, else `disk`.
/// The second half says whether both comm and pid made it, so the body
/// names whatever the title had to leave out.
pub fn process_title(open: &Opened, width: usize) -> (String, bool) {
    let full = format!("disk · {} · pid {}", open.comm, open.pid);
    if title_fits(&full, width) {
        return (full, true);
    }
    let short = format!("disk · pid {}", open.pid);
    if title_fits(&short, width) {
        return (short, false);
    }
    ("disk".into(), false)
}

/// Whether core drew all of `text` on a top row `width` wide, leaving the
/// room it keeps for the version and the notice counter. Measured with the
/// refresh spinner showing: it spends only rule fill, never title room.
fn title_fits(text: &str, width: usize) -> bool {
    let row = tc::title_busy(text, width, "", Some(0));
    visible(&row).contains(&format!(" {} ", text.to_uppercase()))
}

/// `line` without its colour escapes.
pub fn visible(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// `stat`'s state letter, said in words.
fn state_words(state: char) -> String {
    let words = match state {
        'R' => "running",
        'S' => "sleeping",
        'D' => "waiting on disk",
        'I' => "idle kernel thread",
        'T' => "stopped",
        't' => "stopped by a debugger",
        'Z' => "zombie, exited and not yet reaped",
        'X' => "dead",
        'P' => "parked",
        _ => return state.to_string(),
    };
    format!("{state} · {words}")
}

/// The width of the PROCESS block's key column, `since start` and a gap.
const KEY_W: usize = 13;
const KEY_INDENT: usize = 3;
/// Rows of the write-rate history chart.
const HISTORY_ROWS: usize = 3;

/// One `key  value` row of the PROCESS block. A value that does not fit
/// wraps and its continuation lines sit under the value column; on a pane
/// too narrow for both, the value goes under its key.
fn push_field(
    rows: &mut Vec<String>,
    key: &str,
    value: &str,
    ink: &str,
    width: usize,
    p: &Palette,
) {
    let value_w = width.saturating_sub(KEY_INDENT + KEY_W);
    if value_w >= 16 {
        for (i, line) in wrap(value, value_w).into_iter().enumerate() {
            let label = if i == 0 { key } else { "" };
            rows.push(tc::seg(
                &[
                    (
                        p.lbl.as_str(),
                        format!("{}{}", " ".repeat(KEY_INDENT), tc::pad(label, KEY_W)),
                    ),
                    (ink, line),
                ],
                width,
            ));
        }
        return;
    }
    rows.push(tc::seg(
        &[(
            p.lbl.as_str(),
            format!(
                "{}{key}",
                " ".repeat(KEY_INDENT.min(width.saturating_sub(1)))
            ),
        )],
        width,
    ));
    let indent = (KEY_INDENT + 2).min(width / 3);
    for line in wrap(value, width.saturating_sub(indent)) {
        rows.push(tc::seg(
            &[(ink, format!("{}{line}", " ".repeat(indent)))],
            width,
        ));
    }
}

/// The opened process's write rate over the last minute: a line chart
/// drawn by core (pixels in Kitty, Braille elsewhere) with its peak and
/// average beside it when the pane has the room, under it when not.
fn push_history(
    points: &[by_process::RatePoint],
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    plots: &mut Vec<tc::Plot>,
) {
    let Some(stats) = by_process::history_stats(points) else {
        push_lines(
            rows,
            "write history · from the next poll (a rate needs two samples)",
            &p.dim,
            width,
        );
        return;
    };
    let covered = stats.seconds.round().min(by_process::HISTORY_SECONDS);
    let mut labels = vec![format!(
        "write · last {} · peak {} · avg {}",
        secs(covered),
        rate(stats.write_peak),
        rate(stats.write_avg)
    )];
    if stats.read_peak > 0.0 {
        labels.push(format!(
            "read · peak {} · avg {}",
            rate(stats.read_peak),
            rate(stats.read_avg)
        ));
    }
    let label_w = labels
        .iter()
        .map(|l| tc::display_width(l))
        .max()
        .unwrap_or(0);
    // Beside the chart when that still leaves it twenty cells.
    let beside = width >= 60 && label_w + 4 + 20 <= width;
    let chart_w = if beside {
        width - label_w - 4
    } else {
        width.saturating_sub(2).max(1)
    };
    let peak = stats.write_peak;
    let values: Vec<Option<f64>> = points
        .iter()
        .map(|pt| {
            Some(if peak > 0.0 {
                (pt.write / peak).clamp(0.0, 1.0)
            } else {
                0.0
            })
        })
        .collect();
    let positions = by_process::history_positions(points, by_process::HISTORY_SECONDS);
    let y = rows.len();
    for i in 0..HISTORY_ROWS {
        let mut parts = vec![(p.dim.as_str(), " ".repeat(1 + chart_w))];
        if beside {
            if let Some(label) = labels.get(i) {
                parts.push((p.dim.as_str(), format!("  {label}")));
            }
        }
        rows.push(tc::seg(&parts, width));
    }
    let slots = values.len().max(1);
    plots.push(
        tc::Plot::new(
            1,
            y,
            chart_w,
            HISTORY_ROWS,
            tc::LineChart {
                traces: vec![tc::Trace {
                    values,
                    positions: Some(positions),
                    colour: p.accent.clone(),
                    baseline: Some(0.0),
                }],
                slots,
                focus: Some(0),
            },
        )
        .under_text()
        .on_axis(0.0, peak),
    );
    if !beside {
        for label in labels {
            push_lines(rows, &label, &p.dim, width);
        }
    }
}

/// `↵` on a BY PROCESS or WAITING ON DISK row. What a reader watches comes
/// first - the rates, their last minute, the files growing - and what the
/// process is comes after, in an aligned block.
pub fn paint_process(
    view: &View,
    open: &Opened,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    plots: &mut Vec<tc::Plot>,
) {
    let home = std::env::var("HOME").unwrap_or_default();
    paint_process_in(view, open, &home, width, p, rows, plots);
}

fn paint_process_in(
    view: &View,
    open: &Opened,
    home: &str,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    plots: &mut Vec<tc::Plot>,
) {
    let readable = view.writers.is_some_and(|w| w.readable.contains(&open.pid));
    // The last pass did not see it: it has exited, and an empty cmdline or
    // cwd says nothing about what this user could read.
    // A pid that started again since is a different process: also gone.
    let gone = view.writers.is_some_and(|w| {
        !w.names.contains_key(&open.pid)
            || open.start.is_some_and(|start| w.starts.get(&open.pid) != Some(&start))
    });
    let live = view
        .writers
        .and_then(|w| w.waiters.iter().find(|x| x.pid == open.pid))
        .filter(|_| !gone);
    // Read again every poll while this screen is open.
    let detail = view
        .writers
        .and_then(|w| w.detail.as_ref())
        .filter(|d| d.pid == open.pid && !gone);
    let mac = view.writers.is_some_and(|w| w.by_process_only);
    let row = view
        .writers
        .and_then(|w| w.procs.as_ref())
        .and_then(|procs| {
            procs
                .rows
                .iter()
                .find(|r| r.pid == open.pid)
                .map(|r| (r, procs.elapsed, procs.source))
        })
        .filter(|_| !gone);

    // What the title could not hold.
    if !process_title(open, width).1 {
        push_lines(
            rows,
            &format!("{} · pid {}", open.comm, open.pid),
            &p.txt,
            width,
        );
    }

    // The headline: the rates now and the state, in warning ink when D.
    let state: Option<(String, bool)> = match (live, gone, open.from_wait) {
        (Some(_), _, _) => Some(("D · waiting on disk now".into(), true)),
        (None, true, true) => Some(("exited since it was in D".into(), false)),
        (None, true, false) => Some(("exited".into(), false)),
        (None, false, true) => Some(("no longer D".into(), false)),
        (None, false, false) => detail
            .and_then(|d| d.state)
            .map(|s| (state_words(s), s == 'D')),
    };
    let rates = match row {
        Some((r, elapsed, _)) => Some(match (r.write_bps, r.read_bps, elapsed) {
            (Some(w), Some(rd), Some(e)) => format!(
                "writing {} · reading {} over the last {}",
                rate(w),
                rate(rd),
                secs(e)
            ),
            _ => "measuring · a rate needs two samples".into(),
        }),
        None if !readable && !gone => Some("io not readable as you · the rest needs root".into()),
        None => None,
    };
    let state_ink = |warn: bool| {
        if warn {
            p.down.as_str()
        } else {
            p.txt.as_str()
        }
    };
    match (&rates, &state) {
        (Some(r), Some((s, warn)))
            if 1 + tc::display_width(r) + 3 + tc::display_width(s) <= width =>
        {
            rows.push(tc::seg(
                &[
                    (p.txt.as_str(), format!(" {r} · ")),
                    (state_ink(*warn), s.clone()),
                ],
                width,
            ));
        }
        _ => {
            if let Some(r) = &rates {
                push_lines(rows, r, &p.txt, width);
            }
            if let Some((s, warn)) = &state {
                push_lines(rows, s, state_ink(*warn), width);
            }
        }
    }

    // The last minute of the write rate.
    if row.is_some() {
        let points = detail.map(|d| d.history.as_slice()).unwrap_or(&[]);
        push_history(points, width, p, rows, plots);
    }

    // OPEN FILES.
    if let Some(files) = detail.and_then(|d| d.files.as_ref()) {
        rows.push(String::new());
        match files {
            Ok(list) if list.is_empty() => {
                push_part("OPEN FILES", "none open", false, width, p, rows);
            }
            Ok(list) => {
                let growing = list
                    .iter()
                    .filter(|f| matches!(f.change, by_process::Change::Grew(..)))
                    .count();
                let span = list.iter().find_map(|f| f.change.span());
                let since = match span {
                    Some(s) => format!("growth since the previous sample ({})", secs(s)),
                    None => format!("growth from the next sample, every {}", secs(view.every)),
                };
                push_part(
                    "OPEN FILES",
                    &format!("{} open · {growing} growing · {since}", list.len()),
                    false,
                    width,
                    p,
                    rows,
                );
                by_process::paint_files(list, home, width, p, rows);
            }
            Err(why) if !readable => push_part(
                "OPEN FILES",
                &format!("{why} · the rest needs root"),
                false,
                width,
                p,
                rows,
            ),
            Err(why) => push_part("OPEN FILES", why, false, width, p, rows),
        }
    }

    // PROCESS: what it is.
    rows.push(String::new());
    push_part("PROCESS", "", false, width, p, rows);
    if let Some((s, warn)) = &state {
        push_field(rows, "state", s, state_ink(*warn), width, p);
    }
    if let (Some(w), Some(writers)) = (live, view.writers) {
        if writers.wait_times {
            push_field(
                rows,
                "waited",
                &waited_text(w.waited, writers.wait_elapsed),
                &p.txt,
                width,
                p,
            );
        }
    }
    let cmdline = detail.map_or(open.cmdline.as_str(), |d| d.cmdline.as_str());
    let cwd = detail.map_or(open.cwd.as_str(), |d| d.cwd.as_str());
    if !mac {
        if gone && cmdline.is_empty() && cwd.is_empty() {
            push_field(
                rows,
                "command",
                "it had exited before it was read",
                &p.dim,
                width,
                p,
            );
        } else {
            match cmdline {
                "" => push_field(
                    rows,
                    "command",
                    &format!("{} · cmdline empty (a kernel thread has none)", open.comm),
                    &p.txt,
                    width,
                    p,
                ),
                line => push_field(rows, "command", line, &p.txt, width, p),
            }
            match cwd {
                "" => push_field(rows, "cwd", "not readable as you", &p.dim, width, p),
                dir => push_field(rows, "cwd", &by_process::tilde(dir, home), &p.txt, width, p),
            }
        }
        if open.cgroup.is_empty() && !open.from_wait {
            push_field(
                rows,
                "cgroup",
                "not placed by the last walk (it started after it)",
                &p.dim,
                width,
                p,
            );
        } else {
            push_field(rows, "cgroup", &shown_path(&open.cgroup), &p.txt, width, p);
        }
    } else {
        push_field(rows, "command", &open.comm, &p.txt, width, p);
    }
    match row {
        Some((r, _, source)) => {
            let what = match source {
                by_process::Source::Procfs => "write_bytes less cancelled_write_bytes",
                by_process::Source::Rusage => "bytes handed to the disk layer",
            };
            push_field(
                rows,
                "since start",
                &format!(
                    "{} written · {} read ({what})",
                    human(r.write_total),
                    human(r.read_total)
                ),
                &p.txt,
                width,
                p,
            );
        }
        None if !readable && !gone => push_field(
            rows,
            "since start",
            "io not readable as you · the rest needs root",
            &p.dim,
            width,
            p,
        ),
        None => {}
    }
    if mac {
        push_lines(
            rows,
            "cmdline, cwd and open files are not read on macOS",
            &p.dim,
            width,
        );
    }
}

pub fn paint_service(view: &View, path: &str, width: usize, p: &Palette, rows: &mut Vec<String>) {
    let Some(writers) = view.writers else {
        push_lines(rows, "not sampled yet", &p.txt, width);
        return;
    };
    let Services::Table(table) = &writers.services else {
        push_lines(rows, "by service is not available now", &p.txt, width);
        return;
    };
    let mut lines = vec![format!("cgroup /{}", path)];
    match table.enabled.get(path) {
        Some(on) => lines.push(format!("controllers enabled here {}", on.join(" "))),
        None => lines.push("this cgroup has gone".into()),
    }
    match writers.pressure.as_ref().filter(|(at, _)| at == path) {
        Some((_, Ok(pr))) => lines.push(format!(
            "io.pressure some {:.1}% avg10 · {:.1}% avg60 · {:.1}% avg300   full {:.1}% · {:.1}% · {:.1}%",
            pr.some[0], pr.some[1], pr.some[2], pr.full[0], pr.full[1], pr.full[2]
        )),
        Some((_, Err(why))) => lines.push(format!("io.pressure {why}")),
        None => lines.push("io.pressure reading".into()),
    }
    let rolled = table.rolled.get(path).copied().unwrap_or(0);
    lines.push(if path.is_empty() {
        "the root: its own row is I/O no child cgroup was charged for".into()
    } else if rolled > 0 {
        format!(
            "IOAccounting is on here; {rolled} {} below without it {} up into this row · systemctl set-property <unit> IOAccounting=yes gives one its own",
            if rolled == 1 { "cgroup" } else { "cgroups" },
            if rolled == 1 { "rolls" } else { "roll" }
        )
    } else {
        "IOAccounting is on here".into()
    });
    if let Some(disk) = selected_disk(view) {
        let devs = devs_of(disk);
        if let Some(row) = rows_for(table, &devs, &writers.readable)
            .into_iter()
            .find(|r| r.path == path)
        {
            lines.push(format!(
                "on {} over the last {}: write {} · read {}",
                disk.disk.name,
                secs(table.elapsed),
                rate(row.write_bps),
                rate(row.read_bps)
            ));
        } else {
            lines.push(format!(
                "no I/O on {} over the last {}",
                disk.disk.name,
                secs(table.elapsed)
            ));
        }
    }
    for line in lines {
        push_lines(rows, &line, &p.txt, width);
    }
    let pids = table.members.get(path).cloned().unwrap_or_default();
    let readable = pids.iter().filter(|p| writers.readable.contains(p)).count();
    push_lines(
        rows,
        &format!("cgroup.procs {} · {} readable as you", pids.len(), readable),
        &p.lbl,
        width,
    );
    // Per-process rates for the pids this user can read, from the same pass
    // as BY PROCESS. They are every disk's, not only this one's: a
    // process's counters are not kept per device.
    let rates: HashMap<i32, &by_process::ProcRate> = writers
        .procs
        .as_ref()
        .map(|procs| procs.rows.iter().map(|r| (r.pid, r)).collect())
        .unwrap_or_default();
    if readable > 0 {
        push_lines(
            rows,
            "per process: write and read over the last poll, every disk (a process's counters are not kept per device)",
            &p.dim,
            width,
        );
    }
    for pid in pids {
        let name = writers
            .names
            .get(&pid)
            .map(String::as_str)
            .unwrap_or("gone");
        let can = match (writers.readable.contains(&pid), rates.get(&pid)) {
            (true, Some(r)) => match (r.write_bps, r.read_bps) {
                (Some(w), Some(rd)) => format!("write {} · read {}", rate(w), rate(rd)),
                _ => "readable · measuring".into(),
            },
            (true, None) => "readable".into(),
            (false, _) => "not readable".into(),
        };
        push_lines(rows, &format!("{pid}  {name}  {can}"), &p.txt, width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tc::cgroup::Node;

    fn io(dev: &str, wbytes: u64, rbytes: u64) -> IoStat {
        IoStat {
            dev: dev.into(),
            wbytes,
            rbytes,
            ..IoStat::default()
        }
    }

    fn node(path: &str, io_on: bool, lines: Option<Vec<IoStat>>, procs: &[i32]) -> Node {
        Node {
            path: path.into(),
            enabled: if io_on {
                vec!["io".into()]
            } else {
                vec!["memory".into()]
            },
            io: lines,
            cpu: None,
            memory: None,
            procs: Some(procs.to_vec()),
        }
    }

    /// The root, two slices and one service under system.slice, all with
    /// io; a session scope under user.slice without it.
    fn tree(root_w: u64, sys_w: u64, db_w: u64, user_w: u64) -> Tree {
        Tree {
            nodes: vec![
                node("", true, Some(vec![io("8:0", root_w, 0)]), &[1]),
                node("system.slice", true, Some(vec![io("8:0", sys_w, 0)]), &[]),
                node(
                    "system.slice/example-db.service",
                    true,
                    Some(vec![io("8:0", db_w, 0), io("8:1", 0, 0)]),
                    &[100, 101],
                ),
                node("user.slice", true, Some(vec![io("8:0", user_w, 10)]), &[]),
                node("user.slice/session-1.scope", false, None, &[200, 201, 202]),
            ],
        }
    }

    fn devs(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn table_of(prev: &Tree, next: &Tree, elapsed: f64) -> Table {
        table(prev, next, elapsed, false)
    }

    #[test]
    fn a_parent_and_its_children_with_io_are_not_counted_twice() {
        // Over 2 s: root +100, system.slice +80 of which the service +70,
        // user.slice +15. Hierarchical, so the own shares are root 5,
        // system.slice 10, the service 70, user.slice 15: 100 in all.
        let prev = tree(1000, 800, 500, 100);
        let next = tree(1100, 880, 570, 115);
        let t = table_of(&prev, &next, 2.0);
        let rows = rows_for(&t, &devs(&["8:0"]), &BTreeSet::new());
        let sum: f64 = rows.iter().map(|r| r.write_bps).sum();
        assert_eq!(sum, 50.0, "the root's 100 bytes over 2 s, once");
        let by = |p: &str| rows.iter().find(|r| r.path == p).map(|r| r.write_bps);
        assert_eq!(by("system.slice/example-db.service"), Some(35.0));
        assert_eq!(by("system.slice"), Some(5.0));
        assert_eq!(by("user.slice"), Some(7.5));
        assert_eq!(by(""), Some(2.5), "the root's own I/O is a row");
        assert_eq!(shown_path(""), "/");
    }

    #[test]
    fn rows_sum_to_the_device_within_a_named_remainder() {
        let prev = tree(1000, 800, 500, 100);
        let next = tree(1100, 880, 570, 115);
        let t = table_of(&prev, &next, 2.0);
        let rows = rows_for(&t, &devs(&["8:0"]), &BTreeSet::new());
        let sum: f64 = rows.iter().map(|r| r.write_bps).sum();
        // The device moved 4 MiB/s more than every cgroup together.
        let line = reconcile(sum, sum + 4.0 * 1024.0 * 1024.0);
        assert!(line.starts_with("accounted: 50B/s of 4M/s"), "{line}");
        assert!(line.contains(
            "the rest, 4M/s, is writeback on filesystems without cgroup writeback, or root's own"
        ));
        assert_eq!(reconcile(sum, sum), "accounted: 50B/s of 50B/s");
        assert_eq!(reconcile(0.0, 0.0), "accounted: 0B/s of 0B/s");
    }

    #[test]
    fn a_sum_above_the_device_says_dirtying_runs_ahead() {
        let line = reconcile(203.0 * 1048576.0, 188.0 * 1048576.0);
        assert_eq!(
            line,
            "cgroups report 203M/s against 188M/s at the device · dirtying runs ahead of submission"
        );
        assert!(!line.contains('%'));
        // A megabyte over on 418 is the two reads, not a cause.
        assert_eq!(
            reconcile(419.0 * 1048576.0, 418.0 * 1048576.0),
            "accounted: 419M/s of 418M/s · within 2%: the device and the cgroups are read a moment apart"
        );
        // Within rounding of the device, it is accounted for and no more.
        assert!(reconcile(188.2 * 1048576.0, 188.0 * 1048576.0)
            .starts_with("accounted: 188M/s of 188M/s"));
    }

    #[test]
    fn a_partition_counts_toward_its_disk_and_another_disk_does_not() {
        let prev = Tree {
            nodes: vec![
                node("", true, Some(vec![io("8:1", 0, 0), io("8:16", 0, 0)]), &[]),
                node(
                    "system.slice",
                    true,
                    Some(vec![io("8:1", 0, 0), io("8:16", 0, 0)]),
                    &[],
                ),
            ],
        };
        let next = Tree {
            nodes: vec![
                node(
                    "",
                    true,
                    Some(vec![io("8:1", 40, 0), io("8:16", 1000, 0)]),
                    &[],
                ),
                node(
                    "system.slice",
                    true,
                    Some(vec![io("8:1", 40, 0), io("8:16", 1000, 0)]),
                    &[],
                ),
            ],
        };
        let t = table_of(&prev, &next, 1.0);
        let rows = rows_for(&t, &devs(&["8:0", "8:1"]), &BTreeSet::new());
        assert_eq!(rows.len(), 1, "the root's own is zero, so it is not a row");
        assert_eq!(rows[0].write_bps, 40.0);
    }

    #[test]
    fn procs_roll_up_and_say_how_many_are_readable() {
        let prev = tree(1000, 800, 500, 100);
        let next = tree(1100, 880, 570, 115);
        let t = table_of(&prev, &next, 2.0);
        let readable: BTreeSet<i32> = [100, 200, 201].into_iter().collect();
        let rows = rows_for(&t, &devs(&["8:0"]), &readable);
        let user = rows.iter().find(|r| r.path == "user.slice").unwrap();
        assert_eq!(
            (user.procs, user.readable),
            (3, 2),
            "the session scope rolls up"
        );
        assert_eq!(t.rolled.get("user.slice"), Some(&1));
        let db = rows
            .iter()
            .find(|r| r.path.ends_with("example-db.service"))
            .unwrap();
        assert_eq!((db.procs, db.readable), (2, 1));
        assert_eq!(procs_text(3, 2, true), "3 · 2 readable");
        assert_eq!(procs_text(12, 0, true), "12 · none readable as you");
        assert_eq!(procs_text(1, 0, true), "1 · not readable");
        assert_eq!(procs_text(4, 4, true), "4 · all readable");
        assert_eq!(procs_text(4, 0, false), "4");
    }

    #[test]
    fn a_removed_child_does_not_hand_its_lifetime_to_its_parent() {
        // A scope with io wrote 900 bytes in its life and went. user.slice's
        // hierarchical counter still holds them, but over this interval it
        // moved only 10.
        let mut prev = tree(3000, 800, 500, 1000);
        prev.nodes.push(node(
            "user.slice/run-1.scope",
            true,
            Some(vec![io("8:0", 900, 0)]),
            &[],
        ));
        let next = tree(3010, 800, 500, 1010);
        let t = table_of(&prev, &next, 1.0);
        let rows = rows_for(&t, &devs(&["8:0"]), &BTreeSet::new());
        let user = rows.iter().find(|r| r.path == "user.slice").unwrap();
        assert_eq!(user.write_bps, 10.0);
    }

    #[test]
    fn a_new_cgroup_counts_from_zero_and_a_reset_is_not_negative() {
        let prev = tree(1000, 800, 500, 100);
        let mut next = tree(1100, 880, 20, 115);
        next.nodes.push(node(
            "system.slice/example-log.service",
            true,
            Some(vec![io("8:0", 30, 0)]),
            &[300],
        ));
        let delta = delta_tree(&prev, &next);
        let db = delta
            .nodes
            .iter()
            .find(|n| n.path.ends_with("example-db.service"))
            .unwrap();
        assert_eq!(
            db.io.as_ref().unwrap()[0].wbytes,
            20,
            "recreated: counted from zero"
        );
        let log = delta
            .nodes
            .iter()
            .find(|n| n.path.ends_with("example-log.service"))
            .unwrap();
        assert_eq!(log.io.as_ref().unwrap()[0].wbytes, 30);
    }

    #[test]
    fn the_depth_line_and_the_named_states() {
        let root_only = Tree {
            nodes: vec![
                node("", true, Some(vec![io("8:0", 1, 0)]), &[]),
                node("system.slice", false, None, &[]),
            ],
        };
        assert_eq!(
            cgroup::depth_sentence(&root_only, "io"),
            "io accounting at the root only; everything rolls up to it"
        );
        assert_eq!(
            cgroup::depth_sentence(&tree(0, 0, 0, 0), "io"),
            "io accounting down to: services; user sessions roll up to user.slice"
        );
        let mut memory = Memory::default();
        let w = step(
            &mut memory,
            State::NotEnabled("io".into()),
            vec![],
            String::new(),
            false,
            1.0,
            100.0,
        );
        let Services::NotEnabled(line) = &w.services else {
            panic!("{:?}", w.services)
        };
        assert_eq!(
            line,
            "io accounting is off everywhere · systemctl set-property <unit> IOAccounting=yes enables it per unit"
        );
        let w = step(
            &mut memory,
            State::V1,
            vec![],
            String::new(),
            false,
            2.0,
            100.0,
        );
        assert!(matches!(w.services, Services::V1));
        let w = step(
            &mut memory,
            State::Unreadable("/sys/fs/cgroup: denied".into()),
            vec![],
            String::new(),
            false,
            3.0,
            100.0,
        );
        assert!(matches!(w.services, Services::Unreadable(ref why) if why.contains("denied")));
    }

    #[test]
    fn the_first_walk_is_measuring_and_the_second_has_rates() {
        let mut memory = Memory::default();
        let w = step(
            &mut memory,
            State::Ok(tree(1000, 800, 500, 100)),
            vec![],
            String::new(),
            false,
            10.0,
            100.0,
        );
        assert!(matches!(w.services, Services::Measuring));
        let w = step(
            &mut memory,
            State::Container(tree(1100, 880, 570, 115)),
            vec![],
            String::new(),
            false,
            12.0,
            100.0,
        );
        let Services::Table(t) = &w.services else {
            panic!()
        };
        assert!(t.container);
        assert_eq!(t.elapsed, 2.0);
    }

    pub(crate) fn sample(
        pid: i32,
        ppid: i32,
        state: char,
        ticks: Option<u64>,
        readable: bool,
    ) -> PidSample {
        PidSample {
            pid,
            comm: format!("example-{pid}"),
            state,
            ppid,
            starttime: 7,
            blkio_ticks: ticks,
            io: readable.then(PidIo::default),
            cgroup: Some("/system.slice/example-db.service".into()),
        }
    }

    #[test]
    fn wait_times_come_from_a_field_42_delta_when_delayacct_is_on() {
        let mut memory = Memory::default();
        let first = vec![
            sample(1204, 1, 'D', Some(100), false),
            sample(48990, 1, 'S', Some(10), true),
        ];
        let w = step(
            &mut memory,
            State::V1,
            first,
            String::new(),
            true,
            0.0,
            100.0,
        );
        assert!(w.wait_times);
        assert_eq!(
            w.waiters[0].waited, None,
            "a first sample has nothing to subtract"
        );
        let second = vec![
            sample(1204, 1, 'D', Some(280), false),
            sample(48990, 1, 'D', Some(100), true),
        ];
        let w = step(
            &mut memory,
            State::V1,
            second,
            String::new(),
            true,
            2.0,
            100.0,
        );
        let by = |pid| w.waiters.iter().find(|x| x.pid == pid).unwrap();
        assert_eq!(by(1204).waited, Some(1.8));
        assert_eq!(by(48990).waited, Some(0.9));
        assert_eq!(waited_text(by(1204).waited, w.wait_elapsed), "1.8s of 2s");
        let (shown, _) = shown_waiters(&w, false, WaitSort::Waited);
        assert_eq!(shown[0].pid, 1204, "longest wait first");
    }

    #[test]
    fn delayacct_off_or_no_field_42_is_wait_times_off() {
        let mut memory = Memory::default();
        let w = step(
            &mut memory,
            State::V1,
            vec![sample(5, 1, 'D', Some(3), true)],
            String::new(),
            false,
            0.0,
            100.0,
        );
        assert!(!w.wait_times);
        let w = step(
            &mut memory,
            State::V1,
            vec![sample(5, 1, 'D', None, true)],
            String::new(),
            true,
            1.0,
            100.0,
        );
        assert!(!w.wait_times, "a kernel without field 42 behaves as 0");
        assert!(w.waiters.iter().all(|x| x.waited.is_none()));
        assert!(parse_task_delayacct("1\n"));
        assert!(!parse_task_delayacct("0\n"));
        assert!(!parse_task_delayacct(""));
    }

    #[test]
    fn kernel_threads_are_hidden_and_counted_and_every_pid_has_a_cgroup() {
        let mut memory = Memory::default();
        let samples = vec![
            sample(9, 2, 'D', None, false),
            sample(2, 0, 'D', None, false),
            sample(30, 1, 'D', None, false),
            sample(31, 1, 'D', None, true),
        ];
        let w = step(
            &mut memory,
            State::V1,
            samples,
            String::new(),
            false,
            0.0,
            100.0,
        );
        let (shown, hidden) = shown_waiters(&w, false, WaitSort::Waited);
        assert_eq!(hidden, 2);
        assert_eq!(
            shown.iter().map(|x| x.pid).collect::<Vec<_>>(),
            vec![30, 31]
        );
        assert!(shown
            .iter()
            .all(|x| x.cgroup == "system.slice/example-db.service"));
        let (shown, hidden) = shown_waiters(&w, true, WaitSort::Waited);
        assert_eq!((shown.len(), hidden), (4, 0));
    }

    #[test]
    fn a_reused_pid_is_not_subtracted_from_a_stranger() {
        let mut memory = Memory::default();
        step(
            &mut memory,
            State::V1,
            vec![sample(7, 1, 'D', Some(500), true)],
            String::new(),
            true,
            0.0,
            100.0,
        );
        let mut again = sample(7, 1, 'D', Some(20), true);
        again.starttime = 99;
        let w = step(
            &mut memory,
            State::V1,
            vec![again],
            String::new(),
            true,
            2.0,
            100.0,
        );
        assert_eq!(w.waiters[0].waited, None);
    }

    #[test]
    fn the_hints_name_the_next_state() {
        assert_eq!(all_hint(false, 8), "[a] show all cgroups");
        assert_eq!(all_hint(true, 8), "[a] show top 8");
        assert_eq!(WaitSort::Waited.hint(), "[s] sort by pid");
        assert_eq!(WaitSort::Pid.hint(), "[s] sort by waited");
    }
}
