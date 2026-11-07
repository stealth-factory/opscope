//! The per-process files under `/proc`, and the socket table `ss` prints.
//!
//! Several widgets read the same few files - `/proc/<pid>/stat`, `io`,
//! `cgroup`, `fd/` - and each used to parse them its own way. One of those
//! ways multiplied resident pages by a hardcoded 4096, which is wrong on a
//! 16 KiB-page kernel. They are read here once, the same way, for all of
//! them.
//!
//! The split is the repo's usual one, inside a single file:
//!
//! - every `parse_*` takes text and is compiled and tested on every target,
//!   so the macOS CI run proves the Linux parsers too;
//! - the per-pid file readers (`pids`, `read_pid`, `read_stat`, ...) are
//!   plain `std::fs` reads and also compile everywhere. Where there is no
//!   `/proc` they return `Err` naming the path, never a default, so a
//!   widget calling them on a Mac sees an error rather than an empty
//!   process;
//! - the socket and file-descriptor walks netwatch and ports share, and
//!   anything else that has a macOS counterpart in a widget's own
//!   `macos.rs`, sit in the `linux` module below, gated by `cfg(target_os)`.
//!
//! An unreadable file is `Err("<path>: <io error>")`. Not `Option`, not zero:
//! a process whose `io` cannot be read has not done zero I/O.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Seconds before an external command (`ip`, `getent`) is given up on.
const RUN_TIMEOUT: u64 = 5;

// ── parsers: compiled and tested on every target ─────────────────────

/// The fields of `/proc/<pid>/stat` widgets read, numbered as in
/// `proc_pid_stat(5)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PidStat {
    /// Field 1.
    pub pid: i32,
    /// Field 2, as the kernel gives it: at most 15 bytes, no brackets, and
    /// no fallback to argv. `process_name()` is the one that tries harder.
    pub comm: String,
    /// Field 3: `R`, `S`, `D`, `Z`, ...
    pub state: char,
    /// Field 4. A kernel thread's parent is 2.
    pub ppid: i32,
    /// Field 14, in clock ticks (`clock_ticks()`).
    pub utime: u64,
    /// Field 15, in clock ticks.
    pub stime: u64,
    /// Field 19: -20 (most favoured) to 19.
    pub nice: i64,
    /// Field 20.
    pub num_threads: i64,
    /// Field 22: when the process started, in clock ticks after boot.
    pub starttime: u64,
    /// Field 24, in pages - `rss_bytes()` turns it into bytes.
    pub rss_pages: u64,
    /// Field 42. `None` when the kernel is too old to print it. A zero is
    /// a zero here; whether it means accounting is off is for the caller to
    /// decide from `/proc/sys/kernel/task_delayacct`.
    pub delayacct_blkio_ticks: Option<u64>,
}

/// One `/proc/<pid>/stat` line.
///
/// The command name sits in brackets and may itself contain spaces and
/// brackets, so the fields after it are counted from the *last* `)`.
pub fn parse_pid_stat(text: &str) -> Option<PidStat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let pid = text.get(..open)?.trim().parse().ok()?;
    let comm = text.get(open + 1..close)?.to_string();
    // Index n here is field n + 3 of proc_pid_stat(5).
    let rest: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    let num = |i: usize| -> Option<u64> { rest.get(i)?.parse().ok() };
    Some(PidStat {
        pid,
        comm,
        state: rest.first()?.chars().next()?,
        ppid: rest.get(1)?.parse().ok()?,
        utime: num(11)?,
        stime: num(12)?,
        nice: rest.get(16)?.parse().ok()?,
        num_threads: rest.get(17)?.parse().ok()?,
        starttime: num(19)?,
        rss_pages: num(21)?,
        delayacct_blkio_ticks: num(39),
    })
}

/// The five byte counters of `/proc/<pid>/io`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PidIo {
    pub rchar: u64,
    pub wchar: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub cancelled_write_bytes: u64,
}

