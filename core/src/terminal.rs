//! Capability negotiation belongs to the terminal session, never a widget.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(crate) const SYNC_BEGIN: &str = "\x1b[?2026h";
pub(crate) const SYNC_END: &str = "\x1b[?2026l";
/// Disambiguate escape codes. Pushed only after `CSI ? u` is answered.
pub(crate) const KEYBOARD_PUSH: &str = "\x1b[>1u";
/// Pop one keyboard-protocol stack entry, which returns the terminal to legacy.
pub(crate) const KEYBOARD_POP: &str = "\x1b[<u";
const GRAPHICS_QUERY: &str = "\x1b_Gi=2147483646,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
/// Sync, then the keyboard query, then device attributes. The keyboard
/// query goes out before device attributes.
const QUERY_TAIL: &str = "\x1b[?2026$p\x1b[?u\x1b[c";
/// XTGETTCAP for `Smulx` (styled underlines) and `Setulc` (underline colour).
const UNDERLINE_QUERY: &str = "\x1bP+q536d756c78;536574756c63\x1b\\";
const TIMEOUT: Duration = Duration::from_millis(800);
static SYNC_SUPPORTED: AtomicBool = AtomicBool::new(false);
/// True after flag 1 was pushed, so every cleanup path pops exactly once.
static KEYBOARD_PUSHED: AtomicBool = AtomicBool::new(false);

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
    /// Set once the shared-memory probe has an answer or has timed out.
    probe_settled: bool,
    /// Later images go out as `t=s`. A failure clears this and stays on `t=d`.
    use_shm: bool,
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
    probe_settled: true,
    use_shm: false,
});

