// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Linux acquisition for `link`; parsing remains platform-independent.

use super::parse::{parse_ss_listening, parse_ss_sessions};
use super::{ports_to_watch, run_or, Session};

pub fn sessions(named: &[u16]) -> Result<Vec<Session>, String> {
    let listening = if named.is_empty() {
        parse_ss_listening(&run_or(&["ss", "-tlnH"])? )
    } else {
        Vec::new()
    };
    let ports = ports_to_watch(named, listening);
    if ports.is_empty() {
        return Ok(Vec::new());
    }
    let text = run_or(&["ss", "-tinH", "state", "established"])?;
    Ok(parse_ss_sessions(&text, &ports)
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
            delivery: row.delivery,
            mss: row.mss,
            lastsnd: row.lastsnd,
            lastrcv: row.lastrcv,
            raw: row.raw,
            ..Session::default()
        })
        .collect())
}

pub fn source_note() -> &'static str {
    "measured by the kernel via ss, nothing sent"
}

pub fn missing() -> Vec<String> {
    opscope_core::missing(&["ss"])
}

pub fn missing_reason() -> &'static [&'static str] {
    &[
        "ss reads the kernel's own per-socket metrics, which is where",
        "every figure here comes from: round-trip time, retransmits,",
        "delivery rate. Nothing else on the machine reports them.",
    ]
}

pub fn install_hint() -> &'static str {
    "apt install iproute2"
}

pub fn empty_note() -> &'static str {
    "Nothing is connected to this machine, or ss cannot see it."
}
