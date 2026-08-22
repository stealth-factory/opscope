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

//! Multi-target latency monitor.
//!
//! A port of latency.py. One ping per target, read line by line as it
//! arrives, so the numbers are what ping measured rather than anything this
//! timed itself.

use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use toys_core as tc;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Clone, Default)]
struct Target {
    label: String,
    ip: String,
    samples: Vec<(f64, Option<f64>)>, // (when, rtt or a loss)
    down_since: Option<f64>,
    /// Whether the last reading was an answer, for the dot beside the name.
    alive: bool,
    /// The live ping, so a new interval can be applied without waiting for
    /// the old one to notice.
    pid: Option<i32>,
    /// Set while we are killing our own ping on purpose, so its exit is not
    /// logged as an outage.
    restarting: bool,
}

/// Everything the table says about one target over the retained window.
#[derive(Default)]
struct Stats {
    now: Option<f64>,
    avg: Option<f64>,
    med: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
    jit: Option<f64>,
    loss: f64,
    n: usize,
}

impl Target {
    /// Round trips that arrived, newest last.
    fn rtts(&self) -> Vec<f64> {
        self.samples.iter().filter_map(|(_, r)| *r).collect()
    }

    fn stats(&self) -> Stats {
        let got = self.rtts();
        let total = self.samples.len();
        let lost = total - got.len();
        let loss = if total > 0 {
            100.0 * lost as f64 / total as f64
        } else {
            0.0
        };
        if got.is_empty() {
            return Stats {
                loss: if total > 0 { 100.0 } else { 0.0 },
                n: total,
                ..Default::default()
            };
        }
        let mut ordered = got.clone();
        ordered.sort_by(f64::total_cmp);
        // The mean gap between one reply and the next, which is what jitter
        // means on a link: how much the round trip moves from ping to ping,
        // not how far it sits from its own average.
        let jit = if got.len() > 1 {
            Some(
                got.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>()
                    / (got.len() - 1) as f64,
            )
        } else {
            Some(0.0)
        };
        Stats {
            now: self.samples.last().and_then(|(_, r)| *r),
            avg: Some(got.iter().sum::<f64>() / got.len() as f64),
            med: Some(ordered[ordered.len() / 2]),
            min: Some(ordered[0]),
            max: Some(ordered[ordered.len() - 1]),
            jit,
            loss,
            n: total,
        }
    }
}

/// A round trip in a fixed seven cells, so the columns cannot shift.
///
/// Below a millisecond it changes unit rather than losing the value: a
/// loopback reply reads 0.21ms as 210µs, and two decimal places of a
/// millisecond hides what that means.
fn fmt_ms(value: Option<f64>) -> String {
    match value {
        None => "   --  ".to_string(),
        Some(v) if v < 1.0 => format!("{:>5.0}µs", v * 1000.0),
        Some(v) if v < 100.0 => format!("{:>5.2}ms", v),
        Some(v) => format!("{:>5.1}ms", v),
    }
}

const SPARK: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// One target's recent history at one character per ping.
///
/// Scaled to its own range rather than the chart's, so a target that never
/// leaves a two-millisecond band still shows the shape of its variation -
/// which is the question this line answers and the shared chart does not.
fn sparkline(samples: &[(f64, Option<f64>)], n: usize, p: &Palette) -> Vec<(String, String)> {
    let window = &samples[samples.len().saturating_sub(n)..];
    let got: Vec<f64> = window.iter().filter_map(|(_, r)| *r).collect();
    if got.is_empty() {
        return vec![(p.bad.clone(), "×".repeat(window.len().min(n)))];
    }
    let lo = got.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = got.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = if hi > lo { hi - lo } else { 1.0 };
    let mut out: Vec<(String, String)> = Vec::new();
    for (_, r) in window {
        let (colour, glyph) = match r {
            None => (p.bad.clone(), '×'),
            Some(v) => {
                let frac = (v - lo) / span;
                let colour = if frac < 0.5 {
                    &p.ok
                } else if frac < 0.85 {
                    &p.warn
                } else {
                    &p.bad
                };
                (colour.clone(), SPARK[((frac * 7.99) as usize).min(7)])
            }
        };
        match out.last_mut() {
            Some((was, text)) if *was == colour => text.push(glyph),
            _ => out.push((colour, glyph.to_string())),
        }
    }
    out
}

/// A loss, a recovery or a spike, with the time it happened.
#[derive(Clone)]
struct Event {
    at: String,
    hue: String,
    host: String,
    kind: &'static str,
    detail: String,
}

