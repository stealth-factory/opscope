// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! What the pane draws: the process table and one process's detail.
//!
//! Both screens are built at whatever height they need, with the title as
//! their first row; `main` pins the title and windows the rest. Every row
//! is built for the width it is given and no wider - a command that does
//! not fit wraps onto the lines under it rather than being cut.

use std::time::Duration;

use opscope_core as tc;

use super::model::{self, Clock, Facts, Io, MemSource, PerCore, Proc, Sample, Sort, Thread};
use super::parse;
use super::services::{self, Service};

pub struct Palette {
    pub txt: String,
    pub dim: String,
    pub dim_lit: String,
    pub accent: String,
    pub down: String,
    pub bad: String,
    /// Memory's three parts, in the bar and in its legend.
    pub used: String,
    pub reclaimable: String,
    pub free: String,
    /// A meter's empty track: decoration, never text.
    pub track: String,
}

pub fn palette() -> Palette {
    Palette {
        txt: tc::ink(tc::Role::Fg),
        // The plain grey is under AA on the selected-row tint; its lifted
        // twin is what a tinted row reaches for.
        dim: tc::ink(tc::Role::Muted),
        dim_lit: tc::ink_on(tc::Role::Muted, true),
        accent: tc::ink(tc::Role::Accent),
        down: tc::ink(tc::Role::Orange),
        bad: tc::ink(tc::Role::Red),
        used: tc::ink(tc::Role::Magenta),
        reclaimable: tc::ink(tc::Role::Blue),
        free: tc::ink(tc::Role::Green),
        track: tc::ink(tc::Role::Dim),
    }
}

impl Palette {
    /// `colour` as drawn on the selected-row tint: each role in its lifted
    /// form. A colour that is not one of these (the heat ramp, already
    /// asked for in its tinted form) comes back as it went in.
    pub fn lit(&self, colour: &str) -> String {
        if colour == self.dim {
            return self.dim_lit.clone();
        }
        for (plain, role) in [
            (&self.txt, tc::Role::Fg),
            (&self.accent, tc::Role::Accent),
            (&self.down, tc::Role::Orange),
            (&self.bad, tc::Role::Red),
        ] {
            if colour == plain.as_str() {
                return tc::ink_on(role, true);
            }
        }
        colour.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    List,
    Detail,
    /// One service and its processes.
    Service,
}

/// The process the detail screen is about: its pid, its identity, and the
/// last row the table had for it, kept so a process that exits leaves its
/// last reading on screen rather than a blank.
#[derive(Clone, Debug)]
pub struct Opened {
    pub pid: i32,
    pub start: Option<u64>,
    pub last: Proc,
    pub gone: bool,
    /// The last facts the poller read for it, kept for the same reason.
    pub facts: Option<Facts>,
}

pub struct View<'a> {
    pub sample: Option<&'a Sample>,
    pub err: &'a str,
    pub age: Option<Duration>,
    pub every: f64,
    pub sort: Sort,
    pub needle: &'a str,
    pub typing: bool,
    pub all_users: bool,
    pub show_kernel: bool,
    /// `[t]`: threads listed under their processes (Linux).
    pub threads: bool,
    /// The pid the list cursor is on.
    pub cursor: Option<i32>,
    pub opened: Option<&'a Opened>,
    pub svc_sort: services::Sort,
    /// The cgroup path the services cursor is on.
    pub svc_cursor: Option<&'a str>,
    /// The service whose processes are open.
    pub svc_opened: Option<&'a OpenedService>,
    /// Its `memory.max`, as last read.
    pub svc_max: Option<&'a Result<Option<u64>, String>>,
}

/// The service the detail is about, and the last row the view had for it.
#[derive(Clone, Debug)]
pub struct OpenedService {
    pub last: Service,
    /// The cgroup was not in the latest walk.
    pub gone: bool,
}

/// One frame's body: the rows, where each list item landed (row, item),
/// how many rows are pinned, and the pids in the order drawn.
pub struct Drawn {
    pub rows: Vec<String>,
    pub placed: Vec<(usize, usize)>,
    /// Rows pinned whatever the pane's height: the title.
    pub head: usize,
    /// Rows pinned when the pane leaves the list room under them (see
    /// [`pin_head`]): on the list, everything down to the table's column
    /// header. The detail screens pin only their title, so this is `head`.
    pub pin: usize,
    pub keys: Vec<i32>,
    /// The services view's cgroup paths in the order drawn; `keys` is then
    /// empty.
    pub paths: Vec<String>,
    /// The panel's bars, meters and trace, at their row in `rows`; `main`
    /// crops them to the window with `in_viewport`.
    pub plots: Vec<tc::Plot>,
}

// ── numbers as the columns print them ────────────────────────────────

/// Bytes in at most five characters: `1.3G`, `640M`, `12K`, `900B`.
pub fn human(n: u64) -> String {
    const UNITS: &[(u64, &str)] = &[
        (1 << 60, "E"),
        (1 << 50, "P"),
        (1 << 40, "T"),
        (1 << 30, "G"),
        (1 << 20, "M"),
        (1 << 10, "K"),
    ];
    for &(unit, name) in UNITS {
        if n >= unit {
            let whole = n / unit;
            let tenths = (n % unit) * 10 / unit;
            if whole < 10 && tenths > 0 {
                return format!("{whole}.{tenths}{name}");
            }
            return format!("{whole}{name}");
        }
    }
    format!("{n}B")
}

/// A number of seconds as a window: `2s`, `0.5s`, `2.1s`.
pub fn secs(s: f64) -> String {
    let tenths = (s * 10.0).round() / 10.0;
    if (tenths - tenths.round()).abs() < 1e-9 {
        format!("{}s", tenths.round() as i64)
    } else {
        format!("{tenths:.1}s")
    }
}

pub fn cpu_cell(cpu: Option<f64>) -> String {
    match cpu {
        None => "—".into(),
        Some(c) => format!("{:.1}", c.clamp(0.0, 999.9)),
    }
}

fn mem_cell(mem: Option<f64>) -> String {
    match mem {
        None => "—".into(),
        Some(m) if m >= 99.95 => "100%".into(),
        Some(m) => format!("{m:.1}%"),
    }
}

/// What the CGROUP column names: the unit, or - for a login session or
/// the root, which name no unit - the last part of the path.
pub fn cgroup_cell(path: &str) -> String {
    let unit = tc::procfs::unit_name(path);
    if !unit.is_empty() {
        return unit;
    }
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if last.is_empty() {
        "/".into()
    } else {
        last.into()
    }
}

fn state_word(state: char) -> &'static str {
    match state {
        'R' => "running",
        'S' => "sleeping",
        'D' => "waiting on disk, in an uninterruptible sleep",
        'Z' => "zombie: exited, not yet collected by its parent",
        'T' => "stopped",
        't' => "stopped by a debugger",
        'I' => "idle kernel thread",
        'X' => "dead",
        _ => "",
    }
}

/// `1h 02m 03s`, for cumulative CPU time.
fn duration(s: f64) -> String {
    let total = s.max(0.0).round() as u64;
    let (h, m, sec) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}h {m:02}m {sec:02}s")
    } else if m > 0 {
        format!("{m}m {sec:02}s")
    } else {
        format!("{:.2}s", s.max(0.0))
    }
}

/// `text` in pieces of at most `room` cells, every character kept.
pub fn chunks(text: &str, room: usize) -> Vec<String> {
    let room = room.max(1);
    let mut pieces = Vec::new();
    let mut piece = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let cw = tc::display_width(ch.encode_utf8(&mut [0u8; 4]));
        if used + cw > room && !piece.is_empty() {
            pieces.push(std::mem::take(&mut piece));
            used = 0;
        }
        piece.push(ch);
        used += cw;
    }
    pieces.push(piece);
    pieces
}

/// Clauses joined with ` · `, broken between clauses onto as many lines
/// as the width needs; a clause wider than a line is broken at spaces.
pub fn pack_clauses(clauses: &[String], width: usize) -> Vec<String> {
    let room = width.saturating_sub(1).max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for clause in clauses {
        let sep = if line.is_empty() { 0 } else { 3 };
        if !line.is_empty() && tc::display_width(&line) + sep + tc::display_width(clause) > room {
            lines.push(std::mem::take(&mut line));
        }
        if tc::display_width(clause) > room {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            lines.extend(tc::wrap_words(clause, room));
            continue;
        }
        if !line.is_empty() {
            line.push_str(" · ");
        }
        line.push_str(clause);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines.into_iter().map(|l| format!(" {l}")).collect()
}

/// The %CPU clause: what the figure is a share of and over what.
pub fn cpu_label(clock: Clock, cores: usize, every: f64) -> Vec<String> {
    let cores = if cores == 1 {
        "1 core".to_string()
    } else {
        format!("all {cores} cores")
    };
    match clock {
        Clock::Measured { window: Some(w) } => {
            vec![format!("%CPU of {cores} over the last {}", secs(w))]
        }
        Clock::Measured { window: None } => {
            vec![format!(
                "%CPU of {cores} from the next sample, in {}",
                secs(every)
            )]
        }
        Clock::Decaying => vec![
            format!(
                "%CPU = macOS decaying average (ps), not the last {}",
                secs(every)
            ),
            format!("of {cores}"),
        ],
    }
}

// ── the panel: CPU, MEMORY, SWAP and load ───────────────────────────

/// One row as coloured pieces, measured by their text alone.
type Line = Vec<(String, String)>;

fn line_width(line: &Line) -> usize {
    line.iter().map(|(_, t)| tc::display_width(t)).sum()
}

/// The panel above the table: its rows, and the plots in them at their
/// row within the panel.
#[derive(Clone, Debug, Default)]
pub struct Panel {
    pub rows: Vec<String>,
    pub plots: Vec<tc::Plot>,
}

/// Rows and plots built for one width, the plots at their row in `lines`.
#[derive(Default)]
struct Block {
    lines: Vec<Line>,
    plots: Vec<tc::Plot>,
}

impl Block {
    fn push(&mut self, line: Line) {
        self.lines.push(line);
    }

    /// `other` under this one.
    fn stack(&mut self, other: Block) {
        let y = self.lines.len();
        self.lines.extend(other.lines);
        self.plots.extend(other.plots.into_iter().map(|mut plot| {
            plot.y += y;
            plot
        }));
    }

    /// `right` beside this one, starting at column `at`.
    fn beside(mut self, right: Block, at: usize) -> Block {
        let rows = self.lines.len().max(right.lines.len());
        self.lines.resize(rows, Vec::new());
        for (line, extra) in self.lines.iter_mut().zip(right.lines) {
            let used = line_width(line);
            if used < at {
                line.push((String::new(), " ".repeat(at - used)));
            }
            line.extend(extra);
        }
        self.plots.extend(right.plots.into_iter().map(|mut plot| {
            plot.x += at;
            plot
        }));
        self
    }
}

const LABEL: usize = 8;
/// Rows a per-core bar stands in: twenty-four eighths of a cell, so a step
/// of about four percent shows.
const BAR_ROWS: usize = 3;
const BAR_MIN: usize = 8;
const BAR_MAX: usize = 32;

/// `pct` of a fraction, as the panel prints it.
fn pct(frac: f64) -> String {
    format!("{:.0}%", (frac * 100.0).clamp(0.0, 100.0))
}

/// A load average: two decimals, as `uptime` prints them, fewer past ten.
pub fn load_text(value: f64) -> String {
    if value < 10.0 {
        format!("{value:.2}")
    } else if value < 100.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    }
}

