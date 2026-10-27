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
use std::sync::Mutex;

use super::parse::{self, Mount, Space};

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
pub fn stat_mount(path: &[u8]) -> Result<Space, String> {
    let key = String::from_utf8_lossy(path);
    let guard = SPACES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .as_ref()
        .and_then(|spaces| spaces.get(key.as_ref()).copied())
        .ok_or_else(|| format!("no statfs entry for {key}"))
}

fn mount_from_statfs(stat: &libc::statfs) -> Mount {
    let read_only = (stat.f_flags & libc::MNT_RDONLY as u32) != 0;
    let source = c_buf(&stat.f_mntfromname);
    let path = c_bytes(&stat.f_mntonname);
    Mount {
        dev: source.clone(),
        root: "/".to_string(),
        mount_point: String::from_utf8_lossy(&path).into_owned(),
        path,
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

fn c_bytes(buf: &[libc::c_char]) -> Vec<u8> {
    buf.iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect()
}

fn c_buf(buf: &[libc::c_char]) -> String {
    String::from_utf8_lossy(&c_bytes(buf)).into_owned()
}
