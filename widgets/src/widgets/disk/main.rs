// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Which filesystems can still take a write, and which are running out of inodes.
//!
//! Linux reads `/proc/self/mountinfo` and `statvfs`. macOS reads `getmntinfo`.
//! The parsers and the hide/sort rules compile on every target; only the
//! call that asks the kernel is behind `cfg(target_os)`.
//!
//!     disk [-n SECONDS]
//!
//! Keys: up/down select, enter opens, s cycles the sort, p pauses, comma
//! opens settings, q quits.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use opscope_core as tc;

mod parse;
use parse::{inode_percent, is_remote, percent_used, Hidden, Mount, Space};

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod host;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod host;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod host {
    use super::parse::{Mount, Space};

    pub fn read_mounts() -> Result<Vec<Mount>, String> {
        Err(opscope_core::unsupported())
    }

    pub fn stat_mount(_path: &str) -> Result<Space, String> {
        Err(opscope_core::unsupported())
    }
}

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "disk",
    section: "disk",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};

/// Columns that stay however narrow the pane, and the ones that leave.
///
/// Drop order as the pane shrinks is inodes, then type, then size. The bar
/// shrinks from 11 cells to 4 before a column is dropped, so a number is
/// kept in preference to a longer bar. Whatever is left after that goes to
/// the mount column.
const MOUNT_MIN: usize = 5;
const USED_W: usize = 5;
const USABLE_W: usize = 6;
const PCT_W: usize = 4;
const SIZE_W: usize = 5;
const TYPE_W: usize = 12;
const INODES_W: usize = 6;
const BAR_MAX: usize = 11;
const BAR_MIN: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Columns {
    mount: usize,
    typ: bool,
    size: bool,
    inodes: bool,
    bar: usize,
}

fn columns_for(width: usize) -> Columns {
    let sets = [
        (true, true, true),
        (true, true, false),
        (true, false, false),
        (false, false, false),
    ];
    for &(size, typ, inodes) in &sets {
        for bar in (BAR_MIN..=BAR_MAX).rev() {
            let need = column_need(size, typ, inodes, bar);
            if need <= width {
                return Columns {
                    mount: MOUNT_MIN + width - need,
                    typ,
                    size,
                    inodes,
                    bar,
                };
            }
        }
    }
    Columns {
        mount: MOUNT_MIN,
        typ: false,
        size: false,
        inodes: false,
        bar: BAR_MIN,
    }
}

fn column_need(size: bool, typ: bool, inodes: bool, bar: usize) -> usize {
    let mut width = MOUNT_MIN + USED_W + USABLE_W + bar + PCT_W;
    let mut gaps = 4;
    if size {
        width += SIZE_W;
        gaps += 1;
    }
    if typ {
        width += TYPE_W;
        gaps += 1;
    }
    if inodes {
        width += INODES_W;
        gaps += 1;
    }
    width + gaps
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Mount,
    Percent,
    Inodes,
}

impl Sort {
    fn next(self) -> Self {
        match self {
            Self::Mount => Self::Percent,
            Self::Percent => Self::Inodes,
            Self::Inodes => Self::Mount,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Mount => "mount",
            Self::Percent => "% used",
            Self::Inodes => "inodes",
        }
    }

    /// The footer names the sort the next press moves to.
    fn hint(self) -> &'static str {
        match self {
            Self::Mount => "[s] sort by % used",
            Self::Percent => "[s] sort by inodes",
            Self::Inodes => "[s] sort by mount",
        }
    }
}

fn pause_hint(paused: bool) -> &'static str {
    if paused {
        "[p] resume"
    } else {
        "[p] pause"
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    List,
    Detail,
}

#[derive(Clone, Debug)]
enum Figures {
    Ready(Space),
    NoAnswer,
    Failed(String),
}

#[derive(Clone, Debug)]
struct Row {
    mount: Mount,
    figures: Figures,
    order: usize,
}

#[derive(Clone, Debug)]
struct Snap {
    rows: Vec<Row>,
    all: Vec<Mount>,
    hidden: Hidden,
    total: usize,
}

struct Palette {
    txt: String,
    dim: String,
    dim_lit: String,
    lbl: String,
    accent: String,
}

fn palette() -> Palette {
    Palette {
        txt: tc::rgb(225, 235, 245),
        // 127,147,172 measures 3.81 on the selected-row tint. The lighter
        // twin is what the tint closure reaches for.
        dim: tc::rgb(127, 147, 172),
        dim_lit: tc::rgb(140, 170, 195),
        lbl: tc::rgb(130, 165, 200),
        accent: tc::rgb(150, 210, 255),
    }
}

struct View<'a> {
    snap: Option<&'a Snap>,
    err: &'a str,
    age: Option<Duration>,
    paused: bool,
    every: f64,
    sort: Sort,
    cursor: &'a str,
    screen: Screen,
    warn: f64,
    alert: f64,
}

struct Drawn {
    rows: Vec<String>,
    placed: Vec<(usize, usize)>,
    head: usize,
    selected: usize,
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    tc::notices::start("disk");
    if !tc::dependencies_available("disk", include_str!("dependencies.json"), Some(SETTINGS)) {
        return;
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        tc::cannot_start_because(
            "disk",
            &tc::unsupported(),
            &["Filesystem space is read from the kernel on Linux and macOS."],
            "",
        );
        return;
    }