/// `/proc/<pid>/io`. `None` when the text holds none of the five counters,
/// so a file that said nothing is never read as a process that did nothing.
pub fn parse_proc_pid_io(text: &str) -> Option<PidIo> {
    let mut io = PidIo::default();
    let mut any = false;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Ok(n) = value.trim().parse() else {
            continue;
        };
        let slot = match key.trim() {
            "rchar" => &mut io.rchar,
            "wchar" => &mut io.wchar,
            "read_bytes" => &mut io.read_bytes,
            "write_bytes" => &mut io.write_bytes,
            "cancelled_write_bytes" => &mut io.cancelled_write_bytes,
            _ => continue,
        };
        *slot = n;
        any = true;
    }
    any.then_some(io)
}

/// The control group a process is in, from `/proc/<pid>/cgroup`.
///
/// cgroup v2 has one line, `0::<path>`. On v1 there is a line per
/// hierarchy, and the systemd one (`name=systemd`) is the one that names
/// the unit; failing that, the first line's path. Empty text is an empty
/// path.
pub fn parse_proc_pid_cgroup(text: &str) -> String {
    let path_of = |line: &str| line.splitn(3, ':').nth(2).unwrap_or("").to_string();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if let Some(v2) = lines.iter().find(|l| l.starts_with("0::")) {
        return path_of(v2);
    }
    if let Some(systemd) = lines
        .iter()
        .find(|l| l.split(':').nth(1).is_some_and(|c| c.split(',').any(|c| c == "name=systemd")))
    {
        return path_of(systemd);
    }
    lines.first().map(|l| path_of(l)).unwrap_or_default()
}

/// Systemd names the slice, not the thing in it.
const SLICES: &[&str] = &["system.slice", "user.slice", "init.scope", "-.slice", "app.slice"];

/// Who owns a socket, from the cgroup the kernel already reports.
///
/// Another user's /proc is closed, but `ss` prints the control group for
/// every socket regardless, and on a systemd machine that names the unit.
pub fn unit_name(cgroup: &str) -> String {
    for part in cgroup.trim_matches('/').split('/').rev() {
        if part.is_empty() || SLICES.contains(&part) {
            continue;
        }
        let mut name = part;
        for suffix in [".service", ".scope", ".slice"] {
            if let Some(base) = name.strip_suffix(suffix) {
                name = base;
                break;
            }
        }
        // A login session is a person, not a program.
        if name.starts_with("session-") || name.starts_with("user-") {
            continue;
        }
        return name.to_string();
    }
    String::new()
}

/// `/proc/uptime`: seconds since boot, the first of its two fields.
pub fn parse_proc_uptime(text: &str) -> Option<f64> {
    text.split_whitespace().next()?.parse().ok()
}

/// `/proc/self/mountinfo`: whether `/proc` is mounted with a `hidepid`
/// that hides other users' processes from listing (`invisible`, `2`, or
/// `ptraceable`, `4`). `noaccess` (`1`) still lists them, so the total is
/// still every process and the word `visible` would be wrong.
pub fn parse_hidepid(mountinfo: &str) -> bool {
    mountinfo.lines().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(dash) = fields.iter().position(|f| *f == "-") else {
            return false;
        };
        let mount_point = fields.get(4).copied().unwrap_or("");
        let fstype = fields.get(dash + 1).copied().unwrap_or("");
        if mount_point != "/proc" || fstype != "proc" {
            return false;
        }
        let options = fields[5..].iter().chain(fields.get(dash + 3).into_iter());
        options
            .flat_map(|opts| opts.split(','))
            .filter_map(|opt| opt.strip_prefix("hidepid="))
            .any(|v| matches!(v, "2" | "invisible" | "4" | "ptraceable"))
    })
}

/// How long a process has been running: uptime minus its `starttime`,
/// which is in clock ticks after boot.
pub fn age_seconds(uptime: f64, starttime: u64, hz: f64) -> f64 {
    uptime - starttime as f64 / hz
}

