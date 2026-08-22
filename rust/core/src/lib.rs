// terminal-toys - small dependency-free terminal widgets
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

//! What every widget shares: the terminal, the keyboard, and the drawing.
//!
//! A port of `common.py`, kept deliberately close to it. The widgets are
//! being translated one at a time and the two versions have to sit side by
//! side and agree on screen, so where a choice existed this keeps the
//! Python behaviour rather than the more idiomatic Rust one.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;

pub const HIDE: &str = "\x1b[?25l";
pub const SHOW: &str = "\x1b[?25h";
pub const HOME: &str = "\x1b[H";
pub const CLEAR: &str = "\x1b[2J";
pub const EL: &str = "\x1b[K";
pub const RST: &str = "\x1b[0m";
pub const NOBG: &str = "\x1b[49m";

/// A foreground colour, as a truecolor escape.
pub fn rgb(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{};{};{}m", r, g, b)
}

/// A background colour. Needs an explicit reset afterwards, or it bleeds
/// along the rest of the row - the same trap as in the Python.
pub fn bg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[48;2;{};{};{}m", r, g, b)
}

/// Truncate or pad a plain string to exactly `n` cells.
pub fn pad(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count > n {
        s.chars().take(n).collect()
    } else {
        let mut out = String::from(s);
        out.extend(std::iter::repeat(' ').take(n - count));
        out
    }
}

/// Join coloured segments, hard-clipped to `width` printable cells.
///
/// The colour of each segment is a prefix that costs nothing on screen, so
/// only the text counts toward the width. Every widget's layout arithmetic
/// depends on that, which is why escapes have to live in the colour half of
/// the pair and never in the text.
pub fn seg(parts: &[(&str, String)], width: usize) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    for (colour, text) in parts {
        if n >= width {
            break;
        }
        let room = width - n;
        let count = text.chars().count();
        let cut: String = if count > room {
            text.chars().take(room).collect()
        } else {
            text.clone()
        };
        out.push_str(colour);
        out.push_str(&cut);
        n += cut.chars().count();
    }
    out
}

/// The rule across the top of every widget.
pub fn title(text: &str, w: usize, colour: &str) -> String {
    let t = format!(" {} ", text.to_uppercase());
    let left = "╺━";
    let used = t.chars().count() + left.chars().count() + 1;
    let fill = "━".repeat(w.saturating_sub(used));
    format!(
        "{}{}{}{}{}{}{}{}╸{}",
        colour,
        left,
        RST,
        rgb(220, 255, 240),
        t,
        RST,
        colour,
        fill,
        RST
    )
}

/// The terminal's size, or a sane pair if it will not say.
pub fn size() -> (usize, usize) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if ok == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        (
            std::cmp::max(8, ws.ws_col as usize),
            std::cmp::max(4, ws.ws_row as usize),
        )
    } else {
        (80, 24)
    }
}

pub fn out(text: &str) {
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(text.as_bytes());
}

pub fn flush() {
    let _ = std::io::stdout().flush();
}

/// Paint `rows` from the top-left, one full frame.
///
/// Every row is followed by a reset and an erase-to-end, so a short row
/// cannot leave the tail of the previous frame behind it.
pub fn draw(rows: &[String], _w: usize, h: usize) {
    let mut buf = String::from(HOME);
    for i in 0..h {
        let empty = String::new();
        let line = rows.get(i).unwrap_or(&empty);
        buf.push_str(line);
        buf.push_str(RST);
        buf.push_str(EL);
        if i + 1 != h {
            buf.push_str("\r\n");
        }
    }
    out(&buf);
    flush();
}

