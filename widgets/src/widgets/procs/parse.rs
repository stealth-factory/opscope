// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Text to values, compiled and tested on every target.
//!
//! The Linux parsers here read `/proc/stat`, `/proc/meminfo` and
//! `/etc/passwd`; the per-pid files are `tc::procfs`'s. The macOS one reads
//! one `ps -axo` table. Both run on both, so the macOS CI build proves the
//! Linux arithmetic and the other way round.

// Each half is only acquired on its own platform - the `/proc` parsers on
// Linux, `parse_ps_table` on macOS - and both are tested on both.
#![allow(dead_code)]

use std::collections::BTreeMap;

use opscope_core::procfs::PidStat;

/// The columns `macos.rs` asks `ps` for, in this order. `comm` and `args`
/// are both free text, which is why they come last and why
/// [`parse_ps_table`] has to split them.
pub const PS_COLUMNS: &str = "pid=,ppid=,uid=,user=,%cpu=,%mem=,rss=,state=,etime=,comm=,args=";

/// One row of `ps -axo` with [`PS_COLUMNS`].
#[derive(Clone, Debug, PartialEq)]
pub struct PsRow {
    pub pid: i32,
    pub ppid: i32,
    pub uid: u32,
    pub user: String,
    /// `ps`'s `%cpu`: macOS's decaying average, as a share of one core.
    pub cpu: f64,
    pub mem: f64,
    /// Resident set, in KiB as `ps` prints it.
    pub rss_kb: u64,
    /// Already in the Linux letters: `U` (uninterruptible) is `D`, `I`
    /// (idle, asleep for more than twenty seconds) is `S`.
    pub state: char,
    /// `etime` in seconds.
    pub running: f64,
    /// The executable's name, without its directory.
    pub comm: String,
    /// The whole command line.
    pub args: String,
}

/// `ps -axo pid=,ppid=,uid=,user=,%cpu=,%mem=,rss=,state=,etime=,comm=,args=`.
///
/// Nine whitespace-separated fields, then `comm` and `args` - both free
/// text, and a path with a space in it is common on a Mac
/// (`/Applications/Example App.app/...`). The split is made where the rest
/// of the line starts again with the command: `args` begins with the path
/// it was started by, which `comm` also is. Where no split reads that way
/// (a process that rewrote its argv), `comm` is the first word and `args`
/// is the rest, which still keeps every character of the line.
///
/// A line that does not parse is skipped, not zeroed.
pub fn parse_ps_table(text: &str) -> Vec<PsRow> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let (fields, rest) = leading_fields(line, 9);
        if fields.len() < 9 {
            continue;
        }
        let parsed = (|| {
            let state = fields[7].chars().next()?;
            let (comm, args) = split_comm_args(rest);
            Some(PsRow {
                pid: fields[0].parse().ok()?,
                ppid: fields[1].parse().ok()?,
                uid: fields[2].parse().ok()?,
                user: fields[3].to_string(),
                cpu: fields[4].parse().ok()?,
                mem: fields[5].parse().ok()?,
                rss_kb: fields[6].parse().ok()?,
                state: apple_state(state),
                running: parse_etime(fields[8])?,
                comm: basename(&comm).to_string(),
                args,
            })
        })();
        if let Some(row) = parsed {
            rows.push(row);
        }
    }
    rows
}

/// The first `n` whitespace-separated fields of `line`, and what follows
/// them with its leading blanks removed.
fn leading_fields(line: &str, n: usize) -> (Vec<&str>, &str) {
    let mut fields = Vec::with_capacity(n);
    let mut rest = line;
    while fields.len() < n {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            return (fields, "");
        }
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        fields.push(&trimmed[..end]);
        rest = &trimmed[end..];
    }
    (fields, rest.trim())
}

/// `comm` and `args` from the free text after the ninth field.
fn split_comm_args(rest: &str) -> (String, String) {
    let rest = rest.trim();
    // Every place a space could end `comm`, left to right: the first one
    // after which the line starts again with what came before it.
    for (at, _) in rest.match_indices(' ') {
        let comm = rest[..at].trim_end();
        let args = rest[at..].trim_start();
        if comm.is_empty() || args.is_empty() {
            continue;
        }
        if args.starts_with(comm) {
            return (comm.to_string(), args.to_string());
        }
        // `comm` may be only the name while `args` holds the full path.
        let first = args.split(' ').next().unwrap_or("");
        if !comm.contains('/') && basename(first) == comm {
            return (comm.to_string(), args.to_string());
        }
    }
    match rest.split_once(' ') {
        Some((comm, args)) => (comm.to_string(), args.trim_start().to_string()),
        None => (rest.to_string(), rest.to_string()),
    }
}

