// opscope - small dependency-free terminal widgets
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

//! The terminal's own colours, read once and turned into a theme.
//!
//! `OSC 10 ; ?` asks for the foreground, `OSC 11 ; ?` the background and
//! `OSC 4 ; n ; ?` palette entry `n`; each answer is `rgb:rrrr/gggg/bbbb`.
//! They are asked **once, synchronously, the first time a colour is
//! wanted** - before a widget has built its palette, which it does once -
//! and followed by a Device Attributes request, which every terminal
//! answers. A terminal that does not do colour queries answers DA alone, so
//! the wait ends as soon as that arrives; one that answers nothing at all
//! (a dead link) is given up on after [`WAIT`]. Nothing here runs when
//! standard input or output is not a terminal.
//!
//! A light/dark switch later (mode 2031, `CSI ? 997 ; 1|2 n`) restarts the
//! widget in place, the way saving its settings does, and the new process
//! asks again.

use crate::theme::{luminance, mix, Rgb, Role, Theme};

/// The longest the query waits for a terminal that answers nothing.
pub const WAIT: std::time::Duration = std::time::Duration::from_millis(1000);

/// What one answer was about.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Said {
    Foreground(Rgb),
    Background(Rgb),
    Palette(usize, Rgb),
}

/// `rgb:r/g/b` with one to four hex digits a channel, or `#rrggbb`.
pub fn parse_colour_spec(spec: &str) -> Option<Rgb> {
    if let Some(hex) = spec.strip_prefix('#') {
        if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            let b = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
            return Some((b(0)?, b(2)?, b(4)?));
        }
        return None;
    }
    let body = spec.strip_prefix("rgb:").or_else(|| spec.strip_prefix("rgba:"))?;
    let mut parts = body.split('/');
    let mut channel = || -> Option<u8> {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 4 || !p.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(p, 16).ok()?;
        let max = (1u32 << (4 * p.len())) - 1;
        Some(((v * 255 + max / 2) / max) as u8)
    };
    Some((channel()?, channel()?, channel()?))
}

/// One OSC 10, 11 or 4 answer, ended by ST or BEL.
pub fn parse_colour_answer(sequence: &str) -> Option<Said> {
    let body = sequence.strip_prefix("\x1b]")?;
    let body = body.strip_suffix("\x1b\\").or_else(|| body.strip_suffix('\x07'))?;
    let mut fields = body.split(';');
    match fields.next()? {
        "10" => Some(Said::Foreground(parse_colour_spec(fields.next()?)?)),
        "11" => Some(Said::Background(parse_colour_spec(fields.next()?)?)),
        "4" => {
            let n: usize = fields.next()?.parse().ok()?;
            (n < 16).then_some(())?;
            Some(Said::Palette(n, parse_colour_spec(fields.next()?)?))
        }
        _ => None,
    }
}

/// Every colour answer in what the terminal sent back, in order.
pub fn parse_colour_answers(text: &str) -> Vec<Said> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("\x1b]") {
        let tail = &rest[at..];
        let end = match (tail.find("\x1b\\"), tail.find('\x07')) {
            (Some(st), Some(bel)) if bel < st => bel + 1,
            (Some(st), _) => st + 2,
            (None, Some(bel)) => bel + 1,
            (None, None) => break,
        };
        if let Some(said) = parse_colour_answer(&tail[..end]) {
            found.push(said);
        }
        rest = &tail[end..];
    }
    found
}

