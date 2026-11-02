// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Mount tables, disk counters and the arithmetic both hosts share.
//!
//! Nothing here opens a file or runs a tool. Linux hands `parse_mountinfo`
//! the text of its mount table and `parse_diskstats` the text of
//! `/proc/diskstats`. macOS builds the same `Mount` values from
//! `getmntinfo` and the same device rows from `iostat` lines.

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// One mount, after the kernel's escapes have been decoded.
///
/// `dev` is the identity bind duplicates share. On Linux that is the
/// `major:minor` from the mount table. On macOS the same field holds the
/// mount-from name, which is what those duplicates share there. One
/// function can then keep the shortest path on either host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub dev: String,
    pub root: String,
    /// The mount point as text. Bytes that are not UTF-8 become the
    /// replacement character, so this is what the pane draws and what
    /// `hide_mounts` compares. `path` keeps those bytes for `statvfs`.
    pub mount_point: String,
    /// Mount-point bytes after octal escapes, with no text conversion.
    pub path: Vec<u8>,
    pub fstype: String,
    pub source: String,
    pub read_only: bool,
    pub options: String,
}

/// How many mounts each rule held back. A mount is counted once, under the
/// first rule that hid it: covered by a later mount at the same path, then
/// a configured path, then a pseudo filesystem, then a read-only mount,
/// then a bind of something still visible.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hidden {
    pub pseudo: usize,
    pub bind: usize,
    pub read_only: usize,
    pub configured: usize,
    /// Mounted over by a later mount at the same path, so `statvfs` on that
    /// path can only ever answer for the one on top.
    pub covered: usize,
}

impl Hidden {
    /// The categories that actually hid something, in the order the header
    /// names them. That order is not the order the rules are applied.
    /// Empty when every mount is on screen.
    pub fn categories(self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.pseudo > 0 {
            names.push("pseudo");
        }
        if self.bind > 0 {
            names.push("bind");
        }
        if self.read_only > 0 {
            names.push("read-only");
        }
        if self.configured > 0 {
            names.push("hide_mounts");
        }
        if self.covered > 0 {
            names.push("covered");
        }
        names
    }
}

/// Byte and inode counts from one `statvfs` / `statfs`.
///
/// `usable` is `f_bavail` (what an unprivileged writer can still fill) and
/// `reserved` is the root reserve `f_bfree` still includes. They are not
/// the same number, and the pane says which one it is drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Space {
    pub size: u64,
    pub used: u64,
    pub usable: u64,
    pub reserved: u64,
    pub files: u64,
    pub ffree: u64,
}

/// Kernel filesystems that cannot fill a disk. `tmpfs` is absent on
/// purpose: it can.
pub const PSEUDO: &[&str] = &[
    "proc",
    "sysfs",
    "cgroup",
    "cgroup2",
    "devpts",
    "devtmpfs",
    "debugfs",
    "tracefs",
    "securityfs",
    "pstore",
    "bpf",
    "configfs",
    "fusectl",
    "hugetlbfs",
    "mqueue",
    "autofs",
    "binfmt_misc",
    "efivarfs",
    "rpc_pipefs",
    "nsfs",
    "ramfs",
    "devfs",
    "fdesc",
];

const REMOTE: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "smbfs", "afpfs", "sshfs"];

pub fn is_remote(fstype: &str) -> bool {
    REMOTE.contains(&fstype) || fstype.starts_with("fuse.")
}

/// `blocks`/`bfree`/`bavail` are fragment counts; `frsize` is the fragment
/// size. A zero fragment size stays zero rather than being invented as 1.
pub fn space_from_blocks(
    blocks: u64,
    bfree: u64,
    bavail: u64,
    files: u64,
    ffree: u64,
    frsize: u64,
) -> Space {
    let used_blocks = blocks.saturating_sub(bfree);
    Space {
        size: blocks.saturating_mul(frsize),
        used: used_blocks.saturating_mul(frsize),
        usable: bavail.saturating_mul(frsize),
        reserved: bfree.saturating_sub(bavail).saturating_mul(frsize),
        files,
        ffree,
    }
}

/// `df`'s fraction: used / (used + usable). The root reserve is not part
/// of the denominator, which is why this is not `used / size`.
pub fn percent_used(used: u64, usable: u64) -> f64 {
    let denom = used.saturating_add(usable);
    if denom == 0 {
        0.0
    } else {
        used as f64 / denom as f64
    }
}

/// `None` when the filesystem does not count inodes (`f_files == 0`).
pub fn inode_percent(files: u64, ffree: u64) -> Option<f64> {
    if files == 0 {
        None
    } else {
        Some(files.saturating_sub(ffree) as f64 / files as f64)
    }
}

/// The mount table Linux publishes, one mount per line.
///
/// This is the `&str` entry the other parsers have. Linux reads the file as
/// bytes and calls [`parse_mountinfo_bytes`], because the table is not
/// always text.
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    parse_mountinfo_bytes(text.as_bytes())
}

/// The same table as [`parse_mountinfo`], read as the bytes the kernel wrote.
///
/// Optional tag fields sit between the mount options and the ` - `
/// separator. A line that does not have that separator is skipped: a
/// partial read is not a mount.
///
/// Space, tab, newline and backslash are octal escapes. Every other byte is
/// raw, including ones that are not UTF-8. Turning the whole file into text
/// first would fail the read, or replace those bytes, and `statvfs` would
/// then miss a mount that is really there. `Mount::path` keeps the bytes;
/// `mount_point` is only the text the pane shows.
pub fn parse_mountinfo_bytes(text: &[u8]) -> Vec<Mount> {
    let mut out = Vec::new();
    for line in text.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&[u8]> = line
            .split(|byte| *byte == b' ')
            .filter(|field| !field.is_empty())
            .collect();
        let Some(sep) = fields.iter().position(|field| *field == b"-") else {
            continue;
        };
        if sep < 6 || fields.len() < sep + 3 {
            continue;
        }
        let path = unescape(fields[4]);
        let options = text_of(&unescape(fields[5]));
        out.push(Mount {
            dev: text_of(fields[2]),
            root: text_of(&unescape(fields[3])),
            mount_point: text_of(&path),
            path,
            fstype: text_of(&unescape(fields[sep + 1])),
            source: text_of(&unescape(fields[sep + 2])),
            read_only: options.split(',').any(|option| option == "ro"),
            options,
        });
    }
    out
}