/// One TCP socket from `ss -tine`.
#[derive(Clone, Debug, PartialEq)]
pub struct SocketRecord {
    pub inode: String,
    pub sent: u64,
    pub recv: u64,
    pub peer: String,
    pub port: u16,
    pub mine: u16,
    pub cgroup: String,
    /// Smoothed round-trip time in milliseconds, from `rtt:<ms>/<var>`.
    /// `None` when the counter line does not carry one - never 0.
    pub rtt: Option<f64>,
}

/// The value after `key:` on a line, up to the next space.
pub fn field(line: &str, key: &str) -> Option<String> {
    let at = line.find(key)? + key.len();
    let rest = &line[at..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let value = &rest[..end];
    (!value.is_empty()).then(|| value.to_string())
}

/// Addresses from `ip -o addr`, without their prefix length.
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

/// `rtt:<ms>/<var>` off a counter line. Matched as a whole token, because
/// `minrtt:` also contains `rtt:` and would otherwise be read in its place.
fn rtt_of(line: &str) -> Option<f64> {
    line.split_whitespace()
        .find_map(|token| token.strip_prefix("rtt:"))
        .and_then(|value| value.split('/').next()?.parse().ok())
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
            rtt: rtt_of(line),
        });
    }
    found
}

/// Whether this traffic never leaves the machine.
pub fn local_peer(host: &str, own: &[String]) -> bool {
    if host.starts_with("127.") || host == "::1" || host.is_empty() || host == "*" {
        return true;
    }
    let bare = host.strip_prefix("::ffff:").unwrap_or(host);
    own.iter().any(|a| a == bare)
}

/// Whether a peer is out on the internet rather than nearby.
pub fn off_box(host: &str, own: &[String]) -> bool {
    if local_peer(host, own) {
        return false;
    }
    let h = host.strip_prefix("::ffff:").unwrap_or(host);
    if h.starts_with("10.") || h.starts_with("192.168.") || h.starts_with("169.254.") {
        return false;
    }
    if h.starts_with("172.") {
        if let Some(second) = h.split('.').nth(1).and_then(|s| s.parse::<u16>().ok()) {
            if (16..=31).contains(&second) {
                return false;
            }
        }
    }
    if let Some(second) = h.strip_prefix("100.").and_then(|r| r.split('.').next()) {
        if let Ok(n) = second.parse::<u16>() {
            if (64..=127).contains(&n) {
                return false;
            }
        }
    }
    !(h.starts_with("fd7a:115c:a1e0")
        || h.starts_with("fe80:")
        || h.starts_with("fc")
        || h.starts_with("fd"))
}

/// A regular file a process holds open, and how big it is now.
#[derive(Clone, Debug)]
pub struct OpenFile {
    pub path: String,
    pub size: u64,
}

/// Command line and working directory - what a table has no room for.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub cmdline: String,
    pub cwd: String,
}

// ── per-pid file reads: plain std::fs, compiled everywhere ───────────

/// Resident bytes from `rss_pages`, at this kernel's page size.
pub fn rss_bytes(pages: u64) -> u64 {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    // sysconf answers -1 only for a name it does not know; 4096 then is a
    // guess, and the one every kernel this runs on used before 16 KiB pages.
    pages * if page > 0 { page as u64 } else { 4096 }
}

/// Clock ticks per second, the unit of `utime`, `stime` and `starttime`.
pub fn clock_ticks() -> f64 {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    // Only when sysconf itself fails: 100 is USER_HZ on every Linux there
    // is, so the guess is right even then.
    if hz > 0 { hz as f64 } else { 100.0 }
}

fn read_text(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path, e))
}

/// Every pid in `/proc`, in the order the directory lists them.
pub fn pids() -> Result<Vec<i32>, String> {
    let entries = std::fs::read_dir("/proc").map_err(|e| format!("/proc: {}", e))?;
    Ok(entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .collect())
}