/// Coloured clauses joined with ` · `, packed into lines of `room` cells,
/// each line starting with `lead`. A clause wider than a line is broken at
/// its spaces, so nothing is cut.
fn pack(clauses: &[(String, String)], room: usize, lead: &str, sep: &str) -> Vec<Line> {
    let room = room.max(1);
    let mut lines: Vec<Line> = Vec::new();
    let mut line: Line = Vec::new();
    let mut used = 0usize;
    for (colour, clause) in clauses {
        let w = tc::display_width(clause);
        if used > 0 && used + 3 + w > room {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        if w > room {
            if used > 0 {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            for piece in tc::wrap_words(clause, room) {
                lines.push(vec![(colour.clone(), piece)]);
            }
            continue;
        }
        if used > 0 {
            line.push((sep.to_string(), " · ".into()));
            used += 3;
        }
        line.push((colour.clone(), clause.clone()));
        used += w;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    for line in &mut lines {
        line.insert(0, (String::new(), lead.to_string()));
    }
    lines
}

/// `LABEL  clause · clause …`: the label in its own column, the clauses
/// wrapping under the first. Too narrow for the column, the label takes a
/// row of its own.
fn labelled(label: &str, clauses: &[(String, String)], width: usize, p: &Palette) -> Vec<Line> {
    let head = (p.accent.clone(), format!(" {label:<w$}", w = LABEL - 1));
    if width >= LABEL + 20 {
        let mut lines = pack(clauses, width - LABEL - 1, "", &p.dim);
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        for (i, line) in lines.iter_mut().enumerate() {
            let lead = if i == 0 {
                head.clone()
            } else {
                (String::new(), " ".repeat(LABEL))
            };
            line.insert(0, lead);
        }
        return lines;
    }
    let mut lines = vec![vec![(p.accent.clone(), format!(" {label}"))]];
    lines.extend(pack(clauses, width.saturating_sub(2), " ", &p.dim));
    lines
}

/// Cells per core and the gap between cores, widest first, that put every
/// core in `room`; `None` when they do not fit one cell each.
pub fn bar_shape(cores: usize, room: usize) -> Option<(usize, usize)> {
    if cores == 0 {
        return None;
    }
    for k in (1..=4).rev() {
        if cores * (k + 1) - 1 <= room {
            return Some((k, 1));
        }
    }
    (cores <= room).then_some((1, 0))
}

/// The CPU block: the headline, then a bar per core when four or more fit,
/// otherwise one meter of the total.
fn cpu_block(sample: &Sample, every: f64, width: usize, p: &Palette) -> Block {
    let m = &sample.machine;
    let cores = sample.cores;
    let mut clauses = vec![(
        p.txt.clone(),
        format!("{cores} core{}", if cores == 1 { "" } else { "s" }),
    )];
    match (m.busy, sample.clock) {
        (Some(b), Clock::Decaying) => {
            clauses.push((p.txt.clone(), format!("{} decaying average (ps)", pct(b))))
        }
        (Some(b), _) => clauses.push((p.txt.clone(), format!("{} busy", pct(b)))),
        (None, _) => clauses.push((
            p.dim.clone(),
            format!("busy from the next sample, in {}", secs(every)),
        )),
    }
    if let Some((one, five, fifteen)) = m.load {
        clauses.push((
            p.txt.clone(),
            format!(
                "load {} {} {} (of {cores})",
                load_text(one),
                load_text(five),
                load_text(fifteen)
            ),
        ));
    }
    let mut block = Block::default();
    for line in labelled("CPU", &clauses, width, p) {
        block.push(line);
    }
    let room = width.saturating_sub(2);
    let meter = |block: &mut Block| {
        let Some(busy) = m.busy else { return };
        let w = room.min(BAR_MAX);
        if w < 4 {
            return;
        }
        let y = block.lines.len();
        block.plots.push(tc::Plot::meter(
            1,
            y,
            w,
            busy,
            None,
            &tc::heat_on(busy, false),
            &p.track,
            &p.txt,
        ));
        block.push(vec![(String::new(), " ".repeat(1 + w))]);
    };
    let note = |block: &mut Block, text: &str| {
        for line in tc::wrap_words(text, width.saturating_sub(2).max(1)) {
            block.push(vec![(p.dim.clone(), format!(" {line}"))]);
        }
    };
    match &m.per_core {
        PerCore::Read(each) if each.len() >= 4 => match bar_shape(each.len(), room) {
            Some((k, gap)) => {
                let mut columns: Vec<Option<(f64, String)>> = Vec::new();
                for (i, (_, busy)) in each.iter().enumerate() {
                    if i > 0 && gap > 0 {
                        columns.push(None);
                    }
                    for _ in 0..k {
                        columns.push(Some((busy * 100.0, tc::heat_on(*busy, false))));
                    }
                }
                let y = block.lines.len();
                // Text in every mode: these change every poll, and as an
                // image they were a whole upload each time - 31 KB/s of raw
                // RGBA at 100 columns with eight cores, against 0.5 KB/s
                // for the load trace's edits (OPS-222). Each bar stands in
                // its track, so an idle core still shows its full height.
                block.plots.push(
                    tc::Plot::bars_on_track(1, y, &columns, BAR_ROWS, 100.0, &p.track).as_text(),
                );
                for _ in 0..BAR_ROWS {
                    block.push(vec![(String::new(), " ".repeat(1 + columns.len()))]);
                }
                if k >= 3 {
                    // Each core's figure under its bar, in its bar's colour.
                    let mut line: Line = vec![(String::new(), " ".into())];
                    for (i, (_, busy)) in each.iter().enumerate() {
                        if i > 0 {
                            line.push((String::new(), " ".repeat(gap)));
                        }
                        let n = format!("{:.0}", (busy * 100.0).clamp(0.0, 100.0));
                        line.push((tc::heat_on(*busy, false), right(&n, k)));
                    }
                    block.push(line);
                }
                let (id, hot) = each
                    .iter()
                    .copied()
                    .fold((0, -1.0), |best, c| if c.1 > best.1 { c } else { best });
                note(
                    &mut block,
                    &format!(
                        "per core, % over the last poll · busiest cpu{id} {}",
                        pct(hot)
                    ),
                );
            }
            None => meter(&mut block),
        },
        PerCore::Read(_) => meter(&mut block),
        PerCore::Waiting => note(
            &mut block,
            &format!("per core from the next sample, in {}", secs(every)),
        ),
        PerCore::NotRead => {
            meter(&mut block);
            note(
                &mut block,
                "per-core CPU not read on macOS (needs host_processor_info)",
            );
        }
    }
    block
}

/// `LABEL  total  [bar]  legend`, the legend beside the bar when it fits
/// and under it when it does not. `bar_w` caps the bar, so the swap bar
/// can line up under the memory bar.
fn gauge(
    label: &str,
    total: &str,
    legend: &[(String, String)],
    width: usize,
    bar_w: Option<usize>,
    bar: &dyn Fn(usize, usize, usize) -> tc::Plot,
    p: &Palette,
) -> (Block, usize) {
    let head = format!(" {label:<w$}{total:>5}  ", w = LABEL - 1);
    let hw = tc::display_width(&head);
    let legend_w = legend
        .iter()
        .map(|(_, t)| tc::display_width(t))
        .sum::<usize>()
        + 3 * legend.len().saturating_sub(1);
    let cap = bar_w.unwrap_or(BAR_MAX);
    let mut block = Block::default();
    if width >= hw + BAR_MIN + 1 {
        let room = width - hw - 1;
        if room >= BAR_MIN + 2 + legend_w {
            let w = (room - 2 - legend_w).min(cap).max(BAR_MIN);
            block.plots.push(bar(hw, 0, w));
            let mut line: Line = vec![(p.accent.clone(), head), (String::new(), " ".repeat(w + 2))];
            for (i, piece) in legend.iter().enumerate() {
                if i > 0 {
                    line.push((p.dim.clone(), " · ".into()));
                }
                line.push(piece.clone());
            }
            block.push(line);
            return (block, w);
        }
        let w = room.min(cap).max(BAR_MIN);
        block.plots.push(bar(hw, 0, w));
        block.push(vec![
            (p.accent.clone(), head),
            (String::new(), " ".repeat(w)),
        ]);
        let lead = if width >= hw + 16 { hw } else { 1 };
        block.lines.extend(pack(
            legend,
            width.saturating_sub(lead + 1),
            &" ".repeat(lead),
            &p.dim,
        ));
        return (block, w);
    }
    // Narrow: the label and total on one row, the bar under them.
    block.push(vec![(p.accent.clone(), head.trim_end().to_string())]);
    let w = width.saturating_sub(2).min(cap).max(1);
    block.plots.push(bar(1, 1, w));
    block.push(vec![(String::new(), " ".repeat(1 + w))]);
    block
        .lines
        .extend(pack(legend, width.saturating_sub(2), " ", &p.dim));
    (block, w)
}

/// What the memory figures are, under the bar: where `used` comes from.
pub fn mem_note(source: MemSource) -> &'static str {
    match source {
        MemSource::MemAvailable => "used = total − MemAvailable",
        MemSource::VmStat => "from vm_stat",
    }
}

/// MEMORY as used / reclaimable / free, then SWAP.
fn memory_block(sample: &Sample, width: usize, p: &Palette) -> Block {
    let m = &sample.machine;
    let mut block = Block::default();
    let mut bar_w = None;
    if let Some(mem) = m.memory {
        let shares = parse::shares([mem.used, mem.reclaimable, mem.free]);
        let parts: Vec<(f64, String)> = vec![
            (shares[0], p.used.clone()),
            (shares[1], p.reclaimable.clone()),
            (shares[2], p.free.clone()),
        ];
        let legend = vec![
            (p.used.clone(), format!("used {}", human(mem.used))),
            (
                p.reclaimable.clone(),
                format!("reclaimable {}", human(mem.reclaimable)),
            ),
            (p.free.clone(), format!("free {}", human(mem.free))),
        ];
        let (got, w) = gauge(
            "MEMORY",
            &human(mem.total),
            &legend,
            width,
            None,
            &|x, y, w| tc::Plot::stacked_bar(x, y, w, &parts),
            p,
        );
        bar_w = Some(w);
        block.stack(got);
        let note = mem_note(mem.source);
        // Under the legend: in the bar's column where the legend hangs
        // there, at the edge where it does not.
        let hw = LABEL + 7;
        let lead = if width >= hw + 16 { hw } else { 1 };
        for line in tc::wrap_words(note, width.saturating_sub(lead + 1).max(1)) {
            block.push(vec![(p.dim.clone(), format!("{}{line}", " ".repeat(lead)))]);
        }
    }
    match m.swap {
        Some(swap) if swap.total == 0 => block.push(vec![
            (p.accent.clone(), format!(" {:<w$}", "SWAP", w = LABEL - 1)),
            (p.dim.clone(), "no swap".into()),
        ]),
        Some(swap) => {
            let frac = swap.used as f64 / swap.total as f64;
            // Coloured by how full it is: swap in use is pressure, and the
            // bar and its figure say how much in the same colour.
            let hot = tc::heat_on(frac, false);
            let said = if swap.used == 0 {
                (p.dim.clone(), "none used".to_string())
            } else {
                (hot.clone(), format!("{} used", human(swap.used)))
            };
            let (got, _) = gauge(
                "SWAP",
                &human(swap.total),
                &[said],
                width,
                bar_w,
                &|x, y, w| tc::Plot::meter(x, y, w, frac, None, &hot, &p.track, &p.txt),
                p,
            );
            block.stack(got);
        }
        None => {}
    }
    block
}

/// How hot a load of `load` on `cores` cores is, as a fraction of the heat
/// ramp: calm to the core count, where every core has a runnable task on
/// average, and red at twice it.
pub fn load_heat(load: f64, cores: f64) -> f64 {
    let per_core = load / cores.max(1.0);
    if per_core <= 1.0 {
        0.4 * per_core.max(0.0)
    } else {
        (0.4 + 0.6 * (per_core - 1.0)).min(1.0)
    }
}

/// The load chart's colour by height: `load_heat` at nine levels of an
/// axis `hi` tall, mixed between them.
fn load_ramp(cores: f64, hi: f64) -> Vec<(f64, String)> {
    (0..=8)
        .map(|i| {
            let at = i as f64 / 8.0;
            (at, tc::heat_on(load_heat(at * hi, cores), false))
        })
        .collect()
}

/// LOAD: the 1-minute average at every poll for ten minutes, the core
/// count marked on the axis and the latest figure at the line's end.
fn load_block(sample: &Sample, every: f64, width: usize, p: &Palette) -> Block {
    let slots = model::history_slots(every).max(sample.history.len());
    let have = sample.history.len();
    let mut clauses = vec![(p.txt.clone(), "1-minute average, last 10 min".to_string())];
    if have < slots {
        clauses.push((
            p.dim.clone(),
            format!("{} so far", tc::age(have as f64 * every)),
        ));
    }
    let mut block = Block::default();
    for line in labelled("LOAD", &clauses, width, p) {
        block.push(line);
    }
    let latest = sample.history.iter().rev().find_map(|v| *v);
    let Some(latest) = latest else {
        block.push(vec![(p.dim.clone(), " no reading yet".into())]);
        return block;
    };
    let cores = sample.cores.max(1) as f64;
    let peak = sample
        .history
        .iter()
        .flatten()
        .copied()
        .fold(0.0f64, f64::max);
    // The axis is the core count until the load passes it, then the next
    // whole number above the peak: it moves only when the load does.
    let hi = cores.max(peak.ceil()).max(1.0);
    const ROWS: usize = 4;
    let core_row = tc::line_row(cores / hi, ROWS);
    let cores_label = format!("{} cores", sample.cores);
    let label = |row: usize| -> (String, bool) {
        if row == core_row {
            (cores_label.clone(), true)
        } else if row == 0 {
            (load_text(hi), false)
        } else if row == ROWS - 1 {
            ("0".to_string(), false)
        } else {
            (String::new(), false)
        }
    };
    let aw = (0..ROWS)
        .map(|r| tc::display_width(&label(r).0))
        .max()
        .unwrap_or(1);
    let now = format!(" {}", load_text(latest));
    let x = 1 + aw + 1;
    let cw = width.saturating_sub(x + tc::display_width(&now) + 1);
    if cw < 6 {
        block.push(vec![(p.dim.clone(), format!(" now {}", load_text(latest)))]);
        return block;
    }
    let chart = tc::LineChart {
        slots,
        traces: vec![tc::Trace {
            values: sample
                .history
                .iter()
                .map(|v| v.map(|v| (v / hi).clamp(0.0, 1.0)))
                .collect(),
            positions: None,
            colour: p.accent.clone(),
            baseline: None,
        }],
        focus: None,
    };
    let y = block.lines.len();
    block.plots.push(
        tc::Plot::new(x, y, cw, ROWS, chart)
            .under_text()
            .on_axis(0.0, hi)
            .area()
            .ramp(load_ramp(cores, hi))
            .reference(cores / hi, &p.dim)
            .end_dot(),
    );
    let now_row = tc::line_row((latest / hi).clamp(0.0, 1.0), ROWS);
    for row in 0..ROWS {
        let (text, is_cores) = label(row);
        let mut line: Line = vec![
            (
                if is_cores {
                    p.txt.clone()
                } else {
                    p.dim.clone()
                },
                format!(" {}", right(&text, aw)),
            ),
            (
                p.dim.clone(),
                if text.is_empty() { "│" } else { "┤" }.into(),
            ),
            (String::new(), " ".repeat(cw)),
        ];
        if row == now_row {
            line.push((p.accent.clone(), now.clone()));
        }
        block.push(line);
    }
    block
}

/// The whole panel for `width`: problems first, then CPU beside MEMORY and
/// SWAP when both fit on one line each, under each other when they do
/// not, then the load trace across the full width.
pub fn panel(sample: &Sample, every: f64, width: usize, p: &Palette) -> Panel {
    let mut block = Block::default();
    for why in &sample.machine.problems {
        for line in tc::wrap_words(&format!("! {why}"), width.saturating_sub(2).max(1)) {
            block.push(vec![(p.bad.clone(), format!(" {line}"))]);
        }
    }
    // Side by side when the CPU block keeps its natural width and the
    // memory and swap legends stay beside bars a dozen cells long.
    const WIDE: usize = 400;
    let unwrapped = cpu_block(sample, every, WIDE, p);
    let left = unwrapped.lines.iter().map(line_width).max().unwrap_or(0) + 1;
    let gap = 2;
    let right_w = width.saturating_sub(left + gap);
    let cpu = cpu_block(sample, every, left, p);
    let unwrapped_mem = memory_block(sample, WIDE, p).lines.len();
    let roomy = memory_block(sample, right_w.saturating_sub(12 - BAR_MIN), p);
    if left <= width / 2
        && cpu.lines.len() == unwrapped.lines.len()
        && roomy.lines.len() == unwrapped_mem
    {
        let mem = memory_block(sample, right_w, p);
        block.stack(cpu.beside(mem, left + gap));
    } else {
        block.stack(cpu_block(sample, every, width, p));
        block.push(Vec::new());
        block.stack(memory_block(sample, width, p));
    }
    block.push(Vec::new());
    block.stack(load_block(sample, every, width, p));
    let rows = block
        .lines
        .iter()
        .map(|line| {
            let parts: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            tc::seg(&parts, width)
        })
        .collect();
    Panel {
        rows,
        plots: block.plots,
    }
}

// ── the list ─────────────────────────────────────────────────────────

pub const CPU_W: usize = 5;
const MEM_W: usize = 5;
const RSS_W: usize = 5;
const STATE_W: usize = 5;
const RUN_W: usize = 7;
const CMD_MIN: usize = 12;
const CGROUP_MIN: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cols {
    pub pid: usize,
    pub cmd: usize,
    pub user: Option<usize>,
    pub mem: bool,
    pub state: bool,
    pub running: bool,
    pub cgroup: Option<usize>,
}

impl Cols {
    /// The heading, in the order the cells are drawn.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn names(&self) -> Vec<&'static str> {
        let mut out = vec!["PID", "COMMAND"];
        if self.user.is_some() {
            out.push("USER");
        }
        out.push("%CPU");
        if self.mem {
            out.push("MEM");
        }
        out.push("RSS");
        if self.state {
            out.push("STATE");
        }
        if self.running {
            out.push("RUNNING");
        }
        if self.cgroup.is_some() {
            out.push("CGROUP");
        }
        out
    }
}

/// Which columns a width holds. They leave in the order CGROUP, RUNNING,
/// STATE, USER, MEM; a set is taken while COMMAND still gets
/// `CMD_MIN` cells (or all it wants, if less). PID, %CPU and RSS stay
/// however narrow the pane, and COMMAND then wraps rather than being cut.
/// What COMMAND does not want goes to CGROUP; what neither wants is left
/// blank at the end of the row.
pub fn columns(
    width: usize,
    pid_w: usize,
    cmd_want: usize,
    user_want: usize,
    cg_want: usize,
) -> Cols {
    // (user, mem, state, running, cgroup)
    let sets = [
        (true, true, true, true, true),
        (true, true, true, true, false),
        (true, true, true, false, false),
        (true, true, false, false, false),
        (false, true, false, false, false),
        (false, false, false, false, false),
    ];
    let cmd_want = cmd_want.max(1);
    let cg_want = cg_want.max(if cg_want > 0 { CGROUP_MIN } else { 0 });
    let user_w = user_want.max(4);
    let shape = |&(user, mem, state, running, cgroup): &(bool, bool, bool, bool, bool)| {
        let cgroup = cgroup && cg_want > 0;
        let mut fixed = pid_w + CPU_W + RSS_W;
        let mut cells = 4; // pid, command, %cpu, rss
        for (on, w) in [
            (user, user_w),
            (mem, MEM_W),
            (state, STATE_W),
            (running, RUN_W),
        ] {
            if on {
                fixed += w;
                cells += 1;
            }
        }
        if cgroup {
            cells += 1;
        }
        (fixed + cells - 1, cgroup)
    };
    let pick = sets
        .iter()
        .find(|set| {
            let (fixed, cgroup) = shape(set);
            let need = fixed + cmd_want.min(CMD_MIN) + if cgroup { CGROUP_MIN } else { 0 };
            need <= width
        })
        .unwrap_or(&sets[sets.len() - 1]);
    let (fixed, cgroup) = shape(pick);
    let room = width.saturating_sub(fixed);
    let cg_min = if cgroup { CGROUP_MIN } else { 0 };
    // COMMAND first: everything it wants, short of CGROUP's minimum.
    let cmd = tc::fit_columns(room.saturating_sub(cg_min), &[(1, cmd_want)])[0].max(1);
    let cgroup = cgroup.then(|| {
        let left = room.saturating_sub(cmd);
        tc::fit_columns(left, &[(cg_min.min(left), cg_want)])[0].max(1)
    });
    Cols {
        pid: pid_w,
        cmd,
        user: pick.0.then_some(user_w),
        mem: pick.1,
        state: pick.2,
        running: pick.3,
        cgroup,
    }
}

pub fn right(text: &str, w: usize) -> String {
    let n = tc::display_width(text);
    if n >= w {
        text.to_string()
    } else {
        format!("{}{}", " ".repeat(w - n), text)
    }
}

/// The cells of one row, joined with single spaces and the selected row's
/// tint carried to the right edge.
pub fn join(cells: &[(String, String)], width: usize, here: bool, tint: &str) -> String {
    // Neighbours in the same colour are one segment: one escape rather
    // than one per cell, which is most of the bytes a row costs.
    let mut parts: Vec<(&str, String)> = Vec::new();
    for (i, (colour, text)) in cells.iter().enumerate() {
        let gap = if i > 0 { " " } else { "" };
        match parts.last_mut() {
            Some((last, held)) if *last == colour.as_str() => {
                held.push_str(gap);
                held.push_str(text);
            }
            _ => {
                if let Some((_, held)) = parts.last_mut().filter(|_| i > 0) {
                    held.push_str(gap);
                }
                parts.push((colour.as_str(), text.clone()));
            }
        }
    }
    if here {
        let used: usize = parts.iter().map(|(_, t)| tc::display_width(t)).sum();
        if used < width {
            parts.push((tint, " ".repeat(width - used)));
        }
    }
    tc::seg(&parts, width)
}

pub fn header(cols: &Cols, sort: Sort, width: usize, p: &Palette) -> String {
    let sorted = match sort {
        Sort::Cpu => "%CPU",
        Sort::Memory => "RSS",
        Sort::Running => "RUNNING",
    };
    let mut cells: Vec<(String, String)> = Vec::new();
    let ink = |name: &str| {
        if name == sorted {
            p.accent.clone()
        } else {
            p.dim.clone()
        }
    };
    cells.push((ink("PID"), right("PID", cols.pid)));
    cells.push((ink("COMMAND"), tc::pad("COMMAND", cols.cmd)));
    if let Some(w) = cols.user {
        cells.push((ink("USER"), tc::pad("USER", w)));
    }
    cells.push((ink("%CPU"), right("%CPU", CPU_W)));
    if cols.mem {
        cells.push((ink("MEM"), right("MEM", MEM_W)));
    }
    cells.push((ink("RSS"), right("RSS", RSS_W)));
    if cols.state {
        cells.push((ink("STATE"), tc::pad("STATE", STATE_W)));
    }
    if cols.running {
        cells.push((ink("RUNNING"), right("RUNNING", RUN_W)));
    }
    if let Some(w) = cols.cgroup {
        cells.push((ink("CGROUP"), tc::pad("CGROUP", w)));
    }
    join(&cells, width, false, "")
}

/// One process as one or more lines: a second line only where COMMAND or
/// CGROUP wraps.
pub fn row(
    proc: &Proc,
    cols: &Cols,
    cores: usize,
    width: usize,
    here: bool,
    p: &Palette,
) -> Vec<String> {
    let tint = if here {
        tc::tint(tc::Role::Slot)
    } else {
        String::new()
    };
    let c = |colour: &str| {
        if here {
            format!("{}{}", tint, p.lit(colour))
        } else {
            colour.to_string()
        }
    };
    let comm = chunks(&proc.comm, cols.cmd);
    let cg_text = proc.cgroup.as_deref().map(cgroup_cell).unwrap_or_default();
    let cgroup = cols.cgroup.map(|w| chunks(&cg_text, w)).unwrap_or_default();
    let user = cols.user.map(|w| chunks(&proc.user, w)).unwrap_or_default();
    let lines = comm.len().max(cgroup.len()).max(user.len()).max(1);
    // A share of one core, so a process pegging a core is hot whatever
    // the core count.
    let heat = proc
        .cpu
        .map(|cpu| tc::heat_on((cpu * cores as f64 / 100.0).clamp(0.0, 1.0), here));
    let cpu_ink = match (&heat, proc.cpu) {
        (Some(hot), Some(cpu)) if cpu >= 0.05 => format!("{}{}", tint, hot),
        _ => c(&p.dim),
    };
    let state_ink = match proc.state {
        'D' => p.down.as_str(),
        'Z' => p.bad.as_str(),
        _ => p.txt.as_str(),
    };
    let mut out = Vec::with_capacity(lines);
    for line in 0..lines {
        let first = line == 0;
        let at = |pieces: &[String]| pieces.get(line).cloned().unwrap_or_default();
        let mark = if first && here { "▸" } else { " " };
        let pid = if first {
            proc.pid.to_string()
        } else {
            String::new()
        };
        let pid_cell = format!("{mark}{}", right(&pid, cols.pid.saturating_sub(1)));
        let mut cells: Vec<(String, String)> = vec![
            (c(if here { &p.accent } else { &p.txt }), pid_cell),
            (c(&p.txt), tc::pad(&at(&comm), cols.cmd)),
        ];
        if let Some(w) = cols.user {
            cells.push((c(&p.dim), tc::pad(&at(&user), w)));
        }
        let blank = |w: usize| " ".repeat(w);
        cells.push(if first {
            (cpu_ink.clone(), right(&cpu_cell(proc.cpu), CPU_W))
        } else {
            (c(&p.txt), blank(CPU_W))
        });
        if cols.mem {
            cells.push((
                c(&p.txt),
                if first {
                    right(&mem_cell(proc.mem), MEM_W)
                } else {
                    blank(MEM_W)
                },
            ));
        }
        cells.push((
            c(&p.txt),
            if first {
                right(&human(proc.rss), RSS_W)
            } else {
                blank(RSS_W)
            },
        ));
        if cols.state {
            let letter = if first {
                proc.state.to_string()
            } else {
                String::new()
            };
            cells.push((c(state_ink), tc::pad(&letter, STATE_W)));
        }
        if cols.running {
            let ran = match (first, proc.running) {
                (true, Some(r)) => tc::age(r),
                (true, None) => "—".into(),
                _ => String::new(),
            };
            cells.push((c(&p.dim), right(&ran, RUN_W)));
        }
        if let Some(w) = cols.cgroup {
            cells.push((c(&p.dim), tc::pad(&at(&cgroup), w)));
        }
        out.push(join(&cells, width, here, &tint));
    }
    out
}

/// One thread under its process: its tid in PID, `└ name` in COMMAND, its
/// own %CPU, state and age. Memory, the user and the cgroup are the
/// process's, so the thread's cells there are left empty rather than
/// repeated.
fn thread_row(
    thread: &Thread,
    cols: &Cols,
    cores: usize,
    width: usize,
    p: &Palette,
) -> Vec<String> {
    let comm = chunks(&format!("└ {}", thread.comm), cols.cmd);
    let cpu_ink = match thread.cpu {
        Some(cpu) if cpu >= 0.05 => {
            tc::heat_on((cpu * cores as f64 / 100.0).clamp(0.0, 1.0), false)
        }
        _ => p.dim.clone(),
    };
    let state_ink = match thread.state {
        'D' => p.down.clone(),
        'Z' => p.bad.clone(),
        _ => p.dim.clone(),
    };
    let mut out = Vec::with_capacity(comm.len());
    for (line, piece) in comm.iter().enumerate() {
        let first = line == 0;
        let tid = if first {
            thread.tid.to_string()
        } else {
            String::new()
        };
        let first_or = |text: String, w: usize| if first { text } else { " ".repeat(w) };
        let mut cells: Vec<(String, String)> = vec![
            (
                p.dim.clone(),
                format!(" {}", right(&tid, cols.pid.saturating_sub(1))),
            ),
            (p.dim.clone(), tc::pad(piece, cols.cmd)),
        ];
        if let Some(w) = cols.user {
            cells.push((p.dim.clone(), " ".repeat(w)));
        }
        cells.push((
            cpu_ink.clone(),
            first_or(right(&cpu_cell(thread.cpu), CPU_W), CPU_W),
        ));
        if cols.mem {
            cells.push((p.dim.clone(), " ".repeat(MEM_W)));
        }
        cells.push((p.dim.clone(), " ".repeat(RSS_W)));
        if cols.state {
            cells.push((
                state_ink.clone(),
                first_or(tc::pad(&thread.state.to_string(), STATE_W), STATE_W),
            ));
        }
        if cols.running {
            let ran = thread.running.map(tc::age).unwrap_or_else(|| "—".into());
            cells.push((p.dim.clone(), first_or(right(&ran, RUN_W), RUN_W)));
        }
        if let Some(w) = cols.cgroup {
            cells.push((p.dim.clone(), " ".repeat(w)));
        }
        out.push(join(&cells, width, false, ""));
    }
    out
}

/// When the figures were read and how often: the summary on the first
/// group's rule, since every figure under it comes from that one poll.
pub fn timing(view: &View) -> Vec<String> {
    vec![
        match view.age {
            Some(age) => format!("sampled {} ago", tc::age(age.as_secs_f64())),
            None => "not sampled yet".to_string(),
        },
        format!("every {}", secs(view.every)),
    ]
}

/// What the PROCESSES rule says: how many, in what order, what is not
/// shown, then what %CPU in the table is a share of and over what - last,
/// because it is the longest and the one to go under the rule first.
fn status(
    view: &View,
    sample: &Sample,
    listed: usize,
    zombies: usize,
    hidden: usize,
) -> Vec<String> {
    let mut clauses = vec![if sample.hidepid {
        format!("{listed} visible (hidepid hides other users')")
    } else {
        listed.to_string()
    }];
    clauses.push(view.sort.in_force().to_string());
    if zombies > 0 {
        clauses.push(format!(
            "{zombies} zombie{}",
            if zombies == 1 { "" } else { "s" }
        ));
    }
    if hidden > 0 {
        clauses.push(format!(
            "{hidden} kernel thread{} hidden",
            if hidden == 1 { "" } else { "s" }
        ));
    }
    clauses
}

/// One part of the screen - MACHINE, PROCESSES, SERVICES - as a rule
/// across the pane with what the part comes to beside its name, the way
/// `disk` draws its groups: ` ── NAME ── a · b ──────`. A blank line always
/// comes first, so a part never runs on from the one above it. As many
/// clauses as fit go on the rule, in order; the rest go whole onto the
/// lines under it rather than being cut.
pub fn push_group(
    name: &str,
    clauses: &[String],
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
) {
    rows.push(String::new());
    let room = width.saturating_sub(1);
    let rule = "\u{2500}\u{2500}";
    // ` ── NAME ── ` and at least `──` of rule after whatever follows it.
    let head_w = 1 + 2 + 1 + tc::display_width(name) + 1 + 2 + 1;
    let mut on_rule = String::new();
    let mut taken = 0;
    for clause in clauses {
        let next = if on_rule.is_empty() {
            clause.clone()
        } else {
            format!("{on_rule} \u{b7} {clause}")
        };
        if head_w + tc::display_width(&next) + 1 + 2 > room {
            break;
        }
        on_rule = next;
        taken += 1;
    }
    let mut parts: Vec<(&str, String)> = vec![
        (p.dim.as_str(), format!(" {rule} ")),
        (p.txt.as_str(), name.to_string()),
    ];
    let mut used = 1 + 2 + 1 + tc::display_width(name);
    if on_rule.is_empty() {
        parts.push((p.dim.as_str(), " ".into()));
        used += 1;
    } else {
        parts.push((p.dim.as_str(), format!(" {rule} {on_rule} ")));
        used += 4 + tc::display_width(&on_rule) + 1;
    }
    parts.push((p.dim.as_str(), "\u{2500}".repeat(room.saturating_sub(used))));
    rows.push(tc::seg(&parts, width));
    for line in pack_clauses(&clauses[taken..], width) {
        rows.push(tc::seg(&[(p.dim.as_str(), line)], width));
    }
}

/// The table under the status lines: its rows, where each process landed
/// (row within the table, item), and the pids in the order drawn. The
/// expensive half of a frame, which `main` keeps until something it
/// depends on changes.
#[derive(Clone, Debug, Default)]
pub struct Table {
    pub rows: Vec<String>,
    /// How many of `rows`, from the top, are the column header: pinned
    /// with the head above them, so the columns stay named as the rows
    /// under them scroll. None when a sentence stands in for the table.
    pub header: usize,
    pub placed: Vec<(usize, usize)>,
    pub keys: Vec<i32>,
    /// The services view's rows, by cgroup path.
    pub paths: Vec<String>,
}

/// The processes the table lists, in order, and how many there are before
/// the filter.
fn shown<'a>(view: &View, sample: &'a Sample) -> (Vec<&'a Proc>, Vec<&'a Proc>, usize) {
    let (listed, hidden) = model::listed(sample, view.show_kernel);
    let mut shown: Vec<&Proc> = listed
        .iter()
        .copied()
        .filter(|proc| model::passes(proc, view.needle, view.all_users, sample.me))
        .collect();
    model::sort(&mut shown, view.sort);
    (listed, shown, hidden)
}

