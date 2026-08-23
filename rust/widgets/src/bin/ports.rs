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
use std::sync::{Arc, Condvar, Mutex};
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

/// Seconds before an external command is given up on, from ports.py's longest, on the serve/funnel commands.
const RUN_TIMEOUT: u64 = 30;

fn run(args: &[&str]) -> String {
    // Bounded: .output() waits forever, and a wedged child used to freeze
    // the poll thread with the pane still showing its last frame.
    tc::run(args, RUN_TIMEOUT).unwrap_or_default()
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
    SYSTEM_PORTS.contains(&row.port) || !row.user.is_empty()
}

/// Every address this machine holds, by interface.
///
/// Link-local is dropped: an fe80:: address needs a zone index to be usable
/// and is never what somebody wants pasted into a browser.
fn interfaces() -> Vec<(String, String, bool)> {
    let mut found = Vec::new();
    let data: serde_json::Value =
        serde_json::from_str(&run(&["ip", "-j", "addr"])).unwrap_or(serde_json::Value::Null);
    for link in data.as_array().unwrap_or(&Vec::new()) {
        let name = link["ifname"].as_str().unwrap_or("?").to_string();
        for addr in link["addr_info"].as_array().unwrap_or(&Vec::new()) {
            let ip = addr["local"].as_str().unwrap_or("");
            if ip.is_empty() || ip.starts_with("fe80:") {
                continue;
            }
            found.push((
                name.clone(),
                ip.to_string(),
                addr["family"].as_str() == Some("inet6"),
            ));
        }
    }
    found
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
    let data: serde_json::Value = serde_json::from_str(&run(&["tailscale", "status", "--json"]))
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
    let prefs: serde_json::Value = serde_json::from_str(&run(&["tailscale", "debug", "prefs"]))
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
    serde_json::from_str(&run(&["tailscale", "serve", "status", "--json"]))
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
    let path = format!("{}/terminal-toys/tunnels", base);
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
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
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
    let deadline = now() + wait;
    let mut found = String::new();
    while now() < deadline && found.is_empty() {
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
/// signal moves a zombie. /proc knows the difference.
fn alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return false;
    }
    match std::fs::read_to_string(format!("/proc/{}/stat", pid)) {
        Ok(text) => match text.rsplit_once(')') {
            Some((_, rest)) => rest.split_whitespace().next() != Some("Z"),
            None => true,
        },
        Err(_) => true,
    }
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
        None => {
            return (
                None,
                "not yours - no owner for this socket in /proc".into(),
            )
        }
    };
    if pid <= 1 {
        return (None, format!("refusing to signal pid {}", pid));
    }
    match std::fs::metadata(format!("/proc/{}", pid)) {
        Ok(meta) => {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != unsafe { libc::getuid() } {
                return (None, format!("pid {} is not yours", pid));
            }
        }
        Err(_) => return (None, format!("pid {} is already gone", pid)),
    }
    (Some(pid), String::new())
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
        for handler in entry["Handlers"].as_object().into_iter().flatten().map(|(_, v)| v) {
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
        let refs: Vec<(&str, String)> = parts
            .iter()
            .map(|(c, t)| (c.as_str(), t.clone()))
            .collect();
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
        rest = rest[cut..].iter().skip_while(|c| **c == ' ').copied().collect();
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
fn detail_rows(
    row: &Row,
    net: &Net,
    tunnel: &Option<Tunnel>,
    links: &[(String, String)],
    sel: usize,
    w: usize,
    p: &Palette,
) -> Vec<String> {
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
        rows.push(tc::seg(&[(p.lbl.as_str(), " ── PROCESS ── ".into())], w - 1));
        let cmd = if row.cmdline.is_empty() { "?" } else { &row.cmdline };
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
        rows.push(tc::seg(
            &[
                (
                    if here { p.accent.as_str() } else { p.dim.as_str() },
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
    rows
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
                        now() + 6.0,
                    )
                } else {
                    (failed, p.bad, now() + 8.0)
                }
            }
            "unserve" | "unfunnel" => {
                let failed = unserve_port(port, what == "unfunnel");
                if failed.is_empty() {
                    (format!("stopped serving :{}", port), p.ok, now() + 5.0)
                } else {
                    (failed, p.bad, now() + 8.0)
                }
            }
            "tunnel" => {
                let (url, failed) = start_tunnel(port, 25.0);
                if failed.is_empty() {
                    (url, p.ok, now() + 20.0)
                } else {
                    (failed, p.bad, now() + 10.0)
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
                    now() + 5.0,
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
        .map(|group| {
            group
                .iter()
                .map(|(c, t)| (c.as_str(), t.clone()))
                .collect()
        })
        .collect();
    tc::pack_hints(&refs, w - 2, "  ")
        .into_iter()
        .map(|l| format!(" {}", l))
        .collect()
}

struct Store {
    rows: Mutex<Vec<Row>>,
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
        wake: (Mutex::new(false), Condvar::new()),
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
    // Tailscale is asked once per visit to the second screen rather than
    // once per frame: two subprocesses at 3Hz would cost more than the
    // whole rest of the widget. Any change made there clears them.
    let mut net: Option<(Net, serde_json::Value)> = None;

    loop {
        let (w, h) = tc::size();

        // A SIGTERM is given a moment to work before the harder question is
        // asked, because most things do stop.
        if let Some(state) = watch.as_mut() {
            if !state.asked && (!alive(state.pid) || now() >= state.deadline) {
                if !alive(state.pid) {
                    notice = Some((
                        format!("stopped {}", kill_label(&state.row, w - 12)),
                        ok.ok.clone(),
                        now() + 4.0,
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
                    notice = Some(("cancelled".into(), ok.dim.clone(), now() + 2.0));
                    continue;
                }
                if kind == "kill" {
                    let (pid, why) = killable(&row);
                    let pid = match pid {
                        Some(p) => p,
                        None => {
                            notice = Some((why, ok.bad.clone(), now() + 5.0));
                            continue;
                        }
                    };
                    let failed = end(pid, libc::SIGTERM);
                    if failed.is_empty() {
                        watch = Some(Watch {
                            pid,
                            row,
                            asked: false,
                            deadline: now() + 3.0,
                        });
                    } else {
                        notice = Some((
                            format!(
                                "{}: {}",
                                kill_label(&row, w.saturating_sub(4 + failed.len())),
                                failed
                            ),
                            ok.bad.clone(),
                            now() + 5.0,
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
                            now() + 5.0,
                        )
                    } else {
                        (
                            format!(
                                "{}: {}",
                                kill_label(&state.row, w.saturating_sub(4 + failed.len())),
                                failed
                            ),
                            ok.bad.clone(),
                            now() + 5.0,
                        )
                    });
                } else {
                    notice = Some((
                        format!("left running: {}", kill_label(&state.row, w - 17)),
                        ok.dim.clone(),
                        now() + 3.0,
                    ));
                }
                store.wake();
                continue;
            }
            // The second screen keeps its own selection - of addresses
            // rather than rows - and hands every other key back.
            if let Some(view) = detail.as_mut() {
                match key.as_str() {
                    "esc" | "left" | "q" | "Q" | "backspace" => {
                        detail = None;
                    }
                    "up" => view.at = view.at.saturating_sub(1),
                    "down" => view.at += 1,
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
                                if copied { ok.ok.clone() } else { ok.warn.clone() },
                                now() + 8.0,
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
                                now() + 8.0,
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
                "up" => selected = selected.saturating_sub(1),
                "down" => selected += 1,
                "o" | "O" => hide_system = !hide_system,
                "r" | "R" => store.wake(),
                "enter" | "right" | "i" | "I" => {
                    let all: Vec<Row> = store.rows.lock().map(|g| g.clone()).unwrap_or_default();
                    let shown: Vec<Row> = all
                        .into_iter()
                        .filter(|r| !(hide_system && theirs(r)))
                        .collect();
                    if let Some(row) = shown.get(selected.min(shown.len().saturating_sub(1))) {
                        if has_detail(row) {
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
                                now() + 5.0,
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
                                notice = Some((why, ok.bad.clone(), now() + 5.0));
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
        let shown: Vec<&Row> = all
            .iter()
            .filter(|r| !(hide_system && theirs(r)))
            .collect();
        if !shown.is_empty() && selected >= shown.len() {
            selected = shown.len() - 1;
        }
        if notice.as_ref().is_some_and(|n| now() >= n.2) {
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
            let mut rows = detail_rows(
                &view.row,
                self_node,
                &view.tunnel,
                &view.links,
                view.at,
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
                    vec![(ok.accent.clone(), "↑↓".into()), (ok.dim.clone(), " address".into())],
                    vec![(ok.dim.clone(), "[c]opy".into())],
                    vec![(ok.dim.clone(), "[s]erve".into())],
                    vec![(ok.dim.clone(), "[t]unnel".into())],
                    vec![(ok.dim.clone(), "[d] cloudflare".into())],
                    vec![(ok.accent.clone(), "esc".into()), (ok.dim.clone(), " back".into())],
                ],
                &ok,
            );
            let room = h.saturating_sub(foot.len() + 1);
            rows.truncate(room);
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

        let foot = footer(
            &confirm,
            &watch,
            &working,
            &notice,
            w,
            &[
                vec![(ok.accent.clone(), "↑↓".into()), (ok.dim.clone(), " select".into())],
                vec![(ok.accent.clone(), "↵".into()), (ok.dim.clone(), " details".into())],
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