/// Mounts worth a row, and why the rest were held back.
///
/// Bind duplicates share `dev`. The shortest mount point stays; an equal
/// length keeps the earlier line. A pattern in `hide_mounts` matches the
/// mount point exactly, or as a directory prefix when the pattern ends in
/// `/`. The pattern `/` is the root mount only — a prefix of `/` would
/// hide every absolute path, which is not a path anyone names in order to
/// hide one filesystem.
pub fn visible_mounts(mounts: &[Mount], hide_mounts: &[String]) -> (Vec<Mount>, Hidden) {
    let mut hidden = Hidden::default();
    let mut rest: Vec<&Mount> = Vec::new();
    // The mount table lists mounts in the order they were made, so the last
    // one at a path is the one a lookup by that path reaches. The path is
    // the bytes `statvfs` will be given, not the text shown for it.
    let mut top: HashMap<&[u8], usize> = HashMap::new();
    for (index, mount) in mounts.iter().enumerate() {
        top.insert(mount.path.as_slice(), index);
    }
    for (index, mount) in mounts.iter().enumerate() {
        if top.get(mount.path.as_slice()).copied() != Some(index) {
            hidden.covered += 1;
            continue;
        }
        if hidden_by_config(&mount.mount_point, hide_mounts) {
            hidden.configured += 1;
        } else if PSEUDO.contains(&mount.fstype.as_str()) {
            hidden.pseudo += 1;
        } else if mount.read_only {
            hidden.read_only += 1;
        } else {
            rest.push(mount);
        }
    }

    let mut best: HashMap<String, usize> = HashMap::new();
    for (index, mount) in rest.iter().enumerate() {
        match best.get(&mount.dev) {
            None => {
                best.insert(mount.dev.clone(), index);
            }
            Some(&kept) => {
                let shorter =
                    mount.mount_point.chars().count() < rest[kept].mount_point.chars().count();
                if shorter {
                    best.insert(mount.dev.clone(), index);
                }
            }
        }
    }
    let mut visible = Vec::new();
    for (index, mount) in rest.into_iter().enumerate() {
        if best.get(&mount.dev) == Some(&index) {
            visible.push(mount.clone());
        } else {
            hidden.bind += 1;
        }
    }
    (visible, hidden)
}

fn hidden_by_config(mount: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        if pattern == "/" {
            mount == "/"
        } else if let Some(prefix) = pattern.strip_suffix('/') {
            mount == prefix || mount.starts_with(pattern.as_str())
        } else {
            mount == pattern
        }
    })
}

/// `\040` and the other octal escapes the mount table uses, as bytes.
///
/// A backslash that is not three octal digits is left as it was written.
/// The window is taken from the byte slice: slicing the text there panics
/// when the next bytes sit inside a multibyte character, and that panic
/// stops the poller.
fn unescape(raw: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'\\' && index + 3 < raw.len() {
            let octal = &raw[index + 1..index + 4];
            if octal.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
                let value = octal
                    .iter()
                    .fold(0u32, |acc, digit| acc * 8 + u32::from(digit - b'0'));
                if let Ok(value) = u8::try_from(value) {
                    out.push(value);
                    index += 4;
                    continue;
                }
            }
        }
        out.push(raw[index]);
        index += 1;
    }
    out
}

fn text_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// One line of `/proc/diskstats`. Counters are cumulative since boot.
///
/// The kernel writes 14 fields, 18 since 4.18 (discards) or 20 since 5.5
/// (flush). Anything past the eleventh counter is discarded: the rates
/// below do not use it. Sectors in this file are always 512 bytes, whatever
/// the device itself claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskStat {
    pub major: u32,
    pub minor: u32,
    pub name: String,
    pub reads: u64,
    pub reads_merged: u64,
    pub sectors_read: u64,
    pub ms_reading: u64,
    pub writes: u64,
    pub writes_merged: u64,
    pub sectors_written: u64,
    pub ms_writing: u64,
    pub in_flight: u64,
    pub ms_io: u64,
    pub weighted_ms_io: u64,
}

/// Bytes in one `/proc/diskstats` sector. Not the device's logical sector.
pub const SECTOR_BYTES: u64 = 512;

pub fn parse_diskstats(text: &str) -> Vec<DiskStat> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(major) = parts.next().and_then(|token| token.parse().ok()) else {
            continue;
        };
        let Some(minor) = parts.next().and_then(|token| token.parse().ok()) else {
            continue;
        };
        let Some(name) = parts.next() else {
            continue;
        };
        let mut nums = Vec::with_capacity(11);
        for token in parts.take(11) {
            let Ok(value) = token.parse::<u64>() else {
                nums.clear();
                break;
            };
            nums.push(value);
        }
        if nums.len() < 11 {
            continue;
        }
        out.push(DiskStat {
            major,
            minor,
            name: name.to_string(),
            reads: nums[0],
            reads_merged: nums[1],
            sectors_read: nums[2],
            ms_reading: nums[3],
            writes: nums[4],
            writes_merged: nums[5],
            sectors_written: nums[6],
            ms_writing: nums[7],
            in_flight: nums[8],
            ms_io: nums[9],
            weighted_ms_io: nums[10],
        });
    }
    out
}