/// `/proc/<pid>/stat`, parsed.
pub fn read_stat(pid: i32) -> Result<PidStat, String> {
    let path = format!("/proc/{}/stat", pid);
    let text = read_text(&path)?;
    parse_pid_stat(&text).ok_or_else(|| format!("{}: not a stat line", path))
}

/// `/proc/<pid>/io`, parsed. Another user's is closed to us, which is an
/// `Err` like any other unreadable file.
pub fn read_io(pid: i32) -> Result<PidIo, String> {
    let path = format!("/proc/{}/io", pid);
    let text = read_text(&path)?;
    parse_proc_pid_io(&text).ok_or_else(|| format!("{}: no counters in it", path))
}

/// `/proc/<pid>/cgroup`, as `parse_proc_pid_cgroup` reads it. Readable for
/// every process, whoever owns it.
pub fn read_cgroup(pid: i32) -> Result<String, String> {
    read_text(&format!("/proc/{}/cgroup", pid)).map(|t| parse_proc_pid_cgroup(&t))
}

/// `/proc/<pid>/comm`, without its trailing newline.
pub fn read_comm(pid: i32) -> Result<String, String> {
    read_text(&format!("/proc/{}/comm", pid)).map(|s| s.trim_end_matches('\n').to_string())
}

/// `/proc/<pid>/cmdline` with its NULs turned into spaces.
pub fn read_cmdline(pid: i32) -> Result<String, String> {
    let path = format!("/proc/{}/cmdline", pid);
    std::fs::read(&path)
        .map(|raw| String::from_utf8_lossy(&raw).replace('\0', " ").trim().to_string())
        .map_err(|e| format!("{}: {}", path, e))
}

/// Where `/proc/<pid>/cwd` points.
pub fn read_cwd(pid: i32) -> Result<String, String> {
    let path = format!("/proc/{}/cwd", pid);
    std::fs::read_link(&path)
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("{}: {}", path, e))
}

/// `/proc/uptime`: seconds since boot.
pub fn read_uptime() -> Result<f64, String> {
    let text = read_text("/proc/uptime")?;
    parse_proc_uptime(&text).ok_or_else(|| "/proc/uptime: no number in it".to_string())
}

/// Everything `read_pid` reads, each file on its own: one that cannot be
/// read does not take the others with it.
#[derive(Clone, Debug)]
pub struct PidFiles {
    pub pid: i32,
    pub stat: Result<PidStat, String>,
    pub io: Result<PidIo, String>,
    /// The cgroup path, as `parse_proc_pid_cgroup` reads it.
    pub cgroup: Result<String, String>,
    pub cmdline: Result<String, String>,
    pub comm: Result<String, String>,
    pub cwd: Result<String, String>,
    /// The owner of `/proc/<pid>`, which is the process's real uid.
    pub uid: Result<u32, String>,
}

/// The owner of `/proc/<pid>`: the process's uid and gid, as `(uid, gid)`.
/// Cheaper than `read_pid` when a table needs the owner and not the rest.
pub fn read_owner(pid: i32) -> Result<(u32, u32), String> {
    use std::os::unix::fs::MetadataExt;
    let dir = format!("/proc/{}", pid);
    std::fs::metadata(&dir)
        .map(|m| (m.uid(), m.gid()))
        .map_err(|e| format!("{}: {}", dir, e))
}

/// Each of a process's files, read once. A pid that has gone since `pids()`
/// listed it is `Err` on every one of them, and the caller drops it.
pub fn read_pid(pid: i32) -> PidFiles {
    use std::os::unix::fs::MetadataExt;
    let dir = format!("/proc/{}", pid);
    PidFiles {
        pid,
        stat: read_stat(pid),
        io: read_io(pid),
        cgroup: read_cgroup(pid),
        cmdline: read_cmdline(pid),
        comm: read_comm(pid),
        cwd: read_cwd(pid),
        uid: std::fs::metadata(&dir)
            .map(|m| m.uid())
            .map_err(|e| format!("{}: {}", dir, e)),
    }
}

