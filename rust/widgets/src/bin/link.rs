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
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use toys_core as tc;

const IDLE_AFTER: f64 = 300.0;

/// The fewest rows the detail chart is worth drawing in.
///
/// It used to take whatever the fields left over and be dropped silently
/// when that came to less than five, so a short pane showed the numbers and
/// no chart and said nothing about why - indistinguishable from a session
/// with no history. It now takes its rows regardless and the screen scrolls.
const MIN_CHART: usize = 12;

/// How the detail screen reports where you are in it, at a width that does
/// not change with the numbers - the footer is measured before the body is
/// built, so an indicator that grew by a character could change how many
/// lines the hints wrap onto and leave the body sized for the wrong one.
fn scroll_label(first: usize, last: usize, total: usize) -> String {
    format!("rows {:>3}-{:>3} of {:>3}", first, last, total)
}
const SPARK: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// The hues sessions are drawn in, kept as numbers so a faded set can be
/// mixed from the same six.
const HUES: &[(u8, u8, u8)] = &[
    (120, 200, 255),
    (150, 230, 180),
    (220, 170, 255),
    (160, 190, 240),
    (200, 220, 150),
    (240, 180, 210),
];

/// The dark these widgets are drawn against - not the terminal's real
/// background, which cannot be asked for, but the one the palette was chosen
/// for, and what a trace is mixed toward when it is not the one selected.
const BACKDROP: (u8, u8, u8) = (16, 22, 30);

/// How far an unlooked-at trace is mixed toward the backdrop. Measured
/// against the two things it sits between: at 0.60 a faded trace stays
/// visibly ink rather than sinking into the axis furniture, and is far
/// enough from its own full-strength hue to read as a different weight.
const FADE: f64 = 0.60;

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
    /// Retransmits since the previous poll, as a percentage of what was
    /// sent in that gap. Filled in by the poller, which is the only place
    /// that has the previous reading to subtract.
    recent_loss: Option<f64>,
    delivery: Option<f64>,
    cwnd: Option<f64>,
    mss: Option<f64>,
    lastsnd: Option<f64>,
    lastrcv: Option<f64>,
    raw: HashMap<String, String>,
}

/// A command's output, or why it could not be had.
///
/// `run` folds every failure into an empty string, which is right for the
/// callers that read absence as "nothing to show". It is wrong for the two
/// that feed the whole widget: from an empty string, `ss` failing and `ss`
/// reporting no sockets are the same thing, and one of them is a quiet
/// machine while the other is a broken pane imitating one.
fn run_or(args: &[&str]) -> Result<String, String> {
    match std::process::Command::new(args[0]).args(&args[1..]).output() {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).to_string()),
        Ok(out) => Err(format!("{} exited {}", args[0], out.status)),
        Err(e) => Err(format!("{} did not run: {}", args[0], e)),
    }
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
fn listening_ports() -> Result<Vec<u16>, String> {
    let mut ports = Vec::new();
    for line in run_or(&["ss", "-tlnH"])?.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if let Some(local) = cols.get(3) {
            if let Some((_, port)) = local.rsplit_once(':') {
                if let Ok(p) = port.parse() {
                    ports.push(p);
                }
            }
        }
    }
    Ok(ports)
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
fn sessions() -> Result<Vec<Session>, String> {
    let ports = listening_ports()?;
    if ports.is_empty() {
        return Ok(Vec::new());
    }
    let text = run_or(&["ss", "-tinH", "state", "established"])?;
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
            recent_loss: None,
            delivery: num(&m, "delivery_rate"),
            cwnd: num(&m, "cwnd"),
            mss: num(&m, "mss"),
            lastsnd: num(&m, "lastsnd"),
            lastrcv: num(&m, "lastrcv"),
            raw: m,
        });
        head = None;
    }
    Ok(found)
}

/// Who is logged in from where, to put a name against an address.
///
/// Login and tty, not just the login: two SSH sessions from one laptop share
/// an address, and the detail view names the ttys to say so. Repeats are
/// kept for the same reason - deduplicating by user threw away the second
/// session, which is the fact that line exists to report.
fn who() -> HashMap<String, Vec<(String, String)>> {
    let mut seen: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for line in run(&["who"]).lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 2 {
            continue;
        }
        // The address is the last field, in parentheses, where there is one.
        let last = cols[cols.len() - 1];
        if !last.starts_with('(') {
            continue;
        }
        let host = last.trim_matches(|c| c == '(' || c == ')');
        if !host.is_empty() {
            seen.entry(host.to_string())
                .or_default()
                .push((cols[0].to_string(), cols[1].to_string()));
        }
    }
    seen
}

fn rate(n: Option<f64>) -> String {
    let v = match n {
        Some(v) => v,
        None => return "--".into(),
    };
    for (suffix, scale) in [("Gbps", 1e9), ("Mbps", 1e6), ("kbps", 1e3)] {
        if v >= scale {
            return format!("{:.1}{}", v / scale, suffix);
        }
    }
    format!("{}bps", v as i64)
}

/// A byte count as a person would say it.
fn size_of(n: f64) -> String {
    for (unit, step) in [("G", 1e9), ("M", 1e6), ("k", 1e3)] {
        if n >= step {
            return format!("{:.1}{}", n / step, unit);
        }
    }
    format!("{}B", n as i64)
}

/// Milliseconds on link.py's own scale, which drops to microseconds below
/// one: a loopback socket reads 22us, and 0.02ms hides what that means.
fn ms(value: Option<f64>) -> String {
    match value {
        None => "--".into(),
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
        None => return "--".into(),
    };
    if s < 60.0 {
        format!("{}s", s as i64)
    } else if s < 3600.0 {
        format!("{}m", (s / 60.0) as i64)
    } else if s < 86400.0 {
        format!("{}h", (s / 3600.0) as i64)
    } else {
        format!("{}d", (s / 86400.0) as i64)
    }
}

