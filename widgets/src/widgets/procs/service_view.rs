// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! What the services view draws (`g`, Linux): one row per cgroup with
//! `cpu` or `memory` enabled, the header saying how deep the accounting
//! goes, and one service's processes through the process table's own row.
//!
//! Like the process table, every row is built for the width it is given:
//! a path too long for SERVICE wraps under itself, and the columns leave in
//! the order PROCS, MEMORY rather than anything being cut.

use opscope_core as tc;

use super::model::{Clock, Proc, Sample, Sort as ProcSort};
use super::services::{self, Service, Services};
use super::view::{
    self, CPU_W, Drawn, Head, Palette, Table, View, chunks, cpu_cell, cpu_label, field, human,
    join, push_group, right, secs, timing,
};

const MEM_W: usize = 6;
const PROCS_W: usize = 5;
const NAME_MIN: usize = 12;

/// Which of the optional columns a width holds, and SERVICE's width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cols {
    pub name: usize,
    pub memory: bool,
    pub procs: bool,
}

impl Cols {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn names(&self) -> Vec<&'static str> {
        let mut out = vec!["SERVICE", "%CPU"];
        if self.memory {
            out.push("MEMORY");
        }
        if self.procs {
            out.push("PROCS");
        }
        out
    }
}

/// PROCS leaves first, then MEMORY, while SERVICE still gets `NAME_MIN`
/// cells or all it wants. SERVICE takes what is left, up to what it wants;
/// anything beyond is blank at the end of the row.
pub fn columns(width: usize, want: usize) -> Cols {
    let want = want.max(1);
    let fixed = |memory: bool, procs: bool| {
        // the mark, then each cell after a space
        1 + 1 + CPU_W + if memory { 1 + MEM_W } else { 0 } + if procs { 1 + PROCS_W } else { 0 }
    };
    let (memory, procs) = [(true, true), (true, false), (false, false)]
        .into_iter()
        .find(|&(m, p)| fixed(m, p) + want.min(NAME_MIN) <= width)
        .unwrap_or((false, false));
    let room = width.saturating_sub(fixed(memory, procs));
    let name = tc::fit_columns(room, &[(1, want)])[0].max(1);
    Cols {
        name,
        memory,
        procs,
    }
}

fn header(cols: &Cols, sort: services::Sort, width: usize, p: &Palette) -> String {
    let sorted = match sort {
        services::Sort::Cpu => "%CPU",
        services::Sort::Memory => "MEMORY",
        services::Sort::Procs => "PROCS",
    };
    let ink = |name: &str| {
        if name == sorted {
            p.accent.clone()
        } else {
            p.dim.clone()
        }
    };
    let mut cells = vec![
        (
            ink("SERVICE"),
            format!(" {}", tc::pad("SERVICE", cols.name)),
        ),
        (ink("%CPU"), right("%CPU", CPU_W)),
    ];
    if cols.memory {
        cells.push((ink("MEMORY"), right("MEMORY", MEM_W)));
    }
    if cols.procs {
        cells.push((ink("PROCS"), right("PROCS", PROCS_W)));
    }
    join(&cells, width, false, "")
}

fn memory_cell(memory: Option<u64>) -> String {
    memory.map(human).unwrap_or_else(|| "—".into())
}

fn row(
    svc: &Service,
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
    let name = chunks(&svc.name(), cols.name);
    let cpu_ink = match svc.cpu {
        Some(cpu) if cpu >= 0.05 => format!(
            "{}{}",
            tint,
            tc::heat_on((cpu * cores as f64 / 100.0).clamp(0.0, 1.0), here)
        ),
        _ => c(&p.dim),
    };
    let mut out = Vec::with_capacity(name.len());
    for (line, piece) in name.iter().enumerate() {
        let first = line == 0;
        let mark = if first && here { "▸" } else { " " };
        let or_blank = |text: String, w: usize| if first { text } else { " ".repeat(w) };
        let mut cells = vec![
            (
                c(if here { &p.accent } else { &p.txt }),
                format!("{mark}{}", tc::pad(piece, cols.name)),
            ),
            (
                cpu_ink.clone(),
                or_blank(right(&cpu_cell(svc.cpu), CPU_W), CPU_W),
            ),
        ];
        if cols.memory {
            cells.push((
                c(&p.txt),
                or_blank(right(&memory_cell(svc.memory), MEM_W), MEM_W),
            ));
        }
        if cols.procs {
            cells.push((
                c(&p.txt),
                or_blank(right(&svc.pids.len().to_string(), PROCS_W), PROCS_W),
            ));
        }
        out.push(join(&cells, width, here, &tint));
    }
    out
}

