// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Text to values, compiled and tested on every target.
//!
//! The Linux parsers here read `/proc/stat`, `/proc/loadavg`,
//! `/proc/meminfo` and `/etc/passwd`; the per-pid files are `tc::procfs`'s.
//! The macOS ones read one `ps -axo` table, `vm_stat` and `sysctl
//! vm.swapusage`. Both run on both, so the macOS CI build proves the Linux
//! arithmetic and the other way round.

// Each half is only acquired on its own platform - the `/proc` parsers on
// Linux, `parse_ps_table` on macOS - and both are tested on both.
#![allow(dead_code)]

use std::collections::BTreeMap;

use opscope_core::procfs::PidStat;

/// The columns `macos.rs` asks `ps` for, in this order, with
/// [`PS_FLAGS`]. `comm` is free text - a path with a space in it is common
/// on a Mac (`/Applications/Example App.app/...`) - so it is the last
/// column and the only one: Apple's `ps` pads and cuts every column but the
/// last to a fixed width, and `comm`'s is sixteen, which left a name like
/// `wo` from `/Users/runner/work/...`. The command line is read for the
/// open process alone, by [`PS_ARGS`].
pub const PS_COLUMNS: &str = "pid=,ppid=,uid=,user=,%cpu=,%mem=,rss=,state=,etime=,comm=";

/// Every process, and no width limit: without `ww` Apple's `ps` cuts each
/// line to 79 columns when its output is not a terminal.
pub const PS_FLAGS: &str = "-axww";

/// One process's whole command line, as `ps -ww -o args= -p <pid>`.
pub const PS_ARGS: &str = "args=";

/// One row of `ps` with [`PS_COLUMNS`].
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
}

/// `ps -axww -o pid=,ppid=,uid=,user=,%cpu=,%mem=,rss=,state=,etime=,comm=`.
///
/// Nine whitespace-separated fields, then `comm` - the rest of the line,
/// spaces and all, since it is the last column. Its directory is dropped.
///
/// A line that does not parse is skipped, not zeroed.
pub fn parse_ps_table(text: &str) -> Vec<PsRow> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let (fields, rest) = leading_fields(line, 9);
        if fields.len() < 9 || rest.is_empty() {
            continue;
        }
        let parsed = (|| {
            let state = fields[7].chars().next()?;
            Some(PsRow {
                pid: fields[0].parse().ok()?,
                ppid: fields[1].parse().ok()?,
                uid: fields[2].parse().ok()?,
                user: fields[3].to_string(),
                cpu: decimal(fields[4])?,
                mem: decimal(fields[5])?,
                rss_kb: fields[6].parse().ok()?,
                state: apple_state(state),
                running: parse_etime(fields[8])?,
                comm: basename(rest).to_string(),
            })
        })();
        if let Some(row) = parsed {
            rows.push(row);
        }
    }
    rows
}

/// A number as `ps` or `sysctl` printed it. Both call `setlocale`, so
/// under a locale whose decimal mark is a comma `%cpu` reads `1,5`; read as
/// `1.5` rather than dropping every row of the table.
fn decimal(tok: &str) -> Option<f64> {
    tok.replace(',', ".").parse().ok()
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

/// One `cpu` or `cpuN` line of `/proc/stat`: user, nice, system, idle,
/// iowait, irq, softirq, steal, in clock ticks. `guest` and `guest_nice`
/// are left out because `user` and `nice` already count them. A kernel too
/// old to print the later fields has them as zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuTimes {
    /// `None` for the `cpu` line, which sums every online CPU; `Some(n)`
    /// for `cpuN`. An offline CPU has no line, so the numbers can skip.
    pub id: Option<usize>,
    pub ticks: [u64; 8],
}

