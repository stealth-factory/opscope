// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! macOS acquisition for `link`; parsing remains platform-independent.

use super::parse::parse_nettop_snapshot;
use super::{run_or, Session};

pub fn sessions(named: &[u16]) -> Result<Vec<Session>, String> {
    let text = run_or(&[
        "/usr/bin/nettop",
        "-m",
        "tcp",
        "-n",
        "-x",
        "-L",
        "1",
        "-J",
        "bytes_in,bytes_out,re-tx,rtt_min,rtt_avg,rtt_var",
    ])?;
    Ok(parse_nettop_snapshot(&text, named)
        .sessions
        .into_iter()
        .map(|row| Session {
            peer: row.peer,
            ip: row.ip,
            port: row.port,
            rtt: row.rtt,
            jitter: row.jitter,
            floor: row.floor,
            sent: row.sent,
            recv: row.recv,
            retrans_bytes: row.retrans_bytes,
            delivery: None,
            delivery_unavailable: true,
            raw: row.raw,
            ..Session::default()
        })
        .collect())
}

pub fn source_note() -> &'static str {
    "measured by the kernel via nettop, nothing sent"
}

pub fn missing() -> Vec<String> {
    if std::path::Path::new("/usr/bin/nettop").is_file() {
        Vec::new()
    } else {
        vec!["/usr/bin/nettop".to_string()]
    }
}

pub fn missing_reason() -> &'static [&'static str] {
    &[
        "nettop reads the kernel's own per-socket metrics, which is where",
        "round-trip time and retransmit counters come from on macOS.",
        "Without it the pane cannot distinguish no sessions from no source.",
    ]
}

pub fn install_hint() -> &'static str {
    "nettop is included with macOS"
}

pub fn empty_note() -> &'static str {
    "Nothing is connected to this machine, or nettop cannot see it."
}
