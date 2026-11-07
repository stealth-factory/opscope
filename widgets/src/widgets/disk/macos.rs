// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Where the bytes come from on macOS.
//!
//! `getmntinfo(MNT_NOWAIT)` returns every mount's `statfs` in one call, so
//! there is no per-mount `stat` that can hang the poller. The buffer belongs
//! to libc and is replaced on the next call; the strings and the byte
//! counts are copied out before this function returns. `stat_mount` reads
//! that copy. `dev` is the mount-from name, which is the identity
//! `visible_mounts` uses to drop duplicates on this host.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use super::by_process::{self, ProcDetail, ProcMemory, RateHistory, Rusage, Source, Unreadable};
use super::parse::{self, IoSnapshot, LiveDisk, Mount, Space};
use super::writers::{self, Wanted, Writers};

struct IostatState {
    track: parse::IostatTrack,
    missing: bool,
    err: String,
}

static IOSTAT: OnceLock<Mutex<IostatState>> = OnceLock::new();
static IOSTAT_STARTED: OnceLock<()> = OnceLock::new();

static SPACES: Mutex<Option<HashMap<String, Space>>> = Mutex::new(None);

pub fn read_mounts() -> Result<Vec<Mount>, String> {
    let mut buf: *mut libc::statfs = std::ptr::null_mut();
    let count = unsafe { libc::getmntinfo(&mut buf, libc::MNT_NOWAIT) };
    if count <= 0 || buf.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let rows = unsafe { std::slice::from_raw_parts(buf, count as usize) };
    let mut mounts = Vec::with_capacity(rows.len());
    let mut spaces = HashMap::with_capacity(rows.len());
    for stat in rows {
        let mount = mount_from_statfs(stat);
        spaces.insert(mount.mount_point.clone(), space_from_statfs(stat));
        mounts.push(mount);
    }
    match SPACES.lock() {
        Ok(mut guard) => *guard = Some(spaces),
        Err(poisoned) => *poisoned.into_inner() = Some(spaces),
    }
    Ok(mounts)
}

/// The figures from the `getmntinfo` snapshot `read_mounts` just took.
///
/// A second `statfs` would be a look at a mount that can hang, which is
/// what `MNT_NOWAIT` avoided. The poller still calls this once per shown
/// mount so the Linux timeout has one shape on both hosts; here it does
/// not enter the kernel.
pub fn stat_mount(path: &[u8]) -> Result<Space, String> {
    let key = String::from_utf8_lossy(path);
    let guard = SPACES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .as_ref()
        .and_then(|spaces| spaces.get(key.as_ref()).copied())
        .ok_or_else(|| format!("no statfs entry for {key}"))
}

fn mount_from_statfs(stat: &libc::statfs) -> Mount {
    let read_only = (stat.f_flags & libc::MNT_RDONLY as u32) != 0;
    let source = c_buf(&stat.f_mntfromname);
    let path = c_bytes(&stat.f_mntonname);
    Mount {
        dev: source.clone(),
        root: "/".to_string(),
        mount_point: String::from_utf8_lossy(&path).into_owned(),
        path,
        fstype: c_buf(&stat.f_fstypename),
        source,
        read_only,
        options: if read_only { "ro" } else { "rw" }.to_string(),
    }
}

fn space_from_statfs(stat: &libc::statfs) -> Space {
    parse::space_from_blocks(
        stat.f_blocks,
        stat.f_bfree,
        stat.f_bavail,
        stat.f_files,
        stat.f_ffree,
        stat.f_bsize as u64,
    )
}

/// One long-running `iostat -d -K -w`. The first data line is the average
/// since boot and is kept only to decide which disks have never moved;
/// every later line is one interval. Stdout is read a line at a time.
/// A child that exits is started again. A missing binary is not: the
/// widget still runs, and the pane says so.
pub fn sample_io(_mounts: &[Mount], every: f64) -> Result<IoSnapshot, String> {
    let state = IOSTAT.get_or_init(|| {
        Mutex::new(IostatState {
            track: parse::IostatTrack::default(),
            missing: false,
            err: String::new(),
        })
    });
    IOSTAT_STARTED.get_or_init(|| {
        thread::spawn(move || run_iostat(every));
    });
    let guard = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.missing {
        return Ok(IoSnapshot {
            missing: "needs iostat (recommended) — device I/O is not shown".into(),
            combined: true,
            ..IoSnapshot::default()
        });
    }
    let mut disks = Vec::with_capacity(guard.track.names.len());
    for (index, name) in guard.track.names.iter().enumerate() {
        let mac = guard.track.latest.get(index).copied();
        let idle = parse::iostat_disk_idle(&guard.track, index);
        disks.push(LiveDisk {
            name: name.clone(),
            kind: String::new(),
            size: 0,
            model: String::new(),
            dev: String::new(),
            stat: None,
            mac,
            idle,
            partitions: Vec::new(),
            mounts: Vec::new(),
            slaves: Vec::new(),
        });
    }
    Ok(IoSnapshot {
        disks,
        combined: true,
        err: guard.err.clone(),
        ..IoSnapshot::default()
    })
}