    let cfg = tc::load_config("disk");
    let every = refresh_seconds(&cfg);
    let hide = tc::cfg_strings(&cfg, "hide_mounts", &[]);
    let warn = tc::cfg_f64(&cfg, "warn_percent", 80.0);
    let alert = tc::cfg_f64(&cfg, "alert_percent", 90.0);

    let state = Arc::new(Mutex::new(State::default()));
    let pause = Arc::new(AtomicBool::new(false));
    let refresh = tc::Refresh::default();
    {
        let state = Arc::clone(&state);
        let pause = Arc::clone(&pause);
        let refresh = refresh.clone();
        thread::spawn(move || poll(state, pause, refresh, hide, every));
    }

    let p = palette();
    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut sort = Sort::Mount;
    let mut screen = Screen::List;
    let mut cursor = String::new();
    let mut scroll = 0usize;
    let mut chase = true;
    let mut placed: Vec<(usize, usize)> = Vec::new();
    let mut head = 1usize;
    let mut shown_scroll = 0usize;
    let mut selected = 0usize;

    loop {
        let mut keys = keyboard.poll();
        // Set by `enter`, acted on after the batch's arrows have moved the
        // cursor, so an arrow and an enter read together open the new row.
        let mut open = false;
        if screen == Screen::List {
            if let Some(at) =
                tc::rows_clicked(&mut keys, Some(selected), head, shown_scroll, &placed, None)
            {
                selected = at;
                chase = true;
            }
        } else {
            let _ = tc::rows_clicked(&mut keys, None, 0, 0, &[], None);
        }
        for key in keys {
            match key.as_str() {
                "," => {
                    tc::run_settings(&mut keyboard, SETTINGS);
                    continue;
                }
                "!" => {
                    tc::notices::run(&mut keyboard);
                    continue;
                }
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "up" | "k" | "K" => {
                    if screen == Screen::List {
                        selected = selected.saturating_sub(1);
                        chase = true;
                    }
                }
                "down" | "j" | "J" => {
                    if screen == Screen::List {
                        selected = selected.saturating_add(1);
                        chase = true;
                    }
                }
                "enter" => {
                    if screen == Screen::List {
                        open = true;
                    }
                }
                "esc" => {
                    if screen == Screen::Detail {
                        screen = Screen::List;
                        scroll = 0;
                        chase = true;
                    }
                }
                "s" | "S" => {
                    sort = sort.next();
                    chase = false;
                }
                "p" | "P" => {
                    pause.fetch_xor(true, Ordering::AcqRel);
                }
                "ctrl-y" | "wheel-up" => {
                    scroll = scroll.saturating_sub(1);
                    chase = false;
                }
                "ctrl-e" | "wheel-down" => {
                    scroll = scroll.saturating_add(1);
                    chase = false;
                }
                "pgup" => {
                    scroll = scroll.saturating_sub(8);
                    chase = false;
                }
                "pgdn" => {
                    scroll = scroll.saturating_add(8);
                    chase = false;
                }
                "home" => {
                    scroll = 0;
                    chase = false;
                }
                "end" => {
                    scroll = usize::MAX / 4;
                    chase = false;
                }
                _ => {}
            }
        }

        let (snap, err, sampled) = {
            let guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (guard.snap.clone(), guard.err.clone(), guard.sampled)
        };
        let age = sampled.map(|at| at.elapsed());
        let paused = pause.load(Ordering::Acquire);
        if let Some(snap) = snap.as_ref() {
            let count = snap.rows.len();
            let missing = !snap.rows.iter().any(|row| row.mount.mount_point == cursor);
            if count > 0 && (screen == Screen::List && chase || missing) {
                if let Some(path) = path_at(snap, sort, selected.min(count - 1)) {
                    cursor = path;
                }
            }
        }
        if open {
            screen = Screen::Detail;
            scroll = 0;
            chase = true;
        }
        let view = View {
            snap: snap.as_ref(),
            err: &err,
            age,
            paused,
            every,
            sort,
            cursor: &cursor,
            screen,
            warn,
            alert,
        };
        let (w, h) = tc::size();
        let drawn = draw_body(&view, w, &p, refresh.tick(tc::animation_tick()));
        selected = drawn.selected;
        let cursor_gone = snap
            .as_ref()
            .is_some_and(|snap| !snap.rows.iter().any(|row| row.mount.mount_point == cursor));
        if cursor.is_empty() || cursor_gone {
            if let Some(path) = snap.as_ref().and_then(|snap| path_at(snap, sort, selected)) {
                cursor = path;
            }
        }
        let (packed, indent) = footer(w, screen, sort, paused, &p);
        let banner = tc::notices::banner_rows(w, h);
        let foot: Vec<String> = banner
            .iter()
            .cloned()
            .chain(packed.lines.iter().cloned())
            .collect();
        let room = h.saturating_sub(foot.len());
        let head_rows = drawn.head.min(drawn.rows.len());
        let rest = drawn.rows.len().saturating_sub(head_rows);
        let room_below = room.saturating_sub(head_rows).max(1);
        if chase {
            if let Some(at) = drawn
                .placed
                .iter()
                .find(|(_, item)| *item == selected)
                .map(|(row, _)| *row)
            {
                let in_rest = at.saturating_sub(head_rows);
                scroll = tc::follow(scroll, in_rest, room_below);
            }
        }
        let max_scroll = rest.saturating_sub(room_below.min(rest));
        scroll = scroll.min(max_scroll);
        let end = (scroll + room_below).min(rest);
        let mut rows: Vec<String> = drawn.rows[..head_rows].to_vec();
        rows.extend_from_slice(&drawn.rows[head_rows + scroll..head_rows + end]);
        while rows.len() < room {
            rows.push(String::new());
        }
        let foot_top = rows.len();
        rows.extend(foot);
        tc::draw(&rows, w, h);
        keyboard.footer_at(&packed, foot_top, indent);
        placed = drawn.placed;
        head = head_rows;
        shown_scroll = scroll;
        // Frame-local: only the frame a key moved the selection follows it.
        // Left set, a refresh that reorders the rows would hand the cursor
        // to whichever mount now sits at the old index.
        chase = false;
        keyboard.wait(Duration::from_millis(200));
    }
}