/// The title and everything above the table, with the plots in it: any
/// error, then the MACHINE rule (when and how often) over the CPU / MEMORY
/// / SWAP / LOAD panel, then the PROCESSES rule (how many, the sort, what
/// is hidden, what %CPU means) and the filter in force. The services view
/// puts its SERVICES rule where these two parts are.
pub struct Head {
    pub rows: Vec<String>,
    pub plots: Vec<tc::Plot>,
}

/// [`Head`] for this frame. `panel` is built once a poll by `main` (see
/// [`panel`]).
pub fn list_head(
    view: &View,
    width: usize,
    p: &Palette,
    busy: Option<usize>,
    panel: Option<&Panel>,
) -> Head {
    let mut rows = vec![tc::title_busy("procs", width, &p.accent, busy)];
    let mut plots = Vec::new();
    let err_rows = |rows: &mut Vec<String>| {
        if !view.err.is_empty() {
            for line in tc::wrap_words(&format!("! {}", view.err), width.saturating_sub(1).max(1)) {
                rows.push(tc::seg(&[(p.bad.as_str(), format!(" {line}"))], width));
            }
        }
    };
    let Some(sample) = view.sample else {
        rows.push(tc::seg(
            &[(p.dim.as_str(), " reading processes…".into())],
            width,
        ));
        err_rows(&mut rows);
        return Head { rows, plots };
    };
    let (listed, hidden) = model::listed(sample, view.show_kernel);
    let passing: Vec<&Proc> = listed
        .iter()
        .copied()
        .filter(|proc| model::passes(proc, view.needle, view.all_users, sample.me))
        .collect();
    let count = passing.len();
    let (zombies, _) = model::counts(&listed);
    let mut clauses = status(view, sample, listed.len(), zombies, hidden);
    if view.threads {
        clauses.push(match &sample.threads {
            Some(threads) => {
                let n: usize = passing
                    .iter()
                    .filter_map(|proc| threads.get(&proc.pid))
                    .map(Vec::len)
                    .sum();
                format!("{n} threads listed under their processes")
            }
            None => "reading threads…".to_string(),
        });
    }
    clauses.extend(cpu_label(sample.clock, sample.cores, view.every));
    err_rows(&mut rows);
    match panel {
        Some(panel) => {
            push_group("MACHINE", &timing(view), width, p, &mut rows);
            let top = rows.len();
            plots.extend(panel.plots.iter().cloned().map(|mut plot| {
                plot.y += top;
                plot
            }));
            rows.extend(panel.rows.iter().cloned());
        }
        // No panel to carry the timing: it goes on the list's own rule.
        None => {
            clauses.splice(0..0, timing(view));
        }
    }
    push_group("PROCESSES", &clauses, width, p, &mut rows);
    if view.typing {
        rows.push(tc::seg(
            &[
                (p.accent.as_str(), format!(" /{}", view.needle)),
                (p.accent.as_str(), "▏".into()),
            ],
            width,
        ));
    }
    let filters = model::filters(view.needle, view.all_users);
    if let Some(line) = tc::filter_row(count, listed.len(), &filters) {
        let line = if sample.hidepid {
            line.replacen(" shown", " visible shown", 1)
        } else {
            line
        };
        for piece in tc::wrap_words(&line, width.saturating_sub(1).max(1)) {
            rows.push(tc::seg(&[(p.txt.as_str(), format!(" {piece}"))], width));
        }
    }
    Head { rows, plots }
}

