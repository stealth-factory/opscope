// terminal-toys - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Tailscale network: who is online, and how you are reaching them.
//!
//! A port of tailnet.py. The column that matters is PATH: a peer is either
//! DIRECT, meaning NAT traversal succeeded, or relayed through a named DERP
//! region, meaning every packet round-trips through Tailscale's own
//! infrastructure - a difference `tailscale status` does not show unless
//! you go looking for it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Local, NaiveDateTime, TimeZone};
use toys_core as tc;

const SPARK: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Cycled at runtime with n, the same way the latency monitor's i key does.
const REFRESH_CHOICES: &[f64] = &[1.0, 2.0, 5.0, 10.0, 30.0];

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Seconds before an external command is given up on, from tailnet.py's longest, on `tailscale status`.
const RUN_TIMEOUT: u64 = 25;

fn run(args: &[&str]) -> String {
    // Bounded: .output() waits forever, and a wedged child used to freeze
    // the poll thread with the pane still showing its last frame.
    tc::run(args, RUN_TIMEOUT).unwrap_or_default()
}

fn text(value: &serde_json::Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_string()
}

fn bytes_at(value: &serde_json::Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or(0)
}

/// Seconds since tailscaled started.
///
/// Its byte counters live in memory and reset with it, so RX/TX cover this
/// window rather than all time - a peer reading 0B may just predate a
/// restart.
fn daemon_uptime() -> Option<f64> {
    let hz = {
        let n = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if n > 0 { n as f64 } else { 100.0 }
    };
    for entry in std::fs::read_dir("/proc").ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let comm = std::fs::read_to_string(format!("/proc/{}/comm", name)).unwrap_or_default();
        if comm.trim() != "tailscaled" {
            continue;
        }
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", name)).ok()?;
        let rest = stat.rsplit_once(')')?.1;
        let started: f64 = rest.split_whitespace().nth(19)?.parse().ok()?;
        let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
        let up: f64 = uptime.split_whitespace().next()?.parse().ok()?;
        return Some(up - started / hz);
    }
    None
}

/// Tailnet-unique display name.
///
/// HostName is whatever the device calls itself and is frequently useless:
/// iPads, Chromecasts and Pixels all report "localhost", and two Apple TVs
/// report the same "apple-tv". The first label of the MagicDNS name is
/// unique across the tailnet and matches what the admin console shows.
fn peer_name(peer: &serde_json::Value) -> String {
    let dns = text(peer, "DNSName");
    let dns = dns.trim_end_matches('.');
    if !dns.is_empty() {
        return dns.split('.').next().unwrap_or(dns).to_string();
    }
    match text(peer, "HostName") {
        s if s.is_empty() => "?".into(),
        s => s,
    }
}

/// public | private | tailscale | other, from the address alone.
fn classify(ip: &str) -> &'static str {
    let ip = ip.split('%').next().unwrap_or(ip);
    if ip.contains(':') {
        return if ip.to_lowercase().starts_with("fd7a:") {
            "tailscale"
        } else {
            "other"
        };
    }
    let parts: Vec<&str> = ip.split('.').collect();
    let (a, b) = match (
        parts.first().and_then(|x| x.parse::<u32>().ok()),
        parts.get(1).and_then(|x| x.parse::<u32>().ok()),
    ) {
        (Some(a), Some(b)) => (a, b),
        _ => return "other",
    };
    if a == 100 && (64..=127).contains(&b) {
        return "tailscale"; // the CGNAT range Tailscale itself uses
    }
    if a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168) {
        return "private";
    }
    if (a == 169 && b == 254) || a == 127 {
        return "other";
    }
    "public"
}

/// Is an IPv4 address inside a CIDR block?
fn in_network(ip: &str, cidr: &str) -> bool {
    let Some((net, bits)) = cidr.split_once('/') else {
        return false;
    };
    if net.contains(':') {
        return false;
    }
    let Ok(bits) = bits.parse::<u32>() else {
        return false;
    };
    if bits > 32 {
        return false;
    }
    let to_int = |a: &str| -> Option<u32> {
        let parts: Vec<u32> = a.split('.').filter_map(|o| o.parse().ok()).collect();
        if parts.len() != 4 || parts.iter().any(|o| *o > 255) {
            return None;
        }
        Some(parts.iter().enumerate().map(|(i, o)| o << (24 - 8 * i)).sum())
    };
    let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
    match (to_int(ip), to_int(net)) {
        (Some(a), Some(b)) => (a & mask) == (b & mask),
        _ => false,
    }
}

/// Lower is better. Prefers a real LAN address over a virtual bridge.
///
/// A peer often exposes several private endpoints, and docker0 (172.17.0.1)
/// or a k8s bridge is not the address anyone wants to copy. An address
/// inside a subnet the peer advertises is almost certainly its real LAN one.
fn lan_rank(ip: &str, routes: &[String]) -> u8 {
    if routes.iter().any(|r| in_network(ip, r)) {
        return 0;
    }
    let parts: Vec<u32> = ip.split('.').filter_map(|x| x.parse().ok()).collect();
    match (parts.first(), parts.get(1)) {
        (Some(192), Some(168)) => 1,
        (Some(10), _) => 2,
        _ => 3, // 172.16-31: usually docker or another virtual bridge
    }
}

/// Peer LAN and public endpoints from the netmap, which needs root.
///
/// Optional enrichment: `tailscale status` does not carry peer endpoints, so
/// without this the panel simply offers fewer addresses to copy. `sudo -n`
/// so it fails instantly rather than prompting when sudo wants a password.
fn endpoints_by_peer() -> HashMap<String, Vec<String>> {
    let mut found = HashMap::new();
    let text_out = run(&["sudo", "-n", "tailscale", "debug", "netmap"]);
    let data: serde_json::Value = match serde_json::from_str(&text_out) {
        Ok(d) => d,
        Err(_) => return found,
    };
    for peer in data["Peers"].as_array().into_iter().flatten() {
        let name = text(peer, "Name").trim_end_matches('.').to_string();
        if name.is_empty() {
            continue;
        }
        found.insert(
            name,
            peer["Endpoints"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e.as_str())
                .map(|e| e.rsplit_once(':').map(|(h, _)| h).unwrap_or(e).to_string())
                .collect(),
        );
    }
    found
}