/// A theme from what the terminal said about itself: its foreground, its
/// background and as much of the sixteen-colour palette as it answered.
///
/// `fg` and `bg` are its own. The hues come from ANSI 1 to 6, with the
/// bright entry standing in when the normal one did not answer and a fixed
/// hue when neither did. `slot`, `dark`, `muted`, `dim` and `border` are
/// mixed from `fg` and `bg`; orange is between red and yellow. Contrast is
/// then enforced like every theme's, which many terminal palettes need - a
/// dark blue on black is common. It paints no background: the terminal's
/// own is the one it describes.
pub fn theme_from(fg: Rgb, bg: Rgb, palette: &[Option<Rgb>; 16]) -> Theme {
    let light = luminance(bg) > 0.5;
    let ansi = |n: usize, fallback: Rgb| palette[n].or(palette[n + 8]).unwrap_or(fallback);
    let red = ansi(1, (205, 49, 49));
    let green = ansi(2, (13, 188, 121));
    let yellow = ansi(3, (229, 229, 16));
    let blue = ansi(4, (36, 114, 200));
    let magenta = ansi(5, (188, 63, 188));
    let cyan = ansi(6, (17, 168, 205));
    let mut colours = [(0, 0, 0); 16];
    let mut put = |role: Role, c: Rgb| colours[Role::ALL.iter().position(|r| *r == role).unwrap()] = c;
    put(Role::Bg, bg);
    put(Role::Dark, mix(bg, if light { (0, 0, 0) } else { (0, 0, 0) }, 0.25));
    put(Role::Slot, mix(bg, fg, 0.14));
    put(Role::Fg, fg);
    put(Role::Muted, mix(fg, bg, 0.35));
    put(Role::Dim, mix(fg, bg, 0.7));
    put(Role::Border, mix(fg, bg, 0.8));
    put(Role::Accent, blue);
    put(Role::OnAccent, bg);
    put(Role::Green, green);
    put(Role::Blue, blue);
    put(Role::Yellow, yellow);
    put(Role::Red, red);
    put(Role::Cyan, cyan);
    put(Role::Magenta, magenta);
    put(Role::Orange, mix(red, yellow, 0.5));
    let mut theme = Theme::new("terminal", "the terminal's own", light, false, colours);
    theme.group = "terminal".into();
    theme
}

/// The theme the replies in `text` describe, when they named at least the
/// foreground and the background.
pub fn theme_from_answers(text: &str) -> Option<Theme> {
    let (mut fg, mut bg) = (None, None);
    let mut palette = [None; 16];
    for said in parse_colour_answers(text) {
        match said {
            Said::Foreground(c) => fg = Some(c),
            Said::Background(c) => bg = Some(c),
            Said::Palette(n, c) => palette[n] = Some(c),
        }
    }
    Some(theme_from(fg?, bg?, &palette))
}

/// The query: foreground, background, the sixteen palette entries, then
/// Device Attributes so a terminal that ignores the rest still ends the wait.
pub fn query_bytes() -> String {
    let palette: String = (0..16).map(|n| format!("\x1b]4;{n};?\x1b\\")).collect();
    format!("\x1b]10;?\x1b\\\x1b]11;?\x1b\\{palette}\x1b[c")
}

/// Whether `text` holds the Device Attributes answer that ends the wait.
pub fn has_device_attributes(text: &str) -> bool {
    let mut rest = text;
    while let Some(at) = rest.find("\x1b[?") {
        let tail = &rest[at + 3..];
        let digits = tail.bytes().take_while(|b| b.is_ascii_digit() || *b == b';').count();
        if digits > 0 && tail.as_bytes().get(digits) == Some(&b'c') {
            return true;
        }
        rest = tail;
    }
    false
}

/// Ask the terminal, wait for the Device Attributes answer or [`WAIT`], and
/// put the terminal back exactly as it was. `None` when this is not a
/// terminal or it described no foreground and background.
#[cfg(unix)]
pub fn query() -> Option<Theme> {
    use std::io::Write;
    let (input, output) = (libc::STDIN_FILENO, libc::STDOUT_FILENO);
    if unsafe { libc::isatty(input) } != 1 || unsafe { libc::isatty(output) } != 1 {
        return None;
    }
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(input, &mut saved) } != 0 {
        return None;
    }
    let mut raw = saved;
    raw.c_lflag &= !(libc::ICANON | libc::ECHO);
    raw.c_cc[libc::VMIN] = 0;
    raw.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(input, libc::TCSANOW, &raw) } != 0 {
        return None;
    }
    let mut out = std::io::stdout();
    let _ = out.write_all(query_bytes().as_bytes());
    let _ = out.flush();
    let deadline = std::time::Instant::now() + WAIT;
    let mut got = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let mut fds = libc::pollfd { fd: input, events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut fds, 1, left.as_millis().min(100) as libc::c_int) };
        if ready > 0 {
            let n = unsafe { libc::read(input, chunk.as_mut_ptr().cast(), chunk.len()) };
            if n > 0 {
                got.extend_from_slice(&chunk[..n as usize]);
                if has_device_attributes(&String::from_utf8_lossy(&got)) {
                    break;
                }
            }
        }
    }
    unsafe { libc::tcsetattr(input, libc::TCSANOW, &saved) };
    theme_from_answers(&String::from_utf8_lossy(&got))
}

#[cfg(not(unix))]
pub fn query() -> Option<Theme> {
    None
}