pub fn list_table(view: &View, width: usize, p: &Palette) -> Table {
    let mut table = Table::default();
    let Some(sample) = view.sample else {
        return table;
    };
    let (listed, shown, _) = shown(view, sample);
    let rows = &mut table.rows;
    if shown.is_empty() {
        let filters = model::filters(view.needle, view.all_users);
        let said = tc::filtered_to_nothing(listed.len(), &filters)
            .unwrap_or_else(|| "no processes listed".to_string());
        for line in tc::wrap_words(&said, width.saturating_sub(1).max(1)) {
            rows.push(tc::seg(&[(p.dim.as_str(), format!(" {line}"))], width));
        }
        return table;
    }
    let (_, waiting) = model::counts(&shown);
    let threads = sample.threads.as_ref().filter(|_| view.threads);
    let under = |pid: i32| threads.and_then(|t| t.get(&pid)).map(Vec::as_slice);
    let digits = shown
        .iter()
        .map(|p| p.pid)
        .chain(
            shown
                .iter()
                .flat_map(|p| under(p.pid).unwrap_or(&[]).iter().map(|t| t.tid)),
        )
        .map(|pid| pid.to_string().len())
        .max()
        .unwrap_or(3)
        .max(3);
    // A thread's name is drawn after `└ `, so it wants two cells more.
    let cmd_want = shown
        .iter()
        .map(|p| tc::display_width(&p.comm))
        .chain(shown.iter().flat_map(|p| {
            under(p.pid)
                .unwrap_or(&[])
                .iter()
                .map(|t| tc::display_width(&t.comm) + 2)
        }))
        .max()
        .unwrap_or(7)
        .max(7);
    let user_want = shown
        .iter()
        .map(|p| tc::display_width(&p.user))
        .max()
        .unwrap_or(4);
    let cg_want = shown
        .iter()
        .filter_map(|p| p.cgroup.as_deref())
        .map(|c| tc::display_width(&cgroup_cell(c)))
        .max()
        .unwrap_or(0);
    let cols = columns(width, digits + 1, cmd_want, user_want, cg_want);
    rows.push(header(&cols, view.sort, width, p));
    table.header = 1;
    for (index, proc) in shown.iter().enumerate() {
        let here = view.cursor == Some(proc.pid);
        let from = rows.len();
        rows.extend(row(proc, &cols, sample.cores, width, here, p));
        // A thread is part of its process's item: a click on it selects the
        // process, and the arrows step over it to the next process.
        for thread in under(proc.pid).unwrap_or(&[]) {
            rows.extend(thread_row(thread, &cols, sample.cores, width, p));
        }
        table.placed.extend((from..rows.len()).map(|r| (r, index)));
        table.keys.push(proc.pid);
    }
    if cols.state {
        rows.push(String::new());
        let mut parts = vec![
            (p.down.clone(), " D".to_string()),
            (p.dim.clone(), " = waiting on disk".to_string()),
        ];
        if waiting > 0 {
            parts.push((p.dim.clone(), format!(", {waiting} shown now")));
        }
        let parts: Vec<(&str, String)> =
            parts.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&parts, width));
    }
    table
}

