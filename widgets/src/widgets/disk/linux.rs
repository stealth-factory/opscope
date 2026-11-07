// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Where the bytes come from on Linux.
//!
//! The mount table is `/proc/self/mountinfo`, not `/proc/mounts`: it carries
//! the device numbers and the bind root, so a duplicate can be recognised
//! without stating a mount that may hang. Space comes from `statvfs` on the
//! mount point, and that call is the one the poller times out.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use opscope_core::cgroup::State;
use opscope_core::procfs;

use super::parse::{self, InventoryDisk, IoSnapshot, Mount, PartMount, Space};
use super::by_process::{self, FileMemory, ProcDetail, ProcMemory, Source};
use super::writers::{self, Memory, PidSample, Wanted, Writers};

struct InventoryCache {
    at: Instant,
    disks: Vec<InventoryDisk>,
}

static INVENTORY: Mutex<Option<InventoryCache>> = Mutex::new(None);

pub fn read_mounts() -> Result<Vec<Mount>, String> {
    // Bytes, not `read_to_string`: one mount point that is not UTF-8 would
    // otherwise fail this read and the pane would show no mounts at all.
    let bytes = std::fs::read("/proc/self/mountinfo").map_err(|err| err.to_string())?;
    Ok(parse::parse_mountinfo_bytes(&bytes))
}

pub fn stat_mount(path: &[u8]) -> Result<Space, String> {
    let c_path =
        std::ffi::CString::new(path).map_err(|_| "mount point contains a nul".to_string())?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(parse::space_from_blocks(
        stat.f_blocks as u64,
        stat.f_bfree as u64,
        stat.f_bavail as u64,
        stat.f_files as u64,
        stat.f_ffree as u64,
        stat.f_frsize as u64,
    ))
}

/// Device counters and, at most once a minute, the sysfs inventory.
///
/// `/proc/diskstats` is read on every call. `/sys/block` is not: the names,
/// kinds, sizes and partition map change slowly, and walking sysfs on the
/// poll cadence would be the cost this pane does not need. A failed
/// inventory keeps the previous one. Mount points are joined from `mounts`
/// every call, because those do move.
pub fn sample_io(mounts: &[Mount], _every: f64) -> Result<IoSnapshot, String> {
    let text = std::fs::read_to_string("/proc/diskstats")
        .map_err(|err| format!("diskstats unavailable: {err}"))?;
    let stats = parse::parse_diskstats(&text);
    let (inventory, inventory_err) = match inventory_cached() {
        Ok(disks) => (disks, String::new()),
        Err(err) => (Vec::new(), err),
    };
    let mut disks = parse::live_disks(&stats, &inventory);
    parse::attach_mounts(&mut disks, mounts);
    let mut snap = IoSnapshot {
        disks,
        show_pressure: true,
        host_devices: opscope_core::cgroup::in_container(),
        err: inventory_err,
        ..IoSnapshot::default()
    };
    match std::fs::read_to_string("/proc/pressure/io") {
        Ok(text) => match parse::parse_pressure(&text) {
            Some(pressure) => snap.pressure = Some(pressure),
            None => snap.pressure_absent = true,
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            snap.pressure_absent = true;
        }
        Err(err) => snap.pressure_err = format!("pressure unavailable: {err}"),
    }
    Ok(snap)
}

fn inventory_cached() -> Result<Vec<InventoryDisk>, String> {
    let mut guard = INVENTORY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fresh = guard
        .as_ref()
        .is_some_and(|cache| cache.at.elapsed() < Duration::from_secs(60));
    if fresh {
        return Ok(guard
            .as_ref()
            .map(|cache| cache.disks.clone())
            .unwrap_or_default());
    }
    match read_inventory() {
        Ok(disks) => {
            *guard = Some(InventoryCache {
                at: Instant::now(),
                disks: disks.clone(),
            });
            Ok(disks)
        }
        Err(err) => match guard.as_ref() {
            Some(cache) => Ok(cache.disks.clone()),
            None => Err(err),
        },
    }
}

