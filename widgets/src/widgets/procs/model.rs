// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! What one poll found, in a shape both hosts fill and the view draws.
//!
//! Nothing here reads a file or runs a command: `linux.rs` and `macos.rs`
//! build a [`Sample`], and the sort, the filter and the parent chain are
//! the same code on both, tested with hand-built processes.

use std::collections::HashMap;

/// One process as one poll read it.
#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub pid: i32,
    pub ppid: i32,
    /// The short name: `comm` on Linux, the executable's name on macOS.
    pub comm: String,
    pub uid: u32,
    /// The login name, or `uid N` where none is known.
    pub user: String,
    /// % of all cores. `None` until a process has been seen twice (Linux
    /// measures a delta) - never a zero standing in for "not yet".
    pub cpu: Option<f64>,
    /// Resident bytes.
    pub rss: u64,
    /// RSS as a % of the machine's memory; `None` when the total is not
    /// known.
    pub mem: Option<f64>,
    /// `R S D Z T`, and whatever else the kernel prints.
    pub state: char,
    /// Seconds since it started.
    pub running: Option<f64>,
    /// The cgroup path (Linux). `None` where there are no cgroups.
    pub cgroup: Option<String>,
    /// A kernel thread (pid 2, or a child of it).
    pub kernel: bool,
    /// Identity across polls beside the pid: `starttime` on Linux, so a
    /// pid reused between two polls is not the same process.
    pub start: Option<u64>,
    pub threads: Option<i64>,
    pub nice: Option<i64>,
    /// User plus system CPU time over its life, in seconds.
    pub cpu_time: Option<f64>,
    /// The full command line where the poll already has it. Neither host
    /// fills it now: both read it only for the open process, as a fact.
    pub args: String,
}

/// What a %CPU figure means on this platform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Clock {
    /// Linux: the delta of `utime + stime` over a measured window. `None`
    /// on the first poll, which has nothing to subtract from.
    Measured { window: Option<f64> },
    /// macOS: `ps`'s decaying average, which is not a window at all.
    Decaying,
}

/// One poll.
#[derive(Clone, Debug)]
pub struct Sample {
    pub procs: Vec<Proc>,
    pub cores: usize,
    pub clock: Clock,
    /// `/proc` is mounted with a `hidepid` that hides other users'
    /// processes, so every count is of the visible ones.
    pub hidepid: bool,
    /// This user's uid, for `[u] only yours`.
    pub me: u32,
    /// The machine as a whole: CPU, load, memory and swap.
    pub machine: Machine,
    /// The 1-minute load at each poll for the last ten minutes, oldest
    /// first; `None` for a poll that could not read it. Kept by the poller,
    /// not by the host.
    pub history: Vec<Option<f64>>,
    /// Threads per pid, read only while `[t]` is on (Linux). `None` when
    /// off, so a table drawn with threads never shows a stale set.
    pub threads: Option<HashMap<i32, Vec<Thread>>>,
    /// The services view's rows, read only while it is open (Linux). The
    /// poller adds them after the process pass, from one cgroup walk.
    pub services: Option<super::services::Services>,
}

/// One thread of a process, as `/proc/<pid>/task/<tid>/stat` reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Thread {
    pub tid: i32,
    pub comm: String,
    pub state: char,
    /// % of all cores over the same window as the processes; `None` until
    /// it has been read twice.
    pub cpu: Option<f64>,
    pub running: Option<f64>,
}

/// The machine's headline, read once a poll beside the process list.
#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    /// The share of every core that was busy: over the last poll on Linux,
    /// `ps`'s decaying average summed on macOS (`Sample::clock` says
    /// which). `None` until there are two readings to subtract.
    pub busy: Option<f64>,
    pub per_core: PerCore,
    /// The 1, 5 and 15 minute load averages.
    pub load: Option<(f64, f64, f64)>,
    pub memory: Option<Memory>,
    pub swap: Option<Swap>,
    /// Why a figure above is missing, one line each, drawn in the panel:
    /// `/proc/stat unavailable: …`. The table still draws.
    pub problems: Vec<String>,
}

/// Per-core busy shares, or why there are none.
#[derive(Clone, Debug, PartialEq)]
pub enum PerCore {
    /// `(cpu number, busy share)` over the last poll.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Read(Vec<(usize, f64)>),
    /// One reading so far; the next poll makes a delta.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Waiting,
    /// macOS: `ps` has no per-core figure and `host_processor_info` is not
    /// read in this version.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    NotRead,
}

