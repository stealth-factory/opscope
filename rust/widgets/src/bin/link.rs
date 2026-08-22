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

//! How good the connection is between here and whoever is connected to it.
//!
//! A port of link.py. Every other network widget in the collection measures
//! a path it chose; this one measures the path you are on, and it sends
//! nothing to do it - `ss -tin` reports what the kernel has already
//! measured for each established socket.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use toys_core as tc;

const IDLE_AFTER: f64 = 300.0;
const SPARK: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const SERIES: &[char] = &['●', '▲', '■', '◆', '✚', '✦'];

#[derive(Clone, Default)]
struct Session {
    peer: String,
    ip: String,
    port: u16,
    rtt: Option<f64>,
    jitter: Option<f64>,
    floor: Option<f64>,
    sent: f64,
    recv: f64,
    retrans_bytes: f64,
    delivery: Option<f64>,
    cwnd: Option<f64>,
    mss: Option<f64>,
    lastsnd: Option<f64>,
    lastrcv: Option<f64>,
    raw: HashMap<String, String>,
}

fn run(args: &[&str]) -> String {
    match std::process::Command::new(args[0]).args(&args[1..]).output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        _ => String::new(),
    }
}

/// Ports this machine accepts connections on.
///
/// Inbound is defined as "arrived at a port we listen on" rather than by a
/// list of numbers, so SSH, a terminal server and anything else that
/// accepts sessions are all found without being named.
fn listening_ports() -> Vec<u16> {
    let mut ports = Vec::new();
    for line in run(&["ss", "-tlnH"]).lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if let Some(local) = cols.get(3) {
            if let Some((_, port)) = local.rsplit_once(':') {
                if let Ok(p) = port.parse() {
                    ports.push(p);
                }
            }
        }
    }
    ports
}

