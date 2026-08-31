// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Parsers for every source netwatch reads, compiled on every target.

#![allow(dead_code)]

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SocketRecord {
    pub inode: String,
    pub sent: u64,
    pub recv: u64,
    pub peer: String,
    pub port: u16,
    pub mine: u16,
    pub cgroup: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessRecord {
    pub pid: i32,
    pub name: String,
    pub sent: u64,
    pub recv: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRecord {
    pub path: String,
    pub size: u64,
}

/// The value after `key:` on a line, up to the next space.
pub fn field(line: &str, key: &str) -> Option<String> {
    let at = line.find(key)? + key.len();
    let rest = &line[at..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let value = &rest[..end];
    (!value.is_empty()).then(|| value.to_string())
}

pub fn parse_ip_addresses(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if let Some(at) = cols.iter().position(|c| *c == "inet" || *c == "inet6") {
            if let Some(addr) = cols.get(at + 1) {
                found.push(addr.split('/').next().unwrap_or(addr).to_string());
            }
        }
    }
    found
}

pub fn parse_ifconfig_addresses(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            match cols.next()? {
                "inet" | "inet6" => Some(
                    cols.next()?
                        .split('%')
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                ),
                _ => None,
            }
        })
        .collect()
}

/// `ss -tine`: an address line followed by an indented counter line.
pub fn parse_ss_sockets(text: &str) -> Vec<SocketRecord> {
    let mut found = Vec::new();
    let (mut inode, mut peer, mut port, mut cgroup) = (None, String::new(), 0u16, String::new());
    let mut mine = 0u16;
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            let cols: Vec<&str> = line.split_whitespace().collect();
            peer = cols
                .get(4)
                .and_then(|a| a.rsplit_once(':'))
                .map(|(h, _)| h.trim_matches(|c| c == '[' || c == ']').to_string())
                .unwrap_or_default();
            port = cols
                .get(4)
                .and_then(|a| a.rsplit_once(':'))
                .and_then(|(_, p)| p.parse().ok())
                .unwrap_or(0);
            mine = cols
                .get(3)
                .and_then(|a| a.rsplit_once(':'))
                .and_then(|(_, p)| p.parse().ok())
                .unwrap_or(0);
            inode = field(line, "ino:").filter(|v| v != "0");
            cgroup = field(line, "cgroup:").unwrap_or_default();
            continue;
        }
        let Some(id) = inode.take() else {
            continue;
        };
        found.push(SocketRecord {
            inode: id,
            sent: field(line, "bytes_sent:")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            recv: field(line, "bytes_received:")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            peer: peer.clone(),
            port,
            mine,
            cgroup: cgroup.clone(),
        });
    }
    found
}

pub fn parse_proc_net_dev(text: &str, virtual_prefixes: &[&str]) -> (u64, u64, Vec<String>) {
    let (mut rx, mut tx, mut names) = (0u64, 0u64, Vec::new());
    for line in text.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if virtual_prefixes.iter().any(|v| name.starts_with(v)) {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 9 {
            continue;
        }
        rx += fields[0].parse::<u64>().unwrap_or(0);
        tx += fields[8].parse::<u64>().unwrap_or(0);
        names.push(name.to_string());
    }
    (rx, tx, names)
}

/// Link-layer rows only. Address rows repeat the same counters and must not
/// be added a second time. Counting from the right handles interfaces whose
/// link row has no address column.
pub fn parse_netstat_interfaces(text: &str, virtual_prefixes: &[&str]) -> (u64, u64, Vec<String>) {
    let (mut rx, mut tx, mut names) = (0u64, 0u64, Vec::new());
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 10 || !cols[2].starts_with("<Link#") {
            continue;
        }
        let name = cols[0].trim_end_matches('*');
        if virtual_prefixes.iter().any(|v| name.starts_with(v)) {
            continue;
        }
        rx += cols[cols.len() - 5].parse::<u64>().unwrap_or(0);
        tx += cols[cols.len() - 2].parse::<u64>().unwrap_or(0);
        names.push(name.to_string());
    }
    (rx, tx, names)
}

/// `nettop -P -x -L 1 -J bytes_in,bytes_out` CSV-like output.
pub fn parse_nettop_processes(text: &str) -> Vec<ProcessRecord> {
    let mut found = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.trim_end_matches(',').split(',').collect();
        if cols.len() < 3 || cols[0].is_empty() || cols[0] == "bytes_in" {
            continue;
        }
        let Some((name, pid)) = cols[0].rsplit_once('.') else {
            continue;
        };
        let (Ok(pid), Ok(recv), Ok(sent)) = (pid.parse(), cols[1].parse(), cols[2].parse()) else {
            continue;
        };
        found.push(ProcessRecord {
            pid,
            name: name.to_string(),
            sent,
            recv,
        });
    }
    found
}