/// Rates between two samples of one device.
///
/// `busy` is the share of the interval during which at least one request
/// was in flight (`Δms_io / (elapsed × 1000)`), not a utilisation the
/// device cannot exceed. `queue` is `in_flight` at `next`, a gauge, so a
/// fall there is not a counter reset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rates {
    pub read_bps: f64,
    pub write_bps: f64,
    pub iops: f64,
    pub await_ms: Option<f64>,
    pub busy: f64,
    pub queue: u64,
}

pub fn disk_rates(prev: &DiskStat, next: &DiskStat, elapsed_s: f64) -> Option<Rates> {
    if elapsed_s <= 0.0 {
        return None;
    }
    let counters = [
        (prev.reads, next.reads),
        (prev.reads_merged, next.reads_merged),
        (prev.sectors_read, next.sectors_read),
        (prev.ms_reading, next.ms_reading),
        (prev.writes, next.writes),
        (prev.writes_merged, next.writes_merged),
        (prev.sectors_written, next.sectors_written),
        (prev.ms_writing, next.ms_writing),
        (prev.ms_io, next.ms_io),
        (prev.weighted_ms_io, next.weighted_ms_io),
    ];
    if counters.iter().any(|(before, after)| after < before) {
        return None;
    }
    let reads = next.reads - prev.reads;
    let writes = next.writes - prev.writes;
    let ios = reads + writes;
    let await_ms = (ios > 0).then(|| {
        let waited = (next.ms_reading - prev.ms_reading) + (next.ms_writing - prev.ms_writing);
        waited as f64 / ios as f64
    });
    Some(Rates {
        read_bps: (next.sectors_read - prev.sectors_read) as f64 * SECTOR_BYTES as f64 / elapsed_s,
        write_bps: (next.sectors_written - prev.sectors_written) as f64 * SECTOR_BYTES as f64
            / elapsed_s,
        iops: ios as f64 / elapsed_s,
        await_ms,
        busy: (next.ms_io - prev.ms_io) as f64 / (elapsed_s * 1000.0),
        queue: next.in_flight,
    })
}

/// One `/proc/pressure/io` sample. Averages are percents, already scaled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pressure {
    pub some: [f64; 3],
    pub full: [f64; 3],
    pub some_total: u64,
    pub full_total: u64,
}

pub fn parse_pressure(text: &str) -> Option<Pressure> {
    let mut some = None;
    let mut full = None;
    for line in text.lines() {
        let mut tokens = line.split_whitespace();
        let Some(kind) = tokens.next() else {
            continue;
        };
        let mut avgs = [0.0; 3];
        let mut total = 0u64;
        let mut saw = false;
        for token in tokens {
            if let Some(value) = token.strip_prefix("avg10=") {
                avgs[0] = value.parse().ok()?;
                saw = true;
            } else if let Some(value) = token.strip_prefix("avg60=") {
                avgs[1] = value.parse().ok()?;
            } else if let Some(value) = token.strip_prefix("avg300=") {
                avgs[2] = value.parse().ok()?;
            } else if let Some(value) = token.strip_prefix("total=") {
                total = value.parse().ok()?;
            }
        }
        if !saw {
            continue;
        }
        match kind {
            "some" => some = Some((avgs, total)),
            "full" => full = Some((avgs, total)),
            _ => {}
        }
    }
    let (some, some_total) = some?;
    let (full, full_total) = full.unwrap_or(([0.0; 3], 0));
    Some(Pressure {
        some,
        full,
        some_total,
        full_total,
    })
}

/// `/sys/block/<disk>/size` is a count of 512-byte sectors. The result is bytes.
pub fn parse_sys_block_size(text: &str) -> Option<u64> {
    let sectors: u64 = text.trim().parse().ok()?;
    Some(sectors.saturating_mul(SECTOR_BYTES))
}

/// `queue/rotational`: `0` is an SSD, `1` is spinning rust. Anything else,
/// including an empty read, means the file did not say.
pub fn parse_rotational(text: &str) -> Option<bool> {
    match text.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// One disk's triplet from `iostat -d -K -w`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IostatSample {
    pub kb_per_transfer: f64,
    pub tps: f64,
    pub mb_per_s: f64,
}

/// Disk names from an `iostat` header line. The `KB/t tps MB/s` line
/// contributes nothing: those words are the column titles, not disks.
///
/// The binary calls these only on macOS. They stay compiled everywhere so
/// the fixtures run on the Linux CI host too.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_iostat_header(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|token| token.chars().any(|ch| ch.is_ascii_alphabetic()))
        .filter(|token| !matches!(*token, "KB/t" | "tps" | "MB/s"))
        .map(|token| token.to_string())
        .collect()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_iostat_line(text: &str, disks: usize) -> Vec<IostatSample> {
    if disks == 0 {
        return Vec::new();
    }
    let nums: Vec<f64> = text
        .split_whitespace()
        .filter_map(|token| token.parse().ok())
        .collect();
    if nums.len() < disks * 3 {
        return Vec::new();
    }
    nums.chunks(3)
        .take(disks)
        .map(|chunk| IostatSample {
            kb_per_transfer: chunk[0],
            tps: chunk[1],
            mb_per_s: chunk[2],
        })
        .collect()
}

/// Where a partition sits, and which devices a stacked disk names under
/// `slaves/`.
///
/// `links` maps the path that was read to the symlink target `readlink`
/// returned. The last two components of a target are a partition when the
/// parent is one of `disks`. NVMe has no `block` directory
/// (`.../nvme0n1/nvme0n1p1`); SCSI does (`.../block/sda/sda1`). A path
/// containing `slaves` names the device one level under a dm or md disk.
/// No entry is opened here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SysLayout {
    pub partitions: BTreeMap<String, String>,
    pub slaves: BTreeMap<String, Vec<String>>,
}

