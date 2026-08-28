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

//! What every widget shares: the terminal, the keyboard, and the drawing.
//!
//! A port of `common.py`, kept deliberately close to it. The widgets are
//! being translated one at a time and the two versions have to sit side by
//! side and agree on screen, so where a choice existed this keeps the
//! Python behaviour rather than the more idiomatic Rust one.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

pub const HIDE: &str = "\x1b[?25l";
pub const SHOW: &str = "\x1b[?25h";
pub const HOME: &str = "\x1b[H";
pub const CLEAR: &str = "\x1b[2J";
pub const EL: &str = "\x1b[K";
pub const RST: &str = "\x1b[0m";
pub const NOBG: &str = "\x1b[49m";

/// Eight levels used by compact bar charts across the widgets.
pub const SPARK: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The dot mask for each pixel in a two-by-four braille cell.
pub const BRAILLE: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// A consistent animation for work that has not finished yet.
pub const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Seconds since the Unix epoch, for elapsed-time and cache timestamps.
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Lay coloured braille canvases over one another.
///
/// A cell can only have one foreground colour. Contested cells therefore
/// alternate claimants along each row, rather than allowing the last layer
/// to recolour an entire trace or using biased column parity.
pub fn overlay(
    layers: &[(String, Vec<Vec<u8>>)],
    cols: usize,
    rows: usize,
) -> Vec<Vec<(String, u8)>> {
    let mut cells = vec![vec![(String::new(), 0u8); cols]; rows];
    for y in 0..rows {
        let mut turn = 0usize;
        for x in 0..cols {
            let dots = |canvas: &Vec<Vec<u8>>| {
                canvas.get(y).and_then(|line| line.get(x)).copied().unwrap_or(0)
            };
            let claims: Vec<&(String, Vec<Vec<u8>>)> =
                layers.iter().filter(|(_, canvas)| dots(canvas) != 0).collect();
            let Some((colour, canvas)) = claims.get(turn % claims.len().max(1)) else {
                continue;
            };
            cells[y][x] = ((*colour).clone(), dots(canvas));
            if claims.len() > 1 {
                turn += 1;
            }
        }
    }
    cells
}

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

/// The bytes `handle_signal` writes. Built as a constant so the handler
/// never formats, allocates, or takes the stdout lock - any of which can
/// deadlock if the signal arrives while `draw` is already writing.
const SCREEN_RESTORE: &str = concat!("\x1b[?25h", "\x1b[0m", "\x1b[2J", "\x1b[H");

/// Saved cbreak settings, written by `Keyboard` and read by the handler.
///
/// The flag is published after the struct, so a handler that sees it true
/// sees a complete copy. A mutex is not an option: locking one from a
/// signal handler is how the previous version could hang instead of
/// restoring the terminal.
static TERM_FD: AtomicI32 = AtomicI32::new(-1);
static HAS_TERMIOS: AtomicBool = AtomicBool::new(false);
static mut SAVED_IOS: libc::termios = unsafe { std::mem::zeroed() };

fn remember_termios(fd: i32, ios: libc::termios) {
    unsafe {
        SAVED_IOS = ios;
    }
    TERM_FD.store(fd, Ordering::Release);
    HAS_TERMIOS.store(true, Ordering::Release);
}

fn forget_termios() {
    HAS_TERMIOS.store(false, Ordering::Release);
}

