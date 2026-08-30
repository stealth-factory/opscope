// opscope - small dependency-free terminal widgets
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

//! What is listening on this machine, what started it, and who can reach it.
//!
//! Linux reads `/proc`; macOS reads `lsof` and `ps`. The parsers for both
//! compile everywhere so their fixture tests run on every CI target. Only
//! which file to open, or which command to spawn, is behind `cfg(target_os)`.
//!
//!     ports [-n SECONDS]
//!
//! Keys: up/down select, o hides the machine's own ports, r refreshes,
//! q quits.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use opscope_core as tc;

// Sibling modules in this folder: the widget package owns its parsers
// and its acquisition. `mod parse` needs no #[path]. `mod host` still
// does, because linux.rs and macos.rs are the same module under two
// names so the rest of the widget can call host::sockets() on either.
mod parse;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod host;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod host;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod host {
    pub fn sockets() -> Result<Vec<super::Found>, String> {
        Ok(Vec::new())
    }
    pub fn process_info(_: i32) -> (String, String, Option<f64>) {
        (String::new(), String::new(), None)
    }
    pub fn ours(_: i32) -> Result<bool, ()> {
        Err(())
    }
    pub fn is_zombie(_: i32) -> bool {
        false
    }
}

/// The machine's own ports, hidden behind `o` by default: they are never
/// the answer to "which port is my dev server on".
const SYSTEM_PORTS: &[u16] = &[22, 53, 123, 323, 631, 5353];

/// What config said, if it said anything. Set once in main.
///
/// A static rather than a threaded parameter because two predicates deep
/// in the sort and filter want it and neither is worth rewriting to carry
/// a list that changes only at startup.
static CONFIGURED_PORTS: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();

/// Whether a port belongs to the machine rather than to something started.
/// The configured system-port list, or None when the key is absent.
///
/// Present-and-empty is an answer: hide nothing. Absent means use the
/// built-in defaults. A number that is not a port is dropped rather than
/// wrapped - `65558 as u16` is 22, and hiding SSH because of a typo is
/// worse than ignoring the typo.
fn configured_system_ports(cfg: &serde_json::Value) -> Option<Vec<u16>> {
    // Asking whether the key is there has no value to default: empty
    // means hide nothing, absent means the built-in list.
    if cfg.get("system_ports").is_none() {
        return None;
    }
    Some(
        cfg["system_ports"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_u64())
                    .filter_map(|n| u16::try_from(n).ok())
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn is_system_port(port: u16) -> bool {
    match CONFIGURED_PORTS.get() {
        Some(list) => list.contains(&port),
        None => SYSTEM_PORTS.contains(&port),
    }
}

/// Per-port traffic, sample by sample.
///
/// Each entry is one poll's worth: bytes out, bytes in, and how long the
/// gap to the previous sample actually was. The elapsed time is recorded
/// rather than assumed, because `[r]` polls early and a rate computed
/// against the nominal interval would read high every time someone pressed
/// it.
#[derive(Default)]
struct Traffic {
    last: HashMap<String, Counters>,
    at: f64,
    seen: HashMap<u16, VecDeque<(u64, u64, f64)>>,
    /// The same samples summed over every listening port, kept separately
    /// rather than added up at render time. Ports appear and disappear
    /// mid-history, so their rings are different lengths and summing them
    /// by index would quietly attribute one port's sample to another's
    /// moment.
    total: VecDeque<(u64, u64, f64)>,
}

impl Traffic {
    /// Fold one sample in, and forget ports nothing is listening on.
    fn sample(&mut self, text: &str, listening: &[u16], at: f64) {
        let now = socket_counters(text);
        let gap = at - self.at;
        // The first sample has nothing to subtract from, and a gap of zero
        // would divide a rate by nothing.
        if self.at > 0.0 && gap > 0.0 {
            let moved_now = moved(&self.last, &now, listening);
            for &port in listening {
                let (up, down) = moved_now.get(&port).copied().unwrap_or((0, 0));
                let ring = self.seen.entry(port).or_default();
                ring.push_back((up, down, gap));
                while ring.len() > TRAFFIC_KEPT {
                    ring.pop_front();
                }
            }
            // One entry per sample whether or not anything moved, so the
            // chart's columns are moments rather than events - a gap in it
            // would compress quiet minutes into no width at all.
            let (up, down) = moved_now
                .values()
                .fold((0u64, 0u64), |(a, b), (u, d)| (a + u, b + d));
            self.total.push_back((up, down, gap));
            while self.total.len() > TRAFFIC_KEPT {
                self.total.pop_front();
            }
        }
        self.seen.retain(|port, _| listening.contains(port));
        self.last = now;
        self.at = at;
    }

    /// The most recent sample as a rate, in bytes per second.
    fn rate(&self, port: u16) -> Option<(f64, f64)> {
        let (up, down, gap) = *self.seen.get(&port)?.back()?;
        (gap > 0.0).then(|| (up as f64 / gap, down as f64 / gap))
    }

    /// Every kept sample as a rate, oldest first, each with the gap it was
    /// measured over - so a chart showing only the last so many of them can
    /// say how much history *that* is, rather than how much is kept.
    fn series(&self, port: u16) -> Vec<(f64, f64, f64)> {
        self.seen.get(&port).map(|r| rates(r)).unwrap_or_default()
    }

    /// Every sample summed over all the listening ports.
    fn totals(&self) -> Vec<(f64, f64, f64)> {
        rates(&self.total)
    }
}

/// A ring of byte counts as rates, oldest first, each keeping the gap it
/// was measured over so a chart can say how much history it is showing.
fn rates(ring: &VecDeque<(u64, u64, f64)>) -> Vec<(f64, f64, f64)> {
    ring.iter()
        .filter(|(_, _, gap)| *gap > 0.0)
        .map(|(up, down, gap)| (*up as f64 / gap, *down as f64 / gap, *gap))
        .collect()
}

/// How many samples of per-port traffic to keep, which is what the chart on
/// a port's own screen is drawn from.
///
/// The chart draws one column per sample, so this has to outlast the widest
/// pane anyone opens it in. At the default four-second poll it is a little
/// over half an hour.
const TRAFFIC_KEPT: usize = 512;

/// One TCP socket's byte counters, as the kernel has them.
///
/// Keyed by inode because that is what identifies a socket across samples.
/// The port is the *local* one: traffic to a listening port arrives on the
/// connections accepted from it, and the listening socket itself carries
/// none.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Counters {
    port: u16,
    sent: u64,
    recv: u64,
}

/// Every TCP socket's byte counters, keyed by inode.
///
/// `-i` for the counters, `-e` for the inode - the same two flags netwatch
/// asks for, and the same two-line shape: addresses and inode, then the
/// counters on an indented continuation.
///
/// Unlike netwatch this filters nothing. netwatch drops loopback peers
/// because it is about what leaves the machine; here the loopback peers are
/// the whole point, since a browser hitting a dev server on 127.0.0.1 is
/// the traffic being asked about.
fn socket_counters(text: &str) -> HashMap<String, Counters> {
    let mut found = HashMap::new();
    let (mut inode, mut port) = (None, 0u16);
    for (i, line) in text.lines().enumerate() {
        if i == 0 && line.starts_with("State") {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // Column 3 is our address, and its port is the one a listener
            // would be on.
            port = cols
                .get(3)
                .and_then(|a| a.rsplit_once(':'))
                .and_then(|(_, p)| p.parse().ok())
                .unwrap_or(0);
            inode = counter_field(line, "ino:").filter(|v| v != "0");
            continue;
        }
        let Some(id) = inode.take() else { continue };
        found.insert(
            id,
            Counters {
                port,
                sent: counter_field(line, "bytes_sent:")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
                recv: counter_field(line, "bytes_received:")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
            },
        );
    }
    found
}

/// The value after `key:` on a line, up to the next space.
fn counter_field(line: &str, key: &str) -> Option<String> {
    let at = line.find(key)? + key.len();
    let rest = &line[at..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].to_string()).filter(|v| !v.is_empty())
}

/// What moved on each listening port between two samples.
///
/// Only sockets present in *both* count. A socket that appeared since the
/// last sample has no previous total to subtract, and one that closed took
/// its last few bytes with it - said out loud in the doc rather than
/// guessed at here.
///
/// Inodes are reused after a socket closes, so a raw subtraction can go
/// negative; it is clamped rather than wrapped. And only ports something is
/// actually listening on are tallied, because most established sockets are
/// outbound and their local port is an ephemeral one that means nothing.
fn moved(
    before: &HashMap<String, Counters>,
    now: &HashMap<String, Counters>,
    listening: &[u16],
) -> HashMap<u16, (u64, u64)> {
    let mut out: HashMap<u16, (u64, u64)> = HashMap::new();
    for (id, c) in now {
        if !listening.contains(&c.port) {
            continue;
        }
        let Some(was) = before.get(id) else { continue };
        // A reused inode is a different socket wearing the same name; its
        // counters start again and the subtraction is meaningless.
        if was.port != c.port {
            continue;
        }
        let slot = out.entry(c.port).or_default();
        slot.0 += c.sent.saturating_sub(was.sent);
        slot.1 += c.recv.saturating_sub(was.recv);
    }
    out
}

/// Whether the two traffic columns fit, given what the row must show first.
///
/// Both are measured against a row that already has UP and EXPOSED on it,
/// whether or not this pane is wide enough to be showing them yet. Without
/// that, crossing the width where those two arrive pushed TRAFFIC back off
/// again - a pane that got wider and said less, which is the same fault the
/// linear board had and the reason this is a function with a test rather
/// than arithmetic inline.
///
/// Returns whether the rates fit, and whether the shapes fit after them.
fn extra_columns(
    w: usize,
    kind_w: usize,
    project_w: usize,
    traffic_w: usize,
    spark_w: usize,
) -> (bool, bool) {
    // Marker and port, bind, the gap after the name, and the two columns at
    // the end - counted always, so nothing here trades places with them.
    let fixed = 1 + 6 + 8 + 2 + 6 + 8;
    let room = |extra: usize| w.saturating_sub(1) >= fixed + project_w + kind_w + extra;
    let busy = room(traffic_w);
    (busy, busy && room(traffic_w + spark_w))
}

/// The widest name in the table, which is what the WHAT column has to be.
fn longest_kind(rows: &[&Row]) -> usize {
    rows.iter()
        .map(|r| r.kind.chars().count())
        .max()
        .unwrap_or(0)
        .max(4)
}

/// A rate in as few cells as it can be read in, for a table column.
///
/// One significant figure and a single-letter unit: five cells at most, so
/// two of them and their arrows fit in thirteen.
fn brief(n: f64) -> String {
    for (suffix, scale) in [("G", 1e9), ("M", 1e6), ("K", 1e3)] {
        if n >= scale {
            let v = n / scale;
            return if v < 10.0 {
                format!("{:.1}{}", v, suffix)
            } else {
                format!("{:.0}{}", v, suffix)
            };
        }
    }
    format!("{:.0}B", n)
}

/// A byte count at whatever unit keeps it readable.
fn units(n: f64) -> String {
    for (suffix, scale) in [("GB", 1e9), ("MB", 1e6), ("KB", 1e3)] {
        if n >= scale {
            return format!("{:.1} {}", n / scale, suffix);
        }
    }
    format!("{} B", n as i64)
}

fn rate_of(n: f64) -> String {
    if n > 0.0 {
        format!("{}/s", units(n))
    } else {
        "-".into()
    }
}

/// A span at whatever unit keeps it readable.
fn over(seconds: f64) -> String {
    let s = seconds as i64;
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    }
}

/// One row's worth of shape: the last `cells` samples of a port's traffic,
/// both directions together, in a single line of bars.
///
/// Scaled to its *own* peak, not to the busiest port on screen. A shared
/// scale would flatten every quiet port to nothing, and nothing is what a
/// port with no traffic looks like. So this column says shape and the
/// TRAFFIC column beside it says size - and the two are read together.
///
/// Cells with no sample behind them are dotted, the same as the chart on the
/// port's own screen, because a port nobody has measured yet and a port
/// nobody is calling are not the same thing.
fn spark(series: &[(f64, f64, f64)], cells: usize, p: &Palette) -> Vec<(String, String)> {
    if cells == 0 {
        return Vec::new();
    }
    let window: Vec<f64> = series
        .iter()
        .rev()
        .take(cells)
        .rev()
        .map(|s| s.0 + s.1)
        .collect();
    let peak = window.iter().copied().fold(0.0f64, f64::max);
    let mut out = Vec::new();
    if window.len() < cells {
        out.push((p.grid.clone(), "·".repeat(cells - window.len())));
    }
    if peak <= 0.0 {
        // Measured, and nothing moved: a flat baseline. Blank would put it
        // in the same shape as a port nothing has sampled yet, and the
        // dotted cells beside it exist precisely to keep those apart.
        out.push((p.grid.clone(), "─".repeat(window.len())));
        return out;
    }
    let cols: Vec<(f64, String)> = window.iter().map(|v| (*v, p.open.clone())).collect();
    for row in tc::vbars(&cols, 1, peak) {
        for (colour, text) in row {
            out.push((colour, text));
        }
    }
    out
}

