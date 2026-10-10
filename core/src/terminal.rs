//! Capability negotiation belongs to the terminal session, never a widget.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(crate) const SYNC_BEGIN: &str = "\x1b[?2026h";
pub(crate) const SYNC_END: &str = "\x1b[?2026l";
const QUERY: &str = "\x1b_Gi=2147483646,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[?2026$p\x1b[c";
const TIMEOUT: Duration = Duration::from_millis(800);
static SYNC_SUPPORTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub kitty_graphics: bool,
    pub synchronized_updates: bool,
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
}

static SESSION: Mutex<Session> = Mutex::new(Session {
    capabilities: Capabilities {
        kitty_graphics: false,
        synchronized_updates: false,
    },
    deadline: None,
    mode: GraphicsMode::Auto,
    graphics_answered: false,
    image_rejected: false,
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
    // Text mode still benefits from synchronized updates, but sends no image query.
    super::out(if mode == GraphicsMode::Text {
        "\x1b[?2026$p\x1b[c"
    } else {
        QUERY
    });
    super::flush();
}

impl Session {
    fn reply(&mut self, sequence: &str, now: Instant) {
        if let Some((keys, message)) = sequence
            .strip_prefix("\x1b_G")
            .and_then(|s| s.split_once(';'))
        {
            let id = keys
                .split(',')
                .find_map(|key| key.strip_prefix("i=")?.parse::<u32>().ok());
            if id.is_some_and(super::graphics::owns_image) && message != "OK\x1b\\" {
                self.capabilities.kitty_graphics = false;
                self.image_rejected = true;
                self.graphics_answered = true;
                return;
            }
        }
        if !self.deadline.is_some_and(|deadline| now <= deadline) {
            return;
        }
        if let Some(response) = sequence.strip_prefix("\x1b_Gi=2147483646;") {
            if !self.graphics_answered {
                self.capabilities.kitty_graphics =
                    response == "OK\x1b\\" && self.mode != GraphicsMode::Text;
                self.graphics_answered = true;
            }
        } else if sequence == "\x1b[?2026;1$y" || sequence == "\x1b[?2026;2$y" {
            self.capabilities.synchronized_updates = true;
        } else if sequence.starts_with("\x1b[?") && sequence.ends_with('c') {
            // The DA reply follows the graphics query. No earlier graphics
            // response means this path does not support it, even if TERM says kitty.
            self.graphics_answered = true;
        }
    }
}

pub(crate) fn reply(sequence: &str) {
    let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    session.reply(sequence, Instant::now());
    SYNC_SUPPORTED.store(session.capabilities.synchronized_updates, Ordering::Release);
}

pub(crate) fn cleanup_signal() {
    if SYNC_SUPPORTED.load(Ordering::Acquire) {
        unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                SYNC_END.as_ptr().cast(),
                SYNC_END.len(),
            );
        }
    }
}

pub fn capabilities() -> Capabilities {
    SESSION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .capabilities
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
    fn synchronized_frames_are_balanced_and_idle_frames_write_nothing() {
        assert_eq!(synchronized(String::new(), true), "");
        assert_eq!(synchronized("frame".into(), false), "frame");
        assert_eq!(
            synchronized("frame".into(), true),
            format!("{SYNC_BEGIN}frame{SYNC_END}")
        );
    }
}
