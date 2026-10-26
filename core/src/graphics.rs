//! Shared chart geometry and optional inline Kitty images. Widgets never emit
//! protocol bytes and the same normalized measurements drive both backends.
use base64::Engine;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;

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
    /// Place the image under terminal text. Line charts only: a solid fill
    /// would hide the glyphs and fail the contrast rule.
    under_text: bool,
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
    /// Drawn as eighth-cell text in every mode, never as an image. A
    /// one-row bar gains nothing visible from pixels, and meters are the
    /// plots that change most often: a pace marker follows the clock.
    always_text: bool,
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
            under_text: false,
        }
    }

    /// Draw this line chart under the cell text instead of blanking it.
    ///
    /// Pixel mode keeps the glyphs and the selected-row tint. Inside tmux
    /// the blank cells of that rectangle are Unicode placeholders, so the
    /// image stays anchored to the text and the reading stays put. Braille
    /// and text mode still paint the chart the way they do today. Solid fills
    /// ignore it: text over a heatmap or a meter does not stay readable.
    pub fn under_text(mut self) -> Self {
        if self.fill.is_none() {
            self.under_text = true;
        }
        self
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
            always_text: false,
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
        let colours: Vec<_> = parts.iter().map(|(_, colour)| colour.clone()).collect();
        let text = stacked_cells(parts, width, |part, _, _| colours[part].clone());
        let mut plot = Self::filled(x, y, width, 1, rects, vec![text]);
        plot.fill.as_mut().unwrap().always_text = true;
        plot
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
        // Whole cells, then one partial cell to the nearest eighth.
        let eighths = (fraction * width as f64 * 8.0).round() as usize;
        let (whole, part) = (eighths / 8, eighths % 8);
        let at = marker.map(|v| ((v * width as f64).round() as usize).min(width.saturating_sub(1)));
        let text = (0..width)
            .map(|i| {
                let reset = if marker_has_background && at.is_some_and(|at| i == at + 1) {
                    super::NOBG
                } else {
                    ""
                };
                let filled = |glyph: char| match cell(i) {
                    Some(colour) => (format!("{reset}{colour}"), glyph.to_string()),
                    None => (format!("{reset}{track}"), "░".into()),
                };
                if Some(i) == at {
                    (mark.into(), "┃".into())
                } else if i < whole {
                    filled('█')
                } else if i == whole && part > 0 {
                    filled(LEFT_EIGHTHS[part])
                } else {
                    (format!("{reset}{track}"), "░".into())
                }
            })
            .collect();
        let mut plot = Self::filled(x, y, width, 1, rects, vec![text]);
        plot.fill.as_mut().unwrap().always_text = true;
        plot
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
        let (start, end) = gradient_stops(rgb(colour).unwrap_or([180, 200, 220]));
        Self::gradient_meter_between(x, y, width, fraction, marker, start, end, track, mark)
    }

    /// `gradient_meter` between two stops the caller chooses, for a hue the
    /// shared ramp would wash toward white. Each stop is still lifted to AA
    /// on the selected-row tint when it falls short, so a dark brand colour
    /// cannot make a bar unreadable where `gradient_meter`'s would not be.
    #[allow(clippy::too_many_arguments)]
    pub fn gradient_meter_between(
        x: usize,
        y: usize,
        width: usize,
        fraction: f64,
        marker: Option<f64>,
        start: [u8; 3],
        end: [u8; 3],
        track: &str,
        mark: &str,
    ) -> Self {
        let fraction = finite_fraction(fraction);
        let (start, end) = (readable_on_tint(start, 0.0), readable_on_tint(end, 0.0));
        // Every cell the fill touches, the partial one included.
        let filled = ((fraction * width as f64 * 8.0).round() as usize).div_ceil(8).max(usize::from(fraction > 0.0)).min(width);
        let cells: Vec<_> = (0..filled)
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
        let stops: Vec<_> = parts
            .iter()
            .map(|(_, colour)| gradient_stops(rgb(colour).unwrap_or([180, 200, 220])))
            .collect();
        fill.text[0] = stacked_cells(parts, width, |part, i, count| {
            let (start, end) = stops[part];
            colour_at(start, end, i as f64 / count.saturating_sub(1).max(1) as f64)
        });
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
        plot.fill = Some(Fill { rects, text, gradients: Vec::new(), always_text: false });
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

/// Left-aligned partial blocks, in eighths of a cell: index 8 is a full cell.
const LEFT_EIGHTHS: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// The text cells of a stacked bar, to the nearest eighth of a cell.
///
/// Each category's edge is rounded to an eighth. A cell two categories
/// share is the left one's partial block on the right one's colour as a
/// background, so the cell stays solid and the boundary lands where the
/// share puts it; the next cell clears that background. A cell three or
/// more categories share goes whole to the largest of them. A category
/// smaller than a cell can lose its sliver to its neighbours, as in whole
/// cells.
/// `colour(part, i, count)` is category `part`'s colour for its `i`th of
/// `count` cells, so a gradient can restart inside each category.
fn stacked_cells(
    parts: &[(f64, String)],
    width: usize,
    colour: impl Fn(usize, usize, usize) -> String,
) -> Vec<(String, String)> {
    let total = width * 8;
    let mut edges = Vec::with_capacity(parts.len());
    let mut left = 0.0;
    for (i, (fraction, _)) in parts.iter().enumerate() {
        let fraction = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { 0.0 };
        left = if i + 1 == parts.len() { 1.0 } else { (left + fraction).min(1.0) };
        edges.push(((left * total as f64).round() as usize).min(total));
    }
    // The category covering eighth `e`.
    let at = |e: usize| edges.iter().position(|edge| e < *edge);
    // Cells a category touches, for its gradient.
    let span = |part: usize| {
        let start = if part == 0 { 0 } else { edges[part - 1] };
        let end = edges[part];
        if end <= start { (0, 0) } else { (start / 8, (end - 1) / 8 + 1) }
    };
    let mut out = Vec::with_capacity(width + 1);
    let mut background = false;
    for cell in 0..width {
        let reset = if background { super::NOBG } else { "" };
        background = false;
        let (Some(first), Some(last)) = (at(cell * 8), at(cell * 8 + 7)) else {
            out.push((reset.to_string(), " ".to_string()));
            continue;
        };
        let ink = |part: usize| {
            let (start, end) = span(part);
            colour(part, cell.saturating_sub(start), end - start)
        };
        if first == last {
            out.push((format!("{reset}{}", ink(first)), "█".into()));
            continue;
        }
        // Three or more categories in one cell: one partial block can only
        // show two, so the largest overlap takes the whole cell. A zero
        // share between two others occupies nothing and does not count.
        if (first + 1..last).any(|part| edges[part] > edges[part - 1]) {
            let overlap = |part: usize| {
                let start = if part == 0 { 0 } else { edges[part - 1] };
                edges[part].min(cell * 8 + 8).saturating_sub(start.max(cell * 8))
            };
            let widest = (first..=last).rev().max_by_key(|&part| overlap(part)).unwrap_or(first);
            out.push((format!("{reset}{}", ink(widest)), "█".into()));
            continue;
        }
        let share = edges[first] - cell * 8;
        let (fg, bg) = (ink(first), ink(last));
        match rgb(&bg) {
            Some([r, g, b]) => {
                background = true;
                out.push((format!("{reset}{fg}\x1b[48;2;{r};{g};{b}m"), LEFT_EIGHTHS[share].to_string()));
            }
            // No truecolour to paint behind it: the larger share takes the cell.
            None => out.push((format!("{reset}{}", if share >= 4 { fg } else { bg }), "█".into())),
        }
    }
    if background {
        out.push((super::NOBG.into(), String::new()));
    }
    out
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

/// Pixel row of a bottom-to-top chart fraction inside a bitmap `height` pixels tall.
///
/// Both renderers round against this height minus one. A label that rounds
/// against the cell count instead misses the stroke.
fn pixel_y(value: f64, height: usize) -> usize {
    ((1.0 - value.clamp(0.0, 1.0)) * (height - 1) as f64).round() as usize
}

/// Terminal row of a bottom-to-top chart fraction.
///
/// The stroke is placed in a bitmap `rows * cell_h` pixels tall, where
/// `cell_h` is the terminal's cell height, and that pixel falls into a
/// cell. A four-row chart at 20% of peak ends on row 3 when the cell is
/// 16 pixels; rounding against `rows - 1` puts a label on row 2.
pub fn line_row(value: f64, rows: usize) -> usize {
    if rows == 0 {
        return 0;
    }
    let (_, cell_h) = super::terminal::cell_size();
    let cell_h = cell_h.max(1);
    pixel_y(value, rows.saturating_mul(cell_h)) / cell_h
}

/// Terminal column of a left-to-right chart fraction.
///
/// The stroke rounds against `cols * cell_w - 1` pixels, the same width the
/// line raster uses, and that pixel falls into a cell. A label that assumes
/// the point is the rightmost cell sits past the stroke when later buckets
/// are gaps.
pub fn line_col(position: f64, cols: usize) -> usize {
    if cols == 0 {
        return 0;
    }
    let (cell_w, _) = super::terminal::cell_size();
    let cell_w = cell_w.max(1);
    let width = cols.saturating_mul(cell_w);
    let x = (position.clamp(0.0, 1.0) * width.saturating_sub(1) as f64).round() as usize;
    x / cell_w
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
            let y = pixel_y(v, h);
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

pub(crate) const MAX_PLOTS: usize = 32;
#[cfg(test)]
const CELL_W: usize = super::terminal::FALLBACK_CELL_W;
#[cfg(test)]
const CELL_H: usize = super::terminal::FALLBACK_CELL_H;
static LIVE: AtomicU32 = AtomicU32::new(0);
// Tests run in parallel in one process. A process-wide cache makes an
// unchanged chart look new because another test cleared it.
#[cfg(not(test))]
static CACHE: Mutex<Vec<CachedPlot>> = Mutex::new(Vec::new());
#[cfg(test)]
std::thread_local! {
    static CACHE: std::cell::RefCell<Vec<CachedPlot>> = const { std::cell::RefCell::new(Vec::new()) };
}
// At most 8 MiB across all slots. Larger crops compare by re-rasterizing
// the old plot rather than retaining another large pixel buffer.
const MAX_CACHED_RGBA: usize = 256 * 1024;

struct CachedPlot {
    plot: Plot,
    cols: usize,
    cell_size: (usize, usize),
    rgba: Option<Vec<u8>>,
}

impl CachedPlot {
    fn same_region(&self, plot: &Plot, cols: usize, cell_size: (usize, usize)) -> bool {
        self.plot.x == plot.x
            && self.plot.y == plot.y
            && self.cols == cols
            && self.cell_size == cell_size
            && self.plot.first_row == plot.first_row
            && self.plot.visible_rows == plot.visible_rows
    }

    fn same_pixels(&self, plot: &Plot, cols: usize, cell_size: (usize, usize), rgba: &[u8]) -> bool {
        self.same_region(plot, cols, cell_size)
            && match &self.rgba {
                Some(previous) => previous == rgba,
                None => raster_pixels(&self.plot, self.cols, cell_size.0, cell_size.1) == rgba,
            }
    }
}

fn with_cache<R>(f: impl FnOnce(&mut Vec<CachedPlot>) -> R) -> R {
    #[cfg(test)]
    {
        CACHE.with(|cache| f(&mut cache.borrow_mut()))
    }
    #[cfg(not(test))]
    {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut cache)
    }
}

static PLACEMENT_SEQ: AtomicU32 = AtomicU32::new(1);
static PLACEMENTS: [AtomicU32; MAX_PLOTS] = [const { AtomicU32::new(1) }; MAX_PLOTS];
/// Set once a passthrough query is answered. Signal cleanup has to wrap
/// deletes the same way, and it cannot take a lock to find that out.
static PASSTHROUGH: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_passthrough(on: bool) {
    PASSTHROUGH.store(on, Ordering::Release);
}

fn passthrough_enabled() -> bool {
    PASSTHROUGH.load(Ordering::Acquire)
}

/// 0 unknown, 1 the terminal accepted a frame compose, 2 it refused.
static ANIMATION: AtomicU8 = AtomicU8::new(0);
/// Slots whose latest transmission was a frame edit. An error for one of
/// those is the frame op failing, which drops back to full uploads.
static FRAME_SLOTS: AtomicU32 = AtomicU32::new(0);

#[cfg(test)]
thread_local! {
    static FRAMES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn image_id(slot: usize) -> u32 {
    // Five slot bits support a dense quota overview. A separate prefix
    // keeps these IDs clear of older 16-slot widgets in the same mux.
    0x60000000 | (((unsafe { libc::getpid() } as u32) & 0x1fffff) << 5) | slot as u32
}

/// Not a chart slot. A frame op on this image is the animation probe.
pub(crate) fn probe_id() -> u32 {
    0x20000000 | ((unsafe { libc::getpid() } as u32) & 0x0fff_ffff)
}

pub(crate) fn reset_animation() {
    ANIMATION.store(0, Ordering::Release);
}

pub(crate) fn set_animation(yes: bool) {
    ANIMATION.store(if yes { 1 } else { 2 }, Ordering::Release);
}

pub(crate) fn animation_ready() -> bool {
    ANIMATION.load(Ordering::Acquire) == 1
}

fn frames_ready() -> bool {
    #[cfg(test)]
    {
        return FRAMES.with(|cell| cell.get());
    }
    #[cfg(not(test))]
    {
        animation_ready()
    }
}

fn note_frame(slot: usize) {
    FRAME_SLOTS.fetch_or(1u32 << slot, Ordering::Release);
}

fn note_full(slot: usize) {
    FRAME_SLOTS.fetch_and(!(1u32 << slot), Ordering::Release);
}

pub(crate) fn frame_edit_pending(id: u32) -> bool {
    let slots = FRAME_SLOTS.load(Ordering::Acquire);
    (0..MAX_PLOTS).any(|slot| image_id(slot) == id && slots & (1u32 << slot) != 0)
}

#[cfg(test)]
pub(crate) fn clear_frame_edits() {
    FRAME_SLOTS.store(0, Ordering::Release);
}

/// `compose` and the frame-error replies share `FRAME_SLOTS`. Tests that
/// touch either hold this for the whole test.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub(crate) fn testing_mark_frame(slot: usize) {
    note_frame(slot);
}

/// One-pixel image, a second frame copied from it, then a compose of that
/// frame onto the root. Terminals without animation answer ENOENT or EINVAL.
pub(crate) fn animation_probe() -> String {
    let id = probe_id();
    format!(
        "\x1b_Ga=t,f=24,t=d,s=1,v=1,i={id},q=1;AAAA\x1b\\\
         \x1b_Ga=f,f=24,t=d,s=1,v=1,i={id},c=1,X=1,z=-1,q=1;AAAA\x1b\\\
         \x1b_Ga=c,i={id},r=2,c=1,w=1,h=1,C=1\x1b\\"
    )
}

pub(crate) fn discard_probe() -> String {
    format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", probe_id())
}

pub(crate) fn owns_image(id: u32) -> bool {
    (0..MAX_PLOTS).any(|slot| image_id(slot) == id)
}

fn delete(slot: usize) -> String {
    format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", image_id(slot))
}

/// One placement only, keeping the image data. Each upload gets a fresh
/// placement id, and not every terminal drops the old placement when the
/// image id is re-transmitted, so the replaced frame would stay underneath.
fn delete_placement(slot: usize, placement: u32) -> String {
    format!("\x1b_Ga=d,d=i,i={},p={placement},q=2\x1b\\", image_id(slot))
}

/// Also called by the signal handler: only atomics and write(2), no locks,
/// formatting, allocation or shared references to a mutating image cache.
/// Shared memory is not unlinked here. `shm_unlink` is not async-signal-safe;
/// `cleanup` collects those objects on the ordinary path.
pub(crate) fn cleanup_signal() {
    let live = LIVE.swap(0, Ordering::AcqRel);
    let wrap = PASSTHROUGH.load(Ordering::Acquire);
    for slot in 0..MAX_PLOTS {
        if live & (1 << slot) == 0 {
            continue;
        }
        let mut raw = [0u8; 40];
        let prefix = b"\x1b_Ga=d,d=I,i=";
        raw[..prefix.len()].copy_from_slice(prefix);
        let mut pos = prefix.len();
        let id = image_id(slot);
        let mut divisor = 1_000_000_000;
        while divisor > 1 && id / divisor == 0 {
            divisor /= 10;
        }
        loop {
            raw[pos] = b'0' + ((id / divisor) % 10) as u8;
            pos += 1;
            if divisor == 1 {
                break;
            }
            divisor /= 10;
        }
        let suffix = b",q=2\x1b\\";
        raw[pos..pos + suffix.len()].copy_from_slice(suffix);
        pos += suffix.len();
        let mut out = [0u8; 80];
        let n = if wrap {
            wrap_graphics(&raw[..pos], &mut out)
        } else {
            out[..pos].copy_from_slice(&raw[..pos]);
            pos
        };
        unsafe {
            libc::write(libc::STDOUT_FILENO, out.as_ptr().cast(), n);
        }
    }
}

/// Double every ESC and surround the sequence with tmux's DCS passthrough.
/// The header ends at `tmux;`. Stack only: this runs from the signal handler.
fn wrap_graphics(raw: &[u8], out: &mut [u8]) -> usize {
    let header = b"\x1bPtmux;";
    out[..header.len()].copy_from_slice(header);
    let mut pos = header.len();
    for &byte in raw {
        if byte == 0x1b {
            out[pos] = 0x1b;
            pos += 1;
        }
        out[pos] = byte;
        pos += 1;
    }
    out[pos] = 0x1b;
    out[pos + 1] = b'\\';
    pos + 2
}

pub(crate) fn cleanup() {
    super::shm::release();
    cleanup_signal();
    with_cache(|cache| cache.clear());
}

/// Compose chart regions through core, preserving the sanitizer and header.
/// Returns text plus image deletion/transmission bytes for a single frame.
pub(crate) fn compose(
    rows: &[String],
    plots: &[Plot],
    w: usize,
    h: usize,
    kitty: bool,
    placeholders: bool,
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
        let fits_placeholder = !placeholders || (cols <= super::placeholder::LIMIT
            && plot.visible_rows <= super::placeholder::LIMIT);
        let use_pixels = kitty
            && plot.fill.as_ref().is_none_or(|f| !f.always_text)
            && fits_placeholder
            && images.len() < MAX_PLOTS
            && plot.width.saturating_mul(plot.height) <= super::terminal::max_plot_cells()
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
        // A line chart under the text keeps whatever the widget drew there:
        // an axis value, a rate, the tint. Blanking those cells is what
        // puts the image in a hole beside the label. Direct Kitty leaves
        // the cells alone and places the image at z=-1. A placeholder cell
        // cannot also hold a glyph, so the blanks become placeholders and
        // the glyphs stay. Braille and solid fills still replace the cells.
        let slot = images.len();
        let keep_text = use_pixels && plot.under_text && !placeholders;
        let overlay = use_pixels && plot.under_text && placeholders;
        for row in 0..plot.visible_rows {
            if keep_text {
                continue;
            }
            let content = if overlay {
                placeholders_keeping_text(&text[plot.y + row], plot.x, cols, image_id(slot), row)
            } else if use_pixels && placeholders {
                super::placeholder::placeholder_run(image_id(slot), row, cols)
            } else if use_pixels {
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
            // Source width is kept for geometry; rasterization clips the crop.
            images.push(plot);
        }
    }
    let (before, after) = with_cache(|cache| {
    let wrap = placeholders || passthrough_enabled();
    let mut before = String::new();
    let mut after = String::new();
    // One read for the frame: cache comparison, rasterization and upload
    // geometry must agree, including when a font-size change arrives.
    let (cell_w, cell_h) = super::terminal::cell_size();
    for slot in 0..cache.len().max(images.len()) {
        let plot = images.get(slot);
        let cols = plot.map_or(0, |p| p.width.min(w - p.x));
        if !lost && cache.get(slot).is_some_and(|c| {
            Some(&c.plot) == plot && c.cols == cols && c.cell_size == (cell_w, cell_h)
        }) {
            // The periodic whole frame repairs text another process may
            // have scrolled or covered. The terminal still holds the image,
            // so only its placement is put back, not the pixels.
            if let Some(plot) = images.get(slot).filter(|_| heal) {
                    if placeholders {
                        let rgba = raster_pixels(plot, cols, cell_w, cell_h);
                        before.push_str(&emit_graphics(&placeholder_transmit(plot, slot, w, cell_w, cell_h, &rgba), wrap));
                    } else {
                        after.push_str(&place(plot, slot, w));
                    }
            }
            continue;
        }
        let rgba = plot.map(|p| raster_pixels(p, cols, cell_w, cell_h));
        if !lost {
            if let (Some(cached), Some(plot), Some(rgba)) =
                (cache.get_mut(slot), plot, rgba.as_deref())
            {
                // Time-dependent values can change without changing a pixel.
                // Upload acknowledgements must not start a redraw/upload loop.
                if cached.same_pixels(plot, cols, (cell_w, cell_h), rgba) {
                    // Pixels do not record z. Turning under_text on or off
                    // has to re-place this id, or the stroke stays over the
                    // label until the next heal.
                    let z_changed = cached.plot.under_text != plot.under_text;
                    cached.plot = plot.clone();
                    if placeholders && heal {
                        before.push_str(&emit_graphics(&placeholder_transmit(plot, slot, w, cell_w, cell_h, rgba), wrap));
                    } else if !placeholders && (heal || z_changed) {
                        after.push_str(&place(plot, slot, w));
                    }
                    continue;
                }
            }
        }
        // A one-sample slide edits the held image. Identical pixels already
        // returned above, so this only runs when the raster changed.
        let edit = if !lost && frames_ready() {
            match (cache.get(slot), plot, rgba.as_deref()) {
                (Some(cached), Some(next), Some(bytes))
                    if cached.same_region(next, cols, (cell_w, cell_h)) =>
                    frame_edit(&cached.plot, next, slot, w, (cell_w, cell_h), bytes),
                _ => None,
            }
        } else {
            None
        };
        if let Some(edit) = edit {
            note_frame(slot);
            after.push_str(&edit);
            if let Some(next) = plot {
                if heal {
                    after.push_str(&place(next, slot, w));
                }
                if let Some(previous) = cache.get_mut(slot) {
                    previous.plot = next.clone();
                    previous.cols = cols;
                    previous.cell_size = (cell_w, cell_h);
                    if let Some(bytes) = rgba {
                        previous.rgba = (bytes.len() <= MAX_CACHED_RGBA).then_some(bytes);
                    }
                }
            }
            continue;
        }
        note_full(slot);
        let mut replaced = None;
        if let Some(cached) = cache.get(slot) {
            // Clear the old region before text is repainted when scrolling
            // moves/crops it. Some multiplexers retain old placements on a=T.
            // Updates inside the same region still replace without a blank
            // interval, and identical pixels never reach this path.
            if lost || plot.is_none_or(|p| !cached.same_region(p, cols, (cell_w, cell_h))) {
                before.push_str(&emit_graphics(&delete(slot), wrap));
            } else if !placeholders {
                replaced = Some(PLACEMENTS[slot].load(Ordering::Acquire));
            }
            super::shm::retire_id(image_id(slot));
        }
        if let (Some(plot), Some(rgba)) = (plot, rgba) {
            // Set before writing: a signal during transmission must clean it up.
            LIVE.fetch_or(1 << slot, Ordering::Release);
            if placeholders {
                before.push_str(&emit_graphics(&placeholder_transmit(plot, slot, w, cell_w, cell_h, &rgba), wrap));
            } else {
                after.push_str(&emit_graphics(&image_rgba(plot, slot, w, cell_w, cell_h, &rgba), wrap));
            }
            // image_rgba() places at the default z, above the text. The same
            // placement id is put back underneath, in this write, so a
            // frame never shows the chart covering its labels.
            if plot.under_text && !placeholders {
                after.push_str(&place(plot, slot, w));
            }
            if let Some(old) = replaced {
                after.push_str(&delete_placement(slot, old));
            }
            let cached = CachedPlot {
                plot: plot.clone(),
                cols,
                cell_size: (cell_w, cell_h),
                rgba: (rgba.len() <= MAX_CACHED_RGBA).then_some(rgba),
            };
            if let Some(previous) = cache.get_mut(slot) {
                *previous = cached;
            } else {
                cache.push(cached);
            }
        } else {
            LIVE.fetch_and(!(1 << slot), Ordering::Release);
        }
    }
    cache.truncate(images.len());
    (before, after)
    });
    (text, before, after)
}

/// Placeholder cells for the blanks in an under-text plot. The widget's
/// glyphs stay, with the colour that applied to them. A placeholder paints
/// the image id as the foreground, so the next glyph re-applies its own
/// colour, or a reset when it never had one.
fn placeholders_keeping_text(row: &str, x: usize, cols: usize, id: u32, image_row: usize) -> String {
    let row = super::inert(row);
    let mut out = String::new();
    let mut column = 0usize;
    let mut active = String::new();
    let mut dirty = false;
    let mut after_placeholder = false;
    let mut chars = row.chars().peekable();
    while column < x + cols {
        let Some(c) = chars.next() else {
            break;
        };
        if c == '\x1b' {
            let mut seq = String::from("\x1b");
            for c in chars.by_ref() {
                seq.push(c);
                if c == 'm' {
                    break;
                }
            }
            active.push_str(&seq);
            dirty = true;
            continue;
        }
        let source_placeholder = c == super::placeholder::PLACEHOLDER;
        if source_placeholder {
            while chars
                .peek()
                .copied()
                .is_some_and(super::placeholder::is_placeholder_diacritic)
            {
                chars.next();
            }
        }
        let width = if source_placeholder {
            1
        } else {
            unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
        };
        if width == 0 {
            if (x..x + cols).contains(&column.saturating_sub(1)) && !after_placeholder {
                out.push(c);
            }
            continue;
        }
        if column < x + cols && column + width > x {
            let blank = c == ' ' || source_placeholder;
            // A wide glyph that starts outside the rectangle, or hangs past
            // it, is not one cell we can keep. Those columns become
            // placeholders so the row stays `cols` wide.
            let fits = column >= x && column + width <= x + cols;
            if blank || !fits {
                let start = column.max(x);
                let end = (column + width).min(x + cols);
                for col in start..end {
                    out.push_str(&super::placeholder::placeholder_cell(id, image_row, col - x));
                }
                after_placeholder = true;
                dirty = true;
            } else if dirty {
                if active.is_empty() {
                    if after_placeholder {
                        out.push_str(super::RST);
                    }
                } else {
                    out.push_str(&active);
                }
                dirty = false;
                after_placeholder = false;
                out.push(c);
            } else {
                after_placeholder = false;
                out.push(c);
            }
        }
        column += width;
    }
    while column < x + cols {
        if column >= x {
            out.push_str(&super::placeholder::placeholder_cell(id, image_row, column - x));
        }
        column += 1;
    }
    out
}

fn replace_cells(row: &str, x: usize, width: usize, replacement: &str) -> String {
    let row = super::inert(row);
    let mut out = String::new();
    let mut column = 0;
    let mut inserted = false;
    let mut rest = row.as_str();
    while !rest.is_empty() {
        if rest.as_bytes()[0] == 0x1b {
            // An https OSC 8 has no `m` of its own; stopping at the first
            // one cuts the URL and counts the rest as columns. `markup_len`
            // is the same scan `inert` uses, and it consumes the whole
            // sequence.
            let n = super::markup_len(rest).unwrap_or(1);
            out.push_str(&rest[..n]);
            rest = &rest[n..];
            continue;
        }
        let c = rest.chars().next().unwrap();
        rest = &rest[c.len_utf8()..];
        // A placeholder cell is one column whatever unicode-width says of
        // U+10EEEE, and the diacritics after it are part of that cell.
        let placeholder = c == super::placeholder::PLACEHOLDER;
        let size = if placeholder {
            1
        } else {
            unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
        };
        if column >= x && !inserted {
            out.push_str(replacement);
            out.push_str(super::RST);
            inserted = true;
        }
        let keep = column < x || column >= x + width;
        if keep {
            out.push(c);
        }
        if placeholder {
            while let Some(mark) = rest
                .chars()
                .next()
                .filter(|m| super::placeholder::is_placeholder_diacritic(*m))
            {
                rest = &rest[mark.len_utf8()..];
                if keep {
                    out.push(mark);
                }
            }
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
    let t = lift_for_tint(hue, 0.15);
    (mix_rgb(hue, [255; 3], t), mix_rgb(hue, [255; 3], (t + 0.45).min(1.0)))
}

/// How far toward white `hue` has to go, starting from `from`, to reach AA
/// on the selected-row tint.
fn lift_for_tint(hue: [u8; 3], from: f64) -> f64 {
    let minimum = 4.5 * (luminance([38, 56, 76]) + 0.05) - 0.05;
    let mut t = from;
    while t < 1.0 && luminance(mix_rgb(hue, [255; 3], t)) < minimum {
        t = (t + 0.01_f64).min(1.0);
    }
    t
}

/// `hue` itself when it already reads on the tint, otherwise lifted only as
/// far as it must be.
fn readable_on_tint(hue: [u8; 3], from: f64) -> [u8; 3] {
    mix_rgb(hue, [255; 3], lift_for_tint(hue, from))
}

#[cfg(test)]
thread_local! {
    static ALLOCATED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn alloc_bytes(n: usize) -> Vec<u8> {
    #[cfg(test)]
    ALLOCATED.with(|cell| cell.set(cell.get().saturating_add(n)));
    vec![0u8; n]
}

/// Pixel origin and size of the visible crop. Geometry stays in the full
/// chart; only this rectangle is allocated and stroked. Columns are the
/// left-hand pane clip, rows the scrolled window.
fn crop_px(
    plot: &Plot,
    cols: usize,
    cell_w: usize,
    cell_h: usize,
) -> (usize, usize, usize, usize) {
    let cols = cols.min(plot.width);
    let row0 = plot.first_row.min(plot.height);
    let rows = plot.visible_rows.min(plot.height - row0);
    (0, row0 * cell_h, cols * cell_w, rows * cell_h)
}

#[cfg(test)]
fn pixels(plot: &Plot) -> Vec<u8> {
    raster_pixels(plot, plot.width, CELL_W, CELL_H)
}

fn raster_pixels(plot: &Plot, cols: usize, cell_w: usize, cell_h: usize) -> Vec<u8> {
    let (x0, y0, w, h) = crop_px(plot, cols, cell_w, cell_h);
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let full_w = plot.width * cell_w;
    let full_h = plot.height * cell_h;
    let mut pixels = alloc_bytes(w * h * 4);
    if let Some(fill) = &plot.fill {
        for (index, rect) in fill.rects.iter().enumerate() {
            let colour = rgb(&rect.colour).unwrap_or([180, 200, 220]);
            let end = fill.gradients.iter().find(|(i, _)| *i == index).map(|(_, end)| *end);
            let left = rect.x * cell_w as f64;
            let top = rect.y * cell_h as f64;
            let right = left + rect.width * cell_w as f64;
            let bottom = top + rect.height * cell_h as f64;
            let y_start = (top.floor().max(0.0) as usize).max(y0);
            let y_end = (bottom.ceil() as usize).min(full_h).min(y0 + h);
            let x_start = (left.floor().max(0.0) as usize).max(x0);
            let x_end = (right.ceil() as usize).min(full_w).min(x0 + w);
            for y in y_start..y_end {
                for x in x_start..x_end {
                    let area = (right.min(x as f64 + 1.0) - left.max(x as f64)).max(0.0)
                        * (bottom.min(y as f64 + 1.0) - top.max(y as f64)).max(0.0);
                    let at = ((y - y0) * w + (x - x0)) * 4;
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
    let y_end = y0 + h;
    let x_end = x0 + w;
    for i in order {
        let trace = &plot.chart.traces[i];
        let colour = rgb(&trace.colour).unwrap_or([180, 200, 220]);
        let mut coverage = alloc_bytes(w * h * 4);
        segments(
            &trace.values,
            plot.chart.slots,
            full_w,
            full_h,
            trace.baseline,
            trace.positions.as_deref(),
            |a, b| {
                // Endpoints are full-chart pixels. A segment that stays
                // outside the crop cannot stamp a subpixel inside it.
                if a.1.max(b.1) < y0 || a.1.min(b.1) >= y_end || a.0.min(b.0) >= x_end {
                    return;
                }
                // One-pixel stroke at 2x resolution, then downsample. The
                // stamp can land one subpixel across a crop edge, so the
                // walk stays on the full segment and only the covered
                // subpixels are stored.
                raster((a.0 * 2, a.1 * 2), (b.0 * 2, b.1 * 2), |x, y| {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let sx = x + dx;
                            let sy = y + dy;
                            if sx >= x0 * 2
                                && sx < x_end * 2
                                && sy >= y0 * 2
                                && sy < y_end * 2
                                && sx < full_w * 2
                                && sy < full_h * 2
                            {
                                let lx = sx - x0 * 2;
                                let ly = sy - y0 * 2;
                                coverage[ly * w * 2 + lx] = 1;
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
    // z=-1 is under the glyphs and above cells that have a background.
    // Anything below -1073741824 is also under that background, so an
    // opaque selected-row tint would cover the stroke.
    let z = if plot.under_text { ",z=-1" } else { "" };
    format!(
        "\x1b[{};{}H\x1b_Ga=p,i={},p={},c={},r={},C=1,q=1{z}\x1b\\",
        plot.y + 1,
        plot.x + 1,
        image_id(slot),
        PLACEMENTS[slot].load(Ordering::Acquire),
        plot.width.min(pane_width - plot.x),
        plot.visible_rows,
        z = z,
    )
}

/// zlib level for chart payloads. Level 6 is the measured point: on a
/// scrolled 12-trace chart it is about 30% smaller than level 3, and level
/// 9 saves little more at triple the time. See `docs/terminal-rendering.md`.
const ZLIB_LEVEL: u8 = 6;

fn graphics_key(bytes: &str, key: &str) -> bool {
    bytes.split("\x1b_G").skip(1).any(|chunk| {
        chunk
            .split(';')
            .next()
            .is_some_and(|keys| keys.split(',').any(|item| item == key))
    })
}

fn is_frame_edit(bytes: &str) -> bool {
    graphics_key(bytes, "a=f") || graphics_key(bytes, "a=a") || graphics_key(bytes, "a=c")
}

fn emit_graphics(bytes: &str, wrap: bool) -> String {
    // A shared-memory transmission names a filesystem path. Wrapping it
    // would hand that path to the outer terminal. The direct path may
    // still send `t=s` unwrapped. The rewrite that emits `t=s` on the
    // shared-memory branch runs after compose, so this guard does not see
    // that command. Re-checking it on the rebase is still outstanding.
    let wrap = wrap || (is_frame_edit(bytes) && passthrough_enabled());
    if graphics_key(bytes, "t=s") {
        return if wrap { String::new() } else { bytes.to_string() };
    }
    // Frame edits have to be wrapped while placeholders are on, even when
    // the caller forgot: tmux would otherwise swallow them.
    if wrap {
        super::terminal::tmux_passthrough(bytes)
    } else {
        bytes.to_string()
    }
}

/// A slide of the same chart: same scale, same slot count, N new samples on
/// the right. `None` keeps today's full upload.
fn frame_edit(
    prev: &Plot, next: &Plot, slot: usize, pane_width: usize,
    cell_size: (usize, usize), rgba: &[u8],
) -> Option<String> {
    if prev.fill.is_some()
        || next.fill.is_some()
        || prev.x != next.x
        || prev.y != next.y
        || prev.width != next.width
        || prev.height != next.height
        || prev.first_row != next.first_row
        || prev.visible_rows != next.visible_rows
        || prev.under_text != next.under_text
        || prev.chart.slots != next.chart.slots
        || prev.chart.focus != next.chart.focus
        || prev.chart.traces.len() != next.chart.traces.len()
    {
        return None;
    }
    let cols = next.width.min(pane_width.saturating_sub(next.x));
    // A pane that clips the right edge is a different image width. The new
    // samples sit on the full chart, so a shift of the crop is not one column.
    if cols == 0 || cols != next.width || prev.width != next.width {
        return None;
    }
    let slots = next.chart.slots;
    let (cell_w, cell_h) = cell_size;
    let pixel_w = next.width * cell_w;
    let n = shared_slide(&prev.chart.traces, &next.chart.traces, slots)?;
    let dx = slide_dx(&prev.chart.traces, &next.chart.traces, slots, pixel_w, n)?;
    let img_w = pixel_w;
    let img_h = next.visible_rows * cell_h;
    // The join pixel sits one left of a dx-wide strip. Uploading dx columns
    // would leave that pixel as the old endpoint, missing the new segment.
    let col_w = dx.checked_add(1)?;
    if col_w >= img_w || img_h == 0 || dx == 0 {
        return None;
    }
    let col_x = img_w - col_w;
    if rgba.len() != img_w * img_h * 4 {
        return None;
    }
    let column = column_rgba(&rgba, img_w, col_x, col_w);
    // The sample that just became oldest keeps the tail of the segment that
    // fell off the left. That stroke stays inside the sample's own column, so
    // one pixel there, taken from the new raster, makes the shift match.
    let edge = column_rgba(&rgba, img_w, 0, 1);
    let id = image_id(slot);
    let mut out = format!("\x1b_Ga=d,d=f,i={id},r=2,q=2\x1b\\");
    // Kitty returns EINVAL when a frame is composed onto itself and the
    // rectangles overlap, which a one-slot shift always does. The source is
    // a copy of the root. Its single replaced pixel is in the strip the
    // shift discards, so the copy matches the root where it is read.
    out.push_str(&format!(
        "\x1b_Ga=f,f=32,t=d,s=1,v=1,x=0,y=0,i={id},r=2,c=1,X=1,z=-1,q=1,m=0;AAAAAA==\x1b\\"
    ));
    let copied = img_w - dx;
    out.push_str(&format!(
        "\x1b_Ga=c,i={id},r=2,c=1,X={dx},Y=0,x=0,y=0,w={copied},h={img_h},C=1,q=1\x1b\\"
    ));
    out.push_str(&frame_payload(id, 0, 1, img_h, &edge));
    out.push_str(&frame_payload(id, col_x, col_w, img_h, &column));
    Some(out)
}

enum Slide {
    No,
    Unique(usize),
    /// Every shift that matches. A repeated series has more than one.
    Any(Vec<usize>),
}

fn shared_slide(old: &[Trace], new: &[Trace], slots: usize) -> Option<usize> {
    if old.is_empty() || slots < 2 {
        return None;
    }
    let mut unique = None;
    // Shifts a repeated trace actually matches. The unique shift is used
    // only when it is one of them; otherwise the chart is uploaded whole.
    let mut ambiguous = Vec::new();
    for (prev, next) in old.iter().zip(new) {
        if prev.colour != next.colour || prev.baseline != next.baseline {
            return None;
        }
        if prev.positions.is_some() != next.positions.is_some() {
            return None;
        }
        // Explicit positions plot every sample, not the last `slots`.
        let window = if prev.positions.is_some() {
            prev.values.len().max(next.values.len()).max(slots)
        } else {
            slots
        };
        if !note_slide(
            &mut unique,
            &mut ambiguous,
            value_slide(&prev.values, &next.values, window),
        ) {
            return None;
        }
        if let (Some(old_at), Some(new_at)) = (&prev.positions, &next.positions) {
            let window = old_at.len().max(new_at.len()).max(slots);
            if !note_slide(
                &mut unique,
                &mut ambiguous,
                value_slide_f64(old_at, new_at, window),
            ) {
                return None;
            }
        }
    }
    let n = unique?;
    if ambiguous.iter().any(|candidates| !candidates.contains(&n)) {
        return None;
    }
    Some(n)
}

fn note_slide(unique: &mut Option<usize>, ambiguous: &mut Vec<Vec<usize>>, slide: Slide) -> bool {
    match slide {
        Slide::No => false,
        Slide::Unique(n) => {
            if unique.unwrap_or(n) != n {
                return false;
            }
            *unique = Some(n);
            true
        }
        Slide::Any(candidates) => {
            ambiguous.push(candidates);
            true
        }
    }
}

fn value_slide(old: &[Option<f64>], new: &[Option<f64>], slots: usize) -> Slide {
    let old = &old[old.len().saturating_sub(slots)..];
    let new = &new[new.len().saturating_sub(slots)..];
    if old.is_empty() || new.is_empty() {
        return Slide::No;
    }
    if new.len() > old.len() && new.len() <= slots && old == &new[..old.len()] {
        return Slide::Unique(new.len() - old.len());
    }
    if new.len() != old.len() {
        return Slide::No;
    }
    let found: Vec<usize> = (1..old.len())
        .filter(|&n| new[..old.len() - n] == old[n..])
        .collect();
    match found.as_slice() {
        [] => Slide::No,
        [n] => Slide::Unique(*n),
        _ => Slide::Any(found),
    }
}

fn value_slide_f64(old: &[f64], new: &[f64], slots: usize) -> Slide {
    let old: Vec<_> = old.iter().map(|v| v.is_finite().then_some(*v)).collect();
    let new: Vec<_> = new.iter().map(|v| v.is_finite().then_some(*v)).collect();
    value_slide(&old, &new, slots)
}

fn slide_dx(old: &[Trace], new: &[Trace], slots: usize, pixel_w: usize, n: usize) -> Option<usize> {
    if n == 0 || pixel_w < 2 {
        return None;
    }
    if old.iter().all(|t| t.positions.is_none()) {
        let steps = slots - 1;
        let span = pixel_w - 1;
        if steps == 0 || span % steps != 0 {
            return None;
        }
        return Some(n * (span / steps));
    }
    let mut dx = None;
    for (prev, next) in old.iter().zip(new) {
        let d = if prev.positions.is_none() {
            let steps = slots - 1;
            let span = pixel_w - 1;
            if steps == 0 || span % steps != 0 {
                return None;
            }
            n * (span / steps)
        } else {
            position_dx(prev, next, slots, pixel_w, n)?
        };
        if dx.unwrap_or(d) != d {
            return None;
        }
        dx = Some(d);
    }
    dx.filter(|d| *d > 0)
}

fn position_dx(old: &Trace, new: &Trace, slots: usize, pixel_w: usize, n: usize) -> Option<usize> {
    let old_x = point_xs(old, slots, pixel_w);
    let new_x = point_xs(new, slots, pixel_w);
    if old_x.len() != new_x.len() || n >= old_x.len() {
        return None;
    }
    let mut dx = None;
    for (left, right) in new_x[..old_x.len() - n].iter().zip(&old_x[n..]) {
        match (left, right) {
            (None, None) => {}
            (Some(left), Some(right)) => {
                let d = right.checked_sub(*left)?;
                if d == 0 {
                    return None;
                }
                if *dx.get_or_insert(d) != d {
                    return None;
                }
            }
            _ => return None,
        }
    }
    let dx = dx?;
    let origin = pixel_w.checked_sub(dx + 1)?;
    for x in new_x[old_x.len() - n..].iter().flatten() {
        if *x < origin {
            return None;
        }
    }
    Some(dx)
}

fn point_xs(trace: &Trace, slots: usize, pixel_w: usize) -> Vec<Option<usize>> {
    if slots == 0 || pixel_w == 0 {
        return Vec::new();
    }
    let start = if trace.positions.is_some() {
        0
    } else {
        trace.values.len().saturating_sub(slots)
    };
    let values = &trace.values[start..];
    values
        .iter()
        .enumerate()
        .map(|(i, value)| {
            value.filter(|v| v.is_finite()).and_then(|_| {
                if let Some(positions) = &trace.positions {
                    let x = *positions.get(start + i)?;
                    if !x.is_finite() || !(0.0..=1.0).contains(&x) {
                        return None;
                    }
                    Some((x * (pixel_w - 1) as f64).round() as usize)
                } else {
                    let age = values.len() - 1 - i;
                    Some(
                        pixel_w
                            - 1
                            - ((age as f64 * (pixel_w - 1) as f64)
                                / slots.saturating_sub(1).max(1) as f64)
                                .round() as usize,
                    )
                }
            })
        })
        .collect()
}

fn column_rgba(rgba: &[u8], img_w: usize, col_x: usize, col_w: usize) -> Vec<u8> {
    let row = img_w * 4;
    let h = rgba.len() / row;
    let mut out = Vec::with_capacity(col_w * h * 4);
    for y in 0..h {
        let start = y * row + col_x * 4;
        out.extend_from_slice(&rgba[start..start + col_w * 4]);
    }
    out
}

fn frame_payload(id: u32, x: usize, w: usize, h: usize, raw: &[u8]) -> String {
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(raw, ZLIB_LEVEL);
    let payload = base64::engine::general_purpose::STANDARD.encode(compressed);
    let chunks: Vec<_> = payload.as_bytes().chunks(4096).collect();
    let mut out = String::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = usize::from(i + 1 < chunks.len());
        if i == 0 {
            out.push_str(&format!(
                "\x1b_Ga=f,f=32,t=d,o=z,s={w},v={h},x={x},y=0,i={id},r=1,X=1,q=1,m={more};"
            ));
        } else {
            // Later chunks of a frame still name the action.
            out.push_str(&format!("\x1b_Ga=f,m={more};"));
        }
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        out.push_str("\x1b\\");
    }
    out
}

fn image_rgba(
    plot: &Plot,
    slot: usize,
    pane_width: usize,
    cell_w: usize,
    cell_h: usize,
    rgba: &[u8],
) -> String {
    let cols = plot.width.min(pane_width.saturating_sub(plot.x));
    let (_, _, w, h) = crop_px(plot, cols, cell_w, cell_h);
    // Filled plots are RGB only when the crop is fully opaque. Gaps,
    // fractional edges and the meter inset need alpha, and line charts
    // always do: their strokes are partial coverage.
    let opaque =
        plot.fill.is_some() && !rgba.is_empty() && rgba.chunks_exact(4).all(|px| px[3] == 255);
    let (format, raw) = if opaque {
        let rgb: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|px| [px[0], px[1], px[2]])
            .collect();
        (24, Some(rgb))
    } else {
        (32, None)
    };
    let id = image_id(slot);
    let rows = h / cell_h;
    let placement = loop {
        let n = PLACEMENT_SEQ.fetch_add(1, Ordering::Relaxed);
        if n != 0 {
            break n;
        }
    };
    PLACEMENTS[slot].store(placement, Ordering::Release);
    let mut out = format!("\x1b[{};{}H", plot.y + 1, plot.x + 1);
    let keys = format!(
        "s={w},v={h},i={id},p={placement},c={cols},r={rows},C=1,q=0"
    );
    out.push_str(&transmit_pixels(
        raw.as_deref().unwrap_or(rgba),
        format,
        &keys,
        id,
        placement,
    ));
    out
}

/// Quiet placeholder upload. A virtual placement is not displayed here, so
/// the cursor stays put: moving it would move the outer terminal once tmux
/// passes the sequence through. The bytes stay `t=d`. Shared memory is
/// chosen inside `transmit_pixels`, and wrapping that command is not this path.
fn placeholder_transmit(plot: &Plot, slot: usize, pane_width: usize, cell_w: usize, cell_h: usize, rgba: &[u8]) -> String {
    let cols = plot.width.min(pane_width.saturating_sub(plot.x));
    let (_, _, w, h) = crop_px(plot, cols, cell_w, cell_h);
    let opaque =
        plot.fill.is_some() && !rgba.is_empty() && rgba.chunks_exact(4).all(|px| px[3] == 255);
    let (format, raw) = if opaque {
        let rgb: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|px| [px[0], px[1], px[2]])
            .collect();
        (24, rgb)
    } else {
        (32, rgba.to_vec())
    };
    let id = image_id(slot);
    let rows = h / cell_h;
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&raw, ZLIB_LEVEL);
    let payload = base64::engine::general_purpose::STANDARD.encode(compressed);
    let mut out = String::new();
    let chunks: Vec<_> = payload.as_bytes().chunks(4096).collect();
    // q=2 suppresses the response a multiplexer would have to ignore.
    // U=1 is the virtual placement the placeholder cells refer to.
    for (i, chunk) in chunks.iter().enumerate() {
        let more = usize::from(i + 1 < chunks.len());
        if i == 0 {
            out.push_str(&format!(
                "\x1b_Ga=T,f={format},t=d,o=z,s={w},v={h},i={id},p=1,c={cols},r={rows},C=1,q=2,U=1,m={more};",
            ));
        } else {
            out.push_str(&format!("\x1b_Gm={more};"));
        }
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        out.push_str("\x1b\\");
    }
    out
}

// Below 16 KiB, object creation can cost more than compression. Larger
// images avoid zlib/base64 work on both sides when the local probe succeeded.
const MIN_SHARED_BYTES: usize = 16 * 1024;

pub(crate) fn transmit_pixels(pixels: &[u8], format: u8, keys: &str, id: u32, placement: u32) -> String {
    // A local terminal can map these pixels directly. Choose the transport
    // before compression and base64 framing, rather than decoding a direct
    // escape back into another buffer only to put it in shared memory.
    if pixels.len() >= MIN_SHARED_BYTES {
        if let Some(command) = super::shm::publish_pixels(keys, pixels, id, placement, format) {
            return command;
        }
    }
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(pixels, ZLIB_LEVEL);
    let payload = base64::engine::general_purpose::STANDARD.encode(compressed);
    super::shm::mark_direct(id, placement);
    let mut out = String::new();
    let chunks: Vec<_> = payload.as_bytes().chunks(4096).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = usize::from(i + 1 < chunks.len());
        if i == 0 {
            out.push_str(&format!("\x1b_Ga=T,f={format},t=d,o=z,{keys},m={more};"));
        } else {
            out.push_str(&format!("\x1b_Gm={more};"));
        }
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        out.push_str("\x1b\\");
    }
    out
}

/// Direct Kitty transmission. Production draws through `image_rgba`; tests
/// keep this name for the path that places at the cursor.
#[cfg(test)]
fn image(plot: &Plot, slot: usize, pane_width: usize, cell_w: usize, cell_h: usize) -> String {
    let cols = plot.width.min(pane_width.saturating_sub(plot.x));
    image_rgba(plot, slot, pane_width, cell_w, cell_h, &raster_pixels(plot, cols, cell_w, cell_h))
}

/// Render the same chart through either transport for pixel and CPU checks.
#[cfg(test)]
pub(crate) fn direct_chart_command(cols: usize, rows: usize) -> String {
    let values: Vec<_> = (0..48).map(|i| Some(i as f64 / 47.0)).collect();
    let chart = LineChart {
        slots: values.len(),
        traces: vec![Trace {
            positions: None,
            values,
            colour: super::rgb(50, 220, 170),
            baseline: None,
        }],
        focus: None,
    };
    image(&Plot::new(2, 1, cols, rows, chart), 3, cols + 4, CELL_W, CELL_H)
}

#[cfg(test)]
pub(crate) fn cropped_chart_command(plot: &Plot, pane_width: usize) -> String {
    image(plot, 3, pane_width, CELL_W, CELL_H)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meter_between_chosen_stops_keeps_them_unless_they_would_not_read() {
        let floor = 4.5 * (luminance([38, 56, 76]) + 0.05) - 0.05;
        // Readable already: kept exactly, not washed toward white.
        assert_eq!(readable_on_tint([60, 180, 166], 0.0), [60, 180, 166]);
        // Perplexity's own brand teal is too dark for the tint: lifted to it.
        let lifted = readable_on_tint([32, 128, 141], 0.0);
        assert_ne!(lifted, [32, 128, 141]);
        assert!(luminance(lifted) >= floor, "{lifted:?}");
        let plot = Plot::gradient_meter_between(0, 0, 10, 0.5, None, [32, 128, 141], [112, 214, 198], "", "");
        let fill = plot.fill.as_ref().unwrap();
        assert_eq!(fill.gradients, vec![(1, [112, 214, 198])]);
    }
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

    fn rows_of(full: &[u8], plot: &Plot, cropped: &Plot) -> Vec<u8> {
        let row = plot.width * CELL_W * 4;
        let y0 = cropped.first_row * CELL_H;
        let y1 = y0 + cropped.visible_rows * CELL_H;
        (y0..y1)
            .flat_map(|y| full[y * row..(y + 1) * row].iter().copied())
            .collect()
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
                let eighths = (finite_fraction(fraction) * width as f64 * 8.0).round() as usize;
                assert_eq!(text.iter().filter(|(_, t)| t == "█").count(), eighths / 8);
                assert_eq!(
                    text.iter().filter(|(_, t)| LEFT_EIGHTHS[1..8].iter().any(|g| t == &g.to_string())).count(),
                    usize::from(eighths % 8 > 0),
                );
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
    fn meters_and_stacked_bars_are_text_in_kitty_mode() {
        let _transport = super::super::shm::guard();
        let _cache = compose_lock();
        let hue = super::super::rgb(120, 196, 250);
        let other = super::super::rgb(250, 180, 90);
        let track = super::super::rgb(60, 70, 80);
        let mark = format!("{}{}", super::super::bg(10, 12, 18), super::super::rgb(238, 244, 252));
        let parts = [(0.3, hue.clone()), (0.7, other.clone())];
        let plots = [
            Plot::meter(1, 1, 20, 0.43, Some(0.6), &hue, &track, &mark),
            Plot::ramped_meter(1, 2, 20, 0.43, Some(0.6), &[hue.clone(), other.clone()], &track, &mark),
            Plot::gradient_meter(1, 3, 20, 0.43, Some(0.6), &hue, &track, &mark),
            Plot::gradient_meter_between(1, 4, 20, 0.43, None, [40, 90, 200], [120, 200, 255], &track, &mark),
            Plot::stacked_bar(1, 5, 20, &parts),
            Plot::gradient_stacked_bar(1, 6, 20, &parts),
        ];
        let rows = vec![String::new(); 8];
        for plot in &plots {
            for (lost, heal) in [(true, false), (false, false), (false, true)] {
                let (text, before, after) =
                    compose(&rows, std::slice::from_ref(plot), 30, 8, true, false, lost, heal);
                assert!(!before.contains("\x1b_G") && !after.contains("\x1b_G"), "{plot:?}");
                let (plain, _, _) = compose(&rows, std::slice::from_ref(plot), 30, 8, false, false, false, false);
                assert_eq!(text, plain, "Kitty and text mode draw the same cells");
            }
            // Inside tmux too: no placeholder cells for a bar.
            let (text, before, after) =
                compose(&rows, std::slice::from_ref(plot), 30, 8, true, true, true, true);
            assert!(!before.contains("\x1b_G") && !after.contains("\x1b_G"));
            assert!(!text.concat().contains('\u{10EEEE}'));
        }
        // 43% of twenty cells is 8.6: eight whole cells and five eighths.
        let row: String = plots[0].text_row(0).iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(row, "████████▋░░░┃░░░░░░░");
        // 30% of twenty is six cells exactly; the boundary is one colour each.
        let row: String = plots[4].text_row(0).iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(row, "█".repeat(20));
        assert_eq!(plots[4].text_row(0)[5].0, hue);
        assert_eq!(plots[4].text_row(0)[6].0, other);
    }

    #[test]
    fn a_stacked_boundary_inside_a_cell_is_the_left_share_on_the_right_colour() {
        let a = super::super::rgb(120, 196, 250);
        let b = super::super::rgb(250, 180, 90);
        // 0.33 of ten cells is 3.3: three cells, then two eighths of a.
        let plot = Plot::stacked_bar(0, 1, 10, &[(0.33, a.clone()), (0.67, b.clone())]);
        let cells = plot.text_row(0);
        let glyphs: String = cells.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(glyphs, "███▎██████");
        assert_eq!(cells[3].0, format!("{a}{}", super::super::bg(250, 180, 90)));
        assert!(cells[4].0.starts_with(super::super::NOBG), "the next cell clears the background");
        assert_eq!(plot.fill.as_ref().unwrap().row(0, 10).matches(super::super::NOBG).count(), 1);
        // A boundary in the last cell still ends its background before any label.
        let plot = Plot::stacked_bar(0, 1, 10, &[(0.95, a.clone()), (0.05, b.clone())]);
        assert!(plot.fill.as_ref().unwrap().row(0, 10).ends_with(super::super::NOBG));
        // Colours that are not truecolour cannot be painted behind: the larger share wins.
        let plot = Plot::stacked_bar(0, 1, 10, &[(0.33, "\x1b[31m".into()), (0.67, "\x1b[32m".into())]);
        assert_eq!(plot.text_row(0)[3], ("\x1b[32m".to_string(), "█".to_string()));
        // Three categories in one cell: the middle, largest one takes it,
        // rather than the outer two splitting it between them.
        let c = super::super::rgb(90, 120, 250);
        let plot = Plot::stacked_bar(0, 1, 1, &[(0.125, a.clone()), (0.75, b.clone()), (0.125, c.clone())]);
        assert_eq!(plot.text_row(0)[0], (b.clone(), "█".to_string()));
        // An empty category between two others is not a third share: the
        // cell is still three eighths of a on c.
        let plot = Plot::stacked_bar(0, 1, 1, &[(0.375, a.clone()), (0.0, b.clone()), (0.625, c)]);
        assert_eq!(plot.text_row(0)[0], (format!("{a}{}", super::super::bg(90, 120, 250)), "▍".to_string()));
    }

    #[test]
    fn a_sub_cell_fill_needs_a_colour_or_the_pixel_bar_stays_empty() {
        let fill = super::super::rgb(10, 20, 30);
        let track = super::super::rgb(80, 100, 120);
        let mark = super::super::rgb(240, 245, 250);
        // 1% of eight cells is about two thirds of an eighth, so the text
        // bar paints one eighth when it has a colour and stays an empty
        // track when the ramp is empty.
        let sliver = Plot::ramped_meter(0, 0, 8, 0.01, None, &[fill], &track, &mark);
        let blank = Plot::ramped_meter(0, 0, 8, 0.01, None, &[], &track, &mark);
        assert_ne!(pixels(&sliver), pixels(&blank));
        let row = sliver.fill.as_ref().unwrap().row(0, 8);
        assert!(!row.contains('█') && row.contains('▏'), "{row:?}");
        assert_eq!(row.matches('░').count(), 7, "{row:?}");
        let row = blank.fill.as_ref().unwrap().row(0, 8);
        assert!(!row.contains('█') && !row.contains('▏'), "{row:?}");
        assert_eq!(row.matches('░').count(), 8, "{row:?}");
    }

    #[test]
    fn filled_plots_crop_without_changing_values_or_labels() {
        let colour = super::super::rgb(80, 220, 170);
        let p = Plot::bars(4, 3, &[(0.2, colour.clone()), (1.0, colour)], 4, 1.0, false);
        let clipped = p.clone().in_viewport(4, 1, 2).unwrap();
        let full = pixels(&p);
        let cropped = pixels(&clipped);
        assert_eq!(cropped, rows_of(&full, &p, &clipped));
        assert_eq!(cropped.len() * p.height, full.len() * clipped.visible_rows);
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
        let _transport = super::super::shm::guard();
        let p = Plot::new(8, 1, 20, 5, chart(vec![Some(0.0), Some(1.0)]));
        let command = image(&p, 0, 80, CELL_W, CELL_H);
        assert!(command.contains("f=32,t=d,o=z,s=160,v=80"));
        assert!(command.contains("c=20,r=5,C=1,q=0"));
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
    fn an_https_link_does_not_move_the_cells_a_plot_replaces() {
        // The URL contains `m`. A scan that stops there treats the rest of
        // the target as columns, so the plot lands early and splits the
        // escape. The four dots are still the cells that get replaced.
        let visible = "axis│....│unit";
        let linked = super::super::link("https://example.com/opscope", visible);
        let replaced = replace_cells(&linked, 5, 4, "plot");
        assert_eq!(super::super::strip_escapes(&replaced), "axis│plot│unit");
        assert!(
            replaced.contains("\x1b]8;;https://example.com/opscope\x1b\\"),
            "the opener was split: {replaced:?}"
        );
        assert!(replaced.contains("\x1b]8;;\x1b\\"), "{replaced:?}");
    }

    #[test]
    fn an_idle_run_stays_blank_but_activity_reaches_the_baseline() {
        let mut c = chart(vec![Some(0.0); 5]);
        c.traces[0].baseline = Some(0.0);
        assert!(c.cells(10, 3).iter().flatten().all(|(_, mask)| *mask == 0));
        assert!(
            pixels(&Plot::new(0, 1, 10, 3, c.clone()))
                .iter()
                .all(|b| *b == 0)
        );
        c.traces[0].values[2] = Some(1.0);
        assert!(c.cells(10, 3)[0].iter().any(|(_, mask)| *mask != 0));
        assert!(c.cells(10, 3)[2].iter().any(|(_, mask)| *mask != 0));
    }

    #[test]
    fn image_cache_ignores_subpixel_changes_but_updates_visible_changes() {
        let _transport = super::super::shm::guard();
        let rows = vec!["title".into()];
        for width in [57, 120] {
            let meter = |level| Plot::new(3, 2, width - 6, 3, chart(vec![Some(0.4), Some(level)]));
            let first = meter(0.501);
            let next = meter(0.501 + 1e-12);
            assert_ne!(first, next);
            assert_eq!(raster_pixels(&first, width - 6, CELL_W, CELL_H), raster_pixels(&next, width - 6, CELL_W, CELL_H));
            let (_, _, after) = compose(&rows, &[first], width, 8, true, false, true, false);
            assert!(after.contains("a=T"));
            let placement = PLACEMENTS[0].load(Ordering::Acquire);
            let (_, before, after) =
                compose(&rows, std::slice::from_ref(&next), width, 8, true, false, false, false);
            assert!(before.is_empty() && after.is_empty());
            assert_eq!(PLACEMENTS[0].load(Ordering::Acquire), placement);
            let (_, before, after) = compose(&rows, &[meter(0.501 + 2e-12)], width, 8, true, false, false, true);
            assert!(before.is_empty() && after.contains("a=p") && !after.contains("a=T"));
            let (_, before, after) = compose(&rows, &[meter(0.75)], width, 8, true, false, false, false);
            assert!(before.is_empty() && after.contains("a=T"));
            assert_ne!(PLACEMENTS[0].load(Ordering::Acquire), placement);
            let mut moved = meter(0.75);
            moved.x += 1;
            let (_, before, after) = compose(&rows, &[moved], width, 8, true, false, false, false);
            assert!(before.contains("a=d") && after.contains("a=T"));
            let (text, _, _) = compose(&rows, &[next], width, 8, false, false, false, false);
            let (same_text, _, _) = compose(&rows, &[meter(0.501)], width, 8, false, false, false, false);
            assert_eq!(text, same_text);
        }
        cleanup();
    }

    #[test]
    fn image_scroll_crops_clear_the_previous_region_before_replacement() {
        let _transport = super::super::shm::guard();
        let base = Plot::new(2, 3, 12, 8, chart(vec![Some(0.2), Some(0.8)]));
        let rows = vec!["protected title".into()];
        for scroll in [0, 1, 3, 4, 6, 0] {
            let original = base.clone().in_viewport(0, 1, 10).unwrap();
            compose(&rows, &[original], 40, 12, true, false, true, false);
            let next = base.clone().in_viewport(scroll, 1, 10).unwrap();
            let (_, before, after) = compose(&rows, std::slice::from_ref(&next), 40, 12, true, false, false, false);
            if scroll == 0 {
                assert!(before.is_empty() && after.is_empty());
            } else {
                assert!(before.contains("a=d,d=I"), "scroll {scroll} left the old region alive");
                assert!(after.contains("a=T"));
            }
            // A steady frame does not delete or upload anything.
            let (_, before, after) = compose(&rows, &[next], 40, 12, true, false, false, false);
            assert!(before.is_empty() && after.is_empty());
        }
        cleanup();
    }

    #[test]
    fn a_changed_chart_removes_the_placement_it_replaced() {
        // Every upload gets a fresh placement id, so a terminal that keeps
        // placements across a re-transmit showed each earlier frame through
        // the transparent part of the new one (OPS-168).
        let _transport = super::super::shm::guard();
        let colour = super::super::rgb(240, 160, 110);
        let rows = vec!["title".into()];
        let bars = |v: f64| Plot::bars(2, 1, &[(v, colour.clone()), (1.0, colour.clone())], 3, 1.0, false);
        let (_, _, after) = compose(&rows, &[bars(0.9)], 40, 8, true, false, true, false);
        assert!(after.contains("a=T") && !after.contains("a=d"));
        let first = PLACEMENTS[0].load(Ordering::Acquire);
        let (_, before, after) = compose(&rows, &[bars(0.2)], 40, 8, true, false, false, false);
        let second = PLACEMENTS[0].load(Ordering::Acquire);
        assert_ne!(first, second);
        // The new frame goes up before the old one comes down, so nothing
        // blinks, and only that one placement goes: the image stays.
        assert!(before.is_empty());
        let removal = delete_placement(0, first);
        let upload = after.find("a=T").unwrap();
        assert!(removal.contains(",d=i,") && removal.contains(&format!(",p={first},")));
        assert!(after.find(&removal).is_some_and(|at| at > upload));
        assert!(!after.contains(&format!(",p={second},q=2")));
        // A moved chart leaves nothing at its old place either.
        let mut moved = bars(0.2);
        moved.y += 1;
        let (_, before, after) = compose(&rows, &[moved], 40, 8, true, false, false, false);
        assert!(before.contains(&delete(0)));
        assert!(after.contains("a=T"));
        cleanup();
    }

    #[test]
    fn pixel_cache_compares_line_crops_even_without_a_retained_buffer() {
        let p = Plot::new(2, 1, 12, 4, chart(vec![Some(0.2), Some(0.8)]));
        let mut next = p.clone();
        next.chart.traces[0].values[0] = Some(0.2 + 1e-12);
        let rgba = raster_pixels(&p, 12, CELL_W, CELL_H);
        let next_rgba = raster_pixels(&next, 12, CELL_W, CELL_H);
        assert_ne!(p, next);
        for buffer in [Some(rgba), None] {
            let cached = CachedPlot { plot: p.clone(), cols: 12, cell_size: (CELL_W, CELL_H), rgba: buffer };
            assert!(cached.same_pixels(&next, 12, (CELL_W, CELL_H), &next_rgba));
            assert!(!cached.same_pixels(&next, 11, (CELL_W, CELL_H), &next_rgba));
            assert!(!cached.same_pixels(&next, 12, (20, 40), &next_rgba));
            let mut moved = next.clone();
            moved.y += 1;
            assert!(!cached.same_pixels(&moved, 12, (CELL_W, CELL_H), &next_rgba));
        }
    }

    #[test]
    fn image_cache_reuses_unchanged_plots_and_deletes_on_fallback_or_screen_change() {
        let _transport = super::super::shm::guard();
        let _cache = compose_lock();
        let p = Plot::new(2, 1, 10, 3, chart(vec![Some(0.2), Some(0.8)]));
        let rows = vec!["protected title".into(), "axis".into()];
        let (_, _, bytes) = compose(&rows, &[p.clone()], 40, 8, true, false, true, false);
        assert!(bytes.contains("a=T"));
        let (_, before, after) = compose(&rows, &[p.clone()], 40, 8, true, false, false, false);
        assert!(before.is_empty() && after.is_empty());
        // The periodic whole frame re-places the held image, never resends it.
        let (_, before, after) = compose(&rows, &[p.clone()], 40, 8, true, false, false, true);
        assert!(before.is_empty() && after.contains("a=p") && !after.contains("a=T"));
        let (_, before, after) = compose(&rows, &[p.clone()], 40, 8, true, false, true, false);
        assert!(before.contains("a=d") && after.contains("a=T"));
        let (text, before, after) = compose(&rows, &[p.clone()], 40, 8, false, false, false, false);
        assert!(before.contains("a=d") && after.is_empty());
        assert!(
            text.iter()
                .any(|row| row.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)))
        );
        let mut header_plot = p.clone();
        header_plot.y = 0;
        let (text, _, after) = compose(&rows, &[header_plot], 40, 8, true, false, false, false);
        assert_eq!(text[0], rows[0]);
        assert!(after.is_empty());
        compose(&rows, &[p], 40, 8, true, false, false, false);
        let (_, before, after) = compose(&rows, &[], 40, 8, true, false, false, false);
        assert!(before.contains("a=d") && after.is_empty());
        // A quota overview can have more than sixteen visible bars. Keep
        // every one in Kitty and clean up the highest slot as well.
        let colour = super::super::rgb(120, 196, 250);
        let plots: Vec<_> = (0..32).map(|i| Plot::heatmap_levels(
            2, i + 1, &[vec![Some((colour.clone(), 4)); 4]], &colour,
        )).collect();
        let rows = vec!["          ".into(); 34];
        let (text, _, after) = compose(&rows, &plots, 10, 34, true, false, true, false);
        assert_eq!(after.matches("a=T").count(), 32);
        assert!(!text.iter().any(|row| row.contains('█') || row.contains('░')));
        let ids: std::collections::HashSet<_> = (0..32).map(image_id).collect();
        assert_eq!(ids.len(), 32);
        assert_eq!(LIVE.load(Ordering::Acquire), u32::MAX);
        let (_, before, after) = compose(&rows, &[], 10, 34, true, false, false, false);
        assert_eq!(before.matches("a=d").count(), 32);
        assert!(after.is_empty());
        assert_eq!(LIVE.load(Ordering::Acquire), 0);
    }

    #[test]
    fn an_under_text_change_replaces_the_placement_without_resending() {
        let _transport = super::super::shm::guard();
        let rows = vec!["title".into(), "keep".into()];
        let plain = Plot::new(2, 1, 8, 2, chart(vec![Some(0.2), Some(0.8)]));
        let (_, _, first) = compose(&rows, &[plain.clone()], 40, 6, true, false, true, false);
        assert!(first.contains("a=T"));
        assert!(graphics_keys(&first).iter().all(|keys| !keys.contains("z=")));
        let over = plain.clone().under_text();
        let (_, _, placed) = compose(&rows, &[over.clone()], 40, 6, true, false, false, false);
        assert!(!placed.contains("a=T"), "{placed}");
        assert!(placed.contains("a=p") && placed.contains("z=-1"), "{placed}");
        let (_, _, quiet) = compose(&rows, &[over.clone()], 40, 6, true, false, false, false);
        assert!(quiet.is_empty(), "{quiet}");
        let (_, _, back) = compose(&rows, &[plain], 40, 6, true, false, false, false);
        assert!(!back.contains("a=T"), "{back}");
        assert!(back.contains("a=p") && !back.contains("z="), "{back}");
        let (_, before, after) = compose(&rows, &[], 40, 6, true, false, false, false);
        assert!(before.contains("a=d") && after.is_empty());
    }

    #[test]
    fn multi_chunk_images_round_trip_with_exact_cropped_pixels() {
        let _transport = super::super::shm::guard();
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
        let base = Plot::new(3, 2, 100, 10, c);
        let p = base.clone().in_viewport(3, 1, 5).unwrap();
        let command = image(&p, 0, 70, CELL_W, CELL_H);
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
        let full = pixels(&base);
        let visible = rows_of(&full, &base, &p);
        assert_eq!(pixels(&p), visible);
        let expected: Vec<_> = visible
            .chunks(base.width * CELL_W * 4)
            .flat_map(|row| row[..67 * CELL_W * 4].iter().copied())
            .collect();
        assert_eq!(decoded, expected);
        assert!(command.contains("f=32,"));
        assert!(command.contains(&format!("v={}", p.visible_rows * CELL_H)));
    }

    #[test]
    fn a_scrolled_chart_allocates_the_visible_rows() {
        let mut c = chart(Vec::new());
        c.slots = 100;
        c.traces = (0..12)
            .map(|i| Trace {
                positions: None,
                values: (0..100)
                    .map(|n| Some(((n * (i + 3)) % 17) as f64 / 16.0))
                    .collect(),
                colour: super::super::rgb(40 + (i as u8) * 12, 80 + (i as u8) * 8, 160),
                baseline: None,
            })
            .collect();
        let p = Plot::new(0, 0, 100, 10, c);
        ALLOCATED.with(|cell| cell.set(0));
        let full = pixels(&p);
        let full_bytes = ALLOCATED.with(|cell| cell.get());
        let scrolled = p.clone().in_viewport(5, 0, 5).unwrap();
        assert_eq!((scrolled.height, scrolled.visible_rows), (10, 5));
        ALLOCATED.with(|cell| cell.set(0));
        let cropped = pixels(&scrolled);
        let cropped_bytes = ALLOCATED.with(|cell| cell.get());
        assert_eq!(cropped, rows_of(&full, &p, &scrolled));
        assert_eq!(cropped.len() * 2, full.len());
        // One raster plus one coverage buffer per trace, both the crop.
        assert_eq!(cropped_bytes * p.height, full_bytes * scrolled.visible_rows);
        assert_eq!(cropped_bytes, cropped.len() * (1 + p.chart.traces.len()));
        let pane = raster_pixels(&scrolled, 40, CELL_W, CELL_H);
        assert_eq!(pane.len(), scrolled.visible_rows * CELL_H * 40 * CELL_W * 4);
        let row = 100 * CELL_W * 4;
        let cols = 40 * CELL_W * 4;
        let expected: Vec<u8> = cropped
            .chunks(row)
            .flat_map(|line| line[..cols].iter().copied())
            .collect();
        assert_eq!(pane, expected);
    }

    #[test]
    fn opaque_fills_are_rgb_and_anything_with_a_gap_stays_rgba() {
        let _transport = super::super::shm::guard();
        let colour = super::super::rgb(90, 210, 140);
        let solid = Plot::heatmap_levels(0, 0, &[vec![Some((colour.clone(), 4)); 4]], &colour);
        let command = image(&solid, 0, 80, CELL_W, CELL_H);
        assert!(command.contains("f=24,t=d,o=z,s=32,v=16"));
        let (format, decoded) = decode_image(&command);
        assert_eq!(format, 24);
        let rgba = pixels(&solid);
        let rgb: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|px| {
                assert_eq!(px[3], 255);
                px[..3].iter().copied()
            })
            .collect();
        assert_eq!(decoded, rgb);
        let gapped = Plot::heatmap(0, 0, &[vec![None, Some(colour.clone())]], &colour);
        let command = image(&gapped, 0, 80, CELL_W, CELL_H);
        assert!(command.contains("f=32,"));
        let (_, decoded) = decode_image(&command);
        assert_eq!(decoded, pixels(&gapped));
        let line = Plot::new(0, 0, 8, 2, chart(vec![Some(0.2), Some(0.8)]));
        assert!(image(&line, 0, 80, CELL_W, CELL_H).contains("f=32,"));
    }

    #[test]
    fn line_row_is_the_terminal_row_of_the_stroke() {
        // 20% of a four-row chart. Cell rounding lands on row 2.
        assert_eq!(((1.0 - 0.2) * 3.0_f64).round() as usize, 2);
        assert_eq!(line_row(0.2, 4), 3);
        assert_eq!(inked_rows(&chart(vec![Some(0.2); 4]), 4), vec![3]);
        // Inverted 20% is the fraction 0.8, and cell rounding lands on row 1.
        assert_eq!((0.2_f64 * 3.0).round() as usize, 1);
        assert_eq!(line_row(0.8, 4), 0);
        assert_eq!(inked_rows(&chart(vec![Some(0.8); 4]), 4), vec![0]);
        assert_eq!(line_row(0.5, 1), 0);
        assert_eq!(line_row(0.5, 0), 0);
        assert_eq!(line_col(0.0, 4), 0);
        assert_eq!(line_col(1.0, 4), 3);
        assert_eq!(line_col(0.5, 0), 0);
        let (cell_w, _) = super::super::terminal::cell_size();
        let width = 4 * cell_w.max(1);
        let x = (0.2_f64 * (width - 1) as f64).round() as usize;
        assert_eq!(line_col(0.2, 4), x / cell_w.max(1));
    }

    fn inked_rows(chart: &LineChart, rows: usize) -> Vec<usize> {
        let plot = Plot::new(0, 0, 4, rows, chart.clone());
        let bytes = pixels(&plot);
        let w = 4 * CELL_W;
        let mut hit = Vec::new();
        for row in 0..rows {
            let ink = (0..CELL_H).any(|y| {
                (0..w).any(|x| bytes[((row * CELL_H + y) * w + x) * 4 + 3] != 0)
            });
            if ink {
                hit.push(row);
            }
        }
        hit
    }

    #[test]
    fn under_text_keeps_cell_text_in_pixel_mode_and_matches_braille() {
        // compose() writes the shared image cache. Without the guard this
        // races the other cache tests and fails most of the time.
        let _transport = super::super::shm::guard();
        let rows = vec![
            "title".into(),
            "ab12.3ms later".into(),
            "abaxis values".into(),
        ];
        let chart = chart(vec![Some(0.2), Some(0.8)]);
        let plain = Plot::new(2, 1, 8, 2, chart);
        let over = plain.clone().under_text();
        assert!(over.under_text);
        let (braille, _, _) = compose(&rows, &[plain], 40, 6, false, false, true, false);
        let (same, _, quiet) = compose(&rows, &[over.clone()], 40, 6, false, false, true, false);
        assert_eq!(braille, same);
        assert!(quiet.is_empty());
        assert!(braille.iter().any(|row| row.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c))));
        assert!(!braille[1].contains("12.3ms"));
        let (text, _, after) = compose(&rows, &[over], 40, 6, true, false, true, false);
        assert!(text[1].contains("12.3ms"), "{:?}", text[1]);
        assert!(text[2].contains("axis"));
        assert!(after.contains("a=T"));
        assert!(graphics_keys(&after).iter().any(|keys| keys.contains("z=-1")));
        let colour = super::super::rgb(30, 220, 180);
        let cols = vec![(1.0, colour)];
        let bars = Plot::bars(0, 1, &cols, 1, 1.0, false).under_text();
        assert!(!bars.under_text, "a solid bar is not a line chart");
        let bar_rows = vec![String::new(), "keep".into()];
        let (text, _, after) = compose(&bar_rows, &[bars], 20, 4, true, false, true, false);
        assert!(!text[1].contains("keep"));
        assert!(graphics_keys(&after).iter().all(|keys| !keys.contains("z=")));
        compose(&rows, &[], 40, 6, true, false, true, false);
    }

    #[test]
    fn placeholders_keep_under_text_readings() {
        let _transport = super::super::shm::guard();
        // Four blanks, then the reading. The plot starts on the blanks.
        let plain = "ab    12.3ms later".to_string();
        let rows = vec!["title".into(), plain.clone(), "abaxis values".into()];
        let chart = chart(vec![Some(0.2), Some(0.8)]);
        let over = Plot::new(2, 1, 10, 2, chart.clone()).under_text();
        compose(&rows, &[], 40, 6, true, true, true, false);
        let (text, before, after) = compose(&rows, &[over], 40, 6, true, true, true, false);
        assert!(text[1].contains("12.3ms"), "{:?}", text[1]);
        assert!(text[1].contains('\u{10EEEE}'), "{:?}", text[1]);
        assert!(text[2].contains("axis") && text[2].contains("value"), "{:?}", text[2]);
        let reading = text[1].find("12.3ms").unwrap();
        let cell = text[1][..reading].rfind('\u{10EEEE}').unwrap();
        let reset = text[1][..reading].rfind("\x1b[0m").unwrap();
        assert!(cell < reset && reset < reading, "{:?}", text[1]);
        assert_eq!(super::super::display_width(&text[1]), plain.chars().count());
        assert!(before.contains("q=2,U=1"), "{before:?}");
        assert!(!before.contains("a=p") && !after.contains("a=p"), "{before:?} {after:?}");
        // The reading's own colour is put back after the image-id colour.
        // Row 0 is the pinned title; a plot there is not drawn.
        let colour = super::super::rgb(30, 220, 180);
        let coloured = format!("ab    {colour}12.3ms");
        let rows = vec!["title".into(), coloured];
        let over = Plot::new(2, 1, 10, 1, chart.clone()).under_text();
        compose(&rows, &[], 40, 4, true, true, true, false);
        let (text, _, _) = compose(&rows, &[over], 40, 4, true, true, true, false);
        let reading = text[1].find("12.3ms").unwrap();
        let paint = text[1][..reading].rfind(&colour).unwrap();
        let cell = text[1][..reading].rfind('\u{10EEEE}').unwrap();
        assert!(cell < paint && paint < reading, "{:?}", text[1]);
        // A chart that did not ask to keep its text still becomes placeholders.
        let bare = Plot::new(2, 1, 10, 1, chart);
        compose(&rows, &[], 40, 4, true, true, true, false);
        let (text, _, _) = compose(&rows, &[bare], 40, 4, true, true, true, false);
        assert!(!text[1].contains("12.3ms"), "{:?}", text[1]);
        compose(&rows, &[], 40, 4, true, true, true, false);
    }

    fn graphics_keys(bytes: &str) -> Vec<&str> {
        bytes
            .split("\x1b_G")
            .skip(1)
            .filter_map(|chunk| chunk.split(';').next())
            .collect()
    }

    #[test]
    fn placeholders_are_one_cell_and_direct_placement_stays() {
        let _transport = super::super::shm::guard();
        let rows = vec![
            "title".into(),
            "ab12.3ms later".into(),
            "abaxis values".into(),
        ];
        let plot = Plot::new(2, 1, 8, 2, chart(vec![Some(0.2), Some(0.8)]));
        compose(&rows, &[], 40, 6, true, true, true, false);
        let (text, before, after) = compose(&rows, &[plot.clone()], 40, 6, true, true, true, false);
        assert!(text[1].contains('\u{10EEEE}'), "{:?}", text[1]);
        assert!(before.contains("q=2,U=1"), "{before:?}");
        assert!(before.contains("a=T"), "{before:?}");
        assert!(before.contains("\x1bPtmux;"));
        assert!(!before.contains("a=p"), "{before:?}");
        assert!(
            !before.contains("\x1b[2;3H"),
            "a virtual placement must not move the cursor: {before:?}"
        );
        assert!(!after.contains("a=T"), "{after:?}");
        let frame = format!("{before}{}{after}", text.concat());
        let upload = frame.find("q=2,U=1").expect("upload");
        let cell = frame.find('\u{10EEEE}').expect("placeholder");
        assert!(upload < cell, "the virtual placement must precede its cells");
        // A second transmission deletes the previous image before the new one.
        let (text, before, after) = compose(&rows, &[plot.clone()], 40, 6, true, true, true, false);
        let frame = format!("{before}{}{after}", text.concat());
        let delete_at = frame.find("a=d").expect("delete");
        let upload = frame.find("q=2,U=1").expect("upload");
        let cell = frame.find('\u{10EEEE}').expect("placeholder");
        assert!(delete_at < upload && upload < cell, "{frame:?}");
        let cell = crate::placeholder::placeholder_cell(42, 0, 0);
        let mixed = format!("ab{cell}cd");
        assert_eq!(super::super::display_width(&mixed), 5);
        let replaced = replace_cells(&mixed, 2, 1, "Z");
        let z = replaced.find('Z').expect("replacement");
        let tail = replaced.find("cd").expect("tail");
        assert!(z < tail, "{replaced:?}");
        assert!(!replaced.contains('\u{10EEEE}'), "{replaced:?}");
        assert_eq!(super::super::display_width(&replaced), 5);
        // The whole frame resends the quiet upload. A q=1 probe would be
        // typed into the pane when tmux does not forward the APC reply.
        let (text, before, again) = compose(&rows, &[plot.clone()], 40, 6, true, true, false, true);
        assert!(before.contains("q=2,U=1") && before.contains("a=T"), "{before:?}");
        assert!(!before.contains("a=p") && !again.contains("a=p"), "{before:?} {again:?}");
        assert!(!before.contains("q=1") && !again.contains("q=1"), "{before:?} {again:?}");
        let frame = format!("{before}{}{again}", text.concat());
        let upload = frame.find("q=2,U=1").expect("whole-frame upload");
        let cell = frame.find('\u{10EEEE}').expect("placeholder");
        assert!(upload < cell, "the resent image must precede its cells");
        // A frame that is not the whole frame does not resend a static chart.
        let (_, quiet, still) = compose(&rows, &[plot.clone()], 40, 6, true, true, false, false);
        assert!(quiet.is_empty() && still.is_empty(), "{quiet:?} {still:?}");
        let (direct, _, placed) = compose(&rows, &[plot], 40, 6, true, false, true, false);
        assert!(!direct.iter().any(|row| row.contains('\u{10EEEE}')));
        assert!(placed.contains("a=T"));
        assert!(!placed.contains("U=1"));
        compose(&rows, &[], 40, 6, true, false, true, false);
    }

    #[test]
    fn passthrough_wraps_frame_edits_and_leaves_shared_memory_unwrapped() {
        let _transport = super::super::shm::guard();
        struct ResetPassthrough;
        impl Drop for ResetPassthrough {
            fn drop(&mut self) {
                set_passthrough(false);
            }
        }
        let _reset = ResetPassthrough;
        set_passthrough(false);
        let direct = emit_graphics("\x1b_Ga=T,t=d,i=1,q=1\x1b\\", false);
        assert_eq!(direct, "\x1b_Ga=T,t=d,i=1,q=1\x1b\\");
        let shared = emit_graphics("\x1b_Ga=T,t=s,i=1,q=1\x1b\\", false);
        assert_eq!(shared, "\x1b_Ga=T,t=s,i=1,q=1\x1b\\");
        assert!(emit_graphics("\x1b_Ga=T,t=s,i=1,q=1\x1b\\", true).is_empty());
        // The payload can contain the letters of a key. Only the keys count.
        let payload = emit_graphics("\x1b_Ga=T,t=d,q=1;abcd t=s xyz\x1b\\", true);
        assert!(payload.starts_with("\x1bPtmux;"), "{payload:?}");
        assert!(payload.contains("t=s"));
        set_passthrough(true);
        for action in ["a=f", "a=a", "a=c"] {
            let raw = format!("\x1b_G{action},i=1,q=1\x1b\\");
            let wrapped = emit_graphics(&raw, false);
            assert!(wrapped.starts_with("\x1bPtmux;"), "{action}: {wrapped:?}");
            assert!(wrapped.contains(action), "{action}: {wrapped:?}");
        }
        let shared = emit_graphics("\x1b_Ga=T,t=s,i=1,q=1\x1b\\", false);
        assert_eq!(shared, "\x1b_Ga=T,t=s,i=1,q=1\x1b\\");
        assert!(emit_graphics("\x1b_Ga=T,t=s,i=1,q=1\x1b\\", true).is_empty());
        let upload = emit_graphics("\x1b_Ga=T,t=d,i=1,q=1\x1b\\", true);
        assert!(upload.starts_with("\x1bPtmux;"), "{upload:?}");
        set_passthrough(false);
    }

    #[test]
    fn signal_cleanup_wraps_deletes_the_same_way_as_the_query() {
        let raw = b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\";
        let mut out = [0u8; 80];
        let n = wrap_graphics(raw, &mut out);
        let wrapped = super::super::terminal::tmux_passthrough(std::str::from_utf8(raw).unwrap());
        assert_eq!(&out[..n], wrapped.as_bytes());
    }

    fn decode_image(command: &str) -> (u8, Vec<u8>) {
        let format = if command.contains("f=24,") { 24 } else { 32 };
        let mut payload = String::new();
        for chunk in command.split("\x1b_G").skip(1) {
            let (_, data) = chunk.split_once(';').unwrap();
            payload.push_str(data.trim_end_matches("\x1b\\"));
        }
        let encoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        (
            format,
            miniz_oxide::inflate::decompress_to_vec_zlib(&encoded).unwrap(),
        )
    }

    #[test]
    #[ignore = "requires an OPSCOPE_CAPTURED_IMAGE fixture from a live widget"]
    fn measure_captured_compression_levels() {
        let path = std::env::var("OPSCOPE_CAPTURED_IMAGE").expect("captured image JSON path");
        let sample: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let pixels = base64::engine::general_purpose::STANDARD
            .decode(sample["rgba"].as_str().unwrap()).unwrap();
        let w = sample["width"].as_u64().unwrap();
        let h = sample["height"].as_u64().unwrap();
        assert_eq!(pixels.len() as u64, w * h * 4);
        for level in [3, 6, 9] {
            let started = std::time::Instant::now();
            let mut bytes = 0;
            for _ in 0..20 {
                let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&pixels, level);
                bytes = compressed.len();
                std::hint::black_box(compressed);
            }
            eprintln!("captured {w}x{h} RGBA raw={} level={level} bytes={bytes} mean={:?}",
                pixels.len(), started.elapsed() / 20);
        }
    }

    /// `compose` keeps one process-wide image cache. Tests that drive it
    /// hold this for the whole test so a parallel run cannot swap the cache
    /// between two of their calls.
    fn compose_lock() -> std::sync::MutexGuard<'static, ()> {
        super::test_lock()
    }

    /// Turns frames on for this thread and off again, including on panic.
    struct FramesOn;
    impl FramesOn {
        fn new() -> Self {
            FRAMES.with(|cell| cell.set(true));
            Self
        }
    }
    impl Drop for FramesOn {
        fn drop(&mut self) {
            FRAMES.with(|cell| cell.set(false));
        }
    }

    fn slide(values: Vec<Option<f64>>, extra: Option<f64>) -> (Plot, Plot) {
        let old = Plot::new(2, 1, 8, 3, chart(values.clone()));
        let mut next = values;
        next.remove(0);
        next.push(extra);
        let new = Plot::new(2, 1, 8, 3, chart(next));
        (old, new)
    }

    fn assert_pixels_match(got: &[u8], want: &[u8]) {
        assert_eq!(got.len(), want.len());
        let row = 64 * 4;
        for (i, (a, b)) in got.iter().zip(want).enumerate() {
            if a != b {
                panic!(
                    "pixel mismatch at x={} y={} byte {}: got {a} want {b}",
                    (i % row) / 4,
                    i / row,
                    i % 4
                );
            }
        }
    }

    fn spliced(old: &[u8], new: &[u8], img_w: usize, dx: usize) -> Vec<u8> {
        let col_w = dx + 1;
        let col_x = img_w - col_w;
        let row = img_w * 4;
        let h = new.len() / row;
        let mut out = vec![0u8; new.len()];
        for y in 0..h {
            let row_at = y * row;
            out[row_at..row_at + 4].copy_from_slice(&new[row_at..row_at + 4]);
            for x in 1..col_x {
                let src = (y * img_w + x + dx) * 4;
                let dst = (y * img_w + x) * 4;
                out[dst..dst + 4].copy_from_slice(&old[src..src + 4]);
            }
            let at = (y * img_w + col_x) * 4;
            out[at..at + col_w * 4].copy_from_slice(&new[at..at + col_w * 4]);
        }
        out
    }

    fn trace_of(values: &[f64]) -> Trace {
        Trace {
            positions: None,
            values: values.iter().copied().map(Some).collect(),
            colour: super::super::rgb(30, 220, 180),
            baseline: None,
        }
    }

    #[test]
    fn an_ambiguous_trace_does_not_borrow_another_traces_shift() {
        // Eight slots. The repeated trace matches shifts of two, four and
        // six samples, not one. A sibling that uniquely slid by one must
        // not drag it along: a nine-pixel shift is not that trace.
        let repeating = [0.2, 0.8, 0.2, 0.8, 0.2, 0.8, 0.2, 0.8];
        let repeating_next = [0.2, 0.8, 0.2, 0.8, 0.2, 0.8, 0.3, 0.4];
        let unique: Vec<f64> = (1..=8).map(|i| i as f64 / 10.0).collect();
        let mut unique_next = unique[1..].to_vec();
        unique_next.push(0.9);
        assert_eq!(
            shared_slide(
                &[trace_of(&repeating), trace_of(&unique)],
                &[trace_of(&repeating_next), trace_of(&unique_next)],
                8,
            ),
            None
        );
        let old = Plot::new(2, 1, 8, 3, chart(repeating.into_iter().map(Some).collect()));
        let new = Plot::new(
            2,
            1,
            8,
            3,
            chart(repeating_next.into_iter().map(Some).collect()),
        );
        assert!(spliced(&pixels(&old), &pixels(&new), 64, 9) != pixels(&new));

        // [0, 1, 0, 1, 0, 1] → [0, 1, 0, 1, 0.2, 0.3] matches two and four, not three.
        let periodic = [0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
        let periodic_next = [0.0, 1.0, 0.0, 1.0, 0.2, 0.3];
        let six: Vec<f64> = (1..=6).map(|i| i as f64 / 10.0).collect();
        let six_next = vec![0.4, 0.5, 0.6, 0.7, 0.8, 0.9];
        assert_eq!(
            shared_slide(
                &[trace_of(&periodic), trace_of(&six)],
                &[trace_of(&periodic_next), trace_of(&six_next)],
                6,
            ),
            None
        );

        // A constant series matches every shift, including the unique one.
        let flat = [0.5; 8];
        assert_eq!(
            shared_slide(
                &[trace_of(&flat), trace_of(&unique)],
                &[trace_of(&flat), trace_of(&unique_next)],
                8,
            ),
            Some(1)
        );

        // Positions keep their own candidate shifts. These match two and
        // four, while the values uniquely match one.
        let mut old_pos = trace_of(&unique);
        let mut new_pos = trace_of(&unique_next);
        old_pos.positions = Some(repeating.to_vec());
        new_pos.positions = Some(repeating_next.to_vec());
        assert_eq!(shared_slide(&[old_pos], &[new_pos], 8), None);
    }

    #[test]
    fn an_ambiguous_chart_is_uploaded_whole_and_a_constant_trace_may_slide() {
        let _transport = super::super::shm::guard();
        let _cache = compose_lock();
        let _frames = FramesOn::new();
        let rows = vec!["protected title".into(), "axis".into()];
        let repeating = [0.2, 0.8, 0.2, 0.8, 0.2, 0.8, 0.2, 0.8];
        let repeating_next = [0.2, 0.8, 0.2, 0.8, 0.2, 0.8, 0.3, 0.4];
        let unique: Vec<f64> = (1..=8).map(|i| i as f64 / 10.0).collect();
        let mut unique_next = unique[1..].to_vec();
        unique_next.push(0.9);
        let old = Plot::new(2, 1, 8, 3, chart_of(&[&repeating[..], &unique]));
        let new = Plot::new(
            2,
            1,
            8,
            3,
            chart_of(&[&repeating_next[..], unique_next.as_slice()]),
        );
        compose(&rows, &[old], 40, 8, true, false, true, false);
        let (_, _, bytes) = compose(&rows, &[new], 40, 8, true, false, false, false);
        assert!(bytes.contains("a=T") && !bytes.contains("a=c"));
        compose(&rows, &[], 40, 8, true, false, false, false);

        let flat = [0.5; 8];
        let old = Plot::new(2, 1, 8, 3, chart_of(&[&flat[..], &unique]));
        let new = Plot::new(2, 1, 8, 3, chart_of(&[&flat[..], unique_next.as_slice()]));
        compose(&rows, &[old], 40, 8, true, false, true, false);
        let (_, before, after) = compose(&rows, &[new], 40, 8, true, false, false, false);
        assert!(
            before.is_empty()
                && after.contains("a=c")
                && after.contains("a=f")
                && !after.contains("a=T")
        );
        compose(&rows, &[], 40, 8, true, false, false, false);
    }

    fn chart_of(traces: &[&[f64]]) -> LineChart {
        let mut chart = chart(traces[0].iter().copied().map(Some).collect());
        for values in &traces[1..] {
            chart.traces.push(trace_of(values));
        }
        chart
    }

    #[test]
    fn a_one_sample_shift_matches_a_full_raster() {
        // 8 cells, 8 slots: (64 - 1) / 7 = 9, a whole number of pixels.
        let samples = vec![
            Some(0.2),
            Some(0.5),
            None,
            Some(0.8),
            Some(0.15),
            Some(0.6),
            Some(0.35),
            Some(0.9),
        ];
        let (old, new) = slide(samples, Some(0.4));
        let dx = slide_dx(&old.chart.traces, &new.chart.traces, 8, 64, 1).unwrap();
        assert_eq!(dx, 9);
        assert_pixels_match(
            &spliced(&pixels(&old), &pixels(&new), 64, dx),
            &pixels(&new),
        );

        let mut flat = chart(vec![Some(0.5); 8]);
        flat.traces[0].baseline = Some(0.0);
        let old = Plot::new(0, 1, 8, 3, flat.clone());
        let mut next = flat;
        next.traces[0].values.remove(0);
        next.traces[0].values.push(Some(0.7));
        let new = Plot::new(0, 1, 8, 3, next);
        let dx = slide_dx(&old.chart.traces, &new.chart.traces, 8, 64, 1).unwrap();
        assert_eq!(dx, 9);
        assert_pixels_match(
            &spliced(&pixels(&old), &pixels(&new), 64, dx),
            &pixels(&new),
        );
    }

    #[test]
    fn frame_edits_match_full_rasters_at_reported_cell_sizes() {
        let old = Plot::new(2, 1, 8, 3, chart(vec![Some(0.2), Some(0.6), Some(0.3), Some(0.5)]));
        let new = Plot::new(2, 1, 8, 3, chart(vec![Some(0.6), Some(0.3), Some(0.5), Some(0.9)]));
        for cell in [(8, 16), (20, 40)] {
            let w = new.width * cell.0;
            let h = new.visible_rows * cell.1;
            let before = raster_pixels(&old, old.width, cell.0, cell.1);
            let after = raster_pixels(&new, new.width, cell.0, cell.1);
            let dx = (w - 1) / 3;
            let edit = frame_edit(&old, &new, 0, 40, cell, &after).expect("whole-pixel slide");
            assert!(edit.contains(&format!("w={},h={h}", w - dx)));
            assert_eq!(frame_column(&edit, 0), column_rgba(&after, w, 0, 1));
            assert_eq!(frame_column(&edit, w - dx - 1), column_rgba(&after, w, w - dx - 1, dx + 1));
            assert_eq!(spliced(&before, &after, w, dx), after);
        }
        let mut moved = new.clone();
        moved.y += 1;
        assert!(frame_edit(&old, &moved, 0, 40, (8, 16), &pixels(&moved)).is_none());
        assert!(frame_edit(&old, &new.clone().under_text(), 0, 40, (8, 16), &pixels(&new)).is_none());
    }

    #[test]
    fn a_sample_tick_sends_a_frame_edit_and_a_rescale_sends_the_image() {
        let _transport = super::super::shm::guard();
        let _cache = compose_lock();
        let _frames = FramesOn::new();
        let rows = vec!["protected title".into(), "axis".into()];
        let (old, new) = slide(
            vec![
                Some(0.2),
                Some(0.5),
                Some(0.1),
                Some(0.8),
                Some(0.4),
                Some(0.6),
                Some(0.3),
                Some(0.9),
            ],
            Some(0.15),
        );
        // Until the probe succeeds, a tick is still a full upload.
        FRAMES.with(|cell| cell.set(false));
        let (_, _, bytes) = compose(&rows, &[old.clone()], 40, 8, true, false, true, false);
        assert!(bytes.contains("a=T") && !bytes.contains("a=c"));
        let (_, _, bytes) = compose(&rows, &[new.clone()], 40, 8, true, false, false, false);
        assert!(bytes.contains("a=T") && !bytes.contains("a=c"));
        compose(&rows, &[], 40, 8, true, false, false, false);

        FRAMES.with(|cell| cell.set(true));
        let (_, _, bytes) = compose(&rows, &[old.clone()], 40, 8, true, false, true, false);
        assert!(bytes.contains("a=T"));
        let (_, before, after) = compose(&rows, &[new.clone()], 40, 8, true, false, false, false);
        assert!(
            before.is_empty()
                && after.contains("a=c")
                && after.contains("a=f")
                && !after.contains("a=T"),
            "tick should edit the held image"
        );
        assert!(after.contains("r=1"), "the column edits the root frame");
        let rgba = pixels(&new);
        assert_eq!(frame_column(&after, 0), column_rgba(&rgba, 64, 0, 1));
        assert_eq!(frame_column(&after, 54), column_rgba(&rgba, 64, 54, 10));

        let mut scaled = new.clone();
        scaled.chart.traces[0].values[3] = Some(0.05);
        let (_, before, after) = compose(&rows, &[scaled], 40, 8, true, false, false, false);
        // A replacement upload does not delete the image first. That gap is
        // what an acknowledgement would paint over while the new pixels arrive.
        assert!(before.is_empty() && after.contains("a=T") && !after.contains("a=c"));

        let (old, new) = slide(
            vec![
                Some(0.2),
                Some(0.5),
                Some(0.1),
                Some(0.8),
                Some(0.4),
                Some(0.6),
                Some(0.3),
                Some(0.9),
            ],
            Some(0.15),
        );
        compose(&rows, &[old], 40, 8, true, false, true, false);
        let (_, before, after) = compose(&rows, &[new], 40, 8, true, false, true, false);
        assert!(before.contains("a=d") && after.contains("a=T") && !after.contains("a=c"));
        compose(&rows, &[], 40, 8, true, false, false, false);
    }

    #[test]
    fn netwatch_bytes_drop_only_when_the_slot_pitch_is_whole_pixels() {
        let _transport = super::super::shm::guard();
        let _cache = compose_lock();
        let _frames = FramesOn::new();
        let rows = vec!["title".into()];
        // 64 cells is one netwatch trace on an 80-column pane. A full window
        // is two samples per cell, and (512 - 1) is not divisible by 127.
        let steady = netwatch_plot(64, 4, 128);
        let steady_next = slid_plot(&steady);
        compose(&rows, &[steady], 80, 12, true, false, true, false);
        let (_, _, today) = compose(&rows, &[steady_next], 80, 12, true, false, false, false);
        assert!(today.contains("a=T") && !today.contains("a=c"));
        compose(&rows, &[], 80, 12, true, false, false, false);

        // 74 samples on that same width: (512 - 1) / 73 = 7.
        let early = netwatch_plot(64, 4, 74);
        let early_next = slid_plot(&early);
        let (_, _, full) = compose(&rows, &[early], 80, 12, true, false, true, false);
        let full_bytes = full.len();
        let (_, _, edit) = compose(&rows, &[early_next], 80, 12, true, false, false, false);
        assert!(edit.contains("a=c") && edit.contains("a=f") && !edit.contains("a=T"));
        assert!(edit.len() < full_bytes);
        compose(&rows, &[], 80, 12, true, false, false, false);
    }

    fn netwatch_plot(width: usize, height: usize, slots: usize) -> Plot {
        // Prime modulus wider than the window, so the one-sample slide is the
        // only match. A short period also matches a longer shift and the
        // frame path refuses to guess.
        let values: Vec<_> = (0..slots)
            .map(|i| Some((i as f64 * 37.0 % 251.0) / 250.0))
            .collect();
        let mut plot = chart(values);
        plot.traces[0].baseline = Some(0.0);
        Plot::new(12, 2, width, height, plot)
    }

    fn slid_plot(plot: &Plot) -> Plot {
        let mut next = plot.clone();
        next.chart.traces[0].values.remove(0);
        next.chart.traces[0].values.push(Some(0.42));
        next
    }

    fn frame_column(command: &str, x: usize) -> Vec<u8> {
        let mut payload = String::new();
        let mut taking = false;
        let origin = format!("x={x}");
        for chunk in command.split("\x1b_G").skip(1) {
            let (keys, data) = chunk
                .split_once(';')
                .unwrap_or((chunk.trim_end_matches("\x1b\\"), ""));
            let data = data.trim_end_matches("\x1b\\");
            if keys.contains("a=f")
                && keys.contains("r=1")
                && keys.split(',').any(|key| key == origin)
            {
                taking = true;
            } else if taking && !keys.contains("a=f") {
                break;
            }
            if taking {
                payload.push_str(data);
                if keys.contains("m=0") {
                    break;
                }
            }
        }
        let encoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        miniz_oxide::inflate::decompress_to_vec_zlib(&encoded).unwrap()
    }
}