/// The head and the table as one frame body: the title always pinned, and
/// everything down to the column header pinnable.
pub fn compose(head: Head, table: &Table) -> Drawn {
    let mut rows = head.rows;
    let offset = rows.len();
    rows.extend(table.rows.iter().cloned());
    Drawn {
        rows,
        placed: table.placed.iter().map(|&(r, i)| (r + offset, i)).collect(),
        head: 1,
        pin: offset + table.header,
        keys: table.keys.clone(),
        paths: table.paths.clone(),
        plots: head.plots,
    }
}

/// The fewest list rows a pinned head may leave under itself: below this a
/// list of hundreds is read through a slot.
pub const LIST_MIN: usize = 6;

/// How far the room may shrink past the threshold before a pinned head lets
/// go: the filter box and the filter row come and go with a key, and the
/// panel's note lines with a poll, and none of that should make the whole
/// screen jump from one way of scrolling to the other.
pub const PIN_SLACK: usize = 3;

/// Whether the head - everything down to the column header - stays put
/// above the list, with the wheel and the cursor moving the list alone.
///
/// `room` is the rows above the footer, `pin` the rows that would be pinned,
/// `was` this decision on the last frame at the same size and in the same
/// view. Pinned when the list keeps at least [`LIST_MIN`] rows and a third
/// of the room: a third because past that the head is most of the pane
/// and a list squeezed under it is the part that hides things, which the
/// scroll-don't-hide rule is about. Otherwise the whole body scrolls under
/// the title, and every row is still one scroll away.
///
/// A pinned head lets go only [`PIN_SLACK`] rows past the line, so a head
/// that grows by a row or two between polls cannot flick the screen from
/// one mode to the other and back.
pub fn pin_head(room: usize, pin: usize, was: Option<bool>) -> bool {
    let need = LIST_MIN.max(room / 3);
    let left = room.saturating_sub(pin);
    match was {
        Some(true) => left + PIN_SLACK >= need,
        _ => left >= need,
    }
}

/// One frame's split of the body: whether the head is pinned, how many rows
/// stay put above the window, and how many rows the window has. `room` is
/// the rows above the footer; `was` as for [`pin_head`].
pub fn layout(drawn: &Drawn, room: usize, was: Option<bool>) -> (bool, usize, usize) {
    let pinned = drawn.pin > drawn.head && pin_head(room, drawn.pin, was);
    let head = if pinned { drawn.pin } else { drawn.head }.min(drawn.rows.len());
    (pinned, head, room.saturating_sub(head).max(1))
}

/// `scroll` held to the rows there are under `head`, so the window never
/// opens past the end.
pub fn clamp_scroll(drawn: &Drawn, head: usize, scroll: usize, below: usize) -> usize {
    let rest = drawn.rows.len().saturating_sub(head);
    scroll.min(rest.saturating_sub(below.min(rest)))
}

/// The rows on screen: the `head` rows as they are, then `below` rows of
/// the rest from `scroll`.
pub fn visible(drawn: &Drawn, head: usize, scroll: usize, below: usize) -> Vec<String> {
    let rest = drawn.rows.len().saturating_sub(head);
    let end = (scroll + below).min(rest);
    let mut rows: Vec<String> = drawn.rows[..head].to_vec();
    rows.extend_from_slice(&drawn.rows[head + scroll.min(end)..head + end]);
    rows
}

pub fn draw_list(view: &View, width: usize, p: &Palette, busy: Option<usize>) -> Drawn {
    let panel = view
        .sample
        .map(|sample| panel(sample, view.every, width, p));
    compose(
        list_head(view, width, p, busy, panel.as_ref()),
        &list_table(view, width, p),
    )
}

// ── the detail ───────────────────────────────────────────────────────

const LABEL_W: usize = 11;

/// `label  value`, the value wrapped under itself and never cut.
pub fn field(
    rows: &mut Vec<String>,
    label: &str,
    value: &str,
    ink: &str,
    width: usize,
    p: &Palette,
) {
    let room = width.saturating_sub(LABEL_W + 2).max(8);
    if width < LABEL_W + 12 {
        // Too narrow to hang a value beside its label: one under the other.
        rows.push(tc::seg(&[(p.dim.as_str(), format!(" {label}"))], width));
        for line in tc::wrap_words(value, width.saturating_sub(3).max(1)) {
            rows.push(tc::seg(&[(ink, format!("   {line}"))], width));
        }
        return;
    }
    for (i, line) in tc::wrap_words(value, room).into_iter().enumerate() {
        let lead = if i == 0 {
            tc::pad(label, LABEL_W)
        } else {
            " ".repeat(LABEL_W)
        };
        rows.push(tc::seg(
            &[(p.dim.as_str(), format!(" {lead} ")), (ink, line)],
            width,
        ));
    }
}

/// The detail's disk line: a rate, or why there is none.
pub fn io_line(io: Option<&Io>, every: f64) -> String {
    match io {
        None => "reading…".into(),
        Some(Io::Rate { read, write, over }) => format!(
            "read {}/s · write {}/s, over the last {}",
            human(*read as u64),
            human(*write as u64),
            secs(*over)
        ),
        Some(Io::Measuring) => format!("measuring: a rate needs a second read, in {}", secs(every)),
        // The row's label is `disk I/O`, so the row reads as the sentence.
        Some(Io::NeedsRoot) => "needs root for another user's process".into(),
        Some(Io::Unreadable(why)) => format!("unreadable: {why}"),
        Some(Io::NotRead) => "not read on macOS in this version".into(),
    }
}

pub fn draw_detail(view: &View, width: usize, p: &Palette, busy: Option<usize>) -> Drawn {
    let Some(open) = view.opened else {
        return draw_list(view, width, p, busy);
    };
    let proc = &open.last;
    let title = format!("procs · pid {}", open.pid);
    let mut rows = vec![tc::title_busy(&title, width, &p.accent, busy)];
    let head = rows.len();
    let mac = proc.cgroup.is_none() && view.sample.is_some_and(|s| s.clock == Clock::Decaying);
    let not_on_mac = "not read on macOS in this version";
    if open.gone {
        for line in tc::wrap_words(
            "exited: this is how it looked when it was last seen",
            width.saturating_sub(1).max(1),
        ) {
            rows.push(tc::seg(&[(p.bad.as_str(), format!(" {line}"))], width));
        }
    }
    if !view.err.is_empty() {
        for line in tc::wrap_words(&format!("! {}", view.err), width.saturating_sub(1).max(1)) {
            rows.push(tc::seg(&[(p.bad.as_str(), format!(" {line}"))], width));
        }
    }
    let facts = open.facts.as_ref();
    let txt = p.txt.as_str();
    let dim = p.dim.as_str();
    field(&mut rows, "command", &proc.comm, txt, width, p);
    let word = state_word(proc.state);
    let state = if word.is_empty() {
        proc.state.to_string()
    } else {
        format!("{} {}", proc.state, word)
    };
    let state_ink = match proc.state {
        'D' => p.down.as_str(),
        'Z' => p.bad.as_str(),
        _ => txt,
    };
    field(&mut rows, "state", &state, state_ink, width, p);
    let cores = view.sample.map(|s| s.cores).unwrap_or(1);
    let clock = view
        .sample
        .map(|s| s.clock)
        .unwrap_or(Clock::Measured { window: None });
    let cpu = match proc.cpu {
        Some(c) => format!(
            "{:.1}% · {}",
            c,
            cpu_label(clock, cores, view.every).join(" · ")
        ),
        None => format!("— · {}", cpu_label(clock, cores, view.every).join(" · ")),
    };
    field(&mut rows, "%CPU", &cpu, txt, width, p);
    let memory = match proc.mem {
        Some(m) => format!("{} resident · {:.1}% of memory", human(proc.rss), m),
        None => format!("{} resident", human(proc.rss)),
    };
    field(&mut rows, "memory", &memory, txt, width, p);
    let gid = facts.and_then(|f| f.gid);
    let user = match gid {
        Some(gid) => format!("{} · uid {} · gid {}", proc.user, proc.uid, gid),
        None if mac => format!("{} · uid {} · gid {not_on_mac}", proc.user, proc.uid),
        None => format!("{} · uid {}", proc.user, proc.uid),
    };
    field(&mut rows, "user", &user, txt, width, p);
    let threads = proc
        .threads
        .map(|n| n.to_string())
        .unwrap_or_else(|| not_on_mac.into());
    field(&mut rows, "threads", &threads, txt, width, p);
    let nice = proc
        .nice
        .map(|n| n.to_string())
        .unwrap_or_else(|| not_on_mac.into());
    field(&mut rows, "nice", &nice, txt, width, p);
    if let Some(running) = proc.running {
        let started = started_at(chrono::Local::now(), view.age, running);
        let when = format!(
            "{} · running {}",
            started.format("%Y-%m-%d %H:%M:%S"),
            tc::age(running)
        );
        field(&mut rows, "started", &when, txt, width, p);
    }
    let cpu_time = proc
        .cpu_time
        .map(duration)
        .unwrap_or_else(|| not_on_mac.into());
    field(&mut rows, "cpu time", &cpu_time, txt, width, p);
    if let Some(sample) = view.sample {
        let chain = model::parent_chain(&sample.procs, open.pid);
        let text = chain
            .iter()
            .map(|(pid, name)| match name {
                Some(name) => format!("{pid} {name}"),
                None => format!("{pid}"),
            })
            .collect::<Vec<_>>()
            .join(" ← ");
        field(&mut rows, "parents", &text, txt, width, p);
    }
    if let Some(path) = proc.cgroup.as_deref() {
        let unit = tc::procfs::unit_name(path);
        let text = if unit.is_empty() {
            path.to_string()
        } else {
            format!("{path} · unit {unit}")
        };
        field(&mut rows, "cgroup", &text, dim, width, p);
    }
    let cwd = match facts.map(|f| &f.cwd) {
        Some(Ok(cwd)) => cwd.clone(),
        Some(Err(why)) => why.clone(),
        None => "reading…".into(),
    };
    field(&mut rows, "cwd", &cwd, txt, width, p);
    let io = if open.gone {
        "not read: the process has exited".to_string()
    } else {
        io_line(facts.map(|f| &f.io), view.every)
    };
    let io_ink = match facts.map(|f| &f.io) {
        Some(Io::Rate { .. }) if !open.gone => txt,
        _ => dim,
    };
    field(&mut rows, "disk I/O", &io, io_ink, width, p);
    rows.push(String::new());
    rows.push(tc::seg(&[(dim, " command line".to_string())], width));
    let cmdline = match facts.map(|f| &f.cmdline) {
        Some(Ok(line)) => (line.clone(), txt),
        Some(Err(why)) => (why.clone(), dim),
        None if !proc.args.is_empty() => (proc.args.clone(), txt),
        None => ("reading…".to_string(), dim),
    };
    for line in tc::wrap_words(&cmdline.0, width.saturating_sub(3).max(1)) {
        rows.push(tc::seg(&[(cmdline.1, format!("   {line}"))], width));
    }
    if proc.state == 'D' {
        rows.push(String::new());
        for line in tc::wrap_words(
            "D: in an uninterruptible wait, almost always on disk or a network filesystem; the disk widget shows who is waiting",
            width.saturating_sub(1).max(1),
        ) {
            rows.push(tc::seg(&[(dim, format!(" {line}"))], width));
        }
    }
    Drawn {
        rows,
        placed: Vec::new(),
        head,
        pin: head,
        keys: Vec::new(),
        paths: Vec::new(),
        plots: Vec::new(),
    }
}