fn path_at(snap: &Snap, sort: Sort, selected: usize) -> Option<String> {
    let ordered = ordered(snap, sort);
    ordered
        .get(selected)
        .map(|&index| snap.rows[index].mount.mount_point.clone())
}

struct State {
    snap: Option<Snap>,
    err: String,
    sampled: Option<Instant>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            snap: None,
            err: String::new(),
            sampled: None,
        }
    }
}

fn poll(
    state: Arc<Mutex<State>>,
    pause: Arc<AtomicBool>,
    refresh: tc::Refresh,
    hide: Vec<String>,
    every: f64,
) {
    tc::record_panics();
    let mut inflight: HashMap<String, Receiver<Result<Space, String>>> = HashMap::new();
    loop {
        if pause.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        let work = refresh.start();
        let round = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sample(&hide, &mut inflight)
        }));
        match round {
            Ok(Ok(snap)) => {
                if let Ok(mut guard) = state.lock() {
                    guard.snap = Some(snap);
                    guard.err.clear();
                    guard.sampled = Some(Instant::now());
                }
            }
            Ok(Err(err)) => {
                if let Ok(mut guard) = state.lock() {
                    guard.err = err;
                }
            }
            Err(_) => {
                if let Ok(mut guard) = state.lock() {
                    guard.err = tc::take_panic_reason()
                        .map(|why| format!("stopped: {why}"))
                        .unwrap_or_else(|| "stopped: poller panicked".into());
                }
                drop(work);
                return;
            }
        }
        drop(work);
        let until = Instant::now() + Duration::from_secs_f64(every);
        while Instant::now() < until {
            if pause.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

/// One pass over the mount table. `statvfs` for each shown mount runs on its
/// own thread and is given one shared two-second budget: a hung mount costs
/// two seconds once, and is not asked again until that thread returns.
///
/// A thread still inside the kernel is leaked on purpose. Joining it would
/// freeze this poller on the mount that already failed to answer, and the
/// call cannot be cancelled.
fn sample(
    hide: &[String],
    inflight: &mut HashMap<String, Receiver<Result<Space, String>>>,
) -> Result<Snap, String> {
    let all = host::read_mounts().map_err(|err| format!("mounts unavailable: {err}"))?;
    let (visible, hidden) = parse::visible_mounts(&all, hide);
    let live: Vec<String> = visible
        .iter()
        .map(|mount| mount.mount_point.clone())
        .collect();
    inflight.retain(|path, _| live.iter().any(|live| live == path));

    let mut fresh = Vec::new();
    let mut figures: HashMap<String, Figures> = HashMap::new();
    for path in &live {
        if !inflight.contains_key(path) {
            let (tx, rx) = mpsc::channel();
            let owned = path.clone();
            thread::spawn(move || {
                let _ = tx.send(host::stat_mount(&owned));
            });
            inflight.insert(path.clone(), rx);
            fresh.push(path.clone());
            continue;
        }
        let step = match inflight.get(path).map(|rx| rx.try_recv()) {
            Some(Ok(result)) => Step::Ready(result),
            Some(Err(TryRecvError::Empty)) => Step::Waiting,
            _ => Step::Gone,
        };
        match step {
            Step::Ready(result) => {
                inflight.remove(path);
                figures.insert(path.clone(), figures_of(result));
            }
            Step::Waiting => {
                figures.insert(path.clone(), Figures::NoAnswer);
            }
            Step::Gone => {
                inflight.remove(path);
                figures.insert(
                    path.clone(),
                    Figures::Failed("statvfs: the read ended without an answer".into()),
                );
            }
        }
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    for path in fresh {
        let step = {
            let wait = deadline.saturating_duration_since(Instant::now());
            match inflight.get(&path).map(|rx| rx.recv_timeout(wait)) {
                Some(Ok(result)) => Step::Ready(result),
                Some(Err(RecvTimeoutError::Timeout)) => Step::Waiting,
                _ => Step::Gone,
            }
        };
        match step {
            Step::Ready(result) => {
                inflight.remove(&path);
                figures.insert(path, figures_of(result));
            }
            Step::Waiting => {
                figures.insert(path, Figures::NoAnswer);
            }
            Step::Gone => {
                inflight.remove(&path);
                figures.insert(
                    path,
                    Figures::Failed("statvfs: the read ended without an answer".into()),
                );
            }
        }
    }

    let rows = visible
        .into_iter()
        .enumerate()
        .map(|(order, mount)| {
            let figures = figures
                .remove(&mount.mount_point)
                .unwrap_or(Figures::NoAnswer);
            Row {
                mount,
                figures,
                order,
            }
        })
        .collect();
    Ok(Snap {
        rows,
        total: all.len(),
        hidden,
        all,
    })
}

enum Step {
    Ready(Result<Space, String>),
    Waiting,
    Gone,
}

fn figures_of(result: Result<Space, String>) -> Figures {
    match result {
        Ok(space) => Figures::Ready(space),
        Err(err) => Figures::Failed(format!("statvfs: {err}")),
    }
}

fn refresh_seconds(cfg: &serde_json::Value) -> f64 {
    let mut seconds = tc::cfg_f64(cfg, "refresh", 2.0);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "-n" {
            if let Some(value) = args
                .get(index + 1)
                .and_then(|text| text.parse::<f64>().ok())
            {
                seconds = value;
            }
            index += 2;
            continue;
        }
        index += 1;
    }
    // NaN, infinity and anything past a day would panic in
    // `Duration::from_secs_f64` outside the poller's guard.
    let seconds = tc::poll_secs(seconds, 2.0).min(86_400.0);
    if seconds < 0.5 {
        0.5
    } else {
        seconds
    }
}

fn draw_body(view: &View, width: usize, p: &Palette, busy: Option<usize>) -> Drawn {
    let mut rows = vec![tc::title_busy("disk", width, &p.accent, busy)];
    // Placements are stored only after the section paints, so a panic
    // leaves them empty and the fault rows `guard_rows` substitutes cannot
    // be clicked as if they were mounts.
    let placed_slot = Mutex::new(Vec::new());
    let section = tc::guard_rows("filesystems", width, || {
        let mut body = Vec::new();
        let mut at = Vec::new();
        paint(view, width, p, &mut body, &mut at);
        if let Ok(mut slot) = placed_slot.lock() {
            *slot = at;
        }
        body
    });
    let head = rows.len();
    let failed = section.iter().any(|line| line.contains("could not draw"));
    let mut placed = placed_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if failed {
        placed.clear();
    }
    for (row, _) in &mut placed {
        *row += head;
    }
    rows.extend(section);
    let selected = view
        .snap
        .and_then(|snap| {
            let ordered = ordered(snap, view.sort);
            ordered
                .iter()
                .position(|&index| snap.rows[index].mount.mount_point == view.cursor)
        })
        .unwrap_or(0);
    Drawn {
        rows,
        placed,
        head,
        selected,
    }
}

fn paint(
    view: &View,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    placed: &mut Vec<(usize, usize)>,
) {
    for line in wrap(&status_line(view), width) {
        rows.push(tc::seg(&[(p.dim.as_str(), format!(" {line}"))], width));
    }
    if !view.err.is_empty() {
        for line in wrap(view.err, width.saturating_sub(1)) {
            rows.push(tc::seg(&[(p.accent.as_str(), format!(" {line}"))], width));
        }
    }
    let Some(snap) = view.snap else {
        rows.push(tc::seg(&[(p.dim.as_str(), " sampling…".into())], width));
        return;
    };
    let labels = filter_labels(snap.hidden);
    if let Some(line) = tc::filter_row(snap.rows.len(), snap.total, &labels) {
        for line in wrap(&line, width.saturating_sub(1)) {
            rows.push(tc::seg(&[(p.dim.as_str(), format!(" {line}"))], width));
        }
    }
    if snap.rows.is_empty() {
        if let Some(line) = tc::filtered_to_nothing(snap.total, &labels) {
            for line in wrap(&line, width.saturating_sub(1)) {
                rows.push(tc::seg(&[(p.txt.as_str(), format!(" {line}"))], width));
            }
        } else if view.err.is_empty() {
            rows.push(tc::seg(&[(p.dim.as_str(), " no mounts".into())], width));
        }
    }
    let heading = format!(
        "FILESYSTEMS  usable = f_bavail (root reserve excluded) · sorted: {}",
        view.sort.label()
    );
    for line in wrap(&heading, width.saturating_sub(1)) {
        rows.push(tc::seg(&[(p.lbl.as_str(), format!(" {line}"))], width));
    }

    if view.screen == Screen::Detail {
        paint_detail(view, snap, width, p, rows);
        return;
    }
    if snap.rows.is_empty() {
        return;
    }
    let cols = columns_for(width);
    rows.push(header_row(&cols, width, p));
    let ordered = ordered(snap, view.sort);
    for (item, index) in ordered.iter().copied().enumerate() {
        let row = &snap.rows[index];
        let here = row.mount.mount_point == view.cursor;
        let from = rows.len();
        rows.extend(data_rows(row, &cols, width, here, view.warn, view.alert, p));
        placed.extend((from..rows.len()).map(|at| (at, item)));
    }
}

fn paint_detail(view: &View, snap: &Snap, width: usize, p: &Palette, rows: &mut Vec<String>) {
    let Some(row) = ordered(snap, view.sort)
        .into_iter()
        .find(|&index| snap.rows[index].mount.mount_point == view.cursor)
        .map(|index| &snap.rows[index])
    else {
        rows.push(tc::seg(
            &[(p.dim.as_str(), " nothing selected".into())],
            width,
        ));
        return;
    };
    let mount = &row.mount;
    let mut lines = vec![
        format!("mount {}", mount.mount_point),
        format!("source {}", mount.source),
        format!("type {}", mount.fstype),
        format!("options {}", mount.options),
    ];
    match &row.figures {
        Figures::Ready(space) => {
            lines.push(format!(
                "size {}  used {}  usable {}  reserved {}",
                human(space.size),
                human(space.used),
                human(space.usable),
                human(space.reserved)
            ));
            let used = space.files.saturating_sub(space.ffree);
            let inodes = match inode_percent(space.files, space.ffree) {
                Some(_) => format!("{used} / {} inodes", space.files),
                None => "inodes n/a".into(),
            };
            lines.push(inodes);
        }
        Figures::NoAnswer => lines.push("no answer in 2s".into()),
        Figures::Failed(err) => lines.push(err.clone()),
    }
    let siblings: Vec<&str> = snap
        .all
        .iter()
        .filter(|other| other.dev == mount.dev && other.mount_point != mount.mount_point)
        .map(|other| other.mount_point.as_str())
        .collect();
    if siblings.is_empty() {
        lines.push("no other mount shares this device".into());
    } else {
        lines.push(format!("also on this device {}", siblings.join(", ")));
    }
    for line in lines {
        for wrapped in wrap(&line, width.saturating_sub(1)) {
            rows.push(tc::seg(&[(p.txt.as_str(), format!(" {wrapped}"))], width));
        }
    }
}

fn header_row(cols: &Columns, width: usize, p: &Palette) -> String {
    let mut cells = vec![tc::pad("MOUNT", cols.mount)];
    if cols.typ {
        cells.push(tc::pad("TYPE", TYPE_W));
    }
    if cols.size {
        cells.push(tc::pad("SIZE", SIZE_W));
    }
    cells.push(format!("{:>width$}", "USED", width = USED_W));
    cells.push(format!("{:>width$}", "USABLE", width = USABLE_W));
    cells.push(tc::pad("", cols.bar));
    cells.push(format!("{:>width$}", "%", width = PCT_W));
    if cols.inodes {
        cells.push(format!("{:>width$}", "INODES", width = INODES_W));
    }
    tc::seg(&[(p.dim.as_str(), cells.join(" "))], width)
}

fn data_rows(
    row: &Row,
    cols: &Columns,
    width: usize,
    here: bool,
    warn: f64,
    alert: f64,
    p: &Palette,
) -> Vec<String> {
    let tint = if here {
        tc::bg(38, 56, 76)
    } else {
        String::new()
    };
    let c = |colour: &str| {
        // Any colour that would not clear AA on this tint is swapped for
        // its lighter twin. `dim` is 3.81 against bg(38, 56, 76).
        let colour = if tint.is_empty() {
            colour
        } else if colour == p.dim {
            p.dim_lit.as_str()
        } else {
            colour
        };
        format!("{}{}", tint, colour)
    };
    let room = cols.mount.saturating_sub(2).max(1);
    let chunks = chunks(&row.mount.mount_point, room);
    let mut out = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let marker = if index == 0 {
            if here {
                "▸ "
            } else {
                "  "
            }
        } else {
            "  "
        };
        let mount = format!("{marker}{}", tc::pad(chunk, room));
        let mount = tc::pad(&mount, cols.mount);
        if index > 0 {
            let line = tc::seg(&[(c(p.txt.as_str()).as_str(), mount)], width);
            out.push(line);
            continue;
        }
        let mut cells = vec![(
            c(if here {
                p.accent.as_str()
            } else {
                p.txt.as_str()
            }),
            mount,
        )];
        match &row.figures {
            Figures::Ready(space) => {
                let frac = percent_used(space.used, space.usable);
                let points = frac * 100.0;
                let hot = tc::heat_on(heat_frac(points, warn, alert), here);
                if cols.typ {
                    let ink = if is_remote(&row.mount.fstype) {
                        p.dim.as_str()
                    } else {
                        p.txt.as_str()
                    };
                    cells.push((
                        c(ink),
                        tc::pad(&clip_chars(&row.mount.fstype, TYPE_W), TYPE_W),
                    ));
                }
                if cols.size {
                    cells.push((
                        c(p.txt.as_str()),
                        format!("{:>width$}", human(space.size), width = SIZE_W),
                    ));
                }
                cells.push((
                    c(p.txt.as_str()),
                    format!("{:>width$}", human(space.used), width = USED_W),
                ));
                cells.push((
                    c(p.txt.as_str()),
                    format!("{:>width$}", human(space.usable), width = USABLE_W),
                ));
                cells.push((c(&hot), tc::meter(frac, cols.bar)));
                cells.push((
                    c(&hot),
                    format!("{:>width$}", pct_text(frac), width = PCT_W),
                ));
                if cols.inodes {
                    let text = match inode_percent(space.files, space.ffree) {
                        Some(share) => pct_text(share),
                        None => "n/a".into(),
                    };
                    cells.push((
                        c(p.txt.as_str()),
                        format!("{:>width$}", text, width = INODES_W),
                    ));
                }
            }
            Figures::NoAnswer => {
                cells.push((c(p.accent.as_str()), "no answer in 2s".into()));
            }
            Figures::Failed(err) => {
                cells.push((c(p.accent.as_str()), err.clone()));
            }
        }
        let mut parts: Vec<(&str, String)> = cells
            .iter()
            .enumerate()
            .flat_map(|(i, (colour, text))| {
                let mut piece = Vec::new();
                if i > 0 {
                    piece.push((colour.as_str(), " ".into()));
                }
                piece.push((colour.as_str(), text.clone()));
                piece
            })
            .collect();
        if here {
            let used: usize = parts.iter().map(|(_, text)| tc::display_width(text)).sum();
            if used < width {
                parts.push((tint.as_str(), " ".repeat(width - used)));
            }
        }
        out.push(tc::seg(&parts, width));
    }
    out
}

