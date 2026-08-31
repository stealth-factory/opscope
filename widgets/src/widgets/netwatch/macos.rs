use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use opscope_core as tc;

use super::parse::{
    parse_ifconfig_addresses, parse_lsof_files, parse_netstat_interfaces, parse_nettop_processes,
};
use super::{Facts, OpenFile, ProcessSeen, RUN_TIMEOUT, TrafficSample, VIRTUAL};

pub const ROW_SOURCE: &str = "per-process cumulative bytes · macOS nettop";
pub const HAS_CONNECTION_DETAILS: bool = false;
pub const HAS_DISK_IO: bool = false;
pub const NO_SOCKET_DETAIL: &str =
    "   unavailable: macOS exposes process totals, not per-socket bytes";
pub const NO_DISK_IO: &str = "not exposed by an unprivileged macOS CLI source";

#[derive(Default)]
struct NettopState {
    generation: u64,
    latest: Vec<super::parse::ProcessRecord>,
    error: String,
}

struct NettopFeed {
    state: Mutex<NettopState>,
    changed: Condvar,
}

static NETTOP: OnceLock<Arc<NettopFeed>> = OnceLock::new();
static LAST_NETTOP_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Keep one `nettop` logger alive. A one-shot `nettop -L 1` takes roughly six
/// seconds to exit, and a pipe makes the continuous logger buffer several
/// samples together. `script` gives it the terminal it expects, so each CSV
/// sample is flushed at the requested one-second cadence.
fn nettop_feed() -> &'static Arc<NettopFeed> {
    NETTOP.get_or_init(|| {
        let feed = Arc::new(NettopFeed {
            state: Mutex::new(NettopState::default()),
            changed: Condvar::new(),
        });
        let worker = Arc::clone(&feed);
        std::thread::spawn(move || run_nettop(worker));
        feed
    })
}

fn publish_nettop(feed: &NettopFeed, lines: &mut Vec<String>) {
    if lines.len() < 2 {
        lines.clear();
        return;
    }
    let rows = parse_nettop_processes(&lines.join("\n"));
    lines.clear();
    if rows.is_empty() {
        return;
    }
    if let Ok(mut state) = feed.state.lock() {
        state.generation = state.generation.wrapping_add(1).max(1);
        state.latest = rows;
        state.error.clear();
        feed.changed.notify_all();
    }
}

fn fail_nettop(feed: &NettopFeed, reason: String) {
    if let Ok(mut state) = feed.state.lock() {
        state.error = reason;
        feed.changed.notify_all();
    }
}

fn drain_stderr(stderr: Option<std::process::ChildStderr>) -> Arc<Mutex<String>> {
    let last = Arc::new(Mutex::new(String::new()));
    let Some(stderr) = stderr else {
        return last;
    };
    let slot = Arc::clone(&last);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            if let Ok(mut guard) = slot.lock() {
                *guard = line;
            }
        }
    });
    last
}

fn with_stderr(reason: String, stderr: &Mutex<String>) -> String {
    match stderr.lock() {
        Ok(line) if !line.is_empty() => format!("{}: {}", reason, line),
        _ => reason,
    }
}

/// Keep reading samples for as long as the widget lives. A child that exits
/// is a source failure, not the end of the session: the next poll would
/// otherwise keep returning that one error and freeze the process table.
fn run_nettop(feed: Arc<NettopFeed>) {
    loop {
        run_nettop_once(&feed);
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn run_nettop_once(feed: &NettopFeed) {
    let mut child = match Command::new("/usr/bin/script")
        .args([
            "-q",
            "/dev/null",
            "/usr/bin/nettop",
            "-P",
            "-x",
            "-L",
            "0",
            "-s",
            "1",
            "-J",
            "bytes_in,bytes_out",
        ])
        // The logger lives for the whole widget session. If it inherits the
        // pane's stdin it races the UI for key and mouse reports, making
        // selection and scrolling appear to work only at random.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            fail_nettop(
                feed,
                format!("nettop per-process bytes unavailable: {}", err),
            );
            return;
        }
    };
    let stderr = drain_stderr(child.stderr.take());
    let Some(stdout) = child.stdout.take() else {
        fail_nettop(
            feed,
            with_stderr("nettop stdout unavailable".into(), &stderr),
        );
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    let mut sample = Vec::new();
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(line) => line.trim_end_matches('\r').to_string(),
            Err(err) => {
                fail_nettop(
                    feed,
                    with_stderr(format!("nettop output unreadable: {}", err), &stderr),
                );
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        };
        if line.contains(",bytes_in,bytes_out,") {
            publish_nettop(feed, &mut sample);
            sample.push(",bytes_in,bytes_out,".into());
        } else {
            sample.push(line);
        }
    }
    publish_nettop(feed, &mut sample);
    let reason = match child.wait() {
        Ok(status) => format!("nettop stopped with {}", status),
        Err(err) => format!("nettop stopped: {}", err),
    };
    fail_nettop(feed, with_stderr(reason, &stderr));
}

