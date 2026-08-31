use std::collections::HashMap;

use opscope_core as tc;

use super::parse::{parse_ip_addresses, parse_proc_net_dev, parse_ss_sockets};
use super::{Facts, OpenFile, RUN_TIMEOUT, Seen, TrafficSample, VIRTUAL, local_peer, off_box};

pub const ROW_SOURCE: &str = "per-socket TCP bytes · Linux ss";
pub const HAS_CONNECTION_DETAILS: bool = true;
pub const HAS_DISK_IO: bool = true;
#[allow(dead_code)] // Referenced from main when this host has no socket/disk source.
pub const NO_SOCKET_DETAIL: &str = "   none";
#[allow(dead_code)]
pub const NO_DISK_IO: &str = "";

pub fn own_addresses() -> Result<Vec<String>, String> {
    tc::run(&["ip", "-o", "addr"], RUN_TIMEOUT)
        .map(|text| parse_ip_addresses(&text))
        .map_err(|e| format!("ip -o addr unavailable: {}", e))
}

pub fn sockets(external: bool, own: &[String]) -> Result<TrafficSample, String> {
    let text = tc::run(&["ss", "-tine"], RUN_TIMEOUT)
        .map_err(|e| format!("ss -tine unavailable: {}", e))?;
    let found = parse_ss_sockets(&text)
        .into_iter()
        .filter(|row| !local_peer(&row.peer, own) && (!external || off_box(&row.peer, own)))
        .map(|row| {
            (
                row.inode,
                Seen {
                    sent: row.sent,
                    recv: row.recv,
                    peer: row.peer,
                    port: row.port,
                    mine: row.mine,
                    cgroup: row.cgroup,
                },
            )
        })
        .collect();
    Ok(TrafficSample::Sockets(found))
}

pub fn socket_owners() -> Result<HashMap<String, (i32, String)>, String> {
    let mut owners = HashMap::new();
    let entries = std::fs::read_dir("/proc")
        .map_err(|e| format!("/proc socket ownership unavailable: {}", e))?;
    for entry in entries.flatten() {
        let pid: i32 = match entry.file_name().to_string_lossy().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let fds = match std::fs::read_dir(format!("/proc/{}/fd", pid)) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let mut name = String::new();
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                let target = target.to_string_lossy();
                if let Some(rest) = target.strip_prefix("socket:[") {
                    if name.is_empty() {
                        name = process_name(pid);
                    }
                    owners.insert(rest.trim_end_matches(']').to_string(), (pid, name.clone()));
                }
            }
        }
    }
    Ok(owners)
}

fn process_name(pid: i32) -> String {
    let comm = std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if comm.chars().any(|c| c.is_ascii_alphabetic()) {
        return comm;
    }
    let argv0 = std::fs::read(format!("/proc/{}/cmdline", pid))
        .map(|raw| {
            String::from_utf8_lossy(&raw)
                .split('\0')
                .next()
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_default();
    for part in argv0.split('/').rev() {
        if part.chars().any(|c| c.is_ascii_alphabetic())
            && ![
                "versions",
                "bin",
                "sbin",
                "libexec",
                "node_modules",
                "dist",
                "build",
                "lib",
                "share",
                "local",
                "current",
                "releases",
            ]
            .contains(&part.to_lowercase().as_str())
        {
            return part.to_string();
        }
    }
    if comm.is_empty() { "?".into() } else { comm }
}

pub fn wire_bytes() -> Result<(u64, u64, Vec<String>), String> {
    let text = std::fs::read_to_string("/proc/net/dev")
        .map_err(|e| format!("/proc/net/dev unavailable: {}", e))?;
    Ok(parse_proc_net_dev(&text, VIRTUAL))
}

pub fn running(pid: i32) -> bool {
    pid > 0 && std::path::Path::new(&format!("/proc/{}", pid)).is_dir()
}

pub fn proc_io(pid: i32) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(format!("/proc/{}/io", pid)) {
        for line in text.lines() {
            if let Some((key, value)) = line.split_once(':') {
                if let Ok(n) = value.trim().parse() {
                    out.insert(key.trim().to_string(), n);
                }
            }
        }
    }
    out
}

pub fn open_files(pid: i32) -> Result<Vec<OpenFile>, String> {
    let mut found = Vec::new();
    let dir = std::fs::read_dir(format!("/proc/{}/fd", pid))
        .map_err(|e| format!("open files unavailable: {}", e))?;
    for entry in dir.flatten() {
        let Ok(link) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let path = link.to_string_lossy().to_string();
        if !path.starts_with('/')
            || path.starts_with("/dev/")
            || path.starts_with("/proc/")
            || path.starts_with("/sys/")
        {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(entry.path()) {
            found.push(OpenFile {
                path,
                size: meta.len(),
            });
        }
    }
    found.sort_by(|a, b| b.size.cmp(&a.size));
    Ok(found)
}

pub fn process_facts(pid: i32) -> Facts {
    let cmdline = std::fs::read(format!("/proc/{}/cmdline", pid))
        .map(|raw| {
            String::from_utf8_lossy(&raw)
                .replace('\0', " ")
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let cwd = std::fs::read_link(format!("/proc/{}/cwd", pid))
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    Facts { cmdline, cwd }
}