fn run_iostat(every: f64) {
    let wait = every.round().max(1.0) as u64;
    let wait = wait.to_string();
    loop {
        let mut child = match Command::new("iostat")
            .args(["-d", "-K", "-w", &wait])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if let Some(state) = IOSTAT.get() {
                    if let Ok(mut guard) = state.lock() {
                        guard.missing = true;
                        guard.err.clear();
                    }
                }
                return;
            }
            Err(err) => {
                note_iostat_error(format!("iostat stopped: {err}"));
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let stderr = child.stderr.take();
        let stderr_text = std::sync::Arc::new(Mutex::new(String::new()));
        if let Some(stderr) = stderr {
            let slot = std::sync::Arc::clone(&stderr_text);
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Ok(mut guard) = slot.lock() {
                        if !guard.is_empty() {
                            guard.push('\n');
                        }
                        guard.push_str(&line);
                    }
                }
            });
        }
        let Some(stdout) = child.stdout.take() else {
            note_iostat_error("iostat stopped: no stdout".into());
            let _ = child.wait();
            thread::sleep(Duration::from_secs(1));
            continue;
        };
        let mut names = Vec::new();
        let mut skipped_boot = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            let header = parse::parse_iostat_header(&line);
            if !header.is_empty() {
                names = header;
                continue;
            }
            if names.is_empty() {
                continue;
            }
            let samples = parse::parse_iostat_line(&line, names.len());
            if samples.len() != names.len() {
                continue;
            }
            if !skipped_boot {
                publish_iostat(&names, None, Some(samples));
                skipped_boot = true;
                continue;
            }
            publish_iostat(&names, Some(samples), None);
        }
        let _ = child.wait();
        let why = stderr_text
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let why = if why.is_empty() {
            "exited".to_string()
        } else {
            why
        };
        note_iostat_error(format!("iostat stopped: {why}"));
        thread::sleep(Duration::from_secs(1));
    }
}

fn publish_iostat(
    names: &[String],
    latest: Option<Vec<parse::IostatSample>>,
    boot: Option<Vec<parse::IostatSample>>,
) {
    let Some(state) = IOSTAT.get() else {
        return;
    };
    let Ok(mut guard) = state.lock() else {
        return;
    };
    parse::publish_iostat_track(&mut guard.track, names, latest, boot);
    guard.err.clear();
}

fn note_iostat_error(err: String) {
    let Some(state) = IOSTAT.get() else {
        return;
    };
    if let Ok(mut guard) = state.lock() {
        guard.err = err;
    }
}

fn c_bytes(buf: &[libc::c_char]) -> Vec<u8> {
    buf.iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect()
}

fn c_buf(buf: &[libc::c_char]) -> String {
    String::from_utf8_lossy(&c_bytes(buf)).into_owned()
}

/// WHO IS WRITING on a Mac is BY PROCESS alone. By service is cgroup v2,
/// which a Mac does not have, and `ps` has no state that means what
/// Linux's `D` does (`U` is the nearest, and it is a different thing), so
/// neither is drawn - not drawn empty either.
///
/// Per poll: `proc_listallpids` (libproc's `proc_listpids(PROC_ALL_PIDS)`
/// returning a count), then `proc_pid_rusage(RUSAGE_INFO_V2)` per pid for
/// `ri_diskio_bytesread` / `ri_diskio_byteswritten`, then one
/// `ps -axo pid=,comm=` for the names. libproc is in libSystem, so this
/// links nothing new.
#[derive(Default)]
pub struct WriterSource {
    procs: ProcMemory,
    /// The open process's last minute of rates.
    history: Option<RateHistory>,
    start: Option<std::time::Instant>,
}