pub fn layout_from_links(links: &BTreeMap<String, String>, disks: &BTreeSet<String>) -> SysLayout {
    let mut partitions = BTreeMap::new();
    let mut slaves: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (path, target) in links {
        if let Some(disk) = disk_before_slaves(path) {
            if let Some(slave) = target
                .rsplit('/')
                .find(|part| !part.is_empty() && *part != "..")
            {
                slaves.entry(disk).or_default().push(slave.to_string());
            }
            continue;
        }
        if let Some((disk, part)) = partition_in_target(target) {
            if disks.contains(&disk) {
                partitions.insert(part, disk);
            }
        }
    }
    for names in slaves.values_mut() {
        names.sort();
        names.dedup();
    }
    SysLayout { partitions, slaves }
}

fn disk_before_slaves(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let at = parts.iter().position(|part| *part == "slaves")?;
    if at == 0 {
        return None;
    }
    Some(parts[at - 1].to_string())
}

fn partition_in_target(target: &str) -> Option<(String, String)> {
    let mut parts = target
        .split('/')
        .filter(|part| !part.is_empty() && *part != "." && *part != "..");
    let part = parts.next_back()?;
    let disk = parts.next_back()?;
    Some((disk.to_string(), part.to_string()))
}

/// `maj:min` from a sysfs `dev` file. The file is either `8:0` or two
/// whitespace-separated numbers.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_dev(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.contains(':') {
        return Some(text.to_string());
    }
    let mut parts = text.split_whitespace();
    let major = parts.next()?;
    let minor = parts.next()?;
    Some(format!("{major}:{minor}"))
}

/// Names, the since-boot line, and later intervals from one `iostat` child.
///
/// `moved` sticks once an interval is non-zero, so a disk that was idle at
/// boot and then starts work stays visible after that interval goes quiet.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct IostatTrack {
    pub names: Vec<String>,
    pub latest: Vec<IostatSample>,
    pub boot: Vec<IostatSample>,
    pub moved: Vec<bool>,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn publish_iostat_track(
    track: &mut IostatTrack,
    names: &[String],
    latest: Option<Vec<IostatSample>>,
    boot: Option<Vec<IostatSample>>,
) {
    let renamed = track.names != names;
    track.names = names.to_vec();
    if boot.is_some() || renamed {
        if let Some(boot) = boot {
            track.boot = boot;
        } else if track.boot.len() != track.names.len() {
            track.boot.clear();
        }
        // The previous child's intervals are a different device list.
        track.latest.clear();
        track.moved = vec![false; track.names.len()];
    }
    if let Some(latest) = latest {
        if track.moved.len() != latest.len() {
            track.moved = vec![false; latest.len()];
        }
        for (index, sample) in latest.iter().enumerate() {
            if sample.tps != 0.0 || sample.mb_per_s != 0.0 {
                track.moved[index] = true;
            }
        }
        track.latest = latest;
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn iostat_disk_idle(track: &IostatTrack, index: usize) -> bool {
    let boot_idle = track
        .boot
        .get(index)
        .is_some_and(|sample| sample.tps == 0.0 && sample.mb_per_s == 0.0);
    boot_idle && !track.moved.get(index).copied().unwrap_or(false)
}

/// A whole disk as sysfs described it, before counters are attached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryDisk {
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub model: String,
    pub dev: String,
    pub partitions: Vec<PartMount>,
    pub slaves: Vec<String>,
}

/// A partition and the mount points whose `maj:min` is this partition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartMount {
    pub name: String,
    pub dev: String,
    pub mounts: Vec<String>,
}

/// One whole disk the table can show. Partitions are fields, never rows.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveDisk {
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub model: String,
    pub dev: String,
    pub stat: Option<DiskStat>,
    pub mac: Option<IostatSample>,
    pub idle: bool,
    pub partitions: Vec<PartMount>,
    pub mounts: Vec<String>,
    pub slaves: Vec<String>,
}

/// Whole disks only. A name in `stats` that is not in the inventory is a
/// partition (or something sysfs did not call a disk) and is left out, not
/// added into its parent.
pub fn live_disks(stats: &[DiskStat], inventory: &[InventoryDisk]) -> Vec<LiveDisk> {
    inventory
        .iter()
        .map(|disk| {
            let stat = stats.iter().find(|stat| stat.name == disk.name).cloned();
            let idle = stat
                .as_ref()
                .is_some_and(|stat| stat.reads == 0 && stat.writes == 0);
            LiveDisk {
                name: disk.name.clone(),
                kind: disk.kind.clone(),
                size: disk.size,
                model: disk.model.clone(),
                dev: disk.dev.clone(),
                stat,
                mac: None,
                idle,
                partitions: disk.partitions.clone(),
                mounts: Vec::new(),
                slaves: disk.slaves.clone(),
            }
        })
        .collect()
}

pub fn attach_mounts(disks: &mut [LiveDisk], mounts: &[Mount]) {
    for disk in disks {
        disk.mounts = mounts
            .iter()
            .filter(|mount| mount.dev == disk.dev)
            .map(|mount| mount.mount_point.clone())
            .collect();
        for part in &mut disk.partitions {
            part.mounts = mounts
                .iter()
                .filter(|mount| mount.dev == part.dev)
                .map(|mount| mount.mount_point.clone())
                .collect();
        }
    }
}

/// `on nvme0n1 (nvme0n1p2)` when the mount's device is that partition.
/// A mount of the whole disk is `on nvme0n1` with no partition in parentheses.
pub fn device_of(dev: &str, disks: &[LiveDisk]) -> Option<String> {
    for disk in disks {
        if let Some(part) = disk.partitions.iter().find(|part| part.dev == dev) {
            return Some(format!("on {} ({})", disk.name, part.name));
        }
        if !disk.dev.is_empty() && disk.dev == dev {
            return Some(format!("on {}", disk.name));
        }
    }
    None
}