fn ordered(snap: &Snap, sort: Sort) -> Vec<usize> {
    let mut index: Vec<usize> = (0..snap.rows.len()).collect();
    index.sort_by(|&left, &right| match sort {
        Sort::Mount => snap.rows[left].order.cmp(&snap.rows[right].order),
        Sort::Percent => percent_key(&snap.rows[right])
            .total_cmp(&percent_key(&snap.rows[left]))
            .then(snap.rows[left].order.cmp(&snap.rows[right].order)),
        Sort::Inodes => inode_ord(&snap.rows[right])
            .cmp(&inode_ord(&snap.rows[left]))
            .then(snap.rows[left].order.cmp(&snap.rows[right].order)),
    });
    index
}

fn percent_key(row: &Row) -> f64 {
    match &row.figures {
        Figures::Ready(space) => percent_used(space.used, space.usable),
        _ => -1.0,
    }
}

fn inode_ord(row: &Row) -> (bool, u64) {
    match &row.figures {
        Figures::Ready(space) => match inode_percent(space.files, space.ffree) {
            Some(share) => (true, (share * 1_000_000.0) as u64),
            None => (false, 0),
        },
        _ => (false, 0),
    }
}

fn heat_frac(points: f64, warn: f64, alert: f64) -> f64 {
    if points <= warn {
        0.0
    } else if alert <= warn {
        1.0
    } else {
        ((points - warn) / (alert - warn)).clamp(0.0, 1.0)
    }
}

