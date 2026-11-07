// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Where the bytes come from on Linux: `/proc`, through `tc::procfs`.
//!
//! Per poll: the pid list, then for each pid its `stat`, its `cgroup` and
//! the owner of its directory - three reads, each readable for every
//! process whoever owns it - plus `cmdline` only for a process whose
//! `comm` is empty. `/proc/stat`, `/proc/loadavg`, `/proc/uptime` and
//! `/proc/meminfo` are read once. The previous poll's `stat` per pid (and
//! per thread, while `[t]` is on) and `/proc/stat` lines are the only
//! history kept: two samples make a rate.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use opscope_core::procfs::{self, PidIo, PidStat};

use super::model::{
    Clock, Facts, Io, Machine, MemSource, Memory, PerCore, Proc, Sample, Swap, Thread,
};
use super::parse::{self, CpuTimes};
use super::services::{self, Services};

/// `g` switches to the services view here.
pub const READS_SERVICES: bool = true;

/// The cgroup tree mounted on this host.
const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// One walk of the cgroup v2 tree for the services view, with
/// `cgroup.procs` read in it, against the last walk. A controller enabled
/// nowhere is named, and the other one is walked alone so its figures
/// still draw.
pub fn services(source: &mut Source, cores: usize) -> Option<Services> {
    use opscope_core::cgroup::{self, State};
    let began = *source.began.get_or_insert_with(Instant::now);
    let mut state = cgroup::read(&["cpu", "memory"], true);
    let mut off = None;
    if let State::NotEnabled(c) = &state {
        let other = if c == "cpu" { "memory" } else { "cpu" };
        off = Some((c.clone(), services::off_line(c, &state)));
        state = match cgroup::read(&[other], true) {
            // Both off: the first one is the line drawn.
            State::NotEnabled(_) => State::NotEnabled(c.clone()),
            alone => alone,
        };
    }
    let off = off.filter(|_| matches!(state, State::Ok(_) | State::Container(_)));
    let now = began.elapsed().as_secs_f64();
    Some(services::step(&mut source.services, state, off, now, cores))
}

/// The services view closed: the last walk is not kept, so opening it
/// again measures afresh rather than over the whole time it was shut.
pub fn forget_services(source: &mut Source) {
    source.services.forget();
}

/// One cgroup's `memory.max`: `Ok(None)` when it has no limit.
pub fn memory_max(path: &str) -> Result<Option<u64>, String> {
    let file = std::path::Path::new(CGROUP_ROOT)
        .join(path)
        .join("memory.max");
    let text = std::fs::read_to_string(&file).map_err(|err| err.to_string())?;
    parse::parse_memory_max(&text).ok_or_else(|| format!("memory.max reads {:?}", text.trim()))
}

/// `[t]` lists threads here.
pub const READS_THREADS: bool = true;

/// What one poll keeps for the next.
#[derive(Default)]
pub struct Source {
    /// Last poll's `stat` per pid.
    prev: HashMap<i32, PidStat>,
    /// Last poll's `/proc/stat` `cpu` lines, the total and each core.
    prev_cpus: Option<Vec<CpuTimes>>,
    /// Last read of each thread's `stat`, by tid, while `[t]` is on.
    prev_tasks: HashMap<i32, PidStat>,
    /// The uptime at the last poll, so a process that started since can be
    /// measured from zero rather than waiting a poll.
    prev_uptime: Option<f64>,
    prev_at: Option<Instant>,
    users: Users,
    hidepid: Option<(bool, Instant)>,
    /// The open process's last `io` reading: pid, starttime, when, what.
    io: Option<(i32, u64, Instant, PidIo)>,
    /// The last cgroup walk, while the services view is open.
    services: services::Memo,
    /// The clock the walks are timed on.
    began: Option<Instant>,
}

/// uid to name: `/etc/passwd`, read again at most once a minute, then
/// `getpwuid_r` for a uid it does not list (a directory service).
#[derive(Default)]
struct Users {
    passwd: BTreeMap<u32, String>,
    read: Option<Instant>,
    looked_up: HashMap<u32, String>,
}

impl Users {
    fn name(&mut self, uid: u32) -> String {
        let stale = self
            .read
            .is_none_or(|at| at.elapsed() > Duration::from_secs(60));
        if stale {
            if let Ok(text) = std::fs::read_to_string("/etc/passwd") {
                self.passwd = parse::parse_passwd(&text);
            }
            self.read = Some(Instant::now());
        }
        if let Some(name) = self.passwd.get(&uid) {
            return name.clone();
        }
        self.looked_up
            .entry(uid)
            .or_insert_with(|| lookup(uid).unwrap_or_else(|| format!("uid {uid}")))
            .clone()
    }
}