/// How samples sharing one graph column combine.
///
/// Median by default: latency is right-skewed, so a single spike inside a
/// bucket would drag a mean well above the latency actually experienced
/// most of the time.
fn aggregate(values: &[f64], how: &str) -> f64 {
    let mut ordered = values.to_vec();
    ordered.sort_by(f64::total_cmp);
    let n = ordered.len();
    if n == 1 {
        return ordered[0];
    }
    match how {
        "mean" => ordered.iter().sum::<f64>() / n as f64,
        "min" => ordered[0],
        "max" => ordered[n - 1],
        "p95" => ordered[(n - 1).min((n as f64 * 0.95) as usize)],
        _ if n % 2 == 1 => ordered[n / 2],
        _ => (ordered[n / 2 - 1] + ordered[n / 2]) / 2.0,
    }
}

const AGGREGATORS: &[&str] = &["median", "mean", "min", "max", "p95"];
const INTERVAL_CHOICES: &[f64] = &[0.2, 0.5, 1.0, 2.0, 5.0];
const COLUMN_CHOICES: &[f64] = &[0.0, 2.0, 5.0, 10.0];

/// The next entry after `current`, wrapping. Used by the cycling keys.
fn cycle<T: PartialEq + Copy>(choices: &[T], current: T) -> T {
    let at = choices.iter().position(|c| *c == current).unwrap_or(0);
    choices[(at + 1) % choices.len()]
}

