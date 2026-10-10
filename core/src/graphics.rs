//! Shared chart geometry and optional inline Kitty images. Widgets never emit
//! protocol bytes and the same normalized measurements drive both backends.
use base64::Engine;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Mutex,
};

#[derive(Clone, Debug, PartialEq)]
pub struct Trace {
    /// Bottom-to-top fractions, already transformed to the widget's axis.
    /// None is a missing measurement: never join a line across it.
    pub values: Vec<Option<f64>>,
    /// Optional explicit horizontal fractions. Unobserved time positions
    /// need not be inserted as fake missing samples; None values still break
    /// the line. Both pixel and Braille renderers use these same positions.
    pub positions: Option<Vec<f64>>,
    pub colour: String,
    /// Omit idle runs at this normalized value (e.g. zero network traffic).
    /// Edges into/out of activity still reach the baseline.
    pub baseline: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LineChart {
    pub traces: Vec<Trace>,
    /// Number of time slots across the axis; shorter traces align right.
    pub slots: usize,
    /// This trace wins overlapping text cells and is painted last in pixels.
    pub focus: Option<usize>,
}

impl LineChart {
    pub fn cells(&self, cols: usize, rows: usize) -> Vec<Vec<(String, u8)>> {
        let layers: Vec<_> = self
            .traces
            .iter()
            .map(|t| {
                (
                    t.colour.clone(),
                    braille_with_baseline(
                        &t.values,
                        self.slots,
                        cols,
                        rows,
                        t.baseline,
                        t.positions.as_deref(),
                    ),
                )
            })
            .collect();
        let mut cells = super::overlay(&layers, cols, rows);
        if let Some((colour, canvas)) = self.focus.and_then(|i| layers.get(i)) {
            for (y, row) in canvas.iter().enumerate() {
                for (x, mask) in row.iter().enumerate() {
                    if *mask != 0 {
                        cells[y][x] = (colour.clone(), *mask);
                    }
                }
            }
        }
        cells
    }
}

/// A chart region in body coordinates. Use `in_viewport` after scrolling the
/// text body; it crops graphics without changing the plotted time/value range.
#[derive(Clone, Debug, PartialEq)]
pub struct Plot {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub chart: LineChart,
    fill: Option<Fill>,
    first_row: usize,
    visible_rows: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct Rect {
    // Coordinates in terminal cells, with fractional edges.
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    colour: String,
}

#[derive(Clone, Debug, PartialEq)]
struct Fill {
    rects: Vec<Rect>,
    text: Vec<Vec<(String, String)>>,
}

impl Fill {
    fn row(&self, row: usize, cols: usize) -> String {
        let parts: Vec<_> = self.text[row]
            .iter()
            .map(|(c, t)| (c.as_str(), t.clone()))
            .collect();
        super::seg(&parts, cols)
    }
}

impl Plot {
    pub fn new(x: usize, y: usize, width: usize, height: usize, chart: LineChart) -> Self {
        Self {
            x,
            y,
            width,
            height,
            chart,
            fill: None,
            first_row: 0,
            visible_rows: height,
        }
    }

    /// Discrete columns, one terminal cell per bucket. Both renderers share
    /// the same scale; downward bars retain fractional heights in pixels.
    pub fn bars(
        x: usize,
        y: usize,
        columns: &[(f64, String)],
        height: usize,
        peak: f64,
        down: bool,
    ) -> Self {
        let columns: Vec<_> = columns
            .iter()
            .map(|(v, c)| (if v.is_finite() { v.max(0.0) } else { 0.0 }, c.clone()))
            .collect();
        let peak = if peak.is_finite() && peak > 0.0 {
            peak
        } else {
            columns
                .iter()
                .map(|(v, _)| *v)
                .fold(0.0, f64::max)
                .max(f64::MIN_POSITIVE)
        };
        let text = if down {
            super::vbars_down(&columns, height, peak)
        } else {
            super::vbars(&columns, height, peak)
        };
        let rects = columns
            .iter()
            .enumerate()
            .filter(|(_, (v, _))| *v > 0.0)
            .map(|(i, (v, colour))| {
                let h = (v / peak).min(1.0) * height as f64;
                Rect {
                    x: i as f64,
                    y: if down { 0.0 } else { height as f64 - h },
                    width: 1.0,
                    height: h,
                    colour: colour.clone(),
                }
            })
            .collect();
        Self::filled(x, y, columns.len(), height, rects, text)
    }

