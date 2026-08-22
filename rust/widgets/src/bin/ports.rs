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

//! What is listening on this machine, what started it, and who can reach it.
//!
//! A port of ports.py. Same sources - the kernel's socket table for the
//! ports, each process's own cmdline and cwd for the rest - and deliberately
//! the same behaviour on screen, so the two can be compared side by side
//! while the rest of the collection is translated.
//!
//!     ports [-n SECONDS]
//!
//! Keys: up/down select, o hides the machine's own ports, r refreshes,
//! q quits.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use toys_core as tc;

const SYSTEM_PORTS: &[u16] = &[22, 53, 123, 323, 631, 5353];

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
    ("python", "Python"),
    ("node", "Node"),
];

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

// cmdline, cwd and families are carried for the detail screen, which is
// the next thing to be ported; the table itself does not read them.
#[allow(dead_code)]
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

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
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

struct Socket {
    port: u16,
    bind: String,
    inode: String,
    uid: u32,
}

/// Every listening TCP socket, from the kernel's own table.
fn listening() -> Vec<Socket> {
    let mut out = Vec::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for line in text.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 10 || cols[3] != "0A" {
                continue;
            }
            let (addr, port) = match cols[1].rsplit_once(':') {
                Some(pair) => pair,
                None => continue,
            };
            let port = match u16::from_str_radix(port, 16) {
                Ok(p) => p,
                Err(_) => continue,
            };
            out.push(Socket {
                port,
                bind: hex_addr(addr),
                inode: cols[9].to_string(),
                // The uid is in the table even where the process behind it
                // is not reachable, which is the difference between
                // "somebody else's" and "a mystery".
                uid: cols[7].parse().unwrap_or(0),
            });
        }
    }
    out
}

/// inode -> pid, for every process this user can read.
///
/// Root's sockets are not readable, so sshd and the like arrive unowned.
/// That is stated on screen rather than papered over.
fn socket_owners() -> HashMap<String, i32> {
    let mut owners = HashMap::new();
    let entries = match std::fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return owners,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let pid: i32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let fds = match std::fs::read_dir(format!("/proc/{}/fd", pid)) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                let target = target.to_string_lossy();
                if let Some(rest) = target.strip_prefix("socket:[") {
                    owners.insert(rest.trim_end_matches(']').to_string(), pid);
                }
            }
        }
    }
    owners
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

