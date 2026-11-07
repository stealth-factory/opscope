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
//! comes from `sysctlbyname("hw.memsize")`, read once.

use std::sync::OnceLock;

use opscope_core as tc;

use super::model::{Clock, Facts, Io, Proc, Sample};
use super::parse;

/// Seconds before a `ps` that has not answered is given up on.
const PS_TIMEOUT: u64 = 10;

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

/// One `ps -axo` over every process.
pub fn sample(_source: &mut Source) -> Result<Sample, String> {
    let text = tc::run(&["ps", "-axo", parse::PS_COLUMNS], PS_TIMEOUT)
        .map_err(|err| format!("ps failed: {err}"))?;
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let total = memsize();
    let procs = parse::parse_ps_table(&text)
        .into_iter()
        .map(|row| {
            let rss = row.rss_kb * 1024;
            Proc {
                pid: row.pid,
                ppid: row.ppid,
                comm: row.comm,
                uid: row.uid,
                user: row.user,
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
                args: row.args,
            }
        })
        .collect();
    Ok(Sample {
        procs,
        cores,
        clock: Clock::Decaying,
        hidepid: false,
        // SAFETY: geteuid has no preconditions and cannot fail.
        me: unsafe { libc::geteuid() },
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

/// Acquisition tests: they run the real `ps` on the macOS CI runner,
/// through the function the poller calls, so they prove the columns parse
/// on the machine that prints them rather than only in a fixture.
#[cfg(test)]
mod acquisition_tests {
    use super::*;

    #[test]
    fn ps_lists_this_process_and_launchd() {
        let got = sample(&mut Source).expect("ps -axo");
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