fn lookup(uid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    // SAFETY: zeroed is a valid passwd (all pointers null); getpwuid_r
    // fills it from `buf`, which outlives the read of `pw_name` below.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut found) };
    if rc != 0 || found.is_null() || pwd.pw_name.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r succeeded, so pw_name is a NUL-terminated string
    // inside `buf`.
    let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) };
    Some(name.to_string_lossy().to_string())
}

/// What `/proc/stat`, `/proc/loadavg` and `/proc/meminfo` say about the
/// machine, each read once, against the last poll's `/proc/stat`. A file
/// that cannot be read is a line in `problems`, and the table still draws.
fn machine(
    stat: &Result<Vec<CpuTimes>, String>,
    prev: Option<&Vec<CpuTimes>>,
    meminfo: &Result<String, String>,
) -> Machine {
    let mut out = Machine::empty(PerCore::Waiting);
    match stat {
        Ok(cpus) => {
            if let Some(prev) = prev {
                let busy = |id: Option<usize>| {
                    let before = prev.iter().find(|c| c.id == id)?;
                    let after = cpus.iter().find(|c| c.id == id)?;
                    parse::core_busy(before, after)
                };
                out.busy = busy(None);
                let cores: Vec<(usize, f64)> = cpus
                    .iter()
                    .filter_map(|c| c.id)
                    .filter_map(|id| busy(Some(id)).map(|b| (id, b)))
                    .collect();
                if !cores.is_empty() {
                    out.per_core = PerCore::Read(cores);
                }
            }
        }
        Err(why) => out.problems.push(format!("/proc/stat unavailable: {why}")),
    }
    match procfs::read_proc("loadavg") {
        Ok(text) => match parse::parse_loadavg(&text) {
            Some(load) => out.load = Some(load),
            None => out
                .problems
                .push("/proc/loadavg unavailable: no averages in it".into()),
        },
        Err(why) => out
            .problems
            .push(format!("/proc/loadavg unavailable: {why}")),
    }
    match meminfo {
        Ok(text) => match parse::parse_meminfo(text) {
            Some(m) => {
                out.memory = Some(Memory::from_parts(
                    parse::memory_parts(&m),
                    MemSource::MemAvailable,
                ));
                out.swap = Some(Swap {
                    total: m.swap_total,
                    used: m.swap_total.saturating_sub(m.swap_free),
                });
            }
            None => out
                .problems
                .push("/proc/meminfo unavailable: no MemAvailable (a kernel before 3.14)".into()),
        },
        Err(why) => out
            .problems
            .push(format!("/proc/meminfo unavailable: {why}")),
    }
    out
}

/// Every thread of every process with more than one, against the last
/// read of each. Read only while `[t]` is on.
fn threads(
    source: &mut Source,
    procs: &[Proc],
    window: Option<f64>,
    ticks: f64,
    cores: usize,
    uptime: Option<f64>,
) -> HashMap<i32, Vec<Thread>> {
    let mut out = HashMap::new();
    let mut seen = HashMap::new();
    for proc in procs.iter().filter(|p| p.threads.unwrap_or(1) > 1) {
        // A process that left since the listing has no threads to read.
        let Ok(tids) = procfs::tids(proc.pid) else {
            continue;
        };
        let mut list = Vec::with_capacity(tids.len());
        for tid in tids {
            let Ok(stat) = procfs::read_task_stat(proc.pid, tid) else {
                continue;
            };
            let cpu = match (source.prev_tasks.get(&tid), window) {
                (Some(before), Some(window)) if before.starttime == stat.starttime => {
                    Some(parse::cpu_percent(before, &stat, window, ticks, cores))
                }
                _ => None,
            };
            list.push(Thread {
                tid,
                comm: stat.comm.clone(),
                state: stat.state,
                cpu,
                running: uptime.map(|up| parse::running_secs(stat.starttime, ticks, up)),
            });
            seen.insert(tid, stat);
        }
        list.sort_by(|a, b| {
            b.cpu
                .unwrap_or(-1.0)
                .total_cmp(&a.cpu.unwrap_or(-1.0))
                .then(a.tid.cmp(&b.tid))
        });
        out.insert(proc.pid, list);
    }
    source.prev_tasks = seen;
    out
}

