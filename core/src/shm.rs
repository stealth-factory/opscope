//! Shared-memory image transport for a terminal on this machine.
//!
//! The direct path (`t=d`) base64-encodes every image into the escape.
//! Kitty can instead read a POSIX shared-memory object (`t=s`) and unlink
//! it. That only works when the terminal and this process share a host —
//! over SSH the terminal answers `EBADF` — so one probe decides, and a
//! later transport failure drops back to `t=d` without turning graphics
//! off. A full cleanup table is not that failure: that frame stays direct
//! and the next one may use shared memory again.
//!
//! `image()` chooses the transport before compression. Shared memory carries
//! raw RGBA pixels, avoiding zlib and base64 work on both sides. The direct
//! fallback compresses and encodes only when it is actually needed.
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
/// One generation of plots, the generation it replaced, and the probe.
const SLOTS: usize = 2 * super::graphics::MAX_PLOTS + 1;
/// How long a replaced object stays after a newer one is published for the
/// same image. The terminal may still be reading the previous name.
const RETIRE_MS: u64 = 1_000;
/// A transmission's reply is no longer matched after this. The queue would
/// otherwise treat a later frame's error as the one that already left.
const REPLY_MS: u64 = 8_000;
const PENDING_CAP: usize = 8;
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
/// Outstanding replies keyed by placement, one bounded table per owned image.
/// A full table does not drop an upload that is still owed a reply: that
/// frame falls back, and only a superseded entry may be removed to make room.
static PENDING: Mutex<[Pending; super::graphics::MAX_PLOTS]> =
    Mutex::new([Pending::new(); super::graphics::MAX_PLOTS]);
static STARTED: OnceLock<Instant> = OnceLock::new();
/// Test seam: refuse opening (1) or backing allocation after publication (2).
#[cfg(test)]
static FAIL_OS: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy)]
struct Pending {
    len: u8,
    shared: [bool; PENDING_CAP],
    /// Not owed. Its reply is dropped, and the cap may reuse the slot.
    /// A newer transmit does not set this: that reply can still arrive.
    superseded: [bool; PENDING_CAP],
    placement: [u32; PENDING_CAP],
    at_ms: [u64; PENDING_CAP],
}

impl Pending {
    const fn new() -> Self {
        Self {
            len: 0,
            shared: [false; PENDING_CAP],
            superseded: [false; PENDING_CAP],
            placement: [0; PENDING_CAP],
            at_ms: [0; PENDING_CAP],
        }
    }
}

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

/// Herdr hosts the pane and redraws its images in a client of its own. Its
/// server shares this machine, so it answers the shared-memory probe `OK`,
/// and then nothing an upload sends through `t=s` reaches the screen - while
/// the same image sent inline does. Under Herdr the probe is not sent, so
/// every image goes inline. Read from the environment Herdr gives its panes,
/// because the probe's answer is the thing that cannot be believed there.
pub(crate) fn host_relays_images(var: impl Fn(&str) -> Option<String>) -> bool {
    var("HERDR_ENV").is_some_and(|value| value == "1")
}