/// `CSI ? 997 ; 1 n` (dark) or `; 2 n` (light): the terminal's mode 2031
/// notice that its colours changed.
pub fn parse_scheme_change(sequence: &str) -> Option<bool> {
    match sequence {
        "\x1b[?997;1n" => Some(false),
        "\x1b[?997;2n" => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{contrast, AA};

    #[test]
    fn colour_specs_in_every_width_the_terminals_send() {
        assert_eq!(parse_colour_spec("rgb:ffff/0000/8080"), Some((255, 0, 128)));
        assert_eq!(parse_colour_spec("rgb:ff/00/80"), Some((255, 0, 128)));
        assert_eq!(parse_colour_spec("rgb:f/0/8"), Some((255, 0, 136)));
        assert_eq!(parse_colour_spec("rgba:2222/2525/2b2b/ffff"), Some((34, 37, 43)));
        assert_eq!(parse_colour_spec("#22252b"), Some((34, 37, 43)));
        assert_eq!(parse_colour_spec("rgb:zz/00/00"), None);
        assert_eq!(parse_colour_spec("blue"), None);
    }

    #[test]
    fn answers_are_read_whichever_terminator_they_end_in() {
        let text = "\x1b]10;rgb:e0e0/e0e0/e0e0\x1b\\\x1b]11;rgb:1c1c/1e1e/2626\x07\x1b]4;1;rgb:ff/55/55\x1b\\\x1b]4;99;rgb:0/0/0\x1b\\\x1b[?62;22c";
        assert_eq!(
            parse_colour_answers(text),
            vec![Said::Foreground((224, 224, 224)), Said::Background((28, 30, 38)), Said::Palette(1, (255, 85, 85))]
        );
        assert!(has_device_attributes(text));
        assert!(!has_device_attributes("\x1b]10;rgb:0/0/0\x1b\\"));
        assert!(!has_device_attributes("\x1b[?2026;1$y"));
    }

    #[test]
    fn a_terminal_that_answers_only_device_attributes_gives_no_theme() {
        assert!(theme_from_answers("\x1b[?62;22c").is_none());
        // A foreground alone is not enough to place a background.
        assert!(theme_from_answers("\x1b]10;rgb:ffff/ffff/ffff\x1b\\\x1b[?62c").is_none());
    }

    #[test]
    fn a_light_terminal_gives_a_light_theme_readable_on_its_own_background() {
        let mut palette = [None; 16];
        palette[4] = Some((0, 0, 238)); // a pure blue: under AA on white? no, but test the nudge anyway
        palette[3] = Some((205, 205, 0)); // a yellow that is unreadable on white
        let t = theme_from((0, 0, 0), (255, 255, 255), &palette);
        assert!(t.light && !t.paints_background);
        for role in Role::ALL.into_iter().filter(|r| r.is_text()) {
            let r = contrast(t.colour(role), t.colour(Role::Bg));
            assert!(r >= AA, "{role:?} on the terminal's own white is {r:.2}");
        }
        // The yellow was moved, darker, to read on white.
        assert!(luminance(t.colour(Role::Yellow)) < luminance((205, 205, 0)));
    }

    #[test]
    fn a_dark_terminal_keeps_its_own_colours_where_they_read() {
        let mut palette = [None; 16];
        palette[2] = Some((80, 250, 123));
        palette[1] = Some((255, 85, 85));
        let t = theme_from((248, 248, 242), (40, 42, 54), &palette);
        assert!(!t.light);
        assert_eq!(t.colour(Role::Fg), (248, 248, 242));
        assert_eq!(t.colour(Role::Bg), (40, 42, 54));
        assert_eq!(t.colour(Role::Green), (80, 250, 123));
        // A missing normal entry takes the bright one.
        let mut bright_only = [None; 16];
        bright_only[9] = Some((255, 110, 110));
        assert_eq!(theme_from((248, 248, 242), (40, 42, 54), &bright_only).colour(Role::Red), (255, 110, 110));
    }

    #[test]
    fn the_query_asks_for_everything_then_for_device_attributes_last() {
        let q = query_bytes();
        assert!(q.starts_with("\x1b]10;?\x1b\\\x1b]11;?\x1b\\"));
        assert_eq!(q.matches("\x1b]4;").count(), 16);
        assert!(q.ends_with("\x1b[c"));
    }

    #[test]
    fn scheme_changes_say_which_way() {
        assert_eq!(parse_scheme_change("\x1b[?997;1n"), Some(false));
        assert_eq!(parse_scheme_change("\x1b[?997;2n"), Some(true));
        assert_eq!(parse_scheme_change("\x1b[?996n"), None);
    }
}