/// Hide the cursor and clear, and put it all back on the way out.
pub fn setup() {
    unsafe {
        let handler = handle_signal as *const () as libc::sighandler_t;
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
    out(&format!("{}{}{}", HIDE, CLEAR, HOME));
    flush();
}

extern "C" fn handle_signal(_sig: libc::c_int) {
    out(&format!("{}{}{}{}", SHOW, RST, CLEAR, HOME));
    flush();
    std::process::exit(0);
}

/// Put the terminal back the way it was found.
pub fn restore_screen() {
    out(&format!("{}{}{}{}", SHOW, RST, CLEAR, HOME));
    flush();
}

/// Fit hint groups onto as few lines as possible without splitting one.
///
/// A hint that gets cut in half teaches a key that does not exist, so the
/// line wraps instead of truncating - the rule the whole repo follows.
pub fn pack_hints(hints: &[Vec<(&str, String)>], width: usize, sep: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut used = 0usize;
    for hint in hints {
        let plain: usize = hint.iter().map(|(_, t)| t.chars().count()).sum();
        let extra = if current.is_empty() {
            plain
        } else {
            plain + sep.chars().count()
        };
        if !current.is_empty() && used + extra > width {
            lines.push(current.join(sep));
            current = Vec::new();
            used = 0;
        }
        let piece: String = hint
            .iter()
            .map(|(c, t)| format!("{}{}", c, t))
            .collect::<Vec<_>>()
            .join("");
        if current.is_empty() {
            used = plain;
        } else {
            used += plain + sep.chars().count();
        }
        current.push(piece);
    }
    if !current.is_empty() {
        lines.push(current.join(sep));
    }
    lines
}

/// Where settings are looked for, in order of preference.
///
/// The Python's third place is "beside the script", which a compiled
/// binary does not have: its own directory is target/release, where nobody
/// would put a config. The working directory stands in for it, since that
/// is the project directory when a widget is started from a pane, and the
/// executable's own directory is kept last for a binary shipped with one
/// beside it.
pub fn config_paths() -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    if let Ok(env) = std::env::var("TERMINAL_TOYS_CONFIG") {
        if !env.is_empty() {
            found.push(std::path::PathBuf::from(env));
        }
    }
    let xdg = std::env::var("XDG_CONFIG_HOME").ok().filter(|s| !s.is_empty());
    let home = std::env::var("HOME").unwrap_or_default();
    let base = xdg.unwrap_or(format!("{}/.config", home));
    found.push(std::path::PathBuf::from(base).join("terminal-toys/config.json"));
    if let Ok(cwd) = std::env::current_dir() {
        found.push(cwd.join("config.json"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            found.push(dir.join("config.json"));
        }
    }
    found
}

/// One section of the config file, or an empty object.
///
/// The first readable file wins, and a malformed one falls back to the
/// defaults rather than stopping a running panel - a widget on a wall
/// should not vanish because a comma went missing in a file it shares.
pub fn load_config(section: &str) -> serde_json::Value {
    for path in config_paths() {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let parsed: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(found) = parsed.get(section) {
            return found.clone();
        }
        return serde_json::json!({});
    }
    serde_json::json!({})
}

/// A setting, or the default when it is absent or the wrong shape.
pub fn cfg_f64(cfg: &serde_json::Value, key: &str, fallback: f64) -> f64 {
    cfg.get(key).and_then(|v| v.as_f64()).unwrap_or(fallback)
}

pub fn cfg_usize(cfg: &serde_json::Value, key: &str, fallback: usize) -> usize {
    cfg.get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(fallback)
}

pub fn cfg_str(cfg: &serde_json::Value, key: &str, fallback: &str) -> String {
    cfg.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(fallback)
        .to_string()
}

pub fn cfg_strings(cfg: &serde_json::Value, key: &str, fallback: &[&str]) -> Vec<String> {
    match cfg.get(key).and_then(|v| v.as_array()) {
        Some(items) if !items.is_empty() => items
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => fallback.iter().map(|s| s.to_string()).collect(),
    }
}

/// Ask the terminal to put `text` on the system clipboard, via OSC 52.
///
/// The terminal emulator performs the copy, so this reaches the machine you
/// are sitting at even when the program runs on a remote host over SSH.
/// Multiplexers must be willing to forward it. Returns false when stdout is
/// not a terminal, so callers can fall back to showing the text instead.
pub fn clipboard(text: &str) -> bool {
    if unsafe { libc::isatty(1) } == 0 {
        return false;
    }
    out(&format!("\x1b]52;c;{}\x07", base64(text.as_bytes())));
    flush();
    true
}

/// Standard base64, because OSC 52 carries its payload that way.
fn base64(data: &[u8]) -> String {
    const SET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let mut block = [0u8; 3];
        block[..chunk.len()].copy_from_slice(chunk);
        let packed = u32::from(block[0]) << 16 | u32::from(block[1]) << 8 | u32::from(block[2]);
        for i in 0..4 {
            // Each output character is six bits; the ones past the end of a
            // short chunk are padding rather than zeroes.
            if i <= chunk.len() {
                out.push(SET[(packed >> (18 - 6 * i) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Green through amber to red.
///
/// Used wherever a fraction is a temperature - CPU, memory, how close a
/// number is to a limit - so the same load reads the same colour whichever
/// widget is showing it.
pub fn heat(frac: f64) -> String {
    let frac = frac.clamp(0.0, 1.0);
    if frac < 0.5 {
        let t = frac / 0.5;
        rgb((40.0 + 200.0 * t) as u8, 255, (120.0 - 100.0 * t) as u8)
    } else {
        let t = (frac - 0.5) / 0.5;
        rgb(255, (240.0 - 200.0 * t) as u8, (20.0 + 10.0 * t) as u8)
    }
}

/// Draw the reason a widget cannot run, and hold until q.
///
/// Exiting with a message loses it: a widget lives in a pane that is not
/// being watched at the moment it starts, and a line printed to a shell
/// that then sits at a prompt is indistinguishable from the widget never
/// having been launched. This stays on screen until somebody reads it.
pub fn cannot_start(name: &str, needed: &[String], why: &[&str], install: &str) {
    let bad = rgb(255, 100, 110);
    let dim = rgb(127, 147, 172);
    let txt = rgb(225, 235, 245);
    setup();
    let mut keyboard = Keyboard::new();
    loop {
        for key in keyboard.poll() {
            if key == "q" || key == "Q" {
                keyboard.restore();
                restore_screen();
                return;
            }
        }
        let (w, h) = size();
        let mut rows = vec![title(name, w, &bad), String::new()];
        rows.push(seg(
            &[
                (bad.as_str(), " cannot start · ".into()),
                (txt.as_str(), format!("needs {}", needed.join(", "))),
            ],
            w - 1,
        ));
        rows.push(String::new());
        for line in why {
            rows.push(seg(&[(dim.as_str(), format!(" {}", line))], w - 1));
        }
        if !install.is_empty() {
            rows.push(String::new());
            rows.push(seg(
                &[
                    (dim.as_str(), " try: ".into()),
                    (txt.as_str(), install.to_string()),
                ],
                w - 1,
            ));
        }
        while rows.len() < h - 1 {
            rows.push(String::new());
        }
        rows.push(seg(&[(dim.as_str(), " [q]uit".into())], w - 1));
        draw(&rows, w, h);
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Warn when a config file holding a token is readable by others.
///
/// Any widget that takes a token writes it into this file, so the check
/// belongs beside the loader rather than in whichever widget happened to
/// need it first.
pub fn config_token_warning() -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    for path in config_paths() {
        if !path.exists() {
            continue;
        }
        let mode = std::fs::metadata(&path).ok()?.permissions().mode() & 0o077;
        return if mode != 0 {
            Some("config.json is readable by others; chmod 600 it".into())
        } else {
            None
        };
    }
    None
}

/// One HTTPS GET, returning the body.
///
/// Through curl rather than an HTTP crate, for the same reason every other
/// source here is a subprocess: this collection reads `ss`, `ping`,
/// `tailscale` and `herdr` the same way, and a TLS stack would be forty
/// dependencies and a megabyte to do what curl already does on every
/// machine these run on.
///
/// The headers go in on **stdin**, never in the arguments. `/proc/<pid>/
/// cmdline` is world-readable, so a token on the command line is a token
/// handed to every user on the box for as long as the request lasts - and
/// these widgets exist partly to keep one out of the source tree.
pub fn get(url: &str, headers: &[(&str, &str)], seconds: u64) -> Result<String, String> {
    use std::io::Write;
    let mut config = format!(
        "--silent\n--show-error\n--fail\n--location\n--max-time {}\n--url {}\n",
        seconds,
        quoted(url)
    );
    for (name, value) in headers {
        config.push_str(&format!("--header {}\n", quoted(&format!("{}: {}", name, value))));
    }
    let mut child = std::process::Command::new("curl")
        .arg("--config")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("curl would not take its configuration")?
        .write_all(config.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).to_string());
    }
    // curl's own message, which names the status code for --fail. Whatever
    // it says, it must not be allowed to carry the header back out.
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if said.is_empty() {
        format!("curl exited {}", out.status.code().unwrap_or(-1))
    } else {
        said
    })
}

/// A value for curl's config format, which takes double quotes and
/// backslash escapes and would otherwise stop at the first space.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Which of these required commands are not on PATH.
pub fn missing(programs: &[&str]) -> Vec<String> {
    let path = std::env::var("PATH").unwrap_or_default();
    programs
        .iter()
        .filter(|p| {
            !path.split(':').any(|dir| {
                let candidate = std::path::Path::new(dir).join(p);
                candidate.is_file()
            })
        })
        .map(|p| p.to_string())
        .collect()
}

/// Non-blocking key input, decoding the sequences arrows arrive as.
///
/// Returns names for special keys and the bare character otherwise, and
/// restores the terminal's settings when dropped - including when the
/// widget exits by panicking.
pub struct Keyboard {
    fd: i32,
    saved: Option<libc::termios>,
    buf: Vec<u8>,
}

impl Keyboard {
    pub fn new() -> Keyboard {
        let fd = std::io::stdin().as_raw_fd();
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        let is_tty = unsafe { libc::isatty(fd) } == 1;
        let saved = if is_tty && unsafe { libc::tcgetattr(fd, &mut saved) } == 0 {
            let mut raw = saved;
            // cbreak, not full raw: characters arrive unbuffered and
            // unechoed, while the terminal keeps translating signals.
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 0;
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };
            Some(saved)
        } else {
            None
        };
        Keyboard {
            fd,
            saved,
            buf: Vec::new(),
        }
    }

    pub fn restore(&mut self) {
        if let Some(saved) = self.saved.take() {
            unsafe { libc::tcsetattr(self.fd, libc::TCSADRAIN, &saved) };
        }
    }

    /// Every key waiting, decoded. Empty when nothing has been pressed.
    pub fn poll(&mut self) -> Vec<String> {
        if self.saved.is_none() {
            return Vec::new();
        }
        let mut chunk = [0u8; 64];
        loop {
            let flags = unsafe { libc::fcntl(self.fd, libc::F_GETFL) };
            unsafe { libc::fcntl(self.fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
            let n = std::io::stdin().read(&mut chunk);
            unsafe { libc::fcntl(self.fd, libc::F_SETFL, flags) };
            match n {
                Ok(0) | Err(_) => break,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
            }
        }
        let text = String::from_utf8_lossy(&self.buf).to_string();
        self.buf.clear();
        decode(&text)
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Turn a run of input bytes into key names.
fn decode(text: &str) -> Vec<String> {
    const SEQUENCES: &[(&str, &str)] = &[
        ("\x1b[A", "up"),
        ("\x1b[B", "down"),
        ("\x1b[C", "right"),
        ("\x1b[D", "left"),
        ("\x1bOA", "up"),
        ("\x1bOB", "down"),
        ("\x1bOC", "right"),
        ("\x1bOD", "left"),
        ("\x1b[5~", "pgup"),
        ("\x1b[6~", "pgdn"),
        ("\x1b[H", "home"),
        ("\x1b[F", "end"),
        // The other encoding of the same two keys. Which one arrives
        // depends on the terminal and on whether it is in application
        // cursor mode, so both have to be understood or Home and End work
        // on some clients and not others.
        ("\x1b[1~", "home"),
        ("\x1b[4~", "end"),
    ];
    let mut keys = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '\x1b' {
            let rest: String = chars[i..].iter().collect();
            let found = SEQUENCES
                .iter()
                .find(|(seq, _)| rest.starts_with(seq))
                .map(|(seq, name)| (seq.chars().count(), *name));
            match found {
                Some((len, name)) => {
                    keys.push(name.to_string());
                    i += len;
                }
                None => {
                    keys.push("esc".to_string());
                    i += 1;
                }
            }
            continue;
        }
        let ch = chars[i];
        i += 1;
        match ch {
            '\r' | '\n' => keys.push("enter".to_string()),
            '\t' => keys.push("tab".to_string()),
            '\x7f' | '\x08' => keys.push("backspace".to_string()),
            c => keys.push(c.to_string()),
        }
    }
    keys
}

/// Print the doc comment and leave, when asked for help.
pub fn maybe_help(doc: &str) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", doc.trim());
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_curl_config_value_survives_spaces_and_quotes() {
        assert_eq!(quoted("simple"), "\"simple\"");
        // A header is "Name: value" and the space is the whole reason this
        // exists - unquoted, curl would read the rest as another option.
        assert_eq!(
            quoted("Authorization: Bearer abc"),
            "\"Authorization: Bearer abc\""
        );
        assert_eq!(quoted("a\"b"), "\"a\\\"b\"");
        assert_eq!(quoted("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn heat_runs_green_to_red_through_amber() {
        // The ends and the middle, since every widget reads the same scale
        // and a drift in it would make one pane disagree with the next.
        assert_eq!(heat(0.0), rgb(40, 255, 120));
        assert_eq!(heat(0.5), rgb(255, 240, 20));
        assert_eq!(heat(1.0), rgb(255, 40, 30));
        // Out of range is clamped rather than wrapped: a CPU reading over
        // 100% on a multicore box must not come back green.
        assert_eq!(heat(2.0), heat(1.0));
        assert_eq!(heat(-1.0), heat(0.0));
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        // Hand-rolled, and its output is invisible: the copy notice shows
        // the URL whatever actually landed on the clipboard, so a wrong
        // encoder would look like it worked. RFC 4648 section 10.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // The two characters that separate base64 from base64url, and a
        // byte above 127, since a URL may carry either.
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
        assert_eq!(base64("é".as_bytes()), "w6k=");
    }


    #[test]
    fn the_config_search_includes_the_working_directory() {
        // The bug this exists for: a compiled binary looked only beside
        // itself, which is target/release, and silently used defaults
        // while a real config sat in the project directory.
        let paths = config_paths();
        let cwd = std::env::current_dir().unwrap().join("config.json");
        assert!(paths.contains(&cwd), "cwd missing from {:?}", paths);
        assert!(
            paths.iter().any(|p| p.to_string_lossy().contains(".config/terminal-toys")),
            "the xdg location must stay, for an installed binary"
        );
    }

    #[test]
    fn seg_counts_only_the_text() {
        let red = rgb(255, 0, 0);
        // Twelve printable cells asked for, twelve delivered, however many
        // escape bytes rode along with them.
        let line = seg(&[(red.as_str(), "hello world!!!".into())], 12);
        let visible: String = line.replace(&red, "");
        assert_eq!(visible.chars().count(), 12);
    }

    #[test]
    fn pad_is_exact_in_both_directions() {
        assert_eq!(pad("ab", 5).chars().count(), 5);
        assert_eq!(pad("abcdefgh", 3), "abc");
    }

    #[test]
    fn title_fills_the_width() {
        let plain = strip(&title("clocks", 40, &rgb(0, 255, 170)));
        assert_eq!(plain.chars().count(), 40);
        assert!(plain.contains(" CLOCKS "));
    }

    #[test]
    fn arrows_decode_to_names() {
        assert_eq!(decode("\x1b[A"), vec!["up"]);
        assert_eq!(decode("\x1b[B\x1b[B"), vec!["down", "down"]);
        assert_eq!(decode("q"), vec!["q"]);
        assert_eq!(decode("\x1b"), vec!["esc"]);
        // Both encodings of Home and End, since terminals disagree.
        assert_eq!(decode("\x1b[H"), vec!["home"]);
        assert_eq!(decode("\x1b[1~"), vec!["home"]);
        assert_eq!(decode("\x1b[F"), vec!["end"]);
        assert_eq!(decode("\x1b[4~"), vec!["end"]);
        assert_eq!(decode("\r"), vec!["enter"]);
    }

    #[test]
    fn hints_wrap_rather_than_split() {
        let dim = rgb(1, 1, 1);
        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(dim.as_str(), "[a]lpha".into())],
            vec![(dim.as_str(), "[b]ravo".into())],
            vec![(dim.as_str(), "[c]harlie".into())],
        ];
        let lines = pack_hints(&hints, 20, "  ");
        assert!(lines.len() > 1);
        for line in &lines {
            let plain = strip(line);
            assert!(plain.chars().count() <= 20, "{:?} is too wide", plain);
            // A hint is never cut in half.
            assert!(!plain.ends_with("[c]har"));
        }
    }

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                while let Some(n) = chars.next() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