// `rusage_info_v2` comes from `libc` (bsd/sys/resource.h in xnu: a 16-byte
// uuid then eighteen u64 counters, `ri_diskio_bytesread` and
// `ri_diskio_byteswritten` last). A layout that drifted would read another
// counter as the disk bytes; this fails the macOS build instead.
const _: () = assert!(std::mem::size_of::<libc::rusage_info_v2>() == 16 + 18 * 8);

pub fn sample_writers(source: &mut WriterSource, wanted: Wanted) -> Option<Writers> {
    let start = *source.start.get_or_insert_with(std::time::Instant::now);
    let now = start.elapsed().as_secs_f64();
    let (list, mut pid_err) = match list_pids() {
        Ok(pids) => (rusage_all(&pids), String::new()),
        Err(err) => (Vec::new(), format!("processes unavailable: {err}")),
    };
    let names = match process_names() {
        Ok(names) => names,
        Err(err) => {
            if pid_err.is_empty() {
                pid_err = format!("process names unavailable: {err}");
            }
            HashMap::new()
        }
    };
    let samples = by_process::samples_from_rusage(&list, &names);
    let starts: HashMap<i32, u64> = samples.iter().map(|s| (s.pid, s.start)).collect();
    // SAFETY: geteuid has no preconditions and cannot fail.
    let root = unsafe { libc::geteuid() } == 0;
    let procs = by_process::step(&mut source.procs, samples, now, root, false, Source::Rusage);
    let detail = wanted.pid.map(|pid| ProcDetail {
        pid,
        state: None,
        cmdline: String::new(),
        cwd: String::new(),
        files: None,
        history: by_process::feed_history(
            &mut source.history,
            pid,
            starts.get(&pid).copied(),
            &procs,
        ),
    });
    if wanted.pid.is_none() {
        source.history = None;
    }
    Some(writers::by_process_only(procs, starts, pid_err, detail))
}