// ── Linux acquisition with a macOS counterpart in the widget ─────────

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;

    use super::{parse_ip_addresses, Facts, OpenFile, RUN_TIMEOUT};

    /// Addresses this machine answers on, from `ip -o addr`.
    pub fn own_addresses() -> Result<Vec<String>, String> {
        crate::run(&["ip", "-o", "addr"], RUN_TIMEOUT)
            .map(|text| parse_ip_addresses(&text))
            .map_err(|e| format!("ip -o addr unavailable: {}", e))
    }

    /// socket inode -> pid, for every process whose `fd/` this user can
    /// read. Another user's is closed, so their sockets arrive unowned.
    pub fn socket_pids() -> Result<HashMap<String, i32>, String> {
        let mut owners = HashMap::new();
        let entries = std::fs::read_dir("/proc")
            .map_err(|e| format!("/proc socket ownership unavailable: {}", e))?;
        for entry in entries.flatten() {
            let pid: i32 = match entry.file_name().to_string_lossy().parse() {
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
        Ok(owners)
    }

    /// socket inode -> (pid, name), naming each owning process once.
    pub fn socket_owners() -> Result<HashMap<String, (i32, String)>, String> {
        let mut names: HashMap<i32, String> = HashMap::new();
        Ok(socket_pids()?
            .into_iter()
            .map(|(inode, pid)| {
                let name = names.entry(pid).or_insert_with(|| process_name(pid)).clone();
                (inode, (pid, name))
            })
            .collect())
    }

    /// A readable name for a pid: `comm`, unless that is all digits or
    /// punctuation, then the last meaningful part of argv0's path.
    pub fn process_name(pid: i32) -> String {
        let comm = std::fs::read_to_string(format!("/proc/{}/comm", pid))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if comm.chars().any(|c| c.is_ascii_alphabetic()) {
            return comm;
        }
        let argv0 = std::fs::read(format!("/proc/{}/cmdline", pid))
            .map(|raw| {
                String::from_utf8_lossy(&raw)
                    .split('\0')
                    .next()
                    .unwrap_or("")
                    .to_string()
            })
            .unwrap_or_default();
        for part in argv0.split('/').rev() {
            if part.chars().any(|c| c.is_ascii_alphabetic())
                && ![
                    "versions",
                    "bin",
                    "sbin",
                    "libexec",
                    "node_modules",
                    "dist",
                    "build",
                    "lib",
                    "share",
                    "local",
                    "current",
                    "releases",
                ]
                .contains(&part.to_lowercase().as_str())
            {
                return part.to_string();
            }
        }
        if comm.is_empty() { "?".into() } else { comm }
    }

    /// Whether the process still exists.
    pub fn running(pid: i32) -> bool {
        pid > 0 && std::path::Path::new(&format!("/proc/{}", pid)).is_dir()
    }

    /// Regular files this process has open, largest first.
    pub fn open_files(pid: i32) -> Result<Vec<OpenFile>, String> {
        let mut found = Vec::new();
        let dir = std::fs::read_dir(format!("/proc/{}/fd", pid))
            .map_err(|e| format!("open files unavailable: {}", e))?;
        for entry in dir.flatten() {
            let Ok(link) = std::fs::read_link(entry.path()) else {
                continue;
            };
            let path = link.to_string_lossy().to_string();
            if !path.starts_with('/')
                || path.starts_with("/dev/")
                || path.starts_with("/proc/")
                || path.starts_with("/sys/")
            {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(entry.path()) {
                found.push(OpenFile {
                    path,
                    size: meta.len(),
                });
            }
        }
        found.sort_by(|a, b| b.size.cmp(&a.size));
        Ok(found)
    }

    /// Command line and working directory, empty where unreadable.
    pub fn process_facts(pid: i32) -> Facts {
        Facts {
            cmdline: super::read_cmdline(pid).unwrap_or_default(),
            cwd: super::read_cwd(pid).unwrap_or_default(),
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{
    open_files, own_addresses, process_facts, process_name, running, socket_owners, socket_pids,
};

// ── reverse DNS, off the drawing thread ──────────────────────────────

/// Reverse DNS, off the drawing thread.
///
/// A PTR lookup takes half a second when it works and longer when it does
/// not, which is several frames. The address is shown until a name arrives,
/// and an address that has no name is remembered as having none so it is
/// not asked about again every second.
pub struct Resolver {
    known: Arc<Mutex<HashMap<String, String>>>,
    wanted: Arc<Mutex<Vec<String>>>,
}

impl Resolver {
    pub fn new(activity: crate::Refresh) -> Resolver {
        let known: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let wanted: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let (k, q) = (Arc::clone(&known), Arc::clone(&wanted));
        std::thread::spawn(move || loop {
            let next = q.lock().ok().and_then(|mut g| g.pop());
            let ip = match next {
                Some(ip) => ip,
                None => {
                    std::thread::sleep(Duration::from_millis(300));
                    continue;
                }
            };
            let work = activity.start();
            // getent rather than a resolver library: it is glibc's own
            // lookup, so it honours /etc/hosts, nsswitch and the search
            // domains exactly as everything else on the machine does.
            let answer = crate::run_quiet(&["getent", "hosts", &ip], RUN_TIMEOUT);
            let name = answer
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_string();
            if let Ok(mut guard) = k.lock() {
                guard.insert(ip, name);
            }
            drop(work);
        });
        Resolver { known, wanted }
    }

    /// One with no lookup thread behind it, which knows no names and never
    /// learns any - for a test that draws a screen holding addresses.
    pub fn idle() -> Resolver {
        Resolver {
            known: Arc::new(Mutex::new(HashMap::new())),
            wanted: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A name for an address if one is known, queueing a lookup if not.
    pub fn name(&self, ip: &str) -> String {
        if let Ok(guard) = self.known.lock() {
            if let Some(found) = guard.get(ip) {
                return found.clone();
            }
        }
        if let Ok(mut queue) = self.wanted.lock() {
            if !queue.iter().any(|q| q == ip) && queue.len() < 64 {
                queue.push(ip.to_string());
            }
        }
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stat line with every field up to `delayacct_blkio_ticks` (42),
    /// fields 5.. filled with their own index so a wrong offset shows.
    fn stat_line(pid: i32, comm: &str, state: char, ppid: i32) -> String {
        let mut fields: Vec<String> = (5..=44).map(|n| n.to_string()).collect();
        // utime 14, stime 15, num_threads 20, starttime 22, rss 24, blkio 42.
        fields[14 - 5] = "310".into();
        fields[15 - 5] = "90".into();
        fields[19 - 5] = "-5".into();
        fields[20 - 5] = "8".into();
        fields[22 - 5] = "50000".into();
        fields[24 - 5] = "300".into();
        fields[42 - 5] = "7".into();
        format!("{} ({}) {} {} {}", pid, comm, state, ppid, fields.join(" "))
    }

    #[test]
    fn a_stat_line_is_split_on_its_last_bracket() {
        let s = parse_pid_stat(&stat_line(4242, "(a) b (c)", 'S', 1)).expect("parses");
        assert_eq!(s.pid, 4242);
        assert_eq!(s.comm, "(a) b (c)");
        assert_eq!(s.state, 'S');
        assert_eq!(s.ppid, 1);
        assert_eq!((s.utime, s.stime), (310, 90));
        assert_eq!(s.nice, -5);
        assert_eq!(s.num_threads, 8);
        assert_eq!(s.starttime, 50000);
        assert_eq!(s.rss_pages, 300);
        assert_eq!(s.delayacct_blkio_ticks, Some(7));
    }

    #[test]
    fn a_zombie_and_a_kernel_thread_read_as_what_they_are() {
        let z = parse_pid_stat(&stat_line(77, "example-worker", 'Z', 76)).unwrap();
        assert_eq!(z.state, 'Z');
        let k = parse_pid_stat(&stat_line(9, "kworker/0:1", 'I', 2)).unwrap();
        assert_eq!(k.ppid, 2, "a kernel thread is a child of kthreadd");
    }

    #[test]
    fn an_old_kernel_without_blkio_ticks_is_none_and_a_zero_stays_zero() {
        // Up to field 24 (rss) and no further: the shape mux-activity's
        // fixtures have always used.
        let short = "42 (my (odd) proc) S 1 42 42 0 -1 4194304 100 0 0 0 310 90 0 0 20 0 8 0 900 0 4096 0";
        let s = parse_pid_stat(short).expect("parses without field 42");
        assert_eq!((s.utime + s.stime, s.rss_pages), (400, 4096));
        assert_eq!(s.delayacct_blkio_ticks, None);
        let zero = stat_line(1, "example-worker", 'S', 0).replace(" 7 43 44", " 0 43 44");
        assert_eq!(parse_pid_stat(&zero).unwrap().delayacct_blkio_ticks, Some(0));
        // A truncated line has no fields to find and must not be guessed at.
        assert_eq!(parse_pid_stat("42 (short) S 1 2 3"), None);
        assert_eq!(parse_pid_stat("garbage"), None);
    }

    #[test]
    fn io_reads_all_five_counters_or_says_there_were_none() {
        let text = "rchar: 100\nwchar: 200\nsyscr: 3\nsyscw: 4\nread_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 512\n";
        assert_eq!(
            parse_proc_pid_io(text),
            Some(PidIo {
                rchar: 100,
                wchar: 200,
                read_bytes: 4096,
                write_bytes: 8192,
                cancelled_write_bytes: 512,
            })
        );
        assert_eq!(parse_proc_pid_io("syscr: 3\nsyscw: 4\n"), None);
        assert_eq!(parse_proc_pid_io(""), None);
    }

    #[test]
    fn a_cgroup_path_comes_from_v2_or_from_systemd_on_v1() {
        assert_eq!(
            parse_proc_pid_cgroup("0::/system.slice/example-worker.service\n"),
            "/system.slice/example-worker.service"
        );
        let v1 = "12:pids:/user.slice/user-1000.slice\n\
                  11:cpu,cpuacct:/user.slice\n\
                  1:name=systemd:/system.slice/example-worker.service\n";
        assert_eq!(parse_proc_pid_cgroup(v1), "/system.slice/example-worker.service");
        assert_eq!(parse_proc_pid_cgroup("3:memory:/a\n2:pids:/b\n"), "/a");
        assert_eq!(parse_proc_pid_cgroup(""), "");
    }

    #[test]
    fn a_cgroup_names_the_daemon() {
        assert_eq!(unit_name("/system.slice/example-worker.service"), "example-worker");
        assert_eq!(unit_name("/user.slice/user-1000.slice/session-1.scope"), "");
        assert_eq!(unit_name("/"), "");
    }

    #[test]
    fn a_start_time_and_the_uptime_give_an_age_at_any_tick_rate() {
        let up = parse_proc_uptime("12345.67 98765.43\n").expect("a number");
        assert_eq!(up, 12345.67);
        // 250 ticks a second: started 5000 ticks = 20 s after boot.
        let age = age_seconds(up, 5000, 250.0);
        assert!((age - 12325.67).abs() < 1e-9, "{}", age);
        // The same ticks at 100 Hz are 50 s, so the rate is not assumed.
        assert!((age_seconds(up, 5000, 100.0) - 12295.67).abs() < 1e-9);
        assert_eq!(parse_proc_uptime(""), None);
    }

    #[test]
    fn parses_captured_ss_tine_rows() {
        let text = "\
State Recv-Q Send-Q Local Address:Port Peer Address:Port
ESTAB 0      0      192.0.2.10:54321   192.0.2.1:443     ino:4242 cgroup:/user.slice/session-1.scope
\t cubic rto:204 rtt:1.5/0.75 bytes_sent:1669 bytes_received:11469 segs_out:12 minrtt:0.9
ESTAB 0      0      [2001:db8::10]:22  [2001:db8::1]:6000 ino:99
\t bytes_sent:10 bytes_received:20 minrtt:0.4
ESTAB 0      0      127.0.0.1:1        127.0.0.1:2       ino:0
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
                    rtt: Some(1.5),
                },
                SocketRecord {
                    inode: "99".into(),
                    sent: 10,
                    recv: 20,
                    peer: "2001:db8::1".into(),
                    port: 6000,
                    mine: 22,
                    cgroup: String::new(),
                    // minrtt: is not rtt:, and an absent one is not 0.
                    rtt: None,
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

    #[test]
    fn ip_addresses_lose_their_prefix_length() {
        let text = "2: eth0    inet 192.0.2.10/24 brd 192.0.2.255 scope global eth0\n\
                    2: eth0    inet6 2001:db8::10/64 scope global\n";
        assert_eq!(parse_ip_addresses(text), vec!["192.0.2.10", "2001:db8::10"]);
    }

    #[test]
    fn peers_are_sorted_into_here_nearby_and_out_there() {
        let own = vec!["192.0.2.10".to_string()];
        assert!(local_peer("127.0.0.1", &own));
        assert!(local_peer("::ffff:192.0.2.10", &own));
        assert!(off_box("203.0.113.10", &own));
        assert!(!off_box("10.0.0.5", &own));
        assert!(!off_box("100.64.0.1", &own));
    }

    #[test]
    fn hidepid_is_read_from_the_proc_mount() {
        let line = |opts: &str| {
            format!(
                "22 26 0:21 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc {opts}\n"
            )
        };
        assert!(parse_hidepid(&line("rw,hidepid=invisible")));
        assert!(parse_hidepid(&line("rw,hidepid=2")));
        assert!(!parse_hidepid(&line("rw,hidepid=noaccess")), "still listed");
        assert!(!parse_hidepid(&line("rw")));
        assert!(!parse_hidepid(
            "30 22 0:5 / /mnt/elsewhere rw - proc proc rw,hidepid=2\n"
        ));
    }

    #[test]
    fn rss_is_counted_in_pages_of_this_kernel() {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        assert_eq!(rss_bytes(3), 3 * page);
        assert!(clock_ticks() > 0.0);
    }

    #[test]
    fn a_pid_that_does_not_exist_is_an_error_on_every_file() {
        let files = read_pid(i32::MAX);
        assert!(files.stat.is_err());
        assert!(files.io.is_err());
        assert!(files.cgroup.is_err());
        assert!(files.cmdline.is_err());
        assert!(files.comm.is_err());
        assert!(files.cwd.is_err());
        assert!(files.uid.is_err());
        assert!(read_owner(i32::MAX).is_err());
        // And the error says which file.
        assert!(files.stat.unwrap_err().contains("/stat"));
    }

    // Gated only because it opens /proc: the parsers above are not.
    #[cfg(target_os = "linux")]
    #[test]
    fn pids_lists_this_process() {
        let me = std::process::id() as i32;
        assert!(pids().expect("/proc is readable").contains(&me));
        let files = read_pid(me);
        assert_eq!(files.stat.expect("own stat").pid, me);
        assert!(files.io.is_ok() && files.comm.is_ok() && files.uid.is_ok());
        let (uid, gid) = read_owner(me).expect("own /proc dir");
        assert_eq!(uid, unsafe { libc::geteuid() });
        assert_eq!(gid, unsafe { libc::getegid() });
    }
}
