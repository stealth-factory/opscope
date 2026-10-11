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
//!
//! `shm_unlink` is not async-signal-safe, so a signal handler only disarms
//! the transport. Exit, panic and resume unlink from ordinary code. A name
//! is published in the cleanup table before the object is truncated or
//! copied, and a replaced object stays until its read window ends so a
//! later reply cannot collect the replacement that reused the image id.
//! Darwin stores the page-rounded length in the object; the escape carries
//! `S=` so the terminal reads the exact payload. Tests map that object.
//! `read` is not a source: Darwin shm descriptors reject it.

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use base64::Engine;

/// Not an owned placement and not the graphics query (`2147483646`).
pub(crate) const PROBE_ID: u32 = 2_147_483_645;

const NAME_CAP: usize = 31;
const SLOTS: usize = 48;
/// How long a replaced object stays after a newer one is published for the
/// same image. The terminal may still be reading the previous name.
const RETIRE_MS: u64 = 1_000;
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

#[derive(Clone, Copy)]
struct Slot {
    live: bool,
    id: u32,
    /// Zero while this is the object the next reply should keep. A deadline
    /// means a newer object already owns the image id.
    retire_ms: u64,
    name: [u8; NAME_CAP],
    name_len: u8,
}

const fn empty_slot() -> Slot {
    Slot {
        live: false,
        id: 0,
        retire_ms: 0,
        name: [0; NAME_CAP],
        name_len: 0,
    }
}

static TABLE: Mutex<[Slot; SLOTS]> = Mutex::new([empty_slot(); SLOTS]);
static TRANSPORT: AtomicU8 = AtomicU8::new(DIRECT);
static NAME_SEQ: AtomicU32 = AtomicU32::new(1);
/// Plot slots whose latest transmission was `t=s` and has not been replaced
/// by a `t=d` frame. A queued error for one of these stays a transport
/// failure after the first error clears `use_shm`.
static SHARED_PENDING: AtomicU32 = AtomicU32::new(0);
static STARTED: OnceLock<Instant> = OnceLock::new();