/// How much worse than this path's best the connection is right now.
///
/// Compared against the socket's own minrtt rather than a fixed threshold:
/// forty milliseconds is excellent from Hong Kong and poor from the next
/// rack, and the kernel already knows which this is.
fn quality(row: &Session) -> Option<f64> {
    match (row.rtt, row.floor) {
        (Some(rtt), Some(floor)) if rtt != 0.0 && floor != 0.0 => Some(rtt / floor),
        _ => None,
    }
}

fn colour_for<'a>(ratio: Option<f64>, loss: Option<f64>, p: &'a Palette) -> &'a str {
    if loss.is_some_and(|l| l >= 2.0) {
        return &p.bad;
    }
    let ratio = match ratio {
        Some(r) => r,
        None => return &p.dim,
    };
    if ratio >= 3.0 || loss.unwrap_or(0.0) >= 0.5 {
        &p.bad
    } else if ratio >= 1.6 {
        &p.warn
    } else {
        &p.ok
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
    names: HashMap<String, Vec<(String, String)>>,
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
    // [r] asks for a reading now rather than at the end of the interval, so
    // the poller sleeps on a condition it can be woken out of.
    let wake = Arc::new((Mutex::new(false), Condvar::new()));
    let poller = Arc::clone(&state);
    let poller_wake = Arc::clone(&wake);
    std::thread::spawn(move || {
        // A thread that ends leaves the last frame on screen for ever, and a
        // widget frozen on real numbers is harder to catch than one showing
        // none. Every way out of this loop writes down why first - which is
        // what link.py's `run` does around `poll`, and what this port had
        // lost: `State.err` was drawn but never assigned, so the line that
        // explains a dead poller could not fire.
        let stopped = |why: &str| {
            if let Ok(mut guard) = poller.lock() {
                guard.err = format!("poller stopped: {}", why);
            }
        };
        let mut last: HashMap<String, (f64, f64)> = HashMap::new();
        loop {
        let mut found = match sessions() {
            Ok(rows) => {
                if let Ok(mut guard) = poller.lock() {
                    guard.err = String::new();
                }
                rows
            }
            // Not fatal: ss can fail for a moment. Reported and retried
            // rather than ending the thread, but never silently - an empty
            // list and a failed read look identical on screen otherwise.
            Err(why) => {
                if let Ok(mut guard) = poller.lock() {
                    guard.err = why;
                }
                Vec::new()
            }
        };
        let names = who();
        for row in &mut found {
            // Retransmits since the last look, rather than since the
            // connection opened: a session hours old has long since
            // forgiven whatever went wrong at breakfast.
            if let Some((sent, retrans)) = last.get(&row.peer) {
                let moved = row.sent - sent;
                row.recent_loss = Some(if moved > 0.0 {
                    100.0 * (row.retrans_bytes - retrans) / moved
                } else {
                    0.0
                });
            }
            last.insert(row.peer.clone(), (row.sent, row.retrans_bytes));
        }
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
        let (lock, cond) = &*poller_wake;
        let mut asked = match lock.lock() {
            Ok(g) => g,
            Err(_) => return stopped("the wake lock was poisoned"),
        };
        if !*asked {
            asked = match cond.wait_timeout(asked, Duration::from_secs_f64(refresh)) {
                Ok((g, _)) => g,
                Err(_) => return stopped("the wake lock was poisoned while waiting"),
            };
        }
        *asked = false;
        }
    });

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    // Nothing selected until asked for, and no selection is the position
    // above the first row and below the last, so walking off either end
    // lands there and walking off it again comes in at the other. The chart
    // then opens showing every session at equal weight, and focus is
    // something you leave the way you entered it.
    let (mut selected, mut hide_idle, mut span_at) =
        (None::<usize>, false, 0usize);
    let mut count = 0usize;
    let mut detail = false;
    // How far down the detail screen we are. Clamped against the body every
    // frame rather than when the key is pressed, because the body's length
    // depends on the pane, which can change under us between frames.
    let mut scroll = 0usize;

    loop {
        // Read before the keys rather than after them, so a page key knows
        // how big a page is on this pane.
        let (w, h) = tc::size();
        let page = h.saturating_sub(4).max(1);
        for key in keyboard.poll() {
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                // Up and down mean "move through what is in front of you"
                // in both views: in the list that is the selection, on the
                // detail screen it is the screen itself.
                "up" | "k" | "K" => {
                    if detail {
                        scroll = scroll.saturating_sub(1);
                    } else {
                        selected = match selected {
                            None => count.checked_sub(1),
                            Some(0) => None,
                            Some(at) => Some(at - 1),
                        };
                    }
                }
                "down" | "j" | "J" => {
                    if detail {
                        scroll = scroll.saturating_add(1);
                    } else {
                        selected = match selected {
                            None if count > 0 => Some(0),
                            None => None,
                            Some(at) if at + 1 >= count => None,
                            Some(at) => Some(at + 1),
                        };
                    }
                }
                // Right goes in and left comes back out, the way a column
                // of panes works, so the hand does not have to learn a key
                // for it. Enter and esc still do the same two things.
                "right" | "enter" if !detail => {
                    // Opening with nothing selected takes the first row
                    // rather than doing nothing, which would be a key that
                    // the footer offers and that does not answer.
                    if selected.is_none() && count > 0 {
                        selected = Some(0);
                    }
                    detail = selected.is_some();
                    scroll = 0;
                }
                "left" | "esc" if detail => {
                    detail = false;
                    scroll = 0;
                }
                // Stepping between connections without going back to the
                // list. This was on left and right until those were given
                // their directional meaning; it is worth keeping, because
                // comparing two sockets is most of what the detail screen
                // is for.
                // Stays within the list rather than falling off it: the
                // detail screen has to be showing something.
                "n" | "N" if detail => {
                    selected = selected.map(|at| (at + 1).min(count.saturating_sub(1)));
                    scroll = 0;
                }
                "p" | "P" if detail => {
                    selected = selected.map(|at| at.saturating_sub(1));
                    scroll = 0;
                }
                "pgup" if detail => scroll = scroll.saturating_sub(page),
                "pgdn" if detail => scroll = scroll.saturating_add(page),
                "home" if detail => scroll = 0,
                // Clamped to the end of the body when the frame is drawn.
                "end" if detail => scroll = usize::MAX,
                "o" | "O" => hide_idle = !hide_idle,
                "w" | "W" => span_at = (span_at + 1) % windows.len(),
                "r" | "R" => {
                    let (lock, cond) = &*wake;
                    if let Ok(mut asked) = lock.lock() {
                        *asked = true;
                        cond.notify_all();
                    }
                }
                _ => {}
            }
        }

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
        count = shown.len();
        if let Some(at) = selected {
            selected = if shown.is_empty() {
                None
            } else {
                Some(at.min(shown.len() - 1))
            };
        }
        if selected.is_none() {
            detail = false;
        }
        let window = windows[span_at];

        // One connection in full, on its own screen. The list is for
        // noticing; this is for looking into, and the two want different
        // amounts of room for the same chart.
        if let (true, Some(pick)) = (detail && !shown.is_empty(), selected) {
            // The footer is measured before the body is built, and the body
            // is told the height it actually has. Sizing the chart to the
            // whole pane and appending the hints afterwards pushed them off
            // the bottom of it - the keys out of this screen were the rows
            // being lost.
            // Built twice: once to learn how many lines the hints wrap
            // onto, and again with the real position once the body exists.
            // `scroll_label` is a fixed width, so the second pass cannot
            // wrap differently from the first and leave the body sized
            // against a footer that is no longer there.
            let detail_hints = |place: String| -> Vec<Vec<(&str, String)>> {
                vec![
                    vec![
                        (p.accent.as_str(), "←".into()),
                        (p.dim.as_str(), "/[esc] back".into()),
                    ],
                    vec![
                        (p.accent.as_str(), "↑↓".into()),
                        (p.dim.as_str(), " scroll".into()),
                    ],
                    vec![(p.dim.as_str(), "[n]ext [p]rev".into())],
                    vec![
                        (p.accent.as_str(), "[w]".into()),
                        (p.dim.as_str(), format!(" {}", window_label(window))),
                    ],
                    vec![(p.dim.as_str(), "[r]efresh".into())],
                    vec![(p.dim.as_str(), "[q]uit".into())],
                    vec![(p.dim.as_str(), place)],
                ]
            };
            let pack = |hints: &[Vec<(&str, String)>]| -> Vec<String> {
                tc::pack_hints(hints, w - 2, "  ")
                    .into_iter()
                    .map(|l| format!(" {}", l))
                    .collect()
            };
            let foot = pack(&detail_hints(scroll_label(0, 0, 0)));
            let room = h.saturating_sub(foot.len() + 1).max(1);
            let body = detail_view(&shown[pick], &guard, w, room, pick, window, refresh, &p);
            drop(guard);
            // The body is as tall as it needs to be and the pane shows a
            // window onto it, rather than the body being cut to the pane
            // and the remainder going unmentioned.
            let furthest = body.len().saturating_sub(room);
            scroll = scroll.min(furthest);
            let last = (scroll + room).min(body.len());
            let mut shown_body: Vec<String> = body[scroll..last].to_vec();
            while shown_body.len() < room {
                shown_body.push(String::new());
            }
            shown_body.extend(pack(&detail_hints(scroll_label(
                scroll + 1,
                last,
                body.len(),
            ))));
            tc::draw(&shown_body, w, h);
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }

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
            if room < 5 {
                // Say so rather than leaving a gap: a chart that is missing
                // for want of rows looks exactly like one missing for want
                // of data, and only one of those is the reader's to fix.
                if room >= 1 {
                    rows.push(tc::seg(
                        &[(
                            p.dim.as_str(),
                            format!("  chart needs {} more rows", 5 - room),
                        )],
                        w - 1,
                    ));
                }
            } else {
                rows.extend(graph(
                    &shown,
                    &guard.history,
                    w,
                    room,
                    0,
                    selected,
                    window,
                    refresh,
                    &p,
                ));
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
                        (p.dim.as_str(), format!("        {} ago", span(Some(covered * 1000.0)))),
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
                (p.accent.as_str(), "→".into()),
                (p.dim.as_str(), "/[↵] open".into()),
            ],
            vec![
                (p.accent.as_str(), "[w]".into()),
                (p.dim.as_str(), format!(" {}", window_label(window))),
            ],
            vec![(
                p.dim.as_str(),
                format!("[o]{} idle", if hide_idle { "show" } else { "hide" }),
            )],
            vec![(p.dim.as_str(), "[r]efresh".into())],
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

fn table(
    rows: &[Session],
    state: &State,
    w: usize,
    selected: Option<usize>,
    p: &Palette,
) -> Vec<String> {
    // The Python's header, column for column: the two have to sit side by
    // side in a wall and read as the same widget.
    let wide = w >= 74;
    // Two for the glyph and its space, eighteen for the name: together they
    // land the NOW column under its heading. Three and twenty also add up to
    // a plausible-looking row, and put every number three cells right of the
    // word above it.
    // Wide enough for the peer's own port, because that is the only field
    // that differs between sockets from one machine to one service: four
    // browser tabs against the same dev server share an address and a local
    // port and are told apart by nothing else. 21 fits the longest IPv4
    // address and port; the wider pane spends five more on the login.
    let name_w = if wide { 26usize } else { 21usize };
    let mut out = vec![tc::seg(
        &[
            (p.dim.as_str(), "  PEER".into()),
            (p.dim.as_str(), " ".repeat(name_w - 4)),
            (p.dim.as_str(), "    NOW   FLOOR  JITTER    LOSS".into()),
            (p.dim.as_str(), if wide { "  ACHIEVED".into() } else { String::new() }),
            (p.dim.as_str(), if wide { "   IDLE".into() } else { String::new() }),
        ],
        w - 1,
    )];
    for (i, row) in rows.iter().enumerate() {
        let here = selected == Some(i);
        let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
        let hue = &p.hues[i % p.hues.len()];

        // One login, and only where there is room for it: the address is
        // what identifies the session, the name is a courtesy.
        let who = state
            .names
            .get(&row.ip)
            .and_then(|names| names.first())
            .map(|(user, _tty)| user.clone())
            .unwrap_or_default();
        let label = if who.is_empty() || !wide {
            row.peer.clone()
        } else {
            format!("{} {}", row.peer, who)
        };
        let loss = row.recent_loss;
        let tone = format!("{}{}", tint, colour_for(quality(row), loss, p));
        let name_c = format!("{}{}", tint, hue);
        let label_c = format!("{}{}", tint, if here { &p.txt } else { &p.dim });
        let dim_c = format!("{}{}", tint, p.dim);
        let loss_c = format!(
            "{}{}",
            tint,
            if loss.unwrap_or(0.0) >= 0.5 { &p.bad } else { &p.dim }
        );
        let idle = [row.lastsnd, row.lastrcv]
            .into_iter()
            .flatten()
            .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| a.min(v))));
        // A missing reading is a dash of the column's own width, not a
        // narrower cell - anything else shifts every number to its right.
        let cell = |value: Option<f64>, width: usize| match value {
            Some(v) if v != 0.0 => format!("{:>width$}", ms(Some(v)), width = width),
            _ => format!("{:>width$}", "--", width = width),
        };
        let mut line = vec![
            // A colour chip rather than a shape. Six shapes told six
            // sessions apart no better than six hues did, and a seventh
            // session repeated the first one's shape - so the chip carries
            // the hue and the name carries the selection.
            (name_c.as_str(), "▐".to_string()),
            (label_c.as_str(), " ".to_string()),
            (label_c.as_str(), tc::pad(&label, name_w)),
            (tone.as_str(), cell(row.rtt, 7)),
            (dim_c.as_str(), cell(row.floor, 8)),
            (dim_c.as_str(), cell(row.jitter, 8)),
            (
                loss_c.as_str(),
                format!(
                    "{:>7}",
                    loss.map_or("--".to_string(), |v| format!("{:.2}%", v))
                ),
            ),
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
/// One connection, in full.
///
/// The list answers "is anything wrong"; this answers "with what, and how
/// badly". Everything here is a number the kernel already keeps for this
/// socket - nothing is derived beyond the two percentages, and both say
/// what they are measured over.
#[allow(clippy::too_many_arguments)]
fn detail_view(
    row: &Session,
    state: &State,
    w: usize,
    h: usize,
    idx: usize,
    window: f64,
    refresh: f64,
    p: &Palette,
) -> Vec<String> {
    let empty = Vec::new();
    let users = state.names.get(&row.ip).unwrap_or(&empty);
    let mut rows = vec![tc::title("connection", w, &p.link)];
    rows.push(tc::seg(
        &[
            (p.hues[idx % p.hues.len()].as_str(), " ▐ ".to_string()),
            (p.txt.as_str(), row.peer.clone()),
            // Their port identifies the socket; ours identifies the service
            // it reached. Both, because the list is keyed on the first and
            // the question "what is this connected to" is answered by the
            // second.
            (p.dim.as_str(), format!("  · to port {}", row.port)),
            (
                p.dim.as_str(),
                users.first().map_or(String::new(), |(u, _)| format!("  {}", u)),
            ),
        ],
        w - 1,
    ));
    // `who` maps logins to an address, not to a socket, and two SSH sessions
    // from one laptop share the address. Naming both against each socket
    // read as "this connection is pts/0 and pts/35", which it is not - so
    // the ttys are labelled as what they are: the logins from that address.
    if !users.is_empty() {
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "  logins from this address: ".into()),
                (
                    p.txt.as_str(),
                    users
                        .iter()
                        .map(|(_u, tty)| tty.clone())
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            ],
            w - 1,
        ));
    }
    rows.push(String::new());

    // A field with nothing behind it is left out rather than shown empty:
    // the screen is a list of what the kernel knows, so a missing line says
    // "not reported for this socket".
    macro_rules! field {
        ($label:expr, $value:expr, $colour:expr, $note:expr $(,)?) => {{
            if let Some(v) = $value {
                if !v.is_empty() && v != "--" {
                    let note: &str = $note;
                    rows.push(tc::seg(
                        &[
                            (p.dim.as_str(), format!("  {:<16}", $label)),
                            ($colour, v),
                            (
                                p.dim.as_str(),
                                if note.is_empty() {
                                    String::new()
                                } else {
                                    format!("   {}", note)
                                },
                            ),
                        ],
                        w - 1,
                    ));
                }
            }
        }};
    }

    let ratio = quality(row);
    let live = |v: Option<f64>| v.filter(|x| *x != 0.0).map(|x| ms(Some(x)));
    field!(
        "round trip",
        live(row.rtt),
        colour_for(ratio, row.recent_loss, p),
        &ratio.map_or(String::new(), |r| format!("{:.1}x this path's best", r)),
    );
    field!(
        "best ever",
        live(row.floor),
        &p.dim,
        "the floor; the gap above it is congestion",
    );
    field!("jitter", live(row.jitter), &p.dim, "variation in the round trip");
    field!(
        "timeout",
        num(&row.raw, "rto").map(|v| ms(Some(v))),
        &p.dim,
        "how long before a lost packet is resent",
    );
    rows.push(String::new());

    // Lifetime loss lives here rather than in the table because it is a fact
    // about the whole session and changes by the hour, while the table's
    // loss column is about the last two seconds.
    let lifetime = if row.sent > 0.0 {
        100.0 * row.retrans_bytes / row.sent
    } else {
        0.0
    };
    let loss = row.recent_loss;
    field!(
        "loss just now",
        loss.map(|v| format!("{:.2}%", v)),
        if loss.unwrap_or(0.0) >= 0.5 { &p.bad } else { &p.txt },
        "resent since the last look",
    );
    field!(
        "loss lifetime",
        Some(format!("{:.2}%", lifetime)),
        &p.dim,
        &format!(
            "{} resent of {}",
            size_of(row.retrans_bytes),
            size_of(row.sent)
        ),
    );
    field!(
        "reordering",
        row.raw.get("reord_seen").cloned(),
        &p.dim,
        "times packets arrived out of order",
    );
    rows.push(String::new());

    field!("sent", Some(size_of(row.sent)), &p.txt, "");
    field!("received", Some(size_of(row.recv)), &p.txt, "");
    field!(
        "achieved",
        Some(rate(row.delivery)),
        &p.txt,
        "what it has delivered, not its capacity",
    );
    field!(
        "pacing at",
        Some(rate(num(&row.raw, "pacing_rate"))),
        &p.dim,
        "the rate the kernel is willing to send at",
    );
    field!(
        "in flight",
        row.raw.get("cwnd").cloned(),
        &p.dim,
        "packets allowed unacknowledged at once",
    );
    field!(
        "packet size",
        row.mss.map(|v| format!("{} bytes", v as i64)),
        &p.dim,
        "",
    );
    let idle = [row.lastsnd, row.lastrcv]
        .into_iter()
        .flatten()
        .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| a.min(v))));
    field!(
        "idle",
        Some(span(idle)),
        &p.dim,
        "since anything crossed either way",
    );
    rows.push(String::new());

    let one = [row.clone()];
    // Not `if room >= 5`: the chart takes MIN_CHART rows even when the
    // fields have already spent the pane, and the caller scrolls to reach
    // what will not fit. A chart quietly missing reads as missing data.
    let room = h.saturating_sub(rows.len() + 4).max(MIN_CHART);
    {
        rows.extend(graph(
            &one,
            &state.history,
            w,
            room,
            idx,
            // One session on its own screen: there is nothing to push back,
            // so it is drawn at full strength like everything else.
            None,
            window,
            refresh,
            p,
        ));
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), " ".repeat(7)),
                (
                    p.grid.as_str(),
                    format!("└{}", "─".repeat(w.saturating_sub(9).max(10))),
                ),
            ],
            w - 1,
        ));
        let covered = plotted_span(&one, &state.history, window, refresh, w);
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!("        {} ago", span(Some(covered * 1000.0)))),
                (p.dim.as_str(), " ".repeat(w.saturating_sub(26).max(1))),
                (p.dim.as_str(), "now".into()),
            ],
            w - 1,
        ));
    }
    rows
}