/// Traffic on one port over time: out above the line, in below it.
///
/// The chart is as wide as the pane. One column is one sample, newest at the
/// right, and until there is enough history to fill it the left of the plot
/// carries a dotted baseline rather than bars of no height: a flat line
/// there would say the port was quiet then, and it says nothing of the sort.
///
/// Each direction is scaled to its own peak and each says what that peak
/// was, in a gutter down the left so the plot keeps the rest of the width. A
/// shared scale would flatten the smaller of the two into nothing, and
/// nothing is what a source with no traffic looks like.
fn traffic_chart(
    series: &[(f64, f64, f64)],
    heading: &str,
    rows: usize,
    gap: f64,
    w: usize,
    p: &Palette,
) -> Vec<String> {
    let up_peak = series.iter().map(|s| s.0).fold(0.0f64, f64::max);
    let down_peak = series.iter().map(|s| s.1).fold(0.0f64, f64::max);
    // The gutter is as wide as the wider of the two labels, so the plots
    // line up under each other and the divider spans exactly the plot.
    let label = |arrow: &str, peak: f64| {
        if peak > 0.0 {
            format!("{} {}", arrow, rate_of(peak))
        } else {
            String::new()
        }
    };
    let (up_label, down_label) = (label("↑", up_peak), label("↓", down_peak));
    let lab = up_label
        .chars()
        .count()
        .max(down_label.chars().count())
        .clamp(4, 16);
    let plot = (w - 1).saturating_sub(lab + 2).max(12);
    // The most recent `plot` samples, and how far back that reaches - which
    // is not how far back the kept history reaches once it has overflowed.
    let window: Vec<(f64, f64, f64)> = series.iter().rev().take(plot).rev().copied().collect();
    let reach: f64 = window.iter().map(|s| s.2).sum();
    let blank = plot - window.len();

    let mut out = vec![tc::seg(
        &[
            (p.lbl.as_str(), format!(" ── {} ── ", heading)),
            (p.open.as_str(), "↑ out above".into()),
            (p.dim.as_str(), " · ".into()),
            (p.local.as_str(), "↓ in below".into()),
            // The window is named, because a chart of an unnamed window is a
            // shape rather than a measurement.
            (
                p.dim.as_str(),
                if window.is_empty() {
                    String::new()
                } else {
                    format!(
                        "  · {} of history, sampled every {}",
                        over(reach),
                        over(gap)
                    )
                },
            ),
        ],
        w - 1,
    )];
    if window.iter().all(|(a, b, _)| *a == 0.0 && *b == 0.0) {
        out.push(tc::seg(
            &[(
                p.dim.as_str(),
                if series.is_empty() {
                    "  waiting for a second sample".into()
                } else {
                    "  nothing has moved on it".to_string()
                },
            )],
            w - 1,
        ));
        return out;
    }

    let half = |pick: fn(&(f64, f64, f64)) -> f64,
                colour: &str,
                peak: f64,
                text: &str,
                down: bool|
     -> Vec<String> {
        let cols: Vec<(f64, String)> = window
            .iter()
            .map(|s| (pick(s), colour.to_string()))
            .collect();
        let bars = if down {
            tc::vbars_down(&cols, rows, peak)
        } else {
            tc::vbars(&cols, rows, peak)
        };
        // The label sits on the row nearest the divider, which is the row a
        // full-height bar reaches: the top for the upward half, the first
        // drawn row for the downward one.
        let on = if down {
            0
        } else {
            bars.len().saturating_sub(1)
        };
        bars.into_iter()
            .enumerate()
            .map(|(i, row)| {
                let mut line: Vec<(&str, String)> = vec![(
                    colour,
                    format!("{:>1$} ", if i == on { text } else { "" }, lab),
                )];
                // Where there is no history yet, a dotted baseline on the
                // row against the divider. Left blank it would be
                // indistinguishable from a stretch of real zeroes, and a
                // quiet port and an unmeasured one are not the same thing -
                // which is the one confusion this widget must never cause.
                if blank > 0 {
                    if i == on {
                        line.push((p.grid.as_str(), "·".repeat(blank)));
                    } else {
                        line.push((tc::RST, " ".repeat(blank)));
                    }
                }
                for (c, t) in &row {
                    line.push((c.as_str(), t.clone()));
                }
                tc::seg(&line, w - 1)
            })
            .collect()
    };
    out.extend(half(|s| s.0, p.open.as_str(), up_peak, &up_label, false));
    out.push(tc::seg(
        &[
            (p.dim.as_str(), " ".repeat(lab + 1)),
            (p.grid.as_str(), "─".repeat(plot)),
        ],
        w - 1,
    ));
    out.extend(half(
        |s| s.1,
        p.local.as_str(),
        down_peak,
        &down_label,
        true,
    ));
    out
}

/// Process titles worth recognising, first match winning, so the specific
/// ones come before `node` and `python`.
const KINDS: &[(&str, &str)] = &[
    ("next-server", "Next.js"),
    ("node_modules/next/dist", "Next.js"),
    ("node_modules/.bin/vite", "Vite"),
    ("react-scripts", "React"),
    ("webpack", "webpack"),
    ("nuxt", "Nuxt"),
    ("astro", "Astro"),
    ("remix", "Remix"),
    ("uvicorn", "uvicorn"),
    ("gunicorn", "gunicorn"),
    ("manage.py", "Django"),
    ("rails", "Rails"),
    ("postgres", "Postgres"),
    ("redis-server", "Redis"),
    ("mysqld", "MySQL"),
    ("mongod", "MongoDB"),
    ("docker-proxy", "Docker"),
    ("ollama", "Ollama"),
    ("code-server", "VS Code"),
    ("herdr", "Herdr"),
    ("tailscaled", "Tailscale"),
    ("sshd", "SSH"),
    ("systemd-resolve", "DNS"),
];

/// The two that are runtimes rather than programs, and so have to be the
/// command being run rather than a word anywhere in its path.
///
/// The Python anchors these - `[/ ]node(\s|$)` and `[/ ]python[0-9.]*(\s|$)`
/// - and the port flattened both to a plain substring. Every binary that
/// lives under a directory with `node` or `python` in it then answered to
/// the wrong name: a standalone tool installed into `node_modules` was
/// listed as "Node", which is the runtime it is not written in.
const RUNTIMES: &[(&str, &str)] = &[("python", "Python"), ("node", "Node")];

/// Whether a command line actually runs `name`, rather than merely passing
/// through a directory called that.
///
/// A trailing version is part of the name - `python3`, `python3.11` - which
/// is what the `[0-9.]*` in the Python's pattern is for.
fn runs_command(cmdline: &str, name: &str) -> bool {
    cmdline.split_whitespace().any(|word| {
        let base = word.rsplit('/').next().unwrap_or(word);
        match base.strip_prefix(name) {
            Some("") => true,
            Some(tail) => tail.chars().all(|c| c.is_ascii_digit() || c == '.'),
            None => false,
        }
    })
}

/// Ports whose owner is usually root, so /proc will not say what it is.
/// Naming them by convention is a guess, and is marked as one.
const BY_PORT: &[(u16, &str)] = &[
    (22, "SSH"),
    (53, "DNS"),
    (80, "HTTP"),
    (123, "NTP"),
    (443, "HTTPS"),
    (631, "printing"),
    (3306, "MySQL"),
    (5432, "Postgres"),
    (6379, "Redis"),
    (5353, "mDNS"),
    (27017, "MongoDB"),
];

// cmdline, cwd and families are carried for the detail screen; the table
// itself has no room for them.
#[derive(Clone, Default)]
struct Row {
    port: u16,
    bind: String,
    families: u8,
    pid: Option<i32>,
    cmdline: String,
    cwd: String,
    kind: String,
    guessed: bool,
    user: String,
    project: String,
    gone: bool,
    up: Option<f64>,
    exposed: String,
    orphan: bool,
}

/// Who can reach a socket bound to this address.
fn bind_class(bind: &str) -> String {
    if bind == "0.0.0.0" || bind == "::" {
        return "all".into();
    }
    if bind.starts_with("127.") || bind == "::1" {
        return "local".into();
    }
    if is_tailnet_v4(bind) || bind.starts_with("fd7a:115c:a1e0") {
        return "tailnet".into();
    }
    bind.to_string()
}

/// Tailscale hands out 100.64.0.0/10, which is a different answer from
/// either "all" or "local".
fn is_tailnet_v4(bind: &str) -> bool {
    let mut parts = bind.split('.');
    match (parts.next(), parts.next()) {
        (Some("100"), Some(second)) => second
            .parse::<u16>()
            .map(|n| (64..=127).contains(&n))
            .unwrap_or(false),
        _ => false,
    }
}

/// The bind address out of /proc's little-endian hex.
fn hex_addr(text: &str) -> String {
    if text.len() == 8 {
        let n = u32::from_str_radix(text, 16).unwrap_or(0);
        return format!(
            "{}.{}.{}.{}",
            n & 0xff,
            (n >> 8) & 0xff,
            (n >> 16) & 0xff,
            (n >> 24) & 0xff
        );
    }
    if text.len() == 32 {
        // Written as four 32-bit words, each little-endian: the four bytes
        // of a word appear in reverse of the order they take in the
        // address. Reverse each word and the sixteen bytes are in order.
        let mut groups = Vec::new();
        for word in 0..4 {
            let raw = &text[word * 8..word * 8 + 8];
            let mut bytes = [0u8; 4];
            for (i, byte) in bytes.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16).unwrap_or(0);
            }
            bytes.reverse();
            groups.push(format!("{:02x}{:02x}", bytes[0], bytes[1]));
            groups.push(format!("{:02x}{:02x}", bytes[2], bytes[3]));
        }
        return compress_v6(&groups);
    }
    "?".into()
}

/// The conventional shortest form of an IPv6 address.
fn compress_v6(groups: &[String]) -> String {
    let trimmed: Vec<String> = groups
        .iter()
        .map(|g| g.trim_start_matches('0').to_string())
        .map(|g| if g.is_empty() { "0".into() } else { g })
        .collect();
    let (mut best_at, mut best_len, mut at, mut len) = (usize::MAX, 0usize, usize::MAX, 0usize);
    for (i, g) in trimmed.iter().enumerate() {
        if g == "0" {
            if len == 0 {
                at = i;
            }
            len += 1;
            if len > best_len {
                best_len = len;
                best_at = at;
            }
        } else {
            len = 0;
        }
    }
    if best_len < 2 {
        return trimmed.join(":");
    }
    let head = trimmed[..best_at].join(":");
    let tail = trimmed[best_at + best_len..].join(":");
    format!("{}::{}", head, tail)
}

/// A listening socket after acquisition, pid already resolved.
///
/// Linux walks `/proc` for the inode; macOS gets the pid from `lsof`.
/// The rest of the widget does not care which.
struct Found {
    port: u16,
    bind: String,
    uid: u32,
    pid: Option<i32>,
}

/// Whose socket it is, by name where the machine has one.
fn owner_name(uid: u32) -> String {
    if uid == unsafe { libc::getuid() } {
        return String::new();
    }
    let entry = unsafe { libc::getpwuid(uid) };
    if entry.is_null() {
        return format!("uid {}", uid);
    }
    let name = unsafe { std::ffi::CStr::from_ptr((*entry).pw_name) };
    name.to_string_lossy().to_string()
}

/// What a directory calls itself, for the label a person would use.
fn project_name(cwd: &str) -> String {
    if cwd.is_empty() {
        return String::new();
    }
    let real = cwd.trim_end_matches(" (deleted)");
    let named = std::fs::read_to_string(format!("{}/package.json", real))
        .ok()
        .and_then(|text| json_string(&text, "name"));
    let base = std::path::Path::new(real)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let name = named.unwrap_or(base);
    if cwd.ends_with("(deleted)") {
        format!("{} ✗", name)
    } else {
        name
    }
}

/// The first string value for a top-level key, without a JSON parser.
///
/// Enough for `package.json`'s name and for tailscale's serve status: both
/// are shapes this only needs one field out of, and a parser for them would
/// be a dependency bought for two lookups.
fn json_string(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let at = text.find(&needle)? + needle.len();
    let rest = &text[at..];
    let colon = rest.find(':')? + 1;
    let rest = &rest[colon..];
    let open = rest.find('"')? + 1;
    let rest = &rest[open..];
    let close = rest.find('"')?;
    Some(rest[..close].to_string())
}

/// What sort of server this is, from the process itself.
fn kind_of(cmdline: &str, port: u16) -> (String, bool) {
    if !cmdline.is_empty() {
        for (needle, name) in KINDS.iter().chain(RUNTIMES) {
            if if RUNTIMES.iter().any(|(n, _)| n == needle) {
                runs_command(cmdline, needle)
            } else {
                cmdline.contains(needle)
            } {
                // Next.js rewrites its own title to next-server (v16.3.0),
                // which hands over the framework and the version at once.
                if let Some(version) = version_in(cmdline) {
                    return (format!("{} {}", name, version), false);
                }
                return (name.to_string(), false);
            }
        }
        let first = cmdline.split_whitespace().next().unwrap_or("");
        let base = first.rsplit('/').next().unwrap_or(first);
        if !base.is_empty() {
            return (base.to_string(), false);
        }
    }
    for (known, name) in BY_PORT {
        if *known == port {
            return (format!("{}?", name), true);
        }
    }
    (String::new(), false)
}

fn version_in(cmdline: &str) -> Option<String> {
    let at = cmdline.find("(v")?;
    let rest = &cmdline[at + 2..];
    let close = rest.find(')')?;
    let version = &rest[..close];
    if version.chars().next()?.is_ascii_digit() {
        Some(version.to_string())
    } else {
        None
    }
}

/// Seconds before an external command is given up on, from ports.py's longest, on the serve/funnel commands.
const RUN_TIMEOUT: u64 = 30;

/// Ports Tailscale is serving, and whether the world can see them.
fn exposure() -> HashMap<u16, String> {
    let mut served = HashMap::new();
    let text = tc::run_quiet(&["tailscale", "serve", "status", "--json"], RUN_TIMEOUT);
    for port in proxied_ports(&text) {
        served.insert(port, "tailnet".to_string());
    }
    let funnel = tc::run_quiet(&["tailscale", "funnel", "status"], RUN_TIMEOUT);
    if !funnel.contains("tailnet only") {
        for port in proxied_ports(&funnel) {
            served.insert(port, "public".to_string());
        }
    }
    served
}

/// Every local port a proxy line points at, in either output shape.
fn proxied_ports(text: &str) -> Vec<u16> {
    let mut found = Vec::new();
    for marker in ["127.0.0.1:", "localhost:", "[::1]:"] {
        let mut rest = text;
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(port) = digits.parse::<u16>() {
                found.push(port);
            }
        }
    }
    found
}