/// When a process started: `running` was measured when the sample was
/// taken, `age` ago, so the start is that much further back than `now`.
/// Without the age the start crept forward by a second every second
/// between polls and jumped back at each one.
pub fn started_at(
    now: chrono::DateTime<chrono::Local>,
    age: Option<Duration>,
    running: f64,
) -> chrono::DateTime<chrono::Local> {
    let age = age.map(|a| a.as_millis() as i64).unwrap_or(0);
    now - chrono::Duration::milliseconds(age + (running * 1000.0) as i64)
}

// ── the footer ───────────────────────────────────────────────────────

pub struct Keys {
    pub screen: Screen,
    pub sort: Sort,
    pub typing: bool,
    pub all_users: bool,
    /// `[t]`: `Some(on)` where threads can be listed, `None` on macOS,
    /// where the hint is not drawn.
    pub threads: Option<bool>,
    pub filtered: bool,
    /// `[g]`: `Some(in the services view)` on Linux, `None` on macOS, where
    /// the hint is not drawn.
    pub services: Option<bool>,
    pub svc_sort: services::Sort,
}

pub fn footer(width: usize, keys: &Keys, p: &Palette) -> (tc::Footer, usize) {
    let dim = p.dim.as_str();
    let mut hints: Vec<Vec<(&str, String)>> = Vec::new();
    if keys.typing {
        hints.push(vec![(dim, "type to filter".into())]);
        hints.push(vec![(dim, "[↵] keep".into())]);
        hints.push(vec![(dim, "esc clear".into())]);
        hints.push(vec![(dim, "↑↓ select".into())]);
    } else if keys.screen != Screen::List {
        hints.push(vec![(dim, "esc back".into())]);
        hints.push(vec![(dim, "[r]efresh".into())]);
    } else if keys.services == Some(true) {
        hints.push(vec![(dim, "↑↓ select".into())]);
        hints.push(vec![(dim, "[↵] its processes".into())]);
        hints.push(vec![(dim, keys.svc_sort.hint())]);
        hints.push(vec![(dim, "[/] filter".into())]);
        hints.push(vec![(dim, users_hint(keys.all_users).into())]);
        hints.push(vec![(dim, services_hint(true).into())]);
        if keys.filtered {
            hints.push(vec![(dim, "esc clear filter".into())]);
        }
        hints.push(vec![(dim, "[r]efresh".into())]);
    } else {
        hints.push(vec![(dim, "↑↓ select".into())]);
        hints.push(vec![(dim, "[↵] detail".into())]);
        hints.push(vec![(dim, keys.sort.hint())]);
        hints.push(vec![(dim, "[/] filter".into())]);
        hints.push(vec![(dim, users_hint(keys.all_users).into())]);
        if let Some(on) = keys.threads {
            hints.push(vec![(dim, threads_hint(on).into())]);
        }
        if keys.services.is_some() {
            hints.push(vec![(dim, services_hint(false).into())]);
        }
        if keys.filtered {
            hints.push(vec![(dim, "esc clear filter".into())]);
        }
        hints.push(vec![(dim, "[r]efresh".into())]);
    }
    if !keys.typing {
        hints.push(vec![(dim, "[,] settings".into())]);
        hints.push(vec![(dim, tc::PALETTE_HINT.into())]);
        hints.push(vec![(dim, tc::THEME_HINT.into())]);
        hints.push(vec![(dim, "[q]uit".into())]);
    }
    let widest = hints
        .iter()
        .map(|hint| {
            hint.iter()
                .map(|(_, t)| tc::display_width(t))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    let indent = usize::from(widest + 1 <= width);
    let room = width.saturating_sub(indent + 1).max(1);
    let mut packed = tc::pack_hints_placed(&hints, room, "  ");
    packed.lines = packed
        .lines
        .into_iter()
        .map(|line| format!("{}{}", " ".repeat(indent), line))
        .collect();
    (packed, indent)
}

/// `[t]`: the state the next press moves to.
pub fn threads_hint(on: bool) -> &'static str {
    if on {
        "[t] hide threads"
    } else {
        "[t] show threads"
    }
}

/// `[g]`: the view the next press moves to.
pub fn services_hint(in_services: bool) -> &'static str {
    if in_services {
        "[g] by process"
    } else {
        "[g] by service"
    }
}