/// One pass over `/proc`. Threads are read too when `with_threads`.
pub fn sample(source: &mut Source, with_threads: bool) -> Result<Sample, String> {
    let pids = procfs::pids().map_err(|err| format!("processes unavailable: {err}"))?;
    let ticks = procfs::clock_ticks();
    let stat = procfs::read_proc("stat").and_then(|text| {
        let cpus = parse::parse_proc_stat_cpus(&text);
        if cpus.is_empty() {
            Err("no cpu lines in it".to_string())
        } else {
            Ok(cpus)
        }
    });
    let cpu = stat
        .as_ref()
        .ok()
        .and_then(|cpus| parse::cpu_total(cpus))
        .map(|c| c.ticks);
    let cores = stat
        .as_ref()
        .ok()
        .and_then(|cpus| parse::core_count(cpus))
        .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(1);
    let uptime = procfs::read_uptime().ok();
    let meminfo = procfs::read_proc("meminfo");
    let total = meminfo
        .as_ref()
        .ok()
        .and_then(|text| parse::parse_meminfo_total(text));
    let machine = machine(&stat, source.prev_cpus.as_ref(), &meminfo);
    let now = Instant::now();
    // The kernel's own clock for the window; the wall clock only when
    // /proc/stat could not be read twice.
    let prev_cpu = source
        .prev_cpus
        .as_ref()
        .and_then(|cpus| parse::cpu_total(cpus))
        .map(|c| c.ticks);
    let window = match (prev_cpu, cpu) {
        (Some(prev), Some(next)) => parse::window_secs(&prev, &next, ticks, cores),
        _ => None,
    }
    .or_else(|| {
        source
            .prev_at
            .map(|at| now.duration_since(at).as_secs_f64())
    })
    .filter(|w| *w > 0.0);
    let fresh = source
        .hidepid
        .is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(60));
    if !fresh {
        // Unreadable is not hidden: the counts then say every process.
        let on = std::fs::read_to_string("/proc/self/mountinfo")
            .map(|text| procfs::parse_hidepid(&text))
            .unwrap_or(false);
        source.hidepid = Some((on, Instant::now()));
    }

    let mut procs = Vec::with_capacity(pids.len());
    let mut stats = HashMap::with_capacity(pids.len());
    for pid in pids {
        // A pid that has gone since the listing is dropped, never drawn
        // with a delta against nothing.
        let Ok(stat) = procfs::read_stat(pid) else {
            continue;
        };
        let Ok((uid, _)) = procfs::read_owner(pid) else {
            continue;
        };
        let cgroup = procfs::read_cgroup(pid).ok();
        let comm = if stat.comm.is_empty() {
            procfs::read_cmdline(pid)
                .ok()
                .and_then(|line| line.split(' ').next().map(str::to_string))
                .unwrap_or_default()
        } else {
            stat.comm.clone()
        };
        let before = source
            .prev
            .get(&pid)
            .filter(|prev| prev.starttime == stat.starttime)
            .cloned()
            .or_else(|| {
                // Started since the last poll: all of its time is inside
                // the window, so it is measured from zero.
                let started = stat.starttime as f64 / ticks;
                source
                    .prev_uptime
                    .filter(|&up| started >= up)
                    .map(|_| PidStat {
                        utime: 0,
                        stime: 0,
                        ..stat.clone()
                    })
            });
        let cpu_pct = match (before, window) {
            (Some(before), Some(window)) => {
                Some(parse::cpu_percent(&before, &stat, window, ticks, cores))
            }
            _ => None,
        };
        let rss = procfs::rss_bytes(stat.rss_pages);
        procs.push(Proc {
            pid,
            ppid: stat.ppid,
            comm,
            uid,
            user: source.users.name(uid),
            cpu: cpu_pct,
            rss,
            mem: total
                .filter(|&t| t > 0)
                .map(|t| rss as f64 / t as f64 * 100.0),
            state: stat.state,
            running: uptime.map(|up| parse::running_secs(stat.starttime, ticks, up)),
            cgroup,
            kernel: pid == 2 || stat.ppid == 2,
            start: Some(stat.starttime),
            threads: Some(stat.num_threads),
            nice: Some(stat.nice),
            cpu_time: Some((stat.utime + stat.stime) as f64 / ticks),
            args: String::new(),
        });
        stats.insert(pid, stat);
    }
    source.prev = stats;
    source.prev_cpus = stat.ok();
    source.prev_uptime = uptime;
    source.prev_at = Some(now);
    let threads = if with_threads {
        Some(threads(source, &procs, window, ticks, cores, uptime))
    } else {
        // Off: nothing is kept, so turning it on again measures afresh
        // rather than over the whole time it was off.
        source.prev_tasks.clear();
        None
    };
    Ok(Sample {
        procs,
        cores,
        clock: Clock::Measured { window },
        hidepid: source.hidepid.is_some_and(|(on, _)| on),
        // SAFETY: geteuid has no preconditions and cannot fail.
        me: unsafe { libc::geteuid() },
        machine,
        history: Vec::new(),
        threads,
        services: None,
    })
}