/// DERP region code to city, from the local map.
///
/// A peer's home region is the Tailscale POP nearest to it, so this gives a
/// location hint without sending anyone's address to a geolocation service.
fn derp_regions() -> HashMap<String, String> {
    let mut out = HashMap::new();
    let text_out = run(&["tailscale", "debug", "derp-map"]);
    let data: serde_json::Value = match serde_json::from_str(&text_out) {
        Ok(d) => d,
        Err(_) => return out,
    };
    for region in data["Regions"].as_object().into_iter().flatten().map(|(_, v)| v) {
        let code = text(region, "RegionCode");
        if !code.is_empty() {
            let city = text(region, "RegionName");
            out.insert(code.clone(), if city.is_empty() { code } else { city });
        }
    }
    out
}

fn spark(values: &[f64], n: usize, peak: f64) -> String {
    let vals = &values[values.len().saturating_sub(n)..];
    if vals.is_empty() {
        return String::new();
    }
    let hi = if peak > 0.0 {
        peak
    } else {
        vals.iter().cloned().fold(0.0f64, f64::max)
    };
    if hi <= 0.0 {
        return "·".repeat(vals.len());
    }
    vals.iter()
        .map(|v| SPARK[(((v / hi) * 7.99) as usize).min(7)])
        .collect()
}

fn ago(s: f64) -> String {
    let s = s.max(0.0) as i64;
    if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 172_800 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86400)
    }
}

/// A byte count in exactly five cells, so columns stay aligned.
fn human(n: u64) -> String {
    let mut v = n as f64;
    for unit in ["B", "K", "M", "G", "T"] {
        if v < 1024.0 {
            let body = if v >= 10.0 || unit == "B" {
                format!("{:.0}{}", v, unit)
            } else {
                format!("{:.1}{}", v, unit)
            };
            return format!("{:>5}", body);
        }
        v /= 1024.0;
    }
    format!("{:>5}", format!("{:.0}P", v))
}

/// A per-second byte rate in six cells.
fn rate(v: f64) -> String {
    format!("{:>5}/s", human(v as u64).trim())
}

fn parse_iso(iso: &str) -> Option<NaiveDateTime> {
    if iso.len() < 19 || iso.starts_with("0001-01-01") {
        return None;
    }
    NaiveDateTime::parse_from_str(&iso[..19], "%Y-%m-%dT%H:%M:%S").ok()
}