    /// A discrete calendar grid. `None` stays a missing-data dot, distinct
    /// from a measured zero (which the caller supplies with its zero colour).
    pub fn heatmap(
        x: usize,
        y: usize,
        cells: &[Vec<Option<String>>],
        missing_colour: &str,
    ) -> Self {
        let width = cells.iter().map(Vec::len).max().unwrap_or(0);
        let mut rects = Vec::new();
        let mut text = Vec::new();
        for (r, row) in cells.iter().enumerate() {
            let mut line = Vec::new();
            for c in 0..width {
                match row.get(c).and_then(Option::as_ref) {
                    Some(colour) => {
                        line.push((colour.clone(), "■".into()));
                        rects.push(Rect {
                            x: c as f64 + 0.125,
                            y: r as f64 + 0.125,
                            width: 0.75,
                            height: 0.75,
                            colour: colour.clone(),
                        });
                    }
                    None => {
                        line.push((missing_colour.into(), "·".into()));
                        rects.push(Rect {
                            x: c as f64 + 0.375,
                            y: r as f64 + 0.4375,
                            width: 0.25,
                            height: 0.125,
                            colour: missing_colour.into(),
                        });
                    }
                }
            }
            text.push(line);
        }
        Self::filled(x, y, width, cells.len(), rects, text)
    }

    /// A measured fraction plus an optional reference position (e.g. elapsed
    /// quota window). Labels and percentages remain ordinary terminal text.
    pub fn meter(
        x: usize,
        y: usize,
        width: usize,
        fraction: f64,
        marker: Option<f64>,
        colour: &str,
        track: &str,
        mark: &str,
    ) -> Self {
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let marker = marker.filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0));
        let mut rects = vec![
            Rect {
                x: 0.0,
                y: 0.25,
                width: width as f64,
                height: 0.5,
                colour: track.into(),
            },
            Rect {
                x: 0.0,
                y: 0.25,
                width: width as f64 * fraction,
                height: 0.5,
                colour: colour.into(),
            },
        ];
        if let Some(at) = marker {
            rects.push(Rect {
                x: (at * width as f64).min((width as f64 - 0.25).max(0.0)),
                y: 0.0625,
                width: 0.25,
                height: 0.875,
                colour: mark.into(),
            });
        }
        let filled = (fraction * width as f64).round() as usize;
        let at = marker.map(|v| ((v * width as f64).round() as usize).min(width.saturating_sub(1)));
        let text = (0..width)
            .map(|i| {
                if Some(i) == at {
                    (mark.into(), "┃".into())
                } else if i < filled {
                    (colour.into(), "█".into())
                } else {
                    (track.into(), "░".into())
                }
            })
            .collect();
        Self::filled(x, y, width, 1, rects, vec![text])
    }

    fn filled(
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        rects: Vec<Rect>,
        text: Vec<Vec<(String, String)>>,
    ) -> Self {
        let mut plot = Self::new(
            x,
            y,
            width,
            height,
            LineChart {
                traces: Vec::new(),
                slots: 0,
                focus: None,
            },
        );
        plot.fill = Some(Fill { rects, text });
        plot
    }

    /// `top` is the pinned header height; `room` excludes the footer.
    pub fn in_viewport(mut self, scroll: usize, top: usize, room: usize) -> Option<Self> {
        let start = top.saturating_add(scroll);
        let end = start.saturating_add(room);
        let first = self.y.max(start);
        let last = self.y.saturating_add(self.height).min(end);
        if first >= last {
            return None;
        }
        self.first_row = first - self.y;
        self.visible_rows = last - first;
        self.y = top + first - start;
        Some(self)
    }
}

/// Shared Braille rasterizer, also useful to widgets drawing small sparklines.
pub fn braille(values: &[Option<f64>], slots: usize, cols: usize, rows: usize) -> Vec<Vec<u8>> {
    braille_with_baseline(values, slots, cols, rows, None, None)
}

