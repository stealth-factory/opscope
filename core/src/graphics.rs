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
    // Rectangle index and its bright end. Ordinary fills remain solid;
    // gradient fills interpolate at pixel resolution inside that rectangle.
    gradients: Vec<(usize, [u8; 3])>,
}

impl Fill {
    fn row(&self, row: usize, cols: usize) -> String {
        let parts: Vec<_> = self.text[row]
            .iter()
            .map(|(c, t)| (c.as_str(), t.clone()))
            .collect();
        let mut text = super::seg(&parts, cols);
        if let Some((colour, trailer)) = self.text[row].last().filter(|(_, t)| t.is_empty()) {
            // seg clips at the cell budget, including zero-width resets.
            // A marker on the last cell must still reset before the label.
            text.push_str(colour);
            debug_assert!(trailer.is_empty());
        }
        text
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

    /// A discrete calendar grid. `None` stays a blank missing-data cell, distinct
    /// from a measured zero (which the caller supplies with its zero colour).
    pub fn heatmap(
        x: usize,
        y: usize,
        cells: &[Vec<Option<String>>],
        missing_colour: &str,
    ) -> Self {
        let levels: Vec<_> = cells
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.as_ref().map(|colour| (colour.clone(), 4)))
                    .collect()
            })
            .collect();
        Self::heatmap_levels(x, y, &levels, missing_colour)
    }

    /// Calendar activity levels: 0 is measured zero, 1..=4 increasing
    /// activity. Callers supply the corresponding colour ramp. Solid tiles
    /// avoid font-dependent shading and tiny stipple patterns; None is blank.
    pub fn heatmap_levels(
        x: usize,
        y: usize,
        cells: &[Vec<Option<(String, u8)>>],
        missing_colour: &str,
    ) -> Self {
        let width = cells.iter().map(Vec::len).max().unwrap_or(0);
        let mut fill = Fill {
            rects: Vec::new(),
            text: Vec::new(),
            gradients: Vec::new(),
        };
        for (r, row) in cells.iter().enumerate() {
            let mut line = Vec::new();
            for c in 0..width {
                let Some((colour, _level)) = row.get(c).and_then(Option::as_ref) else {
                    line.push((missing_colour.into(), " ".into()));

                    continue;
                };
                line.push((colour.clone(), "█".into()));
                // Fill the entire cell, matching the gapless text blocks.
                fill.rects.push(Rect {
                    x: c as f64,
                    y: r as f64,
                    width: 1.0,
                    height: 1.0,
                    colour: colour.clone(),
                });
            }
            fill.text.push(line);
        }
        Self::filled(x, y, width, cells.len(), fill.rects, fill.text)
    }

    /// Adjacent measured fractions on one shared scale, with fractional
    /// pixel boundaries and the existing block-character fallback.
    pub fn stacked_bar(x: usize, y: usize, width: usize, parts: &[(f64, String)]) -> Self {
        let mut left = 0.0;
        let mut rects = Vec::new();
        for (i, (fraction, colour)) in parts.iter().enumerate() {
            let fraction = if fraction.is_finite() {
                fraction.clamp(0.0, 1.0)
            } else {
                0.0
            };
            let right = if i + 1 == parts.len() {
                1.0
            } else {
                (left + fraction).min(1.0)
            };
            if right > left {
                rects.push(Rect {
                    x: left * width as f64,
                    y: 0.0,
                    width: (right - left) * width as f64,
                    height: 1.0,
                    colour: colour.clone(),
                });
            }
            left = right;
        }
        let text = super::stacked_bar(parts, width)
            .into_iter()
            .flat_map(|(colour, text)| {
                text.chars()
                    .map(move |ch| (colour.clone(), ch.to_string()))
                    .collect::<Vec<_>>()
            })
            .collect();
        Self::filled(x, y, width, 1, rects, vec![text])
    }

    /// `meter`, with a colour per filled cell: `cells[i]` paints cell `i`
    /// of the fill, so a ramp the text bar draws is drawn in pixels too.
    /// A slice shorter than the fill repeats its last colour; an empty one
    /// is the track. Text fallback is the same cells.
    /// A truecolour background followed by a truecolour foreground in `mark`
    /// gives the reference a dark surround in pixels and a background in text.
    pub fn ramped_meter(
        x: usize,
        y: usize,
        width: usize,
        fraction: f64,
        marker: Option<f64>,
        cells: &[String],
        track: &str,
        mark: &str,
    ) -> Self {
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let marker = marker.filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0));
        let cell = |i: usize| cells.get(i).or_else(|| cells.last());
        let mut rects = vec![Rect {
            x: 0.0,
            y: 0.0625,
            width: width as f64,
            height: 0.875,
            colour: track.into(),
        }];
        let filled_px = width as f64 * fraction;
        for i in 0..filled_px.ceil() as usize {
            let Some(colour) = cell(i) else { break };
            rects.push(Rect {
                x: i as f64,
                y: 0.0625,
                width: (filled_px - i as f64).min(1.0),
                height: 0.875,
                colour: colour.clone(),
            });
        }
        let mut marker_has_background = false;
        if let Some(at) = marker {
            let x = (at * width as f64).min((width as f64 - 0.25).max(0.0));
            // Keep the marker's position and width; paint its background on
            // either side, clipped to the bar. Do not send background SGR to
            // the foreground-only rasterizer or it falls back to text.
            let mut foreground = mark;
            if let Some((background, ink)) = mark.strip_prefix("\x1b[48;2;").and_then(|s| s.split_once('m')) {
                let background = format!("\x1b[38;2;{background}m");
                if rgb(&background).is_some() && rgb(ink).is_some() {
                    marker_has_background = true;
                    let left = (x - 0.25).max(0.0);
                    rects.push(Rect {
                        x: left,
                        y: 0.0625,
                        width: (x + 0.5).min(width as f64) - left,
                        height: 0.875,
                        colour: background,
                    });
                    foreground = ink;
                }
            }
            rects.push(Rect {
                x,
                y: 0.0625,
                width: 0.25,
                height: 0.875,
                colour: foreground.into(),
            });
        }
        let filled = (fraction * width as f64).round() as usize;
        let at = marker.map(|v| ((v * width as f64).round() as usize).min(width.saturating_sub(1)));
        let text = (0..width)
            .map(|i| {
                let reset = if marker_has_background && at.is_some_and(|at| i == at + 1) {
                    super::NOBG
                } else {
                    ""
                };
                if Some(i) == at {
                    (mark.into(), "┃".into())
                } else if i < filled {
                    (
                        format!("{reset}{}", cell(i).cloned().unwrap_or_else(|| track.into())),
                        "█".into(),
                    )
                } else {
                    (format!("{reset}{track}"), "░".into())
                }
            })
            .collect();
        Self::filled(x, y, width, 1, rects, vec![text])
    }

    /// A measured fraction plus an optional reference position (e.g. elapsed
    /// quota window). Labels and percentages remain ordinary terminal text.
    /// A truecolour background followed by a truecolour foreground in `mark`
    /// gives the reference a dark surround in pixels and a background in text.
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
        Self::ramped_meter(
            x,
            y,
            width,
            fraction,
            marker,
            &vec![colour.to_string(); width],
            track,
            mark,
        )
    }

    /// A smooth horizontal fill in its own hue. Core chooses readable
    /// colour stops and samples them per text cell or per Kitty pixel.
    /// Progress and marker geometry are the same as `meter`.
    pub fn gradient_meter(
        x: usize,
        y: usize,
        width: usize,
        fraction: f64,
        marker: Option<f64>,
        colour: &str,
        track: &str,
        mark: &str,
    ) -> Self {
        let fraction = finite_fraction(fraction);
        let (start, end) = gradient_stops(rgb(colour).unwrap_or([180, 200, 220]));
        let filled = (fraction * width as f64).round() as usize;
        let cells: Vec<_> = (0..filled.max(usize::from(fraction > 0.0)).min(width))
            .map(|i| colour_at(start, end, i as f64 / filled.saturating_sub(1).max(1) as f64))
            .collect();
        let mut plot = Self::ramped_meter(x, y, width, fraction, marker, &cells, track, mark);
        if width > 0 && marker.is_some_and(|v| v.is_finite()) && mark.starts_with("\x1b[48;2;") {
            // The marker can occupy the last cell, with labels immediately
            // after the bar. End its surround even when no next cell did.
            plot.fill.as_mut().unwrap().text[0].push((super::NOBG.into(), String::new()));
        }
        if width > 0 && fraction > 0.0 {
            let fill = plot.fill.as_mut().unwrap();
            let count = (fraction * width as f64).ceil() as usize;
            // Replace cell-sized fill rectangles with one measured region.
            // Marker rectangles stay after the fill and paint over it.
            fill.rects.drain(1..1 + count);
            fill.rects.insert(1, Rect {
                x: 0.0,
                y: 0.0625,
                width: width as f64 * fraction,
                height: 0.875,
                colour: colour_at(start, end, 0.0),
            });
            fill.gradients.push((1, end));
        }
        plot
    }

    /// Smooth within each measured category, keeping its own hue and the
    /// fractional boundaries of `stacked_bar`. The last category takes the
    /// rounding remainder in text, so the bar always occupies its width.
    pub fn gradient_stacked_bar(x: usize, y: usize, width: usize, parts: &[(f64, String)]) -> Self {
        let mut plot = Self::stacked_bar(x, y, width, parts);
        let fill = plot.fill.as_mut().unwrap();
        for (i, rect) in fill.rects.iter_mut().enumerate() {
            let (start, end) = gradient_stops(rgb(&rect.colour).unwrap_or([180, 200, 220]));
            rect.colour = colour_at(start, end, 0.0);
            fill.gradients.push((i, end));
        }
        fill.text[0] = super::stacked_bar(parts, width).into_iter().flat_map(|(colour, text)| {
            let (start, end) = gradient_stops(rgb(&colour).unwrap_or([180, 200, 220]));
            let count = text.chars().count();
            (0..count).map(move |i| (
                colour_at(start, end, i as f64 / count.saturating_sub(1).max(1) as f64),
                "█".into(),
            ))
        }).collect();
        plot
    }

    /// The same fallback cells `draw_plots` uses. A widget can compose
    /// labels or an alert frame without reimplementing the bar's colours,
    /// marker or background resets. Line charts have no stored text row.
    pub fn text_row(&self, row: usize) -> &[(String, String)] {
        self.fill.as_ref().and_then(|fill| fill.text.get(row))
            .map(Vec::as_slice).unwrap_or(&[])
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
        plot.fill = Some(Fill { rects, text, gradients: Vec::new() });
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

const MAX_PLOTS: usize = 32;
const CELL_W: usize = 8;
const CELL_H: usize = 16;
static LIVE: AtomicU32 = AtomicU32::new(0);
static CACHE: Mutex<Vec<Plot>> = Mutex::new(Vec::new());

pub(crate) fn image_id(slot: usize) -> u32 {
    // Five slot bits support a dense quota overview. A separate prefix
    // keeps these IDs clear of older 16-slot widgets in the same mux.
    0x60000000 | (((unsafe { libc::getpid() } as u32) & 0x1fffff) << 5) | slot as u32
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
    heal: bool,
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
            // The periodic whole frame repairs text another process may
            // have scrolled or covered. The terminal still holds the image,
            // so only its placement is put back, not the pixels.
            if let Some(plot) = images.get(slot).filter(|_| heal) {
                after.push_str(&place(plot, slot, w));
            }
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

fn finite_fraction(fraction: f64) -> f64 {
    if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { 0.0 }
}

fn mix_rgb(start: [u8; 3], end: [u8; 3], t: f64) -> [u8; 3] {
    std::array::from_fn(|i| (start[i] as f64 + (end[i] as f64 - start[i] as f64) * t.clamp(0.0, 1.0)).round() as u8)
}

fn colour_at(start: [u8; 3], end: [u8; 3], t: f64) -> String {
    let [r, g, b] = mix_rgb(start, end, t);
    super::rgb(r, g, b)
}

fn luminance(colour: [u8; 3]) -> f64 {
    colour.into_iter().zip([0.2126, 0.7152, 0.0722]).map(|(c, weight)| {
        let c = c as f64 / 255.0;
        weight * if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }).sum()
}

fn gradient_stops(hue: [u8; 3]) -> ([u8; 3], [u8; 3]) {
    // Clocks' brighter ramp, shared by every horizontal bar. Lift a darker
    // provider/category hue just enough for AA on the selected-row tint;
    // the terminal background is darker and clears the same threshold.
    let minimum = 4.5 * (luminance([38, 56, 76]) + 0.05) - 0.05;
    let mut t = 0.15;
    while luminance(mix_rgb(hue, [255; 3], t)) < minimum {
        t = (t + 0.01_f64).min(1.0);
    }
    (mix_rgb(hue, [255; 3], t), mix_rgb(hue, [255; 3], (t + 0.45).min(1.0)))
}

fn pixels(plot: &Plot) -> Vec<u8> {
    let (w, h) = (plot.width * CELL_W, plot.height * CELL_H);
    let mut pixels = vec![0u8; w * h * 4];
    if let Some(fill) = &plot.fill {
        for (index, rect) in fill.rects.iter().enumerate() {
            let colour = rgb(&rect.colour).unwrap_or([180, 200, 220]);
            let end = fill.gradients.iter().find(|(i, _)| *i == index).map(|(_, end)| *end);
            let left = rect.x * CELL_W as f64;
            let top = rect.y * CELL_H as f64;
            let right = left + rect.width * CELL_W as f64;
            let bottom = top + rect.height * CELL_H as f64;
            for y in top.floor().max(0.0) as usize..(bottom.ceil() as usize).min(h) {
                for x in left.floor().max(0.0) as usize..(right.ceil() as usize).min(w) {
                    let area = (right.min(x as f64 + 1.0) - left.max(x as f64)).max(0.0)
                        * (bottom.min(y as f64 + 1.0) - top.max(y as f64)).max(0.0);
                    let at = (y * w + x) * 4;
                    let colour = end.map(|end| mix_rgb(colour, end,
                        (x as f64 - left) / (right - left - 1.0).max(1.0)))
                        .unwrap_or(colour);
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

/// Re-place an image the terminal already holds, at the same cell and size.
/// Same placement id, so it replaces the old placement rather than adding one.
fn place(plot: &Plot, slot: usize, pane_width: usize) -> String {
    format!(
        "\x1b[{};{}H\x1b_Ga=p,i={},p=1,c={},r={},C=1,q=1\x1b\\",
        plot.y + 1,
        plot.x + 1,
        image_id(slot),
        plot.width.min(pane_width - plot.x),
        plot.visible_rows
    )
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
        assert_eq!(area(0), 0);
        assert_eq!(area(1), 128);
        let text = p.fill.as_ref().unwrap().row(0, 2);
        assert!(text.contains(' ') && text.contains('█'));
        // Measured cells include their edges; only missing days are blank.
        for y in 0..16 {
            assert_eq!(bytes[(y * 16 + 8) * 4 + 3], 255);
        }
    }

    #[test]
    fn calendar_tiles_are_gapless_with_blank_missing_days() {
        let colour = super::super::rgb(90, 210, 140);
        let row = std::iter::once(None)
            .chain((0..=4).map(|level| Some((colour.clone(), level))))
            .collect::<Vec<_>>();
        let plot = Plot::heatmap_levels(0, 0, &[row.clone(), row], &colour);
        let text = plot.fill.as_ref().unwrap().row(0, 6);
        assert_eq!(text.matches('█').count(), 5);
        assert!(text.contains(' '));
        let bytes = pixels(&plot);
        let areas: Vec<_> = (0..6)
            .map(|col| {
                (0..16)
                    .flat_map(|y| (0..8).map(move |x| (y * 48 + col * 8 + x) * 4 + 3))
                    .filter(|i| bytes[*i] == 255)
                    .count()
            })
            .collect();
        assert_eq!(areas, vec![0, 128, 128, 128, 128, 128]);
        // Both row and column boundaries remain opaque between measured days.
        for y in 0..32 {
            for x in 0..48 {
                assert_eq!(bytes[(y * 48 + x) * 4 + 3], if x < 8 { 0 } else { 255 });
            }
        }
    }

    #[test]
    fn stacked_bars_preserve_subcell_shares_and_labels_when_cropped() {
        let red = super::super::rgb(240, 0, 0);
        let blue = super::super::rgb(0, 0, 240);
        let plot = Plot::stacked_bar(1, 2, 2, &[(0.2, red), (0.8, blue)]);
        let bytes = pixels(&plot);
        // 20% of 16 pixels is 3.2: a boundary pixel contains both shares.
        assert_eq!(&bytes[0..4], &[240, 0, 0, 255]);
        assert!(bytes[12] > 0 && bytes[14] > 0);
        assert_eq!(&bytes[16..20], &[0, 0, 240, 255]);
        let text = replace_cells(
            " XX exact counts",
            plot.x,
            plot.width,
            &plot.fill.as_ref().unwrap().row(0, plot.width),
        );
        assert!(text.ends_with(" exact counts"));
        let clipped = plot.in_viewport(1, 1, 1).unwrap();
        assert_eq!(clipped.y, 1);
        assert_eq!(pixels(&clipped), bytes);
    }

    #[test]
    fn meter_marker_background_separates_pixels_without_moving_the_mark() {
        let fill = super::super::rgb(30, 220, 180);
        let track = super::super::rgb(80, 100, 120);
        let mark = format!("{}{}", super::super::bg(10, 12, 18), super::super::rgb(238, 244, 252));
        for width in [0, 1, 2, 10] {
            for fraction in [0.0, 0.5, 1.0] {
                for marker in [None, Some(0.0), Some(0.25), Some(0.5), Some(0.75), Some(1.0)] {
                    let plot = Plot::meter(0, 1, width, fraction, marker, &fill, &track, &mark);
                    assert!(plot.fill.as_ref().unwrap().rects.iter().all(|r| rgb(&r.colour).is_some()));
                    let bytes = pixels(&plot);
                    let bare = pixels(&Plot::meter(0, 1, width, fraction, None, &fill, &track, &mark));
                    let start = marker.map(|m| ((m * width as f64).min((width as f64 - 0.25).max(0.0)) * 8.0) as usize);
                    for x in 0..width * 8 {
                        let at = (8 * width * 8 + x) * 4;
                        let pixel = &bytes[at..at + 4];
                        match start {
                            Some(s) if x >= s && x < s + 2 => assert_eq!(pixel, &[238, 244, 252, 255]),
                            Some(s) if x >= s.saturating_sub(2) && x < s + 4 => assert_eq!(pixel, &[10, 12, 18, 255]),
                            _ => assert_eq!(pixel, &bare[at..at + 4]),
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn meter_text_marker_background_is_confined_to_one_cell() {
        let fill = super::super::rgb(30, 220, 180);
        let track = super::super::rgb(80, 100, 120);
        let mark = format!("{}{}", super::super::bg(10, 12, 18), super::super::rgb(238, 244, 252));
        for width in [0, 1, 2, 10] {
            for marker in [None, Some(0.0), Some(0.5), Some(1.0)] {
                let plot = Plot::meter(0, 1, width, 0.5, marker, &fill, &track, &mark);
                let fill = plot.fill.as_ref().unwrap();
                assert_eq!(fill.text[0].iter().map(|(_, t)| super::super::display_width(t)).sum::<usize>(), width);
                let row = replace_cells(&format!("{} label", " ".repeat(width)), 0, width, &fill.row(0, width));
                if width > 0 && marker.is_some() {
                    let tail = row.split_once(&format!("{mark}┃")).unwrap().1;
                    assert!(tail.starts_with(super::super::NOBG) || tail.starts_with(super::super::RST), "{row:?}");
                } else {
                    assert!(!row.contains('┃'));
                }
                assert!(row.ends_with(" label"));
            }
        }
    }

    #[test]
    fn unmarked_and_foreground_only_meters_preserve_inherited_backgrounds() {
        let fill = super::super::rgb(30, 220, 180);
        let track = super::super::rgb(80, 100, 120);
        let ink = super::super::rgb(238, 244, 252);
        let backed = format!("{}{}", super::super::bg(10, 12, 18), ink);
        for (marker, mark) in [(None, &ink), (None, &backed), (Some(0.5), &ink)] {
            let plot = Plot::meter(0, 1, 10, 0.5, marker, &fill, &track, mark);
            let row = plot.fill.as_ref().unwrap().row(0, 10);
            // A selected row's inherited tint must survive across the meter.
            assert!(!row.contains("\x1b[49m"));
            assert!(!row.contains("\x1b[48;"));
            assert!(!row.contains(super::super::RST));
        }
        let plot = Plot::meter(0, 1, 10, 0.5, Some(0.5), &fill, &track, &backed);
        let row = plot.fill.as_ref().unwrap().row(0, 10);
        assert!(!row[..row.find('┃').unwrap()].contains(super::super::NOBG));
        assert_eq!(row.matches(super::super::NOBG).count(), 1);
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
        assert_eq!(at(19, 1), &[30, 220, 180, 255]);
        assert_eq!(at(19, 14), &[30, 220, 180, 255]);
        assert_eq!(at(20, 1), &[80, 100, 120, 255]);
        assert_eq!(at(20, 14), &[80, 100, 120, 255]);
        assert_eq!(at(19, 15)[3], 0);
    }

    #[test]
    fn gradient_meters_are_smooth_at_pixel_resolution_and_share_text_stops() {
        let hue = super::super::rgb(120, 196, 250);
        let track = super::super::rgb(60, 70, 80);
        let plot = Plot::gradient_meter(0, 1, 10, 0.5, None, &hue, &track, &hue);
        let bytes = pixels(&plot);
        let pixel = |x: usize| &bytes[(8 * 80 + x) * 4..(8 * 80 + x + 1) * 4];
        let text = plot.text_row(0);
        assert_eq!(&pixel(0)[..3], rgb(&text[0].0).unwrap());
        assert_eq!(&pixel(39)[..3], rgb(&text[4].0).unwrap());
        assert_eq!(pixel(40), &[60, 70, 80, 255]);
        // Pixels inside one text cell change too: Kitty is not painting
        // a column of identical pixels for each terminal character.
        assert_ne!(pixel(0), pixel(3));
        for x in 1..40 {
            for c in 0..3 { assert!(pixel(x)[c] >= pixel(x - 1)[c]); }
        }
        let solid = Plot::meter(0, 1, 10, 0.5, None, &hue, &track, &hue);
        let bytes = pixels(&solid);
        assert!(bytes.chunks_exact(4).filter(|p| p[3] == 255)
            .all(|p| p[..3] == [120, 196, 250] || p[..3] == [60, 70, 80]));
    }

    #[test]
    fn gradient_meters_preserve_fractional_fill_empty_tracks_and_markers() {
        let hue = super::super::rgb(175, 130, 255);
        let track = super::super::rgb(60, 70, 80);
        let mark = format!("{}{}", super::super::bg(10, 12, 18), super::super::rgb(238, 244, 252));
        for width in [0, 1, 8] {
            for fraction in [-1.0, 0.0, 0.01, 0.5, 1.0, 2.0, f64::NAN, f64::INFINITY] {
                let plot = Plot::gradient_meter(0, 1, width, fraction, None, &hue, &track, &mark);
                let text = plot.text_row(0);
                assert_eq!(text.iter().map(|(_, t)| t.chars().count()).sum::<usize>(), width);
                assert_eq!(text.iter().filter(|(_, t)| t == "█").count(),
                    (finite_fraction(fraction) * width as f64).round() as usize);
                let bytes = pixels(&plot);
                assert_eq!(bytes.len(), width * CELL_W * CELL_H * 4);
                if width > 0 && fraction == 0.01 {
                    let blank = pixels(&Plot::gradient_meter(0, 1, width, 0.0, None, &hue, &track, &mark));
                    assert_ne!(bytes, blank, "lost a measured sub-cell fill");
                }
            }
        }
        let plot = Plot::gradient_meter(0, 1, 8, 1.0, Some(1.0), &hue, &track, &mark);
        assert_eq!(plot.text_row(0)[7], (mark, "┃".into()));
        let row = plot.fill.as_ref().unwrap().row(0, 8);
        assert!(row.ends_with(super::super::NOBG), "last-cell marker leaked a background");
        let bytes = pixels(&plot);
        assert_eq!(&bytes[(8 * 64 + 62) * 4..(8 * 64 + 63) * 4], &[238, 244, 252, 255]);
    }

    #[test]
    fn stacked_gradients_preserve_categories_and_their_measured_boundaries() {
        let parts = [(0.3, super::super::rgb(126, 208, 176)), (0.7, super::super::rgb(127, 147, 172))];
        let solid = Plot::stacked_bar(0, 1, 10, &parts);
        let gradient = Plot::gradient_stacked_bar(0, 1, 10, &parts);
        let a = solid.fill.as_ref().unwrap();
        let b = gradient.fill.as_ref().unwrap();
        assert_eq!(b.text[0].len(), 10);
        for (left, right) in a.rects.iter().zip(&b.rects) {
            assert_eq!((left.x, left.width), (right.x, right.width));
        }
        let text = gradient.text_row(0);
        assert_ne!(text[0].0, text[2].0);
        assert_ne!(text[3].0, text[9].0);
        let bytes = pixels(&gradient);
        assert_eq!(&bytes[(8 * 80 + 23) * 4..(8 * 80 + 23) * 4 + 3], &rgb(&text[2].0).unwrap());
        assert_eq!(&bytes[(8 * 80 + 24) * 4..(8 * 80 + 24) * 4 + 3], &rgb(&text[3].0).unwrap());
    }

    #[test]
    fn gradient_fill_colours_clear_aa_for_every_provider_and_countdown_hue() {
        for hue in [[90,220,255], [255,200,90], [175,130,255], [255,130,120], [120,235,170],
            [160,172,190], [240,132,84], [206,214,228], [126,208,176], [120,196,250],
            [186,166,255], [232,158,200], [255,112,72], [232,226,212], [148,214,255],
            [196,214,140], [127,147,172], [0,0,0]] {
            let (start, end) = gradient_stops(hue);
            for i in 0..=1000 {
                let colour = mix_rgb(start, end, i as f64 / 1000.0);
                for background in [[0,0,0], [18,24,34], [38,56,76]] {
                    let ratio = (luminance(colour) + 0.05) / (luminance(background) + 0.05);
                    assert!(ratio >= 4.5, "{hue:?} on {background:?}: {ratio:.2}");
                }
            }
        }
    }

    #[test]
    fn a_ramped_meter_paints_each_filled_cell_its_own_colour() {
        let a = super::super::rgb(10, 20, 30);
        let b = super::super::rgb(200, 210, 220);
        let track = super::super::rgb(80, 100, 120);
        let mark = super::super::rgb(240, 245, 250);
        let p = Plot::ramped_meter(0, 0, 4, 0.5, None, &[a.clone(), b.clone()], &track, &mark);
        let bytes = pixels(&p);
        let at = |x: usize, y: usize| &bytes[(y * 32 + x) * 4..(y * 32 + x + 1) * 4];
        assert_eq!(at(4, 8), &[10, 20, 30, 255]);
        assert_eq!(at(12, 8), &[200, 210, 220, 255]);
        assert_eq!(at(20, 8), &[80, 100, 120, 255]);
        assert_eq!(at(28, 8), &[80, 100, 120, 255]);
        // A fill longer than the slice repeats the last colour, and a
        // single-entry ramp is a plain `meter` cell for cell.
        let ramped = pixels(&Plot::ramped_meter(0, 0, 4, 1.0, None, &[a.clone()], &track, &mark));
        let plain = pixels(&Plot::meter(0, 0, 4, 1.0, None, &a, &track, &mark));
        assert_eq!(ramped, plain);
        // Text fallback mirrors the cells: two filled, one track, one mark.
        let p = Plot::ramped_meter(0, 0, 4, 0.5, Some(0.75), &[a.clone(), b.clone()], &track, &mark);
        assert_eq!(
            p.fill.as_ref().unwrap().row(0, 4),
            format!("{a}█{b}█{track}░{mark}┃")
        );
    }

    #[test]
    fn a_ramped_meter_keeps_the_padded_marker_and_resets_its_text_background() {
        let cells = (0..4)
            .map(|i| super::super::rgb(20 + i * 40, 30, 50))
            .collect::<Vec<_>>();
        let track = super::super::rgb(80, 100, 120);
        let ink = super::super::rgb(238, 244, 252);
        let mark = format!("{}{ink}", super::super::bg(10, 12, 18));
        let plot = Plot::ramped_meter(0, 0, 4, 1.0, Some(0.5), &cells, &track, &mark);
        let bytes = pixels(&plot);
        let bare = pixels(&Plot::ramped_meter(0, 0, 4, 1.0, None, &cells, &track, &mark));
        for x in 0..32 {
            let offset = (8 * 32 + x) * 4;
            let pixel = &bytes[offset..offset + 4];
            match x {
                16..=17 => assert_eq!(pixel, &[238, 244, 252, 255]),
                14..=15 | 18..=19 => assert_eq!(pixel, &[10, 12, 18, 255]),
                _ => assert_eq!(pixel, &bare[offset..offset + 4]),
            }
        }
        let row = plot.fill.as_ref().unwrap().row(0, 4);
        assert_eq!(row, format!("{}█{}█{mark}┃{}{}█", cells[0], cells[1], super::super::NOBG, cells[3]));
    }

    #[test]
    fn a_sub_cell_fill_needs_a_colour_or_the_pixel_bar_stays_empty() {
        let fill = super::super::rgb(10, 20, 30);
        let track = super::super::rgb(80, 100, 120);
        let mark = super::super::rgb(240, 245, 250);
        // 1% of eight cells is under half a glyph, so both text fallbacks
        // round to an empty track. The pixel path still paints the sliver
        // when it has a colour, and paints nothing when the ramp is empty.
        let sliver = Plot::ramped_meter(0, 0, 8, 0.01, None, &[fill], &track, &mark);
        let blank = Plot::ramped_meter(0, 0, 8, 0.01, None, &[], &track, &mark);
        assert_ne!(pixels(&sliver), pixels(&blank));
        for plot in [&sliver, &blank] {
            let row = plot.fill.as_ref().unwrap().row(0, 8);
            assert!(!row.contains('█'), "{row:?}");
            assert_eq!(row.matches('░').count(), 8, "{row:?}");
        }
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
        let (_, _, bytes) = compose(&rows, &[p.clone()], 40, 8, true, true, false);
        assert!(bytes.contains("a=T"));
        let (_, before, after) = compose(&rows, &[p.clone()], 40, 8, true, false, false);
        assert!(before.is_empty() && after.is_empty());
        // The periodic whole frame re-places the held image, never resends it.
        let (_, before, after) = compose(&rows, &[p.clone()], 40, 8, true, false, true);
        assert!(before.is_empty() && after.contains("a=p") && !after.contains("a=T"));
        let (_, before, after) = compose(&rows, &[p.clone()], 40, 8, true, true, false);
        assert!(before.contains("a=d") && after.contains("a=T"));
        let (text, before, after) = compose(&rows, &[p.clone()], 40, 8, false, false, false);
        assert!(before.contains("a=d") && after.is_empty());
        assert!(text
            .iter()
            .any(|row| row.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c))));
        let mut header_plot = p.clone();
        header_plot.y = 0;
        let (text, _, after) = compose(&rows, &[header_plot], 40, 8, true, false, false);
        assert_eq!(text[0], rows[0]);
        assert!(after.is_empty());
        compose(&rows, &[p], 40, 8, true, false, false);
        let (_, before, after) = compose(&rows, &[], 40, 8, true, false, false);
        assert!(before.contains("a=d") && after.is_empty());
        // A quota overview can have more than sixteen visible bars. Keep
        // every one in Kitty and clean up the highest slot as well.
        let colour = super::super::rgb(120, 196, 250);
        let plots: Vec<_> = (0..32).map(|i| Plot::gradient_meter(
            2, i + 1, 4, 0.5, None, &colour, &colour, &colour,
        )).collect();
        let rows = vec!["          ".into(); 34];
        let (text, _, after) = compose(&rows, &plots, 10, 34, true, true, false);
        assert_eq!(after.matches("a=T").count(), 32);
        assert!(!text.iter().any(|row| row.contains('█') || row.contains('░')));
        let ids: std::collections::HashSet<_> = (0..32).map(image_id).collect();
        assert_eq!(ids.len(), 32);
        assert_eq!(LIVE.load(Ordering::Acquire), u32::MAX);
        let (_, before, after) = compose(&rows, &[], 10, 34, true, false, false);
        assert_eq!(before.matches("a=d").count(), 32);
        assert!(after.is_empty());
        assert_eq!(LIVE.load(Ordering::Acquire), 0);
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