/// Another user's process answers EACCES on `io`, `cwd` and the rest of
/// the ptrace-gated files.
fn denied(err: &str) -> bool {
    err.contains("Permission denied") || err.contains("os error 13")
}

/// The open process's command line, working directory, gid and disk rate.
/// The rate is the difference between this read of `/proc/<pid>/io` and
/// the last one, so it covers one poll.
pub fn facts(source: &mut Source, proc: &Proc) -> Facts {
    let (pid, start) = (proc.pid, proc.start);
    let cmdline = procfs::read_cmdline(pid).and_then(|line| {
        if line.is_empty() {
            Err("empty: a kernel thread or a zombie has no command line".to_string())
        } else {
            Ok(line)
        }
    });
    let cwd = procfs::read_cwd(pid).map_err(|err| {
        if denied(&err) {
            "needs root for another user's process".to_string()
        } else {
            err
        }
    });
    let gid = procfs::read_owner(pid).ok().map(|(_, gid)| gid);
    let starttime = start.unwrap_or(0);
    let now = Instant::now();
    let io = match procfs::read_io(pid) {
        Ok(next) => {
            let rate = source
                .io
                .filter(|(p, s, _, _)| *p == pid && *s == starttime)
                .and_then(|(_, _, at, prev)| {
                    let over = now.duration_since(at).as_secs_f64();
                    (over > 0.0).then(|| {
                        let written =
                            |io: &PidIo| io.write_bytes.saturating_sub(io.cancelled_write_bytes);
                        Io::Rate {
                            read: next.read_bytes.saturating_sub(prev.read_bytes) as f64 / over,
                            write: written(&next).saturating_sub(written(&prev)) as f64 / over,
                            over,
                        }
                    })
                });
            source.io = Some((pid, starttime, now, next));
            rate.unwrap_or(Io::Measuring)
        }
        Err(err) => {
            source.io = None;
            if denied(&err) {
                Io::NeedsRoot
            } else {
                Io::Unreadable(err)
            }
        }
    };
    Facts {
        pid,
        start,
        cmdline,
        cwd,
        gid,
        io,
    }
}

/// The detail closed: its `io` reading is not kept.
pub fn forget(source: &mut Source) {
    source.io = None;
}

#[cfg(test)]
mod acquisition_tests {
    use super::*;

    #[test]
    fn this_process_is_in_the_table_with_its_owner_and_memory() {
        let mut source = Source::default();
        let first = sample(&mut source, false).expect("/proc is readable");
        let me = std::process::id() as i32;
        let row = first.procs.iter().find(|p| p.pid == me).expect("own row");
        assert_eq!(row.uid, unsafe { libc::geteuid() });
        assert!(row.rss > 0 && row.running.is_some());
        assert_eq!(row.cpu, None, "nothing to subtract from on the first poll");
        // Burn some CPU so the second poll has something to measure.
        let until = Instant::now() + Duration::from_millis(300);
        let mut spin = 0u64;
        while Instant::now() < until {
            spin = spin.wrapping_add(1);
        }
        assert!(spin > 0);
        let second = sample(&mut source, false).expect("/proc is readable");
        let row = second.procs.iter().find(|p| p.pid == me).expect("own row");
        assert!(row.cpu.is_some_and(|c| c > 0.0), "{:?}", row.cpu);
        assert!(matches!(second.clock, Clock::Measured { window: Some(w) } if w > 0.0));
    }