fn lines(rows: &mut Vec<String>, text: &str, ink: &str, width: usize) {
    for line in tc::wrap_words(text, width.saturating_sub(1).max(1)) {
        rows.push(tc::seg(&[(ink, format!(" {line}"))], width));
    }
}

/// The title, then the `SERVICES` rule and what the table is: when, cgroup
/// v2, complete (or partial, when a cgroup could not be read),
/// the %CPU window, the count, the sort, and how deep the accounting goes
/// as core says it. A named state replaces the table below, never an empty
/// one.
pub fn head(view: &View, width: usize, p: &Palette, busy: Option<usize>) -> Head {
    let mut rows = vec![tc::title_busy("procs", width, &p.accent, busy)];
    let sample = view.sample;
    let mut clauses = timing(view);
    clauses.push("cgroup v2".to_string());
    let mut warn: Option<String> = None;
    let mut count = (0, 0, 0);
    match sample.and_then(|s| s.services.as_ref()) {
        Some(Services::Table(table)) => {
            let unread = table.rows.iter().filter(|r| r.unread).count();
            clauses.push(if unread > 0 { "partial" } else { "complete" }.to_string());
            if table.container {
                clauses.push("rows are this container's cgroups".to_string());
            }
            let cores = sample.map(|s| s.cores).unwrap_or(1);
            clauses.extend(cpu_label(
                Clock::Measured {
                    window: table.window,
                },
                cores,
                view.every,
            ));
            let me = sample.map(|s| s.me).unwrap_or(0);
            let (shown, empty) =
                services::shown(&table.rows, view.needle, view.all_users, me, view.svc_sort);
            count = (shown.len(), table.rows.len(), empty);
            clauses.push(format!(
                "{} cgroup{} with cpu or memory",
                table.rows.len(),
                if table.rows.len() == 1 { "" } else { "s" }
            ));
            clauses.push(view.svc_sort.in_force().to_string());
            let gaps = table.rows.iter().filter(|r| r.gap).count();
            if gaps > 0 {
                clauses.push(format!(
                    "{gaps} row{} with a gap this poll",
                    if gaps == 1 { "" } else { "s" }
                ));
            }
            if unread > 0 {
                clauses.push(format!(
                    "{unread} row{} holding an unread cgroup's usage",
                    if unread == 1 { "" } else { "s" }
                ));
            }
            clauses.extend(table.depth.iter().cloned());
            warn = table.off.as_ref().map(|(_, line)| line.clone());
        }
        Some(_) => {}
        None => clauses.push("reading cgroups…".to_string()),
    }
    if !view.err.is_empty() {
        lines(&mut rows, &format!("! {}", view.err), &p.bad, width);
    }
    push_group("SERVICES", &clauses, width, p, &mut rows);
    if let Some(line) = warn {
        lines(&mut rows, &line, &p.down, width);
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
    let (shown, total, empty) = count;
    let filters = services::filters(view.needle, view.all_users, empty);
    if total > 0 && shown > 0 {
        if let Some(line) = tc::filter_row(shown, total, &filters) {
            lines(&mut rows, &line, &p.txt, width);
        }
    }
    Head {
        rows,
        plots: Vec::new(),
    }
}

/// The rows, or the named state that stands in their place.
pub fn table(view: &View, width: usize, p: &Palette) -> Table {
    let mut out = Table::default();
    let Some(sample) = view.sample else {
        return out;
    };
    let rows = &mut out.rows;
    let table = match &sample.services {
        None => return out,
        Some(Services::V1) => {
            lines(
                rows,
                "by service needs cgroup v2: this host mounts cgroup v1 or a hybrid of both",
                &p.down,
                width,
            );
            return out;
        }
        Some(Services::NotEnabled(line)) => {
            lines(rows, line, &p.down, width);
            return out;
        }
        Some(Services::Unreadable(why)) => {
            lines(
                rows,
                &format!("cgroup tree unreadable: {why}"),
                &p.bad,
                width,
            );
            return out;
        }
        Some(Services::Table(table)) => table,
    };
    let (shown, empty) = services::shown(
        &table.rows,
        view.needle,
        view.all_users,
        sample.me,
        view.svc_sort,
    );
    if shown.is_empty() {
        let filters = services::filters(view.needle, view.all_users, empty);
        let said = tc::filtered_to_nothing(table.rows.len(), &filters)
            .unwrap_or_else(|| "no cgroups with cpu or memory enabled".to_string());
        lines(rows, &said, &p.dim, width);
        return out;
    }
    let want = shown
        .iter()
        .map(|s| tc::display_width(&s.name()))
        .max()
        .unwrap_or(7)
        .max(7);
    let cols = columns(width, want);
    rows.push(header(&cols, view.svc_sort, width, p));
    out.header = 1;
    for (index, svc) in shown.iter().enumerate() {
        let here = view.svc_cursor == Some(svc.path.as_str());
        let from = rows.len();
        rows.extend(row(svc, &cols, sample.cores, width, here, p));
        out.placed.extend((from..rows.len()).map(|r| (r, index)));
        out.paths.push(svc.path.clone());
    }
    // What a `—` in the table means, for the rows that show one.
    let mut notes = Vec::new();
    if shown
        .iter()
        .any(|s| s.path.is_empty() && s.memory.is_none())
        && cols.memory
    {
        notes.push("the root has no memory.current of its own".to_string());
    }
    if shown.iter().any(|s| s.gap) {
        notes.push(
            "— a gap: read a moment after this cgroup, the ones under it moved more than it did, so its own share is not a figure this poll"
                .to_string(),
        );
    }
    if shown.iter().any(|s| s.unread) {
        notes.push(
            "— not known: a cgroup under it could not be read this poll, so its usage could not be taken out of this one's"
                .to_string(),
        );
    }
    if table.window.is_some() && shown.iter().any(|s| !s.gap && !s.unread && s.cpu.is_none()) {
        notes.push(
            "— in %CPU: no cpu controller on that cgroup; its CPU is counted in the nearest one above it that has one"
                .to_string(),
        );
    }
    if !notes.is_empty() {
        rows.push(String::new());
        for note in notes {
            lines(rows, &note, &p.dim, width);
        }
    }
    out
}

/// A rate from a delta of microseconds over the window, as % of all cores.
fn share(usec: u64, window: f64, cores: usize) -> f64 {
    usec as f64 / (window * 1e6 * cores.max(1) as f64) * 100.0
}

/// One service: its path, controllers, CPU and memory, then its processes
/// as the process table draws them.
pub fn detail(view: &View, width: usize, p: &Palette, busy: Option<usize>) -> Drawn {
    let mut rows = vec![tc::title_busy("procs · service", width, &p.accent, busy)];
    let head = rows.len();
    let Some(open) = view.svc_opened else {
        return Drawn {
            rows,
            placed: Vec::new(),
            head,
            pin: head,
            keys: Vec::new(),
            paths: Vec::new(),
            plots: Vec::new(),
        };
    };
    let svc = &open.last;
    let txt = p.txt.as_str();
    if open.gone {
        lines(
            &mut rows,
            "removed: this cgroup was not in the last walk; this is how it looked when it was last seen",
            &p.bad,
            width,
        );
    }
    if !view.err.is_empty() {
        lines(&mut rows, &format!("! {}", view.err), &p.bad, width);
    }
    let sample = view.sample;
    let cores = sample.map(|s| s.cores).unwrap_or(1);
    let window = match sample.and_then(|s| s.services.as_ref()) {
        Some(Services::Table(t)) => t.window,
        _ => None,
    };
    field(
        &mut rows,
        "cgroup",
        &format!("/{}", tc::source_text(&svc.path)),
        txt,
        width,
        p,
    );
    let unit = tc::procfs::unit_name(&tc::source_text(&svc.path));
    if !unit.is_empty() {
        field(&mut rows, "unit", &unit, txt, width, p);
    }
    field(&mut rows, "enabled", &svc.enabled.join(" "), txt, width, p);
    let label = cpu_label(Clock::Measured { window }, cores, view.every).join(" · ");
    let cpu = match (svc.cpu, svc.gap) {
        (Some(c), _) => format!("{c:.1}% own · {label}"),
        (None, _) if svc.unread && svc.enabled.iter().any(|c| c == "cpu") => {
            "— not known this poll: a cgroup under it could not be read, so its usage could not be taken out of this one's"
                .to_string()
        }
        (None, true) => {
            "— a gap this poll: read a moment after this cgroup, the ones under it moved more than it did"
                .to_string()
        }
        (None, false) if !svc.enabled.iter().any(|c| c == "cpu") => {
            "— no cpu controller on this cgroup; its CPU is counted in the nearest one above it that has one"
                .to_string()
        }
        (None, false) => format!("— · {label}"),
    };
    field(&mut rows, "%CPU", &cpu, txt, width, p);
    if let (Some(d), Some(w)) = (svc.stat_delta, window) {
        let text = format!(
            "user {:.1}% · system {:.1}% over the last {}, this cgroup and everything under it",
            share(d.user_usec, w, cores),
            share(d.system_usec, w, cores),
            secs(w)
        );
        field(&mut rows, "cpu.stat", &text, txt, width, p);
        let throttled = format!(
            "{} time{} for {} over the last {}",
            d.nr_throttled,
            if d.nr_throttled == 1 { "" } else { "s" },
            secs(d.throttled_usec as f64 / 1e6),
            secs(w)
        );
        field(&mut rows, "throttled", &throttled, txt, width, p);
    }
    let mut memory = Vec::new();
    if let Some(own) = svc.memory {
        memory.push(format!("{} own", human(own)));
    } else if svc.unread && svc.enabled.iter().any(|c| c == "memory") {
        memory.push("own not known: a cgroup under it could not be read".to_string());
    }
    if let Some(total) = svc.memory_total {
        memory.push(format!(
            "memory.current {} with everything under it",
            human(total)
        ));
    }
    if svc.enabled.iter().any(|c| c == "memory") {
        memory.push(match view.svc_max {
            None => "memory.max reading…".to_string(),
            Some(Ok(None)) => "no memory.max limit".to_string(),
            Some(Ok(Some(max))) => format!("memory.max {}", human(*max)),
            Some(Err(why)) => format!("memory.max unreadable: {why}"),
        });
    } else if svc.path.is_empty() {
        memory.push("the root has no memory.current of its own".to_string());
    } else {
        memory.push("no memory controller on this cgroup".to_string());
    }
    field(&mut rows, "memory", &memory.join(" · "), txt, width, p);
    let n = svc.pids.len();
    field(
        &mut rows,
        "processes",
        &format!(
            "{n} from cgroup.procs, its own and those of cgroups under it without the controller"
        ),
        txt,
        width,
        p,
    );
    rows.push(String::new());
    processes(&mut rows, svc, sample, width, p);
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

/// The service's processes through the process table's own columns and
/// row, sorted by %CPU. A pid the last `/proc` pass did not list is named
/// by its pid alone.
fn processes(
    rows: &mut Vec<String>,
    svc: &Service,
    sample: Option<&Sample>,
    width: usize,
    p: &Palette,
) {
    if svc.pids.is_empty() {
        lines(rows, "no processes in it now", &p.dim, width);
        return;
    }
    let Some(sample) = sample else {
        return;
    };
    let mut found: Vec<&Proc> = Vec::new();
    let mut missing: Vec<i32> = Vec::new();
    for pid in &svc.pids {
        match sample.procs.iter().find(|p| p.pid == *pid) {
            Some(proc) => found.push(proc),
            None => missing.push(*pid),
        }
    }
    super::model::sort(&mut found, ProcSort::Cpu);
    let digits = svc
        .pids
        .iter()
        .map(|pid| pid.to_string().len())
        .max()
        .unwrap_or(3)
        .max(3);
    let cmd_want = found
        .iter()
        .map(|p| tc::display_width(&p.comm))
        .max()
        .unwrap_or(7)
        .max(7);
    let user_want = found
        .iter()
        .map(|p| tc::display_width(&p.user))
        .max()
        .unwrap_or(4);
    let cols = view::columns(width, digits + 1, cmd_want, user_want, 0);
    rows.push(view::header(&cols, ProcSort::Cpu, width, p));
    for proc in found {
        rows.extend(view::row(proc, &cols, sample.cores, width, false, p));
    }
    // Not in the pass: hidden from this user, or started or gone between
    // the pass and the walk.
    let why = if sample.hidepid {
        "(not readable)"
    } else {
        "(not in this poll's process list)"
    };
    for pid in missing {
        let text = format!(
            " {} {why}",
            right(&pid.to_string(), cols.pid.saturating_sub(1))
        );
        for line in tc::wrap_words(&text, width.max(1)) {
            rows.push(tc::seg(&[(p.dim.as_str(), line)], width));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::fixtures::{proc, sample};
    use super::super::services::fixtures::{host, table as svc_table};
    use super::super::services::{Memo, step};
    use super::super::view::OpenedService;
    use super::*;
    use opscope_core::cgroup::State;
    use std::time::Duration;

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

    fn with(services: Services) -> Sample {
        let mut s = sample(vec![
            proc(100, "example-db", "service-b", Some(3.0), 900 << 20),
            proc(101, "example-db", "service-b", Some(1.0), 400 << 20),
        ]);
        s.services = Some(services);
        s
    }

    fn view<'a>(sample: &'a Sample) -> View<'a> {
        View {
            sample: Some(sample),
            err: "",
            age: Some(Duration::from_secs(1)),
            every: 2.0,
            sort: ProcSort::Cpu,
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

    fn text(head: &Head, table: &Table) -> String {
        head.rows
            .iter()
            .chain(table.rows.iter())
            .map(|r| plain(r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn flat(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn the_mock_renders_its_rows_and_the_depth_sentence() {
        let s = with(Services::Table(svc_table()));
        let v = view(&s);
        let p = view::palette();
        let (h, t) = (head(&v, 100, &p, None), table(&v, 100, &p));
        let out = text(&h, &t);
        let flat = flat(&out);
        assert!(
            flat.contains("── SERVICES ── sampled 1s ago · every 2s"),
            "{out}"
        );
        assert!(flat.contains("cgroup v2 · complete"), "{out}");
        assert!(
            flat.contains("%CPU of all 8 cores over the last 2s"),
            "{out}"
        );
        assert!(
            flat.contains(
                "cpu accounting down to: services, scopes; user sessions roll up to user.slice"
            ),
            "{out}"
        );
        assert!(flat.contains("user.slice 88.4 9.5G 203"), "{out}");
        assert!(
            flat.contains("system.slice/example-db.service 31.2 3.8G 12"),
            "{out}"
        );
        assert!(
            flat.contains("system.slice/example-log.service 0.4 120M 1"),
            "{out}"
        );
        // The parent's own row, without its enabled children.
        assert!(flat.contains("system.slice 0.0 4G 0"), "{out}");
        assert!(flat.contains("6 of 7 shown · 1 empty hidden"), "{out}");
        assert!(flat.contains("the root has no memory.current"), "{out}");
        assert_eq!(t.paths[0], "user.slice");
    }

    #[test]
    fn named_states_stand_in_for_the_table() {
        let p = view::palette();
        let mut memo = Memo::default();
        // A tree on disk with cpu and memory at the root and enabled on
        // nothing below it, walked as the widget walks /sys/fs/cgroup.
        let dir = std::env::temp_dir().join(format!("procs-cgroup-off-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("system.slice")).unwrap();
        std::fs::write(dir.join("cgroup.controllers"), "cpu memory pids\n").unwrap();
        std::fs::write(dir.join("cgroup.subtree_control"), "pids\n").unwrap();
        let state = opscope_core::cgroup::walk(&dir, &["cpu", "memory"], true)
            .expect_err("cpu is enabled on nothing below the root");
        std::fs::remove_dir_all(&dir).ok();
        let s = with(step(&mut memo, state, None, 1.0, 8));
        let v = view(&s);
        let out = text(&head(&v, 80, &p, None), &table(&v, 80, &p));
        assert!(
            flat(&out).contains(
                "cpu accounting is off everywhere · systemctl set-property <unit> CPUAccounting=yes"
            ),
            "{out}"
        );
        let s = with(step(&mut memo, State::V1, None, 1.0, 8));
        let v = view(&s);
        let out = text(&head(&v, 80, &p, None), &table(&v, 80, &p));
        assert!(flat(&out).contains("by service needs cgroup v2"), "{out}");
        let s = with(step(&mut memo, State::Container(host(0)), None, 1.0, 8));
        let v = view(&s);
        let out = text(&head(&v, 80, &p, None), &table(&v, 80, &p));
        assert!(
            flat(&out).contains("rows are this container's cgroups"),
            "{out}"
        );
        assert!(
            flat(&out).contains("%CPU of all 8 cores from the next sample"),
            "{out}"
        );
    }

    #[test]
    fn the_services_heading_pins_with_the_column_header() {
        let s = with(Services::Table(svc_table()));
        let v = view(&s);
        let p = view::palette();
        for width in [28, 40, 60, 80, 100, 140] {
            let drawn = view::compose(head(&v, width, &p, None), &table(&v, width, &p));
            let rows: Vec<String> = drawn.rows.iter().map(|r| plain(r)).collect();
            let rule = rows
                .iter()
                .position(|r| r.starts_with(" ── SERVICES"))
                .unwrap_or_else(|| panic!("{width}: no heading:\n{}", rows.join("\n")));
            assert!(rows[rule - 1].is_empty(), "a blank line before the rule");
            assert!(rule < drawn.pin, "the heading pins");
            assert!(
                rows[drawn.pin - 1].trim_start().starts_with("SERVICE"),
                "{width}: the column header is the last pinned row:\n{}",
                rows.join("\n")
            );
            // The first row of the window is the first service.
            assert_eq!(drawn.placed.iter().map(|(r, _)| *r).min(), Some(drawn.pin));
        }
    }

    #[test]
    fn every_row_fits_and_columns_leave_procs_then_memory() {
        let s = with(Services::Table(svc_table()));
        let p = view::palette();
        for width in [28, 40, 60, 80, 100, 140] {
            let mut v = view(&s);
            v.svc_cursor = Some("system.slice/example-db.service");
            let (h, t) = (head(&v, width, &p, None), table(&v, width, &p));
            for line in h.rows.iter().chain(t.rows.iter()) {
                let n = tc::display_width(&plain(line));
                assert!(n <= width, "{n} > {width}: {:?}", plain(line));
            }
            // Nothing is cut: the longest path is all there, read down its
            // column, whatever stands between its pieces.
            let column: String = t
                .rows
                .iter()
                .map(|r| plain(r))
                .map(|r| {
                    r.chars()
                        .skip(1)
                        .take(columns(width, 32).name)
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .concat();
            let column: String = column.split_whitespace().collect();
            assert!(
                column.contains("system.slice/example-log.service"),
                "{width}: {column}"
            );
        }
        assert_eq!(
            columns(140, 32).names(),
            vec!["SERVICE", "%CPU", "MEMORY", "PROCS"]
        );
        assert_eq!(columns(30, 32).names(), vec!["SERVICE", "%CPU", "MEMORY"]);
        assert_eq!(columns(22, 32).names(), vec!["SERVICE", "%CPU"]);
    }

    #[test]
    fn filter_and_yours_apply_and_an_empty_result_says_why() {
        let s = with(Services::Table(svc_table()));
        let p = view::palette();
        let mut v = view(&s);
        v.needle = "example";
        let out = text(&head(&v, 100, &p, None), &table(&v, 100, &p));
        assert!(
            flat(&out).contains("2 of 7 shown · /example · 1 empty hidden"),
            "{out}"
        );
        v.needle = "";
        v.all_users = false;
        let t = table(&v, 100, &p);
        assert!(t.paths.is_empty());
        let out = text(&head(&v, 100, &p, None), &t);
        assert!(
            flat(&out).contains("nothing matches · all 7 hidden by the filter · yours"),
            "{out}"
        );
    }

    #[test]
    fn the_detail_lists_its_processes_through_the_process_row() {
        let t = svc_table();
        let db = t
            .rows
            .iter()
            .find(|r| r.path == "system.slice/example-db.service")
            .unwrap()
            .clone();
        let s = with(Services::Table(t));
        let open = OpenedService {
            last: db,
            gone: false,
        };
        let max = Ok(Some(4u64 << 30));
        let mut v = view(&s);
        v.svc_opened = Some(&open);
        v.svc_max = Some(&max);
        let p = view::palette();
        let drawn = detail(&v, 100, &p, None);
        let out = drawn
            .rows
            .iter()
            .map(|r| plain(r))
            .collect::<Vec<_>>()
            .join("\n");
        let f = flat(&out);
        assert!(
            f.contains("cgroup /system.slice/example-db.service"),
            "{out}"
        );
        assert!(f.contains("unit example-db"), "{out}");
        assert!(f.contains("31.2% own"), "{out}");
        assert!(f.contains("memory.max 4G"), "{out}");
        assert!(f.contains("PID COMMAND USER %CPU MEM RSS"), "{out}");
        assert!(f.contains("100 example-db service-b 3.0"), "{out}");
        // The other ten pids of the fixture are not in the process sample.
        assert!(f.contains("102 (not in this poll's process list)"), "{out}");
        for line in &drawn.rows {
            assert!(tc::display_width(&plain(line)) <= 100);
        }
    }
}