/// The round trip out of one ping reply line.
///
/// Both shapes ping writes are read - `time=12.3 ms` and `time=12.3ms` -
/// and anything else on the line is left alone rather than guessed at.
fn rtt_of(line: &str) -> Option<f64> {
    let at = line.find("time=")? + 5;
    let rest = &line[at..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn is_loss(line: &str) -> bool {
    line.contains("Unreachable") || line.contains("no answer") || line.contains("Time to live")
}

/// The address ping resolved the host to, from its first line.
fn ip_of(line: &str) -> Option<String> {
    let open = line.find('(')?;
    let close = line[open..].find(')')? + open;
    let inside = &line[open + 1..close];
    if inside.chars().any(|c| c.is_ascii_digit()) {
        Some(inside.to_string())
    } else {
        None
    }
}

/// What the cycling keys change, shared with the reader threads.
///
/// The interval lives here rather than being handed to each thread once,
/// because pressing i has to reach pings that are already running.
#[derive(Default)]
struct Settings {
    interval: f64,
    seconds_per_column: f64,
    aggregate: String,
    spike_factor: f64,
}

/// Keep one ping running per target, forever.
///
/// Wrapped in its own thread per target and never allowed to end: if ping
/// exits - a name that stopped resolving, a network that went away - the
/// row would otherwise just stop updating, which reads as a quiet link
/// rather than as a broken widget.
fn watch(
    host: String,
    index: usize,
    window: usize,
    shared: Arc<Mutex<Vec<Target>>>,
    settings: Arc<Mutex<Settings>>,
    events: Arc<Mutex<Vec<Event>>>,
    hue: String,
    label: String,
) {
    loop {
        let (interval, spike_factor) = match settings.lock() {
            Ok(s) => (s.interval, s.spike_factor),
            Err(_) => return,
        };
        let child = std::process::Command::new("ping")
            .args(["-n", "-O", "-i", &interval.to_string(), &host])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(_) => {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        if let Ok(mut guard) = shared.lock() {
            guard[index].pid = Some(child.id() as i32);
        }
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => continue,
        };
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let stamp = now();
            let mut guard = match shared.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            let target = &mut guard[index];
            if target.ip.is_empty() {
                if let Some(ip) = ip_of(&line) {
                    target.ip = ip;
                }
            }
            if let Some(rtt) = rtt_of(&line) {
                // A spike is worth a line in the log because the median in
                // the table will not move for it, and a link that is fine
                // except once a minute is a different problem from one that
                // is slow.
                let st = target.stats();
                if let Some(med) = st.med {
                    if st.n > 10 && rtt > med * spike_factor {
                        log(&events, &hue, &label, "SPIKE",
                            format!("{} (median {})", fmt_ms(Some(rtt)).trim(),
                                    fmt_ms(Some(med)).trim()));
                    }
                }
                if let Some(since) = target.down_since.take() {
                    log(&events, &hue, &label, "UP",
                        format!("recovered after {:.0}s", stamp - since));
                }
                target.alive = true;
                target.samples.push((stamp, Some(rtt)));
            } else if is_loss(&line) {
                if target.down_since.is_none() {
                    target.down_since = Some(stamp);
                    log(&events, &hue, &label, "LOSS", "no reply".into());
                }
                target.alive = false;
                target.samples.push((stamp, None));
            }
            if target.samples.len() > window {
                let drop = target.samples.len() - window;
                target.samples.drain(..drop);
            }
        }
        let _ = child.wait();
        let ours = match shared.lock() {
            Ok(mut guard) => {
                guard[index].pid = None;
                let deliberate = guard[index].restarting;
                guard[index].restarting = false;
                deliberate
            }
            Err(_) => return,
        };
        // We killed it ourselves to apply a new interval; not an outage, and
        // it starts again immediately rather than after the retry pause.
        if ours {
            continue;
        }
        if let Ok(mut guard) = shared.lock() {
            let target = &mut guard[index];
            if target.down_since.is_none() {
                target.down_since = Some(now());
                drop(guard);
                log(&events, &hue, &label, "DOWN", "ping exited, retrying".into());
            } else {
                drop(guard);
            }
        }
        if let Ok(mut guard) = shared.lock() {
            guard[index].alive = false;
            guard[index].samples.push((now(), None));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn log(events: &Arc<Mutex<Vec<Event>>>, hue: &str, host: &str, kind: &'static str, detail: String) {
    if let Ok(mut guard) = events.lock() {
        guard.push(Event {
            at: clock_time(),
            hue: hue.to_string(),
            host: host.to_string(),
            kind,
            detail,
        });
        let most = 40;
        if guard.len() > most {
            let drop = guard.len() - most;
            guard.drain(..drop);
        }
    }
}

/// The time of day, as the machine reckons it.
///
/// Local rather than UTC: this sits on a wall beside a clock panel showing
/// server time, and a header eight hours out from the pane next to it is
/// read as a broken widget rather than as a different timezone.
fn clock_time() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Restart every ping so a new interval takes effect at once.
///
/// SIGTERM rather than waiting for the current one to end: at five seconds
/// a change would otherwise take five seconds to become visible, which
/// reads as the key not having worked.
fn apply_interval(shared: &Arc<Mutex<Vec<Target>>>) {
    if let Ok(mut guard) = shared.lock() {
        for target in guard.iter_mut() {
            if let Some(pid) = target.pid {
                target.restarting = true;
                if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
                    target.restarting = false;
                }
            }
        }
    }
}

/// A braille cell is two dots wide and four tall, so one character holds
/// eight addressable points. The bit for each is fixed by the encoding.
const BRAILLE: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// Plot one series on a dot canvas eight times finer than the cells.
///
/// Consecutive samples are joined rather than left as marks, which is the
/// difference between a line that reads as a round trip moving and one that
/// reads as specks a row apart. The masks come back per cell instead of as
/// text so that several series can be laid over one another first.
fn braille_canvas(
    values: &[Option<f64>],
    llo: f64,
    lhi: f64,
    cols: usize,
    rows: usize,
) -> Vec<Vec<u8>> {
    let (px_w, px_h) = (cols * 2, rows * 4);
    let mut grid = vec![vec![0u8; cols]; rows];
    if values.is_empty() || px_w == 0 || px_h == 0 {
        return grid;
    }
    let vals: Vec<Option<f64>> = values.iter().rev().take(px_w).rev().copied().collect();
    // Newest against the right edge: a target that has answered five times
    // shows five samples there, not five stretched across the whole width.
    let left = px_w - vals.len();
    let decade = (lhi - llo).max(1e-9);
    let point = |i: usize| -> Option<(i64, i64)> {
        let v = vals[i]?;
        let frac = ((v.max(1e-3).log10() - llo) / decade).clamp(0.0, 1.0);
        Some((
            (left + i) as i64,
            ((1.0 - frac) * (px_h as f64 - 1.0)).round() as i64,
        ))
    };
    let dot = |x: i64, y: i64, grid: &mut Vec<Vec<u8>>| {
        if x >= 0 && (x as usize) < px_w && y >= 0 && (y as usize) < px_h {
            grid[y as usize / 4][x as usize / 2] |= BRAILLE[y as usize % 4][x as usize % 2];
        }
    };
    // A single reading is a measurement and gets its dot: unlike netwatch's
    // idle zero, there is no value here that means "nothing happened".
    if let Some((x, y)) = point(0) {
        dot(x, y, &mut grid);
    }
    for i in 1..vals.len() {
        // A column with no reply is a gap, and a gap is not drawn through.
        // Joining across one would draw a line where the link was down,
        // which is the opposite of what happened.
        let (Some((mut x0, mut y0)), Some((x1, y1))) = (point(i - 1), point(i)) else {
            if let Some((x, y)) = point(i) {
                dot(x, y, &mut grid);
            }
            continue;
        };
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            dot(x0, y0, &mut grid);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let twice = 2 * err;
            if twice >= dy {
                err += dy;
                x0 += sx;
            }
            if twice <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }
    grid
}

/// Lay the canvases over one another, cell by cell.
///
/// The dots are merged so that no sample is lost where two targets cross.
/// A cell can carry only one colour, and it goes to whichever series comes
/// later in the table above: which trace is hidden is then something the
/// reader can work out from that list rather than something the data decides
/// afresh every frame.
fn overlay(layers: &[(String, Vec<Vec<u8>>)], cols: usize, rows: usize) -> Vec<Vec<(String, u8)>> {
    let mut cells = vec![vec![(String::new(), 0u8); cols]; rows];
    for (colour, canvas) in layers {
        for (y, line) in canvas.iter().enumerate().take(rows) {
            for (x, mask) in line.iter().enumerate().take(cols) {
                if *mask != 0 {
                    cells[y][x].0 = colour.clone();
                    cells[y][x].1 |= mask;
                }
            }
        }
    }
    cells
}

/// Log-scale plot of every target's round trip.
///
/// Log because the targets on one screen can differ by two orders of
/// magnitude, and a linear axis renders the near one as a flat line at the
/// bottom.
///
/// Columns are anchored to a fixed time grid rather than measured backwards
/// from now, so a sample never migrates between columns: the plot steps left
/// exactly once per bucket instead of shuffling as the clock slides.
fn graph(
    targets: &[Target],
    w: usize,
    h: usize,
    bucket: f64,
    how: &str,
    p: &Palette,
) -> (Vec<String>, f64) {
    let gw = w.saturating_sub(9).max(10);
    let gh = h.max(4);
    // Two dot columns to a cell, so the chart holds twice the buckets it
    // did when each one had a whole character to itself.
    let slots = gw * 2;
    let newest = (now() / bucket).floor();
    let series: Vec<(usize, Vec<Option<f64>>)> = targets
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut columns: Vec<Vec<f64>> = vec![Vec::new(); slots];
            for (at, rtt) in &t.samples {
                let Some(rtt) = rtt else { continue };
                let age = newest - (at / bucket).floor();
                if age < 0.0 || age >= slots as f64 {
                    continue;
                }
                columns[slots - 1 - age as usize].push(*rtt);
            }
            let values = columns
                .into_iter()
                .map(|c| if c.is_empty() { None } else { Some(aggregate(&c, how)) })
                .collect();
            (i, values)
        })
        .collect();
    let span = bucket * slots as f64;
    let seen: Vec<f64> = series
        .iter()
        .flat_map(|(_, v)| v.iter().flatten())
        .copied()
        .collect();
    if seen.is_empty() {
        return (
            vec![tc::seg(&[(p.dim.as_str(), "  collecting…".into())], w - 1)],
            span,
        );
    }
    let lo = seen.iter().cloned().fold(f64::INFINITY, f64::min).max(0.05) * 0.8;
    let hi = (seen.iter().cloned().fold(0.0f64, f64::max) * 1.25).max(lo * 1.6);
    let (llo, lhi) = (lo.log10(), hi.log10());

    // One canvas per target rather than one shared grid: the glyphs used to
    // tell the traces apart, and with braille the hue is all that is left to
    // do it with, so each series has to keep its own until the last moment.
    let layers: Vec<(String, Vec<Vec<u8>>)> = series
        .iter()
        .map(|(idx, values)| {
            (
                p.hues[idx % p.hues.len()].clone(),
                braille_canvas(values, llo, lhi, gw, gh),
            )
        })
        .collect();
    let cells = overlay(&layers, gw, gh);

    let mut out = Vec::new();
    for (y, line) in cells.iter().enumerate() {
        let frac = 1.0 - (y as f64 / (gh as f64 - 1.0).max(1.0));
        let value = 10f64.powf(llo + frac * (lhi - llo));
        // Label only the top, middle and bottom: a number on every row is a
        // table pretending to be an axis.
        let label = if y == 0 || y == gh / 2 || y == gh - 1 {
            fmt_ms(Some(value))
        } else {
            " ".repeat(7)
        };
        let mut parts: Vec<(&str, String)> =
            vec![(p.dim.as_str(), label), (p.grid.as_str(), "│".into())];
        for (colour, mask) in line {
            parts.push(match mask {
                0 => (p.grid.as_str(), " ".into()),
                m => (
                    colour.as_str(),
                    char::from_u32(0x2800 + *m as u32).unwrap_or(' ').to_string(),
                ),
            });
        }
        out.push(tc::seg(&parts, w - 1));
    }
    (out, span)
}

fn main() {
    tc::maybe_help(include_str!("latency_help.txt"));
    let cfg = tc::load_config("latency");
    let hosts = tc::cfg_strings(&cfg, "hosts", &["1.1.1.1", "8.8.8.8"]);
    let window = tc::cfg_usize(&cfg, "window", 600);
    let strip: Vec<String> = tc::cfg_strings(&cfg, "strip_suffixes", &[]);
    let mut live = Settings {
        interval: tc::cfg_f64(&cfg, "interval", 0.5),
        seconds_per_column: tc::cfg_f64(&cfg, "seconds_per_column", 0.0),
        aggregate: tc::cfg_str(&cfg, "aggregate", "median"),
        spike_factor: tc::cfg_f64(&cfg, "spike_factor", 3.0),
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut named: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-i" | "--interval" if i + 1 < args.len() => {
                live.interval = args[i + 1].parse::<f64>().unwrap_or(0.5).max(0.2);
                i += 2;
            }
            "-c" | "--column-seconds" if i + 1 < args.len() => {
                live.seconds_per_column = args[i + 1].parse::<f64>().unwrap_or(0.0).max(0.0);
                i += 2;
            }
            "-g" | "--group" if i + 1 < args.len() => {
                if !AGGREGATORS.contains(&args[i + 1].as_str()) {
                    eprintln!("-g must be one of: {}", AGGREGATORS.join(", "));
                    std::process::exit(2);
                }
                live.aggregate = args[i + 1].clone();
                i += 2;
            }
            other if !other.starts_with('-') => {
                named.push(other.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }
    let hosts = if named.is_empty() { hosts } else { named };

    let absent = tc::missing(&["ping"]);
    if !absent.is_empty() {
        cannot_start(&absent);
        return;
    }

    let p = palette();
    let targets: Vec<Target> = hosts
        .iter()
        .map(|h| Target {
            label: label_for(h, &strip),
            ..Default::default()
        })
        .collect();
    let labels: Vec<String> = targets.iter().map(|t| t.label.clone()).collect();
    let shared = Arc::new(Mutex::new(targets));
    let settings = Arc::new(Mutex::new(live));
    let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    for (index, host) in hosts.iter().enumerate() {
        let shared = Arc::clone(&shared);
        let settings = Arc::clone(&settings);
        let events = Arc::clone(&events);
        let host = host.clone();
        let hue = p.hues[index % p.hues.len()].clone();
        let label = labels[index].clone();
        std::thread::spawn(move || {
            watch(host, index, window, shared, settings, events, hue, label)
        });
    }

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    loop {
        for key in keyboard.poll() {
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "i" | "I" => {
                    if let Ok(mut s) = settings.lock() {
                        s.interval = cycle(INTERVAL_CHOICES, s.interval);
                    }
                    apply_interval(&shared);
                }
                "g" | "G" => {
                    if let Ok(mut s) = settings.lock() {
                        let at = AGGREGATORS
                            .iter()
                            .position(|a| *a == s.aggregate)
                            .unwrap_or(0);
                        s.aggregate = AGGREGATORS[(at + 1) % AGGREGATORS.len()].to_string();
                    }
                }
                "c" | "C" => {
                    if let Ok(mut s) = settings.lock() {
                        s.seconds_per_column = cycle(COLUMN_CHOICES, s.seconds_per_column);
                    }
                }
                _ => {}
            }
        }
        let (w, h) = tc::size();
        let snapshot: Vec<Target> = match shared.lock() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        let (interval, per_column, how) = match settings.lock() {
            Ok(s) => (s.interval, s.seconds_per_column, s.aggregate.clone()),
            Err(_) => return,
        };
        // Zero means one bucket per ping, which is the finest motion the
        // grid allows; anything larger trades that for a longer history.
        let bucket = if per_column > 0.0 { per_column } else { interval };

        let mut rows = vec![tc::title("network latency monitor", w, &p.head)];
        rows.push(tc::seg(
            &[
                (
                    p.dim.as_str(),
                    format!(" {} targets · {:.1}s interval · ", snapshot.len(), interval),
                ),
                (p.txt.as_str(), clock_time()),
                (
                    p.dim.as_str(),
                    if bucket <= interval {
                        " · 1 ping/column".to_string()
                    } else {
                        format!(" · {} of {}s blocks", how, bucket)
                    },
                ),
                (
                    p.grid.as_str(),
                    "   [i]nterval [g]roup [c]olumns [q]uit".into(),
                ),
            ],
            w - 1,
        ));
        rows.push(String::new());

        // The columns are dropped from the right as the pane narrows rather
        // than clipped, because half a number is worse than none.
        let wide = w >= 72;
        let show_med = w >= 80;
        let name_w = 22usize;
        rows.push(tc::seg(
            &[(
                p.lbl.as_str(),
                format!(
                    " {} {:>7} {:>7}{} {:>7} {:>7} {:>7} {:>6}",
                    tc::pad("HOST", name_w),
                    "NOW",
                    "AVG",
                    if show_med { "  MEDIAN" } else { "" },
                    "MIN",
                    "MAX",
                    "JITTER",
                    "LOSS"
                ),
            )],
            w - 1,
        ));
        for (i, t) in snapshot.iter().enumerate() {
            let st = t.stats();
            let hue = &p.hues[i % p.hues.len()];
            let loss_c = if st.loss == 0.0 {
                &p.ok
            } else if st.loss < 5.0 {
                &p.warn
            } else {
                &p.bad
            };
            rows.push(tc::seg(
                &[
                    // The dot rides in the colour rather than the text, so
                    // it costs no cell - which is how latency.py draws it,
                    // and the two have to line up column for column when
                    // they sit side by side.
                    (
                        &format!(
                            "{}{}",
                            if t.alive { &p.ok } else { &p.bad },
                            if t.alive { '●' } else { '○' }
                        ),
                        " ".to_string(),
                    ),
                    (hue.as_str(), tc::pad(&t.label, name_w)),
                    (p.txt.as_str(), format!(" {}", fmt_ms(st.now))),
                    (p.txt.as_str(), format!(" {}", fmt_ms(st.avg))),
                    (
                        p.ok.as_str(),
                        if show_med {
                            format!(" {}", fmt_ms(st.med))
                        } else {
                            String::new()
                        },
                    ),
                    (p.dim.as_str(), format!(" {}", fmt_ms(st.min))),
                    (p.dim.as_str(), format!(" {}", fmt_ms(st.max))),
                    (p.txt.as_str(), format!(" {}", fmt_ms(st.jit))),
                    (loss_c.as_str(), format!(" {:>5.1}%", st.loss)),
                ],
                w - 1,
            ));
            if wide && !t.samples.is_empty() {
                let mut line: Vec<(&str, String)> = vec![(p.dim.as_str(), "   ".into())];
                let spark = sparkline(&t.samples, w.saturating_sub(6), &p);
                for (colour, text) in &spark {
                    line.push((colour.as_str(), text.clone()));
                }
                rows.push(tc::seg(&line, w - 1));
            }
        }
        rows.push(String::new());

        // The log only earns its space on a tall pane: on a short one the
        // chart is the thing worth keeping.
        let log_h = if h.saturating_sub(rows.len()) > 20 { 7 } else { 0 };
        let gh = h.saturating_sub(rows.len() + log_h + 4).max(4);
        let (chart, span) = graph(&snapshot, w, gh, bucket, &how, &p);
        let drawn = chart.len();
        rows.extend(chart);
        if drawn > 1 {
            let gw = w.saturating_sub(9).max(10);
            rows.push(tc::seg(
                &[
                    (p.lbl.as_str(), " ".repeat(7)),
                    (p.grid.as_str(), format!("└{}", "─".repeat(gw))),
                ],
                w - 1,
            ));
            let ago = format!("{}s ago", span as i64);
            let ago = if ago.chars().count() + 4 > gw {
                String::new()
            } else {
                ago
            };
            rows.push(tc::seg(
                &[
                    (p.dim.as_str(), format!("{:8}{}", "", ago)),
                    (
                        p.dim.as_str(),
                        " ".repeat(gw.saturating_sub(ago.chars().count() + 3)),
                    ),
                    (p.dim.as_str(), "now".into()),
                ],
                w - 1,
            ));
        }
        rows.push(String::new());

        if log_h > 0 {
            rows.push(tc::seg(&[(p.dim.as_str(), " ── EVENTS ──".into())], w - 1));
            let recent: Vec<Event> = match events.lock() {
                Ok(g) => g.iter().rev().take(log_h - 1).rev().cloned().collect(),
                Err(_) => Vec::new(),
            };
            if recent.is_empty() {
                rows.push(tc::seg(
                    &[(p.dim.as_str(), "   (no loss or spikes recorded)".into())],
                    w - 1,
                ));
            }
            for event in &recent {
                let kind_c = match event.kind {
                    "LOSS" | "DOWN" => &p.bad,
                    "SPIKE" => &p.warn,
                    _ => &p.ok,
                };
                rows.push(tc::seg(
                    &[
                        (p.dim.as_str(), format!(" {} ", event.at)),
                        (kind_c.as_str(), format!("{:<6}", event.kind)),
                        (event.hue.as_str(), tc::pad(&event.host, 22)),
                        (p.dim.as_str(), event.detail.clone()),
                    ],
                    w - 1,
                ));
            }
        }

        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// A host as a person would say it, with the noise stripped off the end.
fn label_for(host: &str, strip: &[String]) -> String {
    let mut label = host.to_string();
    for suffix in strip {
        if let Some(base) = label.strip_suffix(suffix.as_str()) {
            label = base.to_string();
            break;
        }
    }
    label
}

/// Draw the reason and wait, rather than exiting.
fn cannot_start(needed: &[String]) {
    let bad = tc::rgb(255, 100, 110);
    let dim = tc::rgb(127, 147, 172);
    let txt = tc::rgb(225, 235, 245);
    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    loop {
        for key in keyboard.poll() {
            if key == "q" || key == "Q" {
                keyboard.restore();
                tc::restore_screen();
                return;
            }
        }
        let (w, h) = tc::size();
        let mut rows = vec![tc::title("latency", w, &bad), String::new()];
        rows.push(tc::seg(
            &[
                (bad.as_str(), " cannot start · ".into()),
                (txt.as_str(), format!("needs {}", needed.join(", "))),
            ],
            w - 1,
        ));
        rows.push(String::new());
        for line in [
            "Every figure here comes from ping: this widget times replies,",
            "it does not send packets itself. With no ping there is nothing",
            "to time and nothing to draw.",
        ] {
            rows.push(tc::seg(&[(dim.as_str(), format!(" {}", line))], w - 1));
        }
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (dim.as_str(), " try: ".into()),
                (txt.as_str(), "apt install iputils-ping".into()),
            ],
            w - 1,
        ));
        while rows.len() < h - 1 {
            rows.push(String::new());
        }
        rows.push(tc::seg(&[(dim.as_str(), " [q]uit".into())], w - 1));
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(200));
    }
}