/// One `ps -axo pid=,comm=` for every process's name, pid to basename.
fn process_names() -> Result<HashMap<i32, String>, String> {
    let out = Command::new("ps")
        .args(["-axo", "pid=,comm="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|err| format!("ps: {err}"))?;
    Ok(by_process::parse_ps_names(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

fn list_pids() -> Result<Vec<i32>, String> {
    // SAFETY: a null buffer asks only for the count.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // Room for processes started between the two calls.
    let mut pids: Vec<libc::c_int> = vec![0; count as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<libc::c_int>()) as libc::c_int;
    // SAFETY: the buffer is `bytes` long and lives across the call.
    let got = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    if got <= 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    pids.truncate((got as usize).min(pids.len()));
    Ok(pids.into_iter().filter(|&pid| pid >= 0).collect())
}

/// `EPERM` is another user's process (`CHECK_SAME_USER` in xnu's
/// proc_info.c): counted, unreadable. Anything else - `ESRCH` above all -
/// is a process that left between the listing and the read, and is
/// dropped rather than counted.
fn rusage_all(pids: &[i32]) -> Vec<(i32, Result<Rusage, Unreadable>)> {
    let mut out = Vec::with_capacity(pids.len());
    for &pid in pids {
        // SAFETY: zeroed is a valid value for a struct of integers.
        let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        // SAFETY: the kernel writes at most size_of::<rusage_info_v2>() for
        // RUSAGE_INFO_V2, and `info` is that type.
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V2,
                (&mut info as *mut libc::rusage_info_v2).cast::<libc::rusage_info_t>(),
            )
        };
        if rc == 0 {
            out.push((
                pid,
                Ok(Rusage {
                    read: info.ri_diskio_bytesread,
                    write: info.ri_diskio_byteswritten,
                    start: info.ri_proc_start_abstime,
                }),
            ));
        } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) {
            out.push((pid, Err(Unreadable)));
        }
    }
    out
}

pub fn process_facts(_pid: i32) -> (String, String) {
    (String::new(), String::new())
}

/// Acquisition tests: they call the real libproc and `ps` on the macOS CI
/// runner, through the same functions the poller calls, so they prove the
/// counters move rather than only that the calls compile. They cannot live
/// in a parser file: `proc_pid_rusage` does not exist in Linux's libc.
/// `widgets/tests/check.rs` allows `#[test]` in a gated host file only
/// inside this module, and only while nothing in it touches a parser.
/// The layout of `rusage_info_v2` is held by the compile-time assertion
/// above, not here.
#[cfg(test)]
mod acquisition_tests {
    use super::*;
    use std::io::Write;

    fn own_pid() -> i32 {
        std::process::id() as i32
    }

    fn own_rusage() -> Rusage {
        let got = rusage_all(&[own_pid()]);
        assert_eq!(got.len(), 1, "own pid was dropped: {got:?}");
        got[0].1.expect("this process's own rusage must be readable")
    }

    /// Removes the scratch directory however the test ends.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn own_disk_writes_are_counted() {
        const CHUNK: usize = 1 << 20;
        const CHUNKS: usize = 64;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("opscope-disk-{}-{nanos}", own_pid()));
        std::fs::create_dir(&dir).expect("a fresh scratch directory");
        let scratch = Scratch(dir);

        // Bytes that do not repeat, so neither a compressing nor a
        // deduplicating filesystem can store them in less than they are.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15 ^ nanos as u64;
        let mut chunk = vec![0u8; CHUNK];
        let before = own_rusage();
        {
            let mut file =
                std::fs::File::create(scratch.0.join("written")).expect("a scratch file");
            for _ in 0..CHUNKS {
                for word in chunk.chunks_mut(8) {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    word.copy_from_slice(&state.to_le_bytes()[..word.len()]);
                }
                file.write_all(&chunk).expect("write to the scratch file");
            }
            // F_FULLFSYNC on macOS: the bytes reach the disk from this
            // process before the second read, rather than later from the
            // kernel's own flusher, which is not charged to this pid.
            file.sync_all().expect("sync the scratch file");
        }
        let after = own_rusage();

        let written = (CHUNK * CHUNKS) as u64;
        let counted = after.write.saturating_sub(before.write);
        // Half, not all: APFS counts what went to the device, and metadata,
        // copy-on-write placement and block rounding can each move that
        // away from the bytes handed to write(). Parallel tests in this
        // process can only add. What this guards against is a counter that
        // reads zero or reads the wrong field - either is far below half.
        assert!(
            counted >= written / 2,
            "wrote {written} bytes and synced them; ri_diskio_byteswritten rose by {counted} \
             (before {}, after {})",
            before.write,
            after.write
        );
        assert!(
            after.start == before.start && after.start != 0,
            "ri_proc_start_abstime must be set and steady for one process: {} then {}",
            before.start,
            after.start
        );
    }

    #[test]
    fn another_users_process_is_unreadable_not_zero() {
        // pid 1 is launchd, owned by root. For any other user xnu's
        // CHECK_SAME_USER answers EPERM, which must come back as Unreadable
        // - not dropped, and not a row of zeros. Only root may read it, and
        // the GitHub runner user is not root (it has sudo, which this test
        // does not use), so the second arm is for a root shell running this
        // by hand.
        let got = rusage_all(&[1]);
        // SAFETY: geteuid has no preconditions and cannot fail.
        let root = unsafe { libc::geteuid() } == 0;
        assert_eq!(got.len(), 1, "launchd was dropped as if it had exited: {got:?}");
        assert_eq!(got[0].0, 1);
        match got[0].1 {
            Err(Unreadable) => {}
            Ok(rusage) => assert!(root, "pid 1 read as {rusage:?} by a non-root user"),
        }
    }

    #[test]
    fn the_listing_and_ps_both_see_this_process() {
        let pids = list_pids().expect("proc_listallpids");
        assert!(
            pids.contains(&own_pid()),
            "proc_listallpids left out this process ({}) among {} pids",
            own_pid(),
            pids.len()
        );
        assert!(pids.contains(&1), "proc_listallpids left out launchd");

        let names = process_names().expect("ps -axo pid=,comm=");
        let name = names
            .get(&own_pid())
            .unwrap_or_else(|| panic!("ps named {} pids but not this one", names.len()));
        let exe = std::env::current_exe().expect("this test binary's path");
        let exe = exe
            .file_name()
            .and_then(|n| n.to_str())
            .expect("a file name");
        // `comm` may be cut at MAXCOMLEN, so a prefix of the binary's name.
        assert!(
            !name.is_empty() && exe.starts_with(name.as_str()),
            "ps named this process {name:?}; the binary is {exe:?}"
        );
    }
}
