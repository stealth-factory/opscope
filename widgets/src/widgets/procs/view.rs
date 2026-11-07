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

use super::model::{self, Clock, Facts, Io, Proc, Sample, Sort};

pub struct Palette {
    pub txt: String,
    pub dim: String,
    pub dim_lit: String,
    pub accent: String,
    pub down: String,
    pub bad: String,
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
    /// The pid the list cursor is on.
    pub cursor: Option<i32>,
    pub opened: Option<&'a Opened>,
}

/// One frame's body: the rows, where each list item landed (row, item),
/// how many rows are pinned, and the pids in the order drawn.
pub struct Drawn {
    pub rows: Vec<String>,
    pub placed: Vec<(usize, usize)>,
    pub head: usize,
    pub keys: Vec<i32>,
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

fn cpu_cell(cpu: Option<f64>) -> String {
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
fn pack_clauses(clauses: &[String], width: usize) -> Vec<String> {
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

// ── the list ─────────────────────────────────────────────────────────

const CPU_W: usize = 5;
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

fn right(text: &str, w: usize) -> String {
    let n = tc::display_width(text);
    if n >= w {
        text.to_string()
    } else {
        format!("{}{}", " ".repeat(w - n), text)
    }
}

/// The cells of one row, joined with single spaces and the selected row's
/// tint carried to the right edge.
fn join(cells: &[(String, String)], width: usize, here: bool, tint: &str) -> String {
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

fn header(cols: &Cols, sort: Sort, width: usize, p: &Palette) -> String {
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
fn row(
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

/// The status lines above the table: when, how often, what %CPU means,
/// how many, in what order, and what is not shown.
fn status(
    view: &View,
    sample: &Sample,
    listed: usize,
    zombies: usize,
    hidden: usize,
) -> Vec<String> {
    let mut clauses = vec![
        match view.age {
            Some(age) => format!("sampled {} ago", tc::age(age.as_secs_f64())),
            None => "not sampled yet".to_string(),
        },
        format!("every {}", secs(view.every)),
    ];
    clauses.extend(cpu_label(sample.clock, sample.cores, view.every));
    clauses.push(if sample.hidepid {
        format!("{listed} processes visible (hidepid hides other users')")
    } else {
        format!("{listed} processes")
    });
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

/// The table under the status lines: its rows, where each process landed
/// (row within the table, item), and the pids in the order drawn. The
/// expensive half of a frame, which `main` keeps until something it
/// depends on changes.
#[derive(Clone, Debug, Default)]
pub struct Table {
    pub rows: Vec<String>,
    pub placed: Vec<(usize, usize)>,
    pub keys: Vec<i32>,
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

/// The title and everything above the table: when, how often, what %CPU
/// means, how many, the filter in force and any error.
pub fn list_head(view: &View, width: usize, p: &Palette, busy: Option<usize>) -> Vec<String> {
    let mut rows = vec![tc::title_busy("procs", width, &p.accent, busy)];
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
        return rows;
    };
    let (listed, hidden) = model::listed(sample, view.show_kernel);
    let count = listed
        .iter()
        .filter(|proc| model::passes(proc, view.needle, view.all_users, sample.me))
        .count();
    let (zombies, _) = model::counts(&listed);
    for line in pack_clauses(&status(view, sample, listed.len(), zombies, hidden), width) {
        rows.push(tc::seg(&[(p.dim.as_str(), line)], width));
    }
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
    err_rows(&mut rows);
    rows.push(String::new());
    rows
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
    let digits = shown
        .iter()
        .map(|p| p.pid.to_string().len())
        .max()
        .unwrap_or(3)
        .max(3);
    let cmd_want = shown
        .iter()
        .map(|p| tc::display_width(&p.comm))
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
    for (index, proc) in shown.iter().enumerate() {
        let here = view.cursor == Some(proc.pid);
        let from = rows.len();
        rows.extend(row(proc, &cols, sample.cores, width, here, p));
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

/// The head and the table as one frame body, the title pinned.
pub fn compose(head: Vec<String>, table: &Table) -> Drawn {
    let mut rows = head;
    let offset = rows.len();
    rows.extend(table.rows.iter().cloned());
    Drawn {
        rows,
        placed: table.placed.iter().map(|&(r, i)| (r + offset, i)).collect(),
        head: 1,
        keys: table.keys.clone(),
    }
}

pub fn draw_list(view: &View, width: usize, p: &Palette, busy: Option<usize>) -> Drawn {
    compose(list_head(view, width, p, busy), &list_table(view, width, p))
}

// ── the detail ───────────────────────────────────────────────────────

const LABEL_W: usize = 11;

/// `label  value`, the value wrapped under itself and never cut.
fn field(rows: &mut Vec<String>, label: &str, value: &str, ink: &str, width: usize, p: &Palette) {
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
        let started =
            chrono::Local::now() - chrono::Duration::milliseconds((running * 1000.0) as i64);
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
        keys: Vec::new(),
    }
}

// ── the footer ───────────────────────────────────────────────────────

pub struct Keys {
    pub screen: Screen,
    pub sort: Sort,
    pub typing: bool,
    pub all_users: bool,
    pub filtered: bool,
}

pub fn footer(width: usize, keys: &Keys, p: &Palette) -> (tc::Footer, usize) {
    let dim = p.dim.as_str();
    let mut hints: Vec<Vec<(&str, String)>> = Vec::new();
    if keys.typing {
        hints.push(vec![(dim, "type to filter".into())]);
        hints.push(vec![(dim, "[↵] keep".into())]);
        hints.push(vec![(dim, "esc clear".into())]);
        hints.push(vec![(dim, "↑↓ select".into())]);
    } else if keys.screen == Screen::Detail {
        hints.push(vec![(dim, "esc back".into())]);
        hints.push(vec![(dim, "[r]efresh".into())]);
    } else {
        hints.push(vec![(dim, "↑↓ select".into())]);
        hints.push(vec![(dim, "[↵] detail".into())]);
        hints.push(vec![(dim, keys.sort.hint())]);
        hints.push(vec![(dim, "[/] filter".into())]);
        hints.push(vec![(dim, users_hint(keys.all_users).into())]);
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
            cursor: None,
            opened: None,
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
            t.contains("sampled 2s ago · every 2s · %CPU of all 8 cores over the last 2s"),
            "{t}"
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
        assert!(t.contains("4 processes visible"), "{t}");
    }

    #[test]
    fn kernel_threads_hidden_are_counted() {
        let mut s = table();
        let mut k = proc(2, "kthreadd", "root", Some(0.0), 0);
        k.kernel = true;
        s.procs.push(k);
        let t = text(&draw_list(&view(Some(&s)), 120, &palette(), None));
        assert!(t.contains("1 kernel thread hidden"), "{t}");
        assert!(t.contains("4 processes"), "{t}");
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