/// A braille cell is two dots wide and four tall, so one character holds
/// eight addressable points. The bit for each is fixed by the encoding.
const BRAILLE: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// Plot one session's round trips on a dot canvas finer than the cells.
///
/// Consecutive samples are joined rather than left as marks, which is the
/// difference between a line that reads as a path moving and one that reads
/// as specks a row apart. The masks come back per cell instead of as text so
/// that several sessions can be laid over one another first.
///
/// `slots` is how many samples the axis holds, which is not how many this
/// session has: newest sits against the right edge either way, and a session
/// younger than the chart takes its own share of the width rather than being
/// stretched over all of it. The longest session fills the axis by
/// definition, and it is the one the "N ago" under the corner is measured
/// from, so the label and the left edge cannot drift apart.
fn braille_canvas(
    values: &[f64],
    llo: f64,
    lhi: f64,
    cols: usize,
    rows: usize,
    slots: usize,
) -> Vec<Vec<u8>> {
    let (px_w, px_h) = (cols * 2, rows * 4);
    let mut grid = vec![vec![0u8; cols]; rows];
    if values.is_empty() || px_w == 0 || px_h == 0 {
        return grid;
    }
    let vals: Vec<f64> = values.iter().rev().take(px_w).rev().copied().collect();
    let step = (px_w as f64 - 1.0) / (slots.max(2) as f64 - 1.0);
    let decade = (lhi - llo).max(1e-9);
    let point = |i: usize| -> (i64, i64) {
        let frac = ((vals[i].max(1e-3).log10() - llo) / decade).clamp(0.0, 1.0);
        let age = (vals.len() - 1 - i) as f64;
        (
            px_w as i64 - 1 - (age * step).round() as i64,
            ((1.0 - frac) * (px_h as f64 - 1.0)).round() as i64,
        )
    };
    let dot = |x: i64, y: i64, grid: &mut Vec<Vec<u8>>| {
        if x >= 0 && (x as usize) < px_w && y >= 0 && (y as usize) < px_h {
            grid[y as usize / 4][x as usize / 2] |= BRAILLE[y as usize % 4][x as usize % 2];
        }
    };
    // Every value here is a round trip the kernel measured, so unlike
    // netwatch's idle zero there is no reading that means "nothing happened"
    // and should be left blank. One sample is a measurement and gets its dot.
    let (x, y) = point(0);
    dot(x, y, &mut grid);
    for i in 1..vals.len() {
        let (mut x0, mut y0) = point(i - 1);
        let (x1, y1) = point(i);
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            dot(x0, y0, &mut grid);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let twice = 2 * err;
            if twice >= dy {
                err += dy;
                x0 += sx;
            }
            if twice <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }
    grid
}

