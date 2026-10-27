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

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::parse::{self, InventoryDisk, IoSnapshot, Mount, PartMount, Space};

struct InventoryCache {
    at: Instant,
    disks: Vec<InventoryDisk>,
}

static INVENTORY: Mutex<Option<InventoryCache>> = Mutex::new(None);

pub fn read_mounts() -> Result<Vec<Mount>, String> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").map_err(|err| err.to_string())?;
    Ok(parse::parse_mountinfo(&text))
}

pub fn stat_mount(path: &str) -> Result<Space, String> {
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
        host_devices: host_cgroup(),
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

fn host_cgroup() -> bool {
    std::fs::read_to_string("/proc/1/cgroup")
        .map(|text| parse::cgroup_is_host(&text))
        .unwrap_or(false)
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
    let layout = parse::layout_from_links(&links);
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
    let text = read_text(path)?;
    if text.contains(':') {
        return Some(text);
    }
    let mut parts = text.split_whitespace();
    let major = parts.next()?;
    let minor = parts.next()?;
    Some(format!("{major}:{minor}"))
}
