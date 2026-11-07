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
//! `comm` is empty. `/proc/stat`, `/proc/uptime` and `/proc/meminfo` are
//! read once. The previous poll's `stat` per pid is the only history kept:
//! two samples make a rate.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use opscope_core::procfs::{self, PidIo, PidStat};

use super::model::{Clock, Facts, Io, Proc, Sample};
use super::parse;

/// What one poll keeps for the next.
#[derive(Default)]
pub struct Source {
    /// Last poll's `stat` per pid.
    prev: HashMap<i32, PidStat>,
    /// Last poll's `/proc/stat` `cpu` line.
    prev_cpu: Option<[u64; 8]>,
    /// The uptime at the last poll, so a process that started since can be
    /// measured from zero rather than waiting a poll.
    prev_uptime: Option<f64>,
    prev_at: Option<Instant>,
    users: Users,
    hidepid: Option<(bool, Instant)>,
    /// The open process's last `io` reading: pid, starttime, when, what.
    io: Option<(i32, u64, Instant, PidIo)>,
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

/// One pass over `/proc`.
pub fn sample(source: &mut Source) -> Result<Sample, String> {
    let pids = procfs::pids().map_err(|err| format!("processes unavailable: {err}"))?;
    let ticks = procfs::clock_ticks();
    let stat = std::fs::read_to_string("/proc/stat").ok();
    let cpu = stat.as_deref().and_then(parse::parse_proc_stat_cpu);
    let cores = stat
        .as_deref()
        .and_then(parse::parse_proc_stat_cores)
        .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(1);
    let uptime = procfs::read_uptime().ok();
    let total = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| parse::parse_meminfo_total(&text));
    let now = Instant::now();
    // The kernel's own clock for the window; the wall clock only when
    // /proc/stat could not be read twice.
    let window = match (source.prev_cpu, cpu) {
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
    source.prev_cpu = cpu;
    source.prev_uptime = uptime;
    source.prev_at = Some(now);
    Ok(Sample {
        procs,
        cores,
        clock: Clock::Measured { window },
        hidepid: source.hidepid.is_some_and(|(on, _)| on),
        // SAFETY: geteuid has no preconditions and cannot fail.
        me: unsafe { libc::geteuid() },
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
        let first = sample(&mut source).expect("/proc is readable");
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
        let second = sample(&mut source).expect("/proc is readable");
        let row = second.procs.iter().find(|p| p.pid == me).expect("own row");
        assert!(row.cpu.is_some_and(|c| c > 0.0), "{:?}", row.cpu);
        assert!(matches!(second.clock, Clock::Measured { window: Some(w) } if w > 0.0));
    }

    #[test]
    fn own_io_is_measured_on_the_second_read() {
        let mut source = Source::default();
        let me = std::process::id() as i32;
        let table = sample(&mut source).expect("/proc is readable");
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
