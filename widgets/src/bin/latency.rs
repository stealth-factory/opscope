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

//! Multi-target latency monitor.
//!
//! A port of latency.py. One ping per target, read line by line as it
//! arrives, so the numbers are what ping measured rather than anything this
//! timed itself.

/// The hues targets are drawn in, kept as numbers rather than escapes so
/// that a faded set can be mixed from the same nine.
const HUES: &[(u8, u8, u8)] = &[
    (90, 220, 255),
    (255, 170, 80),
    (140, 255, 160),
    (230, 140, 255),
    (255, 110, 130),
    (255, 230, 110),
    (120, 160, 255),
    (255, 140, 200),
    (150, 255, 240),
];

/// The dark these widgets are drawn against.
///
/// Not the terminal's real background - that cannot be asked for - but the
/// one the palette was chosen for, and what a trace is mixed toward when it
/// is not the one being looked at.
const BACKDROP: (u8, u8, u8) = (16, 22, 30);

/// How far an unlooked-at trace is mixed toward the backdrop.
///
/// Measured rather than chosen by eye, against the two things it sits
/// between. At 0.60 a faded trace reads 2.04 against the backdrop where the
/// axis furniture reads 1.55, so it stays visibly ink rather than sinking
/// into the chrome; and it is 3.29 from its own full-strength hue, clearing
/// the 3.0 that separates two graphical tones. Fading further wins
/// separation and loses the trace into the grid - by 0.65 it is dimmer than
/// the axis it is drawn over.
const FADE: f64 = 0.60;