/// One entry per listening service, plus anything served but not bound.
fn scan() -> Result<Vec<Row>, String> {
    let served = exposure();
    let mut rows: Vec<Row> = Vec::new();
    let mut services: HashMap<(u16, Option<i32>, String), usize> = HashMap::new();
    let mut seen: Vec<u16> = Vec::new();
    let stamp = tc::now();
    let mut infos: HashMap<i32, (String, String, Option<f64>)> = HashMap::new();

    for sock in host::sockets()? {
        let pid = sock.pid;
        // A server on both address families is two sockets in the kernel
        // table but one thing to know about. Any of port, owner or
        // reachability differing is a real second row.
        let key = (sock.port, pid, bind_class(&sock.bind));
        if let Some(&at) = services.get(&key) {
            rows[at].families += 1;
            continue;
        }
        let (cmdline, cwd, started) = match pid {
            Some(pid) => infos
                .entry(pid)
                .or_insert_with(|| host::process_info(pid))
                .clone(),
            None => (String::new(), String::new(), None),
        };
        let (kind, guessed) = kind_of(&cmdline, sock.port);
        let row = Row {
            port: sock.port,
            bind: sock.bind.clone(),
            families: 1,
            pid,
            cmdline,
            cwd: cwd.clone(),
            kind,
            guessed,
            user: owner_name(sock.uid),
            project: project_name(&cwd),
            gone: cwd.ends_with("(deleted)"),
            up: started.map(|s| stamp - s),
            exposed: served.get(&sock.port).cloned().unwrap_or_default(),
            orphan: false,
        };
        services.insert(key, rows.len());
        seen.push(sock.port);
        rows.push(row);
    }

    // A port Tailscale forwards to with nothing behind it is worth its own
    // row: the URL exists, answers 502, and nothing in lsof explains why.
    for (port, how) in &served {
        if !seen.contains(port) {
            rows.push(Row {
                port: *port,
                kind: "nothing listening".into(),
                exposed: how.clone(),
                orphan: true,
                ..Default::default()
            });
        }
    }
    rows.sort_by_key(|r| (is_system_port(r.port), r.port));
    Ok(rows)
}

fn span(seconds: Option<f64>) -> String {
    let s = match seconds {
        Some(s) if s >= 0.0 => s,
        _ => return "--".into(),
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

/// Whether a row is part of the machine rather than something you started.
fn theirs(row: &Row) -> bool {
    if row.orphan {
        return false;
    }
    is_system_port(row.port) || !row.user.is_empty()
}

/// Every address this machine holds, by interface.
///
/// Link-local is dropped: an fe80:: address needs a zone index to be usable
/// and is never what somebody wants pasted into a browser.
fn interfaces() -> Vec<(String, String, bool)> {
    // Which command is on PATH varies within a platform: a Linux box
    // without iproute2, a Mac with it. cfg cannot see any of that.
    if have("ip") {
        return parse::parse_ip_json(&tc::run_quiet(&["ip", "-j", "addr"], RUN_TIMEOUT));
    }
    if have("ifconfig") {
        return parse::parse_ifconfig(&tc::run_quiet(&["ifconfig", "-a"], RUN_TIMEOUT));
    }
    Vec::new()
}

#[derive(Clone, Default)]
struct Net {
    name: String,
    ips: Vec<String>,
    funnel: bool,
    operator: bool,
}

/// This node's tailnet name and addresses, and whether it may Funnel.
///
/// Funnel is off unless the tailnet's policy grants the node the attribute,
/// and the node knows: the capability is in the map the coordination server
/// hands it. Asking here means the widget can say so instead of offering a
/// key that only ever returns an error.
fn tailnet_self() -> Net {
    let mut out = Net::default();
    let data: serde_json::Value = serde_json::from_str(&tc::run_quiet(
        &["tailscale", "status", "--json"],
        RUN_TIMEOUT,
    ))
    .unwrap_or(serde_json::Value::Null);
    let node = &data["Self"];
    out.name = node["DNSName"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_string();
    out.ips = node["TailscaleIPs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    out.funnel = node["CapMap"]
        .as_object()
        .is_some_and(|m| m.keys().any(|k| k.contains("cap/funnel")));
    // Changing the serve config is a root operation unless this user has
    // been named the operator. Worth knowing before the key is pressed,
    // since the fix is a one-off command rather than anything this can do.
    let prefs: serde_json::Value = serde_json::from_str(&tc::run_quiet(
        &["tailscale", "debug", "prefs"],
        RUN_TIMEOUT,
    ))
    .unwrap_or(serde_json::Value::Null);
    let who = prefs["OperatorUser"].as_str().unwrap_or("");
    out.operator = unsafe { libc::getuid() } == 0 || (!who.is_empty() && who == username());
    out
}

fn username() -> String {
    std::env::var("USER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| owner_name(unsafe { libc::getuid() }))
}

/// An address as it goes in a URL - IPv6 needs its brackets.
fn host_part(ip: &str, v6: bool) -> String {
    if v6 {
        format!("[{}]", ip)
    } else {
        ip.to_string()
    }
}

/// A URL for a host and port, with the scheme the port implies.
fn url_for(host: &str, v6: bool, port: u16) -> String {
    let scheme = if port == 443 || port == 8443 {
        "https"
    } else {
        "http"
    };
    format!("{}://{}:{}", scheme, host_part(host, v6), port)
}

/// Where this port can actually be reached, most local first.
///
/// Bounded by what the socket is bound to, which is the part that gets got
/// wrong: a server on 127.0.0.1 is not reachable at this machine's LAN
/// address no matter how many addresses the machine has, and offering one
/// to copy would hand somebody a URL that cannot work. Only a socket bound
/// to every interface gets the full list.
///
/// A served port is the exception worth keeping: Tailscale proxies to it
/// over loopback, so its https URL works even for a loopback-only server.
fn addresses(row: &Row, net: &Net, cfg: &serde_json::Value) -> Vec<(String, String)> {
    let (port, reach) = (row.port, bind_class(&row.bind));
    let mut found = Vec::new();
    let served = served_url(cfg, port);
    if !served.is_empty() {
        found.push((served, "tailnet · via serve".to_string()));
    }
    // Nothing is listening, so every address below would refuse the
    // connection. The serve URL above is the only one that exists, and it
    // answers 502 - which is the whole reason this row is on screen.
    if row.orphan {
        return found;
    }
    if reach == "local" {
        found.push((
            url_for("127.0.0.1", false, port),
            "this machine only".to_string(),
        ));
        return found;
    }
    if reach == "tailnet" {
        for ip in &net.ips {
            found.push((url_for(ip, ip.contains(':'), port), "tailnet".to_string()));
        }
        if !net.name.is_empty() {
            found.push((
                url_for(&net.name, false, port),
                "tailnet · name".to_string(),
            ));
        }
        return found;
    }
    if reach != "all" {
        // Bound to one particular address, so that address is the answer.
        found.push((
            url_for(&row.bind, row.bind.contains(':'), port),
            "this interface".to_string(),
        ));
        return found;
    }
    found.push((
        url_for("127.0.0.1", false, port),
        "this machine".to_string(),
    ));
    for (name, ip, v6) in interfaces() {
        if ip.starts_with("127.") || ip == "::1" {
            continue;
        }
        let note = if net.ips.iter().any(|t| *t == ip) {
            "tailnet".to_string()
        } else {
            name
        };
        found.push((url_for(&ip, v6, port), note));
    }
    if !net.name.is_empty() {
        found.push((
            url_for(&net.name, false, port),
            "tailnet · name".to_string(),
        ));
    }
    found
}

/// Tailscale's own serve configuration, as it reports it.
///
/// The JSON form rather than the text: the detail view needs the URL a
/// served port answers on, and putting a second port behind the same node
/// needs to know which mounts are already taken. Both are structure the
/// text output only implies.
fn serve_config() -> serde_json::Value {
    serde_json::from_str(&tc::run_quiet(
        &["tailscale", "serve", "status", "--json"],
        RUN_TIMEOUT,
    ))
    .unwrap_or(serde_json::Value::Null)
}

/// Whether a proxy target names this port - `:3000` or `:3000/`, not
/// `:30001`, which a plain substring search would have accepted.
fn proxies_port(proxy: &str, port: u16) -> bool {
    let want = format!(":{}", port);
    let mut rest = proxy;
    while let Some(at) = rest.find(&want) {
        let after = &rest[at + want.len()..];
        if after.is_empty() || after.starts_with('/') {
            return true;
        }
        rest = &rest[at + 1..];
    }
    false
}

/// The https URL a served port answers on, where one is configured.
fn served_url(cfg: &serde_json::Value, port: u16) -> String {
    let web = match cfg["Web"].as_object() {
        Some(w) => w,
        None => return String::new(),
    };
    for (mount, entry) in web {
        for (path, handler) in entry["Handlers"].as_object().into_iter().flatten() {
            if !proxies_port(handler["Proxy"].as_str().unwrap_or(""), port) {
                continue;
            }
            let (host, listen) = mount.split_once(':').unwrap_or((mount.as_str(), ""));
            let port_part = if listen.is_empty() || listen == "443" {
                String::new()
            } else {
                format!(":{}", listen)
            };
            return format!("https://{}{}{}", host, port_part, path);
        }
    }
    String::new()
}

#[derive(Clone)]
struct Tunnel {
    pid: i32,
    url: String,
}

/// Where a launched quick tunnel's pid and URL are remembered.
///
/// cloudflared holds no listening socket - it dials out - so nothing in
/// /proc ties it to the port it serves. Without a note on disk the widget
/// would lose a tunnel the moment it restarted, and leave it running with
/// no way to find or stop it.
fn tunnel_dir() -> String {
    let base = std::env::var("XDG_STATE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.local/state", std::env::var("HOME").unwrap_or_default()));
    let path = format!("{}/opscope/tunnels", base);
    match std::fs::create_dir_all(&path) {
        Ok(()) => path,
        Err(_) => String::new(),
    }
}

/// The quick tunnel for a port: its pid, its URL, whether it still runs.
fn tunnel_state(port: u16) -> Option<Tunnel> {
    let dir = tunnel_dir();
    if dir.is_empty() {
        return None;
    }
    let text = std::fs::read_to_string(format!("{}/{}.json", dir, port)).ok()?;
    let note: serde_json::Value = serde_json::from_str(&text).ok()?;
    let pid = note["pid"].as_i64().unwrap_or(0) as i32;
    if !alive(pid) {
        forget_tunnel(port);
        return None;
    }
    Some(Tunnel {
        pid,
        url: note["url"].as_str().unwrap_or("").to_string(),
    })
}

fn forget_tunnel(port: u16) {
    let dir = tunnel_dir();
    if !dir.is_empty() {
        let _ = std::fs::remove_file(format!("{}/{}.json", dir, port));
    }
}

/// The first trycloudflare URL in a log, if it has printed one yet.
fn quick_url(text: &str) -> String {
    const TAIL: &str = ".trycloudflare.com";
    let at = match text.find(TAIL) {
        Some(at) => at,
        None => return String::new(),
    };
    let head = &text[..at];
    let start = match head.rfind("https://") {
        Some(s) => s,
        None => return String::new(),
    };
    let name = &head[start + 8..];
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return String::new();
    }
    format!("https://{}{}", name, TAIL)
}

/// Run a cloudflared quick tunnel for a port and return its URL.
///
/// A quick tunnel needs no account and no DNS: cloudflared picks a random
/// trycloudflare.com name and prints it. A named tunnel on a domain of your
/// own needs credentials and a DNS record, which is a setup task rather
/// than a keypress, and is deliberately not attempted here.
fn start_tunnel(port: u16, wait: f64) -> (String, String) {
    let dir = tunnel_dir();
    if dir.is_empty() {
        return (
            String::new(),
            "no state directory to record the tunnel in".into(),
        );
    }
    let log = format!("{}/{}.log", dir, port);
    let handle = match std::fs::File::create(&log) {
        Ok(f) => f,
        Err(e) => return (String::new(), e.to_string()),
    };
    let errors = match handle.try_clone() {
        Ok(f) => f,
        Err(e) => return (String::new(), e.to_string()),
    };
    let child = std::process::Command::new("cloudflared")
        .args([
            "tunnel",
            "--no-autoupdate",
            "--url",
            &format!("http://127.0.0.1:{}", port),
        ])
        .stdout(handle)
        .stderr(errors)
        .stdin(std::process::Stdio::null())
        .spawn();
    let child = match child {
        Ok(c) => c,
        Err(e) => return (String::new(), e.to_string()),
    };
    let pid = child.id() as i32;
    let deadline = tc::now() + wait;
    let mut found = String::new();
    while tc::now() < deadline && found.is_empty() {
        std::thread::sleep(Duration::from_millis(400));
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        found = quick_url(&text);
        if found.is_empty() && !alive(pid) {
            break;
        }
    }
    if found.is_empty() {
        end(pid, libc::SIGTERM);
        return (
            String::new(),
            format!("no URL after {}s - see {}", wait as i64, log),
        );
    }
    let note = serde_json::json!({"pid": pid, "url": found, "port": port});
    let _ = std::fs::write(format!("{}/{}.json", dir, port), note.to_string());
    (found, String::new())
}

/// Whether a pid is still running. Signal 0 checks without delivering.
///
/// A zombie answers signal 0 and is not running: it is an exit status its
/// parent has not collected yet. Reporting one as alive would leave the
/// widget offering to SIGKILL something already dead, forever, since no
/// signal moves a zombie. Which file or command names that state is
/// acquisition; the parsers live in `parse.rs`.
fn alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } != 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return false;
    }
    !host::is_zombie(pid)
}

/// Whether this row can be signalled, and why not when it cannot.
///
/// Only one of the two is ever set. The checks are all about not doing
/// damage past what was asked for: a row whose owner /proc would not name
/// is somebody else's process, and a process in this widget's own group
/// cannot be group-killed without taking the widget down with it.
fn killable(row: &Row) -> (Option<i32>, String) {
    let pid = match row.pid {
        Some(p) => p,
        None => return (None, "not yours - no owner for this socket".into()),
    };
    if pid <= 1 {
        return (None, format!("refusing to signal pid {}", pid));
    }
    match host::ours(pid) {
        Ok(true) => (Some(pid), String::new()),
        Ok(false) => (None, format!("pid {} is not yours", pid)),
        Err(()) => (None, format!("pid {} is already gone", pid)),
    }
}