/// Lay the canvases over one another, cell by cell.
///
/// A braille cell can hold the dots of two traces but only one colour, and
/// there is no honest way to show both: whichever colour the cell takes, the
/// other session's samples are drawn in a hue that is not theirs. This used
/// to merge the dots and give the cell to whichever session came later in
/// the list, and on latency's chart - the same code - that hid a whole host
/// behind another one eleven milliseconds away, drawn end to end in a colour
/// it did not own.
///
/// So a cell belongs to exactly one trace and shows only that trace's dots.
/// Where several want it, ownership advances once per contested cell, which
/// makes a contested stretch read as interleaved dashed lines - each dot its
/// own colour - rather than one solid line belonging to nobody. Traces that
/// never meet are unaffected and stay solid.
///
/// The turn is counted over contested cells and not taken from the column
/// number, which is the same bug one level in. Indexing by `x` looks fair
/// and is not: when the contested cells all share a parity - which happens
/// as soon as one series has a gap in every other column, and the column
/// width is free-form config - `x % 2` picks the same claimant every time
/// and the other trace is absent from the whole stretch. That is the
/// failure this function exists to prevent, in a narrower form.
fn overlay(layers: &[(String, Vec<Vec<u8>>)], cols: usize, rows: usize) -> Vec<Vec<(String, u8)>> {
    let mut cells = vec![vec![(String::new(), 0u8); cols]; rows];
    for y in 0..rows {
        // Counts contested cells along this row, so every claimant takes a
        // turn no matter which columns the contention lands on.
        let mut turn = 0usize;
        for x in 0..cols {
            let dots = |canvas: &Vec<Vec<u8>>| {
                canvas.get(y).and_then(|line| line.get(x)).copied().unwrap_or(0)
            };
            let claims: Vec<&(String, Vec<Vec<u8>>)> =
                layers.iter().filter(|(_, canvas)| dots(canvas) != 0).collect();
            let Some((colour, canvas)) = claims.get(turn % claims.len().max(1)) else {
                continue;
            };
            cells[y][x] = ((*colour).clone(), dots(canvas));
            if claims.len() > 1 {
                turn += 1;
            }
        }
    }
    cells
}