/// Restore the saved termios and the screen, using only async-signal-safe
/// calls, then `_exit`. `process::exit` runs atexit handlers and can
/// deadlock on the same stdout lock `draw` holds; Drop on `Keyboard` never
/// runs either way, so the handler has to give the shell its echo back.
extern "C" fn handle_signal(sig: libc::c_int) {
    if HAS_TERMIOS.load(Ordering::Acquire) {
        let fd = TERM_FD.load(Ordering::Acquire);
        if fd >= 0 {
            let ios = unsafe { SAVED_IOS };
            unsafe {
                libc::tcsetattr(fd, libc::TCSANOW, &ios);
            }
        }
    }
    unsafe {
        libc::write(
            libc::STDOUT_FILENO,
            SCREEN_RESTORE.as_ptr() as *const libc::c_void,
            SCREEN_RESTORE.len(),
        );
        libc::_exit(128 + sig);
    }
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
/// What this project was called before it was opscope.
///
/// Assembled from pieces rather than written out, because these two are the
/// only strings in the tree that a rename of the old name must NOT touch -
/// and the first pass of that rename replaced them both, which is precisely
/// how somebody's tokens quietly stop being found. A test drives both
/// paths; this is the belt to its braces.
const LEGACY_DIR: &str = concat!("terminal", "-", "toys");
const LEGACY_ENV: &str = concat!("TERMINAL", "_", "TOYS", "_CONFIG");

pub fn config_paths() -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    // OPSCOPE_CONFIG first, then the name this project used before it was
    // called opscope. Both are kept, and the old one is not deprecated on
    // a timer: a widget that silently stops finding its settings shows an
    // empty pane, which is indistinguishable from a source with no data -
    // the exact failure this codebase spends most of its checks avoiding.
    // Costing two getenv calls to never do that is a good trade.
    for var in ["OPSCOPE_CONFIG", LEGACY_ENV] {
        if let Ok(env) = std::env::var(var) {
            if !env.is_empty() {
                found.push(std::path::PathBuf::from(env));
            }
        }
    }
    let xdg = std::env::var("XDG_CONFIG_HOME").ok().filter(|s| !s.is_empty());
    let home = std::env::var("HOME").unwrap_or_default();
    let base = std::path::PathBuf::from(xdg.unwrap_or(format!("{}/.config", home)));
    // Same again for the config directory. New name wins where both exist,
    // so moving the file is how you migrate and nothing has to be deleted.
    found.push(base.join("opscope/config.json"));
    found.push(base.join(format!("{}/config.json", LEGACY_DIR)));
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

/// A wait that `Duration::from_secs_f64` will accept.
///
/// A missing or malformed setting falls back. A value that is finite and
/// positive is kept. Anything else - negative, NaN, infinite - used to
/// panic the poller after the first read, which froze the pane on its
/// initial data with no error, the same silence a dead thread leaves.
pub fn poll_secs(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else if fallback.is_finite() && fallback > 0.0 {
        fallback
    } else {
        1.0
    }
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

/// A list of settings, or the default when nobody has given one.
///
/// A list that is there and empty is an answer, not the absence of one, and
/// it is the one case the earlier version got wrong: it read `[]` as "unset"
/// and handed back the fallback, so `"hosts": []` had latency pinging two
/// addresses the config had just said it did not want. Absent means nobody
/// has said what they want and a default is a kindness; present means they
/// have, and substituting a list of our own is the widget talking about
/// something it was not asked about.
///
/// Anything that is not an array at all - a string where a list belongs -
/// is not an answer either, so it falls back with the absent case.
pub fn cfg_strings(cfg: &serde_json::Value, key: &str, fallback: &[&str]) -> Vec<String> {
    match cfg.get(key).and_then(|v| v.as_array()) {
        Some(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        None => fallback.iter().map(|s| s.to_string()).collect(),
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

/// One HTTPS GET, returning the body and its headers.
///
/// Same reason as `post_json`: a rate limit is only knowable from the
/// headers, and a widget that polls REST every minute should be able to
/// say how much of its hour it has left. `get` keeps the body-only shape
/// the other callers already use.
pub fn get_with_headers(
    url: &str,
    headers: &[(&str, &str)],
    seconds: u64,
) -> Result<(String, Vec<(String, String)>), String> {
    use std::io::Write;
    let mut config = format!(
        "--silent\n--show-error\n--request GET\n--location\n--dump-header -\n\
         --max-time {}\n--url {}\n",
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
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if said.is_empty() {
            format!("curl exited {}", out.status.code().unwrap_or(-1))
        } else {
            said
        });
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (head, body) = split_response(&text);
    let status = head
        .first()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let found: Vec<(String, String)> = head
        .iter()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    if !(200..300).contains(&status) {
        return Err(refused(status, &body));
    }
    Ok((body, found))
}

/// One HTTPS POST of a JSON body, returning the body and its headers.
///
/// The headers come back because a rate limit is only knowable from them,
/// and a widget that polls an API every two minutes should be able to say
/// how much of its hour it has left.
///
/// Not `--fail`: a GraphQL endpoint answers 200 with an errors array, and
/// on a 4xx the body is usually the only thing that says what was wrong.
/// The status is read off the dumped headers instead.
pub fn post_json(
    url: &str,
    headers: &[(&str, &str)],
    body: &str,
    seconds: u64,
) -> Result<(String, Vec<(String, String)>), String> {
    use std::io::Write;
    let mut config = format!(
        "--silent\n--show-error\n--request POST\n--dump-header -\n\
         --max-time {}\n--url {}\n--data {}\n",
        seconds,
        quoted(url),
        quoted(body)
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
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if said.is_empty() {
            format!("curl exited {}", out.status.code().unwrap_or(-1))
        } else {
            said
        });
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (head, body) = split_response(&text);
    let status = head
        .first()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let found: Vec<(String, String)> = head
        .iter()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    if !(200..300).contains(&status) {
        return Err(refused(status, &body));
    }
    Ok((body, found))
}

/// What a refused POST says: the status, and the body that explains it.
///
/// The status on its own is the half of a refusal that never says what to
/// do about it. Linear and GitHub put the missing scope, the malformed
/// field and the expired token in the body - which is the case the comment
/// on `post_json` keeps the body for - and returning `HTTP 400` alone sent
/// a reader to the API documentation for something the API had already
/// explained.
///
/// Squeezed onto one line and capped the way every other subprocess
/// complaint here is, so an HTML error page cannot take the whole pane.
fn refused(status: u16, body: &str) -> String {
    let said: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let said: String = said.chars().take(200).collect();
    if said.is_empty() {
        format!("HTTP {}", status)
    } else {
        format!("HTTP {}: {}", status, said)
    }
}

/// Split curl's `--dump-header -` output into its last header block and
/// the body under it.
///
/// The last block, because a redirect or a `100 Continue` leaves earlier
/// ones in front of it, and the one that describes the response is the one
/// nearest the body.
fn split_response(text: &str) -> (Vec<String>, String) {
    let mut head: Vec<String> = Vec::new();
    let mut rest = text;
    loop {
        let mut lines = Vec::new();
        let mut at = 0usize;
        let mut ended = false;
        for line in rest.split_inclusive('\n') {
            at += line.len();
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                ended = true;
                break;
            }
            lines.push(trimmed.to_string());
        }
        if !ended || lines.is_empty() || !lines[0].starts_with("HTTP/") {
            break;
        }
        head = lines;
        rest = &rest[at..];
    }
    (head, rest.to_string())
}

/// A value for curl's config format, which takes double quotes and
/// backslash escapes and would otherwise stop at the first space - or, for
/// a GraphQL query, at the end of its first line.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Proportions as one bar: (fraction, colour) pairs to coloured segments.
///
/// A bar beats a pie in a character grid - no aliasing, and the eye compares
/// lengths far better than angles. The last segment takes whatever rounding
/// left over, so the bar is always exactly its width.
pub fn stacked_bar(parts: &[(f64, String)], width: usize) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut used = 0usize;
    for (i, (frac, colour)) in parts.iter().enumerate() {
        let n = if i + 1 == parts.len() {
            width.saturating_sub(used)
        } else {
            ((frac * width as f64).round() as usize).min(width.saturating_sub(used))
        };
        if n > 0 {
            out.push((colour.clone(), "█".repeat(n)));
            used += n;
        }
    }
    out
}

/// A filled fraction of a fixed-width track.
pub fn meter(frac: f64, n: usize) -> String {
    let filled = ((frac.clamp(0.0, 1.0) * n as f64).round() as usize).min(n);
    format!("{}{}", "█".repeat(filled), "░".repeat(n - filled))
}

const EIGHTHS: &[char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Where a window of `room` rows has to start to keep `row` in view.
///
/// Every list here that is longer than its pane is drawn whole and shown
/// through a window, and they all move it the same way: linear's board and
/// its detail screens, herdr-panes' three sections. It is one function so
/// that a test can break it, which a copy inlined in each could not have.
pub fn follow(at: usize, row: usize, room: usize) -> usize {
    if row < at {
        row
    } else if row + 1 > at + room {
        row + 1 - room
    } else {
        at
    }
}


/// Vertical bar chart, one column per value.
///
/// Each cell resolves an eighth of a row through the partial-block glyphs,
/// so a five-row chart has forty levels rather than five. `hi` fixes the
/// full-scale value so two charts can share a scale and stay comparable.
pub fn vbars(columns: &[(f64, String)], height: usize, hi: f64) -> Vec<Vec<(String, String)>> {
    let hi = if hi > 0.0 {
        hi
    } else {
        columns.iter().map(|(v, _)| *v).fold(0.0, f64::max).max(1.0)
    };
    (0..height)
        .map(|r| {
            let top = hi * (height - r) as f64 / height as f64;
            let bottom = hi * (height - r - 1) as f64 / height as f64;
            columns
                .iter()
                .map(|(value, colour)| {
                    let ch = if *value >= top {
                        '█'
                    } else if *value <= bottom {
                        ' '
                    } else {
                        let step = ((value - bottom) / (top - bottom) * 8.0) as usize;
                        EIGHTHS[step.clamp(1, 8)]
                    };
                    (colour.clone(), ch.to_string())
                })
                .collect()
        })
        .collect()
}

/// Bar chart hanging downward from a baseline above it.
///
/// Paired with `vbars` and a shared `hi`, this makes a diverging chart: one
/// series growing up, another down, one column per day.
///
/// The partial-block glyphs are all bottom-anchored, so a downward bar
/// cannot resolve an eighth of a cell the way `vbars` does - only `▀`
/// exists as a top-anchored partial. Half a cell is ample once peaks are
/// scaled, and the alternative needs the terminal's background painted,
/// which these widgets deliberately never do.
pub fn vbars_down(columns: &[(f64, String)], height: usize, hi: f64) -> Vec<Vec<(String, String)>> {
    let hi = if hi > 0.0 {
        hi
    } else {
        columns.iter().map(|(v, _)| *v).fold(0.0, f64::max).max(1.0)
    };
    (0..height)
        .map(|r| {
            let full = hi * (r + 1) as f64 / height as f64;
            let empty = hi * r as f64 / height as f64;
            columns
                .iter()
                .map(|(value, colour)| {
                    let ch = if *value >= full {
                        '█'
                    } else if *value <= empty {
                        ' '
                    } else if (value - empty) / (full - empty) >= 0.5 {
                        '▀'
                    } else {
                        ' '
                    };
                    (colour.clone(), ch.to_string())
                })
                .collect()
        })
        .collect()
}

/// Column heights in 0..1 bouncing like a level meter, for pending data.
///
/// Two sine waves of different periods per column, so neighbours move
/// together enough to read as one instrument but never march in lockstep.
/// Deterministic in `tick`, so every frame is reproducible and no random
/// source is needed.
pub fn dance(width: usize, tick: usize, phase: f64) -> Vec<f64> {
    (0..width)
        .map(|i| {
            let t = tick as f64;
            let i = i as f64;
            let a = (t * 0.55 + i * 0.85 + phase).sin();
            let b = (t * 0.31 + i * 0.41 + phase * 1.7).sin();
            (0.5 + 0.33 * a + 0.17 * b).clamp(0.08, 1.0)
        })
        .collect()
}

/// Blend two colours, for fading a placeholder into real data.
pub fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> String {
    let t = t.clamp(0.0, 1.0);
    let step = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    rgb(step(a.0, b.0), step(a.1, b.1), step(a.2, b.2))
}

/// The next entry after `current`, wrapping; for a key that cycles.
pub fn cycle<T: PartialEq + Copy>(choices: &[T], current: T) -> T {
    // A value that is not one of the choices starts from the first, as
    // common.py's ValueError branch does. unwrap_or(0) then +1 started from
    // the second instead, silently skipping a choice whenever the current
    // setting came from a config file or an argument rather than from a
    // previous press of the key.
    match choices.iter().position(|c| *c == current) {
        Some(at) => choices[(at + 1) % choices.len()],
        None => choices[0],
    }
}

/// Where `tab` goes next, on the rule every widget with focusable sections
/// follows.
///
/// `lens` is how long each section is right now. Focus moves to the next
/// section after the current one and, from the last, off the end to `None` -
/// so there is a way to put the cursor away, which a ring that only cycles
/// between the sections does not give you.
///
/// Empty sections are stepped over. A section with no rows is not a place
/// you can be: focusing one leaves the arrows moving an index nothing is
/// drawn from and a footer offering "select" over nothing, which reads as a
/// key that has stopped working. From `None`, this is also how you find the
/// first section worth entering.
pub fn next_section(focus: Option<usize>, lens: &[usize]) -> Option<usize> {
    let from = focus.map_or(0, |at| at + 1);
    (from..lens.len()).find(|&at| lens[at] > 0)
}

/// One step of the cursor when a section has the focus.
///
/// Returns the section and row it lands on, or `None` when it leaves the
/// sections entirely - which happens at exactly two places: up from the first
/// row of the first section, and down from the last row of the last one.
///
/// Everywhere else the sections read as one continuous list. Walking off the
/// bottom of a section steps into the top of the next, and walking off the
/// top steps into the *bottom* of the one above - the row you were about to
/// reach if the two had been a single list. Stepping out to nothing in the
/// middle of a screen made the arrows stop for a reason the screen could not
/// show, and left `tab` as the only way to reach a section you were sitting
/// right next to.
///
/// Empty sections are stepped over in both directions, for the reason
/// `next_section` steps over them. A section that empties under the cursor
/// escapes rather than trapping it.
pub fn step_across_sections(
    focus: usize,
    at: usize,
    lens: &[usize],
    down: bool,
) -> Option<(usize, usize)> {
    let len = lens.get(focus).copied().unwrap_or(0);
    if down {
        if at + 1 < len {
            return Some((focus, at + 1));
        }
        let next = (focus + 1..lens.len()).find(|&i| lens[i] > 0)?;
        Some((next, 0))
    } else {
        if at > 0 && at < len {
            return Some((focus, at - 1));
        }
        let above = (0..focus.min(lens.len())).rev().find(|&i| lens[i] > 0)?;
        Some((above, lens[above] - 1))
    }
}

/// A placeholder bar with a highlight sweeping across it.
///
/// For values that are being refetched: showing the previous number while a
/// new one is in flight states something false, and blanking the row makes
/// the layout jump. A shimmering grey bar says "pending" without either.
pub fn skeleton(width: usize, tick: usize, span: usize) -> Vec<(String, String)> {
    let period = width + span * 2;
    let centre = (tick % period) as f64 - span as f64;
    let mut out: Vec<(String, String)> = Vec::new();
    for i in 0..width {
        let near = (1.0 - (i as f64 - centre).abs() / span as f64).max(0.0);
        let level = (58.0 + near * 118.0) as u8;
        let colour = rgb(level, level, level.saturating_add(8));
        match out.last_mut() {
            Some((had, run)) if *had == colour => run.push('█'),
            _ => out.push((colour, "█".to_string())),
        }
    }
    out
}

/// A label that is visibly still working, for a source that has not answered.
///
/// Two rows: the label behind a Braille spinner, and one sweeping line under
/// it. The movement is the whole point. A pane waiting on a slow API and a
/// pane whose poller has died draw the same static sentence, and telling
/// those apart is most of what `widgets/tests/check.rs` exists for - so the
/// wait says "still going" the only way a terminal can.
///
/// It deliberately claims no progress. There is no bar creeping towards a
/// total nobody counted: a widget that knows how far along it is should say
/// so in words, and one that does not should not draw a number it invented.
///
/// `lit` colours the spinner, `dim` the label. Both are passed in rather
/// than chosen here, because every palette in this repo is defined beside
/// the widget that uses it and the contrast check reads those files.
pub fn waiting(label: &str, w: usize, tick: usize, lit: &str, dim: &str) -> Vec<String> {
    let head = seg(
        &[
            (lit, format!("  {} ", SPINNER[tick % SPINNER.len()])),
            (dim, label.to_string()),
        ],
        w.saturating_sub(1),
    );
    // Twice the tick, so the sweep is quicker than the spinner and the two
    // do not appear to be one mechanism running slow.
    let mut line: Vec<(&str, String)> = vec![(RST, "  ".into())];
    let shimmer = skeleton(w.saturating_sub(6).max(10), tick * 2, 7);
    for (colour, txt) in &shimmer {
        line.push((colour.as_str(), txt.clone()));
    }
    vec![head, seg(&line, w.saturating_sub(1))]
}

/// Cell widths for `count` bars that fill `room` columns exactly.
///
/// The remainder goes to the leftmost bars rather than being dropped on the
/// floor by integer division: stopping short of the right edge leaves no way
/// to tell a finished chart from a truncated one. Twenty-eight days across
/// fifty-nine columns is two cells each and three columns wasted, which
/// reads as a chart that gave up.
pub fn spread(count: usize, room: usize) -> Vec<usize> {
    if count == 0 {
        return Vec::new();
    }
    if count >= room {
        // One cell each and the caller decides what to drop: silently
        // returning fewer widths than bars would lose data without saying so.
        return vec![1; count];
    }
    let (slot, extra) = (room / count, room % count);
    (0..count)
        .map(|i| slot + usize::from(i < extra))
        .collect()
}

/// Which of these required commands are not on PATH.
pub fn missing(programs: &[&str]) -> Vec<String> {
    let path = std::env::var("PATH").unwrap_or_default();
    programs
        .iter()
        .filter(|p| {
            !path.split(':').any(|dir| {
                let candidate = std::path::Path::new(dir).join(p);
                // `is_file` is not enough: shutil.which, which the Python
                // uses, also requires the executable bit. A readable but
                // non-executable file of the right name on PATH would
                // otherwise count as the tool being present, and the widget
                // would start and then fail on every call instead of saying
                // up front what it needs.
                candidate.is_file()
                    && std::fs::metadata(&candidate)
                        .map(|m| m.permissions().mode() & 0o111 != 0)
                        .unwrap_or(false)
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
    /// What has arrived but not yet decoded. Kept between polls, because a
    /// sequence can be torn across two reads on a slow link and half an
    /// arrow key is not an Escape.
    pending: String,
    /// A bare ESC is held for one poll before it counts as Escape: it is
    /// indistinguishable from the start of a sequence still arriving.
    lone_esc: bool,
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
            remember_termios(fd, saved);
            Some(saved)
        } else {
            None
        };
        Keyboard {
            pending: String::new(),
            lone_esc: false,
            fd,
            saved,
            buf: Vec::new(),
        }
    }

    pub fn restore(&mut self) {
        if let Some(saved) = self.saved.take() {
            forget_termios();
            unsafe { libc::tcsetattr(self.fd, libc::TCSADRAIN, &saved) };
        }
    }

    /// Take the terminal back after handing it to a child.
    ///
    /// `restore` gives the saved settings back and forgets them, which is
    /// right on the way out and wrong in a launcher: the menu has to
    /// return to cbreak once the widget it started has finished with the
    /// terminal. Anything typed while the child had the keyboard belongs
    /// to the child, so what is left of it is dropped rather than
    /// delivered to the menu a moment later.
    pub fn reclaim(&mut self) {
        if self.saved.is_some() || unsafe { libc::isatty(self.fd) } != 1 {
            return;
        }
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(self.fd, &mut saved) } != 0 {
            return;
        }
        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &raw) };
        remember_termios(self.fd, saved);
        self.saved = Some(saved);
        self.buf.clear();
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
        self.pending
            .push_str(&String::from_utf8_lossy(&self.buf).to_string());
        self.buf.clear();
        decode(&mut self.pending, &mut self.lone_esc)
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Turn a run of input bytes into key names.
/// How long the unmapped escape sequence at the front of `s` is, in chars.
///
/// The shapes a terminal actually sends: CSI is ESC [ then parameter
/// digits and semicolons then a final letter or tilde; SS3 is ESC O then
/// one letter. Anything matching is a key this program does not use -
/// Delete, a function key, a focus report, a bracketed-paste marker - and
/// must be swallowed whole. Emitting it a character at a time is how F9
/// came to reset the pomodoro count, "0" being a real binding in clocks.
fn escape_len(s: &[char]) -> Option<usize> {
    if s.first() != Some(&'\x1b') {
        return None;
    }
    match s.get(1) {
        Some('[') => {
            let mut i = 2;
            while matches!(s.get(i), Some(c) if c.is_ascii_digit() || *c == ';') {
                i += 1;
            }
            match s.get(i) {
                Some(c) if c.is_ascii_alphabetic() || *c == '~' => Some(i + 1),
                _ => None,
            }
        }
        Some('O') => match s.get(2) {
            Some(c) if c.is_ascii_alphabetic() => Some(3),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `s` is the start of a sequence that has not finished arriving.
///
/// ESC, ESC O, and ESC [ with only parameter characters after it can all
/// still become a key. Holding them costs nothing - the next read either
/// completes the sequence or makes it a malformed escape - and it is the
/// difference between a torn arrow key being an arrow and it being three
/// characters that other keys are bound to.
fn still_arriving(s: &[char]) -> bool {
    match s {
        [] => false,
        ['\x1b'] => true,
        ['\x1b', 'O'] => true,
        ['\x1b', '[', rest @ ..] => rest
            .iter()
            .all(|c| c.is_ascii_digit() || *c == ';'),
        _ => false,
    }
}

/// Turn what the terminal sent into key names, leaving anything incomplete
/// in `buf` for the next poll.
///
/// This mirrors common.py's Keyboard.poll rather than reinventing it: the
/// order of the checks is what decides whether an unknown key is silently
/// ignored or arrives as a handful of characters that other keys are bound
/// to.
fn decode(buf: &mut String, lone_esc: &mut bool) -> Vec<String> {
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
    let mut chars: Vec<char> = buf.chars().collect();
    let mut at = 0usize;
    while at < chars.len() {
        if chars[at] == '\x1b' {
            let rest: String = chars[at..].iter().collect();
            // Longest wins: ESC [ 1 ~ is Home, not ESC [ 1 followed by ~.
            let found = SEQUENCES
                .iter()
                .filter(|(seq, _)| rest.starts_with(seq))
                .max_by_key(|(seq, _)| seq.chars().count());
            if let Some((seq, name)) = found {
                keys.push((*name).to_string());
                at += seq.chars().count();
                continue;
            }
            if let Some(len) = escape_len(&chars[at..]) {
                at += len; // a sequence this program does not map; drop it
                continue;
            }
            if still_arriving(&chars[at..]) {
                // Either a bare ESC or a half-arrived sequence. A bare ESC
                // only counts as Escape once a second poll has found
                // nothing following it; a longer prefix is simply kept,
                // since it cannot be Escape at all.
                if chars.len() - at == 1 {
                    if *lone_esc {
                        at += 1;
                        *lone_esc = false;
                        keys.push("esc".to_string());
                    } else {
                        *lone_esc = true;
                    }
                }
                break;
            }
            at += 1; // malformed; discard the ESC and carry on
            continue;
        }
        let ch = chars[at];
        at += 1;
        match ch {
            '\r' | '\n' => keys.push("enter".to_string()),
            '\t' => keys.push("tab".to_string()),
            '\x7f' | '\x08' => keys.push("backspace".to_string()),
            c => keys.push(c.to_string()),
        }
    }
    chars.drain(..at);
    *buf = chars.into_iter().collect();
    if buf != "\x1b" {
        *lone_esc = false;
    }
    keys
}

/// Run a command and give up on it after `seconds`.
///
/// std's `.output()` waits forever, and every one of these commands talks
/// to something that can stop answering - a tailnet coordination server,
/// a Herdr socket, a wedged `ss`. The Pythons all pass a timeout; the port
/// dropped them, so a hung child froze the poll thread with no error and
/// the pane kept drawing its last frame as though it were current.
///
/// Returns everything the child produced, so a caller that needs the exit
/// status or stderr - to say why a command refused - still has them. Both
/// pipes are captured for that reason: `run` below and `ports`'s `refusal`
/// each read `stderr` to name a refusal, and while it was sent to /dev/null
/// they had nothing to read, so "tailscale serve needs an operator" and
/// every other permission, login and bad-argument message arrived as an
/// exit status or as the word "refused".
pub fn run_full(args: &[&str], seconds: u64) -> Result<std::process::Output, String> {
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    let Some((program, rest)) = args.split_first() else {
        return Err("no command given".into());
    };
    let child = Command::new(program)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {}", program, e))?;
    let pid = child.id() as i32;
    let (tx, rx) = mpsc::channel();
    // wait_with_output drains both pipes while it waits; doing the wait on
    // this side and the reads afterwards would deadlock on a child that
    // fills its stdout or stderr buffer.
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(std::time::Duration::from_secs(seconds)) {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(format!("{}: {}", program, e)),
        Err(_) => {
            // SIGKILL rather than SIGTERM: this one has already ignored the
            // time it was given, and the reader thread ends when it dies.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            Err(format!("{} did not answer in {}s", program, seconds))
        }
    }
}

/// The same, reduced to the stdout of a command that succeeded.
///
/// A command that ran and failed is an error here, not empty output: the
/// callers that want the difference use run_full and read the status.
pub fn run(args: &[&str], seconds: u64) -> Result<String, String> {
    let out = run_full(args, seconds)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let why = String::from_utf8_lossy(&out.stderr);
        let why: String = why.split_whitespace().collect::<Vec<_>>().join(" ");
        Err(if why.is_empty() {
            format!("{} exited {}", args[0], out.status)
        } else {
            why.chars().take(200).collect()
        })
    }
}

/// Run a bounded command for callers where absence is non-fatal.
pub fn run_quiet(args: &[&str], seconds: u64) -> String {
    run(args, seconds).unwrap_or_default()
}

/// Print the doc comment and leave, when asked for help.
pub fn maybe_help(doc: &str) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", doc.trim());
        std::process::exit(0);
    }
    // Answered here rather than by each widget, for the same reason `--help`
    // is: fourteen binaries that disagree about how to say their own version
    // are fourteen answers to one question. netwatch used to answer this
    // itself and said "netwatch 1.1" while the workspace was at 0.1.0 - a
    // number nothing set, kept up to date by nobody.
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("{}", version());
        std::process::exit(0);
    }
}

/// What this binary is, in one line.
///
/// The version, the commit it was built from, and the date of that commit.
/// The version alone is not enough to identify a build: it changes only at a
/// release, and almost every binary anybody runs is somewhere between two.
///
/// Any of the three may read `unknown` - built from a tarball with no `.git`,
/// say. That is the honest answer and it is not a build failure; a
/// `--version` that had to guess would be worse than one that admits it.
pub fn version() -> String {
    format!(
        "{} {} ({}, {})",
        binary_name(),
        env!("CARGO_PKG_VERSION"),
        env!("TOYS_COMMIT"),
        env!("TOYS_BUILD_DATE"),
    )
}

/// The name this binary was invoked as, for the first word of `--version`.
fn binary_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "opscope".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_wait_that_does_not_move_is_indistinguishable_from_a_dead_poller() {
        // The point of the helper is the movement, so the test is that
        // consecutive ticks differ. A static sentence would pass any
        // assertion about the label alone, which is how this would rot.
        let frames: Vec<Vec<String>> = (0..6)
            .map(|t| super::waiting("waiting for GitHub…", 60, t, "", ""))
            .collect();
        for pair in frames.windows(2) {
            assert_ne!(pair[0], pair[1], "two ticks drew the same frame");
        }
        for f in &frames {
            assert_eq!(f.len(), 2, "a label row and a sweep row");
            assert!(f[0].contains("waiting for GitHub…"), "{}", f[0]);
        }
        // Narrow panes still get both rows rather than a panic on the
        // saturating widths.
        for w in [0usize, 1, 4, 12] {
            assert_eq!(super::waiting("x", w, 3, "", "").len(), 2, "width {}", w);
        }
    }

    #[test]
    fn the_window_chases_a_cursor_it_cannot_see() {
        // Stated as what the reader sees rather than as the arithmetic:
        // wherever the cursor is, the window has to contain it, and it has
        // to move as little as it can to do that.
        let holds = |at: usize, row: usize, room: usize| row >= at && row < at + room;
        for room in [1usize, 3, 20] {
            for start in [0usize, 5, 30] {
                for row in [0usize, 4, 7, 12, 40] {
                    let moved = follow(start, row, room);
                    assert!(holds(moved, row, room), "row {} not in {}..+{}", row, moved, room);
                    // Not moved at all when it did not need to be.
                    if holds(start, row, room) {
                        assert_eq!(moved, start, "moved without needing to");
                    }
                }
            }
        }
        // Reaching down puts the cursor on the last row, not past it.
        assert_eq!(follow(0, 40, 20) + 20, 41);
        // Reaching up puts it on the first.
        assert_eq!(follow(30, 4, 20), 4);
    }


    // The rule every widget with focusable sections follows. It is tested
    // here rather than in each widget because "the same rule everywhere" is
    // a claim two hand-written copies cannot keep.

    #[test]
    fn tab_walks_the_sections_and_then_off_the_end() {
        let lens = [3usize, 2, 4];
        assert_eq!(next_section(None, &lens), Some(0), "opens into the first");
        assert_eq!(next_section(Some(0), &lens), Some(1));
        assert_eq!(next_section(Some(1), &lens), Some(2));
        assert_eq!(
            next_section(Some(2), &lens),
            None,
            "the last section leads out, not back to the first"
        );
    }

    #[test]
    fn tab_steps_over_sections_with_nothing_in_them() {
        // The middle one is empty: focusing it would offer "select" over
        // nothing and the next arrow would silently leave again.
        assert_eq!(next_section(Some(0), &[3, 0, 4]), Some(2));
        assert_eq!(next_section(None, &[0, 0, 4]), Some(2), "skips a run of them");
        assert_eq!(next_section(Some(0), &[3, 0, 0]), None, "nothing left to enter");
        assert_eq!(next_section(None, &[0, 0, 0]), None, "an empty screen has nowhere to go");
    }

    #[test]
    fn walking_off_a_section_steps_into_the_next_one() {
        let lens = [3usize, 2, 4];
        // down, out of the first section and into the top of the second
        assert_eq!(step_across_sections(0, 1, &lens, true), Some((0, 2)));
        assert_eq!(step_across_sections(0, 2, &lens, true), Some((1, 0)));
        // up, out of the second and into the *bottom* of the first - the row
        // you were about to reach if the two had been one list
        assert_eq!(step_across_sections(1, 1, &lens, false), Some((1, 0)));
        assert_eq!(step_across_sections(1, 0, &lens, false), Some((0, 2)));
    }

    #[test]
    fn only_the_two_far_ends_leave_the_sections() {
        let lens = [3usize, 2, 4];
        assert_eq!(
            step_across_sections(0, 0, &lens, false),
            None,
            "up from the first row of the first section"
        );
        assert_eq!(
            step_across_sections(2, 3, &lens, true),
            None,
            "down from the last row of the last section"
        );
        // and nowhere in between
        for (focus, at) in [(0usize, 2usize), (1, 0), (1, 1), (2, 0)] {
            assert!(
                step_across_sections(focus, at, &lens, true).is_some()
                    || step_across_sections(focus, at, &lens, false).is_some(),
                "section {} row {} had nowhere to go",
                focus,
                at
            );
        }
    }

    #[test]
    fn a_whole_section_can_be_crossed_in_either_direction() {
        // Every row of every section, in order, walking down and back up.
        let lens = [2usize, 1, 3];
        let mut seen = vec![(0usize, 0usize)];
        let (mut f, mut a) = (0usize, 0usize);
        while let Some((nf, na)) = step_across_sections(f, a, &lens, true) {
            seen.push((nf, na));
            f = nf;
            a = na;
        }
        assert_eq!(
            seen,
            vec![(0, 0), (0, 1), (1, 0), (2, 0), (2, 1), (2, 2)],
            "walking down did not visit every row once, in order"
        );
        let mut back = vec![(f, a)];
        while let Some((nf, na)) = step_across_sections(f, a, &lens, false) {
            back.push((nf, na));
            f = nf;
            a = na;
        }
        back.reverse();
        assert_eq!(back, seen, "walking back up did not retrace the same path");
    }

    #[test]
    fn empty_sections_are_stepped_over_in_both_directions() {
        assert_eq!(step_across_sections(0, 2, &[3, 0, 4], true), Some((2, 0)));
        assert_eq!(step_across_sections(2, 0, &[3, 0, 4], false), Some((0, 2)));
        assert_eq!(
            step_across_sections(0, 2, &[3, 0, 0], true),
            None,
            "nothing below but empties"
        );
    }

    #[test]
    fn a_section_that_empties_under_the_cursor_escapes() {
        // Sections are rebuilt every frame and can shrink or vanish.
        assert_eq!(step_across_sections(1, 0, &[3, 0, 4], true), Some((2, 0)));
        assert_eq!(step_across_sections(1, 0, &[3, 0, 4], false), Some((0, 2)));
        assert_eq!(step_across_sections(0, 7, &[3, 2, 0], false), None);
        assert_eq!(step_across_sections(0, 7, &[3, 2, 0], true), Some((1, 0)));
    }

    /// One more hint costs at most one more line.
    ///
    /// netwatch's detail screen leans on this: it sizes the body against the
    /// footer, then adds the scroll position as a seventh hint and re-packs.
    /// It reserves exactly one line for that. If a hint could ever push the
    /// footer two lines further, the body would lose a row the position had
    /// already counted and the label would name a row nobody can see.
    ///
    /// It holds because the packing is greedy and in order: whatever comes
    /// before a hint packs the same way whether that hint follows or not, so
    /// the new one either joins the last line or starts a single new one.
    #[test]
    fn appending_a_hint_adds_at_most_one_line() {
        let hint = |t: &str| vec![("", t.to_string())];
        let base = vec![
            hint("↑↓ select"),
            hint("tab next section"),
            hint("[c]opy"),
            hint("[r]ezero"),
            hint("←/esc back"),
            hint("[q]uit"),
        ];
        for width in 8..=120usize {
            let before = pack_hints(&base, width, "  ").len();
            let mut after_hints = base.clone();
            after_hints.push(hint("rows   1- 43 of  45"));
            let after = pack_hints(&after_hints, width, "  ").len();
            assert!(
                after == before || after == before + 1,
                "width {}: {} lines became {}",
                width,
                before,
                after
            );
            // And the hints that were already there are untouched.
            assert_eq!(
                pack_hints(&base, width, "  ")[..before.saturating_sub(1)],
                pack_hints(&after_hints, width, "  ")[..before.saturating_sub(1)],
                "width {}: an earlier line was repacked",
                width
            );
        }
    }
    use super::*;

    #[test]
    fn a_file_without_the_executable_bit_is_still_missing() {
        // shutil.which, which the Python uses, requires the bit. Checking
        // only is_file() would let a readable non-executable of the right
        // name count as the tool, so the widget would start and then fail
        // on every call rather than saying up front what it needs.
        let dir = std::env::temp_dir().join(format!("toys-missing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("definitely-not-a-real-tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();

        // PATH is process-wide and cargo runs a binary's tests in parallel,
        // so this is held for as long as the value is borrowed. `missing()`
        // reads PATH, and a second test calling it mid-swap would see a
        // directory holding one fake tool - failing for a reason that has
        // nothing to do with what it was testing, and only sometimes.
        static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let held = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", dir.to_string_lossy().to_string());

        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
        let not_executable = missing(&["definitely-not-a-real-tool"]);

        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let executable = missing(&["definitely-not-a-real-tool"]);

        std::env::set_var("PATH", held);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(not_executable, vec!["definitely-not-a-real-tool"], "0644 counted as present");
        assert!(executable.is_empty(), "0755 was not found: {:?}", executable);
    }


    #[test]
    fn a_query_survives_its_own_newlines() {
        // A GraphQL query is several lines. Unescaped, curl's config parser
        // would read the second one as another option.
        assert_eq!(quoted("query {\n  issues\n}"), "\"query {\\n  issues\\n}\"");
        assert_eq!(quoted("a\tb"), "\"a\\tb\"");
    }

    #[test]
    fn the_headers_that_count_are_the_ones_next_to_the_body() {
        // A redirect leaves its own block in front. The response is the one
        // nearest the body, and everything before it is history.
        let text = "HTTP/2 301\r\nlocation: /x\r\n\r\n\
                    HTTP/2 200\r\nX-RateLimit-Requests-Remaining: 2491\r\n\r\n\
                    {\"data\": 1}";
        let (head, body) = split_response(text);
        assert_eq!(head[0], "HTTP/2 200");
        assert_eq!(body, "{\"data\": 1}");
        // A body containing a blank line of its own is not mistaken for a
        // header block, because a block has to open with HTTP/.
        let plain = "HTTP/2 200\r\n\r\nline\n\nline";
        let (head, body) = split_response(plain);
        assert_eq!(head[0], "HTTP/2 200");
        assert_eq!(body, "line\n\nline");
    }

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
    fn a_stacked_bar_is_exactly_its_width() {
        let hue = |n: u8| rgb(n, n, n);
        // Thirds do not divide ten, and the last segment absorbs the
        // rounding rather than leaving a gap at the end.
        let parts = vec![
            (1.0 / 3.0, hue(1)),
            (1.0 / 3.0, hue(2)),
            (1.0 / 3.0, hue(3)),
        ];
        let drawn: usize = stacked_bar(&parts, 10)
            .iter()
            .map(|(_, t)| t.chars().count())
            .sum();
        assert_eq!(drawn, 10);
        // A part that rounds to nothing takes no segment at all, rather
        // than an empty one.
        let tiny = vec![(0.001, hue(1)), (0.999, hue(2))];
        assert_eq!(stacked_bar(&tiny, 10).len(), 1);
    }

    #[test]
    fn a_meter_fills_and_clamps() {
        assert_eq!(meter(0.0, 4), "░░░░");
        assert_eq!(meter(0.5, 4), "██░░");
        assert_eq!(meter(1.0, 4), "████");
        // Over and under are clamped: a percentage above 100 must not
        // draw a bar wider than its track.
        assert_eq!(meter(2.0, 4), "████");
        assert_eq!(meter(-1.0, 4), "░░░░");
    }

    #[test]
    fn bars_resolve_eighths_of_a_row() {
        let hue = rgb(0, 0, 0);
        let columns = vec![(1.0, hue.clone()), (0.5, hue.clone()), (0.0, hue.clone())];
        let rows = vbars(&columns, 1, 1.0);
        assert_eq!(rows[0][0].1, "█");
        assert_eq!(rows[0][1].1, "▄");
        assert_eq!(rows[0][2].1, " ");
        // Hanging downward there is only one partial glyph, so half a cell
        // rounds to it and less than half to nothing.
        let down = vbars_down(&columns, 1, 1.0);
        assert_eq!(down[0][0].1, "█");
        assert_eq!(down[0][1].1, "▀");
        assert_eq!(down[0][2].1, " ");
    }

    #[test]
    fn the_placeholder_moves_but_never_leaves_the_track() {
        // Every column stays inside the range a bar chart can draw, at
        // every tick - a value outside it would render as an empty cell
        // and read as data rather than as waiting.
        for tick in 0..40 {
            for value in dance(12, tick, 0.0) {
                assert!((0.08..=1.0).contains(&value), "{} at tick {}", value, tick);
            }
        }
        // Deterministic, so a frame can be reproduced.
        assert_eq!(dance(4, 7, 0.0), dance(4, 7, 0.0));
        assert_ne!(dance(4, 7, 0.0), dance(4, 8, 0.0));
    }

    #[test]
    fn the_shimmer_covers_its_width_and_moves() {
        let drawn = |tick: usize| -> String {
            skeleton(20, tick, 7).iter().map(|(_, t)| t.clone()).collect()
        };
        // Always exactly its width, whatever the phase: the row it stands
        // in for has a fixed size.
        for tick in 0..40 {
            assert_eq!(drawn(tick).chars().count(), 20, "at tick {}", tick);
        }
        // And the highlight actually travels, or it is just a grey bar.
        let runs = |tick: usize| skeleton(20, tick, 7).len();
        assert!((0..40).map(runs).collect::<std::collections::HashSet<_>>().len() > 1);
    }

    #[test]
    fn spread_fills_its_room_exactly() {
        // Twenty-eight days across fifty-nine columns: the three left over
        // go to the leftmost bars rather than leaving the chart short.
        let widths = spread(28, 59);
        assert_eq!(widths.len(), 28);
        assert_eq!(widths.iter().sum::<usize>(), 59);
        assert_eq!(widths[0], 3);
        assert_eq!(widths[27], 2);
        // More bars than columns is one cell each, and the caller decides
        // what to drop - returning fewer widths would lose data silently.
        assert_eq!(spread(10, 4), vec![1; 10]);
        assert!(spread(0, 10).is_empty());
    }

    #[test]
    fn a_blend_reaches_both_ends() {
        assert_eq!(mix((0, 0, 0), (10, 20, 30), 0.0), rgb(0, 0, 0));
        assert_eq!(mix((0, 0, 0), (10, 20, 30), 1.0), rgb(10, 20, 30));
        assert_eq!(mix((0, 0, 0), (10, 20, 30), 0.5), rgb(5, 10, 15));
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
            paths.iter().any(|p| p.to_string_lossy().contains(".config/opscope")),
            "the xdg location must stay, for an installed binary"
        );
    }

    #[test]
    fn the_config_search_still_finds_the_old_name() {
        // When this project was renamed from terminal-toys to opscope, the
        // bulk rename replaced the very strings that were added to keep the
        // old location working - so the fallback was written, renamed away,
        // and would have shipped looking correct. An install whose config
        // stops being found does not report an error: every widget draws an
        // empty pane, which is what a source with no data looks like.
        //
        // Both names are asserted here so the next rename cannot repeat it.
        let paths = config_paths();
        let all = paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            all.contains(".config/terminal-toys/config.json"),
            "the pre-rename config directory was dropped: {:?}",
            paths
        );
        let new_at = all.find(".config/opscope/").expect("new path present");
        let old_at = all
            .find(".config/terminal-toys/")
            .expect("old path present");
        assert!(
            new_at < old_at,
            "the current name must win where both files exist"
        );
    }

    #[test]
    fn the_old_config_env_var_still_works() {
        // Same reasoning as above, for the environment variable. Set both
        // and the new one wins; set only the old one and it is still read.
        assert_eq!(LEGACY_ENV, "TERMINAL_TOYS_CONFIG");
        assert_eq!(LEGACY_DIR, "terminal-toys");
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

    /// One poll's worth of input, decoded from a fresh keyboard.
    fn keys(text: &str) -> Vec<String> {
        let mut buf = text.to_string();
        let mut held = false;
        decode(&mut buf, &mut held)
    }

    #[test]
    fn a_setting_that_is_not_a_choice_starts_from_the_first() {
        let choices = [0.2f64, 0.5, 1.0, 2.0];
        assert_eq!(cycle(&choices, 0.2), 0.5);
        assert_eq!(cycle(&choices, 2.0), 0.2, "the last wraps to the first");
        // A value from config or an argument that is not on the list. This
        // returned 0.5 - the second - so the first choice could never be
        // reached by pressing the key.
        assert_eq!(cycle(&choices, 3.3), 0.2);
    }

    #[test]
    fn arrows_decode_to_names() {
        assert_eq!(keys("\x1b[A"), vec!["up"]);
        assert_eq!(keys("\x1b[B\x1b[B"), vec!["down", "down"]);
        assert_eq!(keys("q"), vec!["q"]);
        // Both encodings of Home and End, since terminals disagree.
        assert_eq!(keys("\x1b[H"), vec!["home"]);
        assert_eq!(keys("\x1b[1~"), vec!["home"]);
        assert_eq!(keys("\x1b[F"), vec!["end"]);
        assert_eq!(keys("\x1b[4~"), vec!["end"]);
        assert_eq!(keys("\r"), vec!["enter"]);
    }

    #[test]
    fn a_key_this_program_does_not_use_is_dropped_whole() {
        // Each of these used to arrive as "esc" plus its own bytes as
        // separate keys, and those bytes are live bindings elsewhere: "0"
        // resets the pomodoro count in clocks, "1" and "2" reorder
        // netwatch. Pressing F9 must do nothing, not reset a counter.
        for seq in [
            "\x1b[3~",    // Delete
            "\x1b[20~",   // F9
            "\x1b[15~",   // F5
            "\x1bOP",     // F1
            "\x1b[I",     // focus in
            "\x1b[O",     // focus out
            "\x1b[200~",  // bracketed paste begins
            "\x1b[Z",     // shift-tab
            "\x1b[1;5C",  // ctrl-right
        ] {
            assert_eq!(keys(seq), Vec::<String>::new(), "{:?} leaked a key", seq);
        }
        // And it does not swallow what follows it.
        assert_eq!(keys("\x1b[3~q"), vec!["q"]);
    }

    #[test]
    fn a_bare_escape_waits_one_poll_before_it_counts() {
        // ESC alone is indistinguishable from the first byte of a sequence
        // still arriving, so it is held. Two polls with nothing following
        // make it Escape; a poll that completes an arrow makes it an arrow.
        let mut buf = "\x1b".to_string();
        let mut held = false;
        assert_eq!(decode(&mut buf, &mut held), Vec::<String>::new());
        assert!(held);
        assert_eq!(decode(&mut buf, &mut held), vec!["esc"]);
        assert!(buf.is_empty());

        // The torn arrow: ESC in one read, "[A" in the next.
        let mut buf = "\x1b".to_string();
        let mut held = false;
        assert_eq!(decode(&mut buf, &mut held), Vec::<String>::new());
        buf.push_str("[A");
        assert_eq!(decode(&mut buf, &mut held), vec!["up"]);
        assert!(buf.is_empty());
    }

    #[test]
    fn an_incomplete_sequence_stays_in_the_buffer() {
        // Half of Home arrives; nothing is emitted and the half is kept,
        // rather than being spent as "esc" and a bracket.
        let mut buf = "\x1b[1".to_string();
        let mut held = false;
        assert_eq!(decode(&mut buf, &mut held), Vec::<String>::new());
        assert_eq!(buf, "\x1b[1");
        buf.push('~');
        assert_eq!(decode(&mut buf, &mut held), vec!["home"]);
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

    /// A list that is there and empty said something; a list that is not
    /// there did not. The first version read both as "unset", so
    /// `"hosts": []` had latency pinging the two resolvers its config had
    /// just declined.
    #[test]
    fn an_empty_list_is_an_answer_and_a_missing_one_is_not() {
        let fallback = ["1.1.1.1", "8.8.8.8"];
        let said_none = serde_json::json!({ "hosts": [] });
        assert!(
            cfg_strings(&said_none, "hosts", &fallback).is_empty(),
            "an empty list is the answer, not the absence of one"
        );
        let silent = serde_json::json!({ "window": 600 });
        assert_eq!(
            cfg_strings(&silent, "hosts", &fallback),
            vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()],
            "nobody said, so the default is a kindness"
        );
        // Not a list at all is not an answer either.
        let wrong = serde_json::json!({ "hosts": "1.1.1.1" });
        assert_eq!(cfg_strings(&wrong, "hosts", &fallback).len(), 2);
        let given = serde_json::json!({ "hosts": ["a.example", "b.example"] });
        assert_eq!(
            cfg_strings(&given, "hosts", &fallback),
            vec!["a.example".to_string(), "b.example".to_string()]
        );
    }

    #[test]
    fn a_broken_refresh_does_not_reach_from_secs_f64() {
        // Duration::from_secs_f64 panics on anything that is not finite
        // and positive. A config of -1, or `nan` from a mistyped `-n`,
        // used to take the poller down after the first read.
        assert_eq!(poll_secs(-1.0, 120.0), 120.0);
        assert_eq!(poll_secs(f64::NAN, 30.0), 30.0);
        assert_eq!(poll_secs(f64::INFINITY, 4.0), 4.0);
        assert_eq!(poll_secs(0.0, 2.0), 2.0);
        assert_eq!(poll_secs(15.0, 120.0), 15.0);
        // A broken fallback still has to be a duration.
        assert_eq!(poll_secs(-1.0, f64::NAN), 1.0);
        // And the bytes the handler writes are the same four sequences
        // restore_screen formats - so a drift here is a drift on Ctrl-C.
        assert_eq!(
            format!("{}{}{}{}", SHOW, RST, CLEAR, HOME),
            SCREEN_RESTORE
        );
    }

    /// The status names that a request was refused; only the body names
    /// what to do about it.
    #[test]
    fn a_refused_post_carries_what_the_api_said() {
        let said = refused(401, "{\"message\":\"Bad credentials\"}");
        assert!(said.contains("401"), "{:?}", said);
        assert!(said.contains("Bad credentials"), "{:?}", said);
        // A body over several lines still arrives as one row.
        let wrapped = refused(403, "{\n  \"message\": \"Resource not accessible\"\n}");
        assert!(!wrapped.contains('\n'), "{:?}", wrapped);
        assert!(wrapped.contains("Resource not accessible"), "{:?}", wrapped);
        // An error page cannot spend the whole pane.
        let long = refused(502, &"x".repeat(4000));
        assert!(long.chars().count() < 240, "{} characters", long.chars().count());
        // And a server that says nothing still says which refusal it was.
        assert_eq!(refused(500, "   \n"), "HTTP 500");
    }

    /// stderr is where a command says why it refused. It was sent to
    /// /dev/null, so `run` and `ports`'s `refusal` - both of which read it -
    /// had nothing to read, and a permission or login failure arrived as an
    /// exit status.
    ///
    /// `/bin/sh` by absolute path, not `sh`. A release build failed on a
    /// macOS runner with `sh: No such file or directory` - the shell was
    /// there, the PATH the job inherited was not what it should have been.
    /// The behaviour under test has nothing to do with PATH lookup, and a
    /// test that can fail for a reason it is not testing blocks releases
    /// and teaches people to re-run until it is green. `/bin/sh` is
    /// guaranteed on both platforms this ships to.
    #[test]
    fn a_command_that_failed_hands_back_what_it_complained() {
        const SH: &str = "/bin/sh";
        let out = run_full(&[SH, "-c", "echo 'needs an operator' >&2; exit 3"], 5)
            .expect("sh ran");
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("needs an operator"),
            "stderr was {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let why = run(&[SH, "-c", "echo 'needs an operator' >&2; exit 3"], 5)
            .expect_err("exit 3 is an error");
        assert!(why.contains("needs an operator"), "{:?}", why);
        // Capturing stderr must not cost stdout on the way past.
        assert_eq!(run(&[SH, "-c", "echo fine"], 5).unwrap().trim(), "fine");
    }
}