/// Signal the process group, falling back to the process alone.
///
/// A dev server is rarely one process: `npm run dev` is a shell, a package
/// manager and the server itself, sharing a process group precisely so that
/// Ctrl-C reaches all three. Signalling the group is what Ctrl-C does. The
/// fallback covers a process whose group we cannot read, and the guard
/// covers the case where the group is this widget's own.
fn end(pid: i32, sig: libc::c_int) -> String {
    let group = unsafe { libc::getpgid(pid) };
    let ours = unsafe { libc::getpgrp() };
    let sent = if group > 0 && group != ours {
        unsafe { libc::killpg(group, sig) }
    } else {
        unsafe { libc::kill(pid, sig) }
    };
    if sent == 0 {
        return String::new();
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => "already gone".into(),
        Some(libc::EPERM) => "not permitted".into(),
        _ => "failed".into(),
    }
}

/// Whether a command exists, so a key can say so instead of failing.
fn have(program: &str) -> bool {
    tc::missing(&[program]).is_empty()
}

// Tailscale accepts Funnel traffic on these three public ports and no
// others, so a node can have three funnels at once - not the one that
// defaulting to 443 every time would suggest.
const FUNNEL_PORTS: &[u16] = &[443, 8443, 10000];

/// The tailnet-side ports this node's serve config already occupies.
fn taken_ports(cfg: &serde_json::Value) -> Vec<u16> {
    let mut used = Vec::new();
    for key in cfg["TCP"].as_object().into_iter().flatten().map(|(k, _)| k) {
        if let Ok(port) = key.parse() {
            used.push(port);
        }
    }
    for mount in cfg["Web"].as_object().into_iter().flatten().map(|(k, _)| k) {
        if let Some((_, listen)) = mount.rsplit_once(':') {
            if let Ok(port) = listen.parse() {
                used.push(port);
            }
        }
    }
    used
}

/// The first public port free to funnel on, or 0 when all three are used.
fn free_funnel_port(cfg: &serde_json::Value) -> u16 {
    let used = taken_ports(cfg);
    FUNNEL_PORTS
        .iter()
        .copied()
        .find(|p| !used.contains(p))
        .unwrap_or(0)
}

/// What a subprocess said when it refused, on one line.
fn refusal(out: std::process::Output, fallback: &str) -> String {
    if out.status.success() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&out.stderr).to_string()
        + &String::from_utf8_lossy(&out.stdout).to_string();
    let joined: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        fallback.to_string()
    } else {
        joined.chars().take(200).collect()
    }
}

/// Put a local port behind this node's HTTPS name.
///
/// Serve listens on the port's own number rather than 443. Nothing forces
/// that, but 443 is where an unflagged `tailscale serve` lands, so leaving
/// it as the default would mean the second port published quietly took the
/// first one's mount.
///
/// Funnel has only the three ports Tailscale accepts from the internet, so
/// it takes the first of them that is free - a node can hold three at once,
/// and defaulting all of them to 443 would allow one.
fn serve_port(port: u16, public: bool) -> String {
    let mut listen = port;
    if public {
        listen = free_funnel_port(&serve_config());
        if listen == 0 {
            return format!(
                "all three funnel ports are in use ({}) - stop one first",
                FUNNEL_PORTS
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    let verb = if public { "funnel" } else { "serve" };
    let https = format!("--https={}", listen);
    let port = port.to_string();
    match tc::run_full(&["tailscale", verb, "--bg", &https, &port], RUN_TIMEOUT) {
        Ok(out) => refusal(out, "tailscale refused"),
        Err(e) => e,
    }
}

/// Which tailnet-side port a local port is currently published on.
fn listen_for(cfg: &serde_json::Value, port: u16) -> u16 {
    for (mount, entry) in cfg["Web"].as_object().into_iter().flatten() {
        for handler in entry["Handlers"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, v)| v)
        {
            if proxies_port(handler["Proxy"].as_str().unwrap_or(""), port) {
                return mount
                    .rsplit_once(':')
                    .and_then(|(_, l)| l.parse().ok())
                    .unwrap_or(0);
            }
        }
    }
    0
}

/// Take back one port, leaving every other mount as it was.
///
/// The mount to remove is looked up rather than assumed: it was chosen when
/// the port was published, and on a funnel that is whichever of the three
/// public ports happened to be free at the time.
///
/// Never `serve reset`: that clears the whole configuration, including
/// whatever was already published before this widget was ever run.
fn unserve_port(port: u16, public: bool) -> String {
    let mut listen = listen_for(&serve_config(), port);
    if listen == 0 {
        listen = if public { 443 } else { port };
    }
    let verb = if public { "funnel" } else { "serve" };
    let https = format!("--https={}", listen);
    match tc::run_full(&["tailscale", verb, &https, "off"], RUN_TIMEOUT) {
        Ok(out) => refusal(out, "tailscale refused"),
        Err(e) => e,
    }
}

/// What a kill would take down, in whatever room the pane has.
///
/// Everything here identifies the target, but not equally: the port is the
/// one thing the person is looking at, and the framework name without the
/// project it belongs to is no use on a machine running four of them. The
/// pid is the first to go, then the kind.
fn kill_label(row: &Row, room: usize) -> String {
    // An orphan's kind is the words "nothing listening", which reads badly
    // in the middle of a sentence about it. The port is the whole subject.
    if row.orphan {
        return format!(":{}", row.port);
    }
    let what = if row.kind.is_empty() {
        "unidentified"
    } else {
        &row.kind
    };
    let wherein = if row.project.is_empty() {
        String::new()
    } else {
        format!(" in {}", row.project)
    };
    // Not every row has a pid. A port Tailscale serves with nothing behind
    // it has none by definition, and one that exits while its screen is
    // open loses the one it had - both can still be the subject of a prompt.
    let who = row.pid.map_or(String::new(), |p| format!(" (pid {})", p));
    let bare = format!(":{}", row.port);
    for text in [
        format!("{}{} on :{}{}", what, wherein, row.port, who),
        format!("{}{} on :{}", what, wherein, row.port),
        format!(
            "{} on :{}",
            if row.project.is_empty() {
                what
            } else {
                &row.project
            },
            row.port
        ),
        bare.clone(),
    ] {
        if text.chars().count() <= room {
            return text;
        }
    }
    bare
}

/// A question and the key that answers it, fitted to the pane.
///
/// On one line where both fit, on two where they do not, because the key is
/// never the half that may be truncated: a prompt with its `[y]` pushed off
/// the right edge is a prompt nobody can act on. The wording after the key
/// has shorter forms for the same reason, longest first.
fn prompt(
    ask: Vec<(String, String)>,
    key: (String, String),
    options: &[&str],
    w: usize,
    dim: &str,
) -> Vec<String> {
    let answer = |room: usize| -> Vec<(String, String)> {
        let text = options
            .iter()
            .find(|o| key.1.chars().count() + o.chars().count() <= room)
            .copied()
            .unwrap_or("");
        vec![key.clone(), (dim.to_string(), text.to_string())]
    };
    let line = |parts: &[(String, String)]| -> String {
        let refs: Vec<(&str, String)> =
            parts.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        tc::seg(&refs, w - 1)
    };
    // The bare last form - "[y] yes", with no word on what anything else
    // does - is a fallback for a pane too narrow for two lines, not a thing
    // to choose while a second line is going spare.
    let keep = if options.len() > 1 {
        options[options.len() - 2]
    } else {
        options[options.len() - 1]
    };
    let asked: usize = ask.iter().map(|(_, t)| t.chars().count()).sum();
    let room = (w - 1).saturating_sub(asked + 2);
    if room >= key.1.chars().count() + keep.chars().count() {
        let mut parts = ask;
        parts.push((dim.to_string(), "  ".into()));
        parts.extend(answer(room));
        return vec![line(&parts)];
    }
    let mut second = vec![(dim.to_string(), " ".to_string())];
    second.extend(answer(w - 2));
    vec![line(&ask), line(&second)]
}

/// The verb and the cost of each exposure change, for the confirmation.
fn action(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "serve" => ("publish", "tailnet only"),
        "funnel" => ("publish publicly", "anyone with the URL"),
        "unserve" => ("stop serving", ""),
        "unfunnel" => ("stop the funnel", ""),
        "tunnel" => ("open a cloudflare tunnel", "anyone with the URL"),
        "untunnel" => ("close the cloudflare tunnel", ""),
        _ => ("kill", ""),
    }
}

/// Whether there is anything behind this row worth a second screen.
///
/// A process of ours carries a command line, a directory and an age that
/// the table has no room for, and any row at all can be given an address to
/// copy or an exposure to set up. Another user's socket carries none of
/// that: the four columns already say everything /proc will tell us, and
/// opening a screen to repeat them would be a screen that wastes a press.
fn has_detail(row: &Row) -> bool {
    row.pid.is_some() || row.orphan || !row.exposed.is_empty()
}

/// Break a long value across lines at spaces, then anywhere.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest: Vec<char> = text.chars().collect();
    while !rest.is_empty() && lines.len() < 4 {
        if rest.len() <= width {
            lines.push(rest.iter().collect());
            break;
        }
        let cut = rest[..(width + 1).min(rest.len())]
            .iter()
            .rposition(|c| *c == ' ')
            .filter(|c| *c > width / 2)
            .unwrap_or(width);
        lines.push(rest[..cut].iter().collect());
        rest = rest[cut..]
            .iter()
            .skip_while(|c| **c == ' ')
            .copied()
            .collect();
    }
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// One `label   value` line, wrapped under its own label.
fn field(label: &str, value: &str, w: usize, colour: &str, p: &Palette) -> Vec<String> {
    let label_w = 10usize;
    wrap(value, ((w - 3).saturating_sub(label_w)).max(8))
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            tc::seg(
                &[
                    (
                        p.dim.as_str(),
                        format!("  {}", tc::pad(if i == 0 { label } else { "" }, label_w)),
                    ),
                    (colour, line),
                ],
                w - 1,
            )
        })
        .collect()
}

/// The ways this port could be published, and why one is unavailable.
///
/// Each is a key, a name, and the state that decides whether pressing it
/// does anything. An option that cannot work says so on the line rather
/// than failing after the keypress - except Funnel, which is offered even
/// when the capability is missing, because Tailscale's own error names the
/// setting to change in the admin console better than this can.
fn expose_options(
    row: &Row,
    net: &Net,
    tunnel: &Option<Tunnel>,
) -> Vec<(char, &'static str, String, bool)> {
    let how = row.exposed.as_str();
    // One blocker outranks the others: without the operator bit every serve
    // and funnel write is refused, whatever else is true of them.
    let barred = if net.operator {
        ""
    } else {
        "needs: tailscale set --operator"
    };
    vec![
        (
            's',
            "tailscale serve",
            if how == "tailnet" {
                "serving · tailnet only".to_string()
            } else if barred.is_empty() {
                "tailnet only".to_string()
            } else {
                barred.to_string()
            },
            how == "tailnet",
        ),
        (
            't',
            "tailscale funnel",
            if how == "public" {
                "public · anyone with the URL".to_string()
            } else if !barred.is_empty() {
                barred.to_string()
            } else if net.funnel {
                "public".to_string()
            } else {
                "not enabled for this node".to_string()
            },
            how == "public",
        ),
        (
            'd',
            "cloudflare tunnel",
            match tunnel {
                Some(t) => format!("running · {}", t.url),
                None if have("cloudflared") => "quick tunnel, random domain".to_string(),
                None => "cloudflared not installed".to_string(),
            },
            tunnel.is_some(),
        ),
    ]
}

/// The second screen: everything known about one port, and what to do.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn detail_rows(
    row: &Row,
    net: &Net,
    tunnel: &Option<Tunnel>,
    links: &[(String, String)],
    sel: usize,
    seen: Option<&[(f64, f64, f64)]>,
    gap: f64,
    w: usize,
    p: &Palette,
) -> (Vec<String>, Option<usize>) {
    // Which row the selected address came out on, so the caller can keep it
    // in view. Nothing when there are no addresses to pick between.
    let mut cursor = None;
    let mut rows = vec![tc::title(&format!(":{}", row.port), w, &p.port)];
    let mut head = if !row.kind.is_empty() {
        row.kind.clone()
    } else if !row.user.is_empty() {
        format!("{}'s", row.user)
    } else {
        String::new()
    };
    if !row.project.is_empty() {
        head += &format!(" in {}", row.project);
    }
    rows.push(tc::seg(
        &[
            (p.txt.as_str(), format!(" {}", head.trim())),
            (
                p.dim.as_str(),
                match row.up {
                    Some(_) => format!("  ·  up {}", span(row.up)),
                    None => String::new(),
                },
            ),
        ],
        w - 1,
    ));
    rows.push(String::new());

    if let Some(pid) = row.pid {
        rows.push(tc::seg(
            &[(p.lbl.as_str(), " ── PROCESS ── ".into())],
            w - 1,
        ));
        let cmd = if row.cmdline.is_empty() {
            "?"
        } else {
            &row.cmdline
        };
        rows.extend(field("command", cmd, w, &p.txt, p));
        let cwd = if row.cwd.is_empty() { "?" } else { &row.cwd };
        let cwd_c = if row.gone { &p.warn } else { &p.txt };
        rows.extend(field("directory", cwd, w, cwd_c, p));
        let group = match unsafe { libc::getpgid(pid) } {
            g if g > 0 => format!(" · group {}", g),
            _ => String::new(),
        };
        rows.extend(field("pid", &format!("{}{}", pid, group), w, &p.txt, p));
        rows.push(String::new());
    }

    // A lone :: is not an IPv6-only server: Linux maps IPv4 onto it unless
    // the process asked for IPV6_V6ONLY, and /proc cannot say which it did.
    // Claiming "IPv6 only" here would be a guess dressed as a fact.
    let note = if row.families > 1 {
        "two sockets, IPv4 and IPv6"
    } else if row.bind == "::" {
        "IPv4 too, unless the server turned that off"
    } else {
        "one socket"
    };
    rows.push(tc::seg(
        &[
            (p.lbl.as_str(), " ── LISTENING ON ── ".into()),
            (
                p.txt.as_str(),
                if row.bind.is_empty() {
                    "nothing".into()
                } else {
                    row.bind.clone()
                },
            ),
            (p.dim.as_str(), format!("  {}", note)),
        ],
        w - 1,
    ));
    rows.push(String::new());
    // What has actually gone through it. The kernel's own per-socket byte
    // counters, summed over the connections accepted from this port - so
    // this is TCP, and a port serving anything else reads as quiet. A
    // missing ss is not quiet: it is unmeasured, and the chart would look
    // like nothing has moved.
    match seen {
        Some(series) => rows.extend(traffic_chart(series, "TRAFFIC", 3, gap, w, p)),
        None => rows.push(tc::seg(
            &[(p.dim.as_str(), " no traffic · needs ss".into())],
            w - 1,
        )),
    }
    rows.push(String::new());
    rows.push(tc::seg(
        &[
            (p.lbl.as_str(), " ── REACHABLE AT ── ".into()),
            (p.dim.as_str(), "↑↓ to pick, c copies".into()),
        ],
        w - 1,
    ));
    if links.is_empty() {
        rows.push(tc::seg(
            &[(p.dim.as_str(), "   nothing is listening to reach".into())],
            w - 1,
        ));
    }
    for (i, (url, note)) in links.iter().enumerate() {
        let here = i == sel;
        if here {
            cursor = Some(rows.len());
        }
        rows.push(tc::seg(
            &[
                (
                    if here {
                        p.accent.as_str()
                    } else {
                        p.dim.as_str()
                    },
                    if here { " ▸ ".into() } else { "   ".into() },
                ),
                (
                    if here { p.txt.as_str() } else { p.dim.as_str() },
                    url.clone(),
                ),
                (p.dim.as_str(), format!("  {}", note)),
            ],
            w - 1,
        ));
    }
    rows.push(String::new());
    rows.push(tc::seg(&[(p.lbl.as_str(), " ── EXPOSE ── ".into())], w - 1));
    for (key, name, state, on) in expose_options(row, net, tunnel) {
        rows.push(tc::seg(
            &[
                (p.accent.as_str(), format!("  [{}] ", key)),
                (p.txt.as_str(), tc::pad(name, 18)),
                (if on { p.ok.as_str() } else { p.dim.as_str() }, state),
            ],
            w - 1,
        ));
    }
    (rows, cursor)
}