/// The kernel's own numbers for one socket.
///
/// `ss` mixes two shapes on that line: `key:value` pairs and
/// space-separated ones like `delivery_rate 45107960bps`. Both are read;
/// anything unknown is left alone rather than guessed at.
fn parse_metrics(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let words: Vec<&str> = text.split_whitespace().collect();
    for key in ["send", "pacing_rate", "delivery_rate"] {
        if let Some(at) = words.iter().position(|w| *w == key) {
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

fn num(map: &HashMap<String, String>, key: &str) -> Option<f64> {
    map.get(key).and_then(|v| v.parse().ok())
}

/// One entry per established inbound connection, with its metrics.
fn sessions() -> Vec<Session> {
    let ports = listening_ports();
    if ports.is_empty() {
        return Vec::new();
    }
    let text = run(&["ss", "-tinH", "state", "established"]);
    let mut found = Vec::new();
    let mut head: Option<Vec<String>> = None;
    for line in text.lines() {
        if !line.starts_with('\t') && !line.starts_with(' ') {
            head = Some(line.split_whitespace().map(|s| s.to_string()).collect());
            continue;
        }
        let cols = match &head {
            Some(c) if c.len() >= 4 => c.clone(),
            _ => continue,
        };
        let (local, peer) = (&cols[2], &cols[3]);
        let lport: u16 = match local.rsplit_once(':').and_then(|(_, p)| p.parse().ok()) {
            Some(p) => p,
            None => {
                head = None;
                continue;
            }
        };
        let (peer_host, peer_port) = match peer.rsplit_once(':') {
            Some((h, p)) => (h.trim_matches(|c| c == '[' || c == ']'), p),
            None => {
                head = None;
                continue;
            }
        };
        // ::ffff:10.0.0.1 is an IPv4 address wearing an IPv6 hat - the same
        // machine, the same session - so it is unwrapped before anything
        // else looks at it. Left wrapped, ::ffff:127.0.0.1 walked straight
        // past the loopback filter and put a 22-microsecond local socket on
        // the chart, flattening every real session against the ceiling.
        let peer_ip = peer_host.strip_prefix("::ffff:").unwrap_or(peer_host);
        if !ports.contains(&lport) || peer_ip.starts_with("127.") || peer_ip.starts_with("::1") {
            head = None;
            continue;
        }
        let m = parse_metrics(line);
        let rtt_pair = m.get("rtt").cloned().unwrap_or_default();
        let mut halves = rtt_pair.split('/');
        found.push(Session {
            peer: format!("{}:{}", peer_ip, peer_port),
            ip: peer_ip.to_string(),
            port: lport,
            rtt: halves.next().and_then(|v| v.parse().ok()),
            jitter: halves.next().and_then(|v| v.parse().ok()),
            floor: num(&m, "minrtt"),
            sent: num(&m, "bytes_sent").unwrap_or(0.0),
            recv: num(&m, "bytes_received").unwrap_or(0.0),
            retrans_bytes: num(&m, "bytes_retrans").unwrap_or(0.0),
            delivery: num(&m, "delivery_rate"),
            cwnd: num(&m, "cwnd"),
            mss: num(&m, "mss"),
            lastsnd: num(&m, "lastsnd"),
            lastrcv: num(&m, "lastrcv"),
            raw: m,
        });
        head = None;
    }
    found
}

/// Who is logged in from where, to put a name against an address.
fn who() -> HashMap<String, Vec<String>> {
    let mut seen: HashMap<String, Vec<String>> = HashMap::new();
    for line in run(&["who"]).lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 2 {
            continue;
        }
        let user = cols[0];
        // The address is in parentheses at the end, where there is one.
        if let Some(open) = line.rfind('(') {
            if let Some(close) = line[open..].find(')') {
                let host = &line[open + 1..open + close];
                if !host.is_empty() {
                    let names = seen.entry(host.to_string()).or_default();
                    if !names.iter().any(|n| n == user) {
                        names.push(user.to_string());
                    }
                }
            }
        }
    }
    seen
}

fn rate(n: Option<f64>) -> String {
    let v = match n {
        Some(v) if v > 0.0 => v,
        _ => return "—".into(),
    };
    for (suffix, scale) in [("Gbps", 1e9), ("Mbps", 1e6), ("Kbps", 1e3)] {
        if v >= scale {
            return format!("{:.1}{}", v / scale, suffix);
        }
    }
    format!("{:.0}bps", v)
}

/// Milliseconds on link.py's own scale, which drops to microseconds below
/// one: a loopback socket reads 22us, and 0.02ms hides what that means.
fn ms(value: Option<f64>) -> String {
    match value {
        None => "—".into(),
        Some(v) if v >= 100.0 => format!("{}ms", v.round() as i64),
        Some(v) if v >= 10.0 => format!("{:.0}ms", v),
        Some(v) if v >= 1.0 => format!("{:.1}ms", v),
        Some(v) => format!("{}µs", (v * 1000.0).round() as i64),
    }
}

/// A duration in milliseconds, as a person would say it.
fn span(milliseconds: Option<f64>) -> String {
    let s = match milliseconds {
        Some(v) => v / 1000.0,
        None => return "—".into(),
    };
    if s < 90.0 {
        format!("{}s", s as i64)
    } else if s < 5400.0 {
        format!("{}m", (s / 60.0) as i64)
    } else if s < 172_800.0 {
        format!("{}h", (s / 3600.0) as i64)
    } else {
        format!("{}d", (s / 86400.0) as i64)
    }
}

fn sparkline(values: &[f64], width: usize) -> String {
    if values.is_empty() {
        return String::new();
    }
    let window: Vec<f64> = values.iter().rev().take(width).rev().copied().collect();
    let hi = window.iter().cloned().fold(0.0f64, f64::max).max(1e-9);
    window
        .iter()
        .map(|v| {
            let level = ((v / hi) * (SPARK.len() - 1) as f64).round() as usize;
            SPARK[level.min(SPARK.len() - 1)]
        })
        .collect()
}

/// Fit samples to the columns available, by median.
///
/// A fifteen-minute window at a two-second poll is 450 readings and a pane
/// is eighty columns wide, so something has to give. The median of each
/// bucket is the typical round-trip over that slice.
fn condense(values: &[f64], columns: usize) -> Vec<f64> {
    if values.len() <= columns || columns < 1 {
        return values.to_vec();
    }
    let mut out = Vec::with_capacity(columns);
    for i in 0..columns {
        let from = i * values.len() / columns;
        let to = (i + 1) * values.len() / columns;
        let mut chunk: Vec<f64> = values[from..to].to_vec();
        if chunk.is_empty() {
            continue;
        }
        chunk.sort_by(|a, b| a.partial_cmp(b).unwrap());
        out.push(chunk[chunk.len() / 2]);
    }
    out
}

fn window_label(seconds: f64) -> String {
    if seconds < 60.0 {
        format!("{}s", seconds as i64)
    } else if seconds < 3600.0 {
        format!("{}m", (seconds / 60.0).round() as i64)
    } else {
        format!("{}h", (seconds / 3600.0).round() as i64)
    }
}

struct State {
    rows: Vec<Session>,
    names: HashMap<String, Vec<String>>,
    history: HashMap<String, Vec<f64>>,
    err: String,
}

fn main() {
    tc::maybe_help(include_str!("link_help.txt"));
    let cfg = tc::load_config("link");
    let refresh = tc::cfg_f64(&cfg, "refresh", 2.0).max(0.5);
    let windows: Vec<f64> = {
        let got = cfg
            .get("windows")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_f64()).collect::<Vec<f64>>())
            .unwrap_or_default();
        if got.is_empty() {
            vec![60.0, 300.0, 900.0, 3600.0]
        } else {
            got
        }
    };
    // Retention has to cover the longest span on offer, or w would cycle to
    // a window the samples could never fill.
    let history_len = ((windows.iter().cloned().fold(0.0f64, f64::max) / refresh) as usize + 2)
        .max(tc::cfg_usize(&cfg, "history", 120));

    let absent = tc::missing(&["ss"]);
    if !absent.is_empty() {
        hold(&absent);
        return;
    }

    let p = palette();
    let state = Arc::new(Mutex::new(State {
        rows: Vec::new(),
        names: HashMap::new(),
        history: HashMap::new(),
        err: String::new(),
    }));
    let poller = Arc::clone(&state);
    std::thread::spawn(move || loop {
        let found = sessions();
        let names = who();
        {
            let mut guard = match poller.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            for row in &found {
                if let Some(rtt) = row.rtt {
                    let series = guard.history.entry(row.peer.clone()).or_default();
                    series.push(rtt);
                    if series.len() > history_len {
                        let drop = series.len() - history_len;
                        series.drain(..drop);
                    }
                }
            }
            guard.rows = found;
            guard.names = names;
        }
        std::thread::sleep(Duration::from_secs_f64(refresh));
    });

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let (mut selected, mut hide_idle, mut span_at) = (0usize, false, 0usize);

    loop {
        for key in keyboard.poll() {
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "up" | "k" | "K" => selected = selected.saturating_sub(1),
                "down" | "j" | "J" => selected += 1,
                "o" | "O" => hide_idle = !hide_idle,
                "w" | "W" => span_at = (span_at + 1) % windows.len(),
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let guard = match state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let shown: Vec<Session> = guard
            .rows
            .iter()
            .filter(|r| !(hide_idle && r.lastrcv.unwrap_or(0.0) > IDLE_AFTER * 1000.0))
            .cloned()
            .collect();
        if !shown.is_empty() && selected >= shown.len() {
            selected = shown.len() - 1;
        }
        let window = windows[span_at];

        let mut rows = vec![tc::title("connections", w, &p.link)];
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!(" {} inbound", guard.rows.len())),
                (
                    p.dim.as_str(),
                    " · measured by the kernel, nothing sent".into(),
                ),
                (p.dim.as_str(), format!("   every {}s", refresh)),
            ],
            w - 1,
        ));
        if !guard.err.is_empty() {
            rows.push(tc::seg(&[(p.bad.as_str(), format!(" ! {}", guard.err))], w - 1));
        }
        rows.push(String::new());

        if guard.rows.is_empty() {
            rows.push(tc::seg(
                &[(
                    p.dim.as_str(),
                    "  No inbound sessions on a listening port.".into(),
                )],
                w - 1,
            ));
            rows.push(tc::seg(
                &[(
                    p.dim.as_str(),
                    "  Nothing is connected to this machine, or ss cannot see it.".into(),
                )],
                w - 1,
            ));
        } else {
            rows.extend(table(&shown, &guard, w, selected, &p));
            rows.push(String::new());
            let room = h.saturating_sub(rows.len() + 4);
            if room >= 5 {
                rows.extend(graph(&shown, &guard.history, w, room, window, refresh, &p));
                rows.push(tc::seg(
                    &[
                        (p.dim.as_str(), " ".repeat(7)),
                        (p.grid.as_str(), format!("└{}", "─".repeat(w.saturating_sub(9).max(10)))),
                    ],
                    w - 1,
                ));
                let covered = plotted_span(&shown, &guard.history, window, refresh, w);
                rows.push(tc::seg(
                    &[
                        (p.dim.as_str(), format!("        {} ago", window_label(covered))),
                        (p.dim.as_str(), " ".repeat(w.saturating_sub(26).max(1))),
                        (p.dim.as_str(), "now".into()),
                    ],
                    w - 1,
                ));
            }
        }

        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
            vec![
                (p.accent.as_str(), "[w]".into()),
                (p.dim.as_str(), format!(" {}", window_label(window))),
            ],
            vec![(
                p.dim.as_str(),
                format!("[o]{} idle", if hide_idle { "show" } else { "hide" }),
            )],
            vec![(p.dim.as_str(), "[q]uit".into())],
        ];
        drop(guard);
        let foot: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
        while rows.len() < h.saturating_sub(foot.len()) {
            rows.push(String::new());
        }
        rows.extend(foot);
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn plotted_span(
    rows: &[Session],
    history: &HashMap<String, Vec<f64>>,
    window: f64,
    refresh: f64,
    w: usize,
) -> f64 {
    let _ = w;
    let longest = rows
        .iter()
        .filter_map(|r| history.get(&r.peer).map(|h| h.len()))
        .max()
        .unwrap_or(0);
    let capped = longest.min((window / refresh).round() as usize);
    capped as f64 * refresh
}

