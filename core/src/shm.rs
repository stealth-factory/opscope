//! Shared-memory image transport for a terminal on this machine.
//!
//! The direct path (`t=d`) base64-encodes every image into the escape.
//! Kitty can instead read a POSIX shared-memory object (`t=s`) and unlink
//! it. That only works when the terminal and this process share a host —
//! over SSH the terminal answers `EBADF` — so one probe decides, and any
//! later failure drops back to `t=d` without turning graphics off.
//!
//! `image()` still builds the direct frame. It belongs to the crop work.
//! This module rewrites that frame after the fact, when the probe has
//! succeeded. The compressed bytes are the base64 payload with the
//! framing removed, which is the same buffer `t=d` would have carried.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use base64::Engine;

/// Not an owned placement and not the graphics query (`2147483646`).
pub(crate) const PROBE_ID: u32 = 2_147_483_645;

const NAME_CAP: usize = 31;
const SLOTS: usize = 24;
const UNPROBED: u8 = 0;
const SHARED: u8 = 1;
const DIRECT: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProbeFate {
    Unrelated,
    Accepted,
    Rejected,
}

/// What a probe reply means. Always compiled: the acquisition below is the
/// only part that depends on the OS.
pub(crate) fn parse_shm_probe(sequence: &str) -> ProbeFate {
    let Some(rest) = sequence.strip_prefix("\x1b_G") else {
        return ProbeFate::Unrelated;
    };
    let Some((keys, message)) = rest.split_once(';') else {
        return ProbeFate::Unrelated;
    };
    let id = keys
        .split(',')
        .find_map(|key| key.strip_prefix("i=")?.parse::<u32>().ok());
    if id != Some(PROBE_ID) {
        return ProbeFate::Unrelated;
    }
    let message = message.strip_suffix("\x1b\\").unwrap_or(message);
    if message == "OK" {
        ProbeFate::Accepted
    } else {
        ProbeFate::Rejected
    }
}

struct Slot {
    live: AtomicBool,
    len: AtomicU8,
    id: AtomicU32,
    name: [AtomicU8; NAME_CAP],
}

const fn empty_slot() -> Slot {
    Slot {
        live: AtomicBool::new(false),
        len: AtomicU8::new(0),
        id: AtomicU32::new(0),
        name: [const { AtomicU8::new(0) }; NAME_CAP],
    }
}

static SLOTS_TABLE: [Slot; SLOTS] = [const { empty_slot() }; SLOTS];
static TRANSPORT: AtomicU8 = AtomicU8::new(DIRECT);
static SEQ: AtomicU32 = AtomicU32::new(0);

pub(crate) fn is_shared() -> bool {
    TRANSPORT.load(Ordering::Acquire) == SHARED
}

/// Drop every object and stay on the direct path. Resume and text mode
/// both start from here, so a previous probe cannot leak into the next one.
pub(crate) fn force_direct() {
    TRANSPORT.store(DIRECT, Ordering::Release);
    release_signal();
}

/// A 1×1 image, transmitted and not placed, with a reply requested (`q=0`).
/// `None` on an OS without the calls, or when the object cannot be created:
/// the session then never leaves `t=d`.
pub(crate) fn begin_probe() -> Option<String> {
    let name = create(&[0, 0, 0, 0], PROBE_ID)?;
    TRANSPORT.store(UNPROBED, Ordering::Release);
    let payload = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
    Some(format!(
        "\x1b_Ga=t,f=32,t=s,s=1,v=1,i={PROBE_ID},q=0;{payload}\x1b\\"
    ))
}

pub(crate) fn apply_accepted() {
    unlink_id(PROBE_ID);
    let _ = TRANSPORT.compare_exchange(UNPROBED, SHARED, Ordering::AcqRel, Ordering::Acquire);
}

pub(crate) fn apply_rejected() {
    unlink_id(PROBE_ID);
    if TRANSPORT.load(Ordering::Acquire) == UNPROBED {
        TRANSPORT.store(DIRECT, Ordering::Release);
    }
}