/// Every `cpu` line of `/proc/stat`, in the order printed: index 0 is the
/// `cpu` total where the kernel printed one, then one per online CPU. A
/// line that does not read is left out rather than zeroed.
pub fn parse_proc_stat_cpus(text: &str) -> Vec<CpuTimes> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("cpu") else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let id = if rest.starts_with(char::is_whitespace) {
            None
        } else {
            match fields.next().and_then(|n| n.parse::<usize>().ok()) {
                Some(n) => Some(n),
                None => continue,
            }
        };
        let mut ticks = [0u64; 8];
        let mut seen = 0;
        let mut bad = false;
        for (slot, field) in ticks.iter_mut().zip(fields) {
            match field.parse() {
                Ok(n) => *slot = n,
                Err(_) => {
                    bad = true;
                    break;
                }
            }
            seen += 1;
        }
        if !bad && seen >= 4 {
            out.push(CpuTimes { id, ticks });
        }
    }
    out
}

/// The `cpu` total line, if `/proc/stat` printed one.
pub fn cpu_total(cpus: &[CpuTimes]) -> Option<CpuTimes> {
    cpus.iter().find(|c| c.id.is_none()).copied()
}

/// How many CPUs `/proc/stat` lists: its `cpuN` lines, which are the
/// online ones the `cpu` line sums over.
pub fn core_count(cpus: &[CpuTimes]) -> Option<usize> {
    let n = cpus.iter().filter(|c| c.id.is_some()).count();
    (n > 0).then_some(n)
}

/// The share of the time between two readings of one CPU line that it
/// was busy: everything but `idle` and `iowait`, over everything, so
/// `steal` - time the hypervisor gave to someone else - counts as busy.
/// `None` when no tick passed or the total or `idle` went backwards (a CPU
/// taken offline and brought back), never a zero standing in for "not
/// measured". `iowait` alone is documented as unreliable and can go
/// backwards on a CPU that is working normally, so a drop in it counts as
/// no iowait over the window rather than a reading that failed.
pub fn core_busy(prev: &CpuTimes, next: &CpuTimes) -> Option<f64> {
    let total = |t: &[u64; 8]| t.iter().sum::<u64>();
    let all = total(&next.ticks).checked_sub(total(&prev.ticks))?;
    let idle = next.ticks[3].checked_sub(prev.ticks[3])?;
    let iowait = next.ticks[4].saturating_sub(prev.ticks[4]);
    let quiet = idle + iowait;
    if all == 0 {
        return None;
    }
    Some((all.saturating_sub(quiet) as f64 / all as f64).clamp(0.0, 1.0))
}

/// `/proc/loadavg`: the 1, 5 and 15 minute averages, the first three of
/// its five fields.
pub fn parse_loadavg(text: &str) -> Option<(f64, f64, f64)> {
    let mut fields = text.split_whitespace();
    let mut next = || {
        fields
            .next()?
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0)
    };
    Some((next()?, next()?, next()?))
}

/// The lines of `/proc/meminfo` the panel draws, in bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemInfo {
    pub total: u64,
    pub free: u64,
    /// The kernel's estimate of what could be handed out without
    /// swapping: free memory plus the cache and slab it can reclaim.
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

/// One `Key:  1234 kB` line of `/proc/meminfo`, in bytes.
fn meminfo_value(text: &str, key: &str) -> Option<u64> {
    let line = text.lines().find(|l| {
        l.strip_prefix(key)
            .is_some_and(|rest| rest.starts_with(':'))
    })?;
    let mut parts = line[key.len() + 1..].split_whitespace();
    let n: u64 = parts.next()?.parse().ok()?;
    match parts.next() {
        Some("kB") | None => n.checked_mul(1024),
        _ => None,
    }
}

/// `/proc/meminfo`: `MemTotal`, `MemFree`, `MemAvailable`, `SwapTotal`
/// and `SwapFree`. `None` when any is missing - a kernel older than 3.14
/// prints no `MemAvailable`, and a guess at it would be a figure nobody
/// measured.
pub fn parse_meminfo(text: &str) -> Option<MemInfo> {
    Some(MemInfo {
        total: meminfo_value(text, "MemTotal")?,
        free: meminfo_value(text, "MemFree")?,
        available: meminfo_value(text, "MemAvailable")?,
        swap_total: meminfo_value(text, "SwapTotal")?,
        swap_free: meminfo_value(text, "SwapFree")?,
    })
}