#[allow(dead_code)] // Connection-detail hosts call this; macOS process rows do not.
pub fn own_addresses() -> Result<Vec<String>, String> {
    tc::run(&["ifconfig", "-a"], RUN_TIMEOUT)
        .map(|text| parse_ifconfig_addresses(&text))
        .map_err(|e| format!("ifconfig -a unavailable: {}", e))
}

pub fn sockets(_external: bool, _own: &[String]) -> Result<TrafficSample, String> {
    let feed = nettop_feed();
    let last = LAST_NETTOP_GENERATION.load(Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut state = feed
        .state
        .lock()
        .map_err(|_| "nettop feed lock failed".to_string())?;
    while state.generation <= last && state.error.is_empty() {
        let now = Instant::now();
        if now >= deadline {
            return Err("nettop did not publish a new per-process sample within 3s".into());
        }
        let (next, timeout) = feed
            .changed
            .wait_timeout(state, deadline - now)
            .map_err(|_| "nettop feed lock failed".to_string())?;
        state = next;
        if timeout.timed_out() && state.generation <= last {
            return Err("nettop did not publish a new per-process sample within 3s".into());
        }
    }
    if !state.error.is_empty() {
        return Err(state.error.clone());
    }
    LAST_NETTOP_GENERATION.store(state.generation, Ordering::Relaxed);
    let rows = state
        .latest
        .clone()
        .into_iter()
        .map(|row| ProcessSeen {
            pid: row.pid,
            name: row.name,
            sent: row.sent,
            recv: row.recv,
        })
        .collect();
    Ok(TrafficSample::Processes(rows))
}

/// `nettop` already names each process. There is no per-socket counter to
/// join to an owner on macOS, so this seam is deliberately an empty map.
pub fn socket_owners() -> Result<HashMap<String, (i32, String)>, String> {
    Ok(HashMap::new())
}

pub fn wire_bytes() -> Result<(u64, u64, Vec<String>), String> {
    let text = tc::run(&["netstat", "-ib"], RUN_TIMEOUT)
        .map_err(|e| format!("netstat -ib unavailable: {}", e))?;
    Ok(parse_netstat_interfaces(&text, VIRTUAL))
}

pub fn running(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub fn proc_io(_pid: i32) -> HashMap<String, u64> {
    HashMap::new()
}

pub fn open_files(pid: i32) -> Result<Vec<OpenFile>, String> {
    let text = tc::run(
        &["lsof", "-a", "-p", &pid.to_string(), "-s", "-Fnfst"],
        RUN_TIMEOUT,
    )
    .map_err(|e| format!("lsof open files unavailable: {}", e))?;
    let mut found: Vec<OpenFile> = parse_lsof_files(&text)
        .into_iter()
        .map(|file| OpenFile {
            path: file.path,
            size: file.size,
        })
        .collect();
    found.sort_by(|a, b| b.size.cmp(&a.size));
    Ok(found)
}

pub fn process_facts(pid: i32) -> Facts {
    let pid = pid.to_string();
    let cmdline = tc::run_quiet(&["ps", "-www", "-o", "args=", "-p", &pid], RUN_TIMEOUT)
        .trim()
        .to_string();
    let cwd = tc::run_quiet(&["lsof", "-a", "-p", &pid, "-d", "cwd", "-Fn"], RUN_TIMEOUT)
        .lines()
        .find_map(|line| line.strip_prefix('n'))
        .unwrap_or_default()
        .to_string();
    Facts { cmdline, cwd }
}