fn basename(path: &str) -> &str {
    let name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path);
    if name.is_empty() { path } else { name }
}

/// Apple's state letters in the Linux ones the table draws.
pub fn apple_state(letter: char) -> char {
    match letter {
        'U' => 'D',
        'I' => 'S',
        other => other,
    }
}

/// `[[dd-]hh:]mm:ss` as Apple's `etime` prints it.
fn parse_etime(tok: &str) -> Option<f64> {
    let (days, rest) = match tok.split_once('-') {
        Some((d, r)) => (d.parse::<f64>().ok()?, r),
        None => (0.0, tok),
    };
    let parts: Vec<f64> = rest
        .split(':')
        .map(|p| p.parse::<f64>().ok())
        .collect::<Option<_>>()?;
    let secs = match parts.as_slice() {
        [mm, ss] => mm * 60.0 + ss,
        [hh, mm, ss] => hh * 3600.0 + mm * 60.0 + ss,
        _ => return None,
    };
    Some(days * 86_400.0 + secs)
}

/// `/etc/passwd`: uid to login name. Comments, blank lines and lines
/// without a numeric uid are skipped; the first name a uid is given wins,
/// as it does for `getpwuid`.
pub fn parse_passwd(text: &str) -> BTreeMap<u32, String> {
    let mut users = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 3 || fields[0].is_empty() {
            continue;
        }
        let Ok(uid) = fields[2].parse::<u32>() else {
            continue;
        };
        users.entry(uid).or_insert_with(|| fields[0].to_string());
    }
    users
}

/// The `cpu` line of `/proc/stat`: user, nice, system, idle, iowait, irq,
/// softirq, steal, in clock ticks summed over every online CPU. `guest`
/// and `guest_nice` are left out because `user` and `nice` already count
/// them. A kernel too old to print the later fields has them as zero.
pub fn parse_proc_stat_cpu(text: &str) -> Option<[u64; 8]> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let mut out = [0u64; 8];
    let mut seen = 0;
    for (slot, field) in out.iter_mut().zip(line.split_whitespace().skip(1)) {
        *slot = field.parse().ok()?;
        seen += 1;
    }
    (seen >= 4).then_some(out)
}

/// How many CPUs `/proc/stat` lists: its `cpuN` lines, which are the
/// online ones the `cpu` line sums over.
pub fn parse_proc_stat_cores(text: &str) -> Option<usize> {
    let n = text
        .lines()
        .filter(|l| {
            l.strip_prefix("cpu")
                .and_then(|rest| rest.split(char::is_whitespace).next())
                .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
        })
        .count();
    (n > 0).then_some(n)
}

/// `MemTotal` from `/proc/meminfo`, in bytes.
pub fn parse_meminfo_total(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let mut parts = line["MemTotal:".len()..].split_whitespace();
    let n: u64 = parts.next()?.parse().ok()?;
    match parts.next() {
        Some("kB") | None => Some(n * 1024),
        _ => None,
    }
}

/// Seconds that passed between two `/proc/stat` `cpu` lines: every tick
/// of every CPU, idle included, divided back down to one CPU's worth.
/// This is the kernel's own clock for the window the per-process ticks
/// were counted over, so the two cannot drift apart the way a wall clock
/// read a moment later can.
pub fn window_secs(prev: &[u64; 8], next: &[u64; 8], ticks: f64, cores: usize) -> Option<f64> {
    let total = |v: &[u64; 8]| v.iter().sum::<u64>();
    let delta = total(next).checked_sub(total(prev))?;
    if delta == 0 || ticks <= 0.0 || cores == 0 {
        return None;
    }
    Some(delta as f64 / ticks / cores as f64)
}