/// How many whole disks each rule held back. A disk is counted once:
/// a configured name, then an idle disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskHidden {
    pub idle: usize,
    pub configured: usize,
}

impl DiskHidden {
    pub fn labels(self) -> Vec<String> {
        let mut names = Vec::new();
        if self.idle > 0 {
            names.push(format!("{} idle hidden", self.idle));
        }
        if self.configured > 0 {
            names.push(format!("{} hide_devices hidden", self.configured));
        }
        names
    }
}

pub fn visible_disk_indexes(
    disks: &[LiveDisk],
    show_idle: bool,
    hide: &[String],
) -> (Vec<usize>, DiskHidden) {
    let hidden_names: BTreeSet<&str> = hide.iter().map(String::as_str).collect();
    let mut hidden = DiskHidden::default();
    let mut shown = Vec::new();
    for (index, disk) in disks.iter().enumerate() {
        if hidden_names.contains(disk.name.as_str()) {
            hidden.configured += 1;
            continue;
        }
        if disk.idle && !show_idle {
            hidden.idle += 1;
            continue;
        }
        shown.push(index);
    }
    (shown, hidden)
}

/// What one poll learned about devices. `missing` is the macOS row shown
/// when `iostat` is not on `PATH`. `err` is a source that failed but did
/// not stop the filesystem sample. `host_devices` is set inside a container,
/// where `/proc/diskstats` is still the host's block devices.
#[derive(Clone, Debug, Default)]
pub struct IoSnapshot {
    pub disks: Vec<LiveDisk>,
    pub show_pressure: bool,
    pub pressure: Option<Pressure>,
    pub pressure_absent: bool,
    pub pressure_err: String,
    pub host_devices: bool,
    pub combined: bool,
    pub missing: String,
    pub err: String,
}