/// Memory as three parts that sum to `total` exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Memory {
    pub total: u64,
    pub used: u64,
    pub reclaimable: u64,
    pub free: u64,
    pub source: MemSource,
}

/// Where the three parts came from, which the legend names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemSource {
    /// Linux: used is `MemTotal - MemAvailable`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    MemAvailable,
    /// macOS: page counts from `vm_stat`.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    VmStat,
}

impl Memory {
    pub fn from_parts(parts: [u64; 3], source: MemSource) -> Memory {
        Memory {
            total: parts.iter().sum(),
            used: parts[0],
            reclaimable: parts[1],
            free: parts[2],
            source,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Swap {
    pub total: u64,
    pub used: u64,
}

impl Machine {
    /// Nothing read yet, for a host or a test to fill.
    pub fn empty(per_core: PerCore) -> Machine {
        Machine {
            busy: None,
            per_core,
            load: None,
            memory: None,
            swap: None,
            problems: Vec::new(),
        }
    }
}

/// The load ring: one slot a poll for ten minutes. A poll that comes much
/// sooner than `every` (an `r`) replaces the newest slot rather than adding
/// one, so the slots stay about `every` apart and the axis stays ten
/// minutes wide.
pub struct LoadHistory {
    slots: usize,
    every: f64,
    held: std::collections::VecDeque<Option<f64>>,
    last: Option<f64>,
}

impl LoadHistory {
    pub fn new(every: f64) -> LoadHistory {
        let slots = history_slots(every);
        LoadHistory {
            slots,
            every,
            held: std::collections::VecDeque::with_capacity(slots),
            last: None,
        }
    }

    /// `at` is seconds on any monotonic clock.
    pub fn push(&mut self, load: Option<f64>, at: f64) {
        let soon = self.last.is_some_and(|last| at - last < self.every * 0.75);
        if soon && !self.held.is_empty() {
            if let Some(newest) = self.held.back_mut() {
                *newest = load;
            }
            return;
        }
        if self.held.len() == self.slots {
            self.held.pop_front();
        }
        self.held.push_back(load);
        self.last = Some(at);
    }

    pub fn values(&self) -> Vec<Option<f64>> {
        self.held.iter().copied().collect()
    }
}

/// Slots in ten minutes at one a poll: 300 at the default 2 s.
pub fn history_slots(every: f64) -> usize {
    ((600.0 / every.max(0.5)).ceil() as usize).clamp(2, 1200)
}

/// What the detail screen shows beyond the table's own columns. Read for
/// the one open process, on the poller, every poll.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    pub pid: i32,
    pub start: Option<u64>,
    pub cmdline: Result<String, String>,
    pub cwd: Result<String, String>,
    pub gid: Option<u32>,
    pub io: Io,
}

/// A process's disk rate, or why there is none. Linux reads it; macOS
/// says it does not.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum Io {
    /// Bytes per second over the last `over` seconds: `read_bytes`, and
    /// `write_bytes` less `cancelled_write_bytes`, from `/proc/<pid>/io`.
    Rate { read: f64, write: f64, over: f64 },
    /// One reading so far; a rate needs the next.
    Measuring,
    /// `/proc/<pid>/io` is closed to this user.
    NeedsRoot,
    /// Unreadable for some other reason, which is named.
    Unreadable(String),
    /// macOS does not read it in this version.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    NotRead,
}

/// What `[s]` cycles through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Cpu,
    Memory,
    Running,
}

impl Sort {
    pub fn next(self) -> Sort {
        match self {
            Sort::Cpu => Sort::Memory,
            Sort::Memory => Sort::Running,
            Sort::Running => Sort::Cpu,
        }
    }

    /// The footer: what the next press sorts by.
    pub fn hint(self) -> String {
        format!("[s] sort by {}", self.next().name())
    }

    pub fn name(self) -> &'static str {
        match self {
            Sort::Cpu => "cpu",
            Sort::Memory => "memory",
            Sort::Running => "running",
        }
    }

    /// The body: what is in force, and which way round.
    pub fn in_force(self) -> &'static str {
        match self {
            Sort::Cpu => "sorted by %CPU",
            Sort::Memory => "sorted by memory",
            Sort::Running => "sorted by running, newest first",
        }
    }
}

