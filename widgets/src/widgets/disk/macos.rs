// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Where the bytes come from on macOS.
//!
//! `getmntinfo(MNT_NOWAIT)` returns every mount's `statfs` in one call, so
//! there is no per-mount `stat` that can hang the poller. The buffer belongs
//! to libc and is replaced on the next call; the strings and the byte
//! counts are copied out before this function returns. `stat_mount` reads
//! that copy. `dev` is the mount-from name, which is the identity
//! `visible_mounts` uses to drop duplicates on this host.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use super::parse::{self, IoSnapshot, IostatSample, LiveDisk, Mount, Space};

struct IostatState {
    names: Vec<String>,
    latest: Vec<IostatSample>,
    boot: Vec<IostatSample>,
    missing: bool,
    err: String,
}

static IOSTAT: OnceLock<Mutex<IostatState>> = OnceLock::new();
static IOSTAT_STARTED: OnceLock<()> = OnceLock::new();

static SPACES: Mutex<Option<HashMap<String, Space>>> = Mutex::new(None);

pub fn read_mounts() -> Result<Vec<Mount>, String> {
    let mut buf: *mut libc::statfs = std::ptr::null_mut();
    let count = unsafe { libc::getmntinfo(&mut buf, libc::MNT_NOWAIT) };
    if count <= 0 || buf.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let rows = unsafe { std::slice::from_raw_parts(buf, count as usize) };
    let mut mounts = Vec::with_capacity(rows.len());
    let mut spaces = HashMap::with_capacity(rows.len());
    for stat in rows {
        let mount = mount_from_statfs(stat);
        spaces.insert(mount.mount_point.clone(), space_from_statfs(stat));
        mounts.push(mount);
    }
    match SPACES.lock() {
        Ok(mut guard) => *guard = Some(spaces),
        Err(poisoned) => *poisoned.into_inner() = Some(spaces),
    }
    Ok(mounts)
}

/// The figures from the `getmntinfo` snapshot `read_mounts` just took.
///
/// A second `statfs` would be a look at a mount that can hang, which is
/// what `MNT_NOWAIT` avoided. The poller still calls this once per shown
/// mount so the Linux timeout has one shape on both hosts; here it does
/// not enter the kernel.
pub fn stat_mount(path: &str) -> Result<Space, String> {
    let guard = SPACES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .as_ref()
        .and_then(|spaces| spaces.get(path).copied())
        .ok_or_else(|| format!("no statfs entry for {path}"))
}

fn mount_from_statfs(stat: &libc::statfs) -> Mount {
    let read_only = (stat.f_flags & libc::MNT_RDONLY as u32) != 0;
    let source = c_buf(&stat.f_mntfromname);
    Mount {
        dev: source.clone(),
        root: "/".to_string(),
        mount_point: c_buf(&stat.f_mntonname),
        fstype: c_buf(&stat.f_fstypename),
        source,
        read_only,
        options: if read_only { "ro" } else { "rw" }.to_string(),
    }
}

fn space_from_statfs(stat: &libc::statfs) -> Space {
    parse::space_from_blocks(
        stat.f_blocks,
        stat.f_bfree,
        stat.f_bavail,
        stat.f_files,
        stat.f_ffree,
        stat.f_bsize as u64,
    )
}

/// One long-running `iostat -d -K -w`. The first data line is the average
/// since boot and is kept only to decide which disks have never moved;
/// every later line is one interval. Stdout is read a line at a time.
/// A child that exits is started again. A missing binary is not: the
/// widget still runs, and the pane says so.
pub fn sample_io(_mounts: &[Mount], every: f64) -> Result<IoSnapshot, String> {
    let state = IOSTAT.get_or_init(|| {
        Mutex::new(IostatState {
            names: Vec::new(),
            latest: Vec::new(),
            boot: Vec::new(),
            missing: false,
            err: String::new(),
        })
    });
    IOSTAT_STARTED.get_or_init(|| {
        thread::spawn(move || run_iostat(every));
    });
    let guard = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.missing {
        return Ok(IoSnapshot {
            missing: "needs iostat (recommended) — device I/O is not shown".into(),
            combined: true,
            ..IoSnapshot::default()
        });
    }
    let mut disks = Vec::with_capacity(guard.names.len());
    for (index, name) in guard.names.iter().enumerate() {
        let mac = guard.latest.get(index).copied();
        let idle = guard
            .boot
            .get(index)
            .is_some_and(|sample| sample.tps == 0.0 && sample.mb_per_s == 0.0);
        disks.push(LiveDisk {
            name: name.clone(),
            kind: String::new(),
            size: 0,
            model: String::new(),
            dev: String::new(),
            stat: None,
            mac,
            idle,
            partitions: Vec::new(),
            mounts: Vec::new(),
            slaves: Vec::new(),
        });
    }
    Ok(IoSnapshot {
        disks,
        combined: true,
        err: guard.err.clone(),
        ..IoSnapshot::default()
    })
}

fn run_iostat(every: f64) {
    let wait = every.round().max(1.0) as u64;
    let wait = wait.to_string();
    loop {
        let mut child = match Command::new("iostat")
            .args(["-d", "-K", "-w", &wait])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if let Some(state) = IOSTAT.get() {
                    if let Ok(mut guard) = state.lock() {
                        guard.missing = true;
                        guard.err.clear();
                    }
                }
                return;
            }
            Err(err) => {
                note_iostat_error(format!("iostat stopped: {err}"));
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let stderr = child.stderr.take();
        let stderr_text = std::sync::Arc::new(Mutex::new(String::new()));
        if let Some(stderr) = stderr {
            let slot = std::sync::Arc::clone(&stderr_text);
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Ok(mut guard) = slot.lock() {
                        if !guard.is_empty() {
                            guard.push('\n');
                        }
                        guard.push_str(&line);
                    }
                }
            });
        }
        let Some(stdout) = child.stdout.take() else {
            note_iostat_error("iostat stopped: no stdout".into());
            let _ = child.wait();
            thread::sleep(Duration::from_secs(1));
            continue;
        };
        let mut names = Vec::new();
        let mut skipped_boot = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            let header = parse::parse_iostat_header(&line);
            if !header.is_empty() {
                names = header;
                continue;
            }
            if names.is_empty() {
                continue;
            }
            let samples = parse::parse_iostat_line(&line, names.len());
            if samples.len() != names.len() {
                continue;
            }
            if !skipped_boot {
                publish_iostat(&names, None, Some(samples));
                skipped_boot = true;
                continue;
            }
            publish_iostat(&names, Some(samples), None);
        }
        let _ = child.wait();
        let why = stderr_text
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let why = if why.is_empty() {
            "exited".to_string()
        } else {
            why
        };
        note_iostat_error(format!("iostat stopped: {why}"));
        thread::sleep(Duration::from_secs(1));
    }
}

fn publish_iostat(
    names: &[String],
    latest: Option<Vec<IostatSample>>,
    boot: Option<Vec<IostatSample>>,
) {
    let Some(state) = IOSTAT.get() else {
        return;
    };
    let Ok(mut guard) = state.lock() else {
        return;
    };
    guard.names = names.to_vec();
    guard.err.clear();
    if let Some(latest) = latest {
        guard.latest = latest;
    }
    if let Some(boot) = boot {
        guard.boot = boot;
    }
}

fn note_iostat_error(err: String) {
    let Some(state) = IOSTAT.get() else {
        return;
    };
    if let Ok(mut guard) = state.lock() {
        guard.err = err;
    }
}

fn c_buf(buf: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