fn table(rows: &[Session], state: &State, w: usize, selected: usize, p: &Palette) -> Vec<String> {
    // The Python's header, column for column: the two have to sit side by
    // side in a wall and read as the same widget.
    let wide = w >= 74;
    let name_w = 20usize;
    let mut out = vec![tc::seg(
        &[
            (p.dim.as_str(), "  PEER".into()),
            (p.dim.as_str(), " ".repeat(14)),
            (p.dim.as_str(), "    NOW   FLOOR  JITTER    LOSS".into()),
            (p.dim.as_str(), if wide { "  ACHIEVED".into() } else { String::new() }),
            (p.dim.as_str(), if wide { "   IDLE".into() } else { String::new() }),
        ],
        w - 1,
    )];
    for (i, row) in rows.iter().enumerate() {
        let here = i == selected;
        let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
        let hue = &p.hues[i % p.hues.len()];
        let glyph = SERIES[i % SERIES.len()];
        let who = state
            .names
            .get(&row.ip)
            .map(|names| names.join(","))
            .unwrap_or_default();
        let label = if who.is_empty() {
            row.ip.clone()
        } else {
            format!("{} {}", row.ip, who)
        };
        let loss = if row.sent > 0.0 {
            100.0 * row.retrans_bytes / row.sent
        } else {
            0.0
        };
        let name_c = format!("{}{}", tint, hue);
        let txt_c = format!("{}{}", tint, p.txt);
        let dim_c = format!("{}{}", tint, p.dim);
        let loss_c = format!("{}{}", tint, if loss > 0.5 { &p.bad } else { &p.dim });
        let idle = [row.lastsnd, row.lastrcv]
            .into_iter()
            .flatten()
            .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| a.min(v))));
        let mut line = vec![
            (name_c.as_str(), format!(" {} ", glyph)),
            (txt_c.as_str(), tc::pad(&label, name_w)),
            (txt_c.as_str(), format!("{:>7}", ms(row.rtt))),
            (dim_c.as_str(), format!("{:>8}", ms(row.floor))),
            (dim_c.as_str(), format!("{:>8}", ms(row.jitter))),
            (loss_c.as_str(), format!("{:>7.2}%", loss)),
        ];
        if wide {
            line.push((dim_c.as_str(), format!("{:>10}", rate(row.delivery))));
            line.push((dim_c.as_str(), format!("{:>7}", span(idle))));
        }
        if here {
            line.push((tint.as_str(), " ".repeat(w)));
        }
        out.push(tc::seg(&line, w - 1));
    }
    out
}