pub(crate) fn expire_probe() {
    apply_rejected();
}

/// A later image failed while shared memory was in use. Graphics stay on;
/// the next frame is direct, and anything the terminal did not take is unlinked.
pub(crate) fn transport_failed() {
    TRANSPORT.store(DIRECT, Ordering::Release);
    release_signal();
}

/// Rewrite direct transmissions when the probe succeeded. Deletes in
/// `before` free the name they replaced. A failure to publish puts the
/// original direct bytes back and leaves shared memory off.
pub(crate) fn deliver(before: &str, after: String) -> String {
    if !is_shared() {
        return after;
    }
    for id in command_ids(before, "a=d") {
        unlink_id(id);
    }
    match rewrite(&after) {
        Ok(rewritten) => rewritten,
        Err(()) => {
            transport_failed();
            after
        }
    }
}

/// Signal handler and `Keyboard` drop both end here. Names live in atomics
/// so this can run off a signal: `shm_unlink` and nothing that allocates.
pub(crate) fn release_signal() {
    for slot in &SLOTS_TABLE {
        unlink_slot(slot);
    }
}

fn command_ids(bytes: &str, action: &str) -> Vec<u32> {
    let mut ids = Vec::new();
    let mut pos = 0;
    while let Some((_start, end, keys, _payload)) = next_apc(bytes, pos) {
        if keys.split(',').any(|part| part == action) {
            if let Some(id) = image_id_of(keys) {
                ids.push(id);
            }
        }
        pos = end;
    }
    ids
}

fn rewrite(input: &str) -> Result<String, ()> {
    let mut out = String::new();
    let mut pos = 0;
    while let Some((start, end, keys, payload)) = next_apc(input, pos) {
        if !is_direct_transmit(keys) {
            out.push_str(&input[pos..end]);
            pos = end;
            continue;
        }
        let mut group_end = end;
        let mut encoded = String::from(payload);
        let mut scan = end;
        let mut more = is_more(keys);
        while more {
            let Some((next, next_end, next_keys, next_payload)) = next_apc(input, scan) else {
                break;
            };
            if next != scan || !is_continuation(next_keys) {
                break;
            }
            encoded.push_str(next_payload);
            group_end = next_end;
            scan = next_end;
            more = is_more(next_keys);
        }
        let raw = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|_| ())?;
        let id = image_id_of(keys).unwrap_or(0);
        let name = create(&raw, id).ok_or(())?;
        let wire = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
        out.push_str(&input[pos..start]);
        out.push_str("\x1b_G");
        out.push_str(&retarget(keys));
        out.push(';');
        out.push_str(&wire);
        out.push_str("\x1b\\");
        pos = group_end;
    }
    out.push_str(&input[pos..]);
    Ok(out)
}

fn next_apc(s: &str, from: usize) -> Option<(usize, usize, &str, &str)> {
    let rest = s.get(from..)?;
    let rel = rest.find("\x1b_G")?;
    let start = from + rel;
    let body_from = start + "\x1b_G".len();
    let tail = s.get(body_from..)?;
    let rel_end = tail.find("\x1b\\")?;
    let body_end = body_from + rel_end;
    let end = body_end + "\x1b\\".len();
    let body = &s[body_from..body_end];
    let (keys, payload) = body.split_once(';').unwrap_or((body, ""));
    Some((start, end, keys, payload))
}

fn is_direct_transmit(keys: &str) -> bool {
    let mut transmit = false;
    let mut direct = false;
    for part in keys.split(',') {
        transmit |= part == "a=T";
        direct |= part == "t=d";
    }
    transmit && direct
}

fn is_more(keys: &str) -> bool {
    keys.split(',').any(|part| part == "m=1")
}

fn is_continuation(keys: &str) -> bool {
    let mut parts = keys.split(',').filter(|part| !part.is_empty());
    match (parts.next(), parts.next()) {
        (Some(part), None) => part.starts_with("m="),
        _ => false,
    }
}

fn image_id_of(keys: &str) -> Option<u32> {
    keys.split(',')
        .find_map(|part| part.strip_prefix("i=")?.parse().ok())
}

