// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Parsers for both link acquisition paths.
//!
//! This module is deliberately compiled on every target. Platform gates
//! choose which command supplies bytes; they must never hide a parser or its
//! fixtures from the other platform's CI run.

use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedSession {
    pub peer: String,
    pub ip: String,
    pub port: u16,
    pub rtt: Option<f64>,
    pub jitter: Option<f64>,
    pub floor: Option<f64>,
    pub sent: f64,
    pub recv: f64,
    pub retrans_bytes: f64,
    pub delivery: Option<f64>,
    pub mss: Option<f64>,
    pub lastsnd: Option<f64>,
    pub lastrcv: Option<f64>,
    pub raw: HashMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NettopSnapshot {
    pub listening: Vec<u16>,
    pub sessions: Vec<ParsedSession>,
}

/// The kernel's own numbers for one Linux socket.
///
/// `ss` mixes `key:value` pairs with space-separated values such as
/// `delivery_rate 45107960bps`. Unknown fields remain available to the
/// detail screen rather than being guessed at.
pub fn parse_ss_metrics(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let words: Vec<&str> = text.split_whitespace().collect();
    for key in ["send", "pacing_rate", "delivery_rate"] {
        if let Some(at) = words.iter().position(|word| *word == key) {
            if let Some(value) = words.get(at + 1) {
                if let Some(bps) = value.strip_suffix("bps") {
                    out.insert(key.to_string(), bps.to_string());
                }
            }
        }
    }
    for word in &words {
        if let Some((key, value)) = word.split_once(':') {
            out.insert(key.to_string(), value.to_string());
        }
    }
    out
}

pub fn num(map: &HashMap<String, String>, key: &str) -> Option<f64> {
    map.get(key).and_then(|value| value.parse().ok())
}

pub fn parse_ss_listening(text: &str) -> Vec<u16> {
    text.lines()
        .filter_map(|line| line.split_whitespace().nth(3))
        .filter_map(|local| local.rsplit_once(':').and_then(|(_, port)| port.parse().ok()))
        .collect()
}

/// Established Linux sockets whose local port belongs to the watched set.
pub fn parse_ss_sessions(text: &str, ports: &[u16]) -> Vec<ParsedSession> {
    let mut found = Vec::new();
    let mut head: Option<Vec<String>> = None;
    for line in text.lines() {
        if !line.starts_with('\t') && !line.starts_with(' ') {
            head = Some(line.split_whitespace().map(str::to_string).collect());
            continue;
        }
        let cols = match &head {
            Some(cols) if cols.len() >= 4 => cols,
            _ => continue,
        };
        let (local, peer) = (&cols[2], &cols[3]);
        let Some(local_port) = local.rsplit_once(':').and_then(|(_, port)| port.parse().ok()) else {
            head = None;
            continue;
        };
        let Some((peer_host, peer_port)) = peer.rsplit_once(':') else {
            head = None;
            continue;
        };
        let peer_host = peer_host.trim_matches(|c| c == '[' || c == ']');
        let peer_ip = peer_host.strip_prefix("::ffff:").unwrap_or(peer_host);
        if !ports.contains(&local_port) || is_loopback(peer_ip) {
            head = None;
            continue;
        }
        let metrics = parse_ss_metrics(line);
        let rtt_pair = metrics.get("rtt").cloned().unwrap_or_default();
        let mut halves = rtt_pair.split('/');
        found.push(ParsedSession {
            peer: format!("{}:{}", peer_ip, peer_port),
            ip: peer_ip.to_string(),
            port: local_port,
            rtt: halves.next().and_then(|value| value.parse().ok()),
            jitter: halves.next().and_then(|value| value.parse().ok()),
            floor: num(&metrics, "minrtt"),
            sent: num(&metrics, "bytes_sent").unwrap_or(0.0),
            recv: num(&metrics, "bytes_received").unwrap_or(0.0),
            retrans_bytes: num(&metrics, "bytes_retrans").unwrap_or(0.0),
            delivery: num(&metrics, "delivery_rate"),
            mss: num(&metrics, "mss"),
            lastsnd: num(&metrics, "lastsnd"),
            lastrcv: num(&metrics, "lastrcv"),
            raw: metrics,
        });
        head = None;
    }
    found
}

fn endpoint(text: &str) -> Option<(&str, u16)> {
    let text = text
        .strip_prefix("tcp4 ")
        .or_else(|| text.strip_prefix("tcp6 "))
        .unwrap_or(text);
    let split = text.rfind([':', '.'])?;
    Some((&text[..split], text[split + 1..].parse().ok()?))
}

fn measurement(text: &str) -> Option<f64> {
    text.trim().strip_suffix(" ms").unwrap_or(text.trim()).parse().ok()
}

fn is_loopback(ip: &str) -> bool {
    ip.starts_with("127.") || ip == "::1"
}

/// One unprivileged macOS `nettop` CSV sample.
///
/// Column positions are read from the header because `nettop` documents
/// that ordering may change. Process summaries have no endpoint pair;
/// listener rows name a wildcard peer and established rows carry counters.
pub fn parse_nettop_snapshot(text: &str, named_ports: &[u16]) -> NettopSnapshot {
    let mut snapshot = NettopSnapshot::default();
    let mut columns: HashMap<&str, usize> = HashMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.trim_end_matches(',').split(',').collect();
        if fields.first() == Some(&"") {
            columns = fields
                .iter()
                .enumerate()
                .skip(1)
                .map(|(index, name)| (*name, index))
                .collect();
            continue;
        }
        let Some(description) = fields.first() else {
            continue;
        };
        let Some((local, peer)) = description.split_once("<->") else {
            continue;
        };
        let Some((local_host, local_port)) = endpoint(local) else {
            continue;
        };
        if peer == "*:*" || peer == "*.*" {
            snapshot.listening.push(local_port);
            continue;
        }
        let Some((peer_ip, peer_port)) = endpoint(peer) else {
            continue;
        };
        if is_loopback(peer_ip) {
            continue;
        }
        if local_host == "*" {
            continue;
        }
        let get = |name: &str| columns.get(name).and_then(|index| fields.get(*index)).copied();
        let mut raw = HashMap::new();
        for name in ["bytes_in", "bytes_out", "re-tx", "rtt_min", "rtt_avg", "rtt_var"] {
            if let Some(value) = get(name).filter(|value| !value.is_empty()) {
                raw.insert(name.to_string(), value.to_string());
            }
        }
        snapshot.sessions.push(ParsedSession {
            peer: format!("{}:{}", peer_ip, peer_port),
            ip: peer_ip.to_string(),
            port: local_port,
            rtt: get("rtt_avg").and_then(measurement),
            jitter: get("rtt_var").and_then(measurement),
            floor: get("rtt_min").and_then(measurement),
            sent: get("bytes_out").and_then(measurement).unwrap_or(0.0),
            recv: get("bytes_in").and_then(measurement).unwrap_or(0.0),
            retrans_bytes: get("re-tx").and_then(measurement).unwrap_or(0.0),
            raw,
            ..ParsedSession::default()
        });
    }
    snapshot.listening.sort_unstable();
    snapshot.listening.dedup();
    let watched = if named_ports.is_empty() {
        &snapshot.listening
    } else {
        named_ports
    };
    snapshot.sessions.retain(|row| watched.contains(&row.port));
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_shapes_on_the_ss_line_are_read() {
        let line = "\t ts sack cubic rtt:3.604/1.027 minrtt:3.553 cwnd:10 \
                    bytes_sent:1669 delivery_rate 6287464bps";
        let metrics = parse_ss_metrics(line);
        assert_eq!(metrics.get("rtt").map(String::as_str), Some("3.604/1.027"));
        assert_eq!(metrics.get("minrtt").map(String::as_str), Some("3.553"));
        assert_eq!(metrics.get("delivery_rate").map(String::as_str), Some("6287464"));
        assert_eq!(num(&metrics, "cwnd"), Some(10.0));
    }

    #[test]
    fn macos_nettop_rows_keep_real_metrics_and_only_inbound_sessions() {
        let text = concat!(
            ",bytes_in,bytes_out,re-tx,rtt_min,rtt_avg,rtt_var,\n",
            "Example.41,700,900,3,1.00 ms,2.00 ms,0.50 ms,\n",
            "tcp4 *:3000<->*:*,\n",
            "tcp4 192.0.2.10:3000<->203.0.113.9:51000,120,340,4,1.09 ms,1.22 ms,0.38 ms,\n",
            "tcp4 192.0.2.10:4000<->203.0.113.10:52000,50,75,0,2 ms,3 ms,1 ms,\n",
            "tcp6 ::1.3000<->::1.52001,8,9,0,0.1 ms,0.2 ms,0.1 ms,\n",
        );
        let got = parse_nettop_snapshot(text, &[]);
        assert_eq!(got.listening, vec![3000]);
        assert_eq!(got.sessions.len(), 1);
        let row = &got.sessions[0];
        assert_eq!((row.port, row.sent, row.recv, row.retrans_bytes), (3000, 340.0, 120.0, 4.0));
        assert_eq!((row.floor, row.rtt, row.jitter), (Some(1.09), Some(1.22), Some(0.38)));
        assert_eq!(row.peer, "203.0.113.9:51000");
    }

    #[test]
    fn named_ports_pin_the_macos_set_even_without_a_listener_row() {
        let text = concat!(
            ",bytes_in,bytes_out,re-tx,rtt_min,rtt_avg,rtt_var,\n",
            "tcp4 192.0.2.10:4000<->203.0.113.10:52000,50,75,0,2 ms,3 ms,1 ms,\n",
        );
        let got = parse_nettop_snapshot(text, &[4000]);
        assert_eq!(got.sessions.len(), 1);
    }
}