    #[test]
    fn the_machine_is_read_once_a_poll_and_measured_on_the_second() {
        let mut source = Source::default();
        let first = sample(&mut source, false).expect("/proc is readable");
        let m = &first.machine;
        assert!(m.problems.is_empty(), "{:?}", m.problems);
        assert_eq!(m.busy, None, "one reading of /proc/stat is not a delta");
        assert_eq!(m.per_core, PerCore::Waiting);
        assert!(m.load.is_some());
        let memory = m.memory.expect("/proc/meminfo has MemAvailable");
        assert_eq!(memory.source, MemSource::MemAvailable);
        assert_eq!(memory.used + memory.reclaimable + memory.free, memory.total);
        assert!(m.swap.is_some_and(|s| s.used <= s.total));
        std::thread::sleep(Duration::from_millis(250));
        let second = sample(&mut source, false).expect("/proc is readable");
        let m = &second.machine;
        assert!(
            m.busy.is_some_and(|b| (0.0..=1.0).contains(&b)),
            "{:?}",
            m.busy
        );
        match &m.per_core {
            PerCore::Read(cores) => {
                assert_eq!(cores.len(), second.cores, "one bar per online CPU");
                assert!(cores.iter().all(|(_, b)| (0.0..=1.0).contains(b)));
            }
            other => panic!("per-core not measured: {other:?}"),
        }
        assert!(
            second.threads.is_none(),
            "threads are read only while [t] is on"
        );
    }

    #[test]
    fn threads_are_listed_under_their_process_only_while_asked() {
        // This test binary runs its tests on threads of its own, and one
        // more here makes sure there are at least two.
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let held = std::thread::spawn(move || {
            let _ = rx.recv();
        });
        let mut source = Source::default();
        let me = std::process::id() as i32;
        let first = sample(&mut source, true).expect("/proc is readable");
        let own = first.procs.iter().find(|p| p.pid == me).expect("own row");
        let threads = first.threads.as_ref().expect("asked for threads");
        let mine = threads.get(&me).expect("this process has threads");
        assert!(mine.len() >= 2, "{mine:?}");
        assert!(own.threads.is_some_and(|n| n >= 2), "{:?}", own.threads);
        assert!(
            mine.iter().any(|t| t.tid == me),
            "the main thread is listed"
        );
        // SAFETY: gettid has no preconditions and cannot fail.
        let tid = unsafe { libc::gettid() };
        assert!(mine.iter().any(|t| t.tid == tid), "this thread is listed");
        assert!(
            mine.iter().all(|t| t.cpu.is_none()),
            "one read is not a delta"
        );
        let second = sample(&mut source, true).expect("/proc is readable");
        let mine = &second.threads.as_ref().expect("asked")[&me];
        assert!(mine.iter().any(|t| t.cpu.is_some()), "{mine:?}");
        // Threads are never counted as processes.
        assert!(!second.procs.iter().any(|p| p.pid == tid && tid != me));
        let off = sample(&mut source, false).expect("/proc is readable");
        assert!(off.threads.is_none());
        let _ = tx.send(());
        let _ = held.join();
    }

    #[test]
    fn the_services_walk_reads_this_host_without_root() {
        let mut source = Source::default();
        let Some(first) = services(&mut source, 8) else {
            panic!("Linux reads services")
        };
        // A runner without cgroup v2, or with the controllers off, still
        // gets a named state rather than an empty table.
        let Services::Table(first) = first else {
            return;
        };
        assert_eq!(first.window, None, "one walk is not a delta");
        std::thread::sleep(Duration::from_millis(50));
        let Some(Services::Table(second)) = services(&mut source, 8) else {
            panic!("the second walk read the same tree")
        };
        assert!(second.window.is_some_and(|w| w > 0.0));
        let me = std::process::id() as i32;
        assert!(
            second.rows.iter().any(|r| r.pids.contains(&me)),
            "this process lands on a row"
        );
    }

    #[test]
    fn own_io_is_measured_on_the_second_read() {
        let mut source = Source::default();
        let me = std::process::id() as i32;
        let table = sample(&mut source, false).expect("/proc is readable");
        let own = table
            .procs
            .iter()
            .find(|p| p.pid == me)
            .expect("own row")
            .clone();
        let first = facts(&mut source, &own);
        assert_eq!(first.io, Io::Measuring);
        assert!(first.cmdline.is_ok() && first.cwd.is_ok());
        std::thread::sleep(Duration::from_millis(20));
        let second = facts(&mut source, &own);
        assert!(matches!(second.io, Io::Rate { .. }), "{:?}", second.io);
    }
}
