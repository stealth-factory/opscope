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
    host: String,
    label: String,
    ip: String,
    samples: Vec<(f64, Option<f64>)>, // (when, rtt or a loss)
    down_since: Option<f64>,
}

impl Target {
    /// Round trips that arrived, newest last.
    fn rtts(&self) -> Vec<f64> {
        self.samples.iter().filter_map(|(_, r)| *r).collect()
    }

    fn median(&self) -> Option<f64> {
        let mut got = self.rtts();
        if got.is_empty() {
            return None;
        }
        got.sort_by(|a, b| a.partial_cmp(b).unwrap());
        Some(got[got.len() / 2])
    }

    /// The spread of the middle of the distribution, not the extremes.
    ///
    /// A single 400ms spike in a thousand samples is worth knowing about,
    /// but it is not what the link feels like, and a standard deviation
    /// would let it dominate the number.
    fn jitter(&self) -> Option<f64> {
        let got = self.rtts();
        if got.len() < 2 {
            return None;
        }
        let median = self.median()?;
        let mut deviations: Vec<f64> = got.iter().map(|r| (r - median).abs()).collect();
        deviations.sort_by(|a, b| a.partial_cmp(b).unwrap());
        Some(deviations[deviations.len() / 2])
    }

    fn worst(&self) -> Option<f64> {
        self.rtts().into_iter().fold(None, |acc: Option<f64>, r| {
            Some(acc.map_or(r, |a: f64| a.max(r)))
        })
    }

    fn loss(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let lost = self.samples.iter().filter(|(_, r)| r.is_none()).count();
        100.0 * lost as f64 / self.samples.len() as f64
    }
}

fn ms(value: Option<f64>) -> String {
    match value {
        None => "—".into(),
        Some(v) if v >= 100.0 => format!("{:.0}ms", v),
        Some(v) if v >= 10.0 => format!("{:.1}ms", v),
        Some(v) => format!("{:.2}ms", v),
    }
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

/// Keep one ping running per target, forever.
fn watch(host: String, index: usize, interval: f64, window: usize, shared: Arc<Mutex<Vec<Target>>>) {
    loop {
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
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => continue,
        };
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
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
            let stamp = now();
            if let Some(rtt) = rtt_of(&line) {
                target.samples.push((stamp, Some(rtt)));
                target.down_since = None;
            } else if is_loss(&line) {
                target.samples.push((stamp, None));
                if target.down_since.is_none() {
                    target.down_since = Some(stamp);
                }
            }
            if target.samples.len() > window {
                let drop = target.samples.len() - window;
                target.samples.drain(..drop);
            }
        }
        let _ = child.wait();
        // ping exited - the host may have gone, or the network. Retry
        // rather than leaving a dead row that never updates again.
        std::thread::sleep(Duration::from_secs(2));
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
fn braille_canvas(values: &[f64], llo: f64, lhi: f64, cols: usize, rows: usize) -> Vec<Vec<u8>> {
    let (px_w, px_h) = (cols * 2, rows * 4);
    let mut grid = vec![vec![0u8; cols]; rows];
    if values.is_empty() || px_w == 0 || px_h == 0 {
        return grid;
    }
    let vals: Vec<f64> = values.iter().rev().take(px_w).rev().copied().collect();
    // Newest against the right edge: a target that has answered five times
    // shows five samples there, not five stretched across the whole width.
    let left = px_w - vals.len();
    let decade = (lhi - llo).max(1e-9);
    let point = |i: usize| -> (i64, i64) {
        let frac = ((vals[i].max(1e-3).log10() - llo) / decade).clamp(0.0, 1.0);
        (
            (left + i) as i64,
            ((1.0 - frac) * (px_h as f64 - 1.0)).round() as i64,
        )
    };
    let dot = |x: i64, y: i64, grid: &mut Vec<Vec<u8>>| {
        if x >= 0 && (x as usize) < px_w && y >= 0 && (y as usize) < px_h {
            grid[y as usize / 4][x as usize / 2] |= BRAILLE[y as usize % 4][x as usize % 2];
        }
    };
    // Every value here is a reply that arrived, so unlike netwatch's idle
    // zero there is no reading that means "nothing happened" and should be
    // left blank. One sample is a measurement and gets its dot.
    let (x, y) = point(0);
    dot(x, y, &mut grid);
    for i in 1..vals.len() {
        let (mut x0, mut y0) = point(i - 1);
        let (x1, y1) = point(i);
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
fn graph(targets: &[Target], w: usize, h: usize, p: &Palette) -> Vec<String> {
    let gw = w.saturating_sub(9).max(10);
    let gh = h.max(4);
    let series: Vec<(usize, Vec<f64>)> = targets
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let all = t.rtts();
            // Two dots to a cell across, so the chart holds twice the pings
            // it did when each one had a character to itself.
            let start = all.len().saturating_sub(gw * 2);
            (i, all[start..].to_vec())
        })
        .filter(|(_, v)| !v.is_empty())
        .collect();
    if series.is_empty() {
        return vec![tc::seg(&[(p.dim.as_str(), "  collecting…".into())], w - 1)];
    }
    let lo = series
        .iter()
        .flat_map(|(_, v)| v.iter())
        .cloned()
        .fold(f64::INFINITY, f64::min)
        .max(0.05)
        * 0.8;
    let hi = series
        .iter()
        .flat_map(|(_, v)| v.iter())
        .cloned()
        .fold(0.0f64, f64::max)
        .max(lo * 1.6)
        * 1.25;
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
            format!("{:>7}", ms(Some(value)))
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
    out
}

const SERIES: &[char] = &['●', '▲', '■', '◆', '✚', '✦'];

fn main() {
    tc::maybe_help(include_str!("latency_help.txt"));
    let cfg = tc::load_config("latency");
    let hosts = tc::cfg_strings(&cfg, "hosts", &["1.1.1.1", "8.8.8.8"]);
    let mut interval = tc::cfg_f64(&cfg, "interval", 0.5);
    let window = tc::cfg_usize(&cfg, "window", 600);
    let strip: Vec<String> = tc::cfg_strings(&cfg, "strip_suffixes", &[]);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut named: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-i" | "--interval" if i + 1 < args.len() => {
                interval = args[i + 1].parse::<f64>().unwrap_or(0.5).max(0.1);
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
            host: h.clone(),
            label: label_for(h, &strip),
            ..Default::default()
        })
        .collect();
    let shared = Arc::new(Mutex::new(targets));
    for (index, host) in hosts.iter().enumerate() {
        let shared = Arc::clone(&shared);
        let host = host.clone();
        std::thread::spawn(move || watch(host, index, interval, window, shared));
    }

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
        let snapshot: Vec<Target> = match shared.lock() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };

        let mut rows = vec![tc::title("network latency monitor", w, &p.head)];
        let live = snapshot.iter().filter(|t| t.down_since.is_none()).count();
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!(" {} targets", snapshot.len())),
                (p.dim.as_str(), " · ".into()),
                (
                    if live == snapshot.len() { &p.ok } else { &p.bad },
                    format!("{} answering", live),
                ),
                (p.dim.as_str(), format!("   every {}s", interval)),
            ],
            w - 1,
        ));
        rows.push(String::new());

        let name_w = snapshot
            .iter()
            .map(|t| t.label.chars().count())
            .max()
            .unwrap_or(8)
            .clamp(8, 24);
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!("  {}", tc::pad("TARGET", name_w))),
                (p.dim.as_str(), format!("{:>9}", "MEDIAN")),
                (p.dim.as_str(), format!("{:>9}", "JITTER")),
                (p.dim.as_str(), format!("{:>9}", "WORST")),
                (p.dim.as_str(), format!("{:>8}", "LOSS")),
            ],
            w - 1,
        ));
        for (i, t) in snapshot.iter().enumerate() {
            let glyph = SERIES[i % SERIES.len()];
            let hue = &p.hues[i % p.hues.len()];
            let loss = t.loss();
            rows.push(tc::seg(
                &[
                    (hue.as_str(), format!(" {}", glyph)),
                    (p.txt.as_str(), tc::pad(&t.label, name_w)),
                    (p.txt.as_str(), format!("{:>9}", ms(t.median()))),
                    (p.dim.as_str(), format!("{:>9}", ms(t.jitter()))),
                    (p.dim.as_str(), format!("{:>9}", ms(t.worst()))),
                    (
                        if loss > 0.0 { &p.bad } else { &p.dim },
                        format!("{:>7.1}%", loss),
                    ),
                ],
                w - 1,
            ));
        }
        rows.push(String::new());

        let room = h.saturating_sub(rows.len() + 3);
        if room >= 5 {
            rows.extend(graph(&snapshot, w, room, &p));
        }

        let hints: Vec<Vec<(&str, String)>> = vec![vec![(p.dim.as_str(), "[q]uit".into())]];
        let foot: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
        while rows.len() < h.saturating_sub(foot.len()) {
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
    bad: String,
    dim: String,
    grid: String,
    txt: String,
    head: String,
    hues: Vec<String>,
}