/// True when pid 1's environment names a container runtime.
///
/// `/proc/1/environ` is NUL-separated `KEY=value` pairs. An empty
/// `container=` does not count. A cgroup path is not this signal: `0::/`
/// is what pid 1 sees in a private cgroup namespace and on a host whose
/// init stays in the root cgroup.
pub fn parse_container_env(text: &str) -> bool {
    text.split('\0').any(|entry| {
        entry
            .strip_prefix("container=")
            .is_some_and(|value| !value.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
36 35 98:0 / / rw,relatime shared:1 - ext4 /dev/root rw,errors=remount-ro
37 35 98:0 /var /var/lib/app rw,relatime - ext4 /dev/root rw
40 35 0:21 / /proc rw,nosuid,nodev,noexec - proc proc rw
41 35 8:1 / /mnt/example-data ro,noatime - ext4 /dev/disk1 ro
42 35 8:2 / /mnt/space\\040name rw - xfs /dev/disk2 rw
43 35 9:0 / /mnt/opt rw shared:1 master:2 - ext4 /dev/disk3 rw
44 35 9:1 / /mnt/nfs rw - nfs4 storage.example:/export rw
";

    #[test]
    fn parse_mountinfo_keeps_binds_pseudos_readonly_escapes_and_tags() {
        let mounts = parse_mountinfo(TABLE);
        assert_eq!(mounts.len(), 7);
        assert_eq!(mounts[0].dev, "98:0");
        assert_eq!(mounts[0].mount_point, "/");
        assert!(!mounts[0].read_only);
        assert_eq!(mounts[1].mount_point, "/var/lib/app");
        assert_eq!(mounts[1].dev, mounts[0].dev);
        assert_eq!(mounts[2].fstype, "proc");
        assert!(mounts[3].read_only);
        assert_eq!(mounts[3].mount_point, "/mnt/example-data");
        assert_eq!(mounts[4].mount_point, "/mnt/space name");
        assert_eq!(mounts[5].fstype, "ext4");
        assert_eq!(mounts[5].source, "/dev/disk3");
        assert!(is_remote(&mounts[6].fstype));
        assert!(!is_remote("ext4"));
        assert!(is_remote("fuse.sshfs"));
    }

    #[test]
    fn visible_mounts_counts_each_category_and_keeps_the_shortest_bind() {
        let mounts = parse_mountinfo(TABLE);
        let (visible, hidden) = visible_mounts(&mounts, &[]);
        let points: Vec<&str> = visible
            .iter()
            .map(|mount| mount.mount_point.as_str())
            .collect();
        assert_eq!(points, vec!["/", "/mnt/space name", "/mnt/opt", "/mnt/nfs"]);
        assert_eq!(
            hidden,
            Hidden {
                pseudo: 1,
                bind: 1,
                read_only: 1,
                configured: 0,
                covered: 0,
            }
        );

        let (visible, hidden) = visible_mounts(&mounts, &["/mnt/".into()]);
        let points: Vec<&str> = visible
            .iter()
            .map(|mount| mount.mount_point.as_str())
            .collect();
        assert_eq!(points, vec!["/"]);
        // The read-only mount under the prefix counts as configured: the
        // reader's own rule is the one that hid it.
        assert_eq!(hidden.configured, 4);
        assert_eq!(hidden.read_only, 0);
        assert_eq!(hidden.pseudo, 1);
        assert_eq!(hidden.bind, 1);

        let (visible, hidden) = visible_mounts(&mounts, &["/".into()]);
        assert!(visible.iter().all(|mount| mount.mount_point != "/"));
        assert_eq!(hidden.configured, 1);
        // The longer bind is no longer a duplicate of a visible root.
        assert_eq!(hidden.bind, 0);
        assert!(
            visible
                .iter()
                .any(|mount| mount.mount_point == "/var/lib/app")
        );
    }

    #[test]
    fn percent_used_is_not_used_over_size_when_the_reserve_is_nonzero() {
        let space = space_from_blocks(100, 20, 10, 50, 40, 4096);
        assert_eq!(space.used, 80 * 4096);
        assert_eq!(space.usable, 10 * 4096);
        assert_eq!(space.reserved, 10 * 4096);
        let df = percent_used(space.used, space.usable);
        let used_over_size = space.used as f64 / space.size as f64;
        assert!((df - 80.0 / 90.0).abs() < 1e-9);
        assert!((df - used_over_size).abs() > 1e-9);
    }

    #[test]
    fn inode_percent_is_none_when_the_filesystem_does_not_count_them() {
        assert_eq!(inode_percent(0, 0), None);
        let share = inode_percent(100, 40).unwrap();
        assert!((share - 0.6).abs() < 1e-9);
    }

    #[test]
    fn a_non_ascii_mount_point_survives_decoding() {
        // `café` is UTF-8 in the table; only space, tab, newline and
        // backslash are octal escapes.
        let table = "50 35 8:3 / /mnt/caf\u{e9}\\040x rw,relatime - ext4 /dev/sdc1 rw\n";
        let mounts = parse_mountinfo(table);
        assert_eq!(mounts[0].mount_point, "/mnt/caf\u{e9} x");
        assert_eq!(mounts[0].path, "/mnt/caf\u{e9} x".as_bytes());
    }

    #[test]
    fn a_non_utf8_mount_point_keeps_its_bytes_and_the_next_mount() {
        // 0xFF is not UTF-8. The kernel writes it raw. Reading the table as
        // text would fail the whole sample, and a lossy path would make
        // `statvfs` miss this mount.
        let mut table = b"50 35 8:3 / /mnt/".to_vec();
        table.push(0xff);
        table.extend_from_slice(b" rw - ext4 /dev/sdc1 rw\n");
        table.extend_from_slice(b"51 35 8:4 / /home rw - ext4 /dev/sdd1 rw\n");
        let mounts = parse_mountinfo_bytes(&table);
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].path, b"/mnt/\xff");
        assert!(mounts[0].mount_point.contains('\u{FFFD}'));
        assert_eq!(mounts[1].mount_point, "/home");
        assert_eq!(mounts[1].path, b"/home");
    }

    #[test]
    fn an_octal_escape_of_a_non_utf8_byte_stays_a_byte() {
        let table = "50 35 8:3 / /mnt/\\377 rw - ext4 /dev/sdc1 rw\n";
        let mounts = parse_mountinfo(table);
        assert_eq!(mounts[0].path, b"/mnt/\xff");
    }

    #[test]
    fn a_backslash_before_a_multibyte_character_is_left_as_written() {
        // Slicing this as text at the four-byte escape window used to panic:
        // the window ends inside the second `é`.
        let raw = "\\\u{e9}\u{e9}";
        let table = format!("50 35 8:3 / {raw} rw - ext4 /dev/sdc1 rw\n");
        let mounts = parse_mountinfo(&table);
        assert_eq!(mounts[0].path, raw.as_bytes());
        assert_eq!(mounts[0].mount_point, raw);
    }

    #[test]
    fn a_mount_covered_by_a_later_one_at_the_same_path_is_hidden() {
        let table = "\
36 35 98:0 / / rw,relatime - ext4 /dev/root rw
60 36 8:2 / /data rw,relatime - ext4 /dev/sdb1 rw
61 36 8:3 / /data rw,relatime - xfs /dev/sdc1 rw
";
        let (visible, hidden) = visible_mounts(&parse_mountinfo(table), &[]);
        let data: Vec<&str> = visible
            .iter()
            .filter(|mount| mount.mount_point == "/data")
            .map(|mount| mount.source.as_str())
            .collect();
        assert_eq!(
            data,
            vec!["/dev/sdc1"],
            "only the mount on top, which statvfs reaches"
        );
        assert_eq!(hidden.covered, 1);
        assert!(hidden.categories().contains(&"covered"));
    }

    #[test]
    fn a_line_without_the_separator_is_not_a_mount() {
        assert!(parse_mountinfo("not a mount table\n").is_empty());
    }

    fn stat_line(name: &str, extra: &[&str]) -> String {
        let mut tokens = vec![
            "8", "0", name, "10", "1", "20", "5", "4", "0", "8", "3", "1", "6", "9",
        ];
        tokens.extend(extra);
        tokens.join(" ")
    }

    fn stat_at(
        name: &str,
        reads: u64,
        sectors_read: u64,
        writes: u64,
        sectors_written: u64,
    ) -> DiskStat {
        DiskStat {
            major: 8,
            minor: 0,
            name: name.into(),
            reads,
            reads_merged: 0,
            sectors_read,
            ms_reading: reads,
            writes,
            writes_merged: 0,
            sectors_written,
            ms_writing: writes,
            in_flight: 2,
            ms_io: 100,
            weighted_ms_io: 100,
        }
    }

    #[test]
    fn diskstats_accepts_14_18_and_20_fields() {
        let text = format!(
            "{}\n{}\n{}\n",
            stat_line("sda", &[]),
            stat_line("nvme0n1", &["1", "2", "3", "4"]),
            stat_line("mmcblk0", &["1", "2", "3", "4", "5", "6"])
        );
        let rows = parse_diskstats(&text);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "sda");
        assert_eq!(rows[0].reads, 10);
        assert_eq!(rows[0].sectors_read, 20);
        assert_eq!(rows[0].writes, 4);
        assert_eq!(rows[0].sectors_written, 8);
        assert_eq!(rows[0].in_flight, 1);
        assert_eq!(rows[0].ms_io, 6);
        assert_eq!(rows[1].name, "nvme0n1");
        assert_eq!(rows[2].weighted_ms_io, 9);
        assert!(parse_diskstats("8 0 only-three\n").is_empty());
    }

    #[test]
    fn rates_use_512_byte_sectors_and_a_reset_is_a_gap() {
        let prev = stat_at("sda", 0, 0, 0, 0);
        let next = stat_at("sda", 0, 1, 0, 0);
        let rates = disk_rates(&prev, &next, 1.0).unwrap();
        assert!((rates.read_bps - 512.0).abs() < 1e-9);
        assert!((rates.read_bps - 4096.0).abs() > 1.0);

        let busy = stat_at("sda", 4, 8, 6, 10);
        let rates = disk_rates(&prev, &busy, 2.0).unwrap();
        assert!((rates.read_bps - (8.0 * 512.0 / 2.0)).abs() < 1e-9);
        assert!((rates.write_bps - (10.0 * 512.0 / 2.0)).abs() < 1e-9);
        assert!((rates.iops - 5.0).abs() < 1e-9);
        assert!((rates.await_ms.unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(rates.queue, 2);

        let reset = stat_at("sda", 0, 0, 0, 0);
        let later = stat_at("sda", 3, 3, 3, 3);
        assert!(disk_rates(&later, &reset, 1.0).is_none());
        assert!(disk_rates(&prev, &next, 0.0).is_none());
    }

    #[test]
    fn await_is_absent_when_no_io_completed() {
        let prev = stat_at("sda", 5, 5, 5, 5);
        let next = stat_at("sda", 5, 9, 5, 5);
        let rates = disk_rates(&prev, &next, 1.0).unwrap();
        assert!(rates.await_ms.is_none());
        assert!((rates.read_bps - 4.0 * 512.0).abs() < 1e-9);
    }

    #[test]
    fn pressure_parses_both_lines_some_only_and_empty() {
        let both = "\
some avg10=23.1 avg60=18.4 avg300=9.7 total=100
full avg10=7.2 avg60=5.1 avg300=2.0 total=40
";
        let pressure = parse_pressure(both).unwrap();
        assert!((pressure.some[0] - 23.1).abs() < 1e-9);
        assert!((pressure.some[1] - 18.4).abs() < 1e-9);
        assert!((pressure.some[2] - 9.7).abs() < 1e-9);
        assert!((pressure.full[0] - 7.2).abs() < 1e-9);
        assert_eq!(pressure.some_total, 100);
        assert_eq!(pressure.full_total, 40);

        let some = parse_pressure("some avg10=1 avg60=2 avg300=3 total=4\n").unwrap();
        assert_eq!(some.full, [0.0, 0.0, 0.0]);
        assert_eq!(some.full_total, 0);
        assert!(parse_pressure("").is_none());
        assert!(parse_pressure("\n").is_none());
    }

    #[test]
    fn sysfs_size_is_512_byte_sectors_and_rotational_is_a_flag() {
        assert_eq!(parse_sys_block_size("2048\n"), Some(2048 * 512));
        assert_eq!(parse_sys_block_size("nope"), None);
        assert_eq!(parse_rotational("0\n"), Some(false));
        assert_eq!(parse_rotational("1"), Some(true));
        assert_eq!(parse_rotational(""), None);
    }

    #[test]
    fn iostat_header_and_three_lines() {
        let header = parse_iostat_header("          disk0               disk2 ");
        assert_eq!(header, vec!["disk0".to_string(), "disk2".to_string()]);
        assert!(parse_iostat_header("    KB/t  tps  MB/s     KB/t  tps  MB/s ").is_empty());
        let lines = [
            "   12.50    3.00   0.04    8.00    1.00   0.01",
            "   20.00   10.00   0.20    0.00    0.00   0.00",
            "    1.00    1.00   0.00    2.00    2.00   0.01",
        ];
        for line in lines {
            let samples = parse_iostat_line(line, header.len());
            assert_eq!(samples.len(), 2, "{line}");
            assert!(samples[0].tps.is_finite());
        }
        let first = parse_iostat_line(lines[0], 2);
        assert!((first[0].kb_per_transfer - 12.5).abs() < 1e-9);
        assert!((first[0].tps - 3.0).abs() < 1e-9);
        assert!((first[1].mb_per_s - 0.01).abs() < 1e-9);
        assert!(parse_iostat_line("1 2", 2).is_empty());
    }

    #[test]
    fn partition_links_map_sda_nvme_mmc_and_one_dm_slave() {
        let mut links = BTreeMap::new();
        links.insert("dev/block/8:1".into(), "../../block/sda/sda1".into());
        links.insert(
            "dev/block/259:1".into(),
            "../../devices/pci0000:00/0000:00:1d.0/0000:3d:00.0/nvme/nvme0/nvme0n1/nvme0n1p1"
                .into(),
        );
        links.insert(
            "dev/block/259:0".into(),
            "../../devices/pci0000:00/0000:00:1d.0/0000:3d:00.0/nvme/nvme0/nvme0n1".into(),
        );
        links.insert(
            "dev/block/179:1".into(),
            "../../block/mmcblk0/mmcblk0p1".into(),
        );
        links.insert("dev/block/8:0".into(), "../../block/sda".into());
        links.insert("block/dm-0/slaves/sda".into(), "../../../sda".into());
        let disks: BTreeSet<String> = ["sda", "nvme0n1", "mmcblk0"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let layout = layout_from_links(&links, &disks);
        assert_eq!(
            layout.partitions.get("sda1").map(String::as_str),
            Some("sda")
        );
        assert_eq!(
            layout.partitions.get("nvme0n1p1").map(String::as_str),
            Some("nvme0n1")
        );
        assert_eq!(
            layout.partitions.get("mmcblk0p1").map(String::as_str),
            Some("mmcblk0")
        );
        assert!(!layout.partitions.contains_key("sda"));
        assert!(!layout.partitions.contains_key("nvme0n1"));
        assert_eq!(parse_dev("8:0\n").as_deref(), Some("8:0"));
        assert_eq!(parse_dev("  259 1 ").as_deref(), Some("259:1"));
        assert_eq!(parse_dev("8"), None);
        assert_eq!(parse_dev(""), None);
        assert_eq!(
            layout.slaves.get("dm-0").map(Vec::as_slice),
            Some(["sda".to_string()].as_slice())
        );
    }

    #[test]
    fn a_disk_and_its_partitions_become_one_row() {
        let stats = parse_diskstats(&format!(
            "{}\n{}\n{}\n",
            stat_line("sda", &[]),
            "8 1 sda1 1 0 2 1 1 0 2 1 0 1 1",
            "8 2 sda2 1 0 2 1 1 0 2 1 0 1 1"
        ));
        assert_eq!(stats.len(), 3);
        let inventory = vec![InventoryDisk {
            name: "sda".into(),
            kind: "ssd".into(),
            size: 1024,
            model: "Example SSD 1TB".into(),
            dev: "8:0".into(),
            partitions: vec![
                PartMount {
                    name: "sda1".into(),
                    dev: "8:1".into(),
                    mounts: vec!["/".into()],
                },
                PartMount {
                    name: "sda2".into(),
                    dev: "8:2".into(),
                    mounts: vec![],
                },
            ],
            slaves: vec![],
        }];
        let live = live_disks(&stats, &inventory);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].name, "sda");
        assert_eq!(live[0].partitions.len(), 2);
        assert!(!live[0].idle);
        assert_eq!(device_of("8:1", &live).as_deref(), Some("on sda (sda1)"));
        assert_eq!(device_of("8:0", &live).as_deref(), Some("on sda"));
    }

    #[test]
    fn idle_disks_and_named_devices_are_separate_filters() {
        let idle = LiveDisk {
            name: "loop0".into(),
            kind: "virtual".into(),
            size: 0,
            model: String::new(),
            dev: "7:0".into(),
            stat: Some(stat_at("loop0", 0, 0, 0, 0)),
            mac: None,
            idle: true,
            partitions: vec![],
            mounts: vec![],
            slaves: vec![],
        };
        let busy = LiveDisk {
            name: "sda".into(),
            kind: "ssd".into(),
            size: 1,
            model: "Example SSD 1TB".into(),
            dev: "8:0".into(),
            stat: Some(stat_at("sda", 3, 3, 1, 1)),
            mac: None,
            idle: false,
            partitions: vec![],
            mounts: vec![],
            slaves: vec![],
        };
        let disks = vec![idle, busy.clone()];
        let (shown, hidden) = visible_disk_indexes(&disks, false, &[]);
        assert_eq!(shown, vec![1]);
        assert_eq!(hidden.idle, 1);
        assert_eq!(hidden.configured, 0);
        assert_eq!(hidden.labels(), vec!["1 idle hidden".to_string()]);

        let (shown, hidden) = visible_disk_indexes(&disks, true, &["sda".into()]);
        assert_eq!(shown, vec![0]);
        assert_eq!(hidden.idle, 0);
        assert_eq!(hidden.configured, 1);
        assert_eq!(hidden.labels(), vec!["1 hide_devices hidden".to_string()]);
        let _ = busy;
    }

    #[test]
    fn an_idle_boot_sample_stops_being_idle_once_an_interval_moves() {
        let mut track = IostatTrack::default();
        let names = vec!["disk0".to_string(), "disk1".to_string()];
        let quiet = IostatSample {
            kb_per_transfer: 0.0,
            tps: 0.0,
            mb_per_s: 0.0,
        };
        let busy = IostatSample {
            kb_per_transfer: 8.0,
            tps: 10.0,
            mb_per_s: 1.0,
        };
        publish_iostat_track(&mut track, &names, None, Some(vec![quiet, quiet]));
        assert!(iostat_disk_idle(&track, 0));
        assert!(iostat_disk_idle(&track, 1));
        publish_iostat_track(&mut track, &names, Some(vec![busy, quiet]), None);
        assert!(!iostat_disk_idle(&track, 0), "later traffic clears idle");
        assert!(iostat_disk_idle(&track, 1));
        publish_iostat_track(&mut track, &names, Some(vec![quiet, quiet]), None);
        assert!(
            !iostat_disk_idle(&track, 0),
            "a quiet interval does not hide a disk that has moved"
        );
    }

    #[test]
    fn a_new_iostat_child_drops_the_previous_intervals() {
        let mut track = IostatTrack::default();
        let first = vec!["disk0".to_string(), "disk2".to_string()];
        let sample = IostatSample {
            kb_per_transfer: 1.0,
            tps: 4.0,
            mb_per_s: 2.0,
        };
        publish_iostat_track(&mut track, &first, None, Some(vec![sample, sample]));
        publish_iostat_track(&mut track, &first, Some(vec![sample, sample]), None);
        assert_eq!(track.latest.len(), 2);
        let second = vec!["disk0".to_string()];
        let quiet = IostatSample {
            kb_per_transfer: 0.0,
            tps: 0.0,
            mb_per_s: 0.0,
        };
        publish_iostat_track(&mut track, &second, None, Some(vec![quiet]));
        assert!(
            track.latest.is_empty(),
            "old intervals are not paired by position"
        );
        assert_eq!(track.names, second);
        assert!(iostat_disk_idle(&track, 0));
    }

    #[test]
    fn a_container_variable_names_a_runtime_and_an_empty_one_does_not() {
        assert!(parse_container_env("container=podman\0PATH=/bin"));
        assert!(parse_container_env("PATH=/bin\0container=docker"));
        assert!(!parse_container_env("container=\0PATH=/bin"));
        assert!(!parse_container_env("container_name=web\0"));
        assert!(!parse_container_env(""));
        assert!(!parse_container_env("0::/\n"));
    }
}