use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opscope_core as tc;

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
    /// Why this target has no reading, when the reason is this widget's own
    /// rather than the network's. A host that does not answer is a result;
    /// a `ping` that will not start is a failure, and the two used to look
    /// identical - an empty row either way, retried every two seconds for
    /// as long as the widget was up, saying nothing.
    err: String,
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
    /// Record one reading and keep only the last `window` of them.
    ///
    /// Retention lives here rather than beside a push because it used to
    /// live beside one push and not the other: the trim sat inside the loop
    /// that reads `ping`'s output, so a target whose `ping` exits at once -
    /// an unresolvable name, a host with no route to it - appended a loss
    /// from the retry path every two seconds and never trimmed. The vector
    /// grew for as long as the pane was up and `stats()` reread the whole
    /// of it every frame. One door in, one rule.
    fn record(&mut self, at: f64, rtt: Option<f64>, window: usize) {
        self.samples.push((at, rtt));
        if self.samples.len() > window {
            let drop = self.samples.len() - window;
            self.samples.drain(..drop);
        }
    }

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
                (colour.clone(), tc::SPARK[((frac * 7.99) as usize).min(7)])
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
            Err(e) => {
                let why = format!("ping will not start: {}", e);
                if let Ok(mut guard) = shared.lock() {
                    guard[index].err = why;
                    guard[index].alive = false;
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        // It started, so whatever stopped it last time is over.
        if let Ok(mut guard) = shared.lock() {
            guard[index].err.clear();
        }
        if let Ok(mut guard) = shared.lock() {
            guard[index].pid = Some(child.id() as i32);
        }
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => continue,
        };
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let stamp = tc::now();
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
                target.record(stamp, Some(rtt), window);
            } else if is_loss(&line) {
                if target.down_since.is_none() {
                    target.down_since = Some(stamp);
                    log(&events, &hue, &label, "LOSS", "no reply".into());
                }
                target.alive = false;
                target.record(stamp, None, window);
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
                target.down_since = Some(tc::now());
                drop(guard);
                log(&events, &hue, &label, "DOWN", "ping exited, retrying".into());
            } else {
                drop(guard);
            }
        }
        if let Ok(mut guard) = shared.lock() {
            guard[index].alive = false;
            guard[index].record(tc::now(), None, window);
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
            grid[y as usize / 4][x as usize / 2] |= tc::BRAILLE[y as usize % 4][x as usize % 2];
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
    focus: Option<usize>,
    p: &Palette,
) -> (Vec<String>, f64) {
    let gw = w.saturating_sub(9).max(10);
    let gh = h.max(4);
    // Two dot columns to a cell, so the chart holds twice the buckets it
    // did when each one had a whole character to itself.
    let slots = gw * 2;
    let newest = (tc::now() / bucket).floor();
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
        // Nothing to plot has two causes and they are not the same. With no
        // targets there is nothing to collect and never will be, so
        // "collecting…" is a promise the widget cannot keep - it is the
        // pane looking busy over an empty list, which is the shape this
        // repo's checks exist to catch.
        let why = if targets.is_empty() {
            "  no hosts configured - set latency.hosts in config.json"
        } else {
            "  collecting…"
        };
        return (vec![tc::seg(&[(p.dim.as_str(), why.into())], w - 1)], span);
    }
    let lo = seen.iter().cloned().fold(f64::INFINITY, f64::min).max(0.05) * 0.8;
    let hi = (seen.iter().cloned().fold(0.0f64, f64::max) * 1.25).max(lo * 1.6);
    let (llo, lhi) = (lo.log10(), hi.log10());

    // One canvas per target rather than one shared grid: a braille cell can
    // carry the dots of two traces but only one hue, so each series has to
    // keep its own until the moment they are laid over one another.
    //
    // The selected target is laid down last, so where two traces share a
    // cell the colour goes to the one being looked at rather than to
    // whichever happens to sit lower in the table. The others are mixed
    // most of the way to the backdrop - still drawn, because a chart that
    // dropped every other target the moment you selected one would be
    // answering a different question, but no longer competing.
    let mut layers: Vec<(String, Vec<Vec<u8>>)> = Vec::with_capacity(series.len());
    let mut front: Option<(String, Vec<Vec<u8>>)> = None;
    for (idx, values) in &series {
        let canvas = braille_canvas(values, llo, lhi, gw, gh);
        let hue = p.hues[idx % p.hues.len()].clone();
        match focus {
            // Nothing selected: every trace at full strength, in table
            // order, which is the chart this widget has always drawn.
            None => layers.push((hue, canvas)),
            Some(at) if at == *idx => front = Some((hue, canvas)),
            Some(_) => layers.push((p.faded[idx % p.faded.len()].clone(), canvas)),
        }
    }
    let mut cells = tc::overlay(&layers, gw, gh);
    // The focused trace takes its cells outright rather than being merged
    // into them.
    //
    // `overlay` unions the dots and gives the cell to the last writer, which
    // is right when every trace is equal: no sample is lost and the colour
    // follows the table's order. Under focus it is a lie. Two targets a few
    // percent apart share a cell constantly - 138ms and 127ms sit 0.036 of a
    // decade apart where a dot row is 0.065 - and merging drew the other
    // one's dots in the focused colour, so a flat trace came out two rows
    // thick and the second row belonged to a different host.
    //
    // Replacing the cell hides the faded trace where the two meet. That is
    // the right way round: the faded one is the one being pushed back, and
    // it stays legible either side, whereas a sample drawn in a colour that
    // is not its own is a number on screen that is not real.
    if let Some((hue, canvas)) = front {
        for (y, line) in canvas.iter().enumerate().take(gh) {
            for (x, mask) in line.iter().enumerate().take(gw) {
                if *mask != 0 {
                    cells[y][x] = (hue.clone(), *mask);
                }
            }
        }
    }

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
    // Fixed for the life of the run: one poll thread per host, and the
    // list never grows or shrinks, so the cursor can be reasoned about
    // without holding the lock to count rows.
    let count = hosts.len();
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
    // Which target the chart brings to the front, if any. Starts at none,
    // so the chart opens saying what it has always said - every target at
    // equal weight - and focus is something you ask for rather than a state
    // you have to escape from. Clamped against the list when the frame is
    // drawn rather than when the key is pressed, because targets are only
    // known to the poll threads.
    let mut selected: Option<usize> = None;
    // How far the list has been scrolled, for when there are more targets
    // than the pane is tall. The rows were truncated to the body height
    // before, so a seventh host on a six-host pane simply was not drawn.
    //
    // The arrows do not touch this. They walk a focus that deliberately
    // wraps through "no selection" at either end, which is a fine thing
    // for a key and a bad one for a wheel: scrolling up from no selection
    // would jump to the last host and then go round for ever.
    let mut scroll = 0usize;
    loop {
        for key in keyboard.poll() {
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                // No selection is the position above the first row and
                // below the last one, so walking off either end lands there
                // and walking off it again comes in at the other. Focus is
                // then something you can leave the way you entered it,
                // rather than a state with only one door.
                "up" | "k" | "K" => {
                    selected = match selected {
                        None => count.checked_sub(1),
                        Some(0) => None,
                        Some(at) => Some(at - 1),
                    }
                }
                "down" | "j" | "J" => {
                    selected = match selected {
                        None if count > 0 => Some(0),
                        None => None,
                        Some(at) if at + 1 >= count => None,
                        Some(at) => Some(at + 1),
                    }
                }
                // The widget, not the focus. Only does anything when there
                // are more hosts than fit; on a list that already fits it
                // is clamped to zero below and the wheel is inert, which
                // is the right amount of nothing to do.
                "ctrl-y" | "wheel-up" => scroll = scroll.saturating_sub(1),
                "ctrl-e" | "wheel-down" => scroll = scroll.saturating_add(1),
                // Back to every target drawn alike.
                "esc" => selected = None,
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
        if let Some(at) = selected {
            selected = if snapshot.is_empty() {
                None
            } else {
                Some(at.min(snapshot.len() - 1))
            };
        }
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
                    "  {} {:>7} {:>7}{} {:>7} {:>7} {:>7} {:>6}",
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
            let here = selected == Some(i);
            // The selected row is tinted rather than marked, so the thing
            // that says "this one" in the table is the same thing that says
            // it in the chart: one target at full strength, the rest behind.
            let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
            let raw_hue = p.hues[i % p.hues.len()].clone();
            let loss_c = if st.loss == 0.0 {
                &p.ok
            } else if st.loss < 5.0 {
                &p.warn
            } else {
                &p.bad
            };
            // A tint is a background escape and has to come before every
            // foreground on the row, or the colour that follows it resets
            // the background and the highlight stops halfway across.
            let tinted = |colour: &str| format!("{}{}", tint, colour);
            // MIN and MAX are drawn in `dim`, which does not clear AA on the
            // tint. On the selected row they get the lighter one.
            let dim = if here { &p.dim_lit } else { &p.dim };
            // Owned colours rather than borrowed: the row is assembled
            // before it is measured, so a `&format!(...)` temporary would not
            // outlive the vector it was put in.
            let mut cells: Vec<(String, String)> = vec![
                // No status dot. latency.py draws ● or ○ here, but it says
                // exactly what NOW says one column to the right and at the
                // same instant - a target that is not answering has no round
                // trip to print - so it was six glyphs of permanent green
                // reporting something already on screen. Losing it also lets
                // the columns sit under their own headings, which the dot's
                // uncounted cell prevented.
                //
                // The cell the header spends on a leading space is the marker
                // column. A tint alone says "this row is not like the others"
                // without saying which way, and it is gone entirely on a
                // terminal that will not paint one - so the mark is a solid
                // bar rather than a glyph, because the tint cannot be made
                // louder: (28,44,62) is already as bright as it goes before
                // `bad` red drops under AA on it, measured at 4.80 against a
                // floor of 4.5.
                //
                // ▐ is East Asian Neutral, so it is one cell wherever it
                // renders. ▌ and █ read better but are Ambiguous, and this
                // row is otherwise all ASCII - one Ambiguous cell would shift
                // the selected row and nothing else.
                (tinted(&raw_hue), "▐".to_string()),
                // The chip is a colour, not a letter, and reads as part of
                // the name when it touches it. The header spends the same
                // cell so the columns still sit under their own headings.
                (tint.clone(), " ".to_string()),
                // Grey until this is the row being looked at, then white -
                // which is how link tells its selected session apart, and
                // the reason its list reads at a glance. The hue cannot do
                // that job: fading a hue far enough to be a contrast takes
                // it under AA long before it is a difference the eye
                // catches, measured at 3.86 by a fade of 0.30 and only 1.73
                // away from the full colour. So the hue moves to the chip,
                // which is what link puts its glyph in, and the name is free
                // to swing between two colours that are both readable.
                (
                    tinted(if t.err.is_empty() {
                        if here { &p.txt } else { &p.dim_lit }
                    } else {
                        &p.bad
                    }),
                    tc::pad(&t.label, name_w),
                ),
                // Red when there is no round trip to report, which is the
                // whole of what the status dot used to say.
                (
                    tinted(if st.now.is_some() { &p.txt } else { &p.bad }),
                    format!(" {}", fmt_ms(st.now)),
                ),
                (tinted(&p.txt), format!(" {}", fmt_ms(st.avg))),
                (
                    tinted(&p.ok),
                    if show_med {
                        format!(" {}", fmt_ms(st.med))
                    } else {
                        String::new()
                    },
                ),
                (tinted(dim), format!(" {}", fmt_ms(st.min))),
                (tinted(dim), format!(" {}", fmt_ms(st.max))),
                (tinted(&p.txt), format!(" {}", fmt_ms(st.jit))),
                (tinted(loss_c), format!(" {:>5.1}%", st.loss)),
                // The reason, when there is one. It sits after the figures
                // rather than replacing them: the numbers from before it
                // broke are still true, and still the last thing this
                // target was known to be doing.
                (
                    tinted(&p.bad),
                    if t.err.is_empty() {
                        String::new()
                    } else {
                        format!("  ! {}", t.err)
                    },
                ),
            ];
            // Carry the tint to the edge. Left ragged it stops wherever the
            // last number happens to end, and a highlight that stops short
            // reads as a smudge rather than as a bar across the row.
            if here {
                let used: usize = cells.iter().map(|(_, t)| t.chars().count()).sum();
                cells.push((tint.clone(), " ".repeat((w - 1).saturating_sub(used))));
            }
            let parts: Vec<(&str, String)> =
                cells.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&parts, w - 1));
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

        // The keys live along the bottom, so they are measured before the
        // chart and the log are sized and the rows they occupy are taken out
        // of what is left. Sizing the body to the whole pane and appending
        // them afterwards is how a footer ends up pushed off the bottom of
        // the screen it documents. `pack_hints` wraps without splitting one,
        // because half a hint teaches a key that does not exist.
        let mut hints: Vec<Vec<(&str, String)>> = vec![vec![
            (p.head.as_str(), "↑↓".into()),
            (p.dim.as_str(), " focus".into()),
        ]];
        // Offered only once there is a selection to clear. An empty hint
        // would still cost `pack_hints` a separator and leave a gap in the
        // footer where a key used to be.
        if selected.is_some() {
            hints.push(vec![(p.dim.as_str(), "[esc] clear focus".into())]);
        }
        hints.extend([
            vec![(p.dim.as_str(), "[i]nterval".into())],
            vec![(p.dim.as_str(), "[g]roup".into())],
            vec![(p.dim.as_str(), "[c]olumns".into())],
            vec![(p.dim.as_str(), "[q]uit".into())],
        ]);
        let foot: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
        let body_h = h.saturating_sub(foot.len());

        // The log only earns its space on a tall pane: on a short one the
        // chart is the thing worth keeping.
        let log_h = if body_h.saturating_sub(rows.len()) > 20 { 7 } else { 0 };
        let gh = body_h.saturating_sub(rows.len() + log_h + 4).max(4);
        let (chart, span) = graph(&snapshot, w, gh, bucket, &how, selected, &p);
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

        // A window onto the rows rather than a cut of them. Truncating
        // dropped every host past the bottom edge with nothing saying so,
        // which on a short pane is the difference between watching six
        // targets and thinking you have six. The title is pinned above it.
        let (head, rest) = rows.split_at(1.min(rows.len()));
        let room_below = body_h.saturating_sub(head.len()).max(1);
        scroll = scroll.min(rest.len().saturating_sub(room_below));
        let last = (scroll + room_below).min(rest.len());
        let mut rows: Vec<String> = head.to_vec();
        rows.extend_from_slice(&rest[scroll..last]);
        while rows.len() < body_h {
            rows.push(String::new());
        }
        rows.extend(foot);
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
    /// The readable dim, used wherever `dim` would fail: on the selected
    /// row's tint, where (70,100,120) measures 2.27 against AA's 4.5, and
    /// for every host name, which is grey until its row is the one selected.
    /// This clears AA on the tint at 4.83 and on the backdrop at 6.18.
    dim_lit: String,
    head: String,
    hues: Vec<String>,
    /// The same nine, mixed toward the backdrop, for traces that are not
    /// the one selected.
    faded: Vec<String>,
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
        dim_lit: tc::rgb(120, 155, 180),
        head: tc::rgb(90, 220, 255),
        // latency.py's own nine, not the six the other widgets share: the
        // traces are told apart by hue alone now that the glyphs are gone,
        // so more targets than six needs more than six colours.
        hues: HUES.iter().map(|c| tc::rgb(c.0, c.1, c.2)).collect(),
        faded: HUES.iter().map(|c| tc::mix(*c, BACKDROP, FADE)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_that_never_answers_does_not_grow_for_ever() {
        // The retry path - `ping` exited, sleep two seconds, try again -
        // records a loss each time round. Its retention used to sit in the
        // loop over `ping`'s output, which a target that exits immediately
        // never enters, so an unreachable host grew the vector by a sample
        // every two seconds for as long as the pane was up and made
        // `stats()` reread all of it every frame.
        let window = 8;
        let mut t = Target::default();
        for i in 0..500 {
            t.record(i as f64, None, window);
            assert!(t.samples.len() <= window, "grew to {} at {}", t.samples.len(), i);
        }
        assert_eq!(t.samples.len(), window);
        // What is kept is the newest, not the oldest.
        assert_eq!(t.samples.first().map(|(at, _)| *at), Some(492.0));
        assert_eq!(t.samples.last().map(|(at, _)| *at), Some(499.0));
        // Answers are held to the same rule.
        let mut t = Target::default();
        for i in 0..500 {
            t.record(i as f64, Some(i as f64), window);
        }
        assert_eq!(t.samples.len(), window);
    }

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
        tc::BRAILLE.iter().fold(0u8, |acc, row| acc | row[x])
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
    fn a_contested_cell_belongs_to_one_trace_and_shares_the_run() {
        // Two flat traces, one at the top of the decade and one at the
        // bottom, both drawn across all four cells. This used to assert the
        // union of their dots in the later trace's colour, and that was the
        // bug: on a real chart two hosts a few percent apart contest every
        // cell, so one of them was drawn entirely in the other's hue and
        // vanished from the chart as a distinct line.
        let high = [Some(10.0); 8];
        let low = [Some(1.0); 8];
        let top = braille_canvas(&high, 0.0, 1.0, 4, 1);
        let bottom = braille_canvas(&low, 0.0, 1.0, 4, 1);
        assert!(top[0][0] != 0 && bottom[0][0] != 0, "both must contest");
        let cells = tc::overlay(
            &[
                ("first".to_string(), top.clone()),
                ("second".to_string(), bottom.clone()),
            ],
            4,
            1,
        );
        for x in 0..4 {
            let (whose, mask) = &cells[0][x];
            // Never the union: a cell shows one trace's samples, so no dot
            // is ever painted in a colour that is not its own.
            let mine = if whose == "first" { top[0][x] } else { bottom[0][x] };
            assert_eq!(*mask, mine, "column {} carries the other trace's dots", x);
            assert_ne!(*mask, top[0][x] | bottom[0][x], "column {} merged", x);
        }
        // Ownership advances with the column, so a contested stretch shows
        // both traces as interleaved dashes rather than hiding one.
        // The property rather than the phase: the run is shared, so neither
        // trace is missing from it. Pinning the exact alternation here is
        // what let the parity starvation through - it asserted the mechanism
        // and called that the contract.
        let owners: Vec<&str> = (0..4).map(|x| cells[0][x].0.as_str()).collect();
        assert!(owners.contains(&"first"), "{:?}", owners);
        assert!(owners.contains(&"second"), "{:?}", owners);
    }

    #[test]
    fn no_trace_is_starved_by_where_the_contention_falls() {
        // Two traces that meet only in even-numbered cells, which is what a
        // series with a gap in every other column produces - and the column
        // width is free-form config, so it is reachable rather than
        // theoretical. Ownership used to be `x % claims.len()`, so every
        // contested cell shared a parity, the modulus picked the same
        // claimant every time, and the other trace was absent from the whole
        // stretch. That is the failure this function exists to prevent, one
        // level in.
        let a = vec![vec![0b0000_0001u8, 0, 0b0000_0001, 0]];
        let b = vec![vec![0b0100_0000u8, 0, 0b0100_0000, 0]];
        let cells = tc::overlay(&[("first".to_string(), a), ("second".to_string(), b)], 4, 1);
        let owners: Vec<&str> = (0..4)
            .filter(|x| cells[0][*x].1 != 0)
            .map(|x| cells[0][x].0.as_str())
            .collect();
        assert_eq!(owners.len(), 2, "both contested cells should be drawn");
        // The property, not the phase. Which of the two takes the first cell
        // is an implementation detail; that neither vanishes is not.
        assert!(owners.contains(&"first"), "{:?}", owners);
        assert!(owners.contains(&"second"), "{:?}", owners);
    }

}
