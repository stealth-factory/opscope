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

use super::parse::{self, Mount, Space};

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