fn read_inventory() -> Result<Vec<InventoryDisk>, String> {
    let mut links = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir("/sys/dev/block") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Ok(target) = std::fs::read_link(entry.path()) {
                links.insert(
                    format!("dev/block/{name}"),
                    target.to_string_lossy().into_owned(),
                );
            }
        }
    }
    let block = std::fs::read_dir("/sys/block")
        .map_err(|err| format!("block inventory unavailable: {err}"))?;
    let mut names = Vec::new();
    for entry in block.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.is_empty() || name.starts_with('.') {
            continue;
        }
        let slaves = format!("/sys/block/{name}/slaves");
        if let Ok(entries) = std::fs::read_dir(&slaves) {
            for slave in entries.flatten() {
                let Some(slave_name) = slave.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let target = std::fs::read_link(slave.path())
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or(slave_name.clone());
                links.insert(format!("block/{name}/slaves/{slave_name}"), target);
            }
        }
        names.push(name);
    }
    let known: BTreeSet<String> = names.iter().cloned().collect();
    let layout = parse::layout_from_links(&links, &known);
    let mut disks = Vec::with_capacity(names.len());
    for name in names {
        let mut partitions = Vec::new();
        for (part, disk) in &layout.partitions {
            if disk != &name {
                continue;
            }
            let dev = read_dev(&format!("/sys/class/block/{part}/dev"))
                .or_else(|| read_dev(&format!("/sys/block/{name}/{part}/dev")))
                .unwrap_or_default();
            partitions.push(PartMount {
                name: part.clone(),
                dev,
                mounts: Vec::new(),
            });
        }
        partitions.sort_by(|left, right| left.name.cmp(&right.name));
        disks.push(InventoryDisk {
            kind: kind_of(&name).to_string(),
            size: read_text(&format!("/sys/block/{name}/size"))
                .and_then(|text| parse::parse_sys_block_size(&text))
                .unwrap_or(0),
            model: read_text(&format!("/sys/block/{name}/device/model"))
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            dev: read_dev(&format!("/sys/block/{name}/dev")).unwrap_or_default(),
            slaves: layout.slaves.get(&name).cloned().unwrap_or_default(),
            name,
            partitions,
        });
    }
    disks.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(disks)
}

fn kind_of(name: &str) -> &'static str {
    if !std::path::Path::new(&format!("/sys/block/{name}/device")).exists() {
        return "virtual";
    }
    match read_text(&format!("/sys/block/{name}/queue/rotational"))
        .as_deref()
        .and_then(parse::parse_rotational)
    {
        Some(true) => "hdd",
        Some(false) => "ssd",
        None => "virtual",
    }
}

