use std::collections::HashMap;

use opscope_core as tc;
use tc::procfs::{local_peer, off_box, parse_ss_sockets, OpenFile, PidIo};

use super::parse::parse_proc_net_dev;
use super::{RUN_TIMEOUT, Seen, TrafficSample, VIRTUAL};

pub use tc::procfs::{own_addresses, process_facts, running};

pub const ROW_SOURCE: &str = "per-socket TCP bytes · Linux ss";
pub const HAS_CONNECTION_DETAILS: bool = true;
pub const HAS_DISK_IO: bool = true;
#[allow(dead_code)] // Referenced from main when this host has no socket/disk source.
pub const NO_SOCKET_DETAIL: &str = "   none";
#[allow(dead_code)]
pub const NO_DISK_IO: &str = "";

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

/// socket inode -> (pid, name), from the `/proc/<pid>/fd` walk in core.
pub fn owners() -> Result<HashMap<String, (i32, String)>, String> {
    tc::procfs::socket_owners()
}

pub fn wire_bytes() -> Result<(u64, u64, Vec<String>), String> {
    let text = std::fs::read_to_string("/proc/net/dev")
        .map_err(|e| format!("/proc/net/dev unavailable: {}", e))?;
    Ok(parse_proc_net_dev(&text, VIRTUAL))
}

/// `/proc/<pid>/io`. `None` for anyone else's, which is closed to us.
pub fn disk_io(pid: i32) -> Option<PidIo> {
    tc::procfs::read_io(pid).ok()
}

pub fn files_of(pid: i32) -> Result<Vec<OpenFile>, String> {
    tc::procfs::open_files(pid)
}