fn retarget(keys: &str) -> String {
    let mut out = String::with_capacity(keys.len() + 4);
    let mut seen_m = false;
    for part in keys.split(',') {
        if part.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(',');
        }
        if part == "t=d" {
            out.push_str("t=s");
        } else if part.starts_with("m=") {
            seen_m = true;
            out.push_str("m=0");
        } else {
            out.push_str(part);
        }
    }
    if !seen_m {
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str("m=0");
    }
    out
}

fn remember(id: u32, name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > NAME_CAP {
        return false;
    }
    for slot in &SLOTS_TABLE {
        if slot.live.load(Ordering::Acquire) && slot.id.load(Ordering::Acquire) == id {
            // The previous object for this placement. Kitty unlinks after a
            // successful read; unlinking again is ENOENT, and it frees the
            // name if the read never happened.
            unlink_slot(slot);
        }
    }
    for slot in &SLOTS_TABLE {
        if slot
            .live
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            write_name(slot, id, bytes);
            return true;
        }
    }
    false
}

fn write_name(slot: &Slot, id: u32, bytes: &[u8]) {
    // `len == 0` tells the signal handler the bytes are not ready.
    slot.len.store(0, Ordering::Release);
    for (i, byte) in bytes.iter().enumerate() {
        slot.name[i].store(*byte, Ordering::Relaxed);
    }
    slot.id.store(id, Ordering::Relaxed);
    slot.len.store(bytes.len() as u8, Ordering::Release);
}

pub(crate) fn unlink_id(id: u32) {
    for slot in &SLOTS_TABLE {
        if slot.live.load(Ordering::Acquire) && slot.id.load(Ordering::Acquire) == id {
            unlink_slot(slot);
        }
    }
}

