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
        return Err(format!("HTTP {}", status));
    }
    Ok((body, found))
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
    let at = choices.iter().position(|c| *c == current).unwrap_or(0);
    choices[(at + 1) % choices.len()]
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