fn braille_with_baseline(
    values: &[Option<f64>],
    slots: usize,
    cols: usize,
    rows: usize,
    baseline: Option<f64>,
    positions: Option<&[f64]>,
) -> Vec<Vec<u8>> {
    let mut grid = vec![vec![0; cols]; rows];
    segments(
        values,
        slots,
        cols * 2,
        rows * 4,
        baseline,
        positions,
        |a, b| {
            raster(a, b, |x, y| {
                if x < cols * 2 && y < rows * 4 {
                    grid[y / 4][x / 2] |= super::BRAILLE[y % 4][x % 2];
                }
            });
        },
    );
    grid
}

fn segments(
    values: &[Option<f64>],
    slots: usize,
    w: usize,
    h: usize,
    baseline: Option<f64>,
    positions: Option<&[f64]>,
    mut emit: impl FnMut((usize, usize), (usize, usize)),
) {
    if w == 0 || h == 0 || slots == 0 {
        return;
    }
    let start = if positions.is_some() {
        0
    } else {
        values.len().saturating_sub(slots)
    };
    let values = &values[start..];
    let mut previous = None;
    for (i, value) in values.iter().enumerate() {
        let point = value.filter(|v| v.is_finite()).and_then(|v| {
            let age = values.len() - 1 - i;
            let x = if let Some(positions) = positions {
                let x = *positions.get(start + i)?;
                if !x.is_finite() || !(0.0..=1.0).contains(&x) {
                    return None;
                }
                (x * (w - 1) as f64).round() as usize
            } else {
                w - 1
                    - ((age as f64 * (w - 1) as f64) / slots.saturating_sub(1).max(1) as f64)
                        .round() as usize
            };
            let y = ((1.0 - v.clamp(0.0, 1.0)) * (h - 1) as f64).round() as usize;
            Some((x, y))
        });
        let idle = baseline.is_some()
            && *value == baseline
            && (previous.is_none() || i.checked_sub(1).is_some_and(|i| values[i] == baseline));
        if !idle {
            if let Some(p) = point {
                emit(previous.unwrap_or(p), p);
            }
        }
        previous = point;
    }
}