fn mono_ms() -> u64 {
    STARTED
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn with_table<T>(body: impl FnOnce(&mut [Slot; SLOTS]) -> T) -> T {
    let mut table = TABLE.lock().unwrap_or_else(|err| err.into_inner());
    body(&mut table)
}

pub(crate) fn is_shared() -> bool {
    TRANSPORT.load(Ordering::Acquire) == SHARED
}

/// Signal handlers stop here. `shm_unlink` is not async-signal-safe, so the
/// handler can still restore the terminal and exit. Resume and `Drop` unlink.
pub(crate) fn disarm_for_signal() {
    TRANSPORT.store(DIRECT, Ordering::Release);
}

/// Drop every object and stay on the direct path. Resume and text mode
/// both start from here, so a previous probe cannot leak into the next one.
pub(crate) fn force_direct() {
    disarm_for_signal();
    release();
}

/// A 1×1 image, transmitted and not placed, with a reply requested (`q=0`).
/// `None` on an OS without the calls, or when the object cannot be created:
/// the session then never leaves `t=d`.
pub(crate) fn begin_probe() -> Option<String> {
    let name = create(&[0, 0, 0, 0], PROBE_ID)?;
    TRANSPORT.store(UNPROBED, Ordering::Release);
    let payload = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
    Some(format!(
        "\x1b_Ga=t,f=32,t=s,s=1,v=1,i={PROBE_ID},q=0,S=4;{payload}\x1b\\"
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

/// A later image failed while shared memory was in use. Graphics stay on.
/// Objects the terminal may still be reading are retired, not unlinked.
pub(crate) fn transport_failed() {
    TRANSPORT.store(DIRECT, Ordering::Release);
    let deadline = mono_ms().saturating_add(RETIRE_MS);
    with_table(|slots| {
        for slot in slots.iter_mut() {
            if slot.live && slot.retire_ms == 0 {
                slot.retire_ms = deadline;
            }
        }
    });
}

/// Ids whose latest send was shared memory. Cleared when a later frame
/// transmits that id directly.
pub(crate) fn shared_pending(id: u32) -> bool {
    plot_bit(id).is_some_and(|bit| SHARED_PENDING.load(Ordering::Acquire) & bit != 0)
}

pub(crate) fn mark_shared(id: u32) {
    if let Some(bit) = plot_bit(id) {
        SHARED_PENDING.fetch_or(bit, Ordering::Release);
    }
}

pub(crate) fn mark_direct(id: u32) {
    if let Some(bit) = plot_bit(id) {
        SHARED_PENDING.fetch_and(!bit, Ordering::Release);
    }
}

fn plot_bit(id: u32) -> Option<u32> {
    // Main owns the id layout (five slot bits, prefix 0x60000000). Matching
    // it here is what lets a queued shared-memory error stay a transport
    // failure instead of a graphics rejection.
    let slot = (id & 0x1f) as usize;
    if slot >= 32 {
        return None;
    }
    (id == super::graphics::image_id(slot)).then_some(1u32 << slot)
}

/// Rewrite direct transmissions when the probe succeeded. Deletes in
/// `before` retire the name they replaced; they do not unlink it while the
/// terminal may still be reading. A failure to publish puts the original
/// direct bytes back and leaves shared memory off.
pub(crate) fn deliver(before: &str, after: String) -> String {
    reap_expired();
    if !is_shared() {
        note_transport(&after, false);
        return after;
    }
    for id in command_ids(before, "a=d") {
        retire_id(id);
    }
    match rewrite(&after) {
        Ok(rewritten) => {
            note_transport(&rewritten, true);
            rewritten
        }
        Err(()) => {
            transport_failed();
            note_transport(&after, false);
            after
        }
    }
}

fn note_transport(bytes: &str, shared: bool) {
    let mut pos = 0;
    while let Some((_start, end, keys, _payload)) = next_apc(bytes, pos) {
        if keys.split(',').any(|part| part == "a=T") {
            if let Some(id) = image_id_of(keys) {
                if shared {
                    mark_shared(id);
                } else {
                    mark_direct(id);
                }
            }
        }
        pos = end;
    }
}

/// `Keyboard` drop, resume and a normal exit end here. Not a signal handler.
pub(crate) fn release() {
    with_table(|slots| {
        for slot in slots.iter_mut() {
            unlink_slot(slot);
        }
    });
}

pub(crate) fn reap_expired() {
    let now = mono_ms();
    with_table(|slots| reap_in(slots, now));
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
        out.push_str(&retarget(keys, raw.len()));
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

fn retarget(keys: &str, nbytes: usize) -> String {
    let mut out = String::with_capacity(keys.len() + 16);
    let mut seen_m = false;
    for part in keys.split(',') {
        if part.is_empty() || part.starts_with("S=") {
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
    // Darwin's fstat size is the page-rounded object. `S` is the payload.
    out.push_str(&format!(",S={nbytes}"));
    out
}

/// Publish `name` before the caller truncates or copies. The previous object
/// for `id` is retired, not unlinked: the terminal may still be reading it.
/// The name bytes are stored before `live` is set, under the table lock, so
/// cleanup cannot observe a slot it would skip and then forget.
fn claim(id: u32, name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > NAME_CAP {
        return false;
    }
    let now = mono_ms();
    with_table(|slots| {
        reap_in(slots, now);
        let deadline = now.saturating_add(RETIRE_MS);
        for slot in slots.iter_mut() {
            if slot.live && slot.id == id && slot.retire_ms == 0 {
                slot.retire_ms = deadline;
            }
        }
        let Some(slot) = slots.iter_mut().find(|slot| !slot.live) else {
            return false;
        };
        slot.name[..bytes.len()].copy_from_slice(bytes);
        slot.name_len = bytes.len() as u8;
        slot.id = id;
        slot.retire_ms = 0;
        slot.live = true;
        true
    })
}

fn retire_id(id: u32) {
    let deadline = mono_ms().saturating_add(RETIRE_MS);
    with_table(|slots| {
        for slot in slots.iter_mut() {
            if slot.live && slot.id == id && slot.retire_ms == 0 {
                slot.retire_ms = deadline;
            }
        }
    });
}

pub(crate) fn unlink_id(id: u32) {
    with_table(|slots| {
        for slot in slots.iter_mut() {
            if slot.live && slot.id == id {
                unlink_slot(slot);
            }
        }
    });
}

/// An `ENOENT` for an image id drops only objects already replaced. The
/// current object reused that id and is still the one to keep.
pub(crate) fn unlink_retired(id: u32) {
    with_table(|slots| {
        for slot in slots.iter_mut() {
            if slot.live && slot.id == id && slot.retire_ms != 0 {
                unlink_slot(slot);
            }
        }
    });
}

fn reap_in(slots: &mut [Slot; SLOTS], now: u64) {
    for slot in slots.iter_mut() {
        if slot.live && slot.retire_ms != 0 && now >= slot.retire_ms {
            unlink_slot(slot);
        }
    }
}

fn drop_named(name: &str) {
    let bytes = name.as_bytes();
    with_table(|slots| {
        for slot in slots.iter_mut() {
            if slot.live && slot.name_len as usize == bytes.len() && &slot.name[..bytes.len()] == bytes
            {
                unlink_slot(slot);
            }
        }
    });
}

fn unlink_slot(slot: &mut Slot) {
    if !slot.live {
        return;
    }
    let len = slot.name_len as usize;
    if (1..=NAME_CAP).contains(&len) {
        let mut buf = [0u8; NAME_CAP + 1];
        buf[..len].copy_from_slice(&slot.name[..len]);
        unlink_cstr(&buf);
    }
    slot.name_len = 0;
    slot.retire_ms = 0;
    slot.live = false;
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
        let n = NAME_SEQ.fetch_add(1, Ordering::Relaxed);
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
        // The cleanup table learns the name before truncate, map or copy.
        // A panic while filling the object can still unlink it.
        if !claim(id, &name) {
            unsafe {
                libc::close(fd);
                libc::shm_unlink(c_name.as_ptr());
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
            unsafe { libc::close(fd) };
            drop_named(&name);
            return None;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast(), map_len);
            let unmapped = libc::munmap(ptr, map_len);
            libc::close(fd);
            if unmapped != 0 {
                drop_named(&name);
                return None;
            }
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
pub(crate) fn expire_retired() {
    with_table(|slots| reap_in(slots, u64::MAX));
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

    /// Darwin shm descriptors use `fo_no_read`, so `read` fails there. The
    /// bytes are only visible through `mmap`, which is how the terminal
    /// reads them. Darwin's `fstat` size is the page-rounded object, so
    /// callers compare the payload prefix rather than the mapped length.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn read_object(name: &str) -> Option<Vec<u8>> {
        let c_name = std::ffi::CString::new(name).ok()?;
        let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
        if fd < 0 {
            return None;
        }
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::fstat(fd, &mut stat) } == 0;
        let len = usize::try_from(stat.st_size).unwrap_or(0);
        if !ok || len == 0 {
            unsafe { libc::close(fd) };
            return None;
        }
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            unsafe { libc::close(fd) };
            return None;
        }
        let buf = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) }.to_vec();
        unsafe {
            libc::munmap(ptr, len);
            libc::close(fd);
        }
        Some(buf)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn assert_object(name: &str, expected: &[u8]) {
        let got = read_object(name).unwrap_or_else(|| panic!("{name} could not be mapped"));
        assert!(
            got.len() >= expected.len(),
            "{name} mapped {} bytes, payload {}",
            got.len(),
            expected.len()
        );
        if let Some(i) = got[..expected.len()]
            .iter()
            .zip(expected)
            .position(|(a, b)| a != b)
        {
            panic!(
                "{name} differs at byte {i}: got {:#x} want {:#x}",
                got[i], expected[i]
            );
        }
    }

    /// `read` on a Darwin shm descriptor is `fo_no_read` and returns nothing.
    /// The bytes the terminal sees are the mapping. Where `read` does return
    /// the payload, the mapping is that same length; Darwin's `fstat` size is
    /// the page-rounded object, so the mapping is longer and `read` is absent.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn posix_read(name: &str) -> Option<Vec<u8>> {
        let c_name = std::ffi::CString::new(name).ok()?;
        let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
        if fd < 0 {
            return None;
        }
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::fstat(fd, &mut stat) } == 0;
        let len = usize::try_from(stat.st_size).unwrap_or(0);
        if !ok || len == 0 {
            unsafe { libc::close(fd) };
            return None;
        }
        let mut buf = vec![0u8; len];
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), len) };
        unsafe { libc::close(fd) };
        if n <= 0 {
            return None;
        }
        buf.truncate(n as usize);
        Some(buf)
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
        assert!(rewritten.contains(&format!("S={}", raw.len())));
        let name = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(payload_of(&rewritten))
                .unwrap(),
        )
        .unwrap();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            assert_object(&name, &raw);
            let again = rewrite(&command).expect("replacement");
            let name2 = String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(payload_of(&again))
                    .unwrap(),
            )
            .unwrap();
            assert_ne!(name, name2);
            assert!(
                exists(&name),
                "the object still being read was unlinked with its image id"
            );
            assert_object(&name2, &raw);
            unlink_retired(42);
            assert!(!exists(&name), "the retired object survived ENOENT");
            assert!(exists(&name2), "ENOENT unlinked the replacement");
            expire_retired();
            assert!(exists(&name2));
        }
        release();
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
        assert!(rewritten.contains(&format!("S={}", compressed.len())));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let name = String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(payload_of(&rewritten))
                    .unwrap(),
            )
            .unwrap();
            assert_object(&name, &compressed);
            release();
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
    fn the_payload_is_mapped_because_read_is_not_required() {
        let _guard = guard();
        let raw: Vec<u8> = (0..100).collect();
        let Some(name) = create(&raw, 5) else {
            return;
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let mapped = read_object(&name).expect("mapped");
            assert_eq!(&mapped[..raw.len()], raw.as_slice());
            match posix_read(&name) {
                // Darwin: fo_no_read, and fstat reports the page-rounded size.
                None => assert!(
                    mapped.len() > raw.len(),
                    "read() saw nothing, but the object was not page-rounded"
                ),
                Some(bytes) => {
                    assert_eq!(bytes, raw);
                    assert_eq!(mapped.len(), raw.len());
                }
            }
        }
        release();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(!exists(&name));
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
        // Unwind alone does not collect a POSIX object. Panic and a normal
        // exit reach `release` from `graphics::cleanup` (`Keyboard` drop).
        // A signal handler only disarms: `shm_unlink` is not async-signal-safe.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(exists(&name));
        disarm_for_signal();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(exists(&name), "the signal path unlinked an object");
        release();
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