/// The processes the table can show at all: everything, less kernel
/// threads unless they are asked for.
pub fn listed(sample: &Sample, show_kernel: bool) -> (Vec<&Proc>, usize) {
    let mut hidden = 0;
    let mut out = Vec::with_capacity(sample.procs.len());
    for proc in &sample.procs {
        if proc.kernel && !show_kernel {
            hidden += 1;
        } else {
            out.push(proc);
        }
    }
    (out, hidden)
}

/// Whether a process passes the `/` filter and the `[u]` toggle. The
/// needle matches the command or the user, ignoring case.
pub fn passes(proc: &Proc, needle: &str, all_users: bool, me: u32) -> bool {
    if !all_users && proc.uid != me {
        return false;
    }
    if needle.is_empty() {
        return true;
    }
    let needle = needle.to_lowercase();
    proc.comm.to_lowercase().contains(&needle) || proc.user.to_lowercase().contains(&needle)
}

/// The filters in force, named the way `filter_row` draws them.
pub fn filters(needle: &str, all_users: bool) -> Vec<String> {
    let mut out = Vec::new();
    if !needle.is_empty() {
        out.push(format!("/{needle}"));
    }
    if !all_users {
        out.push("yours".to_string());
    }
    out
}

/// Sorted in place. Ties fall to the pid, so a quiet table does not
/// shuffle between polls.
pub fn sort(procs: &mut [&Proc], by: Sort) {
    let cpu = |p: &Proc| p.cpu.unwrap_or(-1.0);
    procs.sort_by(|a, b| {
        let first = match by {
            Sort::Cpu => cpu(b).total_cmp(&cpu(a)).then(b.rss.cmp(&a.rss)),
            Sort::Memory => b.rss.cmp(&a.rss),
            Sort::Running => a
                .running
                .unwrap_or(f64::MAX)
                .total_cmp(&b.running.unwrap_or(f64::MAX)),
        };
        first.then(a.pid.cmp(&b.pid))
    });
}

/// `pid ← ppid ← … ← 1`, with each name from the same sample. Stops at a
/// parent the sample does not have (pid 0, or one `hidepid` hides) and
/// never loops.
pub fn parent_chain(procs: &[Proc], pid: i32) -> Vec<(i32, Option<String>)> {
    let by_pid: HashMap<i32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut chain = Vec::new();
    let mut at = pid;
    while chain.len() < 64 {
        let found = by_pid.get(&at);
        chain.push((at, found.map(|p| p.comm.clone())));
        let Some(proc) = found else { break };
        if proc.ppid <= 0 || proc.ppid == at || chain.iter().any(|(seen, _)| *seen == proc.ppid) {
            break;
        }
        at = proc.ppid;
    }
    chain
}

/// How many zombies, and how many in D, among the processes listed.
pub fn counts(procs: &[&Proc]) -> (usize, usize) {
    let zombies = procs.iter().filter(|p| p.state == 'Z').count();
    let waiting = procs.iter().filter(|p| p.state == 'D').count();
    (zombies, waiting)
}

#[cfg(test)]
pub mod fixtures {
    use super::*;

    pub fn proc(pid: i32, comm: &str, user: &str, cpu: Option<f64>, rss: u64) -> Proc {
        Proc {
            pid,
            ppid: 1,
            comm: comm.into(),
            uid: if user == "user-a" { 1000 } else { 0 },
            user: user.into(),
            cpu,
            rss,
            mem: Some(rss as f64 / (16u64 << 30) as f64 * 100.0),
            state: 'S',
            running: Some(pid as f64 * 10.0),
            cgroup: Some(format!("/system.slice/{comm}.service")),
            kernel: false,
            start: Some(pid as u64),
            threads: Some(1),
            nice: Some(0),
            cpu_time: Some(1.5),
            args: String::new(),
        }
    }

    pub fn sample(procs: Vec<Proc>) -> Sample {
        Sample {
            procs,
            cores: 8,
            clock: Clock::Measured { window: Some(2.0) },
            hidepid: false,
            me: 1000,
            machine: machine(8),
            history: (0..300)
                .map(|i| Some(2.0 + (i % 40) as f64 / 10.0))
                .collect(),
            threads: None,
            services: None,
        }
    }