/// `[u]`: the state the next press moves to.
pub fn users_hint(all_users: bool) -> &'static str {
    if all_users {
        "[u] only yours"
    } else {
        "[u] all users"
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_start_time_holds_still_as_the_sample_ages() {
        let taken = chrono::Local::now();
        let at_sample = super::started_at(taken, Some(Duration::ZERO), 90.0);
        let later = taken + chrono::Duration::milliseconds(1500);
        let after = super::started_at(later, Some(Duration::from_millis(1500)), 90.0);
        assert_eq!(at_sample, after);
        assert_eq!(taken - at_sample, chrono::Duration::seconds(90));
    }

    use super::super::model::fixtures::{proc, sample};
    use super::*;

    fn plain(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn view<'a>(sample: Option<&'a Sample>) -> View<'a> {
        View {
            sample,
            err: "",
            age: Some(Duration::from_secs(2)),
            every: 2.0,
            sort: Sort::Cpu,
            needle: "",
            typing: false,
            all_users: true,
            show_kernel: false,
            threads: false,
            cursor: None,
            opened: None,
            svc_sort: services::Sort::Cpu,
            svc_cursor: None,
            svc_opened: None,
            svc_max: None,
        }
    }

    fn table() -> Sample {
        let mut long = proc(
            48_213,
            "example-worker-with-a-rather-long-name",
            "user-a",
            Some(61.2),
            1_300 << 20,
        );
        long.cgroup = Some("/user.slice/user-1000.slice/session-2.scope".into());
        let mut db = proc(1_204, "example-db", "service-b", Some(4.4), 3_900 << 20);
        db.state = 'D';
        let mut zombie = proc(51_020, "(a) b (c)", "user-a", None, 0);
        zombie.state = 'Z';
        sample(vec![
            long,
            db,
            zombie,
            proc(1, "init", "root", Some(0.0), 12 << 20),
        ])
    }

    /// Every character of `want` appears in `text`, in order: a wrapped
    /// command has the other columns between its pieces, but loses none
    /// of itself.
    fn in_order(text: &str, want: &str) -> bool {
        let mut chars = text.chars();
        want.chars().all(|w| chars.any(|c| c == w))
    }

    fn flat(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn text(drawn: &Drawn) -> String {
        drawn
            .rows
            .iter()
            .map(|r| plain(r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn columns_leave_in_the_stated_order_as_the_pane_narrows() {
        let at = |w| columns(w, 6, 38, 9, 16);
        let wide = at(140);
        assert_eq!(
            wide.names(),
            vec![
                "PID", "COMMAND", "USER", "%CPU", "MEM", "RSS", "STATE", "RUNNING", "CGROUP"
            ]
        );
        assert_eq!(wide.cmd, 38, "COMMAND takes all it wants first");
        assert_eq!(wide.cgroup, Some(16), "then CGROUP");
        assert_eq!(at(100).names(), wide.names());
        assert_eq!(at(80).names(), wide.names());
        assert_eq!(
            at(60).names(),
            vec!["PID", "COMMAND", "USER", "%CPU", "MEM", "RSS", "STATE"]
        );
        assert_eq!(at(40).names(), vec!["PID", "COMMAND", "%CPU", "MEM", "RSS"]);
        assert_eq!(at(28).names(), vec!["PID", "COMMAND", "%CPU", "RSS"]);
        // The full order, one column at a time.
        let mut seen = Vec::new();
        for w in (20..=160).rev() {
            let names = at(w).names();
            if seen.last() != Some(&names) {
                seen.push(names);
            }
        }
        let dropped: Vec<&str> = seen
            .windows(2)
            .flat_map(|pair| {
                pair[0]
                    .iter()
                    .filter(|n| !pair[1].contains(n))
                    .copied()
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(dropped, vec!["CGROUP", "RUNNING", "STATE", "USER", "MEM"]);
    }

    #[test]
    fn every_row_fits_and_a_long_command_wraps_whole() {
        let s = table();
        for width in [28, 40, 60, 80, 100, 140] {
            let mut v = view(Some(&s));
            v.cursor = Some(48_213);
            let drawn = draw_list(&v, width, &palette(), None);
            for line in &drawn.rows {
                assert!(
                    tc::display_width(&plain(line)) <= width,
                    "{width}: {} cells: {:?}",
                    tc::display_width(&plain(line)),
                    plain(line)
                );
            }
            let all = text(&drawn);
            assert!(
                in_order(&all, "example-worker-with-a-rather-long-name"),
                "{width}: the command was cut:\n{all}"
            );
            assert!(in_order(&all, "(a) b (c)"), "{width}:\n{all}");
            // The wrapped item is one item, clicked as one.
            let rows_of_long: Vec<usize> = drawn
                .placed
                .iter()
                .filter(|(_, i)| *i == 0)
                .map(|(r, _)| *r)
                .collect();
            if width < 60 {
                assert!(rows_of_long.len() > 1, "{width}: {rows_of_long:?}");
            }
            assert_eq!(drawn.keys[0], 48_213, "sorted by cpu");
        }
    }

    #[test]
    fn a_wrapped_command_is_its_column_read_down() {
        let s = table();
        let long = &s.procs[0];
        for width in [28, 40, 60] {
            let cols = columns(width, 6, 38, 9, 16);
            let lines = row(long, &cols, 8, width, true, &palette());
            assert!(lines.len() > 1, "{width}");
            let column: String = lines
                .iter()
                .map(|l| {
                    plain(l)
                        .chars()
                        .skip(cols.pid + 1)
                        .take(cols.cmd)
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect();
            assert_eq!(column, long.comm, "{width}");
        }
    }

    #[test]
    fn the_header_names_the_window_and_the_platform_labels_differ() {
        let linux = cpu_label(Clock::Measured { window: Some(2.0) }, 8, 2.0).join(" · ");
        let mac = cpu_label(Clock::Decaying, 8, 2.0).join(" · ");
        assert_eq!(linux, "%CPU of all 8 cores over the last 2s");
        assert_eq!(
            mac,
            "%CPU = macOS decaying average (ps), not the last 2s · of all 8 cores"
        );
        assert_ne!(linux, mac);
        let first = cpu_label(Clock::Measured { window: None }, 8, 2.0).join("");
        assert!(first.contains("next sample"), "{first}");

        let s = table();
        let drawn = draw_list(&view(Some(&s)), 100, &palette(), None);
        let t = text(&drawn);
        assert!(
            t.contains("── MACHINE ── sampled 2s ago · every 2s ──"),
            "{t}"
        );
        assert!(
            flat(&t).contains("1 zombie · %CPU of all 8 cores over the last 2s"),
            "the window is on the list's own rule:\n{t}"
        );
        assert!(t.contains("1 zombie"), "{t}");
        assert!(t.contains("D = waiting on disk, 1 shown now"), "{t}");
        assert!(
            !t.contains(" shown ·"),
            "nothing filtered, so no filter row:\n{t}"
        );
    }

    #[test]
    fn filters_show_in_the_body_and_an_empty_result_says_why() {
        let s = table();
        let mut v = view(Some(&s));
        v.needle = "example";
        v.all_users = false;
        let t = text(&draw_list(&v, 100, &palette(), None));
        assert!(t.contains("1 of 4 shown · /example · yours"), "{t}");
        v.needle = "nothing-matches-this";
        let drawn = draw_list(&v, 100, &palette(), None);
        let t = text(&drawn);
        assert!(
            t.contains("nothing matches · all 4 hidden by the filter"),
            "{t}"
        );
        assert!(drawn.keys.is_empty() && drawn.placed.is_empty());
    }

    #[test]
    fn hidepid_says_visible() {
        let mut s = table();
        s.hidepid = true;
        let mut v = view(Some(&s));
        v.needle = "example";
        let t = text(&draw_list(&v, 120, &palette(), None));
        assert!(t.contains("2 of 4 visible shown · /example"), "{t}");
        assert!(
            t.contains("── PROCESSES ── 4 visible (hidepid hides other users')"),
            "{t}"
        );
    }

    #[test]
    fn kernel_threads_hidden_are_counted() {
        let mut s = table();
        let mut k = proc(2, "kthreadd", "root", Some(0.0), 0);
        k.kernel = true;
        s.procs.push(k);
        let t = text(&draw_list(&view(Some(&s)), 120, &palette(), None));
        assert!(t.contains("1 kernel thread hidden"), "{t}");
        assert!(t.contains("── PROCESSES ── 4 ·"), "{t}");
    }

    #[test]
    fn a_dead_poller_is_a_row() {
        let s = table();
        let mut v = view(Some(&s));
        v.err = "stopped: poller panicked";
        let t = text(&draw_list(&v, 60, &palette(), None));
        assert!(t.contains("! stopped: poller panicked"), "{t}");
        let mut v = view(None);
        v.err = "processes unavailable: /proc: permission denied";
        let t = text(&draw_list(&v, 60, &palette(), None));
        assert!(t.contains("processes unavailable"), "{t}");
    }

    #[test]
    fn the_detail_shows_a_rate_only_when_readable() {
        let s = table();
        let mut open = Opened {
            pid: 1_204,
            start: Some(1_204),
            last: s.procs[1].clone(),
            gone: false,
            facts: None,
        };
        let facts = |io: Io| Facts {
            pid: 1_204,
            start: Some(1_204),
            cmdline: Ok("example-db --data /srv/example".into()),
            cwd: Err("needs root for another user's process".into()),
            gid: Some(0),
            io,
        };
        for (io, want) in [
            (
                Io::Rate {
                    read: 2048.0,
                    write: 3.0 * 1048576.0,
                    over: 2.0,
                },
                "read 2K/s · write 3M/s, over the last 2s",
            ),
            (
                Io::NeedsRoot,
                "disk I/O needs root for another user's process",
            ),
            (Io::NotRead, "disk I/O not read on macOS in this version"),
            (Io::Measuring, "measuring"),
        ] {
            open.facts = Some(facts(io));
            let mut v = view(Some(&s));
            v.opened = Some(&open);
            for width in [28, 40, 60, 100] {
                let drawn = draw_detail(&v, width, &palette(), None);
                let t = text(&drawn);
                for line in &drawn.rows {
                    assert!(
                        tc::display_width(&plain(line)) <= width,
                        "{width}: {:?}",
                        plain(line)
                    );
                }
                assert!(flat(&t).contains(want), "{width}: {want}\n{t}");
                assert!(
                    flat(&t).contains("waiting on disk"),
                    "D is named in the detail:\n{t}"
                );
                assert!(drawn.placed.is_empty(), "the detail has no rows to click");
            }
        }
    }

    #[test]
    fn an_exited_process_keeps_its_last_reading_and_says_so() {
        let s = table();
        let open = Opened {
            pid: 48_213,
            start: Some(48_213),
            last: s.procs[0].clone(),
            gone: true,
            facts: Some(Facts {
                pid: 48_213,
                start: Some(48_213),
                cmdline: Ok("example-worker --once".into()),
                cwd: Ok("/srv/example".into()),
                gid: Some(1000),
                io: Io::Rate {
                    read: 0.0,
                    write: 1.0,
                    over: 2.0,
                },
            }),
        };
        let mut v = view(Some(&s));
        v.opened = Some(&open);
        let t = flat(&text(&draw_detail(&v, 80, &palette(), None)));
        assert!(t.contains("exited: this is how it looked"), "{t}");
        assert!(t.contains("example-worker --once"), "{t}");
        assert!(
            t.contains("disk I/O not read: the process has exited"),
            "{t}"
        );
    }

    #[test]
    fn hints_name_the_next_state() {
        assert_eq!(users_hint(true), "[u] only yours");
        assert_eq!(users_hint(false), "[u] all users");
        assert_eq!(threads_hint(false), "[t] show threads");
        assert_eq!(threads_hint(true), "[t] hide threads");
        assert_eq!(services_hint(false), "[g] by service");
        assert_eq!(services_hint(true), "[g] by process");
    }

    /// Linux offers `g` and names the other view; macOS, with no cgroups,
    /// draws no `g` at all. The services view has its own enter and sort.
    #[test]
    fn the_services_hint_is_per_platform() {
        let footer_text = |services: Option<bool>| {
            let keys = Keys {
                screen: Screen::List,
                sort: Sort::Cpu,
                typing: false,
                all_users: true,
                threads: services.map(|_| false),
                filtered: false,
                services,
                svc_sort: services::Sort::Memory,
            };
            footer(300, &keys, &palette())
                .0
                .lines
                .iter()
                .map(|l| plain(l))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let linux = footer_text(Some(false));
        assert!(
            linux.contains("[g] by service") && linux.contains("[↵] detail"),
            "{linux}"
        );
        let services = footer_text(Some(true));
        assert!(services.contains("[g] by process"), "{services}");
        assert!(services.contains("[↵] its processes"), "{services}");
        assert!(services.contains("[s] sort by procs"), "{services}");
        assert!(!services.contains("[t]"), "{services}");
        let mac = footer_text(None);
        assert!(!mac.contains("[g]"), "{mac}");
        // The platform's own hosts agree with what they read.
        assert_eq!(
            super::super::host::READS_SERVICES,
            cfg!(target_os = "linux")
        );
    }

    use super::super::model::{Machine, Memory, Swap, fixtures::machine};

    fn with_machine(cores: usize, m: Machine) -> Sample {
        let mut s = table();
        s.cores = cores;
        s.machine = m;
        s
    }

    fn mac() -> Sample {
        let mut s = with_machine(
            8,
            Machine {
                per_core: PerCore::NotRead,
                memory: Some(Memory::from_parts(
                    [9 << 30, 5 << 30, 2 << 30],
                    MemSource::VmStat,
                )),
                ..machine(8)
            },
        );
        s.clock = Clock::Decaying;
        s
    }

    fn bars(panel: &Panel) -> Vec<&tc::Plot> {
        panel
            .plots
            .iter()
            .filter(|p| p.height == BAR_ROWS && p.chart.traces.is_empty())
            .collect()
    }

    fn one_row(panel: &Panel) -> usize {
        panel.plots.iter().filter(|p| p.height == 1).count()
    }

    fn panel_text(panel: &Panel) -> String {
        flat(
            &panel
                .rows
                .iter()
                .map(|r| plain(r))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    #[test]
    fn every_panel_row_fits_and_every_plot_sits_inside_it() {
        let mut waiting = machine(8);
        waiting.per_core = PerCore::Waiting;
        waiting.busy = None;
        let mut broken = machine(8);
        broken.problems =
            vec!["/proc/stat unavailable: /proc/stat: Permission denied (os error 13)".into()];
        broken.memory = None;
        let mut no_swap = machine(8);
        no_swap.swap = Some(Swap { total: 0, used: 0 });
        let samples = [
            with_machine(8, machine(8)),
            with_machine(2, machine(2)),
            with_machine(4, machine(4)),
            with_machine(64, machine(64)),
            with_machine(8, waiting),
            with_machine(8, broken),
            with_machine(8, no_swap),
            mac(),
        ];
        for s in &samples {
            for width in [28, 40, 60, 80, 100, 140] {
                let panel = panel(s, 2.0, width, &palette());
                for line in &panel.rows {
                    assert!(
                        tc::display_width(&plain(line)) <= width,
                        "{width} cells, {} cores: {:?}",
                        s.cores,
                        plain(line)
                    );
                }
                for plot in &panel.plots {
                    assert!(plot.x + plot.width <= width, "{width}: {plot:?}");
                    assert!(plot.y + plot.height <= panel.rows.len());
                    // The cells under a plot are left blank for it.
                    for row in plot.y..plot.y + plot.height {
                        let cells: String = plain(&panel.rows[row])
                            .chars()
                            .skip(plot.x)
                            .take(plot.width)
                            .collect();
                        assert!(cells.trim().is_empty(), "{width}: {cells:?}");
                    }
                }
                let mut v = view(Some(s));
                v.cursor = Some(48_213);
                let drawn = draw_list(&v, width, &palette(), None);
                for line in &drawn.rows {
                    assert!(tc::display_width(&plain(line)) <= width, "{width}");
                }
                assert_eq!(drawn.plots.len(), panel.plots.len());
            }
        }
    }

    #[test]
    fn four_cores_draw_a_bar_each_and_two_draw_one_meter() {
        let four = panel(&with_machine(4, machine(4)), 2.0, 80, &palette());
        let b = bars(&four);
        assert_eq!(b.len(), 1, "one bar chart");
        // Four cores at four cells each and a gap between them.
        assert_eq!(b[0].width, 4 * 4 + 3);
        let two = panel(&with_machine(2, machine(2)), 2.0, 80, &palette());
        assert!(bars(&two).is_empty(), "two cores is a meter of the total");
        // The CPU meter, the memory bar and the swap meter.
        assert_eq!(one_row(&two), 3);
        assert_eq!(one_row(&four), 2, "memory and swap only");
        // Too many cores for the width is a meter too.
        let many = panel(&with_machine(64, machine(64)), 2.0, 40, &palette());
        assert!(bars(&many).is_empty());
        assert_eq!(bar_shape(64, 38), None);
        assert_eq!(bar_shape(8, 38), Some((3, 1)));
        assert_eq!(bar_shape(8, 9), Some((1, 0)));
        // Extra width buys wider bars, and their figures under them.
        let t = panel_text(&panel(&with_machine(8, machine(8)), 2.0, 100, &palette()));
        assert!(t.contains("92 10 20 30 40 50 60 70"), "{t}");
        assert!(t.contains("busiest cpu0 92%"), "{t}");
    }

    #[test]
    fn the_panel_names_cpu_load_memory_and_swap() {
        let s = with_machine(8, machine(8));
        let t = panel_text(&panel(&s, 2.0, 100, &palette()));
        assert!(
            t.contains("CPU 8 cores · 41% busy · load 3.90 2.80 2.10 (of 8)"),
            "{t}"
        );
        assert!(t.contains("MEMORY 31G"), "{t}");
        assert!(t.contains("used 18G · reclaimable 6G · free 7G"), "{t}");
        assert!(t.contains("used = total − MemAvailable"), "{t}");
        assert!(t.contains("SWAP 4G"), "{t}");
        assert!(t.contains("1.2G used"), "{t}");
        assert!(t.contains("LOAD 1-minute average, last 10 min"), "{t}");
        assert!(
            t.contains("8 cores┤"),
            "the core count marks the axis:\n{t}"
        );
        let mut none = machine(8);
        none.swap = Some(Swap {
            total: 4 << 30,
            used: 0,
        });
        let t = panel_text(&panel(&with_machine(8, none), 2.0, 100, &palette()));
        assert!(t.contains("none used"), "{t}");
        let mut off = machine(8);
        off.swap = Some(Swap { total: 0, used: 0 });
        let t = panel_text(&panel(&with_machine(8, off), 2.0, 100, &palette()));
        assert!(t.contains("SWAP no swap"), "{t}");
        // A file that could not be read is named, and the table still draws.
        let mut broken = machine(8);
        broken.problems = vec!["/proc/stat unavailable: example".into()];
        let s = with_machine(8, broken);
        let drawn = draw_list(&view(Some(&s)), 100, &palette(), None);
        let t = flat(&text(&drawn));
        assert!(t.contains("! /proc/stat unavailable: example"), "{t}");
        assert_eq!(drawn.keys.len(), 4, "the table is there");
    }

    #[test]
    fn the_platform_lines_differ() {
        let macos = panel_text(&panel(&mac(), 2.0, 100, &palette()));
        let linux = panel_text(&panel(&with_machine(8, machine(8)), 2.0, 100, &palette()));
        let per_core = "per-core CPU not read on macOS (needs host_processor_info)";
        assert!(macos.contains(per_core), "{macos}");
        assert!(!linux.contains(per_core), "{linux}");
        assert!(macos.contains("from vm_stat"), "{macos}");
        assert!(!linux.contains("vm_stat"), "{linux}");
        assert!(macos.contains("41% decaying average (ps)"), "{macos}");
        assert!(!linux.contains("decaying"), "{linux}");
        assert!(linux.contains("MemAvailable") && !macos.contains("MemAvailable"));
        // No bars on macOS: the line says why instead.
        assert!(bars(&panel(&mac(), 2.0, 100, &palette())).is_empty());
    }

    #[test]
    fn load_is_calm_to_the_core_count_and_red_at_twice_it() {
        assert_eq!(load_heat(0.0, 8.0), 0.0);
        assert!(load_heat(8.0, 8.0) < 0.5, "a full machine is not yet hot");
        assert!(load_heat(4.0, 8.0) < load_heat(8.0, 8.0));
        assert!(load_heat(12.0, 8.0) > 0.5);
        assert_eq!(load_heat(16.0, 8.0), 1.0);
        assert_eq!(load_heat(40.0, 8.0), 1.0);
        // Rising all the way: a ramp that is the same colour for two loads
        // says nothing about which is higher.
        let mut last = -1.0;
        for tenth in 0..=200 {
            let h = load_heat(tenth as f64 / 10.0, 8.0);
            assert!(h >= last);
            last = h;
        }
    }

    #[test]
    fn the_load_trace_holds_ten_minutes_and_is_under_its_text() {
        let s = with_machine(8, machine(8));
        let panel = panel(&s, 2.0, 100, &palette());
        let trace: Vec<&tc::Plot> = panel
            .plots
            .iter()
            .filter(|p| !p.chart.traces.is_empty())
            .collect();
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].chart.slots, 300, "600 s at one slot every 2 s");
        assert_eq!(trace[0].height, 4);
        // Five-second polls: 120 slots.
        let slow = super::panel(&s, 5.0, 100, &palette());
        let slots = slow
            .plots
            .iter()
            .find(|p| !p.chart.traces.is_empty())
            .map(|p| p.chart.slots);
        assert_eq!(slots, Some(300), "never fewer slots than samples held");
        let mut short = s.clone();
        short.history.truncate(30);
        let t = panel_text(&super::panel(&short, 2.0, 100, &palette()));
        assert!(t.contains("60s so far"), "{t}");
        // The latest reading is text at the line's end, in both modes.
        let latest = load_text(s.history.last().unwrap().unwrap());
        assert!(panel_text(&panel).contains(&latest), "{latest}");
    }

    #[test]
    fn threads_list_under_their_process_and_are_not_counted() {
        use super::super::model::Thread;
        let mut s = table();
        let thread = |tid: i32, comm: &str, cpu: f64| Thread {
            tid,
            comm: comm.into(),
            state: 'S',
            cpu: Some(cpu),
            running: Some(60.0),
        };
        s.threads = Some(
            [(
                48_213,
                vec![
                    thread(48_213, "example-worker", 40.0),
                    thread(4_194_301, "example-io", 21.0),
                ],
            )]
            .into_iter()
            .collect(),
        );
        let mut v = view(Some(&s));
        let without = draw_list(&v, 100, &palette(), None);
        assert!(!text(&without).contains("example-io"), "off by default");
        v.threads = true;
        for width in [28, 40, 60, 80, 100, 140] {
            let drawn = draw_list(&v, width, &palette(), None);
            for line in &drawn.rows {
                assert!(tc::display_width(&plain(line)) <= width, "{width}");
            }
            assert_eq!(drawn.keys.len(), 4, "threads are not items of their own");
            let t = text(&drawn);
            assert!(in_order(&t, "└ example-io"), "{width}:\n{t}");
            assert!(t.contains("4194301"), "a tid wider than any pid fits:\n{t}");
        }
        let t = flat(&text(&draw_list(&v, 100, &palette(), None)));
        assert!(
            t.contains("── PROCESSES ── 4 ·"),
            "the count is of processes:\n{t}"
        );
        assert!(t.contains("2 threads listed under their processes"), "{t}");
        // The thread rows sit under their process and belong to its item.
        let drawn = draw_list(&v, 100, &palette(), None);
        let rows: Vec<usize> = drawn
            .placed
            .iter()
            .filter(|(_, i)| *i == 0)
            .map(|(r, _)| *r)
            .collect();
        assert!(rows.len() >= 3, "the process and its two threads: {rows:?}");
        let last = plain(&drawn.rows[*rows.last().unwrap()]);
        assert!(last.contains("example-io"), "{last}");
        assert!(!plain(&drawn.rows[rows[0]]).contains("└"));
        let keys = |threads| Keys {
            screen: Screen::List,
            sort: Sort::Cpu,
            typing: false,
            all_users: true,
            threads,
            filtered: false,
            services: None,
            svc_sort: services::Sort::Cpu,
        };
        let hints = |threads| {
            footer(200, &keys(threads), &palette())
                .0
                .lines
                .iter()
                .map(|l| plain(l))
                .collect::<String>()
        };
        assert!(hints(Some(false)).contains("[t] show threads"));
        assert!(hints(Some(true)).contains("[t] hide threads"));
        assert!(!hints(None).contains("[t]"), "macOS has no threads to show");
    }

    /// A sample with `n` processes, enough to scroll a table under a
    /// pinned head; memory falls with the index so the order is fixed.
    fn many(n: usize) -> Sample {
        sample(
            (0..n)
                .map(|i| {
                    proc(
                        1_000 + i as i32,
                        &format!("worker-{i}"),
                        "user-a",
                        Some(50.0 - i as f64),
                        (500 - i as u64) << 20,
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn the_head_pins_only_while_the_list_keeps_its_share() {
        // 38 rows: the list wants a third, 12.
        assert!(pin_head(38, 26, None), "12 left: pinned");
        assert!(!pin_head(38, 27, None), "11 left: the whole body scrolls");
        // Once pinned, a head that grows by a row or three holds.
        assert!(pin_head(38, 27, Some(true)));
        assert!(pin_head(38, 29, Some(true)));
        assert!(!pin_head(38, 30, Some(true)), "past the slack it lets go");
        // Once let go, it pins again only at the line itself.
        assert!(!pin_head(38, 27, Some(false)));
        assert!(pin_head(38, 26, Some(false)));
        // A short pane: never fewer than LIST_MIN rows of list.
        assert!(pin_head(12, 6, None));
        assert!(!pin_head(12, 7, None));
        assert!(!pin_head(4, 1, None), "no room for a list at all");
        // The decision is a function of its inputs and nothing else.
        for room in 0..80 {
            for pin in 0..80 {
                assert_eq!(pin_head(room, pin, None), pin_head(room, pin, None));
            }
        }
    }

    #[test]
    fn pinned_the_wheel_moves_the_table_and_nothing_above_it() {
        let s = many(40);
        let drawn = draw_list(&view(Some(&s)), 100, &palette(), None);
        assert!(drawn.pin > drawn.head, "the list has a head to pin");
        assert!(!drawn.plots.is_empty(), "the panel draws its charts");
        // The column header is the last pinned row.
        assert!(
            plain(&drawn.rows[drawn.pin - 1]).contains("PID"),
            "{}",
            text(&drawn)
        );
        let room = drawn.pin + 15;
        let (pinned, head, below) = layout(&drawn, room, None);
        assert!(pinned);
        assert_eq!((head, below), (drawn.pin, 15));
        let at = |scroll: usize| {
            let scroll = clamp_scroll(&drawn, head, scroll, below);
            let rows = visible(&drawn, head, scroll, below);
            let plots: Vec<tc::Plot> = drawn
                .plots
                .iter()
                .cloned()
                .filter_map(|p| p.in_viewport(scroll, head, below))
                .collect();
            (scroll, rows, plots)
        };
        let (_, top, top_plots) = at(0);
        let (scroll, moved, moved_plots) = at(3);
        assert_eq!(scroll, 3);
        assert_eq!(top[..head], moved[..head], "the head did not move");
        assert_eq!(top_plots, moved_plots, "nor did its charts");
        assert_eq!(top_plots.len(), drawn.plots.len(), "every chart is drawn");
        assert_eq!(moved[head], drawn.rows[head + 3], "the table did");
        assert_eq!(moved.len(), head + below);
        // To the end and no further.
        let (end, rows, _) = at(usize::MAX / 4);
        assert_eq!(end, drawn.rows.len() - head - below);
        assert_eq!(rows.last(), drawn.rows.last());
    }

    #[test]
    fn a_short_pane_scrolls_the_whole_body_and_hides_nothing() {
        let s = many(40);
        let drawn = draw_list(&view(Some(&s)), 100, &palette(), None);
        let room = drawn.pin + 3;
        let (pinned, head, below) = layout(&drawn, room, None);
        assert!(!pinned);
        assert_eq!((head, below), (1, room - 1), "only the title stays");
        // Every row of the body is reachable at some scroll.
        let last = clamp_scroll(&drawn, head, usize::MAX / 4, below);
        let mut seen = std::collections::BTreeSet::new();
        for scroll in 0..=last {
            for (i, _) in visible(&drawn, head, scroll, below)
                .iter()
                .enumerate()
                .skip(head)
            {
                seen.insert(scroll + i);
            }
        }
        assert_eq!(seen.len(), drawn.rows.len() - head);
        // The panel's charts go with the text, and are cropped by the window.
        let gone: Vec<_> = drawn
            .plots
            .iter()
            .cloned()
            .filter_map(|p| p.in_viewport(last, head, below))
            .collect();
        assert!(gone.len() < drawn.plots.len());
    }

    #[test]
    fn a_click_lands_on_the_row_drawn_there_in_both_modes() {
        let s = many(40);
        let drawn = draw_list(&view(Some(&s)), 100, &palette(), None);
        for room in [drawn.pin + 15, drawn.pin + 3] {
            let (_, head, below) = layout(&drawn, room, None);
            for scroll in [0, 2, 7] {
                let scroll = clamp_scroll(&drawn, head, scroll, below);
                let rows = visible(&drawn, head, scroll, below);
                for (y, row) in rows.iter().enumerate() {
                    let item = tc::item_at(y, head, scroll, &drawn.placed);
                    if y < head {
                        assert_eq!(item, None, "a pinned row is no item");
                        continue;
                    }
                    // The item is the process whose pid the row shows.
                    if let Some(item) = item {
                        let pid = drawn.keys[item].to_string();
                        assert!(
                            plain(row).split_whitespace().next() == Some(pid.as_str()),
                            "room {room} scroll {scroll} y {y}: {:?} is not {pid}",
                            plain(row)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_list_and_the_machine_each_have_a_heading() {
        let s = table();
        for width in [28, 40, 60, 80, 100, 140] {
            let drawn = draw_list(&view(Some(&s)), width, &palette(), None);
            let t = text(&drawn);
            assert!(t.contains("── MACHINE ──"), "{width}:\n{t}");
            assert!(t.contains("── PROCESSES ──"), "{width}:\n{t}");
            // Every clause is there, on the rule or under it, whole.
            let f = flat(&t);
            for clause in ["sampled 2s ago", "every 2s", "sorted by %CPU", "1 zombie"] {
                assert!(f.contains(clause), "{width}: {clause}:\n{t}");
            }
            assert!(
                in_order(&f, "%CPU of all 8 cores over the last 2s"),
                "{width}:\n{t}"
            );
            let heading = drawn
                .rows
                .iter()
                .position(|r| plain(r).contains("── PROCESSES"))
                .unwrap();
            assert!(
                plain(&drawn.rows[heading - 1]).is_empty(),
                "a blank line before"
            );
            assert!(heading < drawn.pin, "the heading pins with the head");
        }
    }

    #[test]
    fn a_group_rule_fits_and_keeps_every_clause() {
        let clauses: Vec<String> = vec![
            "580".into(),
            "sorted by running, newest first".into(),
            "a-clause-with-no-spaces-wider-than-a-narrow-pane-by-far".into(),
            "%CPU of all 8 cores over the last 2s".into(),
        ];
        for width in [28, 40, 60, 80, 100, 140] {
            let mut rows = Vec::new();
            push_group("PROCESSES", &clauses, width, &palette(), &mut rows);
            assert!(plain(&rows[0]).is_empty());
            let rule = plain(&rows[1]);
            assert!(rule.starts_with(" ── PROCESSES"), "{width}: {rule:?}");
            assert!(rule.ends_with('─'), "{width}: the rule runs on: {rule:?}");
            for row in &rows {
                assert!(
                    tc::display_width(&plain(row)) <= width,
                    "{width}: {:?}",
                    plain(row)
                );
            }
            let all: String = rows.iter().map(|r| plain(r)).collect::<Vec<_>>().join(" ");
            for clause in &clauses {
                assert!(in_order(&all, clause), "{width}: {clause} was cut:\n{all}");
            }
        }
    }

    #[test]
    fn numbers_fit_their_columns() {
        for n in [
            0u64,
            900,
            1023,
            1024,
            12 << 10,
            640 << 20,
            1_300 << 20,
            u64::MAX,
        ] {
            assert!(human(n).len() <= RSS_W, "{n}: {}", human(n));
        }
        assert!(mem_cell(Some(99.99)).len() <= MEM_W);
        assert!(mem_cell(Some(12.34)).len() <= MEM_W);
        assert!(cpu_cell(Some(100.0)).len() <= CPU_W);
        assert_eq!(secs(2.0), "2s");
        assert_eq!(secs(0.5), "0.5s");
        assert_eq!(secs(2.04), "2s");
    }
}