fn raster(a: (usize, usize), b: (usize, usize), mut dot: impl FnMut(usize, usize)) {
    let (mut x, mut y) = (a.0 as i64, a.1 as i64);
    let (x1, y1) = (b.0 as i64, b.1 as i64);
    let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
    let (sx, sy) = (if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
    let mut err = dx + dy;
    loop {
        dot(x as usize, y as usize);
        if (x, y) == (x1, y1) {
            break;
        }
        let twice = err * 2;
        if twice >= dy {
            err += dy;
            x += sx;
        }
        if twice <= dx {
            err += dx;
            y += sy;
        }
    }
}

const MAX_PLOTS: usize = 16;
const CELL_W: usize = 8;
const CELL_H: usize = 16;
static LIVE: AtomicU32 = AtomicU32::new(0);
static CACHE: Mutex<Vec<Plot>> = Mutex::new(Vec::new());

fn image_id(slot: usize) -> u32 {
    // Avoid collisions with a sibling process after a shell/launcher handoff.
    0x40000000 | (((unsafe { libc::getpid() } as u32) & 0x1fffff) << 4) | slot as u32
}

pub(crate) fn owns_image(id: u32) -> bool {
    (0..MAX_PLOTS).any(|slot| image_id(slot) == id)
}

fn delete(slot: usize) -> String {
    format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", image_id(slot))
}

/// Also called by the signal handler: only atomics and write(2), no locks,
/// formatting, allocation or shared references to a mutating image cache.
pub(crate) fn cleanup_signal() {
    let live = LIVE.swap(0, Ordering::AcqRel);
    for slot in 0..MAX_PLOTS {
        if live & (1 << slot) == 0 {
            continue;
        }
        let mut buf = [0u8; 64];
        let prefix = b"\x1b_Ga=d,d=I,i=";
        buf[..prefix.len()].copy_from_slice(prefix);
        let mut pos = prefix.len();
        let id = image_id(slot);
        let mut divisor = 1_000_000_000;
        while divisor > 1 && id / divisor == 0 {
            divisor /= 10;
        }
        loop {
            buf[pos] = b'0' + ((id / divisor) % 10) as u8;
            pos += 1;
            if divisor == 1 {
                break;
            }
            divisor /= 10;
        }
        let suffix = b",q=2\x1b\\";
        buf[pos..pos + suffix.len()].copy_from_slice(suffix);
        unsafe {
            libc::write(libc::STDOUT_FILENO, buf.as_ptr().cast(), pos + suffix.len());
        }
    }
}

pub(crate) fn cleanup() {
    cleanup_signal();
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Compose chart regions through core, preserving the sanitizer and header.
/// Returns text plus image deletion/transmission bytes for a single frame.
pub(crate) fn compose(
    rows: &[String],
    plots: &[Plot],
    w: usize,
    h: usize,
    kitty: bool,
    lost: bool,
) -> (Vec<String>, String, String) {
    let mut text = rows.to_vec();
    text.resize(h, String::new());
    let mut images = Vec::new();
    for plot in plots {
        if plot.y == 0
            || plot.x >= w
            || plot.y >= h
            || plot.width == 0
            || plot.height == 0
            || plot.width.saturating_mul(plot.height) > 1_000_000
        {
            continue;
        }
        let mut plot = plot.clone();
        plot.visible_rows = plot
            .visible_rows
            .min(h - plot.y)
            .min(plot.height.saturating_sub(plot.first_row));
        if plot.visible_rows == 0 {
            continue;
        }
        let cols = plot.width.min(w - plot.x);
        let use_pixels = kitty
            && images.len() < MAX_PLOTS
            && plot.width.saturating_mul(plot.height) <= 20_000
            && plot
                .fill
                .as_ref()
                .is_none_or(|f| f.rects.iter().all(|r| rgb(&r.colour).is_some()))
            && plot.chart.traces.iter().all(|t| rgb(&t.colour).is_some());
        let cells = if use_pixels || plot.fill.is_some() {
            Vec::new()
        } else {
            plot.chart.cells(plot.width, plot.height)
        };
        for row in 0..plot.visible_rows {
            let content = if use_pixels {
                " ".repeat(cols)
            } else if let Some(fill) = &plot.fill {
                fill.row(row + plot.first_row, cols)
            } else {
                cells[row + plot.first_row]
                    .iter()
                    .take(cols)
                    .map(|(c, mask)| {
                        format!(
                            "{}{}",
                            c,
                            char::from_u32(0x2800 + *mask as u32).unwrap_or(' ')
                        )
                    })
                    .collect::<String>()
            };
            text[plot.y + row] = replace_cells(&text[plot.y + row], plot.x, cols, &content);
        }
        if use_pixels {
            // Source width is kept for geometry; clipping happens in image().
            images.push(plot);
        }
    }
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let mut before = String::new();
    let mut after = String::new();
    for slot in 0..cache.len().max(images.len()) {
        if !lost && cache.get(slot) == images.get(slot) {
            continue;
        }
        if cache.get(slot).is_some() {
            before.push_str(&delete(slot));
        }
        if let Some(plot) = images.get(slot) {
            // Set before writing: a signal during transmission must clean it up.
            LIVE.fetch_or(1 << slot, Ordering::Release);
            after.push_str(&image(plot, slot, w));
        } else {
            LIVE.fetch_and(!(1 << slot), Ordering::Release);
        }
    }
    *cache = images;
    (text, before, after)
}

fn replace_cells(row: &str, x: usize, width: usize, replacement: &str) -> String {
    let row = super::inert(row);
    let mut out = String::new();
    let mut column = 0;
    let mut inserted = false;
    let mut chars = row.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            for c in chars.by_ref() {
                out.push(c);
                if c == 'm' {
                    break;
                }
            }
            continue;
        }
        let size = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if column >= x && !inserted {
            out.push_str(replacement);
            out.push_str(super::RST);
            inserted = true;
        }
        if column < x || column >= x + width {
            out.push(c);
        }
        column += size;
    }
    if !inserted {
        out.push_str(&" ".repeat(x.saturating_sub(column)));
        out.push_str(replacement);
        out.push_str(super::RST);
    }
    out
}