/// Query only after cbreak is active, so replies are never echoed as text.
/// No wait here: the first frames use text while normal input polling negotiates.
pub(crate) fn start() {
    let mode = match std::env::var("OPSCOPE_GRAPHICS").as_deref() {
        Ok("text") => GraphicsMode::Text,
        Ok("kitty") => GraphicsMode::Kitty,
        _ => GraphicsMode::Auto,
    };
    // Resume and a fresh start both drop the previous probe. A shared-memory
    // object that outlived the last session would be the leak this path is for.
    super::shm::force_direct();
    let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    *session = Session {
        mode,
        probe_settled: true,
        ..Session::default()
    };
    SYNC_SUPPORTED.store(false, Ordering::Release);
    super::graphics::reset_animation();
    if unsafe { libc::isatty(libc::STDOUT_FILENO) } != 1 {
        return;
    }
    session.deadline = Some(Instant::now() + TIMEOUT);
    let probe = if mode == GraphicsMode::Text {
        None
    } else {
        super::shm::begin_probe()
    };
    if probe.is_some() {
        session.probe_settled = false;
    }
    drop(session);
    // A resumed or reclaimed session asks again. Pop first so a second push
    // does not leave flag 1 on the stack after the matching pop.
    pop_keyboard();
    // Text mode still benefits from synchronized updates, but sends no image query.
    // The keyboard query is sent either way: it is independent of graphics.
    // It goes out before device attributes. A flags reply inside the window
    // is support even when device attributes arrived first. No keyboard
    // reply before the deadline leaves the legacy decoder in place.
    // The frame probe sits before the tail, so a reply to the tail has already
    // seen it. Until it is acknowledged, charts keep the full upload.
    let mut bytes = if mode == GraphicsMode::Text {
        format!("{QUERY_TAIL}{UNDERLINE_QUERY}")
    } else {
        format!(
            "{GRAPHICS_QUERY}{}{QUERY_TAIL}{UNDERLINE_QUERY}",
            super::graphics::animation_probe()
        )
    };
    if let Some(probe) = probe {
        bytes.push_str(&probe);
    }
    super::out(&bytes);
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

/// Called once a frame. A probe that never answers is abandoned here, so the
/// images in this frame stay on the direct path. Retired shared-memory
/// objects are collected once their read window has passed.
pub(crate) fn on_frame() {
    super::shm::reap_expired();
    let now = Instant::now();
    let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if !session.probe_settled && session.deadline.is_some_and(|deadline| now > deadline) {
        session.probe_settled = true;
        session.use_shm = false;
        drop(session);
        super::shm::expire_probe();
    }
}

impl Session {
    /// Returns whether this reply is the one that turns disambiguation on.
    fn reply(&mut self, sequence: &str, now: Instant) -> bool {
        let in_window = self.deadline.is_some_and(|deadline| now <= deadline);
        if !self.probe_settled {
            match super::shm::parse_shm_probe(sequence) {
                super::shm::ProbeFate::Accepted if in_window => {
                    self.probe_settled = true;
                    self.use_shm = true;
                    super::shm::apply_accepted();
                }
                super::shm::ProbeFate::Accepted | super::shm::ProbeFate::Rejected => {
                    self.probe_settled = true;
                    self.use_shm = false;
                    super::shm::apply_rejected();
                }
                super::shm::ProbeFate::Unrelated => {}
            }
        }
        if let Some((keys, message)) = sequence
            .strip_prefix("\x1b_G")
            .and_then(|s| s.split_once(';'))
        {
            let id = keys
                .split(',')
                .find_map(|key| key.strip_prefix("i=")?.parse::<u32>().ok());
            if id == Some(super::graphics::probe_id()) {
                if message.starts_with("OK") {
                    super::graphics::set_animation(true);
                } else if message.starts_with("ENOENT") || message.starts_with("EINVAL") {
                    super::graphics::set_animation(false);
                }
                // Same path as every other write outside `draw`, so the
                // delete stays ordered with the frame bytes.
                super::out(&super::graphics::discard_probe());
                super::flush();
                return false;
            }
            // A frame edit the terminal rejects is not a broken image.
            // Kitty answers ENOSPC when animation storage is exhausted.
            // One edit can draw several of these, so the slot bits stay
            // until the next full upload replaces them. Clearing them on
            // the first reply made the next one look like a bad image and
            // turned graphics off.
            if id.is_some_and(super::graphics::frame_edit_pending) && frame_refused(message) {
                super::graphics::set_animation(false);
                super::screen_lost();
                return false;
            }
            if let Some(id) = id.filter(|id| super::graphics::owns_image(*id)) {
                // ENOENT means the terminal dropped the placement. Only a
                // replaced object is collected; the current one reused the id.
                // It is not the reply to a transmission, so the generation
                // that is still owed stays owed.
                if message.starts_with("ENOENT") {
                    super::shm::unlink_retired(id);
                    super::screen_lost();
                    return false;
                }
                let placement = keys.split(',').find_map(|key| key.strip_prefix("p=")?.parse::<u32>().ok());
                let generation = placement.and_then(|p| super::shm::take_reply(id, p));
                if message != "OK\x1b\\" {
                    // A shared reply stays a transport failure even when the
                    // fallback frame has already been sent. That frame has
                    // a different placement key. Only a matched direct error
                    // turns graphics off.
                    // EBADF is the protocol's file/shared-memory read
                    // failure, even if its keyed entry has already expired.
                    // Other uncorrelated replies cannot reject newer images.
                    let shared_failure = if message.starts_with("EBADF") {
                        true
                    } else {
                        match generation {
                            Some(shared) => shared,
                            None => return false,
                        }
                    };
                    if shared_failure {
                        self.use_shm = false;
                        super::shm::transport_failed();
                        super::screen_lost();
                        return false;
                    }
                    self.capabilities.kitty_graphics = false;
                    self.image_rejected = true;
                    self.graphics_answered = true;
                    return false;
                }
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

fn frame_refused(message: &str) -> bool {
    message.starts_with("ENOENT")
        || message.starts_with("EINVAL")
        || message.starts_with("ENOSPC")
}

pub(crate) fn reply(sequence: &str) {
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
        let _guard = crate::shm::guard();
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
            crate::shm::mark_direct(ours, 1);
            s.reply(&format!("\x1b_Gi={ours},p=1;EINVAL:bad\x1b\\"), now);
            assert!(!s.capabilities.kitty_graphics && s.image_rejected);
        }
    }

    #[test]
    fn a_refused_frame_probe_keeps_full_uploads() {
        let _slots = crate::graphics::test_lock();
        let now = Instant::now();
        let mut s = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        s.reply("\x1b_Gi=2147483646;OK\x1b\\", now);
        let probe = crate::graphics::probe_id();
        s.reply(&format!("\x1b_Gi={probe};EINVAL:no animation\x1b\\"), now);
        assert!(s.capabilities.kitty_graphics && !s.image_rejected);
        assert!(!crate::graphics::animation_ready());
        s.reply(&format!("\x1b_Gi={probe};OK\x1b\\"), now);
        assert!(crate::graphics::animation_ready());
        crate::graphics::reset_animation();
    }

    #[test]
    fn repeated_frame_errors_keep_graphics() {
        let _slots = crate::graphics::test_lock();
        crate::graphics::clear_frame_edits();
        let now = Instant::now();
        let mut s = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        s.reply("\x1b_Gi=2147483646;OK\x1b\\", now);
        let pid = unsafe { libc::getpid() } as u32;
        let slot = |n: u32| 0x40000000 | ((pid & 0x1fffff) << 4) | n;
        // An image error that was not a frame edit still ends graphics.
        s.reply(
            &format!("\x1b_Gi={};ENOSPC:animation storage exhausted\x1b\\", slot(0)),
            now,
        );
        assert!(!s.capabilities.kitty_graphics && s.image_rejected);

        let mut s = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        s.reply("\x1b_Gi=2147483646;OK\x1b\\", now);
        crate::graphics::testing_mark_frame(0);
        crate::graphics::testing_mark_frame(1);
        for message in [
            format!("\x1b_Gi={};EINVAL:overlap\x1b\\", slot(0)),
            format!("\x1b_Gi={};EINVAL:overlap\x1b\\", slot(0)),
            format!("\x1b_Gi={};ENOSPC:animation storage exhausted\x1b\\", slot(1)),
        ] {
            s.reply(&message, now);
            assert!(
                s.capabilities.kitty_graphics && !s.image_rejected,
                "{message}"
            );
        }
        assert!(!crate::graphics::animation_ready());
        // Once the fallback upload clears the bits, a real image error still ends graphics.
        crate::graphics::clear_frame_edits();
        s.reply(&format!("\x1b_Gi={};EINVAL:bad\x1b\\", slot(0)), now);
        assert!(!s.capabilities.kitty_graphics && s.image_rejected);
        crate::graphics::reset_animation();
        crate::graphics::clear_frame_edits();
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
    fn a_rejected_shared_memory_probe_stays_direct() {
        let _guard = crate::shm::guard();
        let now = Instant::now();
        let mut s = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        s.reply("\x1b_Gi=2147483646;OK\x1b\\", now);
        s.reply("\x1b_Gi=2147483645;EBADF:remote\x1b\\", now);
        assert!(
            s.capabilities.kitty_graphics && !s.use_shm && s.probe_settled && !s.image_rejected
        );
        s.reply("\x1b_Gi=2147483645;OK\x1b\\", now);
        assert!(
            !s.use_shm,
            "a second probe reply must not turn shared memory back on"
        );
    }

    #[test]
    fn queued_shared_memory_errors_keep_graphics_until_a_direct_send() {
        let _guard = crate::shm::guard();
        let now = Instant::now();
        let mut s = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        s.reply("\x1b_Gi=2147483646;OK\x1b\\", now);
        s.reply("\x1b_Gi=2147483645;OK\x1b\\", now);
        assert!(s.use_shm && s.capabilities.kitty_graphics);
        let id0 = crate::graphics::image_id(0);
        let id1 = crate::graphics::image_id(31);
        crate::shm::mark_shared(id0, 1);
        crate::shm::mark_shared(id1, 1);
        s.reply(&format!("\x1b_Gi={id0},p=1;EBADF:shared memory\x1b\\"), now);
        assert!(s.capabilities.kitty_graphics && !s.image_rejected && !s.use_shm);
        s.reply(&format!("\x1b_Gi={id1},p=1;EBADF:shared memory\x1b\\"), now);
        assert!(
            s.capabilities.kitty_graphics && !s.image_rejected,
            "a second queued shared-memory error turned graphics off"
        );
        crate::shm::mark_direct(id0, 2);
        s.reply(&format!("\x1b_Gi={id0},p=2;EINVAL:bad\x1b\\"), now);
        assert!(!s.capabilities.kitty_graphics && s.image_rejected);
    }

    #[test]
    fn a_late_shared_memory_error_after_fallback_keeps_graphics() {
        let _guard = crate::shm::guard();
        let now = Instant::now();
        let mut s = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        s.reply("\x1b_Gi=2147483646;OK\x1b\\", now);
        s.reply("\x1b_Gi=2147483645;OK\x1b\\", now);
        assert!(s.use_shm && s.capabilities.kitty_graphics);
        let id = crate::graphics::image_id(0);
        crate::shm::mark_shared(id, 1);
        crate::shm::mark_direct(id, 2);
        s.reply(&format!("\x1b_Gi={id},p=1;EBADF:shared memory\x1b\\"), now);
        assert!(
            s.capabilities.kitty_graphics && !s.image_rejected,
            "a late shared-memory error disabled graphics"
        );
        s.reply(&format!("\x1b_Gi={id},p=2;EINVAL:bad\x1b\\"), now);
        assert!(!s.capabilities.kitty_graphics && s.image_rejected);
    }

    #[test]
    fn uncorrelated_late_errors_never_reject_a_newer_direct_upload() {
        let _guard = crate::shm::guard();
        let now = Instant::now();
        let mut s = Session::default();
        s.capabilities.kitty_graphics = true;
        let id = crate::graphics::image_id(0);
        crate::shm::mark_shared(id, 1);
        crate::shm::supersede(id);
        assert!(crate::shm::mark_direct(id, 2));
        s.reply(&format!("\x1b_Gi={id},p=1;ENOMEM:late shared\x1b\\"), now);
        assert!(s.capabilities.kitty_graphics && !s.image_rejected);
        s.reply(&format!("\x1b_Gi={id},p=1;EBADF:late shared\x1b\\"), now);
        assert!(s.capabilities.kitty_graphics && !s.image_rejected);
        s.reply(&format!("\x1b_Gi={id},p=2;EINVAL:direct\x1b\\"), now);
        assert!(!s.capabilities.kitty_graphics && s.image_rejected);
    }

    #[test]
    fn a_reply_after_resume_is_dropped() {
        let _guard = crate::shm::guard();
        let now = Instant::now();
        let mut s = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        s.reply("\x1b_Gi=2147483646;OK\x1b\\", now);
        s.reply("\x1b_Gi=2147483645;OK\x1b\\", now);
        assert!(s.use_shm && s.capabilities.kitty_graphics);
        let id = crate::graphics::image_id(0);
        assert!(crate::shm::mark_direct(id, 4));
        // SIGCONT runs cleanup and start, which clears PENDING and leaves
        // the new session on direct transport before the buffered reply is read.
        crate::shm::force_direct();
        s.use_shm = false;
        s.probe_settled = true;
        s.reply(&format!("\x1b_Gi={id},p=4;EINVAL:stale session\x1b\\"), now);
        assert!(
            s.capabilities.kitty_graphics && !s.image_rejected,
            "a reply from the previous session was treated as a direct failure"
        );
        assert!(crate::shm::mark_direct(id, 5));
        s.reply(&format!("\x1b_Gi={id},p=5;EINVAL:current\x1b\\"), now);
        assert!(!s.capabilities.kitty_graphics && s.image_rejected);
    }

    #[test]
    fn a_late_shared_memory_ok_does_not_enable_it() {
        let _guard = crate::shm::guard();
        let now = Instant::now();
        let mut s = Session {
            deadline: Some(now + TIMEOUT),
            ..Session::default()
        };
        s.reply("\x1b_Gi=2147483645;OK\x1b\\", now + TIMEOUT + TIMEOUT);
        assert!(!s.use_shm && s.probe_settled);
    }
}