    /// A Linux machine with `cores` cores, the first one hot.
    pub fn machine(cores: usize) -> Machine {
        Machine {
            busy: Some(0.41),
            per_core: PerCore::Read(
                (0..cores)
                    .map(|n| (n, if n == 0 { 0.92 } else { 0.1 * n as f64 % 1.0 }))
                    .collect(),
            ),
            load: Some((3.9, 2.8, 2.1)),
            memory: Some(Memory::from_parts(
                [18 << 30, 6 << 30, 7 << 30],
                MemSource::MemAvailable,
            )),
            swap: Some(Swap {
                total: 4 << 30,
                used: 1_288_490_189,
            }),
            problems: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn the_load_ring_holds_ten_minutes_at_the_poll_rate() {
        assert_eq!(history_slots(2.0), 300);
        assert_eq!(history_slots(5.0), 120);
        assert_eq!(history_slots(0.5), 1200);
        let mut ring = LoadHistory::new(2.0);
        for i in 0..400 {
            ring.push(Some(i as f64), i as f64 * 2.0);
        }
        let held = ring.values();
        assert_eq!(held.len(), 300, "trimmed to the window");
        assert_eq!(held[0], Some(100.0), "the oldest fell off");
        assert_eq!(held[299], Some(399.0));
        // An `r` a moment after a poll replaces the newest slot.
        ring.push(Some(7.0), 399.0 * 2.0 + 0.3);
        let held = ring.values();
        assert_eq!(held.len(), 300);
        assert_eq!(held[299], Some(7.0));
        assert_eq!(held[298], Some(398.0));
        // A poll that could not read the load is a gap, not a zero.
        ring.push(None, 900.0);
        assert_eq!(ring.values().last(), Some(&None));
    }

    #[test]
    fn sort_cycles_three_ways_and_names_the_next() {
        assert_eq!(Sort::Cpu.hint(), "[s] sort by memory");
        assert_eq!(Sort::Memory.hint(), "[s] sort by running");
        assert_eq!(Sort::Running.hint(), "[s] sort by cpu");
        assert_eq!(Sort::Running.next(), Sort::Cpu);
    }

    #[test]
    fn a_process_not_yet_measured_sorts_below_a_measured_idle_one() {
        let a = proc(10, "example-a", "user-a", None, 100);
        let b = proc(20, "example-b", "user-a", Some(0.0), 50);
        let c = proc(30, "example-c", "user-a", Some(40.0), 10);
        let mut list = vec![&a, &b, &c];
        sort(&mut list, Sort::Cpu);
        assert_eq!(
            list.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![30, 20, 10]
        );
        sort(&mut list, Sort::Memory);
        assert_eq!(
            list.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![10, 20, 30]
        );
        sort(&mut list, Sort::Running);
        assert_eq!(
            list.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![10, 20, 30]
        );
    }

    #[test]
    fn the_filter_reads_command_and_user_and_yours_is_named() {
        let mine = proc(10, "example-worker", "user-a", None, 1);
        let theirs = proc(11, "example-db", "service-b", None, 1);
        assert!(passes(&mine, "WORKER", true, 1000));
        assert!(passes(&theirs, "service", true, 1000));
        assert!(!passes(&theirs, "", false, 1000));
        assert!(passes(&mine, "", false, 1000));
        assert_eq!(filters("example", false), vec!["/example", "yours"]);
        assert!(filters("", true).is_empty());
    }

    #[test]
    fn kernel_threads_are_counted_when_hidden() {
        let mut k = proc(2, "kthreadd", "root", None, 0);
        k.kernel = true;
        let s = sample(vec![k, proc(10, "example-worker", "user-a", None, 1)]);
        let (shown, hidden) = listed(&s, false);
        assert_eq!((shown.len(), hidden), (1, 1));
        let (shown, hidden) = listed(&s, true);
        assert_eq!((shown.len(), hidden), (2, 0));
    }

    #[test]
    fn the_parent_chain_ends_at_what_the_sample_has_and_never_loops() {
        let mut a = proc(1, "init", "root", None, 0);
        a.ppid = 0;
        let mut b = proc(50, "example-agent", "user-a", None, 0);
        b.ppid = 1;
        let mut c = proc(51, "example-worker", "user-a", None, 0);
        c.ppid = 50;
        let chain = parent_chain(&[a, b.clone(), c.clone()], 51);
        assert_eq!(
            chain.iter().map(|x| x.0).collect::<Vec<_>>(),
            vec![51, 50, 1]
        );
        // A parent the sample cannot see ends the chain, named as unknown.
        let chain = parent_chain(&[c.clone()], 51);
        assert_eq!(chain, vec![(51, Some("example-worker".into())), (50, None)]);
        // A loop (which the kernel never makes, but a torn read could)
        // stops.
        b.ppid = 51;
        let chain = parent_chain(&[b, c], 51);
        assert_eq!(chain.len(), 2);
    }
}