/// Something to say at the bottom of the screen until a moment passes.
type Notice = (String, String, f64);

/// A SIGTERM that has been sent and not yet answered for.
struct Watch {
    pid: i32,
    row: Row,
    asked: bool,
    deadline: f64,
}

/// An exposure change running on a thread, so the frame keeps drawing.
struct Working {
    kind: String,
    row: Row,
    done: Arc<Mutex<Vec<Notice>>>,
}

/// The second screen's own state: which port, and which address is picked.
struct Detail {
    port: u16,
    row: Row,
    at: usize,
    links: Vec<(String, String)>,
    tunnel: Option<Tunnel>,
}

/// Carry out one exposure change and record how it went.
fn start_work(kind: &str, row: Row) -> Working {
    let done = Arc::new(Mutex::new(Vec::new()));
    let job = Arc::clone(&done);
    let (what, port) = (kind.to_string(), row.port);
    let p = rgb_ok();
    std::thread::spawn(move || {
        let said: Notice = match what.as_str() {
            "serve" | "funnel" => {
                let failed = serve_port(port, what == "funnel");
                if failed.is_empty() {
                    (
                        format!("{} now serves :{}", what, port),
                        p.ok,
                        tc::now() + 6.0,
                    )
                } else {
                    (failed, p.bad, tc::now() + 8.0)
                }
            }
            "unserve" | "unfunnel" => {
                let failed = unserve_port(port, what == "unfunnel");
                if failed.is_empty() {
                    (format!("stopped serving :{}", port), p.ok, tc::now() + 5.0)
                } else {
                    (failed, p.bad, tc::now() + 8.0)
                }
            }
            "tunnel" => {
                let (url, failed) = start_tunnel(port, 25.0);
                if failed.is_empty() {
                    (url, p.ok, tc::now() + 20.0)
                } else {
                    (failed, p.bad, tc::now() + 10.0)
                }
            }
            _ => {
                if let Some(t) = tunnel_state(port) {
                    end(t.pid, libc::SIGTERM);
                    forget_tunnel(port);
                }
                (
                    format!("closed the tunnel on :{}", port),
                    p.ok,
                    tc::now() + 5.0,
                )
            }
        };
        if let Ok(mut guard) = job.lock() {
            guard.push(said);
        }
    });
    Working {
        kind: kind.to_string(),
        row,
        done,
    }
}

/// The bottom of either screen.
///
/// One of five things, in the order they matter: a question that must be
/// answered before anything happens, the wait after answering it, a slow
/// action still running, the outcome of the last one, or the ordinary keys.
fn footer(
    confirm: &Option<(String, Row)>,
    watch: &Option<Watch>,
    working: &Option<Working>,
    notice: &Option<Notice>,
    w: usize,
    hints: &[Vec<(String, String)>],
    p: &Palette,
) -> Vec<String> {
    if let Some((kind, row)) = confirm {
        let (verb, cost) = action(kind);
        let mut ask = vec![
            (p.bad.clone(), format!(" {} ", verb)),
            (
                p.txt.clone(),
                kill_label(row, w.saturating_sub(34 + verb.len())),
            ),
        ];
        if !cost.is_empty() {
            ask.push((p.warn.clone(), format!(" - {}", cost)));
        }
        ask.push((p.dim.clone(), "?".into()));
        return prompt(
            ask,
            (p.warn.clone(), "[y]".into()),
            &[
                " yes  ·  any other key cancels",
                " yes  ·  any key cancels",
                " yes",
            ],
            w,
            &p.dim,
        );
    }
    if let Some(state) = watch {
        if state.asked {
            return prompt(
                vec![
                    (p.warn.clone(), " still up: ".into()),
                    (p.txt.clone(), kill_label(&state.row, w - 30)),
                ],
                (p.bad.clone(), "[f]".into()),
                &[
                    " force kill  ·  any other key leaves it",
                    " SIGKILL  ·  any key leaves it",
                    " SIGKILL",
                ],
                w,
                &p.dim,
            );
        }
        return vec![tc::seg(
            &[
                (p.dim.as_str(), " SIGTERM sent, waiting for ".into()),
                (p.txt.as_str(), kill_label(&state.row, w - 29)),
            ],
            w - 1,
        )];
    }
    if let Some(job) = working {
        let verb = action(&job.kind).0;
        return vec![tc::seg(
            &[(
                p.warn.as_str(),
                format!(" {} :{} - this can take a moment", verb, job.row.port),
            )],
            w - 1,
        )];
    }
    if let Some((text, colour, _)) = notice {
        return vec![tc::seg(&[(colour.as_str(), format!(" {}", text))], w - 1)];
    }
    let refs: Vec<Vec<(&str, String)>> = hints
        .iter()
        .map(|group| group.iter().map(|(c, t)| (c.as_str(), t.clone())).collect())
        .collect();
    tc::pack_hints(&refs, w - 2, "  ")
        .into_iter()
        .map(|l| format!(" {}", l))
        .collect()
}

struct Store {
    rows: Mutex<Vec<Row>>,
    /// Why the poller stopped, if it did. A caught panic used to be thrown
    /// away here: the scan returned an empty list and the table drew as if
    /// nothing were listening, which is the one thing this widget must
    /// never say by accident.
    err: Mutex<String>,
    /// What has moved on each listening port, sampled by the same poll that
    /// finds the ports. One `ss` call per scan rather than a second thread:
    /// a thread that dies is invisible, and this needs no finer resolution
    /// than the table it sits under.
    traffic: Mutex<Traffic>,
    // [r] asks for a scan now rather than at the end of the interval, and
    // so does anything that has just changed what a scan would find.
    wake: (Mutex<bool>, Condvar),
}

impl Store {
    fn wake(&self) {
        if let Ok(mut asked) = self.wake.0.lock() {
            *asked = true;
            self.wake.1.notify_all();
        }
    }
}