fn status_line(view: &View) -> String {
    let every = every_label(view.every);
    match (view.paused, view.age) {
        (true, Some(age)) => format!("paused · sampled {}s ago · {every}", age.as_secs()),
        (true, None) => format!("paused · {every}"),
        (false, None) => format!("sampling… · {every}"),
        (false, Some(age)) => format!("sampled {}s ago · {every}", age.as_secs()),
    }
}

fn every_label(seconds: f64) -> String {
    if (seconds - seconds.round()).abs() < 0.05 {
        format!("every {:.0}s", seconds)
    } else {
        format!("every {seconds:.1}s", seconds = seconds)
    }
}

fn filter_labels(hidden: Hidden) -> Vec<String> {
    let names = hidden.categories();
    if names.is_empty() {
        Vec::new()
    } else {
        vec![format!("{} hidden", names.join(", "))]
    }
}

fn human(n: u64) -> String {
    // Five characters is the column. P and E keep a full `u64` inside it:
    // stopping at T would draw `16777216T` and the row would be clipped.
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
            let frac = n % unit;
            if whole < 10 && frac > 0 {
                let tenths = (frac * 10) / unit;
                if tenths > 0 {
                    return format!("{whole}.{tenths}{name}");
                }
            }
            return format!("{whole}{name}");
        }
    }
    format!("{n}B")
}