/// Regular files from `lsof -Fnfst`. Each record begins with `f`; cwd, txt
/// and other non-descriptor entries are not open file descriptors whose
/// growth would mean a download.
pub fn parse_lsof_files(text: &str) -> Vec<FileRecord> {
    let mut out = Vec::new();
    let (mut size, mut path, mut fd, mut kind) =
        (None, None::<String>, None::<String>, None::<String>);
    let flush = |out: &mut Vec<FileRecord>,
                 size: &mut Option<u64>,
                 path: &mut Option<String>,
                 fd: &mut Option<String>,
                 kind: &mut Option<String>| {
        let fd = fd.take();
        let kind = kind.take();
        if let (Some(size), Some(path)) = (size.take(), path.take()) {
            let descriptor = fd.is_some_and(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_digit()));
            let regular = kind.as_deref().is_none_or(|k| k == "REG");
            if descriptor && regular && path.starts_with('/') && !path.starts_with("/dev/") {
                out.push(FileRecord { path, size });
            }
        }
    };
    for line in text.lines() {
        if let Some(value) = line.strip_prefix('f') {
            flush(&mut out, &mut size, &mut path, &mut fd, &mut kind);
            fd = Some(value.to_string());
            continue;
        }
        if line.starts_with('p') {
            flush(&mut out, &mut size, &mut path, &mut fd, &mut kind);
            continue;
        }
        if let Some(value) = line.strip_prefix('s') {
            size = value.parse().ok();
        } else if let Some(value) = line.strip_prefix('n') {
            path = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix('t') {
            kind = Some(value.to_string());
        }
    }
    flush(&mut out, &mut size, &mut path, &mut fd, &mut kind);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_captured_macos_nettop_rows() {
        let text = ",bytes_in,bytes_out,\nkernel_task.0,6034696638,668471625,\nGoogle Chrome H.12974,1437658006,22465882,\n";
        assert_eq!(
            parse_nettop_processes(text),
            vec![
                ProcessRecord {
                    pid: 0,
                    name: "kernel_task".into(),
                    recv: 6_034_696_638,
                    sent: 668_471_625
                },
                ProcessRecord {
                    pid: 12974,
                    name: "Google Chrome H".into(),
                    recv: 1_437_658_006,
                    sent: 22_465_882
                },
            ]
        );
    }

    #[test]
    fn netstat_counts_each_interface_once() {
        let text = "Name Mtu Network Address Ipkts Ierrs Ibytes Opkts Oerrs Obytes Coll\nen0 1500 <Link#27> aa:bb 100 0 1000 50 0 500 0\nen0 1500 192.0.2 192.0.2.1 100 - 1000 50 - 500 -\nutun0 1500 <Link#30> 2 0 200 3 0 300 0\n";
        assert_eq!(
            parse_netstat_interfaces(text, &["lo", "utun"]),
            (1000, 500, vec!["en0".into()])
        );
    }

    #[test]
    fn parses_macos_addresses_without_scope_ids() {
        let text = "en0: flags=8863\n\tinet 192.0.2.10 netmask 0xffffff00\n\tinet6 fe80::1234%en0 prefixlen 64\n";
        assert_eq!(
            parse_ifconfig_addresses(text),
            vec!["192.0.2.10", "fe80::1234"]
        );
    }

    #[test]
    fn parses_regular_files_from_lsof_fields() {
        let text = "p42\nfcwd\ntDIR\ns768\nn/tmp/work\nftxt\ntREG\ns123\nn/bin/tool\nf3\ntREG\ns4096\nn/tmp/download.iso\nf0\ntCHR\ns0\nn/dev/null\n";
        assert_eq!(
            parse_lsof_files(text),
            vec![FileRecord {
                path: "/tmp/download.iso".into(),
                size: 4096
            }]
        );
    }

    #[test]
    fn parses_captured_ss_tine_rows() {
        let text = "\
Netid State Recv-Q Send-Q Local Address:Port Peer Address:Port Process
tcp   ESTAB 0      0      192.0.2.10:54321   192.0.2.1:443     ino:4242 cgroup:/user.slice/session-1.scope
\t cubic bytes_sent:1669 bytes_received:11469 segs_out:12
tcp   ESTAB 0      0      [2001:db8::10]:22  [2001:db8::1]:6000 ino:99
\t bytes_sent:10 bytes_received:20
tcp   ESTAB 0      0      127.0.0.1:1        127.0.0.1:2       ino:0
\t bytes_sent:1 bytes_received:2
";
        assert_eq!(
            parse_ss_sockets(text),
            vec![
                SocketRecord {
                    inode: "4242".into(),
                    sent: 1669,
                    recv: 11469,
                    peer: "192.0.2.1".into(),
                    port: 443,
                    mine: 54321,
                    cgroup: "/user.slice/session-1.scope".into(),
                },
                SocketRecord {
                    inode: "99".into(),
                    sent: 10,
                    recv: 20,
                    peer: "2001:db8::1".into(),
                    port: 6000,
                    mine: 22,
                    cgroup: String::new(),
                },
            ]
        );
    }

    #[test]
    fn ss_fields_are_read_off_the_line() {
        let line = "\t ts sack cubic bytes_sent:1669 bytes_received:11469 segs_out:272";
        assert_eq!(field(line, "bytes_sent:"), Some("1669".into()));
        assert_eq!(field(line, "bytes_received:"), Some("11469".into()));
        assert_eq!(field(line, "nothing:"), None);
    }
}
