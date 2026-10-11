//! Capability negotiation belongs to the terminal session, never a widget.
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub(crate) const SYNC_BEGIN: &str = "\x1b[?2026h";
pub(crate) const SYNC_END: &str = "\x1b[?2026l";
/// Disambiguate escape codes. Pushed only after `CSI ? u` is answered.
pub(crate) const KEYBOARD_PUSH: &str = "\x1b[>1u";
/// Pop one keyboard-protocol stack entry, which returns the terminal to legacy.
pub(crate) const KEYBOARD_POP: &str = "\x1b[<u";
/// Raster used when the terminal never reports a cell, or reports one that
/// is not a character cell. Today's charts are this size.
pub(crate) const FALLBACK_CELL_W: usize = 8;
pub(crate) const FALLBACK_CELL_H: usize = 16;
/// Cells in one plot at the fallback raster. A larger cell lowers the cap
/// so the uncompressed RGBA bytes of a plot stay at or under this times
/// 8×16×4.
pub(crate) const MAX_PLOT_CELLS: usize = 20_000;
/// `CSI ? u` stays immediately before `CSI c`. `CSI 16 t` follows `CSI c`.
const QUERY: &str = "\x1b_Gi=2147483646,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[?2026$p\x1b[?u\x1b[c\x1b[16t";
const TEXT_QUERY: &str = "\x1b[?2026$p\x1b[?u\x1b[c";
/// XTGETTCAP for `Smulx` (styled underlines) and `Setulc` (underline colour).
const UNDERLINE_QUERY: &str = "\x1bP+q536d756c78;536574756c63\x1b\\";
const TIMEOUT: Duration = Duration::from_millis(800);
/// Inclusive window for a reported cell. 20×40, a HiDPI Kitty cell, sits
/// inside it. Wider or taller than this, or an aspect outside 5:4..=3:1,
/// is not a cell we will rasterise.
const MIN_CELL_W: usize = 4;
const MAX_CELL_W: usize = 48;
const MIN_CELL_H: usize = 8;
const MAX_CELL_H: usize = 96;
static SYNC_SUPPORTED: AtomicBool = AtomicBool::new(false);
/// True after flag 1 was pushed, so every cleanup path pops exactly once.
static KEYBOARD_PUSHED: AtomicBool = AtomicBool::new(false);
static QUERY_CELLS: AtomicBool = AtomicBool::new(false);
static CELL_QUERY_GEN: AtomicU64 = AtomicU64::new(0);
static CELL_REPORT_GEN: AtomicU64 = AtomicU64::new(u64::MAX);
static CELL_STALE: AtomicBool = AtomicBool::new(false);
/// Set from the SIGWINCH handler. The next complete frame sends the query;
/// the handler itself does not write, so `CSI 16 t` cannot land inside an
/// image chunk.
static CELL_NEED_SEND: AtomicBool = AtomicBool::new(false);
static CELL: AtomicU32 = AtomicU32::new(((FALLBACK_CELL_H as u32) << 16) | FALLBACK_CELL_W as u32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub kitty_graphics: bool,
    pub synchronized_updates: bool,
    /// Kitty `Smulx`: curly and dashed underlines.
    pub styled_underlines: bool,
    /// Kitty `Setulc`: an underline colour distinct from the foreground.
    pub underline_colour: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum GraphicsMode {
    #[default]
    Auto,
    Text,
    Kitty,
}

#[derive(Default)]
struct Session {
    capabilities: Capabilities,
    deadline: Option<Instant>,
    mode: GraphicsMode,
    graphics_answered: bool,
    image_rejected: bool,
    /// An XTGETTCAP reply for the underline query arrived inside the window.
    underline_answered: bool,
    /// A `CSI ? u` reply arrived inside the negotiation window.
    /// Device attributes do not set this: that reply can arrive first, and
    /// the keyboard reply is still support until the deadline.
    keyboard_answered: bool,
    /// Flag 1 is the mode we pushed. Current flags in the query reply are
    /// usually 0 until that push; the reply itself is what shows support.
    keyboard_disambiguate: bool,
}

static SESSION: Mutex<Session> = Mutex::new(Session {
    capabilities: Capabilities {
        kitty_graphics: false,
        synchronized_updates: false,
        styled_underlines: false,
        underline_colour: false,
    },
    deadline: None,
    mode: GraphicsMode::Auto,
    graphics_answered: false,
    image_rejected: false,
    underline_answered: false,
    keyboard_answered: false,
    keyboard_disambiguate: false,
});

/// Query only after cbreak is active, so replies are never echoed as text.
/// No wait here: the first frames use text while normal input polling negotiates.
pub(crate) fn start() {
    let mode = match std::env::var("OPSCOPE_GRAPHICS").as_deref() {
        Ok("text") => GraphicsMode::Text,
        Ok("kitty") => GraphicsMode::Kitty,
        _ => GraphicsMode::Auto,
    };
    let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    *session = Session {
        mode,
        ..Session::default()
    };
    SYNC_SUPPORTED.store(false, Ordering::Release);
    if unsafe { libc::isatty(libc::STDOUT_FILENO) } != 1 {
        return;
    }
    session.deadline = Some(Instant::now() + TIMEOUT);
    drop(session);
    // A resumed or reclaimed session asks again. Pop first so a second push
    // does not leave flag 1 on the stack after the matching pop.
    pop_keyboard();
    // Text mode still benefits from synchronized updates, but sends no image query
    // and does not ask for a cell size it will not rasterise.
    // The keyboard query is sent either way: it is independent of graphics.
    // It goes out before device attributes. A flags reply inside the window
    // is support even when device attributes arrived first. No keyboard
    // reply before the deadline leaves the legacy decoder in place.
    let query_cells = mode != GraphicsMode::Text;
    QUERY_CELLS.store(query_cells, Ordering::Release);
    if query_cells {
        CELL_QUERY_GEN.fetch_add(1, Ordering::AcqRel);
        let (w, h) = cell_from_winsize().unwrap_or((FALLBACK_CELL_W, FALLBACK_CELL_H));
        publish_cell(w, h);
    }
    super::out(&format!(
        "{}{UNDERLINE_QUERY}",
        if mode == GraphicsMode::Text {
            TEXT_QUERY
        } else {
            QUERY
        }
    ));
    super::flush();
}

/// `CSI ? <flags> u` — the keyboard-protocol query reply.
///
/// The number is the mode currently in force, not a capability bitmask.
/// At startup that is 0 on a terminal that implements the protocol, so any
/// well-formed reply inside the window is support. Device attributes are a
/// different query and do not answer this one.
fn keyboard_answer(sequence: &str) -> Option<u32> {
    let rest = sequence.strip_prefix("\x1b[?")?.strip_suffix('u')?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

impl Session {
    /// Returns whether this reply is the one that turns disambiguation on.
    fn reply(&mut self, sequence: &str, now: Instant) -> bool {
        if let Some((keys, message)) = sequence
            .strip_prefix("\x1b_G")
            .and_then(|s| s.split_once(';'))
        {
            let id = keys
                .split(',')
                .find_map(|key| key.strip_prefix("i=")?.parse::<u32>().ok());
            // ENOENT for one of ours means the terminal dropped the image
            // (a re-place found nothing): send it again, not give up.
            if id.is_some_and(super::graphics::owns_image) && message.starts_with("ENOENT") {
                super::screen_lost();
                return false;
            }
            if id.is_some_and(super::graphics::owns_image) && message != "OK\x1b\\" {
                self.capabilities.kitty_graphics = false;
                self.image_rejected = true;
                self.graphics_answered = true;
                return false;
            }
        }
        if !self.deadline.is_some_and(|deadline| now <= deadline) {
            return false;
        }
        if let Some(response) = sequence.strip_prefix("\x1b_Gi=2147483646;") {
            if !self.graphics_answered {
                self.capabilities.kitty_graphics =
                    response == "OK\x1b\\" && self.mode != GraphicsMode::Text;
                self.graphics_answered = true;
            }
            false
        } else if sequence == "\x1b[?2026;1$y" || sequence == "\x1b[?2026;2$y" {
            self.capabilities.synchronized_updates = true;
            false
        } else if let Some(body) = sequence
            .strip_prefix("\x1bP")
            .and_then(|s| s.strip_suffix("\x1b\\"))
        {
            // `1+r` lists capabilities the terminal has; `0+r` lists those
            // it does not. Either one answers the query. A late reply is
            // already excluded by the deadline check above this arm.
            let acknowledged = body.strip_prefix("1+r");
            let declined = body.strip_prefix("0+r");
            if acknowledged.is_some() || declined.is_some() {
                self.underline_answered = true;
            }
            if let Some(rest) = acknowledged {
                for part in rest.split(';') {
                    let name = part.split('=').next().unwrap_or("").to_ascii_lowercase();
                    match name.as_str() {
                        "536d756c78" => self.capabilities.styled_underlines = true,
                        "536574756c63" => self.capabilities.underline_colour = true,
                        _ => {}
                    }
                }
            }
            false
        } else if keyboard_answer(sequence).is_some() {
            if self.keyboard_answered {
                return false;
            }
            self.keyboard_answered = true;
            self.keyboard_disambiguate = true;
            true
        } else if sequence.starts_with("\x1b[?") && sequence.ends_with('c') {
            // The DA reply follows the other queries. No earlier graphics
            // response means this path does not support it, even if TERM says kitty.
            // It does not answer the keyboard query. Terminals send these
            // two replies in either order, and a flags reply still inside
            // the window is support.
            self.graphics_answered = true;
            false
        } else {
            false
        }
    }
}

pub(crate) fn reply(sequence: &str) {
    // Outside the 800 ms capability window on purpose: a font change asks
    // again on SIGWINCH, long after startup, and the reply is still a cell size.
    if let Some(report) = cell_report(sequence) {
        let ioctl = if report.is_some() {
            None
        } else {
            cell_from_winsize()
        };
        let (w, h) = resolve_cell(report, ioctl);
        CELL_REPORT_GEN.store(CELL_QUERY_GEN.load(Ordering::Acquire), Ordering::Release);
        publish_cell(w, h);
    }
    let push = {
        let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        let push = session.reply(sequence, Instant::now());
        SYNC_SUPPORTED.store(session.capabilities.synchronized_updates, Ordering::Release);
        push
    };
    if push {
        // The flag and the bytes are one transition on this thread.
        // pthread_sigmask does not block the same signals elsewhere.
        // SIGINT, SIGTERM, SIGTSTP and SIGQUIT stay blocked here until both
        // have happened, and the flag is stored first so a handler that
        // still runs pops.
        let _hold = block_handled_signals();
        KEYBOARD_PUSHED.store(true, Ordering::Release);
        super::out(KEYBOARD_PUSH);
        super::flush();
    }
}

/// Pop flag 1 if this process pushed it. Safe to call more than once.
pub(crate) fn pop_keyboard() {
    let _hold = block_handled_signals();
    if KEYBOARD_PUSHED.swap(false, Ordering::AcqRel) {
        super::out(KEYBOARD_POP);
        super::flush();
    }
}

/// `CSI 16 t` answers `CSI 6 ; height ; width t`.
///
/// `None` means this sequence is not that reply. `Some(None)` means it is,
/// and the numbers are zero, missing or not a character cell — the caller
/// falls through to the window size, then to 8×16.
pub(crate) fn cell_report(sequence: &str) -> Option<Option<(usize, usize)>> {
    let rest = sequence.strip_prefix("\x1b[6;")?.strip_suffix('t')?;
    let (height, width) = rest.split_once(';')?;
    if height.is_empty()
        || width.is_empty()
        || height.len() > 6
        || width.len() > 6
        || !height.bytes().all(|b| b.is_ascii_digit())
        || !width.bytes().all(|b| b.is_ascii_digit())
    {
        return Some(None);
    }
    let width = width.parse::<usize>().unwrap_or(0);
    let height = height.parse::<usize>().unwrap_or(0);
    Some(sane_cell(width, height))
}

pub(crate) fn sane_cell(width: usize, height: usize) -> Option<(usize, usize)> {
    if !(MIN_CELL_W..=MAX_CELL_W).contains(&width) || !(MIN_CELL_H..=MAX_CELL_H).contains(&height) {
        return None;
    }
    // height/width from 5/4 through 3, inclusive. 8×16 and 20×40 are 2.
    if height.saturating_mul(4) < width.saturating_mul(5) || height > width.saturating_mul(3) {
        return None;
    }
    Some((width, height))
}

pub(crate) fn resolve_cell(
    report: Option<(usize, usize)>,
    ioctl: Option<(usize, usize)>,
) -> (usize, usize) {
    report.unwrap_or(ioctl.unwrap_or((FALLBACK_CELL_W, FALLBACK_CELL_H)))
}

pub(crate) fn max_plot_cells_for(cell_w: usize, cell_h: usize) -> usize {
    let area = cell_w.saturating_mul(cell_h).max(1);
    MAX_PLOT_CELLS.saturating_mul(FALLBACK_CELL_W * FALLBACK_CELL_H) / area
}

pub(crate) fn max_plot_cells() -> usize {
    let (w, h) = cell_size();
    max_plot_cells_for(w, h)
}

pub(crate) fn cell_size() -> (usize, usize) {
    let packed = CELL.load(Ordering::Acquire);
    ((packed & 0xffff) as usize, (packed >> 16) as usize)
}

fn publish_cell(width: usize, height: usize) {
    // Unit tests share one process, and several of them decode terminal
    // replies. Publishing here would change the 8×16 raster those tests
    // measure. The probe binary is not built as a test.
    #[cfg(test)]
    {
        let _ = (width, height);
        return;
    }
    #[cfg(not(test))]
    {
        let packed = ((height as u32) << 16) | (width as u32);
        if CELL.swap(packed, Ordering::AcqRel) != packed {
            super::screen_lost();
        }
    }
}

fn cell_from_winsize() -> Option<(usize, usize)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if ok != 0 || ws.ws_col == 0 || ws.ws_row == 0 || ws.ws_xpixel == 0 || ws.ws_ypixel == 0 {
        return None;
    }
    sane_cell(
        ws.ws_xpixel as usize / ws.ws_col as usize,
        ws.ws_ypixel as usize / ws.ws_row as usize,
    )
}

/// A font change in Kitty is a SIGWINCH with a new cell size. Record that
/// the next frame should ask again. Signal-safe: atomics only, no write
/// and no locks, so the query cannot split an image chunk.
pub(crate) fn note_resize() {
    if !QUERY_CELLS.load(Ordering::Acquire) {
        return;
    }
    CELL_QUERY_GEN.fetch_add(1, Ordering::AcqRel);
    CELL_STALE.store(true, Ordering::Release);
    CELL_NEED_SEND.store(true, Ordering::Release);
}

/// `CSI 16 t` for the front of the next complete frame, or empty when no
/// resize is waiting. A failed write puts the flag back.
pub(crate) fn take_cell_query() -> &'static str {
    if CELL_NEED_SEND.swap(false, Ordering::AcqRel) {
        "\x1b[16t"
    } else {
        ""
    }
}