fn rgb(colour: &str) -> Option<[u8; 3]> {
    let s = colour.strip_prefix("\x1b[38;2;")?.strip_suffix('m')?;
    let mut values = s.split(';').map(str::parse::<u8>);
    let colour = [
        values.next()?.ok()?,
        values.next()?.ok()?,
        values.next()?.ok()?,
    ];
    values.next().is_none().then_some(colour)
}

fn pixels(plot: &Plot) -> Vec<u8> {
    let (w, h) = (plot.width * CELL_W, plot.height * CELL_H);
    let mut pixels = vec![0u8; w * h * 4];
    if let Some(fill) = &plot.fill {
        for rect in &fill.rects {
            let colour = rgb(&rect.colour).unwrap_or([180, 200, 220]);
            let left = rect.x * CELL_W as f64;
            let top = rect.y * CELL_H as f64;
            let right = left + rect.width * CELL_W as f64;
            let bottom = top + rect.height * CELL_H as f64;
            for y in top.floor().max(0.0) as usize..(bottom.ceil() as usize).min(h) {
                for x in left.floor().max(0.0) as usize..(right.ceil() as usize).min(w) {
                    let area = (right.min(x as f64 + 1.0) - left.max(x as f64)).max(0.0)
                        * (bottom.min(y as f64 + 1.0) - top.max(y as f64)).max(0.0);
                    let at = (y * w + x) * 4;
                    blend(
                        &mut pixels[at..at + 4],
                        colour,
                        (area * 255.0).round() as u32,
                    );
                }
            }
        }
        return pixels;
    }
    let order = (0..plot.chart.traces.len())
        .filter(|i| Some(*i) != plot.chart.focus)
        .chain(plot.chart.focus.filter(|i| *i < plot.chart.traces.len()));
    for i in order {
        let trace = &plot.chart.traces[i];
        let colour = rgb(&trace.colour).unwrap_or([180, 200, 220]);
        let mut coverage = vec![0u8; w * h * 4];
        segments(
            &trace.values,
            plot.chart.slots,
            w,
            h,
            trace.baseline,
            trace.positions.as_deref(),
            |a, b| {
                // Draw a one-pixel-wide stroke at 2x resolution, then downsample
                // coverage. Smoothing never crosses a missing measurement.
                raster((a.0 * 2, a.1 * 2), (b.0 * 2, b.1 * 2), |x, y| {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            if x + dx < w * 2 && y + dy < h * 2 {
                                coverage[(y + dy) * w * 2 + x + dx] = 1;
                            }
                        }
                    }
                });
            },
        );
        for y in 0..h {
            for x in 0..w {
                let at = y * 2 * w * 2 + x * 2;
                let count = coverage[at]
                    + coverage[at + 1]
                    + coverage[at + w * 2]
                    + coverage[at + w * 2 + 1];
                if count == 0 {
                    continue;
                }
                let alpha = count as u32 * 255 / 4;
                let p = (y * w + x) * 4;
                let old = pixels[p + 3] as u32 * (255 - alpha) / 255;
                let total = alpha + old;
                for c in 0..3 {
                    pixels[p + c] =
                        ((colour[c] as u32 * alpha + pixels[p + c] as u32 * old) / total) as u8;
                }
                pixels[p + 3] = total as u8;
            }
        }
    }
    pixels
}

fn blend(pixel: &mut [u8], colour: [u8; 3], alpha: u32) {
    if alpha == 0 {
        return;
    }
    let old = pixel[3] as u32 * (255 - alpha) / 255;
    let total = alpha + old;
    for c in 0..3 {
        pixel[c] = ((colour[c] as u32 * alpha + pixel[c] as u32 * old) / total) as u8;
    }
    pixel[3] = total as u8;
}