/// `MemTotal` from `/proc/meminfo`, in bytes: what MEM is a share of,
/// read even from a kernel that prints no `MemAvailable`.
pub fn parse_meminfo_total(text: &str) -> Option<u64> {
    meminfo_value(text, "MemTotal")
}

/// A cgroup's `memory.max`: `Some(None)` for `max` (no limit),
/// `Some(Some(bytes))` for a limit, `None` for anything else.
pub fn parse_memory_max(text: &str) -> Option<Option<u64>> {
    match text.trim() {
        "max" => Some(None),
        other => other.parse().ok().map(Some),
    }
}

/// Used, reclaimable and free, in bytes, summing to `MemTotal` exactly.
///
/// used = `MemTotal - MemAvailable`; reclaimable = `MemAvailable -
/// MemFree`; free = `MemFree`. On an idle machine with little cache the
/// kernel's reserve can put `MemAvailable` under `MemFree`; then nothing
/// is reclaimable and free is `MemAvailable`, so the three still sum to
/// the total rather than overrunning it.
pub fn memory_parts(m: &MemInfo) -> [u64; 3] {
    let available = m.available.min(m.total);
    let free = m.free.min(available);
    [m.total - available, available - free, free]
}

/// [`memory_parts`] as fractions of the total, summing to one.
pub fn memory_shares(m: &MemInfo) -> [f64; 3] {
    shares(memory_parts(m))
}

/// Three byte counts as fractions of their sum; all zero for nothing.
pub fn shares(parts: [u64; 3]) -> [f64; 3] {
    let sum: u64 = parts.iter().sum();
    if sum == 0 {
        return [0.0; 3];
    }
    let a = parts[0] as f64 / sum as f64;
    let b = parts[1] as f64 / sum as f64;
    // The last takes the rounding, so the three are one exactly.
    [a, b, 1.0 - a - b]
}

/// What macOS `vm_stat` prints that the panel draws: the page size from
/// its first line, then page counts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VmStat {
    pub page_size: u64,
    pub free: u64,
    pub active: u64,
    pub inactive: u64,
    pub speculative: u64,
    pub wired: u64,
    pub compressor: u64,
}

/// `vm_stat`: `Mach Virtual Memory Statistics: (page size of 16384
/// bytes)`, then `Pages free:  1234.` and the rest. The page size is read,
/// never assumed - Apple silicon's is 16 KiB, Intel's 4 KiB.
pub fn parse_vm_stat(text: &str) -> Option<VmStat> {
    let page_size = text
        .lines()
        .next()?
        .split("page size of")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)?;
    let get = |key: &str| -> Option<u64> {
        let line = text.lines().find(|l| {
            l.strip_prefix(key)
                .is_some_and(|rest| rest.starts_with(':'))
        })?;
        line[key.len() + 1..]
            .trim()
            .trim_end_matches('.')
            .parse()
            .ok()
    };
    Some(VmStat {
        page_size,
        free: get("Pages free")?,
        active: get("Pages active")?,
        inactive: get("Pages inactive")?,
        speculative: get("Pages speculative")?,
        wired: get("Pages wired down")?,
        compressor: get("Pages occupied by compressor")?,
    })
}

/// Used (active, wired and compressed), reclaimable (inactive and
/// speculative) and free, in bytes, from `vm_stat`.
pub fn vm_stat_parts(v: &VmStat) -> [u64; 3] {
    let bytes = |pages: u64| pages.saturating_mul(v.page_size);
    [
        bytes(v.active + v.wired + v.compressor),
        bytes(v.inactive + v.speculative),
        bytes(v.free),
    ]
}