#[allow(clippy::too_many_arguments)]
fn graph(
    rows: &[Session],
    history: &HashMap<String, Vec<f64>>,
    w: usize,
    h: usize,
    // `start` keeps a session's glyph and hue the same on its own screen as
    // in the list: opening the ▲ row and finding a ● chart reads as a
    // different connection.
    start_at: usize,
    focus: Option<usize>,
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
            // The same span of history as before, condensed to twice as many
            // points: two dots to a cell across.
            let vals = condense(&all[start..], gw * 2);
            if vals.is_empty() {
                None
            } else {
                Some((start_at + i, vals))
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

    // The axis holds as many samples as the longest session has, which is
    // the same number plotted_span turns into the "N ago" beneath the chart:
    // one quantity, so the label and the left edge state the same thing.
    let slots = series.iter().map(|(_, v)| v.len()).max().unwrap_or(1);
    // One canvas per session rather than one shared grid: a braille cell can
    // carry the dots of two traces but only one hue, so each series has to
    // keep its own until the moment they are laid over one another.
    // The selected session is laid down last and at full strength while the
    // rest are mixed toward the backdrop, so the trace being looked at wins
    // any cell it shares. With nothing selected every trace is equal, which
    // is the chart this widget has always drawn.
    let mut layers: Vec<(String, Vec<Vec<u8>>)> = Vec::with_capacity(series.len());
    let mut front: Option<(String, Vec<Vec<u8>>)> = None;
    for (idx, values) in &series {
        let canvas = braille_canvas(values, llo, lhi, gw, gh, slots);
        let hue = p.hues[idx % p.hues.len()].clone();
        match focus {
            None => layers.push((hue, canvas)),
            Some(at) if at == *idx => front = Some((hue, canvas)),
            Some(_) => layers.push((p.faded[idx % p.faded.len()].clone(), canvas)),
        }
    }
    let mut cells = overlay(&layers, gw, gh);
    // The focused trace takes its cells outright rather than being merged
    // into them: a sample drawn in a colour that is not its own is a number
    // on screen that is not real.
    if let Some((hue, canvas)) = front {
        for (y, line) in canvas.iter().enumerate().take(gh) {
            for (x, mask) in line.iter().enumerate().take(gw) {
                if *mask != 0 {
                    cells[y][x] = (hue.clone(), *mask);
                }
            }
        }
    }

    let mut out = Vec::new();
    for (y, line) in cells.iter().enumerate() {
        let frac = 1.0 - (y as f64 / (gh as f64 - 1.0).max(1.0));
        let value = 10f64.powf(llo + frac * (lhi - llo));
        let label = if y == 0 || y == gh / 2 || y == gh - 1 {
            format!("{:>7}", ms(Some(value)))
        } else {
            " ".repeat(7)
        };
        let mut parts: Vec<(&str, String)> =
            vec![(p.dim.as_str(), label), (p.grid.as_str(), "│".into())];
        for (colour, mask) in line {
            parts.push(match mask {
                0 => (p.grid.as_str(), " ".into()),
                m => (
                    colour.as_str(),
                    char::from_u32(0x2800 + *m as u32).unwrap_or(' ').to_string(),
                ),
            });
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
    ok: String,
    warn: String,
    bad: String,
    dim: String,
    grid: String,
    txt: String,
    accent: String,
    link: String,
    hues: Vec<String>,
    /// The same six, mixed toward the backdrop, for traces that are not the
    /// one selected.
    faded: Vec<String>,
}

fn palette() -> Palette {
    Palette {
        ok: tc::rgb(90, 240, 160),
        warn: tc::rgb(255, 200, 90),
        bad: tc::rgb(255, 100, 110),
        dim: tc::rgb(127, 147, 172),
        grid: tc::rgb(60, 78, 98),
        txt: tc::rgb(225, 235, 245),
        accent: tc::rgb(150, 210, 255),
        link: tc::rgb(140, 200, 255),
        hues: HUES.iter().map(|c| tc::rgb(c.0, c.1, c.2)).collect(),
        faded: HUES.iter().map(|c| tc::mix(*c, BACKDROP, FADE)).collect(),
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
        assert_eq!(ms(None), "--");
    }

    #[test]
    fn rates_read_as_a_person_would_say_them() {
        assert_eq!(rate(Some(6_287_464.0)), "6.3Mbps");
        // Lower-case k, as link.py writes it: the two sit side by side on a
        // wall and a capital there reads as a different widget.
        assert_eq!(rate(Some(1_500.0)), "1.5kbps");
        assert_eq!(rate(Some(45_000_000_000.0)), "45.0Gbps");
        assert_eq!(rate(None), "--");
        // A socket that has delivered nothing has delivered nothing; it is
        // not an absent reading.
        assert_eq!(rate(Some(0.0)), "0bps");
    }

    #[test]
    fn byte_counts_read_as_a_person_would_say_them() {
        assert_eq!(size_of(1_669.0), "1.7k");
        assert_eq!(size_of(15_900_000.0), "15.9M");
        assert_eq!(size_of(512.0), "512B");
    }

    #[test]
    fn spans_come_from_milliseconds() {
        assert_eq!(span(Some(45_000.0)), "45s");
        assert_eq!(span(Some(600_000.0)), "10m");
        // The unit changes at the unit, not half again past it: 90s is a
        // minute and a half, and reading it as "90s" was a threshold this
        // port had invented.
        assert_eq!(span(Some(90_000.0)), "1m");
        assert_eq!(span(Some(7_200_000.0)), "2h");
        assert_eq!(span(None), "--");
    }

    #[test]
    fn a_connection_is_judged_against_its_own_best() {
        let near = Session {
            rtt: Some(1.2),
            floor: Some(1.0),
            ..Default::default()
        };
        let far = Session {
            rtt: Some(120.0),
            floor: Some(100.0),
            ..Default::default()
        };
        // Both are 1.2x their floor, so both are fine - a fixed millisecond
        // threshold would have called the second one bad.
        assert_eq!(quality(&near), quality(&far));
        let p = palette();
        assert_eq!(colour_for(quality(&near), None, &p), p.ok);
        assert_eq!(colour_for(quality(&far), None, &p), p.ok);
        // Three times its own floor is congestion wherever it is.
        let slow = Session {
            rtt: Some(300.0),
            floor: Some(100.0),
            ..Default::default()
        };
        assert_eq!(colour_for(quality(&slow), None, &p), p.bad);
        // Loss outranks latency: a fast path dropping packets is not fine.
        assert_eq!(colour_for(quality(&near), Some(2.5), &p), p.bad);
        // Nothing measured yet is not a verdict.
        assert_eq!(colour_for(None, None, &p), p.dim);
    }

    /// Drop the colour, keep the cells - alignment is a question about text.
    #[cfg(test)]
    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out.trim_end().to_string()
    }

    #[test]
    fn a_row_is_laid_out_cell_for_cell() {
        // Captured from link.py in an 85-column pty, with the address
        // replaced by one from RFC 5737's documentation range - it is the
        // same width, so the alignment this exists to check is unchanged,
        // and this repository is public. The row is built from
        // eight separate formats and the header is one fixed string, so
        // nothing inside this file can catch a drift between them - only
        // the other implementation can. This port had three cells of it,
        // and every half looked plausible on its own.
        let want =
            "▐ 203.0.113.221:22 williamli   37ms    20ms    10ms  0.00%  11.1Mbps     1m";
        let row = Session {
            peer: "203.0.113.221:22".into(),
            ip: "203.0.113.221".into(),
            port: 22,
            rtt: Some(37.0),
            jitter: Some(10.0),
            floor: Some(20.0),
            recent_loss: Some(0.0),
            delivery: Some(11_100_000.0),
            lastsnd: Some(60_000.0),
            lastrcv: Some(90_000.0),
            ..Default::default()
        };
        let state = State {
            rows: vec![row.clone()],
            names: HashMap::from([(
                "203.0.113.221".to_string(),
                vec![("williamli".to_string(), "pts/0".to_string())],
            )]),
            history: HashMap::new(),
            err: String::new(),
        };
        // Nothing selected, so no row carries the highlight.
        let drawn = table(&[row], &state, 86, None, &palette());
        assert_eq!(plain(&drawn[1]), want);
    }

    #[test]
    fn a_missing_reading_keeps_its_column() {
        // A socket the kernel has no round trip for still has to leave the
        // numbers to its right where they were, or the whole table shifts
        // on one absent value.
        let row = Session {
            peer: "203.0.113.9:22".into(),
            ip: "203.0.113.9".into(),
            port: 22,
            ..Default::default()
        };
        let state = State {
            rows: vec![row.clone()],
            names: HashMap::new(),
            history: HashMap::new(),
            err: String::new(),
        };
        let drawn = table(&[row], &state, 86, None, &palette());
        assert_eq!(
            plain(&drawn[1]),
            "▐ 203.0.113.9:22                 --      --      --     --        --     --"
        );
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

    #[test]
    fn a_rising_series_climbs_the_canvas() {
        // Eight samples across four cells - two dots each - from the bottom
        // of the decade the axis covers to the top of it.
        let values: Vec<f64> = (0..8).map(|i| 10f64.powf(1.0 + i as f64 / 7.0)).collect();
        let grid = braille_canvas(&values, 1.0, 2.0, 4, 4, 8);
        let highest: Vec<usize> = (0..4)
            .map(|x| {
                grid.iter()
                    .position(|row| row[x] != 0)
                    .expect("every column carries part of the trace")
            })
            .collect();
        // Row zero is the top of the canvas, so climbing counts down.
        assert_eq!(highest.first(), Some(&3));
        assert_eq!(highest.last(), Some(&0));
        assert!(highest.windows(2).all(|p| p[0] >= p[1]), "{:?}", highest);
    }

    #[test]
    fn a_young_session_keeps_to_its_share_of_the_axis() {
        // Four samples on an axis holding eight: half the width, against the
        // right edge, because half the chart is older than the session is.
        let grid = braille_canvas(&[10.0, 10.0, 10.0, 10.0], 0.0, 1.0, 4, 1, 8);
        assert_eq!((grid[0][0], grid[0][1]), (0, 0));
        assert!(grid[0][2] != 0 && grid[0][3] != 0);
        // The same four with the axis to themselves reach the left edge.
        let full = braille_canvas(&[10.0, 10.0, 10.0, 10.0], 0.0, 1.0, 4, 1, 4);
        assert!(full[0].iter().all(|m| *m != 0));
    }

    #[test]
    fn the_scroll_label_keeps_its_width() {
        // The detail footer is measured before the body is built, so this
        // string has to be one width whatever the numbers are. A wider one
        // could wrap the hints onto an extra line and leave the body sized
        // against a footer that is no longer the footer being drawn.
        let widths: Vec<usize> = [(1, 28, 35), (9, 36, 350), (100, 128, 999)]
            .into_iter()
            .map(|(a, b, c)| scroll_label(a, b, c).chars().count())
            .collect();
        assert!(widths.windows(2).all(|p| p[0] == p[1]), "{:?}", widths);
    }

    #[test]
    fn a_contested_cell_belongs_to_one_trace_and_shares_the_run() {
        // Two flat traces contesting all four cells. This used to assert the
        // union of their dots in the later session's colour; that is the bug
        // the latency chart surfaced, where two hosts a few percent apart
        // contest every cell and one was drawn wholly in the other's hue.
        let top = braille_canvas(&[10.0; 8], 0.0, 1.0, 4, 1, 8);
        let bottom = braille_canvas(&[1.0; 8], 0.0, 1.0, 4, 1, 8);
        assert!(top[0][0] != 0 && bottom[0][0] != 0, "both must contest");
        let cells = overlay(
            &[
                ("first".to_string(), top.clone()),
                ("second".to_string(), bottom.clone()),
            ],
            4,
            1,
        );
        for x in 0..4 {
            let (whose, mask) = &cells[0][x];
            let mine = if whose == "first" { top[0][x] } else { bottom[0][x] };
            assert_eq!(*mask, mine, "column {} carries the other trace's dots", x);
            assert_ne!(*mask, top[0][x] | bottom[0][x], "column {} merged", x);
        }
        // The property rather than the phase: the run is shared, so neither
        // trace is missing from it. Pinning the exact alternation here is
        // what let the parity starvation through - it asserted the mechanism
        // and called that the contract.
        let owners: Vec<&str> = (0..4).map(|x| cells[0][x].0.as_str()).collect();
        assert!(owners.contains(&"first"), "{:?}", owners);
        assert!(owners.contains(&"second"), "{:?}", owners);
    }

    #[test]
    fn a_command_that_will_not_run_says_so_rather_than_returning_nothing() {
        // `run` folds failure into an empty string, and for `ss` that is the
        // difference between a machine with nobody connected and a widget
        // that cannot see. The poller puts this text on screen; before it
        // existed, State.err was drawn but never assigned and a dead read
        // rendered as "No inbound sessions".
        let why = run_or(&["definitely-not-a-real-binary-xyz", "--version"])
            .expect_err("a missing binary must not read as empty output");
        assert!(why.contains("did not run"), "{}", why);
        assert!(why.contains("definitely-not-a-real-binary-xyz"), "{}", why);
        // A command that does run still comes back as Ok.
        assert!(run_or(&["true"]).is_ok());
    }

    #[test]
    fn no_trace_is_starved_by_where_the_contention_falls() {
        // Two traces that meet only in even-numbered cells, which is what a
        // series with a gap in every other column produces - and the column
        // width is free-form config, so it is reachable rather than
        // theoretical. Ownership used to be `x % claims.len()`, so every
        // contested cell shared a parity, the modulus picked the same
        // claimant every time, and the other trace was absent from the whole
        // stretch. That is the failure this function exists to prevent, one
        // level in.
        let a = vec![vec![0b0000_0001u8, 0, 0b0000_0001, 0]];
        let b = vec![vec![0b0100_0000u8, 0, 0b0100_0000, 0]];
        let cells = overlay(&[("first".to_string(), a), ("second".to_string(), b)], 4, 1);
        let owners: Vec<&str> = (0..4)
            .filter(|x| cells[0][*x].1 != 0)
            .map(|x| cells[0][x].0.as_str())
            .collect();
        assert_eq!(owners.len(), 2, "both contested cells should be drawn");
        // The property, not the phase. Which of the two takes the first cell
        // is an implementation detail; that neither vanishes is not.
        assert!(owners.contains(&"first"), "{:?}", owners);
        assert!(owners.contains(&"second"), "{:?}", owners);
    }

}