pub(crate) fn restore_cell_query() {
    CELL_NEED_SEND.store(true, Ordering::Release);
}

/// Cell to publish for a resize that does not already have a reply.
///
/// `None` leaves a still-valid report for this resize alone. Otherwise the
/// window pixel size, or 8×16 when that report is missing, zero or absurd.
pub(crate) fn pending_cell(
    report_is_current: bool,
    ioctl: Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    if report_is_current {
        None
    } else {
        Some(ioctl.unwrap_or((FALLBACK_CELL_W, FALLBACK_CELL_H)))
    }
}

/// Use the window pixel size, or 8×16, when a resize is still waiting on
/// `CSI 16 t`. A reply that already matched this resize is left alone.
pub(crate) fn apply_pending_cell_size() {
    if !CELL_STALE.swap(false, Ordering::AcqRel) {
        return;
    }
    let report_is_current =
        CELL_REPORT_GEN.load(Ordering::Acquire) == CELL_QUERY_GEN.load(Ordering::Acquire);
    if let Some((w, h)) = pending_cell(report_is_current, cell_from_winsize()) {
        publish_cell(w, h);
    }
}

pub(crate) fn cleanup_signal() {
    // Same hold as the push: the handler can itself be interrupted, and
    // the flag clear and the pop bytes have to stay one transition.
    let _hold = block_handled_signals();
    if SYNC_SUPPORTED.load(Ordering::Acquire) {
        unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                SYNC_END.as_ptr().cast(),
                SYNC_END.len(),
            );
        }
    }
    if KEYBOARD_PUSHED.swap(false, Ordering::AcqRel) {
        unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                KEYBOARD_POP.as_ptr().cast(),
                KEYBOARD_POP.len(),
            );
        }
    }
}