/// Log-scale multi-series plot of round-trip time.
fn graph(
    rows: &[Session],
    history: &HashMap<String, Vec<f64>>,
    w: usize,
    h: usize,
    window: f64,
    refresh: f64,
    p: &Palette,
) -> Vec<String> {
    let gw = w.saturating_sub(9).max(10);
    let gh = h.max(4);
    let want = ((window / refresh).round() as usize).max(1);
    let series: Vec<(usize, Vec<f64>)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, row)| {
            let all = history.get(&row.peer)?;
            let start = all.len().saturating_sub(want);
            let vals = condense(&all[start..], gw);
            if vals.is_empty() {
                None
            } else {
                Some((i, vals))
            }
        })
        .collect();
    if series.is_empty() {
        return vec![tc::seg(&[(p.dim.as_str(), "  collecting…".into())], w - 1)];
    }
    let lo = series
        .iter()
        .flat_map(|(_, v)| v.iter())
        .cloned()
        .fold(f64::INFINITY, f64::min)
        .max(0.05)
        * 0.8;
    let hi = (series
        .iter()
        .flat_map(|(_, v)| v.iter())
        .cloned()
        .fold(0.0f64, f64::max)
        * 1.25)
        .max(lo * 1.6);
    let (llo, lhi) = (lo.log10(), hi.log10());

    let mut grid = vec![vec![(p.grid.clone(), ' '); gw]; gh];
    for (idx, values) in &series {
        let glyph = SERIES[idx % SERIES.len()];
        let colour = &p.hues[idx % p.hues.len()];
        let start = gw - values.len();
        let mut previous: Option<usize> = None;
        for (x, value) in values.iter().enumerate() {
            let frac = (value.max(1e-3).log10() - llo) / (lhi - llo);
            let y = ((1.0 - frac) * (gh as f64 - 1.0)).round().clamp(0.0, gh as f64 - 1.0) as usize;
            let col = start + x;
            if let Some(prev) = previous {
                if prev.abs_diff(y) > 1 {
                    for fill in prev.min(y) + 1..prev.max(y) {
                        if grid[fill][col].1 == ' ' {
                            grid[fill][col] = (colour.clone(), '│');
                        }
                    }
                }
            }
            grid[y][col] = (colour.clone(), glyph);
            previous = Some(y);
        }
    }

    let mut out = Vec::new();
    for (y, line) in grid.iter().enumerate() {
        let frac = 1.0 - (y as f64 / (gh as f64 - 1.0).max(1.0));
        let value = 10f64.powf(llo + frac * (lhi - llo));
        let label = if y == 0 || y == gh / 2 || y == gh - 1 {
            format!("{:>7}", ms(Some(value)))
        } else {
            " ".repeat(7)
        };
        let mut parts: Vec<(&str, String)> =
            vec![(p.dim.as_str(), label), (p.grid.as_str(), "│".into())];
        for (colour, ch) in line {
            parts.push((colour.as_str(), ch.to_string()));
        }
        out.push(tc::seg(&parts, w - 1));
    }
    out
}