/// A 1×1 query (`a=q`), not a stored transmit. `q=0` asks for the reply.
/// The terminal checks that it can read the object and does not keep the
/// image, so this id cannot replace another one. `None` on an OS without
/// the calls, or when the object cannot be created: the session then never
/// leaves `t=d`.
pub(crate) fn begin_probe() -> Option<String> {
    let name = create(&[0, 0, 0, 0], PROBE_ID).ok()?;
    TRANSPORT.store(UNPROBED, Ordering::Release);
    let payload = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
    Some(format!(
        "\x1b_Ga=q,f=32,t=s,s=1,v=1,i={PROBE_ID},q=0,S=4;{payload}\x1b\\"
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

pub(crate) fn mark_shared(id: u32, placement: u32) -> bool {
    push_pending(id, placement, true)
}

pub(crate) fn mark_direct(id: u32, placement: u32) -> bool {
    push_pending(id, placement, false)
}

/// Drop uploads that are no longer owed. A reply for one is ignored, and
/// the cap may reuse the slot. Transmit does not call this.
#[cfg(test)]
pub(crate) fn supersede(id: u32) {
    let Some(slot) = plot_slot(id) else {
        return;
    };
    with_pending(|rows| {
        let row = &mut rows[slot];
        for i in 0..row.len as usize {
            row.superseded[i] = true;
        }
    });
}

/// Match exactly the placement echoed by an upload response. An unknown or
/// superseded placement is dropped and does not consume a newer upload.
pub(crate) fn take_reply(id: u32, placement: u32) -> Option<bool> {
    let slot = plot_slot(id)?;
    with_pending(|rows| {
        expire_pending(&mut rows[slot], mono_ms());
        let row = &mut rows[slot];
        let at = row.placement[..row.len as usize]
            .iter()
            .position(|p| *p == placement)?;
        let superseded = row.superseded[at];
        let shared = row.shared[at];
        remove_pending(row, at);
        if superseded { None } else { Some(shared) }
    })
}

fn plot_slot(id: u32) -> Option<usize> {
    let slot = (id & 0x1f) as usize;
    if slot >= super::graphics::MAX_PLOTS {
        return None;
    }
    (id == super::graphics::image_id(slot)).then_some(slot)
}

fn with_pending<T>(body: impl FnOnce(&mut [Pending; super::graphics::MAX_PLOTS]) -> T) -> T {
    let mut rows = PENDING.lock().unwrap_or_else(|err| err.into_inner());
    body(&mut rows)
}

/// Room for one more owed upload. An id this process does not own is not
/// tracked, so it does not consume a slot.
fn pending_room(id: u32) -> bool {
    let Some(slot) = plot_slot(id) else {
        return true;
    };
    with_pending(|rows| {
        let row = &mut rows[slot];
        expire_pending(row, mono_ms());
        (row.len as usize) < PENDING_CAP || row_has_superseded(row)
    })
}

fn row_has_superseded(row: &Pending) -> bool {
    row.superseded[..row.len as usize].contains(&true)
}

fn push_pending(id: u32, placement: u32, shared: bool) -> bool {
    let Some(slot) = plot_slot(id) else {
        return true;
    };
    with_pending(|rows| {
        let row = &mut rows[slot];
        expire_pending(row, mono_ms());
        if row.len as usize == PENDING_CAP {
            let Some(at) = (0..row.len as usize).find(|&i| row.superseded[i]) else {
                return false;
            };
            remove_pending(row, at);
        }
        let at = row.len as usize;
        row.shared[at] = shared;
        row.superseded[at] = false;
        row.placement[at] = placement;
        row.at_ms[at] = mono_ms();
        row.len += 1;
        true
    })
}

fn expire_pending(row: &mut Pending, now: u64) {
    while row.len > 0 && now.saturating_sub(row.at_ms[0]) >= REPLY_MS {
        remove_pending(row, 0);
    }
}

fn remove_pending(row: &mut Pending, at: usize) {
    if row.len == 0 {
        return;
    }
    let n = row.len as usize;
    for i in at + 1..n {
        row.shared[i - 1] = row.shared[i];
        row.superseded[i - 1] = row.superseded[i];
        row.placement[i - 1] = row.placement[i];
        row.at_ms[i - 1] = row.at_ms[i];
    }
    row.len -= 1;
}

fn clear_pending() {
    with_pending(|rows| *rows = [Pending::new(); super::graphics::MAX_PLOTS]);
}

/// Publish raw RGB or RGBA pixels before the direct path does compression or framing.
/// A full table falls back for this image only. An OS refusal turns the
/// transport off; neither case disables graphics.
pub(crate) fn publish_pixels(keys: &str, pixels: &[u8], id: u32, placement: u32, format: u8) -> Option<String> {
    if !is_shared() {
        return None;
    }
    // Check before creating the object. A full reply table must not retire
    // the upload the terminal may still be reading.
    if !pending_room(id) {
        return None;
    }
    let name = match create(pixels, id) {
        Ok(name) => name,
        Err(Publish::Busy) => return None,
        Err(Publish::Failed) => {
            transport_failed();
            return None;
        }
    };
    if !mark_shared(id, placement) {
        drop_named(&name);
        return None;
    }
    let payload = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
    // Darwin rounds fstat's object size to a page; S is the exact pixel size.
    Some(format!(
        "\x1b_Ga=T,f={format},t=s,{keys},S={},m=0;{payload}\x1b\\",
        pixels.len()
    ))
}

/// `Keyboard` drop, resume and a normal exit end here. Not a signal handler.
pub(crate) fn release() {
    with_table(|slots| {
        for slot in slots.iter_mut() {
            unlink_slot(slot);
        }
    });
    clear_pending();
}

pub(crate) fn reap_expired() {
    let now = mono_ms();
    with_table(|slots| reap_in(slots, now));
    with_pending(|rows| {
        for row in rows.iter_mut() {
            expire_pending(row, now);
        }
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Publish {
    /// No free cleanup slot. Shared memory still works for a later image.
    Busy,
    /// `shm_open`, `ftruncate` or `mmap` refused the object.
    Failed,
}

#[cfg(test)]
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

#[cfg(test)]
fn is_direct_transmit(keys: &str) -> bool {
    keys.split(',').any(|p| p == "a=T") && keys.split(',').any(|p| p == "t=d")
}

#[cfg(test)]
fn is_continuation(keys: &str) -> bool {
    matches!(keys, "m=0" | "m=1")
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

pub(crate) fn retire_id(id: u32) {
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
            if slot.live
                && slot.name_len as usize == bytes.len()
                && &slot.name[..bytes.len()] == bytes
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
fn create(bytes: &[u8], id: u32) -> Result<String, Publish> {
    #[cfg(test)]
    let failure = FAIL_OS.swap(0, Ordering::AcqRel);
    #[cfg(test)]
    if failure == 1 {
        return Err(Publish::Failed);
    }
    if bytes.is_empty() {
        return Err(Publish::Failed);
    }
    for _ in 0..8 {
        let n = NAME_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = unsafe { libc::getpid() } as u32;
        let name = format!("/o{pid:x}-{n:x}");
        if name.len() > NAME_CAP {
            return Err(Publish::Failed);
        }
        let c_name = match std::ffi::CString::new(name.clone()) {
            Ok(c_name) => c_name,
            Err(_) => return Err(Publish::Failed),
        };
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
            return Err(Publish::Failed);
        }
        // The cleanup table learns the name before truncate, map or copy.
        // A panic while filling the object can still unlink it.
        if !claim(id, &name) {
            unsafe {
                libc::close(fd);
                libc::shm_unlink(c_name.as_ptr());
            }
            return Err(Publish::Busy);
        }
        let map_len = bytes.len();
        let failed = unsafe { libc::ftruncate(fd, map_len as libc::off_t) } != 0;
        // Linux tmpfs can accept ftruncate/mmap with no space left, then
        // SIGBUS during the copy. Reserve the backing pages first so an
        // exhausted /dev/shm becomes an ordinary direct fallback.
        #[cfg(target_os = "linux")]
        let failed = failed || unsafe { libc::posix_fallocate(fd, 0, map_len as libc::off_t) } != 0;
        #[cfg(test)]
        let failed = failed || failure == 2;
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
            return Err(Publish::Failed);
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast(), map_len);
            let unmapped = libc::munmap(ptr, map_len);
            libc::close(fd);
            if unmapped != 0 {
                drop_named(&name);
                return Err(Publish::Failed);
            }
        }
        return Ok(name);
    }
    Err(Publish::Failed)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn create(_bytes: &[u8], _id: u32) -> Result<String, Publish> {
    Err(Publish::Failed)
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
    fn herdr_panes_never_probe_shared_memory() {
        let herdr = |key: &str| (key == "HERDR_ENV").then(|| "1".to_string());
        assert!(host_relays_images(herdr));
        assert!(!host_relays_images(|_: &str| None));
        let other = |key: &str| (key == "HERDR_ENV").then(|| "0".to_string());
        assert!(!host_relays_images(other));
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

    fn live_names(id: u32) -> Vec<String> {
        with_table(|slots| {
            slots
                .iter()
                .filter(|slot| slot.live && slot.id == id)
                .map(|slot| {
                    let len = slot.name_len as usize;
                    String::from_utf8_lossy(&slot.name[..len]).into_owned()
                })
                .collect()
        })
    }

    #[test]
    fn the_probe_is_a_query_so_the_terminal_does_not_store_it() {
        let _guard = guard();
        let Some(probe) = begin_probe() else {
            return;
        };
        assert!(
            probe.contains("a=q,"),
            "the probe transmits an image the terminal would store"
        );
        assert!(!probe.contains("a=t"));
        assert!(probe.contains("t=s"));
    }

    #[test]
    fn a_full_cleanup_table_falls_back_per_image_and_stays_shared() {
        let _guard = guard();
        assert!(SLOTS >= 2 * super::super::graphics::MAX_PLOTS + 1);
        let Some(_) = begin_probe() else {
            return;
        };
        apply_accepted();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            for i in 0..SLOTS - 1 {
                create(&[1, 2, 3, 4], 100_000 + i as u32).expect("fill");
            }
            let id = super::super::graphics::image_id(0);
            let id2 = super::super::graphics::image_id(1);
            assert!(publish_pixels("q=0", &[1, 2, 3, 4], id, 1, 32).is_some());
            assert!(publish_pixels("q=0", &[1, 2, 3, 4], id2, 1, 32).is_none());
            assert!(is_shared(), "a full table turned shared memory off");
            assert_eq!(live_names(id).len(), 1, "the sent image lost its object");
            assert!(
                live_names(id2).is_empty(),
                "the refused image left an object"
            );
            assert_eq!(take_reply(id, 1), Some(true));
            assert_eq!(take_reply(id2, 1), None, "unsent image queued a reply");
            unlink_id(id);
            assert!(publish_pixels("q=0", &[1, 2, 3, 4], id2, 1, 32).is_some());
        }
    }

    #[test]
    fn an_os_refusal_turns_shared_memory_off() {
        let _guard = guard();
        let Some(_) = begin_probe() else {
            return;
        };
        apply_accepted();
        FAIL_OS.store(1, Ordering::Release);
        assert!(publish_pixels("q=0", &[1, 2, 3, 4], 7, 1, 32).is_none());
        assert!(!is_shared(), "an OS refusal left shared memory on");
    }

    #[test]
    fn a_backing_allocation_refusal_unlinks_the_published_object() {
        let _guard = guard();
        let Some(_) = begin_probe() else { return; };
        apply_accepted();
        let id = super::super::graphics::image_id(0);
        let sequence = NAME_SEQ.load(Ordering::Acquire);
        let name = format!("/o{:x}-{sequence:x}", unsafe { libc::getpid() });
        FAIL_OS.store(2, Ordering::Release);
        assert!(publish_pixels("q=0", &[1, 2, 3, 4], id, 1, 32).is_none());
        assert!(!is_shared());
        assert!(live_names(id).is_empty());
        assert_eq!(take_reply(id, 1), None);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(!exists(&name), "failed allocation left a shared-memory object");
    }

    #[test]
    fn raw_pixels_round_trip_through_one_object_and_replacements_survive() {
        let _guard = guard();
        let Some(_) = begin_probe() else {
            return;
        };
        apply_accepted();
        let raw: Vec<u8> = (0..9000).map(|i| (i * 17 + 3) as u8).collect();
        let command = publish_pixels("s=30,v=75,i=42,q=0", &raw, 42, 1, 32).expect("object");
        assert!(command.contains("t=s") && !command.contains("o=z"));
        assert!(command.contains("m=0") && !command.contains("m=1"));
        assert!(command.contains(&format!("S={}", raw.len())));
        let name = shared_name(&command);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            assert_object(&name, &raw);
            let again = publish_pixels("s=30,v=75,i=42,q=0", &raw, 42, 1, 32).expect("replacement");
            let name2 = shared_name(&again);
            assert_ne!(name, name2);
            assert!(exists(&name), "an object still being read was unlinked");
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

    fn placement_of(command: &str) -> u32 {
        command.split(",p=").nth(1).unwrap().split(',').next().unwrap().parse().unwrap()
    }

    fn shared_name(command: &str) -> String {
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(payload_of(command))
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn both_chart_transports_have_identical_cropped_pixels() {
        let _guard = guard();
        force_direct();
        let direct = super::super::graphics::direct_chart_command(24, 4);
        let pixels =
            miniz_oxide::inflate::decompress_to_vec_zlib(&direct_payload(&direct)).unwrap();
        let Some(_) = begin_probe() else {
            return;
        };
        apply_accepted();
        let shared = super::super::graphics::direct_chart_command(24, 4);
        assert!(shared.contains("t=s") && !shared.contains("t=d") && !shared.contains("o=z"));
        assert!(shared.contains(&format!("S={}", pixels.len())));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert_object(&shared_name(&shared), &pixels);
    }

    #[test]
    fn cropped_rgb_and_rgba_charts_keep_their_format_and_bytes_in_shared_memory() {
        let _guard = guard();
        let colour = super::super::rgb(90, 210, 140);
        let solid = super::super::Plot::heatmap_levels(
            2, 1, &vec![vec![Some((colour.clone(), 4)); 64]; 10], &colour,
        );
        let mut cells = vec![vec![Some((colour.clone(), 4)); 64]; 10];
        cells[5][10] = None;
        let gapped = super::super::Plot::heatmap_levels(2, 1, &cells, &colour);
        let line = super::super::Plot::new(2, 1, 64, 10, super::super::LineChart {
            slots: 3,
            traces: vec![super::super::Trace {
                positions: None, values: vec![Some(0.2), Some(0.8), Some(0.5)],
                colour, baseline: None,
            }],
            focus: None,
        });
        for (plot, format) in [(solid, 24), (gapped, 32), (line, 32)] {
            force_direct();
            let plot = plot.in_viewport(5, 1, 5).unwrap();
            let direct = super::super::graphics::cropped_chart_command(&plot, 42);
            assert!(direct.contains(&format!("f={format},t=d")));
            assert!(direct.contains("s=320,v=80") && direct.contains("c=40,r=5"));
            let bytes = miniz_oxide::inflate::decompress_to_vec_zlib(&direct_payload(&direct)).unwrap();
            assert_eq!(bytes.len(), 320 * 80 * (format as usize / 8));
            let Some(_) = begin_probe() else { return; };
            apply_accepted();
            let shared = super::super::graphics::cropped_chart_command(&plot, 42);
            assert!(shared.contains(&format!("f={format},t=s")));
            assert!(shared.contains(&format!("S={}", bytes.len())));
            assert!(shared.contains("s=320,v=80") && shared.contains("c=40,r=5"));
            assert!(!shared.contains("o=z"));
            assert_ne!(placement_of(&direct), placement_of(&shared));
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            assert_object(&shared_name(&shared), &bytes);
        }
    }

    #[test]
    fn tiny_images_stay_direct_without_disabling_shared_memory() {
        let _guard = guard();
        let Some(_) = begin_probe() else { return; };
        apply_accepted();
        let id = super::super::graphics::image_id(3);
        let small = super::super::graphics::direct_chart_command(16, 1);
        assert!(small.contains("t=d"));
        assert_eq!(take_reply(id, placement_of(&small)), Some(false));
        let large = super::super::graphics::direct_chart_command(32, 1);
        assert!(large.contains("t=s"));
        assert_eq!(take_reply(id, placement_of(&large)), Some(true));
        assert!(is_shared());
    }

    #[test]
    fn replies_match_their_placement_even_after_eviction_expiry_or_reordering() {
        let _guard = guard();
        let id = super::super::graphics::image_id(0);
        assert!(mark_shared(id, 1));
        assert!(mark_direct(id, 2));
        assert!(mark_direct(id, 3));
        // The first reply never arrives. The later one still matches itself.
        assert_eq!(
            take_reply(id, 3),
            Some(false),
            "out-of-order reply consumed another entry"
        );
        assert_eq!(
            take_reply(id, 1),
            Some(true),
            "a dropped reply was attributed to a later upload"
        );
        with_pending(|rows| expire_pending(&mut rows[0], mono_ms() + REPLY_MS));
        assert!(mark_shared(id, 10));
        assert_eq!(take_reply(id, 2), None, "expired reply matched a newer entry");
        assert_eq!(take_reply(id, 10), Some(true));
    }

    #[test]
    fn a_full_pending_cap_keeps_owed_replies_and_evicts_only_superseded() {
        let _guard = guard();
        let id = super::super::graphics::image_id(0);
        assert!(mark_shared(id, 1));
        for p in 2..=PENDING_CAP as u32 {
            assert!(mark_direct(id, p), "upload {p} was refused before the table filled");
        }
        assert!(
            !mark_direct(id, 9),
            "the ninth upload discarded an owed reply"
        );
        assert_eq!(take_reply(id, 1), Some(true), "cap dropped an owed shared reply");
        assert_eq!(take_reply(id, 9), None, "an unrecorded upload was matched");
        assert_eq!(take_reply(id, 2), Some(false));

        release();
        assert!(mark_shared(id, 1));
        supersede(id);
        for p in 2..=PENDING_CAP as u32 {
            assert!(mark_direct(id, p));
        }
        assert!(
            mark_shared(id, 9),
            "a superseded slot was not reused"
        );
        assert_eq!(take_reply(id, 1), None, "a superseded reply was applied");
        assert_eq!(take_reply(id, 2), Some(false), "an owed reply was evicted");
        assert_eq!(take_reply(id, 9), Some(true));
    }

    #[test]
    fn deleting_a_plot_retires_its_old_object_before_publishing_the_new_one() {
        let _guard = guard();
        let Some(_) = begin_probe() else {
            return;
        };
        apply_accepted();
        let colour = super::super::rgb(120, 196, 250);
        let plot =
            super::super::Plot::gradient_meter(2, 1, 40, 0.5, None, &colour, &colour, &colour);
        let rows = vec!["title".into(); 4];
        let (_, _, first) =
            super::super::graphics::compose(&rows, &[plot.clone()], 44, 4, true, true, false);
        let old = shared_name(&first);
        let (_, before, second) =
            super::super::graphics::compose(&rows, &[plot], 44, 4, true, true, false);
        let new = shared_name(&second);
        assert!(before.contains("a=d"));
        assert_ne!(old, new);
        expire_retired();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            assert!(!exists(&old));
            assert!(exists(&new), "deletion retired the replacement");
        }
        super::super::graphics::cleanup();
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
        let Ok(name) = create(&raw, 5) else {
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
        let Ok(name) = create(b"still-here", 99) else {
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
        let rounds = 40u32;
        for (cols, rows) in [(4, 1), (8, 1), (16, 1), (32, 1), (24, 4), (70, 8), (140, 16)] {
            force_direct();
            let sample = super::super::graphics::direct_chart_command(cols, rows);
            let direct = cpu_for(|| {
                for _ in 0..rounds {
                    std::hint::black_box(super::super::graphics::direct_chart_command(cols, rows));
                }
            });
            let Some(_) = begin_probe() else {
                return;
            };
            apply_accepted();
            let shared = cpu_for(|| {
                for _ in 0..rounds {
                    std::hint::black_box(super::super::graphics::direct_chart_command(cols, rows));
                    // The terminal normally unlinks after mapping. Count the
                    // syscall here too, conservatively, to avoid table pressure.
                    unlink_id(super::super::graphics::image_id(3));
                }
            });
            eprintln!("chart {cols}x{rows} cells raw={} zlib={} direct_escape={} cpu {rounds} rounds: direct={direct:?} raw_shared={shared:?}", cols * 8 * rows * 16 * 4, direct_payload(&sample).len(), sample.len());
        }
    }

    #[test]
    #[ignore = "requires an OPSCOPE_CAPTURED_IMAGE fixture from a live widget"]
    fn measure_captured_image_transport() {
        let _guard = guard();
        let path = std::env::var("OPSCOPE_CAPTURED_IMAGE").expect("captured image JSON path");
        let sample: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let pixels = base64::engine::general_purpose::STANDARD.decode(sample["rgba"].as_str().unwrap()).unwrap();
        let w = sample["width"].as_u64().unwrap();
        let h = sample["height"].as_u64().unwrap();
        assert_eq!(pixels.len() as u64, w * h * 4);
        let id = super::super::graphics::image_id(0);
        let keys = format!("s={w},v={h},i={id},p=1,C=1,q=0");
        let rounds = 400;
        force_direct();
        let direct = cpu_for(|| {
            for _ in 0..rounds {
                std::hint::black_box(super::super::graphics::transmit_pixels(&pixels, 32, &keys, id, 1));
            }
        });
        let Some(_) = begin_probe() else { return; };
        apply_accepted();
        let shared = cpu_for(|| {
            for _ in 0..rounds {
                std::hint::black_box(super::super::graphics::transmit_pixels(&pixels, 32, &keys, id, 1));
                unlink_id(id);
            }
        });
        eprintln!("captured {w}x{h} pixels raw={} cpu {rounds} uploads: direct={direct:?} shared={shared:?}", pixels.len());
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