fn read_text(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

fn read_dev(path: &str) -> Option<String> {
    parse::parse_dev(&read_text(path)?)
}

/// What the WHO IS WRITING poll keeps between passes: the previous tree
/// and wait ticks (in [`Memory`]), one previous `io` reading per pid (in
/// [`ProcMemory`]), the open process's previous file sizes, and when
/// `task_delayacct` and the `/proc` mount's `hidepid` were last read.
pub struct WriterSource {
    memory: Memory,
    procs: ProcMemory,
    files: Option<FileMemory>,
    delayacct: Option<(bool, Instant)>,
    hidepid: Option<(bool, Instant)>,
    start: Instant,
}

impl Default for WriterSource {
    fn default() -> Self {
        Self {
            memory: Memory::default(),
            procs: ProcMemory::default(),
            files: None,
            delayacct: None,
            hidepid: None,
            start: Instant::now(),
        }
    }
}

/// One cgroup walk and one `/proc` pass. `wanted` names the cgroup whose
/// detail screen is open (its `io.pressure` is read with the walk) and the
/// process whose detail is open (its command line, working directory and
/// open files are read with the pass).
///
/// Per pid: `stat` (state, ppid, field 42) and `io`, which is both whether
/// this user can read it and what the BY PROCESS rates diff. The cgroup of
/// a pid comes from the walk's `cgroup.procs`; only a process in D state
/// the walk did not list has `/proc/<pid>/cgroup` read for it.
pub fn sample_writers(source: &mut WriterSource, wanted: Wanted) -> Option<Writers> {
    let state = opscope_core::cgroup::read(&["io"], true);
    let fresh = source
        .delayacct
        .is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(60));
    if !fresh {
        // Missing on a kernel without delay accounting: that is off.
        let on = std::fs::read_to_string("/proc/sys/kernel/task_delayacct")
            .map(|text| writers::parse_task_delayacct(&text))
            .unwrap_or(false);
        source.delayacct = Some((on, Instant::now()));
    }
    let delayacct = source.delayacct.is_some_and(|(on, _)| on);
    let fresh = source
        .hidepid
        .is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(60));
    if !fresh {
        // Unreadable is not hidden: the count then says every process.
        let on = std::fs::read_to_string("/proc/self/mountinfo")
            .map(|text| by_process::parse_hidepid(&text))
            .unwrap_or(false);
        source.hidepid = Some((on, Instant::now()));
    }
    let visible = source.hidepid.is_some_and(|(on, _)| on);
    let listed: BTreeSet<i32> = match &state {
        State::Ok(tree) | State::Container(tree) => tree
            .nodes
            .iter()
            .flat_map(|node| node.procs.iter().flatten().copied())
            .collect(),
        _ => BTreeSet::new(),
    };
    let rows_of = writers::pid_rows(&state);
    let (samples, pid_err) = match procfs::pids() {
        Ok(pids) => (pass(&pids, &listed), String::new()),
        Err(err) => (Vec::new(), format!("processes unavailable: {err}")),
    };
    let now = source.start.elapsed().as_secs_f64();
    let proc_samples = by_process::samples_from_pids(&samples, &rows_of);
    let state_of = wanted
        .pid
        .and_then(|pid| samples.iter().find(|s| s.pid == pid).map(|s| s.state));
    let mut out = writers::step(
        &mut source.memory,
        state,
        samples,
        pid_err,
        delayacct,
        now,
        procfs::clock_ticks(),
    );
    // SAFETY: geteuid has no preconditions and cannot fail.
    let root = unsafe { libc::geteuid() } == 0;
    out.procs = Some(by_process::step(
        &mut source.procs,
        proc_samples,
        now,
        root,
        visible,
        Source::Procfs,
    ));
    out.detail = wanted.pid.map(|pid| {
        let facts = procfs::process_facts(pid);
        let files = procfs::open_files(pid)
            .map(|files| by_process::file_growth(&mut source.files, pid, &files, now));
        ProcDetail {
            pid,
            state: state_of,
            cmdline: facts.cmdline,
            cwd: facts.cwd,
            files: Some(files),
        }
    });
    if wanted.pid.is_none() {
        source.files = None;
    }
    out.pressure = wanted.cgroup.map(|path| {
        let file = format!("/sys/fs/cgroup/{path}/io.pressure");
        let reading = match std::fs::read_to_string(&file) {
            Ok(text) => {
                parse::parse_pressure(&text).ok_or_else(|| "not in the expected shape".to_string())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Err("not available (no PSI on this kernel, or the cgroup has gone)".to_string())
            }
            Err(err) => Err(format!("unreadable: {err}")),
        };
        (path, reading)
    });
    Some(out)
}

fn pass(pids: &[i32], listed: &BTreeSet<i32>) -> Vec<PidSample> {
    let mut out = Vec::with_capacity(pids.len());
    for &pid in pids {
        // A pid that has gone since the listing is dropped, not zeroed.
        let Ok(stat) = procfs::read_stat(pid) else {
            continue;
        };
        let cgroup = (stat.state == 'D' && !listed.contains(&pid))
            .then(|| procfs::read_cgroup(pid).ok())
            .flatten();
        out.push(PidSample {
            pid,
            comm: stat.comm,
            state: stat.state,
            ppid: stat.ppid,
            starttime: stat.starttime,
            blkio_ticks: stat.delayacct_blkio_ticks,
            io: procfs::read_io(pid).ok(),
            cgroup,
        });
    }
    out
}

/// Command line and working directory for the process detail, read once
/// when it opens so the first frame has them. Empty where this user cannot
/// read them. The poller reads them again every poll after that.
pub fn process_facts(pid: i32) -> (String, String) {
    let facts = procfs::process_facts(pid);
    (facts.cmdline, facts.cwd)
}