/// Draw the reason and wait, rather than exiting.
fn hold(needed: &[String]) {
    let bad = tc::rgb(255, 100, 110);
    let dim = tc::rgb(127, 147, 172);
    let txt = tc::rgb(225, 235, 245);
    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    loop {
        for key in keyboard.poll() {
            if key == "q" || key == "Q" {
                keyboard.restore();
                tc::restore_screen();
                return;
            }
        }
        let (w, h) = tc::size();
        let mut rows = vec![tc::title("connections", w, &bad), String::new()];
        rows.push(tc::seg(
            &[
                (bad.as_str(), " cannot start · ".into()),
                (txt.as_str(), format!("needs {}", needed.join(", "))),
            ],
            w - 1,
        ));
        rows.push(String::new());
        for line in [
            "ss reads the kernel's own per-socket metrics, which is where",
            "every figure here comes from: round-trip time, retransmits,",
            "delivery rate. Nothing else on the machine reports them.",
        ] {
            rows.push(tc::seg(&[(dim.as_str(), format!(" {}", line))], w - 1));
        }
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (dim.as_str(), " try: ".into()),
                (txt.as_str(), "apt install iproute2".into()),
            ],
            w - 1,
        ));
        while rows.len() < h - 1 {
            rows.push(String::new());
        }
        rows.push(tc::seg(&[(dim.as_str(), " [q]uit".into())], w - 1));
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(200));
    }
}