fn unlink_slot(slot: &Slot) {
    if !slot.live.load(Ordering::Acquire) {
        return;
    }
    let len = slot.len.load(Ordering::Acquire) as usize;
    if (1..=NAME_CAP).contains(&len) {
        let mut buf = [0u8; NAME_CAP + 1];
        for i in 0..len {
            buf[i] = slot.name[i].load(Ordering::Relaxed);
        }
        unlink_cstr(&buf);
    }
    slot.len.store(0, Ordering::Release);
    slot.live.store(false, Ordering::Release);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unlink_cstr(buf: &[u8]) {
    unsafe { libc::shm_unlink(buf.as_ptr().cast()) };
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn unlink_cstr(_buf: &[u8]) {}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn create(bytes: &[u8], id: u32) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    for _ in 0..8 {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = unsafe { libc::getpid() } as u32;
        let name = format!("/o{pid:x}-{n:x}");
        if name.len() > NAME_CAP {
            return None;
        }
        let c_name = std::ffi::CString::new(name.clone()).ok()?;
        let fd = unsafe {
            libc::shm_open(
                c_name.as_ptr(),
                libc::O_CREAT | libc::O_RDWR | libc::O_EXCL,
                0o600,
            )
        };
        if fd < 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err == libc::EEXIST {
                continue;
            }
            return None;
        }
        let map_len = bytes.len();
        let failed = unsafe { libc::ftruncate(fd, map_len as libc::off_t) } != 0;
        let ptr = if failed {
            libc::MAP_FAILED
        } else {
            unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    map_len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    0,
                )
            }
        };
        if ptr == libc::MAP_FAILED {
            unsafe {
                libc::close(fd);
                libc::shm_unlink(c_name.as_ptr());
            }
            return None;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast(), map_len);
            let unmapped = libc::munmap(ptr, map_len);
            libc::close(fd);
            if unmapped != 0 {
                libc::shm_unlink(c_name.as_ptr());
                return None;
            }
        }
        if !remember(id, &name) {
            unsafe { libc::shm_unlink(c_name.as_ptr()) };
            return None;
        }
        return Some(name);
    }
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn create(_bytes: &[u8], _id: u32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    pub(crate) struct Guard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            force_direct();
        }
    }

    pub(crate) fn guard() -> Guard {
        Guard {
            _lock: TEST.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }

    static TEST: Mutex<()> = Mutex::new(());

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn exists(name: &str) -> bool {
        let Ok(c_name) = std::ffi::CString::new(name) else {
            return false;
        };
        let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
        if fd < 0 {
            return false;
        }
        unsafe { libc::close(fd) };
        true
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn read_object(name: &str) -> Option<Vec<u8>> {
        let c_name = std::ffi::CString::new(name).ok()?;
        let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
        if fd < 0 {
            return None;
        }
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::fstat(fd, &mut stat) } == 0;
        let len = stat.st_size as usize;
        let mut buf = vec![0u8; len];
        let mut got = 0;
        while ok && got < len {
            let n = unsafe { libc::read(fd, buf[got..].as_mut_ptr().cast(), len - got) };
            if n <= 0 {
                break;
            }
            got += n as usize;
        }
        unsafe { libc::close(fd) };
        (ok && got == len).then_some(buf)
    }

    fn payload_of(command: &str) -> String {
        let mut pos = 0;
        while let Some((_s, end, keys, payload)) = next_apc(command, pos) {
            if keys.split(',').any(|part| part == "t=s")
                && keys.split(',').any(|part| part == "a=T")
            {
                return payload.to_string();
            }
            pos = end;
        }
        panic!("no shared transmission in {command}");
    }

    #[test]
    fn parse_shm_probe_accepts_only_its_ok() {
        assert_eq!(
            parse_shm_probe("\x1b_Gi=2147483645;OK\x1b\\"),
            ProbeFate::Accepted
        );
        assert_eq!(
            parse_shm_probe("\x1b_Gi=2147483645,I=1;OK\x1b\\"),
            ProbeFate::Accepted
        );
        assert_eq!(
            parse_shm_probe("\x1b_Gi=2147483645;EBADF:remote\x1b\\"),
            ProbeFate::Rejected
        );
        assert_eq!(
            parse_shm_probe("\x1b_Gi=2147483645;EINVAL:bad\x1b\\"),
            ProbeFate::Rejected
        );
        assert_eq!(
            parse_shm_probe("\x1b_Gi=2147483646;OK\x1b\\"),
            ProbeFate::Unrelated
        );
        assert_eq!(
            parse_shm_probe("\x1b_Gi=1;ENOENT:no such image\x1b\\"),
            ProbeFate::Unrelated
        );
        assert_eq!(parse_shm_probe("not a reply"), ProbeFate::Unrelated);
    }

    #[test]
    fn multi_chunk_direct_frame_round_trips_through_one_object() {
        let _guard = guard();
        let raw: Vec<u8> = (0..9000).map(|i| (i * 17 + 3) as u8).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&raw);
        assert!(encoded.len() > 4096);
        let mut command = format!(
            "\x1b[2;4H\x1b_Ga=p,i=7,p=1,c=3,r=2,C=1,q=1\x1b\\\x1b[8;4H\x1b_Ga=T,f=32,t=d,o=z,s=10,v=10,i=42,p=1,c=10,r=10,C=1,q=1,m=1;"
        );
        let (first, rest) = encoded.split_at(4096);
        command.push_str(first);
        command.push_str("\x1b\\\x1b_Gm=0;");
        command.push_str(rest);
        command.push_str("\x1b\\");
        let rewritten = rewrite(&command).expect("object");
        assert!(rewritten.contains("a=p,i=7"));
        assert!(rewritten.contains("t=s"));
        assert!(!rewritten.contains("t=d"));
        assert!(rewritten.contains("m=0"));
        assert!(!rewritten.contains("m=1"));
        let name = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(payload_of(&rewritten))
                .unwrap(),
        )
        .unwrap();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            assert_eq!(read_object(&name).as_deref(), Some(raw.as_slice()));
            let again = rewrite(&command).expect("replacement");
            let name2 = String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(payload_of(&again))
                    .unwrap(),
            )
            .unwrap();
            assert_ne!(name, name2);
            assert!(!exists(&name), "the replaced object was left behind");
            assert_eq!(read_object(&name2).as_deref(), Some(raw.as_slice()));
        }
        release_signal();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(!exists(&name), "release left an object");
    }

    #[test]
    fn a_real_chart_command_is_rewritten_without_changing_pixels() {
        let _guard = guard();
        let command = super::super::graphics::direct_chart_command(24, 4);
        assert!(command.contains("t=d"));
        let compressed = direct_payload(&command);
        let rewritten = rewrite(&command).expect("object");
        assert!(rewritten.contains("t=s") && !rewritten.contains("t=d"));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let name = String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(payload_of(&rewritten))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(read_object(&name).as_deref(), Some(compressed.as_slice()));
            release_signal();
            assert!(!exists(&name));
        }
    }

    fn direct_payload(command: &str) -> Vec<u8> {
        let mut encoded = String::new();
        let mut pos = 0;
        while let Some((_s, end, keys, payload)) = next_apc(command, pos) {
            if is_direct_transmit(keys) || is_continuation(keys) {
                encoded.push_str(payload);
            }
            pos = end;
        }
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap()
    }

    #[test]
    fn release_unlinks_after_unwind_the_way_exit_and_suspend_do() {
        let _guard = guard();
        let Some(name) = create(b"still-here", 99) else {
            return;
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(exists(&name));
        let panicked = std::panic::catch_unwind(|| panic!("fixture")).is_err();
        assert!(panicked);
        // Unwind does not collect a POSIX object. Exit, suspend and panic
        // reach `release_signal` from `graphics::cleanup_signal` (signals,
        // including SIGTSTP) and from `graphics::cleanup` (`Keyboard` drop).
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(exists(&name));
        release_signal();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(!exists(&name));
    }

    #[test]
    #[ignore = "timed by hand for the PR write-up"]
    fn measure_shared_memory_transport() {
        let _guard = guard();
        let cols = 70;
        let rows = 8;
        let raw_pixels = cols * 8 * rows * 16 * 4;
        let command = super::super::graphics::direct_chart_command(cols, rows);
        let compressed = direct_payload(&command);
        let rounds = 40u32;
        let direct = cpu_for(|| {
            for _ in 0..rounds {
                let _ = frame_direct(&compressed);
            }
        });
        let shared = cpu_for(|| {
            for _ in 0..rounds {
                let name = create(&compressed, 7).expect("shm");
                let _ = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
                unlink_id(7);
            }
        });
        let shipped = cpu_for(|| {
            for _ in 0..rounds {
                let framed = frame_direct(&compressed);
                let _ = rewrite(&framed);
                unlink_id(7);
            }
        });
        eprintln!(
            "chart {cols}x{rows} cells raw={raw_pixels} zlib={} direct_escape={} t=s_name_escape~80",
            compressed.len(),
            frame_direct(&compressed).len()
        );
        eprintln!(
            "cpu {rounds} rounds: direct_only={direct:?} shm_only={shared:?} shipped_direct_plus_rewrite={shipped:?}"
        );
    }

    fn frame_direct(bytes: &[u8]) -> String {
        let payload = base64::engine::general_purpose::STANDARD.encode(bytes);
        let chunks: Vec<_> = payload.as_bytes().chunks(4096).collect();
        let mut out = String::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let more = usize::from(i + 1 < chunks.len());
            if i == 0 {
                out.push_str(&format!("\x1b_Ga=T,f=32,t=d,o=z,s=1,v=1,i=7,m={more};"));
            } else {
                out.push_str(&format!("\x1b_Gm={more};"));
            }
            out.push_str(std::str::from_utf8(chunk).unwrap());
            out.push_str("\x1b\\");
        }
        out
    }

    fn cpu_for(body: impl FnOnce()) -> std::time::Duration {
        let start = proc_cpu();
        body();
        proc_cpu().saturating_sub(start)
    }

    fn proc_cpu() -> std::time::Duration {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
        std::time::Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
    }
}

#[cfg(test)]
pub(crate) fn guard() -> tests::Guard {
    tests::guard()
}