/// Signals whose handlers pop the keyboard mode. Held across the push, the
/// pop, and the flag update so those handlers cannot observe a half-written
/// transition on this thread. `pthread_sigmask` does not cover other threads.
/// Restores the previous mask on drop, including from a handler.
struct HeldSignals {
    old: libc::sigset_t,
}

fn block_handled_signals() -> HeldSignals {
    unsafe {
        let mut block = std::mem::zeroed();
        libc::sigemptyset(&mut block);
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGTSTP, libc::SIGQUIT] {
            libc::sigaddset(&mut block, sig);
        }
        let mut old = std::mem::zeroed();
        libc::pthread_sigmask(libc::SIG_BLOCK, &block, &mut old);
        HeldSignals { old }
    }
}

impl Drop for HeldSignals {
    fn drop(&mut self) {
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.old, std::ptr::null_mut());
        }
    }
}

pub fn capabilities() -> Capabilities {
    SESSION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .capabilities
}

/// How far underline styles may go on this terminal right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnderlineSupport {
    /// Still inside the negotiation window. Keep today's colour-only text.
    Unknown,
    /// `Smulx` was acknowledged: curly and dashed are safe.
    Styled,
    /// The terminal declined, or the window closed without an answer.
    /// A plain underline is the legible fallback.
    Plain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UnderlineCaps {
    pub support: UnderlineSupport,
    pub colour: bool,
}