/// %CPU of all cores over a window: `Δ(utime + stime) / ticks / elapsed /
/// cores × 100`. A busy single thread on eight cores is 12.5.
///
/// The two samples must be the same process - same pid *and* same
/// `starttime` - which the caller checks; a counter that went backwards
/// reads as zero rather than negative.
pub fn cpu_percent(
    prev: &PidStat,
    next: &PidStat,
    elapsed_s: f64,
    ticks: f64,
    cores: usize,
) -> f64 {
    if elapsed_s <= 0.0 || ticks <= 0.0 || cores == 0 {
        return 0.0;
    }
    let used = (next.utime + next.stime).saturating_sub(prev.utime + prev.stime);
    used as f64 / ticks / elapsed_s / cores as f64 * 100.0
}

/// How long a process has been running: uptime less its `starttime`,
/// which is in clock ticks after boot. Never negative.
pub fn running_secs(starttime: u64, ticks: f64, uptime: f64) -> f64 {
    opscope_core::procfs::age_seconds(uptime, starttime, ticks).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opscope_core::procfs::parse_pid_stat;

    /// A `/proc/<pid>/stat` line with the fields this widget reads set and
    /// every other one holding its own number.
    fn stat_line(pid: i32, comm: &str, utime: u64, stime: u64, starttime: u64) -> String {
        let mut fields: Vec<String> = (5..=44).map(|n| n.to_string()).collect();
        fields[14 - 5] = utime.to_string();
        fields[15 - 5] = stime.to_string();
        fields[19 - 5] = "0".into();
        fields[20 - 5] = "4".into();
        fields[22 - 5] = starttime.to_string();
        fields[24 - 5] = "300".into();
        format!("{} ({}) S 1 {}", pid, comm, fields.join(" "))
    }

    #[test]
    fn cpu_is_a_share_of_every_core_over_the_window_at_this_tick_rate() {
        // 250 Hz and eight cores, so neither the USER_HZ guess of 100 nor
        // one core gives the same figure.
        let ticks = 250.0;
        let cores = 8;
        let stat_a = "cpu  1000 0 1000 10000 0 0 0 0 0 0\ncpu0 1 1 1 1\n";
        // Two seconds later: 250 ticks × 2 s × 8 cores more in total.
        let stat_b = "cpu  2000 0 2000 12000 0 0 0 0 0 0\ncpu0 1 1 1 1\n";
        let a = parse_proc_stat_cpu(stat_a).expect("cpu line");
        let b = parse_proc_stat_cpu(stat_b).expect("cpu line");
        let window = window_secs(&a, &b, ticks, cores).expect("time passed");
        assert!((window - 2.0).abs() < 1e-9, "{window}");
        assert_ne!(
            window_secs(&a, &b, 100.0, cores),
            Some(2.0),
            "100 Hz is not this kernel"
        );
        assert_ne!(
            window_secs(&a, &b, ticks, 1),
            Some(2.0),
            "one core is not this machine"
        );

        // One busy thread: 500 ticks in two seconds at 250 Hz is one core.
        let p0 = parse_pid_stat(&stat_line(4242, "example-worker", 1000, 0, 9000)).unwrap();
        let p1 = parse_pid_stat(&stat_line(4242, "example-worker", 1400, 100, 9000)).unwrap();
        let pct = cpu_percent(&p0, &p1, window, ticks, cores);
        assert!((pct - 12.5).abs() < 1e-9, "{pct}");
        assert!((cpu_percent(&p0, &p1, window, 100.0, cores) - 12.5).abs() > 1.0);
        assert!((cpu_percent(&p0, &p1, window, ticks, 1) - 12.5).abs() > 1.0);
        // A counter that went backwards is not negative.
        assert_eq!(cpu_percent(&p1, &p0, window, ticks, cores), 0.0);
    }

    #[test]
    fn cores_are_the_cpu_lines_that_have_a_number() {
        let text = "cpu  1 2 3 4\ncpu0 1 2 3 4\ncpu1 1 2 3 4\ncpu11 1 2 3 4\nintr 5\nctxt 9\n";
        assert_eq!(parse_proc_stat_cores(text), Some(3));
        assert_eq!(parse_proc_stat_cores("intr 5\n"), None);
        assert_eq!(parse_proc_stat_cpu("intr 5\n"), None);
        // An old kernel with four fields still reads.
        assert_eq!(
            parse_proc_stat_cpu("cpu 1 2 3 4\n"),
            Some([1, 2, 3, 4, 0, 0, 0, 0])
        );
    }

    #[test]
    fn running_time_is_counted_in_this_kernels_ticks() {
        // 50 000 ticks at 250 Hz is 200 s after boot; at 100 Hz it would
        // be 500 s, which this would then report as 500 s running.
        assert_eq!(running_secs(50_000, 250.0, 1000.0), 800.0);
        assert_ne!(running_secs(50_000, 100.0, 1000.0), 800.0);
        // A start after the uptime that was read is a clock race, not a
        // negative age.
        assert_eq!(running_secs(500_000, 250.0, 1000.0), 0.0);
    }

    #[test]
    fn a_command_with_brackets_and_spaces_is_one_name() {
        let s = parse_pid_stat(&stat_line(7, "(a) b (c)", 1, 2, 3)).expect("parses");
        assert_eq!(s.comm, "(a) b (c)");
        assert_eq!((s.utime, s.stime, s.starttime), (1, 2, 3));
    }

    #[test]
    fn meminfo_total_is_bytes() {
        let text = "MemTotal:       16314280 kB\nMemFree:  100 kB\n";
        assert_eq!(parse_meminfo_total(text), Some(16_314_280 * 1024));
        assert_eq!(parse_meminfo_total("MemFree: 1 kB\n"), None);
    }

    #[test]
    fn passwd_skips_comments_and_malformed_lines() {
        let text = "\
# a comment:x:5:5::/:/bin/sh
root:x:0:0:root:/root:/bin/bash

user-a:x:1000:1000::/home/user-a:/bin/sh
broken-line-without-fields
service-b:x:notanumber:0::/:/bin/false
service-c:x:998:998::/:/usr/sbin/nologin
shadowed:x:1000:1000::/:/bin/sh
";
        let users = parse_passwd(text);
        assert_eq!(users.get(&0).map(String::as_str), Some("root"));
        assert_eq!(
            users.get(&1000).map(String::as_str),
            Some("user-a"),
            "first one wins"
        );
        assert_eq!(users.get(&998).map(String::as_str), Some("service-c"));
        assert_eq!(users.len(), 3, "{users:?}");
    }

    #[test]
    fn a_ps_table_keeps_spaces_in_a_command_and_reads_apple_states() {
        let text = "\
    1     0     0 root               0.0  0.1  12345 Ss   12-03:04:05 /sbin/launchd    /sbin/launchd
  501     1   501 user-a             3.5  1.2 204800 U    01:02:03 /Applications/Example App.app/Contents/MacOS/Example App /Applications/Example App.app/Contents/MacOS/Example App --flag value
  777   501   501 user-a             0.0  0.0    900 I    05:09 example-worker   example-worker: rewritten title
  778   501   501 user-a             0.0  0.0    900 Z    00:01 (example-zombie)
garbage line
";
        let rows = parse_ps_table(text);
        assert_eq!(rows.len(), 4, "{rows:#?}");

        let launchd = &rows[0];
        assert_eq!(
            launchd.running,
            12.0 * 86_400.0 + 3.0 * 3600.0 + 4.0 * 60.0 + 5.0
        );
        assert_eq!(launchd.comm, "launchd");
        assert_eq!(launchd.state, 'S');

        let app = &rows[1];
        assert_eq!(app.state, 'D', "U is uninterruptible, drawn as D");
        assert_eq!(
            app.comm, "Example App",
            "a space in the path stays in the name"
        );
        assert_eq!(
            app.args,
            "/Applications/Example App.app/Contents/MacOS/Example App --flag value"
        );
        assert_eq!(
            (app.user.as_str(), app.uid, app.rss_kb),
            ("user-a", 501, 204_800)
        );
        assert_eq!(app.cpu, 3.5);
        assert_eq!(app.running, 3723.0);

        let renamed = &rows[2];
        assert_eq!(renamed.state, 'S', "I is idle sleep");
        assert_eq!(renamed.comm, "example-worker");
        assert_eq!(renamed.args, "example-worker: rewritten title");
        assert_eq!(renamed.running, 309.0);

        assert_eq!(rows[3].state, 'Z');
        assert_eq!(rows[3].comm, "(example-zombie)");
    }
}