/// The age of an ISO-8601 timestamp, coarsely.
fn seen(iso: &str) -> String {
    let Some(at) = parse_iso(iso) else {
        return "  -".into();
    };
    let s = (chrono::Utc::now().naive_utc() - at).num_seconds().max(0);
    if s < 90 {
        "now".into()
    } else if s < 5400 {
        format!("{}m", s / 60)
    } else if s < 172_800 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

/// ISO-8601 to a readable local time, or nothing when unset.
fn stamp(iso: &str) -> String {
    let Some(at) = parse_iso(iso) else {
        return String::new();
    };
    match Local.from_utc_datetime(&at).format("%Y-%m-%d %H:%M").to_string() {
        s => s,
    }
}

/// The addresses worth copying for a peer, as (label, value) pairs.
fn addresses(peer: &serde_json::Value, eps: &HashMap<String, Vec<String>>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let ips: Vec<String> = peer["TailscaleIPs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i.as_str().map(String::from))
        .collect();
    let v4: Vec<&String> = ips.iter().filter(|i| !i.contains(':')).collect();
    let v6: Vec<&String> = ips.iter().filter(|i| i.contains(':')).collect();
    if let Some(first) = v4.first() {
        out.push(("Tailscale IP".into(), (*first).clone()));
    }
    let dns = text(peer, "DNSName").trim_end_matches('.').to_string();
    if !dns.is_empty() {
        out.push(("MagicDNS name".into(), dns.clone()));
    }

    let (mut pub_ips, mut priv_ips): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let cur = text(peer, "CurAddr");
    let cur = cur.rsplit_once(':').map(|(h, _)| h).unwrap_or(&cur).to_string();
    if !cur.is_empty() && classify(&cur) == "public" {
        pub_ips.push(cur);
    }
    for ip in eps.get(&dns).into_iter().flatten() {
        match classify(ip) {
            "public" if !pub_ips.contains(ip) => pub_ips.push(ip.clone()),
            "private" if !priv_ips.contains(ip) => priv_ips.push(ip.clone()),
            _ => {}
        }
    }
    // PrimaryRoutes only: AllowedIPs also carries 0.0.0.0/0 for exit nodes,
    // which would match every address and defeat the ranking entirely.
    let routes = primary_routes(peer);
    priv_ips.sort_by_key(|i| lan_rank(i, &routes));
    if let Some(first) = pub_ips.first() {
        out.push(("Public IP".into(), first.clone()));
    }
    if let Some(first) = priv_ips.first() {
        out.push(("Private IP (LAN)".into(), first.clone()));
        if let Some(second) = priv_ips.get(1) {
            out.push(("Other private IP".into(), second.clone()));
        }
    }
    if let Some(first) = v6.first() {
        out.push(("Tailscale IPv6".into(), (*first).clone()));
    }
    out
}

fn primary_routes(peer: &serde_json::Value) -> Vec<String> {
    peer["PrimaryRoutes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str())
        .filter(|r| *r != "0.0.0.0/0" && *r != "::/0")
        .map(String::from)
        .collect()
}

/// Pings whichever peer is selected, keeping per-peer history.
///
/// Probing every peer continuously would mean two dozen ping processes for
/// data nobody is looking at, so exactly one runs at a time and follows the
/// selection. History is kept per peer, so returning to one still shows its
/// earlier samples.
#[derive(Default)]
struct Prober {
    samples: HashMap<String, Vec<Option<f64>>>,
    want: Option<(String, String)>,
    pid: Option<i32>,
}

fn rtt_of(line: &str) -> Option<f64> {
    let at = line.find("time=")? + 5;
    let rest = &line[at..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn prober_loop(shared: Arc<Mutex<Prober>>) {
    loop {
        let target = shared.lock().ok().and_then(|g| g.want.clone());
        let Some((machine, ip)) = target else {
            std::thread::sleep(Duration::from_millis(400));
            continue;
        };
        let child = std::process::Command::new("ping")
            .args(["-n", "-O", "-i", "1", &ip])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(_) => {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        if let Ok(mut g) = shared.lock() {
            g.pid = Some(child.id() as i32);
        }
        if let Some(stdout) = child.stdout.take() {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let mut g = match shared.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                // The selection moved on; this ping is for a peer nobody is
                // looking at any more.
                if g.want.as_ref().map(|w| w.0.clone()) != Some(machine.clone()) {
                    break;
                }
                let hist = g.samples.entry(machine.clone()).or_default();
                if let Some(rtt) = rtt_of(&line) {
                    hist.push(Some(rtt));
                } else if line.contains("no answer yet") || line.contains("Unreachable") {
                    hist.push(None);
                }
                if hist.len() > 120 {
                    let drop = hist.len() - 120;
                    hist.drain(..drop);
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        if let Ok(mut g) = shared.lock() {
            g.pid = None;
        }
    }
}

fn watch(shared: &Arc<Mutex<Prober>>, machine: &str, ip: Option<String>) {
    let kill = {
        let Ok(mut g) = shared.lock() else { return };
        if g.want.as_ref().is_some_and(|w| w.0 == machine) {
            return;
        }
        g.want = ip.map(|ip| (machine.to_string(), ip));
        g.pid.take()
    };
    // The running ping belongs to the peer that was selected a moment ago,
    // so it is stopped rather than left to finish into a history nobody
    // will read.
    if let Some(pid) = kill {
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

#[derive(Default)]
struct State {
    data: Option<serde_json::Value>,
    endpoints: HashMap<String, Vec<String>>,
    err: String,
    rates: HashMap<String, Vec<(f64, f64)>>,
    counters: HashMap<String, (u64, u64, f64)>,
    endpoints_at: f64,
}

/// Turn cumulative byte counters into per-second rates.
fn sample_rates(state: &mut State, data: &serde_json::Value, history: usize) {
    let at = now();
    for peer in data["Peer"].as_object().into_iter().flatten().map(|(_, v)| v) {
        let key = peer_name(peer);
        let (rx, tx) = (bytes_at(peer, "RxBytes"), bytes_at(peer, "TxBytes"));
        let prev = state.counters.insert(key.clone(), (rx, tx, at));
        let Some((was_rx, was_tx, when)) = prev else {
            continue;
        };
        let dt = at - when;
        if dt <= 0.0 {
            continue;
        }
        // A tailscaled restart zeroes the counters; report no traffic rather
        // than a large negative spike.
        let drx = rx.saturating_sub(was_rx) as f64 / dt;
        let dtx = tx.saturating_sub(was_tx) as f64 / dt;
        let hist = state.rates.entry(key).or_default();
        hist.push((drx, dtx));
        if hist.len() > history {
            let drop = hist.len() - history;
            hist.drain(..drop);
        }
    }
}

struct Palette {
    online: String,
    offline: String,
    direct: String,
    relay: String,
    dim: String,
    txt: String,
    lbl: String,
    accent: String,
    route: String,
    exit: String,
}

fn palette() -> Palette {
    Palette {
        online: tc::rgb(90, 240, 160),
        offline: tc::rgb(120, 130, 150),
        direct: tc::rgb(90, 240, 160),
        relay: tc::rgb(255, 190, 90),
        dim: tc::rgb(127, 147, 172),
        txt: tc::rgb(225, 235, 245),
        lbl: tc::rgb(130, 165, 200),
        accent: tc::rgb(120, 200, 255),
        route: tc::rgb(200, 160, 255),
        exit: tc::rgb(255, 140, 200),
    }
}

/// Live throughput for peers that are actually moving data.
fn activity_rows(
    rates: &HashMap<String, Vec<(f64, f64)>>,
    w: usize,
    limit: usize,
    p: &Palette,
) -> Vec<String> {
    let mut active: Vec<(f64, &String, &Vec<(f64, f64)>)> = rates
        .iter()
        .filter_map(|(name, hist)| {
            if hist.is_empty() {
                return None;
            }
            let recent = &hist[hist.len().saturating_sub(30)..];
            let peak = recent.iter().map(|(r, t)| r + t).fold(0.0f64, f64::max);
            // Ignore keepalives: a tailnet is never entirely silent.
            if peak < 64.0 {
                return None;
            }
            Some((peak, name, hist))
        })
        .collect();
    active.sort_by(|a, b| b.0.total_cmp(&a.0));
    if active.is_empty() {
        return vec![tc::seg(
            &[(p.dim.as_str(), " no peer traffic in the last few minutes".into())],
            w - 1,
        )];
    }
    let sw = (w.saturating_sub(42)).clamp(8, 28);
    let peak = active[0].0;
    active
        .iter()
        .take(limit)
        .map(|(_, name, hist)| {
            let rx: Vec<f64> = hist.iter().map(|(r, _)| *r).collect();
            let tx: Vec<f64> = hist.iter().map(|(_, t)| *t).collect();
            tc::seg(
                &[
                    (
                        p.txt.as_str(),
                        format!(
                            " {}",
                            tc::pad(&name.chars().take(18).collect::<String>(), 19)
                        ),
                    ),
                    (p.online.as_str(), format!("↓{}", spark(&rx, sw, peak))),
                    (p.relay.as_str(), format!(" ↑{}", spark(&tx, sw, peak))),
                    (
                        p.dim.as_str(),
                        format!(
                            "  {} {}",
                            rate(*rx.last().unwrap_or(&0.0)),
                            rate(*tx.last().unwrap_or(&0.0))
                        ),
                    ),
                ],
                w - 1,
            )
        })
        .collect()
}

fn main() {
    tc::maybe_help(include_str!("tailnet_help.txt"));
    let cfg = tc::load_config("tailnet");
    let mut refresh = tc::cfg_f64(&cfg, "refresh", 2.0);
    let history = tc::cfg_usize(&cfg, "history", 180);
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() >= 2 && (args[0] == "-n" || args[0] == "--refresh") {
        refresh = args[1].parse::<f64>().unwrap_or(2.0).max(1.0);
    }

    let absent = tc::missing(&["tailscale"]);
    if !absent.is_empty() {
        tc::cannot_start(
            "tailnet",
            &absent,
            &[
                "Everything here comes from the local tailscaled through its",
                "own CLI: who is online, whether each peer is direct or",
                "relayed, and the byte counters the WireGuard engine keeps.",
                "",
                "There is no other source for any of it, and nothing here is",
                "sent anywhere - the DERP region names come from the local",
                "map rather than from a geolocation service.",
            ],
            "see https://tailscale.com/download",
        );
        return;
    }

    let p = palette();
    let state = Arc::new(Mutex::new(State::default()));
    let prober = Arc::new(Mutex::new(Prober::default()));
    let wake = Arc::new((Mutex::new(false), Condvar::new()));
    let refresh_now = Arc::new(Mutex::new(refresh));

    let poller = Arc::clone(&state);
    let poller_wake = Arc::clone(&wake);
    let poller_refresh = Arc::clone(&refresh_now);
    std::thread::spawn(move || loop {
        let out = run(&["tailscale", "status", "--json"]);
        let parsed: Option<serde_json::Value> = serde_json::from_str(&out).ok();
        // The netmap needs sudo and changes slowly, so it is asked for far
        // less often than the status is.
        let want_eps = poller
            .lock()
            .map(|g| now() - g.endpoints_at > 60.0)
            .unwrap_or(false);
        let eps = if want_eps { Some(endpoints_by_peer()) } else { None };
        if let Ok(mut g) = poller.lock() {
            match &parsed {
                None => g.err = "tailscale CLI unavailable or not logged in".into(),
                Some(d) => {
                    g.err.clear();
                    sample_rates(&mut g, d, history);
                    g.data = Some(d.clone());
                }
            }
            if let Some(eps) = eps {
                if !eps.is_empty() {
                    g.endpoints = eps;
                }
                g.endpoints_at = now();
            }
        }
        let wait = poller_refresh.lock().map(|g| *g).unwrap_or(2.0);
        let (lock, cond) = &*poller_wake;
        let mut asked = match lock.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if !*asked {
            asked = match cond.wait_timeout(asked, Duration::from_secs_f64(wait)) {
                Ok((g, _)) => g,
                Err(_) => return,
            };
        }
        *asked = false;
    });

    let probing = Arc::clone(&prober);
    std::thread::spawn(move || prober_loop(probing));

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let (mut hide_offline, mut show_graph) = (false, true);
    let (mut selected, mut scroll, mut visible) = (0usize, 0usize, 1usize);
    // None, "copy" or "info".
    let mut view: Option<&'static str> = None;
    // Where the copy list was opened from, so closing it puts you back
    // there. It used to drop to the machine list whichever screen you had
    // been reading, which loses your place for the sake of one keystroke.
    let mut copy_from: Option<&'static str> = None;
    let mut note: (String, f64) = (String::new(), 0.0);
    let mut listed: Vec<serde_json::Value> = Vec::new();
    let derp = derp_regions();

    let nudge = |wake: &Arc<(Mutex<bool>, Condvar)>| {
        let (lock, cond) = &**wake;
        if let Ok(mut asked) = lock.lock() {
            *asked = true;
            cond.notify_all();
        }
    };

    loop {
        let (data, eps_now, err, rates) = match state.lock() {
            Ok(g) => (g.data.clone(), g.endpoints.clone(), g.err.clone(), g.rates.clone()),
            Err(_) => return,
        };

        for key in keyboard.poll() {
            if view.is_some() {
                match key.as_str() {
                    // Left comes back out, the way it does everywhere
                    // else: right and enter go in, left and esc come
                    // out, and no widget needs a letter of its own for
                    // it. `i` used to open this and no longer does.
                    "left" | "esc" => {
                        view = copy_from.take();
                    }
                    // q quits from here too. It used to close the view
                    // instead, which is its own kind of trap: the key
                    // appears to do nothing to a widget you are trying
                    // to leave, and every other widget quits on it.
                    "q" | "Q" => {
                        keyboard.restore();
                        tc::restore_screen();
                        return;
                    }
                    "c" | "C" => {
                        view = if view != Some("copy") {
                            copy_from = view;
                            Some("copy")
                        } else {
                            copy_from.take()
                        }
                    }
                    digit
                        if view == Some("copy")
                            && digit.len() == 1
                            && digit.chars().all(|c| c.is_ascii_digit())
                            && !listed.is_empty() =>
                    {
                        let pairs =
                            addresses(&listed[selected.min(listed.len() - 1)], &eps_now);
                        let at = digit.parse::<usize>().unwrap_or(0);
                        if at >= 1 && at <= pairs.len() {
                            let (label, value) = &pairs[at - 1];
                            note = (
                                if tc::clipboard(value) {
                                    format!("✓ copied {}", label.to_lowercase())
                                } else {
                                    "! no clipboard; select the text with the mouse".into()
                                },
                                now() + 3.0,
                            );
                        }
                    }
                    _ => {}
                }
                continue;
            }
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "r" | "R" => nudge(&wake),
                "o" | "O" => {
                    hide_offline = !hide_offline;
                    selected = 0;
                }
                "g" | "G" => show_graph = !show_graph,
                // i, as latency names the same key. n was this widget's
                // own letter for it and meant nothing to a reader coming
                // from the widget beside it; i was free here because the
                // info screen moved to the arrows.
                "i" | "I" => {
                    if let Ok(mut g) = refresh_now.lock() {
                        *g = tc::cycle(REFRESH_CHOICES, *g);
                    }
                    nudge(&wake); // apply the new interval immediately
                }
                "up" => selected = selected.saturating_sub(1),
                "down" => selected += 1,
                "pgup" => selected = selected.saturating_sub(visible),
                "pgdn" => selected += visible,
                "home" => selected = 0,
                "end" => selected = listed.len().saturating_sub(1),
                "c" | "C" => {
                    if !listed.is_empty() {
                        view = Some("copy");
                        copy_from = None;
                        note = (String::new(), 0.0);
                    }
                }
                "right" | "enter" => {
                    if !listed.is_empty() {
                        view = Some("info");
                        copy_from = None;
                    }
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let interval = refresh_now.lock().map(|g| *g).unwrap_or(2.0);
        if !note.0.is_empty() && now() > note.1 {
            note = (String::new(), 0.0);
        }
        let mut rows = vec![tc::title("tailnet", w, &p.accent)];
        let Some(data) = data else {
            rows.push(tc::seg(
                &[(
                    p.relay.as_str(),
                    format!(" {}", if err.is_empty() { "connecting…" } else { &err }),
                )],
                w - 1,
            ));
            tc::draw(&rows, w, h);
            std::thread::sleep(Duration::from_millis(400));
            continue;
        };

        let me = data["Self"].clone();
        let peers: Vec<serde_json::Value> = data["Peer"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, v)| v.clone())
            .collect();
        let online: Vec<&serde_json::Value> = peers
            .iter()
            .filter(|x| x["Online"].as_bool().unwrap_or(false))
            .collect();
        let direct = online.iter().filter(|x| !text(x, "CurAddr").is_empty()).count();
        let routers: Vec<&serde_json::Value> = peers
            .iter()
            .filter(|x| !primary_routes(x).is_empty())
            .collect();
        let exits: Vec<&serde_json::Value> = peers
            .iter()
            .filter(|x| x["ExitNode"].as_bool().unwrap_or(false))
            .collect();

        let my_name = text(&me, "DNSName");
        rows.push(tc::seg(
            &[
                (
                    p.txt.as_str(),
                    format!(
                        " {}",
                        my_name.trim_end_matches('.').split('.').next().unwrap_or("")
                    ),
                ),
                (
                    p.dim.as_str(),
                    format!(
                        "  {}",
                        me["TailscaleIPs"][0].as_str().unwrap_or("?")
                    ),
                ),
                (p.dim.as_str(), format!("  {}", text(&data, "MagicDNSSuffix"))),
            ],
            w - 1,
        ));
        rows.push(tc::seg(
            &[
                (p.online.as_str(), format!(" {} online", online.len())),
                (p.dim.as_str(), format!(" / {} peers", peers.len())),
                (p.direct.as_str(), format!("   {} direct", direct)),
                (
                    p.relay.as_str(),
                    format!("   {} relayed", online.len() - direct),
                ),
                (p.dim.as_str(), format!("   every {}s", interval)),
            ],
            w - 1,
        ));
        rows.push(tc::seg(
            &[
                (p.route.as_str(), format!(" {} advertising routes", routers.len())),
                (
                    p.exit.as_str(),
                    format!(
                        "   exit node: {}",
                        match exits.first() {
                            Some(x) => peer_name(x),
                            None => "none".into(),
                        }
                    ),
                ),
            ],
            w - 1,
        ));
        rows.push(String::new());

        if let (Some(which), false) = (view, listed.is_empty()) {
            let chosen = listed[selected.min(listed.len() - 1)].clone();
            let body = if which == "info" {
                let users: HashMap<String, serde_json::Value> = data["User"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let latency = prober
                    .lock()
                    .ok()
                    .and_then(|g| g.samples.get(&peer_name(&chosen)).cloned())
                    .unwrap_or_default();
                info_overlay(&chosen, &eps_now, &users, w, h, &rates, &latency, &derp, &p)
            } else {
                copy_overlay(&chosen, &eps_now, w, h, &note.0, &p)
            };
            tc::draw(&body, w, h);
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        if show_graph {
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ── LIVE THROUGHPUT ── ".into()),
                    (p.dim.as_str(), "peers moving data".into()),
                ],
                w - 1,
            ));
            rows.extend(activity_rows(&rates, w, 4, &p));
            rows.push(String::new());
        }

        let wide = w >= 62;
        // Machine names are long; spend spare width on them, not padding.
        let namew = if wide {
            (w.saturating_sub(45)).clamp(16, 32)
        } else {
            w.saturating_sub(22).max(12)
        };
        let mut head = format!(" {} {:<8} {:<7}", tc::pad("MACHINE", namew + 1), "OS", "PATH");
        if wide {
            head += &format!(" {:>5} {:>5} {:>5}", "RX", "TX", "SEEN");
        }
        rows.push(tc::seg(&[(p.lbl.as_str(), tc::pad(&head, w - 1))], w - 1));
        if wide {
            let span = daemon_uptime();
            rows.push(tc::seg(
                &[(
                    p.dim.as_str(),
                    format!(
                        " rx/tx = this host ↔ peer, since tailscaled started{}",
                        match span {
                            Some(s) => format!(" {}", ago(s)),
                            None => String::new(),
                        }
                    ),
                )],
                w - 1,
            ));
        }

        if let Some(sel_peer) = listed.get(selected.min(listed.len().saturating_sub(1))) {
            let ip4 = sel_peer["TailscaleIPs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|i| i.as_str())
                .find(|i| !i.contains(':'))
                .map(String::from);
            // Nothing is learned by pinging ourselves, and the round trip
            // would read as a suspiciously good link.
            let live = sel_peer["Online"].as_bool().unwrap_or(false)
                && !sel_peer["_self"].as_bool().unwrap_or(false);
            watch(
                &prober,
                &peer_name(sel_peer),
                if live { ip4 } else { None },
            );
        }

        let mut sorted = peers.clone();
        sorted.sort_by_key(|x| {
            (
                !x["Online"].as_bool().unwrap_or(false),
                text(x, "CurAddr").is_empty(),
                std::cmp::Reverse(bytes_at(x, "RxBytes") + bytes_at(x, "TxBytes")),
            )
        });
        listed = sorted
            .into_iter()
            .filter(|x| !(hide_offline && !x["Online"].as_bool().unwrap_or(false)))
            .collect();
        // This machine belongs in the list of machines - it was only ever in
        // the header - but never in the counts above it: "3 direct, 2
        // relayed" describes connections out of here, and there is no
        // connection from here to here. It pins to the top rather than
        // sorting by traffic, because where you are is not a ranking.
        if !me.is_null() {
            let mut mine = me.clone();
            mine["_self"] = serde_json::Value::Bool(true);
            listed.insert(0, mine);
        }
        if !listed.is_empty() && selected >= listed.len() {
            selected = listed.len() - 1;
        }
        visible = h.saturating_sub(rows.len() + 2).max(1);
        if selected < scroll {
            scroll = selected;
        } else if selected >= scroll + visible {
            scroll = selected - visible + 1;
        }
        scroll = scroll.min(listed.len().saturating_sub(visible));

        for (idx, peer) in listed.iter().enumerate().skip(scroll).take(visible) {
            if rows.len() >= h.saturating_sub(2) {
                break;
            }
            let mine = peer["_self"].as_bool().unwrap_or(false);
            let up = mine || peer["Online"].as_bool().unwrap_or(false);
            let here = idx == selected;
            let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
            let c = |colour: &str| format!("{}{}", tint, colour);
            let path_direct = !text(peer, "CurAddr").is_empty();
            // "this" rather than DIRECT or a relay name: the path column
            // answers how the traffic gets there, and for this machine it
            // does not go anywhere.
            let path = if mine {
                "this".to_string()
            } else if path_direct {
                "DIRECT".to_string()
            } else {
                match text(peer, "Relay") {
                    s if s.is_empty() => "?".into(),
                    s => s,
                }
            };
            let name = peer_name(peer);
            let mut line = vec![
                (
                    c(if mine {
                        &p.accent
                    } else if up {
                        &p.online
                    } else {
                        &p.offline
                    }),
                    format!(
                        "{}{} ",
                        if here { "▸" } else { " " },
                        if mine { '◆' } else if up { '●' } else { '○' }
                    ),
                ),
                (
                    c(if up { &p.txt } else { &p.offline }),
                    tc::pad(&name.chars().take(namew - 1).collect::<String>(), namew),
                ),
                (
                    c(&p.dim),
                    format!("{:<8}", text(peer, "OS").chars().take(8).collect::<String>()),
                ),
                (
                    c(if mine {
                        &p.accent
                    } else if path_direct {
                        &p.direct
                    } else {
                        &p.relay
                    }),
                    format!("{:<7}", if up { path.as_str() } else { "-" }),
                ),
            ];
            if wide {
                line.push((
                    c(&p.dim),
                    format!(
                        " {} {}",
                        human(bytes_at(peer, "RxBytes")),
                        human(bytes_at(peer, "TxBytes"))
                    ),
                ));
                line.push((
                    c(&p.dim),
                    format!(
                        " {:>5}",
                        if up { "now".to_string() } else { seen(&text(peer, "LastSeen")) }
                    ),
                ));
            }
            if !primary_routes(peer).is_empty() {
                line.push((c(&p.route), " ⇄".into()));
            }
            if here {
                line.push((tint.clone(), " ".repeat(w)));
            }
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
        }

        while rows.len() < h.saturating_sub(2) {
            rows.push(String::new());
        }
        if let Some(first) = routers.first() {
            let rts = primary_routes(first);
            rows.push(tc::seg(
                &[
                    (p.route.as_str(), " ⇄ ".into()),
                    (p.dim.as_str(), format!("{} routes ", text(first, "HostName"))),
                    (p.txt.as_str(), rts.iter().take(2).cloned().collect::<Vec<_>>().join(", ")),
                    (
                        p.dim.as_str(),
                        if rts.len() > 2 {
                            format!(" +{} more", rts.len() - 2)
                        } else {
                            String::new()
                        },
                    ),
                ],
                w - 1,
            ));
        }
        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
            vec![
                (p.accent.as_str(), "→".into()),
                (p.dim.as_str(), "/↵ info".into()),
            ],
            vec![(p.dim.as_str(), "[c]opy".into())],
            vec![(p.dim.as_str(), "[g]raph".into())],
            vec![(p.dim.as_str(), "[o]ffline".into())],
            // The current value is worth the three cells: this key cycles
            // rather than toggles, so "[i]nterval" alone would not say what
            // pressing it is about to change from.
            vec![(p.dim.as_str(), format!("[i]nterval {}s", interval))],
            vec![(p.dim.as_str(), "[r]efresh".into())],
            vec![(p.dim.as_str(), "[q]uit".into())],
        ];
        for line in tc::pack_hints(&hints, w - 2, "  ") {
            rows.push(format!(" {}", line));
        }
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Everything known about one machine, including every address.
#[allow(clippy::too_many_arguments)]
fn info_overlay(
    peer: &serde_json::Value,
    eps: &HashMap<String, Vec<String>>,
    users: &HashMap<String, serde_json::Value>,
    w: usize,
    h: usize,
    rates: &HashMap<String, Vec<(f64, f64)>>,
    latency: &[Option<f64>],
    derp: &HashMap<String, String>,
    p: &Palette,
) -> Vec<String> {
    let mut rows = vec![tc::title("machine info", w, &p.accent)];
    let dns = text(peer, "DNSName").trim_end_matches('.').to_string();
    let owner = users
        .get(&peer["UserID"].as_i64().unwrap_or(0).to_string())
        .map(|u| text(u, "LoginName"))
        .unwrap_or_default();

    macro_rules! field {
        ($label:expr, $value:expr, $colour:expr) => {{
            let value: String = $value;
            if !value.is_empty() {
                rows.push(tc::seg(
                    &[
                        (p.dim.as_str(), format!(" {:<13}", $label)),
                        ($colour, value),
                    ],
                    w - 1,
                ));
            }
        }};
    }

    field!("machine", peer_name(peer), p.accent.as_str());
    field!("dns name", dns.clone(), p.txt.as_str());
    let hostname = text(peer, "HostName");
    if hostname != peer_name(peer) && !hostname.is_empty() {
        field!(
            "hostname",
            format!("{}   (self-reported)", hostname),
            p.dim.as_str()
        );
    }
    field!("os", text(peer, "OS"), p.txt.as_str());
    let region = text(peer, "Relay");
    if !region.is_empty() {
        field!(
            "region",
            match derp.get(&region) {
                Some(city) => format!("{} — {}", region, city),
                None => region.clone(),
            },
            p.route.as_str()
        );
    }
    field!("owner", owner, p.txt.as_str());
    field!(
        "tags",
        peer["Tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        p.route.as_str()
    );
    rows.push(String::new());

    let up = peer["Online"].as_bool().unwrap_or(false);
    field!(
        "status",
        if up { "online" } else { "offline" }.to_string(),
        if up { p.online.as_str() } else { p.offline.as_str() }
    );
    let cur = text(peer, "CurAddr");
    if !cur.is_empty() {
        field!("path", format!("DIRECT via {}", cur), p.direct.as_str());
    } else {
        let relay = match region.as_str() {
            "" => "?".to_string(),
            s => s.to_string(),
        };
        let city = derp.get(&relay).cloned().unwrap_or_default();
        field!(
            "path",
            format!(
                "relayed through DERP {}{}",
                relay,
                if city.is_empty() { String::new() } else { format!(" ({})", city) }
            ),
            p.relay.as_str()
        );
    }
    field!(
        "rx / tx",
        format!(
            "{}  /  {}",
            human(bytes_at(peer, "RxBytes")).trim(),
            human(bytes_at(peer, "TxBytes")).trim()
        ),
        p.txt.as_str()
    );
    field!("handshake", stamp(&text(peer, "LastHandshake")), p.txt.as_str());
    let last = stamp(&text(peer, "LastSeen"));
    field!(
        "last seen",
        if last.is_empty() {
            if up { "connected".to_string() } else { "-".to_string() }
        } else {
            last
        },
        p.txt.as_str()
    );
    field!("added", stamp(&text(peer, "Created")), p.txt.as_str());
    rows.push(String::new());

    rows.push(tc::seg(&[(p.lbl.as_str(), " addresses".into())], w - 1));
    for ip in peer["TailscaleIPs"].as_array().into_iter().flatten() {
        field!(
            "  tailscale",
            ip.as_str().unwrap_or("").to_string(),
            p.accent.as_str()
        );
    }
    let (mut pub_ips, mut priv_ips, mut other) = (Vec::new(), Vec::new(), Vec::new());
    let cur_host = cur.rsplit_once(':').map(|(h, _)| h).unwrap_or(&cur).to_string();
    if !cur_host.is_empty() {
        pub_ips.push(cur_host);
    }
    for ip in eps.get(&dns).into_iter().flatten() {
        let bucket = match classify(ip) {
            "public" => &mut pub_ips,
            "private" => &mut priv_ips,
            _ => &mut other,
        };
        if !bucket.contains(ip) {
            bucket.push(ip.clone());
        }
    }
    let routes = primary_routes(peer);
    priv_ips.sort_by_key(|i| lan_rank(i, &routes));
    for ip in &pub_ips {
        field!("  public", ip.clone(), p.txt.as_str());
    }
    for ip in &priv_ips {
        let tag = if routes.iter().any(|r| in_network(ip, r)) {
            "  (in advertised subnet)"
        } else {
            ""
        };
        field!("  private", format!("{}{}", ip, tag), p.txt.as_str());
    }
    for ip in &other {
        field!("  other", ip.clone(), p.dim.as_str());
    }
    if eps.is_empty() {
        rows.push(tc::seg(
            &[(
                p.dim.as_str(),
                "   endpoints need sudo; only the current path is shown".into(),
            )],
            w - 1,
        ));
    }

    if !routes.is_empty() {
        rows.push(String::new());
        rows.push(tc::seg(&[(p.lbl.as_str(), " advertises".into())], w - 1));
        for r in routes.iter().take(6) {
            rows.push(tc::seg(&[(p.route.as_str(), format!("   {}", r))], w - 1));
        }
        if routes.len() > 6 {
            rows.push(tc::seg(
                &[(p.dim.as_str(), format!("   +{} more", routes.len() - 6))],
                w - 1,
            ));
        }
    }

    let pings: Vec<f64> = latency.iter().filter_map(|x| *x).collect();
    if !latency.is_empty() {
        rows.push(String::new());
        let loss = 100.0 * (latency.len() - pings.len()) as f64 / latency.len() as f64;
        if !pings.is_empty() {
            let mut ordered = pings.clone();
            ordered.sort_by(f64::total_cmp);
            let jit = if pings.len() > 1 {
                pings.windows(2).map(|x| (x[1] - x[0]).abs()).sum::<f64>()
                    / (pings.len() - 1) as f64
            } else {
                0.0
            };
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " latency  ".into()),
                    (
                        p.dim.as_str(),
                        format!("(icmp over tailscale, {} samples)", latency.len()),
                    ),
                ],
                w - 1,
            ));
            rows.push(tc::seg(
                &[
                    (p.dim.as_str(), "   now ".into()),
                    (p.txt.as_str(), format!("{:>7.2}ms", pings[pings.len() - 1])),
                    (p.dim.as_str(), "  avg ".into()),
                    (
                        p.txt.as_str(),
                        format!("{:>7.2}ms", pings.iter().sum::<f64>() / pings.len() as f64),
                    ),
                    (p.dim.as_str(), "  med ".into()),
                    (p.txt.as_str(), format!("{:>7.2}ms", ordered[ordered.len() / 2])),
                ],
                w - 1,
            ));
            rows.push(tc::seg(
                &[
                    (p.dim.as_str(), "   min ".into()),
                    (p.txt.as_str(), format!("{:>7.2}ms", ordered[0])),
                    (p.dim.as_str(), "  max ".into()),
                    (p.txt.as_str(), format!("{:>7.2}ms", ordered[ordered.len() - 1])),
                    (p.dim.as_str(), "  jit ".into()),
                    (p.txt.as_str(), format!("{:>7.2}ms", jit)),
                ],
                w - 1,
            ));
            rows.push(tc::seg(
                &[
                    (p.dim.as_str(), "   loss".into()),
                    (
                        if loss == 0.0 { p.online.as_str() } else { p.relay.as_str() },
                        format!("{:>7.1}%", loss),
                    ),
                ],
                w - 1,
            ));
            let (lo, hi) = (ordered[0], ordered[ordered.len() - 1]);
            let span = if hi > lo { hi - lo } else { 1.0 };
            let sw = w.saturating_sub(8).max(10);
            let mut marks: Vec<(&str, String)> = vec![(p.dim.as_str(), "   ".into())];
            for v in latency.iter().rev().take(sw).rev() {
                match v {
                    None => marks.push((p.relay.as_str(), "×".into())),
                    Some(v) => marks.push((
                        p.online.as_str(),
                        SPARK[((((v - lo) / span) * 7.99) as usize).min(7)].to_string(),
                    )),
                }
            }
            rows.push(tc::seg(&marks, w - 1));
        } else {
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " latency  ".into()),
                    (p.relay.as_str(), "no replies".into()),
                ],
                w - 1,
            ));
        }
    } else if up {
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " latency  ".into()),
                (p.dim.as_str(), "probing…".into()),
            ],
            w - 1,
        ));
    }

    if let Some(hist) = rates.get(&peer_name(peer)).filter(|h| !h.is_empty()) {
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " throughput  ".into()),
                (
                    p.dim.as_str(),
                    format!("(last {} samples)", hist.len().min(60)),
                ),
            ],
            w - 1,
        ));
        let rx: Vec<f64> = hist.iter().map(|(r, _)| *r).collect();
        let tx: Vec<f64> = hist.iter().map(|(_, t)| *t).collect();
        let peak = rx
            .iter()
            .chain(tx.iter())
            .cloned()
            .fold(0.0f64, f64::max)
            .max(1.0);
        let sw = w.saturating_sub(22).max(10);
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "   down ".into()),
                (p.online.as_str(), spark(&rx, sw, peak)),
                (p.dim.as_str(), format!(" {}", rate(*rx.last().unwrap_or(&0.0)))),
            ],
            w - 1,
        ));
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "   up   ".into()),
                (p.relay.as_str(), spark(&tx, sw, peak)),
                (p.dim.as_str(), format!(" {}", rate(*tx.last().unwrap_or(&0.0)))),
            ],
            w - 1,
        ));
        rows.push(tc::seg(
            &[(p.dim.as_str(), format!("   peak  {}", rate(peak)))],
            w - 1,
        ));
    }

    if peer["ExitNodeOption"].as_bool().unwrap_or(false) {
        rows.push(String::new());
        rows.push(tc::seg(
            &[(p.exit.as_str(), " offers itself as an exit node".into())],
            w - 1,
        ));
    }

    while rows.len() < h.saturating_sub(1) {
        rows.push(String::new());
    }
    rows.push(tc::seg(
        &[(p.dim.as_str(), " [c]opy addresses · ← or esc to close".into())],
        w - 1,
    ));
    rows
}

