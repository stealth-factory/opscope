// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Mount tables and the arithmetic both hosts share.
//!
//! Nothing here opens a file or runs a tool. Linux hands `parse_mountinfo`
//! the text of its mount table; macOS builds the same `Mount` values from
//! `getmntinfo` and then calls `visible_mounts`.

use std::collections::HashMap;

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
    pub mount_point: String,
    pub fstype: String,
    pub source: String,
    pub read_only: bool,
    pub options: String,
}

/// How many mounts each rule held back. A mount is counted once, under the
/// first rule that hid it: a configured path, then a pseudo filesystem,
/// then a read-only mount, then a bind of something still visible.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hidden {
    pub pseudo: usize,
    pub bind: usize,
    pub read_only: usize,
    pub configured: usize,
}

impl Hidden {
    /// The categories that actually hid something, in the order the header
    /// names them. Empty when every mount is on screen.
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
/// Optional tag fields sit between the mount options and the ` - `
/// separator. A line that does not have that separator is skipped: a
/// partial read is not a mount.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(' ').filter(|field| !field.is_empty()).collect();
        let Some(sep) = fields.iter().position(|field| *field == "-") else {
            continue;
        };
        if sep < 6 || fields.len() < sep + 3 {
            continue;
        }
        let options = unescape(fields[5]);
        out.push(Mount {
            dev: fields[2].to_string(),
            root: unescape(fields[3]),
            mount_point: unescape(fields[4]),
            fstype: unescape(fields[sep + 1]),
            source: unescape(fields[sep + 2]),
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
    for mount in mounts {
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

/// `\040` and the other octal escapes the mount table uses. A backslash
/// that is not three octal digits is left as it was written.
fn unescape(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let octal = &raw[index + 1..index + 4];
            if octal.bytes().all(|byte| (b'0'..=b'7').contains(&byte)) {
                if let Ok(value) = u8::from_str_radix(octal, 8) {
                    out.push(value as char);
                    index += 4;
                    continue;
                }
            }
        }
        out.push(bytes[index] as char);
        index += 1;
    }
    out
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
        assert!(visible
            .iter()
            .any(|mount| mount.mount_point == "/var/lib/app"));
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
    fn a_line_without_the_separator_is_not_a_mount() {
        assert!(parse_mountinfo("not a mount table\n").is_empty());
    }
}