fn image(plot: &Plot, slot: usize, pane_width: usize) -> String {
    let source = pixels(plot);
    let cols = plot.width.min(pane_width - plot.x);
    let (w, h) = (cols * CELL_W, plot.visible_rows * CELL_H);
    let mut clipped = Vec::with_capacity(w * h * 4);
    for y in plot.first_row * CELL_H..(plot.first_row + plot.visible_rows) * CELL_H {
        let start = y * plot.width * CELL_W * 4;
        clipped.extend_from_slice(&source[start..start + w * 4]);
    }
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&clipped, 3);
    let payload = base64::engine::general_purpose::STANDARD.encode(compressed);
    let mut out = format!("\x1b[{};{}H", plot.y + 1, plot.x + 1);
    let chunks: Vec<_> = payload.as_bytes().chunks(4096).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = usize::from(i + 1 < chunks.len());
        if i == 0 {
            out.push_str(&format!(
                "\x1b_Ga=T,f=32,t=d,o=z,s={w},v={h},i={},p=1,c={cols},r={},C=1,q=1,m={more};",
                image_id(slot),
                plot.visible_rows
            ));
        } else {
            out.push_str(&format!("\x1b_Gm={more};"));
        }
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        out.push_str("\x1b\\");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn chart(values: Vec<Option<f64>>) -> LineChart {
        LineChart {
            slots: values.len(),
            traces: vec![Trace {
                positions: None,
                values,
                colour: super::super::rgb(30, 220, 180),
                baseline: None,
            }],
            focus: None,
        }
    }

    #[test]
    fn sparse_time_positions_connect_in_both_renderers_but_losses_break() {
        let mut c = chart(vec![Some(0.5), Some(0.5)]);
        c.traces[0].positions = Some(vec![0.1, 0.9]);
        let grid = c.cells(10, 4);
        assert!(grid[1..3].iter().any(|row| row[5].1 != 0));
        let bytes = pixels(&Plot::new(0, 1, 10, 4, c.clone()));
        assert!((0..64).any(|y| bytes[(y * 80 + 40) * 4 + 3] != 0));
        c.traces[0].values = vec![Some(0.5), None, Some(0.5)];
        c.traces[0].positions = Some(vec![0.1, 0.5, 0.9]);
        // Explicit positions are not truncated to the regular-slot count.
        let grid = c.cells(10, 4);
        assert!(grid.iter().all(|row| row[5].1 == 0));
        assert!(grid.iter().any(|row| row[1].1 != 0));
        let bytes = pixels(&Plot::new(0, 1, 10, 4, c));
        assert!((0..64).all(|y| bytes[(y * 80 + 40) * 4 + 3] == 0));
    }

    #[test]
    fn identical_time_positions_draw_vertical_segments_in_both_renderers() {
        let mut c = chart(vec![Some(0.0), Some(1.0)]);
        c.traces[0].positions = Some(vec![0.5, 0.5]);
        assert!(c.cells(10, 8).iter().all(|row| row[5].1 != 0));
        let bytes = pixels(&Plot::new(0, 1, 10, 8, c));
        assert!((0..128).all(|y| bytes[(y * 80 + 40) * 4 + 3] != 0));
    }

    #[test]
    fn discrete_bars_preserve_scale_direction_and_empty_buckets() {
        let colour = super::super::rgb(30, 220, 180);
        let cols = vec![
            (0.0, colour.clone()),
            (0.1, colour.clone()),
            (1.0, colour.clone()),
        ];
        let up = Plot::bars(0, 1, &cols, 3, 1.0, false);
        let down = Plot::bars(0, 1, &cols, 3, 1.0, true);
        let a = pixels(&up);
        let b = pixels(&down);
        for y in 0..48 {
            for x in 0..24 {
                assert_eq!(a[(y * 24 + x) * 4 + 3], b[((47 - y) * 24 + x) * 4 + 3]);
                if x < 8 {
                    assert_eq!(a[(y * 24 + x) * 4 + 3], 0);
                }
            }
        }
        // A tenth-height bar reaches 4.8 pixels; it must not round up to
        // the text backend's half-cell step or disappear below that step.
        assert_eq!(b[(4 * 24 + 8) * 4 + 3], 204);
        assert_eq!(b[(5 * 24 + 8) * 4 + 3], 0);
        let subsecond = Plot::bars(0, 1, &[(0.2, colour)], 1, 0.0, false);
        assert!(pixels(&subsecond).chunks_exact(4).all(|p| p[3] == 255));
    }

    #[test]
    fn heatmaps_distinguish_missing_days_from_measured_zeroes() {
        let zero = super::super::rgb(120, 145, 170);
        let p = Plot::heatmap(5, 2, &[vec![None, Some(zero.clone())]], &zero);
        let bytes = pixels(&p);
        let area = |col: usize| {
            (0..16)
                .flat_map(|y| (col * 8..(col + 1) * 8).map(move |x| (y * 16 + x) * 4 + 3))
                .filter(|i| bytes[*i] == 255)
                .count()
        };
        assert_eq!(area(0), 4);
        assert_eq!(area(1), 72);
        let text = p.fill.as_ref().unwrap().row(0, 2);
        assert!(text.contains('·') && text.contains('■'));
        // Cell gutters remain transparent: adjacent days never merge.
        for y in 0..16 {
            assert_eq!(bytes[(y * 16 + 8) * 4 + 3], 0);
        }
    }

    #[test]
    fn meter_reference_does_not_change_measured_fill() {
        let fill = super::super::rgb(30, 220, 180);
        let track = super::super::rgb(80, 100, 120);
        let mark = super::super::rgb(240, 245, 250);
        let p = Plot::meter(2, 1, 10, 0.25, Some(0.75), &fill, &track, &mark);
        let bytes = pixels(&p);
        let at = |x: usize, y: usize| &bytes[(y * 80 + x) * 4..(y * 80 + x + 1) * 4];
        assert_eq!(at(19, 8), &[30, 220, 180, 255]);
        assert_eq!(at(20, 8), &[80, 100, 120, 255]);
        assert_eq!(at(60, 8), &[240, 245, 250, 255]);
        assert_eq!(at(20, 0)[3], 0);
    }

    #[test]
    fn filled_plots_crop_without_changing_values_or_labels() {
        let colour = super::super::rgb(80, 220, 170);
        let p = Plot::bars(4, 3, &[(0.2, colour.clone()), (1.0, colour)], 4, 1.0, false);
        let clipped = p.clone().in_viewport(4, 1, 2).unwrap();
        assert_eq!(pixels(&p), pixels(&clipped));
        assert_eq!(
            (clipped.y, clipped.first_row, clipped.visible_rows),
            (1, 2, 2)
        );
        let rows: Vec<String> = vec!["title".into(), "axis.. units".into(), "axis.. units".into()];
        // Render text directly here to avoid sharing the image cache with
        // lifecycle tests running in parallel.
        let fill = clipped.fill.as_ref().unwrap();
        for (i, row) in rows.iter().skip(1).enumerate() {
            let text = replace_cells(row, 4, 2, &fill.row(i + clipped.first_row, 2));
            assert!(text.starts_with("axis") && text.ends_with(" units"));
        }
    }

    #[test]
    fn missing_measurements_are_gaps_at_both_resolutions() {
        let c = chart(vec![Some(0.5), None, None, Some(0.5)]);
        let grid = c.cells(8, 2);
        assert!(grid.iter().all(|r| r[3].1 == 0 && r[4].1 == 0));
        let p = Plot::new(0, 1, 8, 2, c);
        let bytes = pixels(&p);
        for y in 0..32 {
            assert_eq!(bytes[(y * 64 + 32) * 4 + 3], 0);
        }
    }

    #[test]
    fn scrolling_crops_without_rescaling_and_protects_the_header() {
        let p = Plot::new(8, 4, 20, 8, chart(vec![Some(0.2)]));
        let visible = p.clone().in_viewport(5, 1, 4).unwrap();
        assert_eq!(
            (
                visible.y,
                visible.height,
                visible.first_row,
                visible.visible_rows
            ),
            (1, 8, 2, 4)
        );
        assert!(p.in_viewport(20, 1, 4).is_none());
    }

    #[test]
    fn inline_images_are_compressed_chunked_and_do_not_move_the_cursor() {
        let p = Plot::new(8, 1, 20, 5, chart(vec![Some(0.0), Some(1.0)]));
        let command = image(&p, 0, 80);
        assert!(command.contains("f=32,t=d,o=z,s=160,v=80"));
        assert!(command.contains("c=20,r=5,C=1,q=1"));
        let payload = command.split_once(';').unwrap().1;
        // Locate the graphics header (the cursor sequence also has a semicolon).
        let payload = payload
            .split_once(";")
            .unwrap()
            .1
            .trim_end_matches("\x1b\\");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        let decoded = miniz_oxide::inflate::decompress_to_vec_zlib(&bytes).unwrap();
        assert_eq!(decoded, pixels(&p));
        assert!(command.len() < decoded.len() / 4);
    }

    #[test]
    fn replacement_keeps_axis_and_footer_text_and_removes_widget_escapes() {
        let row = format!("{}axis│....│unit\x1b[2J", super::super::rgb(100, 200, 200));
        let replaced = replace_cells(&row, 5, 4, "plot");
        assert!(replaced.contains("axis│plot"));
        assert!(replaced.ends_with("│unit"));
        assert!(!replaced.contains("\x1b[2J"));
    }

    #[test]
    fn an_idle_run_stays_blank_but_activity_reaches_the_baseline() {
        let mut c = chart(vec![Some(0.0); 5]);
        c.traces[0].baseline = Some(0.0);
        assert!(c.cells(10, 3).iter().flatten().all(|(_, mask)| *mask == 0));
        assert!(pixels(&Plot::new(0, 1, 10, 3, c.clone()))
            .iter()
            .all(|b| *b == 0));
        c.traces[0].values[2] = Some(1.0);
        assert!(c.cells(10, 3)[0].iter().any(|(_, mask)| *mask != 0));
        assert!(c.cells(10, 3)[2].iter().any(|(_, mask)| *mask != 0));
    }

    #[test]
    fn image_cache_reuses_unchanged_plots_and_deletes_on_fallback_or_screen_change() {
        let p = Plot::new(2, 1, 10, 3, chart(vec![Some(0.2), Some(0.8)]));
        let rows = vec!["protected title".into(), "axis".into()];
        let (_, _, bytes) = compose(&rows, &[p.clone()], 40, 8, true, true);
        assert!(bytes.contains("a=T"));
        let (_, before, after) = compose(&rows, &[p.clone()], 40, 8, true, false);
        assert!(before.is_empty() && after.is_empty());
        let (_, before, after) = compose(&rows, &[p.clone()], 40, 8, true, true);
        assert!(before.contains("a=d") && after.contains("a=T"));
        let (text, before, after) = compose(&rows, &[p.clone()], 40, 8, false, false);
        assert!(before.contains("a=d") && after.is_empty());
        assert!(text
            .iter()
            .any(|row| row.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c))));
        let mut header_plot = p.clone();
        header_plot.y = 0;
        let (text, _, after) = compose(&rows, &[header_plot], 40, 8, true, false);
        assert_eq!(text[0], rows[0]);
        assert!(after.is_empty());
        compose(&rows, &[p], 40, 8, true, false);
        let (_, before, after) = compose(&rows, &[], 40, 8, true, false);
        assert!(before.contains("a=d") && after.is_empty());
    }

    #[test]
    fn multi_chunk_images_round_trip_with_exact_cropped_pixels() {
        let mut c = chart(Vec::new());
        c.slots = 120;
        let mut seed = 7u32;
        c.traces = (0..12)
            .map(|i| Trace {
                positions: None,
                values: (0..120)
                    .map(|_| {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        Some(seed as f64 / u32::MAX as f64)
                    })
                    .collect(),
                colour: super::super::rgb(30 + i * 15, 220 - i * 10, 170),
                baseline: None,
            })
            .collect();
        let p = Plot::new(3, 2, 100, 10, c).in_viewport(3, 1, 5).unwrap();
        let command = image(&p, 0, 70);
        let chunks: Vec<_> = command.split("\x1b_G").skip(1).collect();
        assert!(chunks.len() > 1);
        let mut payload = String::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let (keys, data) = chunk.split_once(';').unwrap();
            let data = data.trim_end_matches("\x1b\\");
            assert!(data.len() <= 4096);
            assert_eq!(data.len() % 4, 0);
            assert!(keys.contains(if i + 1 == chunks.len() { "m=0" } else { "m=1" }));
            payload.push_str(data);
        }
        let encoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        let decoded = miniz_oxide::inflate::decompress_to_vec_zlib(&encoded).unwrap();
        let full = pixels(&p);
        let expected: Vec<_> = (p.first_row * CELL_H..(p.first_row + p.visible_rows) * CELL_H)
            .flat_map(|y| {
                full[y * p.width * CELL_W * 4..y * p.width * CELL_W * 4 + 67 * CELL_W * 4]
                    .iter()
                    .copied()
            })
            .collect();
        assert_eq!(decoded, expected);
    }
}