fn copy_overlay(
    peer: &serde_json::Value,
    eps: &HashMap<String, Vec<String>>,
    w: usize,
    h: usize,
    note: &str,
    p: &Palette,
) -> Vec<String> {
    let pairs = addresses(peer, eps);
    let mut rows = vec![tc::title("copy address", w, &p.accent)];
    rows.push(tc::seg(
        &[
            (p.accent.as_str(), format!(" {}", peer_name(peer))),
            (
                p.dim.as_str(),
                format!("   {}", text(peer, "DNSName").trim_end_matches('.')),
            ),
        ],
        w - 1,
    ));
    rows.push(String::new());
    if pairs.is_empty() {
        rows.push(tc::seg(
            &[(p.dim.as_str(), "  no addresses known for this machine".into())],
            w - 1,
        ));
    }
    for (i, (label, value)) in pairs.iter().enumerate() {
        rows.push(tc::seg(
            &[
                (p.online.as_str(), format!("  [{}] ", i + 1)),
                (p.txt.as_str(), format!("{:<18} ", label)),
                (p.accent.as_str(), value.clone()),
            ],
            w - 1,
        ));
    }
    while rows.len() < h.saturating_sub(2) {
        rows.push(String::new());
    }
    rows.push(tc::seg(
        &[(
            p.dim.as_str(),
            format!(
                " press 1-{} to copy · ← or esc to close",
                pairs.len().max(1)
            ),
        )],
        w - 1,
    ));
    rows.push(if note.is_empty() {
        String::new()
    } else {
        tc::seg(&[(p.online.as_str(), format!(" {}", note))], w - 1)
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_comes_from_magicdns_not_the_device() {
        // Two iPads both call themselves "localhost"; the MagicDNS label is
        // unique across the tailnet and matches the admin console.
        let peer: serde_json::Value =
            serde_json::from_str(r#"{"DNSName": "garden-sensor.example.ts.net.", "HostName": "localhost"}"#)
                .unwrap();
        assert_eq!(peer_name(&peer), "garden-sensor");
        // Only when there is no DNS name does the device get to say.
        let bare: serde_json::Value = serde_json::from_str(r#"{"HostName": "kitchen-pi"}"#).unwrap();
        assert_eq!(peer_name(&bare), "kitchen-pi");
        assert_eq!(peer_name(&serde_json::json!({})), "?");
    }

    #[test]
    fn an_address_is_classified_from_itself() {
        // 100.64/10 is the CGNAT range Tailscale uses, not a public address.
        assert_eq!(classify("100.64.0.1"), "tailscale");
        assert_eq!(classify("100.127.255.254"), "tailscale");
        assert_eq!(classify("100.63.0.1"), "public");
        assert_eq!(classify("100.128.0.1"), "public");
        assert_eq!(classify("fd7a:115c:a1e0::1"), "tailscale");
        assert_eq!(classify("10.0.0.5"), "private");
        assert_eq!(classify("172.17.0.1"), "private");
        assert_eq!(classify("192.168.1.4"), "private");
        assert_eq!(classify("203.0.113.9"), "public");
        assert_eq!(classify("169.254.1.1"), "other");
        assert_eq!(classify("127.0.0.1"), "other");
        // A zone index does not change what an address is.
        assert_eq!(classify("fe80::1%eth0"), "other");
    }

    #[test]
    fn a_cidr_contains_what_it_should() {
        assert!(in_network("192.168.1.50", "192.168.1.0/24"));
        assert!(!in_network("192.168.2.50", "192.168.1.0/24"));
        assert!(in_network("10.4.5.6", "10.0.0.0/8"));
        // A v6 route cannot contain a v4 address, and a bare address is not
        // a network.
        assert!(!in_network("10.4.5.6", "fd7a::/48"));
        assert!(!in_network("10.4.5.6", "10.0.0.0"));
    }

    #[test]
    fn a_real_lan_address_beats_a_docker_bridge() {
        let routes = vec!["192.168.7.0/24".to_string()];
        // Inside an advertised subnet wins outright.
        assert_eq!(lan_rank("192.168.7.20", &routes), 0);
        // Then 192.168, then 10, and docker's 172.17 last.
        assert!(lan_rank("192.168.9.1", &routes) < lan_rank("10.1.2.3", &routes));
        assert!(lan_rank("10.1.2.3", &routes) < lan_rank("172.17.0.1", &routes));
    }

    #[test]
    fn an_exit_node_route_does_not_defeat_the_ranking() {
        // AllowedIPs carries 0.0.0.0/0 for an exit node, which would match
        // every address; PrimaryRoutes is filtered so it cannot.
        let peer: serde_json::Value =
            serde_json::from_str(r#"{"PrimaryRoutes": ["0.0.0.0/0", "::/0", "192.168.7.0/24"]}"#)
                .unwrap();
        assert_eq!(primary_routes(&peer), vec!["192.168.7.0/24"]);
    }

    #[test]
    fn a_byte_count_is_always_five_cells() {
        // Up to petabytes, which is well past anything a WireGuard counter
        // reaches before tailscaled restarts. Beyond that the number itself
        // is wider than the column, and so is the Python's.
        for n in [0u64, 9, 10, 1023, 1024, 15_000, 3_000_000_000, 900_000_000_000_000] {
            assert_eq!(human(n).chars().count(), 5, "{}", n);
        }
        assert_eq!(human(0), "   0B");
        assert_eq!(human(1536), " 1.5K");
        assert_eq!(human(15_360), "  15K");
    }
}