fn pct_text(frac: f64) -> String {
    format!("{:.0}%", (frac * 100.0).round().clamp(0.0, 100.0))
}

fn chunks(text: &str, room: usize) -> Vec<String> {
    let room = room.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    // By display width, not character count: the rows are laid out in cells,
    // and a wide character counted as one would overflow its piece and be
    // cut by the pad, never reaching the next row.
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

fn clip_chars(text: &str, room: usize) -> String {
    text.chars().take(room).collect()
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0usize;
    for word in text.split(' ') {
        let word_w = tc::display_width(word);
        if word_w > width {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            for ch in word.chars() {
                if used >= width {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                line.push(ch);
                used += 1;
            }
            continue;
        }
        let extra = if line.is_empty() { word_w } else { word_w + 1 };
        if used + extra > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        if !line.is_empty() {
            line.push(' ');
            used += 1;
        }
        line.push_str(word);
        used += word_w;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn footer(w: usize, screen: Screen, sort: Sort, paused: bool, p: &Palette) -> (tc::Footer, usize) {
    let mut hints: Vec<Vec<(&str, String)>> = Vec::new();
    if screen == Screen::Detail {
        hints.push(vec![(p.dim.as_str(), "esc back".into())]);
    } else {
        hints.push(vec![(p.dim.as_str(), "↑↓ select".into())]);
        hints.push(vec![(p.dim.as_str(), "[↵] detail".into())]);
        hints.push(vec![(p.dim.as_str(), sort.hint().into())]);
    }
    hints.push(vec![(p.dim.as_str(), pause_hint(paused).into())]);
    hints.push(vec![(p.dim.as_str(), "[,] settings".into())]);
    hints.push(vec![(p.dim.as_str(), "[q]uit".into())]);
    let widest = hints
        .iter()
        .map(|hint| {
            hint.iter()
                .map(|(_, text)| text.chars().count())
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    let indent = usize::from(widest + 1 <= w);
    let room = w.saturating_sub(indent + 1).max(1);
    if let Some(notices) = tc::notices::hint(room) {
        let at = hints.len().saturating_sub(2);
        hints.insert(at, notices);
    }
    let mut packed = tc::pack_hints_placed(&hints, room, "  ");
    packed.lines = packed
        .lines
        .into_iter()
        .map(|line| format!("{}{}", " ".repeat(indent), line))
        .collect();
    (packed, indent)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_path_with_wide_characters_wraps_whole_by_cells() {
        let path = "/mnt/\u{6570}\u{636e}\u{76d8}/\u{5907}\u{4efd}";
        for room in 1..=12 {
            let pieces = chunks(path, room);
            assert_eq!(pieces.concat(), path, "nothing lost at {room}");
            for piece in &pieces {
                // A single wide character is wider than one cell of room and
                // still gets a row of its own rather than vanishing.
                assert!(
                    tc::display_width(piece) <= room.max(2),
                    "{piece:?} is wider than {room}"
                );
            }
        }
    }

    #[test]
    fn a_refresh_that_is_not_a_number_of_seconds_falls_back() {
        let at = |value: serde_json::Value| refresh_seconds(&serde_json::json!({ "refresh": value }));
        assert_eq!(at(serde_json::json!(1e300)), 86_400.0, "capped, so Duration cannot panic");
        assert_eq!(at(serde_json::json!(-3)), 2.0, "not positive: the default");
        assert_eq!(at(serde_json::json!(0.1)), 0.5, "floored");
        assert_eq!(at(serde_json::json!(5)), 5.0);
    }

    use super::*;

    fn space(used: u64, usable: u64, files: u64, ffree: u64) -> Space {
        Space {
            size: used + usable + 10,
            used,
            usable,
            reserved: 10,
            files,
            ffree,
        }
    }

    fn row(point: &str, fstype: &str, figures: Figures, order: usize) -> Row {
        Row {
            mount: Mount {
                dev: "9:0".into(),
                root: "/".into(),
                mount_point: point.into(),
                fstype: fstype.into(),
                source: "/dev/example".into(),
                read_only: false,
                options: "rw".into(),
            },
            figures,
            order,
        }
    }

    fn snap(rows: Vec<Row>, hidden: Hidden) -> Snap {
        let total = rows.len()
            + hidden.pseudo
            + hidden.bind
            + hidden.read_only
            + hidden.configured
            + hidden.covered;
        Snap {
            all: rows.iter().map(|row| row.mount.clone()).collect(),
            rows,
            hidden,
            total,
        }
    }

    fn view<'a>(snap: Option<&'a Snap>, cursor: &'a str) -> View<'a> {
        View {
            snap,
            err: "",
            age: Some(Duration::from_secs(2)),
            paused: false,
            every: 2.0,
            sort: Sort::Mount,
            cursor,
            screen: Screen::List,
            warn: 80.0,
            alert: 90.0,
        }
    }

    #[test]
    fn columns_drop_inodes_then_type_then_size() {
        let at = |width| {
            let cols = columns_for(width);
            (cols.inodes, cols.typ, cols.size)
        };
        assert_eq!(at(28), (false, false, false));
        assert_eq!(at(40), (false, false, true));
        for width in [60, 80, 100, 140] {
            assert_eq!(at(width), (true, true, true), "width {width}");
        }
        let mut previous = (true, true, true);
        for width in (28..=140).rev() {
            let now = at(width);
            assert!(previous.0 || !now.0, "inodes returned at {width}");
            assert!(previous.1 || !now.1, "type returned at {width}");
            assert!(previous.2 || !now.2, "size returned at {width}");
            if !now.0 {
                assert!(!previous.0 || width < 60);
            }
            // A dropped column stays dropped as the pane gets narrower,
            // and a later column in the drop order is still present only
            // while the earlier one is.
            if !now.2 {
                assert!(!now.1 && !now.0);
            }
            if !now.1 {
                assert!(!now.0);
            }
            previous = now;
        }
    }

    #[test]
    fn every_frame_row_fits_and_the_cells_are_whole() {
        let rows = vec![
            row("/", "ext4", Figures::Ready(space(80, 20, 100, 90)), 0),
            row("/a", "xfs", Figures::Ready(space(10, 90, 0, 0)), 1),
            row("/b", "nfs4", Figures::NoAnswer, 2),
        ];
        let hidden = Hidden {
            pseudo: 2,
            bind: 1,
            read_only: 1,
            configured: 0,
            covered: 0,
        };
        let snap = snap(rows, hidden);
        let p = palette();
        for width in [28, 40, 60, 80, 100, 140] {
            let drawn = draw_body(&view(Some(&snap), "/"), width, &p, None);
            for line in &drawn.rows {
                assert!(
                    tc::display_width(line) <= width,
                    "width {width} drew {} cells: {line:?}",
                    tc::display_width(line)
                );
            }
            let flat = drawn.rows.join("\n");
            assert!(flat.contains("FILESYSTEMS"), "{width}");
            assert!(flat.contains("f_bavail"), "{width}");
            assert!(flat.contains("sorted: mount"), "{width}");
            assert!(flat.contains("no answer in 2s"), "{width}");
            let cols = columns_for(width);
            if cols.inodes {
                assert!(flat.contains("n/a"), "inode n/a missing at {width}");
            }
            let header = drawn
                .rows
                .iter()
                .find(|line| tc::display_width(line) > 0 && line.contains("MOUNT"))
                .unwrap_or_else(|| panic!("no header at {width}"));
            assert!(header.contains("USED") && header.contains("USABLE"));
            assert_eq!(header.contains("INODES"), cols.inodes, "{width}");
            assert_eq!(header.contains("TYPE"), cols.typ, "{width}");
            assert_eq!(header.contains("SIZE"), cols.size, "{width}");
            let (packed, _) = footer(width, Screen::List, Sort::Mount, false, &p);
            for line in &packed.lines {
                assert!(
                    tc::display_width(line) <= width,
                    "footer at {width}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn a_long_mount_point_is_wrapped_rather_than_cut() {
        let path = "/mnt/example-data";
        let rows = vec![row(path, "ext4", Figures::Ready(space(1, 1, 10, 9)), 0)];
        let snap = snap(rows, Hidden::default());
        let p = palette();
        let drawn = draw_body(&view(Some(&snap), path), 28, &p, None);
        for line in &drawn.rows {
            assert!(tc::display_width(line) <= 28, "{line:?}");
        }
        let room = columns_for(28).mount.saturating_sub(2).max(1);
        let pieces = chunks(path, room);
        assert!(
            pieces.len() > 1,
            "the path fits the mount column, so this tests nothing"
        );
        let plain = drawn
            .rows
            .iter()
            .map(|line| plain(line))
            .collect::<Vec<_>>()
            .join("\n");
        for piece in &pieces {
            assert!(
                plain.contains(piece),
                "lost {piece:?} of {path} from a 28-column frame:\n{plain}"
            );
        }
        assert!(
            drawn.placed.len() >= pieces.len(),
            "a wrapped mount must stay one item across its rows"
        );
    }

    fn plain(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\u{1b}' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                continue;
            }
            out.push(ch);
        }
        out
    }

    #[test]
    fn the_first_frame_says_sampling_and_pause_is_named() {
        let p = palette();
        let mut pending = view(None, "");
        pending.age = None;
        let drawn = draw_body(&pending, 80, &p, None);
        let flat = drawn.rows.join("\n");
        assert!(flat.contains("sampling…"));
        assert!(!flat.contains("0%"));
        assert!(!flat.contains("0B"));

        pending.paused = true;
        let drawn = draw_body(&pending, 80, &p, None);
        assert!(drawn.rows.join("\n").contains("paused"));
        assert_eq!(pause_hint(false), "[p] pause");
        assert_eq!(pause_hint(true), "[p] resume");
        assert_eq!(Sort::Mount.hint(), "[s] sort by % used");
        assert_eq!(Sort::Mount.next().hint(), "[s] sort by inodes");
    }

    #[test]
    fn percent_sort_puts_the_fullest_first_and_a_missing_row_last() {
        let rows = vec![
            row("/low", "ext4", Figures::Ready(space(10, 90, 10, 9)), 0),
            row("/high", "ext4", Figures::Ready(space(90, 10, 10, 1)), 1),
            row("/wait", "ext4", Figures::NoAnswer, 2),
        ];
        let snap = snap(rows, Hidden::default());
        let order = ordered(&snap, Sort::Percent);
        let names: Vec<&str> = order
            .iter()
            .map(|&index| snap.rows[index].mount.mount_point.as_str())
            .collect();
        assert_eq!(names, vec!["/high", "/low", "/wait"]);
    }

    #[test]
    fn a_stopped_poller_is_a_row() {
        let p = palette();
        let mut pending = view(None, "");
        pending.err = "stopped: poller panicked";
        pending.age = None;
        let drawn = draw_body(&pending, 60, &p, None);
        assert!(drawn.rows.iter().any(|line| line.contains("stopped:")));
    }
}