fn main() {
    tc::maybe_help(include_str!("help.txt"));
    // Both of ports' config keys were documented and read by nobody.
    // Config is the default; argv still overrides.
    let cfg = tc::load_config("ports");
    if let Some(listed) = configured_system_ports(&cfg) {
        let _ = CONFIGURED_PORTS.set(listed);
    }
    let mut refresh = tc::poll_secs(tc::cfg_f64(&cfg, "refresh", 4.0), 4.0).max(1.0);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        if (args[i] == "-n" || args[i] == "--refresh") && i + 1 < args.len() {
            refresh = tc::poll_secs(args[i + 1].parse().unwrap_or(4.0), 4.0).max(1.0);
            i += 2;
        } else {
            i += 1;
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        tc::cannot_start_because("dev servers", &tc::unsupported(), &[], "");
        return;
    }

    #[cfg(target_os = "macos")]
    {
        let absent = tc::missing(&["lsof"]);
        if !absent.is_empty() {
            tc::cannot_start(
                "dev servers",
                &absent,
                &[
                    "lsof names each listening socket and the process behind it.",
                    "It ships with macOS; without it this pane cannot list ports.",
                ],
                "",
            );
            return;
        }
    }

    // Traffic is ss. A Linux box without iproute2, or any Mac, has none:
    // the columns stay off and the header says so, rather than filling
    // with dots that look like a quiet port.
    let traffic_from_ss = have("ss");

    let ok = rgb_ok();
    let store = Arc::new(Store {
        rows: Mutex::new(Vec::new()),
        err: Mutex::new(String::new()),
        traffic: Mutex::new(Traffic::default()),
        wake: (Mutex::new(false), Condvar::new()),
    });
    let poller = Arc::clone(&store);
    std::thread::spawn(move || loop {
        // A thread that dies takes its explanation with it, so the scan is
        // caught - but catching it is only half of the rule. It used to end
        // in `unwrap_or_default()`, which handed the table an empty list and
        // drew a machine with nothing listening. The reason goes on screen
        // and the thread stops, the way agent-usage and herdr-panes do it.
        // A failed lsof is not an empty table: keep the last good scan and
        // put the reason on screen. The thread keeps going so the next poll
        // can recover.
        let found = match std::panic::catch_unwind(scan) {
            Ok(Ok(found)) => {
                if let Ok(mut guard) = poller.err.lock() {
                    if !guard.contains("poller stopped") {
                        guard.clear();
                    }
                }
                Some(found)
            }
            Ok(Err(why)) => {
                if let Ok(mut guard) = poller.err.lock() {
                    *guard = why;
                }
                None
            }
            Err(_) => {
                let why = "poller stopped - see the pane it was started from";
                if let Ok(mut guard) = poller.err.lock() {
                    *guard = why.into();
                }
                return;
            }
        };
        if let Some(found) = found {
            // The ports to tally against, taken from the scan that just ran, so
            // a port that has just appeared is measured from its next sample
            // rather than never.
            let listening: Vec<u16> = found.iter().filter(|r| !r.gone).map(|r| r.port).collect();
            let counters = if traffic_from_ss {
                match std::panic::catch_unwind(|| tc::run_quiet(&["ss", "-tine"], RUN_TIMEOUT)) {
                    Ok(text) => Some(text),
                    Err(_) => {
                        let why = "traffic poller stopped - the table below is still current";
                        if let Ok(mut guard) = poller.err.lock() {
                            *guard = why.into();
                        }
                        // Traffic is one column of many; the ports themselves are
                        // still being found, so this one says so and carries on.
                        // Do not sample an empty string: that records zeros and
                        // looks like a quiet port, then the next good poll has
                        // no baseline for the missed interval.
                        None
                    }
                }
            } else {
                None
            };
            if let Ok(mut guard) = poller.rows.lock() {
                *guard = found;
            }
            if let Some(counters) = counters {
                if let Ok(mut guard) = poller.traffic.lock() {
                    guard.sample(&counters, &listening, tc::now());
                }
            }
        }
        let (lock, cond) = &poller.wake;
        let mut asked = match lock.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if !*asked {
            asked = match cond.wait_timeout(asked, Duration::from_secs_f64(refresh)) {
                Ok((g, _)) => g,
                Err(_) => return,
            };
        }
        *asked = false;
    });

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let (mut selected, mut hide_system, mut scroll) = (0usize, true, 0usize);
    // The five things that can be happening besides the list: a question
    // waiting on a key, the pause after a SIGTERM, a slow action on a
    // thread, the outcome of the last one, and the second screen.
    let mut confirm: Option<(String, Row)> = None;
    let mut watch: Option<Watch> = None;
    let mut working: Option<Working> = None;
    let mut notice: Option<Notice> = None;
    let mut detail: Option<Detail> = None;
    // How far the detail screen has been scrolled. It had none: the body
    // was built at whatever height it needed and then truncated to the
    // pane, so everything past the bottom edge was dropped with nothing
    // saying so - on a short pane that is most of the screen.
    let mut dscroll = 0usize;
    // Whether a key has just moved a cursor - the address cursor on the
    // detail, the row cursor on the list. The wheel never sets either, so
    // neither view chases a selection the moment it is turned.
    let (mut dmoved, mut moved) = (false, false);
    // Tailscale is asked once per visit to the second screen rather than
    // once per frame: two subprocesses at 3Hz would cost more than the
    // whole rest of the widget. Any change made there clears them.
    let mut net: Option<(Net, serde_json::Value)> = None;

    loop {
        let (w, h) = tc::size();

        // A SIGTERM is given a moment to work before the harder question is
        // asked, because most things do stop.
        if let Some(state) = watch.as_mut() {
            if !state.asked && (!alive(state.pid) || tc::now() >= state.deadline) {
                if !alive(state.pid) {
                    notice = Some((
                        format!("stopped {}", kill_label(&state.row, w - 12)),
                        ok.ok.clone(),
                        tc::now() + 4.0,
                    ));
                    watch = None;
                    store.wake();
                } else {
                    state.asked = true;
                }
            }
        }

        // An action that talks to tailscaled or cloudflared takes seconds,
        // which is far too long to hold a frame for, so it runs on a thread
        // and its answer is collected here.
        if let Some(job) = working.as_ref() {
            let done = job.done.lock().ok().and_then(|g| g.first().cloned());
            if let Some(said) = done {
                notice = Some(said);
                working = None;
                net = None;
                store.wake();
            }
        }

        for key in keyboard.poll() {
            // Only an explicit yes acts. Every other key cancels,
            // deliberately including q: quitting must never double as
            // consent to signal something or publish it.
            if let Some((kind, row)) = confirm.take() {
                if key != "y" && key != "Y" {
                    notice = Some(("cancelled".into(), ok.dim.clone(), tc::now() + 2.0));
                    continue;
                }
                if kind == "kill" {
                    let (pid, why) = killable(&row);
                    let pid = match pid {
                        Some(p) => p,
                        None => {
                            notice = Some((why, ok.bad.clone(), tc::now() + 5.0));
                            continue;
                        }
                    };
                    let failed = end(pid, libc::SIGTERM);
                    if failed.is_empty() {
                        watch = Some(Watch {
                            pid,
                            row,
                            asked: false,
                            deadline: tc::now() + 3.0,
                        });
                    } else {
                        notice = Some((
                            format!(
                                "{}: {}",
                                kill_label(&row, w.saturating_sub(4 + failed.len())),
                                failed
                            ),
                            ok.bad.clone(),
                            tc::now() + 5.0,
                        ));
                        store.wake();
                    }
                } else if working.is_none() {
                    working = Some(start_work(&kind, row));
                }
                continue;
            }
            if watch.as_ref().is_some_and(|s| s.asked) {
                let state = watch.take().expect("just checked");
                if key == "f" || key == "F" {
                    let failed = end(state.pid, libc::SIGKILL);
                    notice = Some(if failed.is_empty() {
                        (
                            format!("SIGKILL sent to {}", kill_label(&state.row, w - 19)),
                            ok.warn.clone(),
                            tc::now() + 5.0,
                        )
                    } else {
                        (
                            format!(
                                "{}: {}",
                                kill_label(&state.row, w.saturating_sub(4 + failed.len())),
                                failed
                            ),
                            ok.bad.clone(),
                            tc::now() + 5.0,
                        )
                    });
                } else {
                    notice = Some((
                        format!("left running: {}", kill_label(&state.row, w - 17)),
                        ok.dim.clone(),
                        tc::now() + 3.0,
                    ));
                }
                store.wake();
                continue;
            }
            // The second screen keeps its own selection - of addresses
            // rather than rows - and hands every other key back.
            if let Some(view) = detail.as_mut() {
                match key.as_str() {
                    // Left and esc come out; q quits, which is what the
                    // footer beside it says and what q does everywhere
                    // else. backspace is gone: an alias no hint named.
                    "esc" | "left" => {
                        detail = None;
                    }
                    "q" | "Q" => {
                        keyboard.restore();
                        tc::restore_screen();
                        return;
                    }
                    // The arrows pick an address, which is what the footer
                    // beside them says. The wheel scrolls the screen they
                    // are on - two different things, and conflating them
                    // was why scrolling a detail did nothing but move the
                    // copy target.
                    "up" => {
                        view.at = view.at.saturating_sub(1);
                        dmoved = true;
                    }
                    "down" => {
                        view.at += 1;
                        dmoved = true;
                    }
                    "ctrl-y" | "wheel-up" => dscroll = dscroll.saturating_sub(1),
                    "ctrl-e" | "wheel-down" => dscroll = dscroll.saturating_add(1),
                    "c" | "C" => {
                        if !view.links.is_empty() {
                            let url = &view.links[view.at.min(view.links.len() - 1)].0;
                            // The address goes in the notice either way:
                            // OSC 52 is refused by some terminals and
                            // swallowed by some multiplexers, and a copy
                            // that silently did nothing would leave nothing
                            // on screen to read instead.
                            let copied = tc::clipboard(url);
                            notice = Some((
                                format!(
                                    "{}{}",
                                    if copied { "copied  " } else { "no clipboard  " },
                                    url
                                ),
                                if copied {
                                    ok.ok.clone()
                                } else {
                                    ok.warn.clone()
                                },
                                tc::now() + 8.0,
                            ));
                        }
                    }
                    "s" | "S" | "t" | "T" | "d" | "D" => {
                        let mut kind = match key.to_lowercase().as_str() {
                            "s" => "serve",
                            "t" => "funnel",
                            _ => "tunnel",
                        };
                        let how = view.row.exposed.as_str();
                        if kind == "serve" && how == "tailnet" {
                            kind = "unserve";
                        } else if kind == "funnel" && how == "public" {
                            kind = "unfunnel";
                        } else if kind == "tunnel" && view.tunnel.is_some() {
                            kind = "untunnel";
                        } else if kind == "tunnel" && !have("cloudflared") {
                            notice = Some((
                                "cloudflared is not installed - see the docs \
                                 for the one-line install"
                                    .into(),
                                ok.warn.clone(),
                                tc::now() + 8.0,
                            ));
                            continue;
                        }
                        if working.is_none() {
                            confirm = Some((kind.to_string(), view.row.clone()));
                        }
                    }
                    "r" | "R" => {
                        net = None;
                        store.wake();
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
                "up" => {
                    selected = selected.saturating_sub(1);
                    moved = true;
                }
                "down" => {
                    selected += 1;
                    moved = true;
                }
                // The wheel moves the list and leaves the cursor where it
                // is - selection is the arrows' job, here as everywhere.
                "ctrl-y" | "wheel-up" => scroll = scroll.saturating_sub(1),
                "ctrl-e" | "wheel-down" => scroll = scroll.saturating_add(1),
                // Hiding the system rows shortens the list under the
                // cursor, so the window has to come back to it - otherwise
                // the clamped cursor can end up above the window, invisible,
                // with enter still opening whatever it sits on.
                "o" | "O" => {
                    hide_system = !hide_system;
                    moved = true;
                }
                "r" | "R" => store.wake(),
                "enter" | "right" => {
                    let all: Vec<Row> = store.rows.lock().map(|g| g.clone()).unwrap_or_default();
                    let shown: Vec<Row> = all
                        .into_iter()
                        .filter(|r| !(hide_system && theirs(r)))
                        .collect();
                    if let Some(row) = shown.get(selected.min(shown.len().saturating_sub(1))) {
                        if has_detail(row) {
                            dmoved = true;
                            detail = Some(Detail {
                                port: row.port,
                                row: row.clone(),
                                at: 0,
                                links: Vec::new(),
                                tunnel: None,
                            });
                            net = None;
                        } else {
                            notice = Some((
                                "nothing more to show - /proc will not name \
                                 another user's process"
                                    .into(),
                                ok.dim.clone(),
                                tc::now() + 5.0,
                            ));
                        }
                    }
                }
                "k" | "K" => {
                    if watch.is_none() {
                        let all: Vec<Row> =
                            store.rows.lock().map(|g| g.clone()).unwrap_or_default();
                        let shown: Vec<Row> = all
                            .into_iter()
                            .filter(|r| !(hide_system && theirs(r)))
                            .collect();
                        if let Some(row) = shown.get(selected.min(shown.len().saturating_sub(1))) {
                            let (pid, why) = killable(row);
                            if pid.is_some() {
                                confirm = Some(("kill".into(), row.clone()));
                            } else {
                                notice = Some((why, ok.bad.clone(), tc::now() + 5.0));
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Rebuilt after the keys rather than before them, so that a press of
        // o is answered in the frame it was made in and not the next one.
        let all: Vec<Row> = store.rows.lock().map(|g| g.clone()).unwrap_or_default();
        let shown: Vec<&Row> = all.iter().filter(|r| !(hide_system && theirs(r))).collect();
        if !shown.is_empty() && selected >= shown.len() {
            selected = shown.len() - 1;
        }
        if notice.as_ref().is_some_and(|n| tc::now() >= n.2) {
            notice = None;
        }

        if let Some(view) = detail.as_mut() {
            if net.is_none() {
                net = Some((tailnet_self(), serve_config()));
            }
            let (self_node, cfg) = net.as_ref().expect("just filled");
            match all.iter().find(|r| r.port == view.port) {
                Some(live) => view.row = live.clone(),
                None => {
                    view.row.pid = None;
                    view.row.gone = true;
                }
            }
            view.tunnel = tunnel_state(view.port);
            view.links = addresses(&view.row, self_node, cfg);
            if let Some(t) = view.tunnel.as_ref() {
                view.links
                    .push((t.url.clone(), "public · cloudflare".to_string()));
            }
            view.at = view.at.min(view.links.len().saturating_sub(1));
            let seen = if traffic_from_ss {
                store
                    .traffic
                    .lock()
                    .map(|t| t.series(view.port))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let (rows, cursor) = detail_rows(
                &view.row,
                self_node,
                &view.tunnel,
                &view.links,
                view.at,
                if traffic_from_ss { Some(&seen) } else { None },
                refresh,
                w,
                &ok,
            );
            let foot = footer(
                &confirm,
                &watch,
                &working,
                &notice,
                w,
                &[
                    vec![
                        (ok.accent.clone(), "↑↓".into()),
                        (ok.dim.clone(), " address".into()),
                    ],
                    vec![(ok.dim.clone(), "[c]opy".into())],
                    vec![(ok.dim.clone(), "[s]erve".into())],
                    vec![(ok.dim.clone(), "[t]unnel".into())],
                    vec![(ok.dim.clone(), "[d] cloudflare".into())],
                    vec![
                        (ok.accent.clone(), "←".into()),
                        (ok.dim.clone(), "/esc back".into()),
                    ],
                    vec![(ok.dim.clone(), "[q]uit".into())],
                ],
                &ok,
            );
            let room = h.saturating_sub(foot.len() + 1);
            // A window onto the body rather than a cut of it. The title is
            // pinned above it: on a detail screen it is the only row that
            // says which port you opened, and scrolling it away leaves
            // nothing identifying what is on screen.
            let (head, rest) = rows.split_at(1.min(rows.len()));
            let room_below = room.saturating_sub(head.len()).max(1);
            // Only on the frame a key moved the address cursor: chasing it
            // every frame would drag the view back from wherever the wheel
            // put it. Before this the cursor was never chased at all, so
            // walking the addresses on a short pane moved the copy target
            // off the bottom with nothing on screen saying where it went.
            if dmoved {
                if let Some(at) = cursor {
                    dscroll = tc::follow(dscroll, at.saturating_sub(head.len()), room_below);
                }
                dmoved = false;
            }
            dscroll = dscroll.min(rest.len().saturating_sub(room_below));
            let last = (dscroll + room_below).min(rest.len());
            let mut rows: Vec<String> = head.to_vec();
            rows.extend_from_slice(&rest[dscroll..last]);
            while rows.len() < room {
                rows.push(String::new());
            }
            rows.extend(foot);
            tc::draw(&rows, w, h);
            std::thread::sleep(Duration::from_millis(300));
            continue;
        }

        let mine = all.iter().filter(|r| r.pid.is_some()).count();
        let off_box = all.iter().filter(|r| !r.exposed.is_empty()).count();

        let mut rows = vec![tc::title("dev servers", w, &ok.port)];
        rows.push(tc::seg(
            &[
                (ok.dim.as_str(), format!(" {} listening", all.len())),
                (ok.dim.as_str(), format!(" · {} yours", mine)),
                (ok.dim.as_str(), " · ".into()),
                (
                    if off_box > 0 { &ok.ok } else { &ok.dim },
                    format!("{} reachable off-box", off_box),
                ),
                (ok.dim.as_str(), format!("   every {}s", refresh as i64)),
            ],
            w - 1,
        ));
        if !traffic_from_ss {
            rows.push(tc::seg(
                &[(ok.dim.as_str(), " no traffic · needs ss".into())],
                w - 1,
            ));
        }
        // A dead poller says so, right under the counts it has stopped
        // updating. Same line, same shape and same words as herdr-panes and
        // agent-usage, so it reads the same wherever you meet it.
        let err = store.err.lock().map(|g| g.clone()).unwrap_or_default();
        if !err.is_empty() {
            rows.push(tc::seg(&[(ok.bad.as_str(), format!(" ! {}", err))], w - 1));
        }
        rows.push(String::new());

        let wide = w >= 78;
        let rates = if traffic_from_ss {
            store.traffic.lock().ok()
        } else {
            None
        };
        // Everything moving through every listening port, over time. The
        // table is what this widget is for, so the chart yields to it: it is
        // drawn only when there are rows to spare after the table, the
        // header and the footer have taken theirs.
        let totals = rates.as_ref().map(|t| t.totals()).unwrap_or_default();
        let spare = h
            .saturating_sub(rows.len() + 3 + shown.len().min(12) + 1)
            .min(3);
        if spare >= 3 && totals.iter().any(|(u, d, _)| *u > 0.0 || *d > 0.0) {
            rows.extend(traffic_chart(
                &totals,
                "EVERYTHING MOVING",
                spare / 2 + spare % 2,
                refresh,
                w,
                &ok,
            ));
            rows.push(String::new());
        }
        // The project column takes whatever the fixed ones leave: it is the
        // one that identifies the server, and the one whose contents are a
        // directory name of any length.
        // The WHAT column takes the widest name it has to show, plus a gap.
        // It used to be a flat eighteen with nothing after it, so a name of
        // exactly that length ran straight into the project and the two read
        // as one word - and anything longer was cut, which names a different
        // program. Sized to the whole list rather than the visible slice, so
        // the columns do not shift as it scrolls.
        // The traffic column is the first to go and the last to arrive: it
        // is the only one here not about what the port *is*. It arrives when
        // there is room for it *after* the names, rather than past some
        // number of columns picked in advance - the project column is the
        // one that gives, and a project's name cut in half is a different
        // project.
        let traffic_w = 13usize;
        // The shape column is the more decorative of the two, so it arrives
        // after the rates and leaves before them.
        let spark_cells = 14usize;
        let widest_project = shown
            .iter()
            .map(|r| {
                if r.project.is_empty() {
                    &r.user
                } else {
                    &r.project
                }
                .chars()
                .count()
            })
            .max()
            .unwrap_or(8)
            .clamp(8, 24);
        let (fits, shapes) = extra_columns(
            w,
            longest_kind(&shown),
            widest_project,
            traffic_w,
            spark_cells + 4,
        );
        let busy = rates.is_some() && fits;
        let sparks = rates.is_some() && shapes;
        let traffic_w = if busy { traffic_w } else { 0 };
        // Two cells of gap either side, the same as every other column here.
        let spark_w = if sparks { spark_cells + 4 } else { 0 };
        // Named from the samples actually shown, so it stays true while the
        // history is still filling.
        let spark_head = rates
            .as_ref()
            .map(|t| {
                let deepest = shown
                    .iter()
                    .map(|r| t.series(r.port))
                    .map(|s| s.iter().rev().take(spark_cells).map(|x| x.2).sum::<f64>())
                    .fold(0.0f64, f64::max);
                if deepest > 0.0 {
                    format!("LAST {}", over(deepest))
                } else {
                    "SHAPE".to_string()
                }
            })
            .unwrap_or_else(|| "SHAPE".to_string());
        let rest = 1 + 6 + 8 + 2 + 8 + traffic_w + spark_w + if wide { 6 + 8 } else { 0 };
        let kind_w = longest_kind(&shown).min((w - 1).saturating_sub(rest).max(4));
        let fixed = 1 + 6 + 8 + kind_w + 2 + traffic_w + spark_w + if wide { 6 + 8 } else { 0 };
        let name_w = std::cmp::max(8, (w - 1).saturating_sub(fixed));
        rows.push(tc::seg(
            &[
                (ok.dim.as_str(), "  PORT  BIND    ".into()),
                (ok.dim.as_str(), format!("{}  ", tc::pad("WHAT", kind_w))),
                (ok.dim.as_str(), tc::pad("PROJECT", name_w)),
                (
                    ok.dim.as_str(),
                    if busy {
                        tc::pad("TRAFFIC", traffic_w)
                    } else {
                        String::new()
                    },
                ),
                // The window the shapes cover, named rather than left to be
                // guessed at - a sparkline without one is a shape.
                (
                    ok.dim.as_str(),
                    // The same two-cell gap the cells carry, so the heading
                    // sits over the shapes rather than two left of them.
                    if sparks {
                        format!("  {}  ", tc::pad(&spark_head, spark_cells))
                    } else {
                        String::new()
                    },
                ),
                (
                    ok.dim.as_str(),
                    if wide {
                        "UP    EXPOSED".into()
                    } else {
                        String::new()
                    },
                ),
            ],
            w - 1,
        ));

        let visible = std::cmp::max(1, h.saturating_sub(rows.len() + 3));
        // Only on the frame a key moved the cursor. Chasing it every frame
        // pulls the list back to the selection the instant the wheel moves
        // it, which reads as the wheel doing nothing at all.
        if moved {
            scroll = tc::follow(scroll, selected, visible);
            moved = false;
        }
        scroll = std::cmp::min(scroll, shown.len().saturating_sub(visible));

        for (i, row) in shown.iter().enumerate().skip(scroll).take(visible) {
            let here = i == selected;
            let tint = if here {
                tc::bg(28, 44, 62)
            } else {
                String::new()
            };
            let (note, note_colour) = bind_note(row, &ok);
            // Another user's row names its owner rather than its project,
            // which it has none of that we can read. That is the whole of
            // what is knowable about it.
            let who = if !row.project.is_empty() {
                row.project.clone()
            } else if !row.user.is_empty() {
                row.user.clone()
            } else if row.pid.is_some() {
                "—".into()
            } else {
                String::new()
            };
            let port_colour = format!("{}{}", tint, if here { &ok.accent } else { &ok.port });
            let note_c = format!("{}{}", tint, note_colour);
            let kind_c = format!(
                "{}{}",
                tint,
                if row.guessed || row.kind.is_empty() {
                    &ok.dim
                } else {
                    &ok.txt
                }
            );
            let who_c = format!(
                "{}{}",
                tint,
                if row.gone {
                    &ok.warn
                } else if !row.user.is_empty() {
                    &ok.dim
                } else {
                    &ok.txt
                }
            );
            // Nothing rather than "0 B/s" on a quiet port: a column of
            // zeroes down the table reads as a measurement that has failed,
            // and this one is just a port nobody is calling.
            let (up, down) = rates
                .as_ref()
                .and_then(|t| t.rate(row.port))
                .unwrap_or((0.0, 0.0));
            let moving = up > 0.0 || down > 0.0;
            // Each direction only when it has moved. A port serving a
            // download reads "↑820K", not "↑820K ↓0B" - the second half
            // would be four cells saying nothing happened.
            let traffic_text = if busy && moving {
                [(up, "↑"), (down, "↓")]
                    .into_iter()
                    .filter(|(v, _)| *v > 0.0)
                    .map(|(v, arrow)| format!("{}{}", arrow, brief(v)))
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                String::new()
            };
            let traffic_c = format!("{}{}", tint, if moving { &ok.open } else { &ok.dim });
            // Built before the row, because every segment's colour has to
            // outlive the borrows the row is assembled from.
            let shape: Vec<(String, String)> = if sparks {
                let mut out = vec![(format!("{}{}", tint, ok.dim), "  ".to_string())];
                let seen = rates
                    .as_ref()
                    .map(|t| t.series(row.port))
                    .unwrap_or_default();
                for (colour, text) in spark(&seen, spark_cells, &ok) {
                    out.push((format!("{}{}", tint, colour), text));
                }
                out.push((format!("{}{}", tint, ok.dim), "  ".to_string()));
                out
            } else {
                Vec::new()
            };
            let mut line = vec![
                (
                    port_colour.as_str(),
                    format!("{}{:<6}", if here { "▸" } else { " " }, row.port),
                ),
                (note_c.as_str(), format!("{:<8}", note)),
                (kind_c.as_str(), format!("{}  ", tc::pad(&row.kind, kind_w))),
                (who_c.as_str(), tc::pad(&who, name_w)),
            ];
            if busy {
                line.push((traffic_c.as_str(), tc::pad(&traffic_text, traffic_w)));
            }
            for (colour, text) in &shape {
                line.push((colour.as_str(), text.clone()));
            }
            let up_c = format!("{}{}", tint, ok.dim);
            let exp_c = format!(
                "{}{}",
                tint,
                match row.exposed.as_str() {
                    "tailnet" => &ok.ok,
                    "public" => &ok.bad,
                    _ => &ok.grid,
                }
            );
            if wide {
                line.push((up_c.as_str(), format!("{:<6}", span(row.up))));
                line.push((
                    exp_c.as_str(),
                    if row.exposed.is_empty() {
                        "-".into()
                    } else {
                        row.exposed.clone()
                    },
                ));
            }
            if here {
                line.push((tint.as_str(), " ".repeat(w)));
            }
            rows.push(tc::seg(&line, w - 1));
        }

        let foot = footer(
            &confirm,
            &watch,
            &working,
            &notice,
            w,
            &[
                vec![
                    (ok.accent.clone(), "↑↓".into()),
                    (ok.dim.clone(), " select".into()),
                ],
                vec![
                    (ok.accent.clone(), "→/↵".into()),
                    (ok.dim.clone(), " details".into()),
                ],
                vec![(ok.dim.clone(), "[k]ill".into())],
                vec![(
                    ok.dim.clone(),
                    format!("[o]{} system", if hide_system { "show" } else { "hide" }),
                )],
                vec![(ok.dim.clone(), "[r]efresh".into())],
                vec![(ok.dim.clone(), "[q]uit".into())],
            ],
            &ok,
        );
        while rows.len() < h.saturating_sub(foot.len() + 1) {
            rows.push(String::new());
        }
        rows.extend(foot);
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(300));
    }
}

struct Palette {
    ok: String,
    lbl: String,
    warn: String,
    bad: String,
    dim: String,
    grid: String,
    txt: String,
    accent: String,
    port: String,
    open: String,
    local: String,
}

fn rgb_ok() -> Palette {
    Palette {
        ok: tc::rgb(90, 240, 160),
        lbl: tc::rgb(130, 165, 200),
        warn: tc::rgb(255, 200, 90),
        bad: tc::rgb(255, 100, 110),
        dim: tc::rgb(127, 147, 172),
        grid: tc::rgb(60, 78, 98),
        txt: tc::rgb(225, 235, 245),
        accent: tc::rgb(150, 210, 255),
        port: tc::rgb(160, 220, 255),
        open: tc::rgb(255, 170, 120),
        local: tc::rgb(120, 200, 160),
    }
}

/// What the bound address means for who can reach it.
///
/// The address itself is too wide for the column - a tailnet IPv6 address
/// is 24 characters - and is rarely the answer to the question being asked.
fn bind_note(row: &Row, p: &Palette) -> (String, String) {
    if row.orphan {
        return ("--".into(), p.dim.clone());
    }
    let reach = bind_class(&row.bind);
    match reach.as_str() {
        "all" => ("all".into(), p.open.clone()),
        "local" => ("local".into(), p.local.clone()),
        "tailnet" => ("tailnet".into(), p.accent.clone()),
        other => (other.chars().take(7).collect(), p.txt.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two sockets in the shape `ss -tine` prints them: the addresses and
    /// the inode on one line, the counters on an indented continuation.
    fn ss_dump(sent_a: u64, sent_b: u64, ino_b: &str) -> String {
        format!(
            "State  Recv-Q Send-Q Local Address:Port  Peer Address:Port\n\
             ESTAB  0      0      192.0.2.7:3000      192.0.2.9:51234 ino:111 sk:1\n\
             \t ts sack cubic bytes_sent:{} bytes_received:40 segs_out:9\n\
             ESTAB  0      0      192.0.2.7:9999      192.0.2.9:51235 ino:{} sk:2\n\
             \t ts sack cubic bytes_sent:{} bytes_received:70 segs_out:9\n",
            sent_a, ino_b, sent_b
        )
    }

    #[test]
    fn a_sockets_counters_are_read_off_its_continuation_line() {
        let got = socket_counters(&ss_dump(1000, 2000, "222"));
        assert_eq!(got.len(), 2);
        // The *local* port, because that is the one a listener is on.
        assert_eq!(
            got["111"],
            Counters {
                port: 3000,
                sent: 1000,
                recv: 40
            }
        );
        assert_eq!(
            got["222"],
            Counters {
                port: 9999,
                sent: 2000,
                recv: 70
            }
        );
    }

    #[test]
    fn only_a_socket_seen_twice_can_say_what_moved() {
        let before = socket_counters(&ss_dump(1000, 2000, "222"));
        let after = socket_counters(&ss_dump(1500, 2000, "222"));
        let moved_now = moved(&before, &after, &[3000, 9999]);
        assert_eq!(moved_now.get(&3000), Some(&(500, 0)));
        assert_eq!(moved_now.get(&9999), Some(&(0, 0)));

        // A socket with no previous reading contributes nothing rather than
        // its whole lifetime total in one sample. Stated on a port that
        // *also* has a socket carrying over, because a new connection to a
        // busy port is when this actually happens - and because asserting
        // it on an empty result would pass whether or not it were true.
        let mut fresh = after.clone();
        fresh.insert(
            "333".into(),
            Counters {
                port: 3000,
                sent: 8_000_000,
                recv: 8_000_000,
            },
        );
        let moved_now = moved(&before, &fresh, &[3000, 9999]);
        assert_eq!(
            moved_now.get(&3000),
            Some(&(500, 0)),
            "a connection seen for the first time reported its whole life as one sample"
        );

        // Ports nothing is listening on are not tallied at all. Most
        // established sockets are outbound and their local port is an
        // ephemeral number that belongs to nothing.
        let moved_now = moved(&before, &after, &[3000]);
        assert!(!moved_now.contains_key(&9999));
    }

    #[test]
    fn a_reused_inode_does_not_report_a_lifetime_as_one_sample() {
        // Inodes come back after a socket closes. The new socket's counters
        // start again, so subtracting the old ones is meaningless - and
        // subtracting a larger number from a smaller one would wrap.
        let before = socket_counters(&ss_dump(1000, 9_000_000, "222"));
        // Same inode, now on a different port: a different socket.
        let after = socket_counters(&ss_dump(1000, 5, "222"))
            .into_iter()
            .map(|(k, mut c)| {
                if k == "222" {
                    c.port = 4444;
                }
                (k, c)
            })
            .collect();
        let moved_now = moved(&before, &after, &[3000, 4444, 9999]);
        assert_eq!(moved_now.get(&4444), None, "a reused inode was counted");

        // And on the same port, a counter that went backwards clamps rather
        // than wrapping to sixteen exabytes.
        let after = socket_counters(&ss_dump(1000, 5, "222"));
        let moved_now = moved(&before, &after, &[3000, 9999]);
        assert_eq!(moved_now.get(&9999), Some(&(0, 0)));
    }

    #[test]
    fn a_rate_is_measured_against_the_gap_that_actually_happened() {
        // [r] polls early. A rate divided by the nominal interval would
        // read high every time somebody pressed it.
        let mut t = Traffic::default();
        t.sample(&ss_dump(1000, 0, "222"), &[3000], 100.0);
        // The first sample has nothing to subtract from and so is not a
        // reading at all.
        assert_eq!(t.rate(3000), None);
        assert!(t.series(3000).is_empty());

        // Two seconds later, a thousand bytes: five hundred a second.
        t.sample(&ss_dump(3000, 0, "222"), &[3000], 102.0);
        assert_eq!(t.rate(3000), Some((1000.0, 0.0)));
        // Half a second later, the same thousand: two thousand a second.
        t.sample(&ss_dump(4000, 0, "222"), &[3000], 102.5);
        assert_eq!(t.rate(3000), Some((2000.0, 0.0)));
        // Each sample carries the gap it was measured over, so a chart of
        // the last few can say how much history they are.
        let seen = t.series(3000);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen.iter().map(|s| s.2).sum::<f64>(), 2.5);
    }

    #[test]
    fn a_wider_pane_never_shows_fewer_columns_than_a_narrower_one() {
        // TRAFFIC used to arrive at seventy-six and leave again at
        // seventy-eight, where UP and EXPOSED turn up and take the room -
        // one fact traded for another as the pane grew.
        let (kind_w, project_w, traffic_w, spark_w) = (23usize, 22usize, 13usize, 18usize);
        let mut had = (false, false);
        for w in 40..220usize {
            let now = extra_columns(w, kind_w, project_w, traffic_w, spark_w);
            assert!(
                now.0 >= had.0 && now.1 >= had.1,
                "w={} lost a column the narrower pane had: {:?} then {:?}",
                w,
                had,
                now
            );
            // The shapes are the more decorative of the two and never
            // arrive on their own.
            assert!(!now.1 || now.0, "shapes without rates at w={}", w);
            had = now;
        }
        // Both ends: nothing in a narrow pane, both in a wide one.
        assert_eq!(
            extra_columns(60, kind_w, project_w, traffic_w, spark_w),
            (false, false)
        );
        assert_eq!(
            extra_columns(210, kind_w, project_w, traffic_w, spark_w),
            (true, true)
        );
    }

    #[test]
    fn the_totals_ring_has_one_entry_per_sample_whether_or_not_anything_moved() {
        let mut t = Traffic::default();
        t.sample(&ss_dump(1000, 0, "222"), &[3000, 9999], 100.0);
        // The first sample is not a reading, here as anywhere.
        assert!(t.totals().is_empty());

        // Quiet second.
        t.sample(&ss_dump(1000, 0, "222"), &[3000, 9999], 101.0);
        // Busy second, on both ports at once.
        t.sample(&ss_dump(1500, 400, "222"), &[3000, 9999], 102.0);
        let totals = t.totals();
        assert_eq!(totals.len(), 2, "a quiet sample still takes a column");
        assert_eq!(totals[0].0, 0.0);
        // Summed across ports: five hundred on one, four hundred on the
        // other, in one second.
        assert_eq!(totals[1].0, 900.0);

        // Bounded like the per-port rings, or a chart that keeps one column
        // per sample grows for as long as the widget is up.
        for i in 0..(TRAFFIC_KEPT + 20) {
            t.sample(
                &ss_dump(2000 + i as u64, 0, "222"),
                &[3000],
                200.0 + i as f64,
            );
        }
        assert_eq!(t.totals().len(), TRAFFIC_KEPT);
    }

    #[test]
    fn a_sparkline_keeps_unmeasured_and_quiet_apart() {
        let p = rgb_ok();
        let plain = |cells: Vec<(String, String)>| -> String {
            cells.into_iter().map(|(_, t)| t).collect()
        };

        // Nothing sampled at all: every cell dotted.
        assert_eq!(plain(spark(&[], 6, &p)), "······");

        // Sampled and quiet: a baseline, which is a different thing and has
        // to look like one - the port's own screen makes the same
        // distinction one keypress away.
        let quiet: Vec<(f64, f64, f64)> = (0..6).map(|_| (0.0, 0.0, 1.0)).collect();
        assert_eq!(plain(spark(&quiet, 6, &p)), "──────");

        // Partly sampled: dots for what is missing, then the rest.
        let some: Vec<(f64, f64, f64)> = (0..2).map(|_| (0.0, 0.0, 1.0)).collect();
        assert_eq!(
            plain(spark(&some, 6, &p)),
            "········".chars().take(4).collect::<String>() + "──"
        );

        // Scaled to its own peak, not to some other row's: the biggest
        // sample here is full height whatever its absolute size.
        let small: Vec<(f64, f64, f64)> = vec![(1.0, 0.0, 1.0), (8.0, 0.0, 1.0)];
        let big: Vec<(f64, f64, f64)> = vec![(1e6, 0.0, 1.0), (8e6, 0.0, 1.0)];
        assert_eq!(plain(spark(&small, 2, &p)), plain(spark(&big, 2, &p)));
        assert!(
            plain(spark(&small, 2, &p)).ends_with('█'),
            "the peak is full height"
        );
    }

    #[test]
    fn a_port_that_stops_listening_is_forgotten_and_the_ring_is_bounded() {
        let mut t = Traffic::default();
        for i in 0..(TRAFFIC_KEPT + 40) {
            t.sample(&ss_dump(i as u64 * 10, 0, "222"), &[3000], 100.0 + i as f64);
        }
        assert_eq!(t.series(3000).len(), TRAFFIC_KEPT);
        // Nothing listens on it any more: its history goes with it rather
        // than growing for as long as the widget is up.
        t.sample(&ss_dump(9999, 0, "222"), &[], 500.0);
        assert!(t.series(3000).is_empty());
        assert_eq!(t.rate(3000), None);
    }

    #[test]
    fn a_binary_living_under_a_runtimes_path_is_not_that_runtime() {
        // Through `kind_of`, not the helper alone: an earlier version of
        // this test passed with the table still calling `contains`, which
        // is the bug it was written for.
        //
        // A real one from this machine: a standalone browser binary that
        // npm installed into node_modules. It is not written in Node and
        // the widget said "Node".
        let under = "/home/u/.nvm/versions/node/v24.18.0/lib/node_modules/agent-browser/bin/agent-browser-linux-x64";
        assert_eq!(kind_of(under, 37397).0, "agent-browser-linux-x64");
        assert_eq!(
            kind_of("/srv/python-tools/bin/collector", 9000).0,
            "collector"
        );

        // The runtimes themselves still answer to their names.
        assert_eq!(kind_of("/usr/bin/node server.js", 3000).0, "Node");
        assert_eq!(kind_of("/usr/bin/python3 app.py", 8000).0, "Python");

        // And the specific frameworks still win over the runtime, which is
        // the order the table is written in.
        assert_eq!(kind_of("node /app/node_modules/.bin/vite", 5173).0, "Vite");
    }

    #[test]
    fn a_runtime_is_named_only_when_it_is_the_thing_being_run() {
        // The command really is the runtime.
        assert!(runs_command("/usr/bin/node server.js", "node"));
        assert!(runs_command("node server.js", "node"));
        assert!(runs_command("/usr/bin/python3 app.py", "python"));
        assert!(runs_command("/usr/bin/python3.11 -m http.server", "python"));

        // A directory on the way to something else is not. This is the
        // whole bug: a standalone binary installed under node_modules was
        // listed as "Node", the runtime it is not written in.
        assert!(!runs_command("/usr/lib/node_modules/x/bin/mytool", "node"));
        assert!(!runs_command(
            "/home/u/.nvm/versions/node/v24/bin/agent-browser-linux-x64",
            "node"
        ));
        assert!(!runs_command("/srv/python-tools/bin/collector", "python"));
        // And a name that merely starts the same way is a different name.
        assert!(!runs_command("/usr/bin/nodemon app.js", "node"));
        assert!(!runs_command("/usr/bin/python-config", "python"));
    }

    #[test]
    fn ipv6_comes_out_of_little_endian_words() {
        // ::1 as /proc/net/tcp6 writes it: four words, each reversed.
        assert_eq!(hex_addr("00000000000000000000000001000000"), "::1");
        assert_eq!(hex_addr("00000000000000000000000000000000"), "::");
        // A tailnet address, taken from this machine's own /proc/net/tcp6
        // and checked against what ss prints for the same socket, rather
        // than assembled by hand - the first attempt at that was wrong in
        // a way that made a correct decoder look broken.
        assert_eq!(
            hex_addr("5C117AFD0000E0A100000000686338DE"),
            "fd7a:115c:a1e0::de38:6368"
        );
    }

    #[test]
    fn ipv4_comes_out_of_little_endian_hex() {
        // 0100007F is 127.0.0.1 the way /proc writes it.
        assert_eq!(hex_addr("0100007F"), "127.0.0.1");
        assert_eq!(hex_addr("00000000"), "0.0.0.0");
    }

    #[test]
    fn reachability_is_classified_not_printed() {
        assert_eq!(bind_class("0.0.0.0"), "all");
        assert_eq!(bind_class("::"), "all");
        assert_eq!(bind_class("127.0.0.1"), "local");
        assert_eq!(bind_class("::1"), "local");
        assert_eq!(bind_class("100.64.0.102"), "tailnet");
        assert_eq!(bind_class("fd7a:115c:a1e0::1"), "tailnet");
        // A LAN address is its own answer, not one of the three.
        assert_eq!(bind_class("192.168.1.9"), "192.168.1.9");
        assert_eq!(bind_class("10.0.0.46"), "10.0.0.46");
    }

    #[test]
    fn tailnet_range_stops_where_it_should() {
        assert!(is_tailnet_v4("100.64.0.1"));
        assert!(is_tailnet_v4("100.127.255.254"));
        assert!(!is_tailnet_v4("100.63.0.1"));
        assert!(!is_tailnet_v4("100.128.0.1"));
    }

    #[test]
    fn a_version_in_the_title_is_kept() {
        let (kind, guessed) = kind_of("next-server (v16.3.1)", 3000);
        assert_eq!(kind, "Next.js 16.3.1");
        assert!(!guessed);
    }

    #[test]
    fn a_port_number_alone_is_marked_as_a_guess() {
        let (kind, guessed) = kind_of("", 443);
        assert_eq!(kind, "HTTPS?");
        assert!(guessed, "a guess from a port number must say so");
    }

    #[test]
    fn proxy_lines_give_up_their_ports() {
        let json = r#"{"Web":{"host:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:4100"}}}}}"#;
        assert_eq!(proxied_ports(json), vec![4100]);
    }

    #[test]
    fn a_package_name_beats_the_directory() {
        assert_eq!(
            json_string(r#"{"name": "piaf-web", "version": "1.0"}"#, "name"),
            Some("piaf-web".into())
        );
    }

    #[test]
    fn a_proxy_target_names_a_whole_port() {
        // The Python matches `:3000` or `:3000/` and not `:30001`. A plain
        // substring search would report a served port that is not served,
        // and the detail screen would offer a URL that answers nothing.
        assert!(proxies_port("http://127.0.0.1:3000", 3000));
        assert!(proxies_port("http://127.0.0.1:3000/", 3000));
        assert!(proxies_port("http://127.0.0.1:3000/app", 3000));
        assert!(!proxies_port("http://127.0.0.1:30001", 3000));
        assert!(!proxies_port("http://127.0.0.1:13000", 3000));
    }

    #[test]
    fn the_serve_url_carries_its_mount() {
        let cfg: serde_json::Value = serde_json::from_str(
            r#"{"Web": {"host.ts.net:443": {"Handlers": {"/":
               {"Proxy": "http://127.0.0.1:3003"}}}}}"#,
        )
        .unwrap();
        // 443 is the default and is left off; anything else is spelled out.
        assert_eq!(served_url(&cfg, 3003), "https://host.ts.net/");
        assert_eq!(served_url(&cfg, 3004), "");
        let other: serde_json::Value = serde_json::from_str(
            r#"{"Web": {"host.ts.net:8443": {"Handlers": {"/":
               {"Proxy": "http://127.0.0.1:3003"}}}}}"#,
        )
        .unwrap();
        assert_eq!(served_url(&other, 3003), "https://host.ts.net:8443/");
        assert_eq!(listen_for(&other, 3003), 8443);
    }

    #[test]
    fn a_funnel_takes_the_first_port_that_is_free() {
        // Tailscale accepts funnel traffic on three ports, so a node can
        // hold three at once. Defaulting to 443 every time would allow one.
        let cfg: serde_json::Value =
            serde_json::from_str(r#"{"Web": {"host.ts.net:443": {"Handlers": {}}}}"#).unwrap();
        assert_eq!(free_funnel_port(&cfg), 8443);
        let full: serde_json::Value =
            serde_json::from_str(r#"{"Web": {"h:443": {}, "h:8443": {}, "h:10000": {}}}"#).unwrap();
        assert_eq!(free_funnel_port(&full), 0);
    }

    #[test]
    fn a_kill_prompt_gives_up_its_parts_in_order() {
        let row = Row {
            port: 3000,
            kind: "Next.js 16.3.1".into(),
            project: "piaf-web".into(),
            pid: Some(4242),
            ..Default::default()
        };
        let full = "Next.js 16.3.1 in piaf-web on :3000 (pid 4242)";
        assert_eq!(kill_label(&row, 99), full);
        // The pid is the first thing to go, then the kind - the port is the
        // one thing the person is actually looking at.
        assert_eq!(
            kill_label(&row, full.len() - 1),
            "Next.js 16.3.1 in piaf-web on :3000"
        );
        assert_eq!(kill_label(&row, 25), "piaf-web on :3000");
        assert_eq!(kill_label(&row, 8), ":3000");
        // An orphan's kind is the words "nothing listening", which reads
        // badly in the middle of a sentence about it.
        let orphan = Row {
            port: 4100,
            orphan: true,
            ..Default::default()
        };
        assert_eq!(kill_label(&orphan, 99), ":4100");
    }

    #[test]
    fn a_long_value_wraps_at_a_space_when_there_is_one() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        // A word with no break in it is cut rather than dropped.
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 8), vec![""]);
    }

    #[test]
    fn only_a_row_with_something_behind_it_opens() {
        assert!(has_detail(&Row {
            pid: Some(7),
            ..Default::default()
        }));
        assert!(has_detail(&Row {
            orphan: true,
            ..Default::default()
        }));
        assert!(has_detail(&Row {
            exposed: "tailnet".into(),
            ..Default::default()
        }));
        // Another user's socket: the four columns already say everything
        // /proc will tell us, so a second screen would waste the press.
        assert!(!has_detail(&Row {
            user: "root".into(),
            ..Default::default()
        }));
    }

    #[test]
    fn a_quick_tunnel_url_is_read_out_of_the_log() {
        let log = "INF +--------------------------------------+\n\
                   INF |  https://calm-fox-runs.trycloudflare.com  |\n";
        assert_eq!(quick_url(log), "https://calm-fox-runs.trycloudflare.com");
        assert_eq!(quick_url("INF starting tunnel"), "");
    }

    #[test]
    fn an_empty_system_port_list_is_not_the_defaults() {
        // Absent means the built-in list. Present and empty means hide
        // nothing. The two used to collapse because `[]` was read as
        // unset, so `"system_ports": []` kept hiding 22 and 53.
        assert!(configured_system_ports(&serde_json::json!({})).is_none());
        assert_eq!(
            configured_system_ports(&serde_json::json!({"system_ports": []})),
            Some(vec![])
        );
        // A number that is not a port is dropped, not wrapped. 65558 as
        // u16 is 22, and hiding SSH for a typo is worse than ignoring it.
        assert_eq!(
            configured_system_ports(&serde_json::json!({"system_ports": [22, 65558, 65535]})),
            Some(vec![22, 65535])
        );
    }

    #[test]
    fn spans_read_as_a_person_would_say_them() {
        assert_eq!(span(Some(45.0)), "45s");
        // The unit changes at the unit, not half again past it.
        assert_eq!(span(Some(90.0)), "1m");
        assert_eq!(span(Some(5000.0)), "1h");
        assert_eq!(span(Some(600.0)), "10m");
        assert_eq!(span(Some(7200.0)), "2h");
        assert_eq!(span(Some(200_000.0)), "2d");
        assert_eq!(span(None), "--");
    }
}
