// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Where the bytes come from on macOS: one `ps -axo` per poll.
//!
//! `ps` reads every user's processes, so the table is complete. Its
//! `%cpu` is macOS's decaying average rather than a delta over the poll,
//! and the header says so (William, 2026-10-22); a true per-process delta
//! through libproc is not in this version. The machine's memory for MEM
//! comes from `sysctlbyname("hw.memsize")`, read once. The panel's memory
//! is `vm_stat`'s, its swap `sysctl -n vm.swapusage`'s and its load
//! `getloadavg`'s, each poll; there is no per-core CPU figure.

use std::sync::OnceLock;

use opscope_core as tc;

use super::model::{Clock, Facts, Io, Machine, MemSource, Memory, PerCore, Proc, Sample, Swap};
use super::parse;

/// Seconds before a `ps` that has not answered is given up on.
const PS_TIMEOUT: u64 = 10;
/// Seconds for `vm_stat` and `sysctl`, which answer at once.
const TOOL_TIMEOUT: u64 = 5;

/// `[t]` has no threads to list here: `ps -axo` prints processes.
pub const READS_THREADS: bool = false;

/// Nothing is kept between polls: `ps` hands over its own average.
#[derive(Default)]
pub struct Source;

fn memsize() -> Option<u64> {
    static TOTAL: OnceLock<Option<u64>> = OnceLock::new();
    *TOTAL.get_or_init(|| {
        let mut value: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: the name is NUL-terminated, `value` is a u64 and `len`
        // says so; hw.memsize is a 64-bit integer.
        let rc = unsafe {
            libc::sysctlbyname(
                c"hw.memsize".as_ptr(),
                (&mut value as *mut u64).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && value > 0).then_some(value)
    })
}

/// The load averages from `getloadavg(3)`.
fn load() -> Option<(f64, f64, f64)> {
    let mut avg = [0f64; 3];
    // SAFETY: `avg` holds three doubles and the count says three.
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
    (n == 3).then_some((avg[0], avg[1], avg[2]))
}

/// Memory from `vm_stat`, swap from `sysctl -n vm.swapusage`, the load
/// from `getloadavg`, and the CPU total as the sum of `ps`'s decaying
/// averages. There is no per-core figure: that needs
/// `host_processor_info`, which this version does not call.
fn machine(procs: &[Proc]) -> Machine {
    let mut out = Machine::empty(PerCore::NotRead);
    // Each process's share is already of every core.
    let sum: f64 = procs.iter().filter_map(|p| p.cpu).sum();
    out.busy = Some((sum / 100.0).clamp(0.0, 1.0));
    match load() {
        Some(avg) => out.load = Some(avg),
        None => out.problems.push("getloadavg failed".into()),
    }
    match tc::run(&["vm_stat"], TOOL_TIMEOUT) {
        Ok(text) => match parse::parse_vm_stat(&text) {
            Some(v) => {
                out.memory = Some(Memory::from_parts(
                    parse::vm_stat_parts(&v),
                    MemSource::VmStat,
                ))
            }
            None => out
                .problems
                .push("vm_stat unavailable: its page counts did not read".into()),
        },
        Err(why) => out.problems.push(format!("vm_stat unavailable: {why}")),
    }
    match tc::run(&["sysctl", "-n", "vm.swapusage"], TOOL_TIMEOUT) {
        Ok(text) => match parse::parse_swapusage(&text) {
            Some((total, used)) => out.swap = Some(Swap { total, used }),
            None => out
                .problems
                .push("vm.swapusage unavailable: its sizes did not read".into()),
        },
        Err(why) => out
            .problems
            .push(format!("vm.swapusage unavailable: {why}")),
    }
    out
}

/// One `ps -axo` over every process. macOS lists no threads.
pub fn sample(_source: &mut Source, _with_threads: bool) -> Result<Sample, String> {
    let text = tc::run(&["ps", "-axo", parse::PS_COLUMNS], PS_TIMEOUT)
        .map_err(|err| format!("ps failed: {err}"))?;
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let total = memsize();
    let rows = parse::parse_ps_table(&text);
    // ps always lists at least itself: nothing read is a format this
    // parser does not know, never a machine running nothing.
    if rows.is_empty() {
        return Err(format!(
            "ps printed {} lines and none read as a process",
            text.lines().count()
        ));
    }
    let procs = rows
        .into_iter()
        .map(|row| {
            let rss = row.rss_kb * 1024;
            Proc {
                pid: row.pid,
                ppid: row.ppid,
                // Any user names their own process; its bytes are text.
                comm: tc::source_text(&row.comm),
                uid: row.uid,
                user: tc::source_text(&row.user),
                // ps gives a share of one core; the table's is of all.
                cpu: Some(row.cpu / cores as f64),
                rss,
                mem: total.map(|t| rss as f64 / t as f64 * 100.0),
                state: row.state,
                running: Some(row.running),
                cgroup: None,
                kernel: false,
                start: None,
                threads: None,
                nice: None,
                cpu_time: None,
                args: tc::source_text(&row.args),
            }
        })
        .collect::<Vec<_>>();
    let machine = machine(&procs);
    Ok(Sample {
        procs,
        cores,
        clock: Clock::Decaying,
        hidepid: false,
        // SAFETY: geteuid has no preconditions and cannot fail.
        me: unsafe { libc::geteuid() },
        machine,
        history: Vec::new(),
        threads: None,
        services: None,
    })
}

/// The open process's facts: the command line `ps` already printed, and
/// nothing read that this version does not read.
pub fn facts(_source: &mut Source, proc: &Proc) -> Facts {
    Facts {
        pid: proc.pid,
        start: proc.start,
        cmdline: if proc.args.is_empty() {
            Err("ps printed no command line".to_string())
        } else {
            Ok(proc.args.clone())
        },
        cwd: Err("not read on macOS in this version".to_string()),
        gid: None,
        io: Io::NotRead,
    }
}

pub fn forget(_source: &mut Source) {}

/// No services view here: macOS has no cgroups, so `g` is not offered and
/// does nothing.
pub const READS_SERVICES: bool = false;

pub fn services(_source: &mut Source, _cores: usize) -> Option<super::services::Services> {
    None
}

pub fn forget_services(_source: &mut Source) {}

pub fn memory_max(_path: &str) -> Result<Option<u64>, String> {
    Err("macOS has no cgroups".to_string())
}

/// Acquisition tests: they run the real `ps` on the macOS CI runner,
/// through the function the poller calls, so they prove the columns parse
/// on the machine that prints them rather than only in a fixture.
#[cfg(test)]
mod acquisition_tests {
    use super::*;

    #[test]
    fn the_machine_comes_from_vm_stat_sysctl_and_getloadavg() {
        let got = sample(&mut Source, true).expect("ps -axo");
        let m = &got.machine;
        assert!(m.problems.is_empty(), "{:?}", m.problems);
        assert_eq!(m.per_core, PerCore::NotRead, "no per-core figure on macOS");
        assert!(
            m.busy.is_some_and(|b| (0.0..=1.0).contains(&b)),
            "{:?}",
            m.busy
        );
        let (one, five, fifteen) = m.load.expect("getloadavg");
        assert!(one >= 0.0 && five >= 0.0 && fifteen >= 0.0);
        let memory = m.memory.expect("vm_stat read");
        assert_eq!(memory.source, MemSource::VmStat);
        assert!(memory.used > 0 && memory.total > memory.used);
        // vm_stat's pages are of this machine's memory, give or take what
        // it does not count: never more than hw.memsize, and most of it.
        let installed = memsize().expect("hw.memsize");
        assert!(memory.total <= installed, "{} > {installed}", memory.total);
        assert!(
            memory.total > installed / 2,
            "{} of {installed}",
            memory.total
        );
        let swap = m.swap.expect("vm.swapusage read");
        assert!(swap.used <= swap.total);
        assert!(got.threads.is_none(), "macOS lists no threads");
    }

    #[test]
    fn ps_lists_this_process_and_launchd() {
        let got = sample(&mut Source, false).expect("ps -axo");
        let me = std::process::id() as i32;
        let row =
            got.procs.iter().find(|p| p.pid == me).unwrap_or_else(|| {
                panic!("ps listed {} processes but not this one", got.procs.len())
            });
        assert_eq!(row.uid, unsafe { libc::geteuid() });
        assert!(!row.user.is_empty(), "ps -o user= printed no name");
        assert!(row.rss > 0, "a running test binary has resident memory");
        assert!(row.mem.is_some(), "hw.memsize was not read");
        assert!(row.running.is_some_and(|r| r >= 0.0));
        let exe = std::env::current_exe().expect("this test binary's path");
        let exe = exe
            .file_name()
            .and_then(|n| n.to_str())
            .expect("a file name");
        assert!(
            exe.starts_with(row.comm.as_str()) || row.comm.starts_with(exe),
            "ps named this process {:?}; the binary is {exe:?}",
            row.comm
        );
        assert!(
            row.args.contains(exe),
            "{:?} does not name {exe:?}",
            row.args
        );
        let launchd = got.procs.iter().find(|p| p.pid == 1).expect("launchd");
        assert_eq!(launchd.uid, 0);
        assert!(matches!(got.clock, Clock::Decaying));
    }
}