fn process_info(pid: i32) -> (String, String, Option<f64>) {
    let cmdline = std::fs::read(format!("/proc/{}/cmdline", pid))
        .map(|raw| {
            String::from_utf8_lossy(&raw)
                .replace('\0', " ")
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let cwd = std::fs::read_link(format!("/proc/{}/cwd", pid))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    // The link resolves even when the directory is gone; the kernel just
    // marks it, and that marker is worth keeping.
    let deleted = std::fs::metadata(format!("/proc/{}/cwd", pid)).is_err() && !cwd.is_empty();
    let cwd = if deleted && !cwd.ends_with("(deleted)") {
        format!("{} (deleted)", cwd)
    } else {
        cwd
    };
    let started = std::fs::metadata(format!("/proc/{}", pid))
        .ok()
        .and_then(|m| m.created().or_else(|_| m.modified()).ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64());
    (cmdline, cwd, started)
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
        for (needle, name) in KINDS {
            if cmdline.contains(needle) {
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

fn run(args: &[&str]) -> String {
    match std::process::Command::new(args[0]).args(&args[1..]).output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        _ => String::new(),
    }
}

/// Ports Tailscale is serving, and whether the world can see them.
fn exposure() -> HashMap<u16, String> {
    let mut served = HashMap::new();
    let text = run(&["tailscale", "serve", "status", "--json"]);
    for port in proxied_ports(&text) {
        served.insert(port, "tailnet".to_string());
    }
    let funnel = run(&["tailscale", "funnel", "status"]);
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
fn scan() -> Vec<Row> {
    let owners = socket_owners();
    let served = exposure();
    let mut rows: Vec<Row> = Vec::new();
    let mut services: HashMap<(u16, Option<i32>, String), usize> = HashMap::new();
    let mut seen: Vec<u16> = Vec::new();
    let stamp = now();

    for sock in listening() {
        let pid = owners.get(&sock.inode).copied();
        // A server on both address families is two sockets in the kernel
        // table but one thing to know about. Any of port, owner or
        // reachability differing is a real second row.
        let key = (sock.port, pid, bind_class(&sock.bind));
        if let Some(&at) = services.get(&key) {
            rows[at].families += 1;
            continue;
        }
        let (cmdline, cwd, started) = match pid {
            Some(pid) => process_info(pid),
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
    rows.sort_by_key(|r| (SYSTEM_PORTS.contains(&r.port), r.port));
    rows
}

fn span(seconds: Option<f64>) -> String {
    let s = match seconds {
        Some(s) if s >= 0.0 => s,
        _ => return "--".into(),
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

/// Whether a row is part of the machine rather than something you started.
fn theirs(row: &Row) -> bool {
    if row.orphan {
        return false;
    }
    SYSTEM_PORTS.contains(&row.port) || !row.user.is_empty()
}

struct Store {
    rows: Mutex<Vec<Row>>,
}

fn main() {
    tc::maybe_help(include_str!("ports_help.txt"));
    let mut refresh = 4.0f64;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        if (args[i] == "-n" || args[i] == "--refresh") && i + 1 < args.len() {
            refresh = args[i + 1].parse::<f64>().unwrap_or(4.0).max(1.0);
            i += 2;
        } else {
            i += 1;
        }
    }

    let ok = rgb_ok();
    let store = Arc::new(Store {
        rows: Mutex::new(Vec::new()),
    });
    let poller = Arc::clone(&store);
    std::thread::spawn(move || loop {
        // A thread that dies takes its explanation with it, so the scan is
        // caught rather than left to unwind: an empty table would look
        // exactly like a machine with nothing listening.
        let found = std::panic::catch_unwind(scan).unwrap_or_default();
        if let Ok(mut guard) = poller.rows.lock() {
            *guard = found;
        }
        std::thread::sleep(Duration::from_secs_f64(refresh));
    });

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let (mut selected, mut hide_system, mut scroll) = (0usize, true, 0usize);

    loop {
        for key in keyboard.poll() {
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "up" => selected = selected.saturating_sub(1),
                "down" => selected += 1,
                "o" | "O" => hide_system = !hide_system,
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let all: Vec<Row> = store.rows.lock().map(|g| g.clone()).unwrap_or_default();
        let shown: Vec<&Row> = all
            .iter()
            .filter(|r| !(hide_system && theirs(r)))
            .collect();
        if !shown.is_empty() && selected >= shown.len() {
            selected = shown.len() - 1;
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
        rows.push(String::new());

        let wide = w >= 78;
        // The project column takes whatever the fixed ones leave: it is the
        // one that identifies the server, and the one whose contents are a
        // directory name of any length.
        let fixed = 1 + 6 + 8 + 18 + if wide { 6 + 8 } else { 0 };
        let name_w = std::cmp::max(8, (w - 1).saturating_sub(fixed));
        rows.push(tc::seg(
            &[
                (ok.dim.as_str(), "  PORT  BIND    WHAT              ".into()),
                (ok.dim.as_str(), tc::pad("PROJECT", name_w)),
                (
                    ok.dim.as_str(),
                    if wide { "UP    EXPOSED".into() } else { String::new() },
                ),
            ],
            w - 1,
        ));

        let visible = std::cmp::max(1, h.saturating_sub(rows.len() + 3));
        if selected < scroll {
            scroll = selected;
        } else if selected >= scroll + visible {
            scroll = selected - visible + 1;
        }
        scroll = std::cmp::min(scroll, shown.len().saturating_sub(visible));

        for (i, row) in shown.iter().enumerate().skip(scroll).take(visible) {
            let here = i == selected;
            let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
            let (note, note_colour) = bind_note(row, &ok);
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
            let mut line = vec![
                (
                    port_colour.as_str(),
                    format!("{}{:<6}", if here { "▸" } else { " " }, row.port),
                ),
                (note_c.as_str(), format!("{:<8}", note)),
                (kind_c.as_str(), tc::pad(&row.kind, 18)),
                (who_c.as_str(), tc::pad(&who, name_w)),
            ];
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

        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(ok.accent.as_str(), "↑↓".into()), (ok.dim.as_str(), " select".into())],
            vec![(
                ok.dim.as_str(),
                format!("[o]{} system", if hide_system { "show" } else { "hide" }),
            )],
            vec![(ok.dim.as_str(), "[r]efresh".into())],
            vec![(ok.dim.as_str(), "[q]uit".into())],
        ];
        let foot: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
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
        assert_eq!(bind_class("100.89.99.102"), "tailnet");
        assert_eq!(bind_class("fd7a:115c:a1e0::1"), "tailnet");
        // A LAN address is its own answer, not one of the three.
        assert_eq!(bind_class("192.168.1.9"), "192.168.1.9");
        assert_eq!(bind_class("10.240.0.46"), "10.240.0.46");
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
    fn spans_read_as_a_person_would_say_them() {
        assert_eq!(span(Some(45.0)), "45s");
        assert_eq!(span(Some(600.0)), "10m");
        assert_eq!(span(Some(7200.0)), "2h");
        assert_eq!(span(Some(200_000.0)), "2d");
        assert_eq!(span(None), "--");
    }
}