struct Palette {
    ok: String,
    warn: String,
    bad: String,
    dim: String,
    grid: String,
    txt: String,
    lbl: String,
    head: String,
    hues: Vec<String>,
}

fn palette() -> Palette {
    Palette {
        ok: tc::rgb(110, 255, 170),
        warn: tc::rgb(255, 200, 90),
        bad: tc::rgb(255, 95, 105),
        dim: tc::rgb(70, 100, 120),
        grid: tc::rgb(38, 58, 74),
        txt: tc::rgb(215, 235, 250),
        lbl: tc::rgb(120, 170, 200),
        head: tc::rgb(90, 220, 255),
        // latency.py's own nine, not the six the other widgets share: the
        // traces are told apart by hue alone now that the glyphs are gone,
        // so more targets than six needs more than six colours.
        hues: vec![
            tc::rgb(90, 220, 255),
            tc::rgb(255, 170, 80),
            tc::rgb(140, 255, 160),
            tc::rgb(230, 140, 255),
            tc::rgb(255, 110, 130),
            tc::rgb(255, 230, 110),
            tc::rgb(120, 160, 255),
            tc::rgb(255, 140, 200),
            tc::rgb(150, 255, 240),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_gives_up_its_round_trip() {
        let line = "64 bytes from 1.1.1.1: icmp_seq=1 ttl=57 time=12.3 ms";
        assert_eq!(rtt_of(line), Some(12.3));
        // The other shape ping writes, without the space.
        assert_eq!(rtt_of("... time=0.45ms"), Some(0.45));
        assert_eq!(rtt_of("PING example (1.2.3.4) 56 bytes"), None);
    }

    #[test]
    fn losses_are_recognised_but_not_timed() {
        assert!(is_loss("From 10.0.0.1 icmp_seq=2 Destination Net Unreachable"));
        assert!(is_loss("no answer yet for icmp_seq=3"));
        assert!(!is_loss("64 bytes from 1.1.1.1: time=1 ms"));
    }

    #[test]
    fn the_resolved_address_is_taken_from_the_header() {
        assert_eq!(
            ip_of("PING one.one.one.one (1.1.1.1) 56(84) bytes of data."),
            Some("1.1.1.1".into())
        );
        assert_eq!(ip_of("64 bytes from host: time=1 ms"), None);
    }

    #[test]
    fn a_spike_belongs_in_max_and_not_in_the_middle() {
        let mut t = Target::default();
        // Nine steady samples and one wild spike. The median and the
        // typical round trip are unmoved by it; max is where it shows.
        for v in [10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 400.0] {
            t.samples.push((0.0, Some(v)));
        }
        let st = t.stats();
        assert_eq!(st.med, Some(10.0));
        assert_eq!(st.max, Some(400.0));
        assert_eq!(st.min, Some(10.0));
        assert_eq!(st.now, Some(400.0));
    }

    #[test]
    fn jitter_is_the_gap_between_one_reply_and_the_next() {
        let mut t = Target::default();
        // Ten and twenty alternating: every consecutive gap is 10ms, so
        // that is the jitter - even though every sample sits 5ms from the
        // mean, which is what a deviation would have reported.
        for v in [10.0, 20.0, 10.0, 20.0, 10.0] {
            t.samples.push((0.0, Some(v)));
        }
        assert_eq!(t.stats().jit, Some(10.0));
        // A steady link has none, and one sample cannot have any.
        let mut steady = Target::default();
        for _ in 0..4 {
            steady.samples.push((0.0, Some(30.0)));
        }
        assert_eq!(steady.stats().jit, Some(0.0));
    }

    #[test]
    fn loss_counts_the_unanswered() {
        let mut t = Target::default();
        t.samples.push((0.0, Some(10.0)));
        t.samples.push((0.0, None));
        t.samples.push((0.0, Some(12.0)));
        t.samples.push((0.0, None));
        assert_eq!(t.stats().loss, 50.0);
        // Nothing back at all is total loss, not an absent reading.
        let mut silent = Target::default();
        silent.samples.push((0.0, None));
        assert_eq!(silent.stats().loss, 100.0);
        assert_eq!(silent.stats().med, None);
    }

    #[test]
    fn milliseconds_keep_their_column_width() {
        // Seven cells whatever the value, or the columns shift under the
        // headings as a link speeds up.
        for value in [Some(123.4), Some(12.34), Some(1.234), Some(0.21), None] {
            assert_eq!(fmt_ms(value).chars().count(), 7, "{:?}", value);
        }
        assert_eq!(fmt_ms(Some(123.4)), "123.4ms");
        assert_eq!(fmt_ms(Some(12.34)), "12.34ms");
        // Below a millisecond it changes unit rather than losing the value.
        assert_eq!(fmt_ms(Some(0.21)), "  210µs");
        assert_eq!(fmt_ms(None), "   --  ");
    }

    #[test]
    fn a_bucket_keeps_the_typical_not_the_extreme() {
        let block = [10.0, 10.0, 10.0, 10.0, 400.0];
        // Median by default, because latency is right-skewed and one spike
        // in a bucket would drag a mean well above what the link felt like.
        assert_eq!(aggregate(&block, "median"), 10.0);
        assert_eq!(aggregate(&block, "min"), 10.0);
        assert_eq!(aggregate(&block, "max"), 400.0);
        assert_eq!(aggregate(&block, "mean"), 88.0);
        assert_eq!(aggregate(&block, "p95"), 400.0);
        // An even count takes the middle of the two middles.
        assert_eq!(aggregate(&[10.0, 20.0], "median"), 15.0);
        assert_eq!(aggregate(&[7.0], "mean"), 7.0);
    }

    #[test]
    fn the_cycling_keys_wrap() {
        assert_eq!(cycle(INTERVAL_CHOICES, 0.5), 1.0);
        assert_eq!(cycle(INTERVAL_CHOICES, 5.0), 0.2);
        // A value that is not one of the choices starts from the first.
        assert_eq!(cycle(INTERVAL_CHOICES, 3.3), 0.5);
        assert_eq!(cycle(COLUMN_CHOICES, 10.0), 0.0);
    }

    #[test]
    fn a_gap_in_the_data_is_not_drawn_through() {
        // Two readings with a lost bucket between them. Joining across it
        // would draw a line where the link was down.
        let values = [Some(10.0), None, Some(10.0)];
        let grid = braille_canvas(&values, 1.0, 2.0, 3, 2);
        let occupied: Vec<bool> = (0..6)
            .map(|x| grid.iter().any(|row| row[x / 2] & column_mask(x % 2) != 0))
            .collect();
        // Six dot columns for three cells, and three values, so they sit in
        // the last three: a reading, the lost bucket, a reading.
        assert!(occupied[3] && occupied[5], "the readings are missing");
        assert!(!occupied[4], "something was drawn across the gap");
        assert!(
            !occupied[..3].iter().any(|hit| *hit),
            "the empty left of the axis was painted"
        );
    }

    /// Every dot bit in one column of a braille cell.
    fn column_mask(x: usize) -> u8 {
        BRAILLE.iter().fold(0u8, |acc, row| acc | row[x])
    }

    #[test]
    fn a_suffix_is_stripped_from_the_label() {
        let strip = vec![".example.internal".to_string()];
        assert_eq!(label_for("box.example.internal", &strip), "box");
        assert_eq!(label_for("1.1.1.1", &strip), "1.1.1.1");
    }

    #[test]
    fn a_rising_series_climbs_the_canvas() {
        // Eight samples across four cells - two dots each - from the bottom
        // of the decade the axis covers to the top of it.
        let values: Vec<Option<f64>> = (0..8)
            .map(|i| Some(10f64.powf(1.0 + i as f64 / 7.0)))
            .collect();
        let grid = braille_canvas(&values, 1.0, 2.0, 4, 4);
        let highest: Vec<usize> = (0..4)
            .map(|x| {
                grid.iter()
                    .position(|row| row[x] != 0)
                    .expect("every column carries part of the trace")
            })
            .collect();
        // Row zero is the top of the canvas, so climbing counts down.
        assert_eq!(highest.first(), Some(&3));
        assert_eq!(highest.last(), Some(&0));
        assert!(highest.windows(2).all(|p| p[0] >= p[1]), "{:?}", highest);
    }

    #[test]
    fn two_traces_in_one_cell_keep_both_their_dots() {
        let top = braille_canvas(&[Some(10.0), Some(10.0)], 0.0, 1.0, 1, 1);
        let bottom = braille_canvas(&[Some(1.0), Some(1.0)], 0.0, 1.0, 1, 1);
        assert!(top[0][0] != 0 && bottom[0][0] != 0);
        let cells = overlay(
            &[
                ("first".to_string(), top.clone()),
                ("second".to_string(), bottom.clone()),
            ],
            1,
            1,
        );
        assert_eq!(cells[0][0].1, top[0][0] | bottom[0][0]);
        // Only the hue has to be given up, and it goes to the lower row of
        // the table, which is the rule the reader can apply from outside.
        assert_eq!(cells[0][0].0, "second");
    }
}