fn palette() -> Palette {
    Palette {
        ok: tc::rgb(90, 240, 160),
        bad: tc::rgb(255, 100, 110),
        dim: tc::rgb(127, 147, 172),
        grid: tc::rgb(60, 78, 98),
        txt: tc::rgb(225, 235, 245),
        head: tc::rgb(90, 220, 255),
        hues: vec![
            tc::rgb(120, 200, 255),
            tc::rgb(150, 230, 180),
            tc::rgb(220, 170, 255),
            tc::rgb(160, 190, 240),
            tc::rgb(200, 220, 150),
            tc::rgb(240, 180, 210),
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
    fn jitter_is_the_middle_of_the_spread_not_the_extremes() {
        let mut t = Target::default();
        // Nine steady samples and one wild spike: the spike belongs in
        // worst, and must not be allowed to define jitter.
        for v in [10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 400.0] {
            t.samples.push((0.0, Some(v)));
        }
        assert_eq!(t.median(), Some(10.0));
        assert_eq!(t.jitter(), Some(0.0));
        assert_eq!(t.worst(), Some(400.0));
    }

    #[test]
    fn loss_counts_the_unanswered() {
        let mut t = Target::default();
        t.samples.push((0.0, Some(10.0)));
        t.samples.push((0.0, None));
        t.samples.push((0.0, Some(12.0)));
        t.samples.push((0.0, None));
        assert_eq!(t.loss(), 50.0);
    }

    #[test]
    fn milliseconds_gain_precision_as_they_shrink() {
        assert_eq!(ms(Some(123.4)), "123ms");
        assert_eq!(ms(Some(12.34)), "12.3ms");
        assert_eq!(ms(Some(1.234)), "1.23ms");
        assert_eq!(ms(None), "—");
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
        let values: Vec<f64> = (0..8).map(|i| 10f64.powf(1.0 + i as f64 / 7.0)).collect();
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
        let top = braille_canvas(&[10.0, 10.0], 0.0, 1.0, 1, 1);
        let bottom = braille_canvas(&[1.0, 1.0], 0.0, 1.0, 1, 1);
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