/// `sysctl -n vm.swapusage`: `total = 2048.00M  used = 1.00M  free =
/// 2047.00M  (encrypted)`, as `(total, used)` in bytes.
pub fn parse_swapusage(text: &str) -> Option<(u64, u64)> {
    let value = |key: &str| -> Option<u64> {
        let at = text.find(&format!("{key} ="))?;
        let tok = text[at + key.len() + 2..].split_whitespace().next()?;
        let (num, unit) = tok.split_at(tok.find(|c: char| c.is_ascii_alphabetic())?);
        let n: f64 = decimal(num)?;
        let scale = match unit {
            "B" => 1.0,
            "K" => 1024.0,
            "M" => 1024.0 * 1024.0,
            "G" => 1024.0 * 1024.0 * 1024.0,
            "T" => 1024.0f64.powi(4),
            _ => return None,
        };
        (n.is_finite() && n >= 0.0).then(|| (n * scale).round() as u64)
    };
    Some((value("total")?, value("used")?))
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
    #[test]
    fn memory_max_is_a_limit_or_none() {
        assert_eq!(super::parse_memory_max("max\n"), Some(None));
        assert_eq!(super::parse_memory_max("1073741824\n"), Some(Some(1 << 30)));
        assert_eq!(super::parse_memory_max(""), None);
    }

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
        let a = cpu_total(&parse_proc_stat_cpus(stat_a))
            .expect("cpu line")
            .ticks;
        let b = cpu_total(&parse_proc_stat_cpus(stat_b))
            .expect("cpu line")
            .ticks;
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
        let cpus = parse_proc_stat_cpus(text);
        assert_eq!(core_count(&cpus), Some(3));
        assert_eq!(
            cpus.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![None, Some(0), Some(1), Some(11)],
            "an offline CPU leaves a gap in the numbers, not a renumbering"
        );
        assert_eq!(core_count(&parse_proc_stat_cpus("intr 5\n")), None);
        assert_eq!(cpu_total(&parse_proc_stat_cpus("intr 5\n")), None);
        // An old kernel with four fields still reads.
        assert_eq!(
            cpu_total(&parse_proc_stat_cpus("cpu 1 2 3 4\n")).map(|c| c.ticks),
            Some([1, 2, 3, 4, 0, 0, 0, 0])
        );
    }

    /// Eight cores with a `steal` column and the two guest columns that
    /// `user` and `nice` already count.
    const STAT_8_A: &str = "\
cpu  8000 0 2000 70000 400 0 100 1500 900 0
cpu0 1000 0 250 8750 50 0 13 187 100 0
cpu1 1000 0 250 8750 50 0 13 187 100 0
cpu2 1000 0 250 8750 50 0 13 187 100 0
cpu3 1000 0 250 8750 50 0 13 187 100 0
cpu4 1000 0 250 8750 50 0 12 188 100 0
cpu5 1000 0 250 8750 50 0 12 188 100 0
cpu6 1000 0 250 8750 50 0 12 188 100 0
cpu7 1000 0 250 8750 50 0 12 188 100 0
intr 123456 0 0
ctxt 987654
btime 1700000000
processes 4242
procs_running 3
procs_blocked 0
";

    #[test]
    fn each_core_is_busy_for_everything_but_idle_and_iowait() {
        let a = parse_proc_stat_cpus(STAT_8_A);
        assert_eq!(a.len(), 9, "the total and eight cores");
        assert_eq!(a[0].id, None, "index 0 is the cpu total");
        assert_eq!(core_count(&a), Some(8));
        assert_eq!(a[1].ticks, [1000, 0, 250, 8750, 50, 0, 13, 187]);
        // 200 ticks later on cpu0: 150 user, 10 iowait, 20 idle, 20 steal.
        // Busy is user + steal over everything: 170 / 200.
        let mut b0 = a[1];
        b0.ticks[0] += 150;
        b0.ticks[4] += 10;
        b0.ticks[3] += 20;
        b0.ticks[7] += 20;
        let busy = core_busy(&a[1], &b0).expect("ticks passed");
        assert!((busy - 0.85).abs() < 1e-9, "{busy}");
        // An idle core is zero, measured.
        let mut idle = a[2];
        idle.ticks[3] += 200;
        assert_eq!(core_busy(&a[2], &idle), Some(0.0));
        // No tick passed, or a counter went backwards: not measured.
        assert_eq!(core_busy(&a[1], &a[1]), None);
        assert_eq!(core_busy(&b0, &a[1]), None);
        // iowait alone going backwards, which the kernel allows: still a
        // reading. 100 user and 100 idle with iowait 5 down is 195 ticks,
        // 95 of them busy.
        let mut dip = a[1];
        dip.ticks[0] += 100;
        dip.ticks[3] += 100;
        dip.ticks[4] -= 5;
        let busy = core_busy(&a[1], &dip).expect("an iowait dip is not a failed read");
        assert!((busy - 95.0 / 195.0).abs() < 1e-9, "{busy}");
    }

    #[test]
    fn loadavg_is_its_first_three_fields() {
        assert_eq!(
            parse_loadavg("3.90 2.81 2.10 4/1234 56789\n"),
            Some((3.9, 2.81, 2.1))
        );
        assert_eq!(
            parse_loadavg("0.00 0.01 0.05 1/90 7\n"),
            Some((0.0, 0.01, 0.05))
        );
        assert_eq!(parse_loadavg("3.90 2.81\n"), None);
        assert_eq!(parse_loadavg("x 1 2 3/4 5\n"), None);
    }

    const MEMINFO: &str = "\
MemTotal:       32768000 kB
MemFree:         7340032 kB
MemAvailable:   13631488 kB
Buffers:          204800 kB
Cached:          6000000 kB
SwapCached:        10240 kB
SwapTotal:       4194304 kB
SwapFree:        2936013 kB
";

    #[test]
    fn memory_is_three_parts_that_sum_to_the_total() {
        let m = parse_meminfo(MEMINFO).expect("parses");
        assert_eq!(m.total, 32_768_000 * 1024);
        assert!(m.available > m.free);
        let [used, reclaimable, free] = memory_parts(&m);
        assert_eq!(
            used,
            (32_768_000 - 13_631_488) * 1024,
            "total - MemAvailable"
        );
        assert_eq!(reclaimable, (13_631_488 - 7_340_032) * 1024);
        assert_eq!(free, 7_340_032 * 1024, "MemFree");
        assert_eq!(used + reclaimable + free, m.total);
        let shares = memory_shares(&m);
        assert!(
            (shares.iter().sum::<f64>() - 1.0).abs() < 1e-9,
            "{shares:?}"
        );
        assert!((shares[0] - used as f64 / m.total as f64).abs() < 1e-9);
        assert_eq!(
            (m.swap_total, m.swap_free),
            (4_194_304 * 1024, 2_936_013 * 1024)
        );

        // The kernel's reserve can put MemAvailable under MemFree: nothing
        // is reclaimable, and the parts still sum to the total.
        let tight = MemInfo {
            available: m.free - 1024,
            ..m
        };
        let parts = memory_parts(&tight);
        assert_eq!(parts[1], 0);
        assert_eq!(parts.iter().sum::<u64>(), m.total);

        // No MemAvailable (a kernel before 3.14): not guessed at.
        let old = MEMINFO.replace("MemAvailable:   13631488 kB\n", "");
        assert_eq!(parse_meminfo(&old), None);
        assert_eq!(parse_meminfo_total(&old), Some(32_768_000 * 1024));
    }

    #[test]
    fn vm_stat_reads_its_own_page_size() {
        let text = "\
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                5000.
Pages active:                            300000.
Pages inactive:                          280000.
Pages speculative:                        20000.
Pages throttled:                              0.
Pages wired down:                        150000.
Pages purgeable:                          12000.
\"Translation faults\":                 123456789.
Pages copy-on-write:                    4567890.
Pages zero filled:                     98765432.
Pages reactivated:                       123456.
Pages purged:                             65432.
File-backed pages:                       200000.
Anonymous pages:                         400000.
Pages stored in compressor:              250000.
Pages occupied by compressor:             50000.
Decompressions:                          111111.
Compressions:                            222222.
Pageins:                                 333333.
Pageouts:                                  4444.
Swapins:                                      0.
Swapouts:                                     0.
";
        let v = parse_vm_stat(text).expect("parses");
        assert_eq!(v.page_size, 16_384, "not the 4096 an Intel Mac has");
        assert_eq!(
            (
                v.free,
                v.active,
                v.inactive,
                v.speculative,
                v.wired,
                v.compressor
            ),
            (5_000, 300_000, 280_000, 20_000, 150_000, 50_000)
        );
        let [used, reclaimable, free] = vm_stat_parts(&v);
        assert_eq!(used, (300_000 + 150_000 + 50_000) * 16_384);
        assert_eq!(reclaimable, 300_000 * 16_384);
        assert_eq!(free, 5_000 * 16_384);
        let s = shares(vm_stat_parts(&v));
        assert!((s.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        // Without the page-size line nothing is assumed.
        let headless: String = text.lines().skip(1).collect::<Vec<_>>().join("\n");
        assert_eq!(parse_vm_stat(&headless), None);
    }

    #[test]
    fn swapusage_reads_each_unit() {
        assert_eq!(
            parse_swapusage("total = 2048.00M  used = 1.00M  free = 2047.00M  (encrypted)\n"),
            Some((2048 << 20, 1 << 20))
        );
        assert_eq!(
            parse_swapusage("total = 4.00G  used = 1536.50M  free = 2559.50M  (encrypted)"),
            Some((4 << 30, (1536.5 * 1048576.0) as u64))
        );
        assert_eq!(
            parse_swapusage("total = 0.00M  used = 0.00M  free = 0.00M  (encrypted)"),
            Some((0, 0))
        );
        assert_eq!(parse_swapusage("vm.swapusage: unknown oid"), None);
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
    1     0     0 root               0.0  0.1  12345 Ss   12-03:04:05 /sbin/launchd
  501     1   501 user-a             3.5  1.2 204800 U    01:02:03 /Applications/Example App.app/Contents/MacOS/Example App
  777   501   501 user-a             0.0  0.0    900 I    05:09 example-worker
  778   501   501 user-a             0.0  0.0    900 Z    00:01 (example-zombie)
  779   501   501 user-a             0.0  0.0    900 S    00:01
garbage line
";
        let rows = parse_ps_table(text);
        assert_eq!(rows.len(), 4, "a row with no command is skipped: {rows:#?}");

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
            (app.user.as_str(), app.uid, app.rss_kb),
            ("user-a", 501, 204_800)
        );
        assert_eq!(app.cpu, 3.5);
        assert_eq!(app.running, 3723.0);

        let worker = &rows[2];
        assert_eq!(worker.state, 'S', "I is idle sleep");
        assert_eq!(worker.comm, "example-worker");
        assert_eq!(worker.running, 309.0);

        assert_eq!(rows[3].state, 'Z');
        assert_eq!(rows[3].comm, "(example-zombie)");
    }

    #[test]
    fn a_long_path_is_named_by_its_last_part_not_cut_at_sixteen() {
        // The name is the last column, so nothing pads or cuts it.
        let text = "  901     1   501 user-a   0.0  0.0  900 S  00:01 /Users/example/work/project/target/debug/deps/procs-0123456789abcdef\n";
        let rows = parse_ps_table(text);
        assert_eq!(rows[0].comm, "procs-0123456789abcdef");
        assert!(PS_COLUMNS.ends_with(",comm="), "{PS_COLUMNS}");
        assert!(PS_FLAGS.ends_with("ww"), "{PS_FLAGS}");
    }

    #[test]
    fn a_comma_locale_still_reads_every_row() {
        // ps calls setlocale, so a locale with a decimal comma prints
        // `%cpu` and `%mem` as `3,5`: every row must still read, or the
        // table is empty and looks like a machine running nothing.
        let text = "\
    1     0     0 root               0,0  0,1  12345 Ss   12-03:04:05 /sbin/launchd
  501     1   501 user-a            12,5  1,2 204800 R    01:02:03 /usr/bin/example
";
        let rows = parse_ps_table(text);
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!((rows[1].cpu, rows[1].mem), (12.5, 1.2));
        assert_eq!(
            parse_swapusage("total = 2048,00M  used = 1,50M  free = 2046,50M  (encrypted)"),
            Some((2048 << 20, 1_572_864))
        );
    }
}