pub(crate) fn underline_caps() -> UnderlineCaps {
    let session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let support = if session.capabilities.styled_underlines {
        UnderlineSupport::Styled
    } else if session.underline_answered || session.deadline.is_some_and(|d| Instant::now() > d) {
        UnderlineSupport::Plain
    } else {
        UnderlineSupport::Unknown
    };
    UnderlineCaps {
        support,
        colour: session.capabilities.underline_colour,
    }
}

/// Explicit requests are still negotiated; never force image bytes into a
/// terminal that has not acknowledged them. Core puts the explanation on screen.
pub fn graphics_notice() -> Option<&'static str> {
    let s = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if s.image_rejected {
        return Some("Kitty image rejected; using text");
    }
    (s.mode == GraphicsMode::Kitty
        && !s.capabilities.kitty_graphics
        && (s.graphics_answered || s.deadline.is_some_and(|d| Instant::now() > d)))
    .then_some("Kitty unavailable; using text")
}

pub(crate) fn synchronized(bytes: String, enabled: bool) -> String {
    if enabled && !bytes.is_empty() {
        format!("{SYNC_BEGIN}{bytes}{SYNC_END}")
    } else {
        bytes
    }
}

/// Length of a complete terminal control string (APC/OSC/DCS/PM/SOS).
/// Incomplete strings stay buffered by the input decoder, including late replies.
pub(crate) fn string_len(chars: &[char]) -> Option<usize> {
    if chars.first() != Some(&'\x1b') || !matches!(chars.get(1), Some('_' | ']' | 'P' | '^' | 'X'))
    {
        return None;
    }
    for i in 2..chars.len() {
        if chars[i] == '\x07' || (chars[i] == '\\' && chars[i - 1] == '\x1b') {
            return Some(i + 1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_independent_and_late_replies_do_not_enable_them() {
        let now = Instant::now();
        let mut s = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        s.reply("\x1b[?2026;2$y", now);
        assert!(s.capabilities.synchronized_updates);
        assert!(!s.capabilities.kitty_graphics);
        s.reply("\x1b_Gi=2147483646;OK\x1b\\", now + TIMEOUT + TIMEOUT);
        assert!(!s.capabilities.kitty_graphics);
        s.reply("\x1b_Gi=2147483646;OK\x1b\\", now);
        assert!(s.capabilities.kitty_graphics);
    }

    #[test]
    fn underline_and_keyboard_replies_enable_independently() {
        let now = Instant::now();
        let underline = "\x1bP1+r536d756c78=1b5b343a25703125646d;536574756c63=1b5b3538\x1b\\";
        let keyboard = "\x1b[?0u";
        for mode in [GraphicsMode::Auto, GraphicsMode::Text] {
            for replies in [[underline, keyboard], [keyboard, underline]] {
                let mut s = Session {
                    mode,
                    deadline: Some(now + TIMEOUT),
                    ..Session::default()
                };
                for reply in replies {
                    assert_eq!(s.reply(reply, now), reply == keyboard);
                }
                assert!(s.capabilities.styled_underlines);
                assert!(s.capabilities.underline_colour);
                assert!(s.keyboard_disambiguate);
                assert!(!s.capabilities.kitty_graphics);
                assert!(!s.reply("\x1b[?62;c", now));
                assert!(s.keyboard_disambiguate);
            }
        }
    }

    #[test]
    fn a_dropped_image_is_resent_rather_than_ending_graphics() {
        let now = Instant::now();
        for slot in [0, 31] {
            let mut s = Session {
                deadline: Some(now + TIMEOUT),
                ..Session::default()
            };
            s.reply("\x1b_Gi=2147483646;OK\x1b\\", now);
            let ours = crate::graphics::image_id(slot);
            s.reply(&format!("\x1b_Gi={ours};ENOENT:no such image\x1b\\"), now);
            assert!(s.capabilities.kitty_graphics && !s.image_rejected);
            s.reply(&format!("\x1b_Gi={ours};EINVAL:bad\x1b\\"), now);
            assert!(!s.capabilities.kitty_graphics && s.image_rejected);
        }
    }

    #[test]
    fn negative_or_missing_graphics_reply_and_text_override_keep_text() {
        for first in ["\x1b_Gi=2147483646;ENOTSUP\x1b\\", "\x1b[?62;c"] {
            let now = Instant::now();
            let mut s = Session {
                deadline: Some(now + TIMEOUT),
                ..Session::default()
            };
            s.reply(first, now);
            s.reply("\x1b_Gi=2147483646;OK\x1b\\", now);
            assert!(!s.capabilities.kitty_graphics);
        }
        let now = Instant::now();
        let mut s = Session {
            mode: GraphicsMode::Text,
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        s.reply("\x1b_Gi=2147483646;OK\x1b\\", now);
        assert!(!s.capabilities.kitty_graphics);
    }

    #[test]
    fn underline_replies_count_only_inside_the_window() {
        let now = Instant::now();
        let mut s = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        s.reply(
            "\x1bP1+r536d756c78=1b5b343a25703125646d;536574756c63=1b5b3538\x1b\\",
            now,
        );
        assert!(s.capabilities.styled_underlines);
        assert!(s.capabilities.underline_colour);
        assert!(s.underline_answered);
        assert!(!s.capabilities.kitty_graphics);

        let mut declined = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        declined.reply("\x1bP0+r536d756c78;536574756c63\x1b\\", now);
        assert!(declined.underline_answered);
        assert!(!declined.capabilities.styled_underlines);
        assert!(!declined.capabilities.underline_colour);

        let mut late = Session {
            deadline: Some(now),
            ..Session::default()
        };
        late.reply("\x1bP1+r536d756c78=1b\x1b\\", now + TIMEOUT + TIMEOUT);
        assert!(!late.underline_answered);
        assert!(!late.capabilities.styled_underlines);
    }

    #[test]
    fn synchronized_frames_are_balanced_and_idle_frames_write_nothing() {
        assert_eq!(synchronized(String::new(), true), "");
        assert_eq!(synchronized("frame".into(), false), "frame");
        assert_eq!(
            synchronized("frame".into(), true),
            format!("{SYNC_BEGIN}frame{SYNC_END}")
        );
    }

    #[test]
    fn keyboard_disambiguation_is_pushed_only_when_the_query_is_answered() {
        let now = Instant::now();
        let mut s = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        // Current flags are 0 until we push. The reply is still support.
        assert!(s.reply("\x1b[?0u", now));
        assert!(s.keyboard_disambiguate);
        // A second answer must not push another stack entry.
        assert!(!s.reply("\x1b[?31u", now));
        assert_eq!(KEYBOARD_PUSH, "\u{1b}[>1u");
        assert_eq!(KEYBOARD_POP, "\u{1b}[<u");

        let mut late = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        assert!(!late.reply("\x1b[?1u", now + TIMEOUT + TIMEOUT));
        assert!(!late.keyboard_disambiguate);

        // Device attributes do not close the keyboard question. A flags
        // reply that is still inside the window is support, whichever
        // arrived first. Graphics keeps its own answer.
        let mut after_da = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        assert!(!after_da.reply("\x1b[?62;c", now));
        assert!(after_da.graphics_answered);
        assert!(!after_da.keyboard_answered);
        assert!(after_da.reply("\x1b[?0u", now));
        assert!(after_da.keyboard_disambiguate);

        // The same flags reply after the deadline is still not support.
        let mut late_after_da = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        assert!(!late_after_da.reply("\x1b[?62;c", now));
        assert!(!late_after_da.reply("\x1b[?0u", now + TIMEOUT + TIMEOUT));
        assert!(!late_after_da.keyboard_answered);
        assert!(!late_after_da.keyboard_disambiguate);
    }

    #[test]
    fn keyboard_transitions_block_the_signals_that_pop() {
        let mut before = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut before);
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut before),
                0
            );
        }
        let held = [libc::SIGINT, libc::SIGTERM, libc::SIGTSTP, libc::SIGQUIT];
        {
            let _hold = block_handled_signals();
            let mut during = unsafe { std::mem::zeroed() };
            unsafe {
                libc::sigemptyset(&mut during);
                assert_eq!(
                    libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut during),
                    0
                );
            }
            for sig in held {
                assert_eq!(unsafe { libc::sigismember(&during, sig) }, 1, "{sig}");
            }
        }
        let mut after = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut after);
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut after),
                0
            );
        }
        for sig in held {
            assert_eq!(
                unsafe { libc::sigismember(&after, sig) },
                unsafe { libc::sigismember(&before, sig) },
                "{sig}"
            );
        }
    }

    #[test]
    fn a_cell_report_is_height_then_width_and_junk_is_not_one() {
        // CSI 16 t is answered `CSI 6 ; height ; width t`.
        assert_eq!(cell_report("\x1b[6;40;20t"), Some(Some((20, 40))));
        assert_eq!(cell_report("\x1b[6;16;8t"), Some(Some((8, 16))));
        assert_eq!(cell_report("\x1b[6;22;10t"), Some(Some((10, 22))));
        assert_eq!(cell_report("\x1b[?62;c"), None);
        assert_eq!(cell_report("\x1b_Gi=2147483646;OK\x1b\\"), None);
        assert_eq!(cell_report("\x1b[6;40;20;1t"), Some(None));
    }

    #[test]
    fn zero_missing_and_absurd_cell_reports_fall_back_to_8x16() {
        assert_eq!(cell_report("\x1b[6;0;0t"), Some(None));
        assert_eq!(cell_report("\x1b[6;0;20t"), Some(None));
        assert_eq!(cell_report("\x1b[6;;t"), Some(None));
        assert_eq!(cell_report("\x1b[6;2;500t"), Some(None));
        assert_eq!(cell_report("\x1b[6;8;8t"), Some(None));
        assert_eq!(cell_report("\x1b[6;400;200t"), Some(None));
        assert_eq!(sane_cell(0, 0), None);
        assert_eq!(resolve_cell(None, None), (FALLBACK_CELL_W, FALLBACK_CELL_H));
        assert_eq!(
            resolve_cell(cell_report("\x1b[6;0;0t").unwrap(), None),
            (8, 16)
        );
        assert_eq!(
            resolve_cell(cell_report("\x1b[6;2;500t").unwrap(), None),
            (8, 16)
        );
        // A usable window size is the step between a bad report and 8×16.
        assert_eq!(resolve_cell(None, Some((10, 22))), (10, 22));
        assert_eq!(resolve_cell(Some((20, 40)), Some((10, 22))), (20, 40));
    }

    #[test]
    fn a_resize_without_a_report_or_pixels_uses_8x16() {
        // A reply already matched this resize: leave that size alone.
        assert_eq!(pending_cell(true, None), None);
        assert_eq!(pending_cell(true, Some((20, 40))), None);
        // No reply yet. A sane window size stands; a missing one is 8×16.
        assert_eq!(pending_cell(false, Some((20, 40))), Some((20, 40)));
        assert_eq!(pending_cell(false, None), Some((8, 16)));
    }

    #[test]
    fn a_larger_cell_does_not_raise_the_worst_case_raster_bytes() {
        let fallback_cells = max_plot_cells_for(FALLBACK_CELL_W, FALLBACK_CELL_H);
        assert_eq!(fallback_cells, MAX_PLOT_CELLS);
        let pixels = |w, h| max_plot_cells_for(w, h) * w * h;
        let cap = pixels(FALLBACK_CELL_W, FALLBACK_CELL_H);
        assert_eq!(pixels(20, 40), cap);
        for (w, h) in [(10, 22), (48, 96), (7, 14), (4, 8), (9, 18)] {
            assert!(sane_cell(w, h).is_some(), "{w}x{h} should be a cell");
            assert!(pixels(w, h) <= cap, "{w}x{h} exceeds the 8x16 byte cap");
        }
    }
}