struct Palette {
    bad: String,
    dim: String,
    grid: String,
    txt: String,
    accent: String,
    link: String,
    hues: Vec<String>,
}

fn palette() -> Palette {
    Palette {
        bad: tc::rgb(255, 100, 110),
        dim: tc::rgb(127, 147, 172),
        grid: tc::rgb(60, 78, 98),
        txt: tc::rgb(225, 235, 245),
        accent: tc::rgb(150, 210, 255),
        link: tc::rgb(140, 200, 255),
        hues: vec![
            tc::rgb(120, 200, 255),
            tc::rgb(150, 230, 180),
            tc::rgb(220, 170, 255),
            tc::rgb(160, 190, 240),
            tc::rgb(200, 220, 150),
            tc::rgb(240, 180, 210),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_shapes_on_the_ss_line_are_read() {
        let line = "\t ts sack cubic rtt:3.604/1.027 minrtt:3.553 cwnd:10 \
                    bytes_sent:1669 delivery_rate 6287464bps";
        let m = parse_metrics(line);
        assert_eq!(m.get("rtt").map(String::as_str), Some("3.604/1.027"));
        assert_eq!(m.get("minrtt").map(String::as_str), Some("3.553"));
        // The space-separated shape, which a key:value scan alone misses.
        assert_eq!(m.get("delivery_rate").map(String::as_str), Some("6287464"));
        assert_eq!(num(&m, "cwnd"), Some(10.0));
    }

    #[test]
    fn milliseconds_follow_the_python_scale() {
        assert_eq!(ms(Some(123.4)), "123ms");
        assert_eq!(ms(Some(28.1)), "28ms");
        assert_eq!(ms(Some(2.74)), "2.7ms");
        // Below a millisecond it changes unit rather than losing the value.
        assert_eq!(ms(Some(0.022)), "22µs");
        assert_eq!(ms(None), "—");
    }

    #[test]
    fn rates_read_as_a_person_would_say_them() {
        assert_eq!(rate(Some(6_287_464.0)), "6.3Mbps");
        assert_eq!(rate(Some(1_500.0)), "1.5Kbps");
        assert_eq!(rate(Some(45_000_000_000.0)), "45.0Gbps");
        assert_eq!(rate(None), "—");
        assert_eq!(rate(Some(0.0)), "—");
    }

    #[test]
    fn spans_come_from_milliseconds() {
        assert_eq!(span(Some(45_000.0)), "45s");
        assert_eq!(span(Some(600_000.0)), "10m");
        assert_eq!(span(None), "—");
    }

    #[test]
    fn condensing_keeps_the_typical_not_the_extreme() {
        // Ten samples into two columns: each column is its half's median,
        // so a single spike cannot define a column on its own.
        let values = vec![10.0, 10.0, 10.0, 10.0, 400.0, 10.0, 10.0, 10.0, 10.0, 10.0];
        let got = condense(&values, 2);
        assert_eq!(got, vec![10.0, 10.0]);
        // Fewer samples than columns is left alone.
        assert_eq!(condense(&[1.0, 2.0], 8), vec![1.0, 2.0]);
    }

    #[test]
    fn a_sparkline_scales_to_its_own_peak() {
        let line = sparkline(&[0.0, 5.0, 10.0], 3);
        let chars: Vec<char> = line.chars().collect();
        assert_eq!(chars.len(), 3);
        assert_eq!(chars[0], '▁');
        assert_eq!(chars[2], '█');
    }

    #[test]
    fn window_labels_are_short() {
        assert_eq!(window_label(60.0), "1m");
        assert_eq!(window_label(900.0), "15m");
        assert_eq!(window_label(3600.0), "1h");
        assert_eq!(window_label(45.0), "45s");
    }
}
