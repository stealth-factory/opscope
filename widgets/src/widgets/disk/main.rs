// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Which filesystems can still take a write, which disks are busy, and, on Linux, whether I/O is stalling.
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
mod state;
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

    pub fn stat_mount(_path: &[u8]) -> Result<Space, String> {
        Err(opscope_core::unsupported())
    }

    pub fn sample_io(
        _mounts: &[super::parse::Mount],
        _every: f64,
    ) -> Result<super::parse::IoSnapshot, String> {
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
/// Drop order as the pane shrinks is the one-hour rate, then inodes, then
/// type, then size. The window rate and FULL IN stay. The bar shrinks from
/// 11 cells to 4 before a column is dropped. A row that still does not fit
/// wraps whole cells onto the next line.
const MOUNT_MIN: usize = 5;
const USED_W: usize = 5;
const USABLE_W: usize = 6;
const PCT_W: usize = 4;
const SIZE_W: usize = 5;
const TYPE_W: usize = 12;
const INODES_W: usize = 6;
const HOUR_W: usize = 8;
const DAY_W: usize = 8;
const FULL_W: usize = 13;
const BAR_MAX: usize = 11;
const BAR_MIN: usize = 4;
const WINDOWS: [u64; 3] = [7, 14, 30];

#[derive(Clone, Copy, PartialEq, Eq)]
struct Columns {
    mount: usize,
    typ: bool,
    size: bool,
    inodes: bool,
    hour: bool,
    bar: usize,
}

fn columns_for(width: usize) -> Columns {
    // Drop order as the pane shrinks: 1h Δ, then inodes, then type, then
    // size. The window Δ and FULL IN stay; they are why the section exists.
    // The sets are that order reversed, so the first one that fits is the
    // widest the pane can hold.
    let sets = [
        (true, true, true, true),
        (false, true, true, true),
        (false, false, true, true),
        (false, false, false, true),
        (false, false, false, false),
    ];
    for &(hour, inodes, typ, size) in &sets {
        for bar in (BAR_MIN..=BAR_MAX).rev() {
            let need = column_need(hour, inodes, typ, size, bar);
            if need <= width {
                return Columns {
                    mount: MOUNT_MIN + width - need,
                    typ,
                    size,
                    inodes,
                    hour,
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
        hour: false,
        bar: BAR_MIN,
    }
}

fn column_need(hour: bool, inodes: bool, typ: bool, size: bool, bar: usize) -> usize {
    let mut width = MOUNT_MIN + USED_W + USABLE_W + bar + PCT_W + DAY_W + FULL_W;
    let mut gaps = 6;
    if hour {
        width += HOUR_W;
        gaps += 1;
    }
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
    Fills,
    Percent,
    Inodes,
}

impl Sort {
    fn next(self) -> Self {
        match self {
            Self::Mount => Self::Fills,
            Self::Fills => Self::Percent,
            Self::Percent => Self::Inodes,
            Self::Inodes => Self::Mount,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Mount => "mount",
            Self::Fills => "fills soonest",
            Self::Percent => "% used",
            Self::Inodes => "inodes",
        }
    }

    /// The footer names the sort the next press moves to.
    fn hint(self) -> &'static str {
        match self {
            Self::Mount => "[s] sort by fills soonest",
            Self::Fills => "[s] sort by % used",
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

/// Item ids for a click. Filesystem rows stay below this so a phase-1
/// index is unchanged. Disks and the pressure row sit above it.
const DISK_BASE: usize = 1_000_000;
const PRESSURE_ID: usize = 2_000_000;
const SERIES_CAP: usize = 60;

const DISK_MIN: usize = 9;
const DISK_TYPE_W: usize = 7;
const DISK_SIZE_W: usize = 5;
const READ_W: usize = 5;
const WRITE_W: usize = 5;
const IOPS_W: usize = 5;
const AWAIT_W: usize = 5;
const BUSY_W: usize = 5;
const QUEUE_W: usize = 5;
const TPS_W: usize = 6;
const MB_W: usize = 6;

/// Linux DISKS columns. Drop order as the pane shrinks: queue, await,
/// type, size, then iops. The name, the two rates and the busy share stay.
#[derive(Clone, Copy)]
struct DiskCols {
    disk: usize,
    typ: bool,
    size: bool,
    iops: bool,
    await_ms: bool,
    queue: bool,
}

fn disk_columns(width: usize) -> DiskCols {
    // (iops, size, typ, await, queue) — the reverse of the drop order.
    let sets = [
        (true, true, true, true, true),
        (true, true, true, true, false),
        (true, true, true, false, false),
        (true, true, false, false, false),
        (true, false, false, false, false),
        (false, false, false, false, false),
    ];
    for &(iops, size, typ, await_ms, queue) in &sets {
        let need = disk_need(iops, size, typ, await_ms, queue);
        if need <= width {
            return DiskCols {
                disk: DISK_MIN + width - need,
                typ,
                size,
                iops,
                await_ms,
                queue,
            };
        }
    }
    DiskCols {
        disk: DISK_MIN,
        typ: false,
        size: false,
        iops: false,
        await_ms: false,
        queue: false,
    }
}

fn disk_need(iops: bool, size: bool, typ: bool, await_ms: bool, queue: bool) -> usize {
    let mut width = DISK_MIN + READ_W + WRITE_W + BUSY_W;
    let mut gaps = 3;
    if iops {
        width += IOPS_W;
        gaps += 1;
    }
    if size {
        width += DISK_SIZE_W;
        gaps += 1;
    }
    if typ {
        width += DISK_TYPE_W;
        gaps += 1;
    }
    if await_ms {
        width += AWAIT_W;
        gaps += 1;
    }
    if queue {
        width += QUEUE_W;
        gaps += 1;
    }
    width + gaps
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiskSort {
    Write,
    Read,
    Name,
}

impl DiskSort {
    fn next(self) -> Self {
        match self {
            Self::Write => Self::Read,
            Self::Read => Self::Name,
            Self::Name => Self::Write,
        }
    }

    /// What the body says is in force. On macOS the write key is combined
    /// MB/s and the read key is transfers per second, which is what `iostat`
    /// reports.
    fn label(self, combined: bool) -> &'static str {
        match self {
            Self::Write if combined => "MB/s",
            Self::Read if combined => "tps",
            Self::Write => "write",
            Self::Read => "read",
            Self::Name => "name",
        }
    }

    /// The sort the next press moves to.
    fn hint(self, combined: bool) -> &'static str {
        match self {
            Self::Write if combined => "[s] sort by tps",
            Self::Name if combined => "[s] sort by MB/s",
            Self::Write => "[s] sort by read",
            Self::Read => "[s] sort by name",
            Self::Name => "[s] sort by write",
        }
    }
}

fn idle_hint(show_idle: bool) -> &'static str {
    if show_idle {
        "[v] hide idle disks"
    } else {
        "[v] show idle disks"
    }
}

#[derive(Clone, Debug)]
enum ShownRate {
    Measuring,
    Gap,
    Linux(parse::Rates),
    Mac(parse::IostatSample),
}

#[derive(Clone, Debug)]
struct DiskViewRow {
    disk: parse::LiveDisk,
    rate: ShownRate,
    read: Vec<Option<f64>>,
    write: Vec<Option<f64>>,
}

#[derive(Clone, Debug)]
struct DiskBoard {
    rows: Vec<DiskViewRow>,
    show_pressure: bool,
    pressure: Option<parse::Pressure>,
    pressure_absent: bool,
    pressure_err: String,
    host_devices: bool,
    combined: bool,
    missing: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    List,
    Detail,
    DiskDetail,
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
    down: String,
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
        down: tc::rgb(255, 176, 110),
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
    board: Option<&'a DiskBoard>,
    disk_err: &'a str,
    disk_sort: DiskSort,
    focus: usize,
    show_idle: bool,
    disk_cursor: &'a str,
    hide_devices: &'a [String],
    usage: &'a state::Store,
    window_days: u64,
    now: f64,
    history_note: &'a str,
    save_err: &'a str,
}

struct Drawn {
    rows: Vec<String>,
    placed: Vec<(usize, usize)>,
    head: usize,
    selected: usize,
    disk_selected: usize,
    plots: Vec<tc::Plot>,
    lens: [usize; 3],
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
    let hide_devices = tc::cfg_strings(&cfg, "hide_devices", &[]);
    let warn = tc::cfg_f64(&cfg, "warn_percent", 80.0);
    let alert = tc::cfg_f64(&cfg, "alert_percent", 90.0);
    let mut window_days = window_days_of(&cfg);

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
    let mut disk_sort = DiskSort::Write;
    let mut screen = Screen::List;
    let mut cursor = String::new();
    let mut disk_cursor = String::new();
    let mut scroll = 0usize;
    let mut chase = true;
    let mut placed: Vec<(usize, usize)> = Vec::new();
    let mut head = 1usize;
    let mut shown_scroll = 0usize;
    let mut fs_at = 0usize;
    let mut disk_at = 0usize;
    let mut focus = 0usize;
    let mut lens = [0usize; 3];
    let mut show_idle = false;

    loop {
        let mut keys = keyboard.poll();
        // Set by `enter`, acted on after the batch's arrows have moved the
        // cursor, so an arrow and an enter read together open the new row.
        let mut open = false;
        if screen == Screen::List {
            let current = match focus {
                0 => fs_at,
                1 => DISK_BASE + disk_at,
                _ => PRESSURE_ID,
            };
            if let Some(at) =
                tc::rows_clicked(&mut keys, Some(current), head, shown_scroll, &placed, None)
            {
                if at >= PRESSURE_ID {
                    focus = 2;
                } else if at >= DISK_BASE {
                    focus = 1;
                    disk_at = at - DISK_BASE;
                } else {
                    focus = 0;
                    fs_at = at;
                }
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
                        let at = section_index(focus, fs_at, disk_at);
                        if let Some((next, row)) = tc::step_across_sections(focus, at, &lens, false)
                        {
                            focus = next;
                            set_section_index(next, row, &mut fs_at, &mut disk_at);
                            chase = true;
                        }
                    }
                }
                "down" | "j" | "J" => {
                    if screen == Screen::List {
                        let at = section_index(focus, fs_at, disk_at);
                        if let Some((next, row)) = tc::step_across_sections(focus, at, &lens, true)
                        {
                            focus = next;
                            set_section_index(next, row, &mut fs_at, &mut disk_at);
                            chase = true;
                        }
                    }
                }
                "tab" => {
                    if screen == Screen::List {
                        focus = tc::next_section(Some(focus), &lens)
                            .or_else(|| tc::next_section(None, &lens))
                            .unwrap_or(focus);
                        chase = true;
                    }
                }
                "v" | "V" => {
                    show_idle = !show_idle;
                }
                "enter" => {
                    if screen == Screen::List {
                        // Acted on after this batch's arrows have moved the
                        // cursor, so an arrow and an enter read together open
                        // the row the cursor lands on.
                        open = true;
                    }
                }
                "esc" => {
                    if screen != Screen::List {
                        screen = Screen::List;
                        scroll = 0;
                        chase = true;
                    }
                }
                "s" | "S" => {
                    match focus {
                        0 => sort = sort.next(),
                        1 => disk_sort = disk_sort.next(),
                        _ => {}
                    }
                    chase = false;
                }
                "w" | "W" => {
                    window_days = tc::cycle(&WINDOWS, window_days);
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

        let (snap, err, sampled, board, disk_err, usage, history_note, save_err) = {
            let guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                guard.snap.clone(),
                guard.err.clone(),
                guard.sampled,
                guard.board.clone(),
                guard.disk_err.clone(),
                Arc::clone(&guard.usage),
                guard.history_note.clone(),
                guard.save_err.clone(),
            )
        };
        let age = sampled.map(|at| at.elapsed());
        let paused = pause.load(Ordering::Acquire);
        if let Some(snap) = snap.as_ref() {
            let count = snap.rows.len();
            let missing = !snap.rows.iter().any(|row| row.mount.mount_point == cursor);
            if count > 0 && ((screen == Screen::List && chase && focus == 0) || missing) {
                if let Some(path) = path_at(snap, sort, fs_at.min(count - 1), &usage, window_days) {
                    cursor = path;
                }
            }
        }
        if let Some(board) = board.as_ref() {
            let indexes = ordered_disks(board, disk_sort, show_idle, &hide_devices);
            let missing = !indexes
                .iter()
                .any(|&index| board.rows[index].disk.name == disk_cursor);
            if !indexes.is_empty() && ((screen == Screen::List && chase && focus == 1) || missing) {
                let at = disk_at.min(indexes.len() - 1);
                disk_cursor = board.rows[indexes[at]].disk.name.clone();
            }
        }
        if open {
            match focus {
                0 => {
                    screen = Screen::Detail;
                    scroll = 0;
                    chase = true;
                }
                1 => {
                    screen = Screen::DiskDetail;
                    scroll = 0;
                    chase = true;
                }
                _ => {}
            }
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
            board: board.as_ref(),
            disk_err: &disk_err,
            disk_sort,
            focus,
            show_idle,
            disk_cursor: &disk_cursor,
            hide_devices: &hide_devices,
            usage: &usage,
            window_days,
            now: tc::now(),
            history_note: &history_note,
            save_err: &save_err,
        };
        let (w, h) = tc::size();
        let drawn = draw_body(&view, w, &p, refresh.tick(tc::animation_tick()));
        fs_at = drawn.selected;
        disk_at = drawn.disk_selected;
        lens = drawn.lens;
        let cursor_gone = snap
            .as_ref()
            .is_some_and(|snap| !snap.rows.iter().any(|row| row.mount.mount_point == cursor));
        if cursor.is_empty() || cursor_gone {
            if let Some(path) = snap
                .as_ref()
                .and_then(|snap| path_at(snap, sort, fs_at, &usage, window_days))
            {
                cursor = path;
            }
        }
        if disk_cursor.is_empty()
            || board.as_ref().is_some_and(|board| {
                ordered_disks(board, disk_sort, show_idle, &hide_devices)
                    .iter()
                    .all(|&index| board.rows[index].disk.name != disk_cursor)
            })
        {
            if let Some(name) = board.as_ref().and_then(|board| {
                let indexes = ordered_disks(board, disk_sort, show_idle, &hide_devices);
                indexes
                    .get(disk_at)
                    .map(|&index| board.rows[index].disk.name.clone())
            }) {
                disk_cursor = name;
            }
        }
        let combined = board.as_ref().is_some_and(|board| board.combined);
        let (packed, indent) = footer(
            w,
            screen,
            sort,
            disk_sort,
            focus,
            show_idle,
            paused,
            window_days,
            combined,
            &p,
        );
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
        let follow_id = match focus {
            0 => fs_at,
            1 => DISK_BASE + disk_at,
            _ => PRESSURE_ID,
        };
        if chase {
            if let Some(at) = drawn
                .placed
                .iter()
                .find(|(_, item)| *item == follow_id)
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
        let plots: Vec<tc::Plot> = drawn
            .plots
            .into_iter()
            .filter_map(|plot| plot.in_viewport(scroll, head_rows, room_below))
            .collect();
        tc::draw_plots(&rows, w, h, &plots);
        keyboard.footer_at(&packed, foot_top, indent);
        placed = drawn.placed;
        head = head_rows;
        shown_scroll = scroll;
        // Frame-local: only the frame a key moved the selection follows it.
        // Left set, a refresh that reorders the rows would hand the cursor
        // to whichever mount or disk now sits at the old index.
        chase = false;
        keyboard.wait(Duration::from_millis(200));
    }
}

fn section_index(focus: usize, fs_at: usize, disk_at: usize) -> usize {
    match focus {
        0 => fs_at,
        1 => disk_at,
        _ => 0,
    }
}

fn set_section_index(focus: usize, row: usize, fs_at: &mut usize, disk_at: &mut usize) {
    match focus {
        0 => *fs_at = row,
        1 => *disk_at = row,
        _ => {}
    }
}

fn path_at(
    snap: &Snap,
    sort: Sort,
    selected: usize,
    usage: &state::Store,
    window_days: u64,
) -> Option<String> {
    let ordered = ordered(snap, sort, usage, window_days, tc::now());
    ordered
        .get(selected)
        .map(|&index| snap.rows[index].mount.mount_point.clone())
}

struct State {
    snap: Option<Snap>,
    err: String,
    sampled: Option<Instant>,
    board: Option<DiskBoard>,
    disk_err: String,
    usage: Arc<state::Store>,
    history_note: String,
    save_err: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            snap: None,
            err: String::new(),
            sampled: None,
            board: None,
            disk_err: String::new(),
            usage: Arc::new(state::Store::default()),
            history_note: String::new(),
            save_err: String::new(),
        }
    }
}

struct RateMemory {
    prev: HashMap<String, parse::DiskStat>,
    at: Option<Instant>,
    series: HashMap<String, (Vec<Option<f64>>, Vec<Option<f64>>)>,
}

impl Default for RateMemory {
    fn default() -> Self {
        Self {
            prev: HashMap::new(),
            at: None,
            series: HashMap::new(),
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
    let mut inflight: HashMap<Vec<u8>, Receiver<Result<Space, String>>> = HashMap::new();
    let mut rates = RateMemory::default();
    let loaded = state::load_usage(&state::usage_path());
    let mut usage = loaded.store;
    let history_note = loaded.note;
    let mut save_err = String::new();
    let mut pending_save = false;
    let mut last_save = Instant::now() - Duration::from_secs(120);
    if let Ok(mut guard) = state.lock() {
        guard.usage = Arc::new(usage.clone());
        guard.history_note = history_note.clone();
    }
    loop {
        if pause.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        let work = refresh.start();
        let round = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let filesystems = sample(&hide, &mut inflight);
            let mounts = filesystems
                .as_ref()
                .map(|snap| snap.all.clone())
                .unwrap_or_default();
            let io = host::sample_io(&mounts, every);
            (filesystems, io)
        }));
        match round {
            Ok((filesystems, io)) => {
                if let Ok(mut guard) = state.lock() {
                    match filesystems {
                        Ok(snap) => {
                            if !pause.load(Ordering::Acquire) {
                                let now = tc::now();
                                for row in &snap.rows {
                                    if let Figures::Ready(space) = &row.figures {
                                        let id = state::key(
                                            &row.mount.mount_point,
                                            &row.mount.fstype,
                                            space.blocks,
                                        );
                                        let series = usage.series.entry(id).or_default();
                                        if state::record(series, now, space.used) {
                                            pending_save = true;
                                        }
                                    }
                                }
                            }
                            guard.snap = Some(snap);
                            guard.err.clear();
                            guard.sampled = Some(Instant::now());
                            guard.usage = Arc::new(usage.clone());
                            guard.save_err = save_err.clone();
                        }
                        Err(err) => guard.err = err,
                    }
                    match io {
                        Ok(snap) => {
                            guard.disk_err = snap.err.clone();
                            guard.board = Some(board_from(&snap, &mut rates));
                        }
                        Err(err) => guard.disk_err = err,
                    }
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
        if pending_save && last_save.elapsed() >= Duration::from_secs(60) {
            match state::save_usage(&state::usage_path(), &usage) {
                Ok(merged) => {
                    usage = merged;
                    save_err.clear();
                    pending_save = false;
                    last_save = Instant::now();
                }
                Err(err) => {
                    save_err = err;
                    last_save = Instant::now();
                }
            }
            if let Ok(mut guard) = state.lock() {
                guard.usage = Arc::new(usage.clone());
                guard.save_err = save_err.clone();
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
    inflight: &mut HashMap<Vec<u8>, Receiver<Result<Space, String>>>,
) -> Result<Snap, String> {
    let all = host::read_mounts().map_err(|err| format!("mounts unavailable: {err}"))?;
    let (visible, hidden) = parse::visible_mounts(&all, hide);
    // The raw path, not the display text: `statvfs` has to be given the
    // bytes the kernel published, including ones that are not UTF-8.
    let live: Vec<Vec<u8>> = visible.iter().map(|mount| mount.path.clone()).collect();
    inflight.retain(|path, _| live.iter().any(|live| live == path));

    let mut fresh = Vec::new();
    let mut figures: HashMap<Vec<u8>, Figures> = HashMap::new();
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
            let figures = figures.remove(&mount.path).unwrap_or(Figures::NoAnswer);
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

fn window_days_of(cfg: &serde_json::Value) -> u64 {
    let days = tc::cfg_usize(cfg, "window_days", 7) as u64;
    if WINDOWS.contains(&days) {
        days
    } else {
        7
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
    let fs_plots_slot = Mutex::new(Vec::new());
    let section = tc::guard_rows("filesystems", width, || {
        let mut body = Vec::new();
        let mut at = Vec::new();
        let mut plots = Vec::new();
        paint(view, width, p, &mut body, &mut at, &mut plots);
        if let Ok(mut slot) = placed_slot.lock() {
            *slot = at;
        }
        if let Ok(mut slot) = fs_plots_slot.lock() {
            *slot = plots;
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
    let mut plots = Vec::new();
    if !failed {
        let base = head;
        let fs_plots = fs_plots_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        for mut plot in fs_plots {
            plot.y += base;
            plots.push(plot);
        }
    }
    let fs_len = placed
        .iter()
        .map(|(_, item)| *item)
        .max()
        .map(|item| item + 1)
        .unwrap_or(0);
    let mut disk_len = 0usize;
    let mut pressure_len = 0usize;
    if view.screen != Screen::Detail {
        let disk_slot: Mutex<(Vec<(usize, usize)>, Vec<tc::Plot>, usize)> =
            Mutex::new((Vec::new(), Vec::new(), 0));
        let disk_section = tc::guard_rows("disks", width, || {
            let mut body = Vec::new();
            let mut at = Vec::new();
            let mut local_plots = Vec::new();
            let count = paint_disks(view, width, p, &mut body, &mut at, &mut local_plots);
            if let Ok(mut slot) = disk_slot.lock() {
                *slot = (at, local_plots, count);
            }
            body
        });
        let disk_failed = disk_section
            .iter()
            .any(|line| line.contains("could not draw"));
        let (mut disk_placed, disk_plots, count) = disk_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if disk_failed {
            disk_placed.clear();
        } else {
            disk_len = count;
            let base = rows.len();
            for (row, _) in &mut disk_placed {
                *row += base;
            }
            for plot in disk_plots {
                let mut plot = plot;
                plot.y += base;
                plots.push(plot);
            }
            placed.extend(disk_placed);
        }
        rows.extend(disk_section);
        if view.screen == Screen::List {
            if let Some(board) = view.board {
                if board.show_pressure {
                    let pressure_slot = Mutex::new(Vec::new());
                    let pressure = tc::guard_rows("pressure", width, || {
                        let mut body = Vec::new();
                        let mut at = Vec::new();
                        paint_pressure(board, view.focus, width, p, &mut body, &mut at);
                        if let Ok(mut slot) = pressure_slot.lock() {
                            *slot = at;
                        }
                        body
                    });
                    let failed = pressure.iter().any(|line| line.contains("could not draw"));
                    let mut at = pressure_slot
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clone();
                    if !failed {
                        pressure_len = usize::from(!at.is_empty());
                        let base = rows.len();
                        for (row, _) in &mut at {
                            *row += base;
                        }
                        placed.extend(at);
                    }
                    rows.extend(pressure);
                }
            }
        }
    }
    let selected = view
        .snap
        .and_then(|snap| {
            let ordered = ordered(snap, view.sort, view.usage, view.window_days, view.now);
            ordered
                .iter()
                .position(|&index| snap.rows[index].mount.mount_point == view.cursor)
        })
        .unwrap_or(0);
    let disk_selected = view
        .board
        .and_then(|board| {
            ordered_disks(board, view.disk_sort, view.show_idle, view.hide_devices)
                .iter()
                .position(|&index| board.rows[index].disk.name == view.disk_cursor)
        })
        .unwrap_or(0);
    let lens = if view.screen == Screen::List {
        [fs_len, disk_len, pressure_len]
    } else {
        [0, 0, 0]
    };
    Drawn {
        rows,
        placed,
        head,
        selected,
        disk_selected,
        plots,
        lens,
    }
}

fn paint(
    view: &View,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    placed: &mut Vec<(usize, usize)>,
    plots: &mut Vec<tc::Plot>,
) {
    for line in wrap(&status_line(view), width.saturating_sub(1)) {
        rows.push(tc::seg(&[(p.dim.as_str(), format!(" {line}"))], width));
    }
    if !view.err.is_empty() {
        for line in wrap(view.err, width.saturating_sub(1)) {
            rows.push(tc::seg(&[(p.accent.as_str(), format!(" {line}"))], width));
        }
    }
    for note in [view.history_note, view.save_err] {
        if note.is_empty() {
            continue;
        }
        for line in wrap(note, width.saturating_sub(1)) {
            rows.push(tc::seg(&[(p.accent.as_str(), format!(" {line}"))], width));
        }
    }
    if view.screen == Screen::DiskDetail {
        return;
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
        paint_detail(view, snap, width, p, rows, plots);
        return;
    }
    if snap.rows.is_empty() {
        return;
    }
    let cols = columns_for(width);
    rows.extend(header_row(&cols, view.window_days, width, p));
    let ordered = ordered(snap, view.sort, view.usage, view.window_days, view.now);
    for (item, index) in ordered.iter().copied().enumerate() {
        let row = &snap.rows[index];
        let here = view.focus == 0 && row.mount.mount_point == view.cursor;
        let from = rows.len();
        rows.extend(data_rows(
            row,
            &cols,
            width,
            here,
            view.warn,
            view.alert,
            view.usage,
            view.window_days,
            view.now,
            p,
        ));
        placed.extend((from..rows.len()).map(|at| (at, item)));
    }
    for line in footnote_lines(view, snap, width, p) {
        rows.push(line);
    }
}

fn paint_detail(
    view: &View,
    snap: &Snap,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    plots: &mut Vec<tc::Plot>,
) {
    let Some(row) = ordered(snap, view.sort, view.usage, view.window_days, view.now)
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
    if let Some(board) = view.board {
        let lives: Vec<parse::LiveDisk> = board.rows.iter().map(|row| row.disk.clone()).collect();
        if let Some(line) = parse::device_of(&mount.dev, &lives) {
            lines.insert(2, line);
        }
    }
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
    if let Figures::Ready(_) = &row.figures {
        push_usage(row, view, width, p, rows, plots);
    }
}

fn header_row(cols: &Columns, window_days: u64, width: usize, p: &Palette) -> Vec<String> {
    let dim = p.dim.clone();
    let left = |text: &str, width: usize| (dim.clone(), tc::pad(text, width));
    let right = |text: String, width: usize| (dim.clone(), align_right(text, width));
    let mut cells = vec![left("MOUNT", cols.mount)];
    if cols.typ {
        cells.push(left("TYPE", TYPE_W));
    }
    if cols.size {
        cells.push(left("SIZE", SIZE_W));
    }
    cells.push(right("USED".into(), USED_W));
    cells.push(right("USABLE".into(), USABLE_W));
    cells.push(left("", cols.bar));
    cells.push(right("%".into(), PCT_W));
    if cols.hour {
        cells.push(right("1h Δ".into(), HOUR_W));
    }
    cells.push(right(format!("{window_days}d Δ"), DAY_W));
    cells.push(right("FULL IN".into(), FULL_W));
    if cols.inodes {
        cells.push(right("INODES".into(), INODES_W));
    }
    pack_cells(cells, width, "")
}

fn data_rows(
    row: &Row,
    cols: &Columns,
    width: usize,
    here: bool,
    warn: f64,
    alert: f64,
    usage: &state::Store,
    window_days: u64,
    now: f64,
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
        let series = series_for(usage, row);
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
                let (hour, window) = slopes(series, window_days, now);
                if cols.hour {
                    cells.push((c(p.txt.as_str()), rate_cell(hour, 3_600.0, HOUR_W)));
                }
                cells.push((c(p.txt.as_str()), rate_cell(window, 86_400.0, DAY_W)));
                cells.push((
                    c(p.txt.as_str()),
                    full_cell(space.usable, window, hour, series, window_days, now),
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
        let tint_fill = if here { tint.as_str() } else { "" };
        out.extend(pack_cells(cells, width, tint_fill));
    }
    out
}

fn ordered(
    snap: &Snap,
    sort: Sort,
    usage: &state::Store,
    window_days: u64,
    now: f64,
) -> Vec<usize> {
    let mut index: Vec<usize> = (0..snap.rows.len()).collect();
    // One fit per row. Doing it inside the comparison refits the same
    // history for every pair, and the frame sorts again for the cursor
    // and the footnote.
    let fills = if sort == Sort::Fills {
        snap.rows
            .iter()
            .map(|row| fill_ord(usage, row, window_days, now))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    index.sort_by(|&left, &right| {
        let mount = snap.rows[left].order.cmp(&snap.rows[right].order);
        match sort {
            Sort::Mount => mount,
            Sort::Fills => fills[left].cmp(&fills[right]).then(mount),
            Sort::Percent => percent_key(&snap.rows[right])
                .total_cmp(&percent_key(&snap.rows[left]))
                .then(mount),
            Sort::Inodes => inode_ord(&snap.rows[right])
                .cmp(&inode_ord(&snap.rows[left]))
                .then(mount),
        }
    });
    index
}

fn fill_ord(usage: &state::Store, row: &Row, days: u64, now: f64) -> (u8, u64) {
    // 0 is a real projection, 1 is not filling or still measuring, 2 has
    // not answered. Mount order is the tie-break, so 2 has to be its own
    // rank or a quiet row with a later mount sorts after a row with no answer.
    let Figures::Ready(space) = &row.figures else {
        return (2, 0);
    };
    let Some(series) = series_for(usage, row) else {
        return (1, 0);
    };
    let slope = state::slope_bytes_per_s(series, days as f64 * 86_400.0, now);
    match state::eta(space.usable, slope) {
        state::Eta::In(secs) => (0, secs.max(0.0) as u64),
        _ => (1, 0),
    }
}

fn series_for<'a>(usage: &'a state::Store, row: &Row) -> Option<&'a state::Series> {
    let Figures::Ready(space) = &row.figures else {
        return None;
    };
    usage.series.get(&state::key(
        &row.mount.mount_point,
        &row.mount.fstype,
        space.blocks,
    ))
}

fn slopes(
    series: Option<&state::Series>,
    days: u64,
    now: f64,
) -> (Option<state::Slope>, Option<state::Slope>) {
    let Some(series) = series else {
        return (None, None);
    };
    (
        state::slope_bytes_per_s(series, 3_600.0, now),
        state::slope_bytes_per_s(series, days as f64 * 86_400.0, now),
    )
}

fn align_right(text: String, width: usize) -> String {
    let n = text.chars().count();
    if n >= width {
        text
    } else {
        format!("{text:>width$}")
    }
}

fn rate_cell(slope: Option<state::Slope>, seconds: f64, width: usize) -> String {
    let text = match slope {
        Some(slope) => signed_rate(slope.per_s, seconds),
        None => "—".into(),
    };
    align_right(text, width)
}

fn signed_rate(per_s: f64, seconds: f64) -> String {
    let bytes = per_s * seconds;
    let sign = if bytes < 0.0 { '-' } else { '+' };
    let abs = bytes.abs().round() as u64;
    let suffix = if seconds >= 86_400.0 { "/d" } else { "/h" };
    format!("{sign}{}{suffix}", human(abs))
}

fn full_cell(
    usable: u64,
    window: Option<state::Slope>,
    hour: Option<state::Slope>,
    series: Option<&state::Series>,
    days: u64,
    now: f64,
) -> String {
    let eta_window = state::eta(usable, window);
    let eta_hour = state::eta(usable, hour);
    let text = match eta_window {
        state::Eta::NotFilling => "not filling".into(),
        state::Eta::Measuring(span) => {
            let span = if span > 0.0 {
                span
            } else {
                series
                    .map(|series| state::covered(series, days as f64 * 86_400.0, now).0)
                    .unwrap_or(0.0)
            };
            format!("measuring {}", tc::age(span))
        }
        state::Eta::In(secs) => {
            let mut text = format!("~{}", eta_words(secs));
            if let Some(mark) = state::arrow(eta_window, eta_hour) {
                text.push(' ');
                text.push(mark);
            }
            text
        }
    };
    align_right(text, FULL_W)
}

fn eta_words(secs: f64) -> String {
    let secs = secs.max(0.0);
    let days = (secs / 86_400.0) as u64;
    let hours = ((secs % 86_400.0) / 3_600.0) as u64;
    let mins = ((secs % 3_600.0) / 60.0) as u64;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

fn pack_cells(cells: Vec<(String, String)>, width: usize, tint: &str) -> Vec<String> {
    let width = width.max(1);
    let mut groups: Vec<Vec<(String, String)>> = Vec::new();
    let mut current: Vec<(String, String)> = Vec::new();
    let mut used = 0usize;
    for (colour, text) in cells {
        let cell_w = tc::display_width(&text);
        let gap = usize::from(!current.is_empty());
        if !current.is_empty() && used + gap + cell_w > width {
            groups.push(std::mem::take(&mut current));
            used = 0;
        }
        if !current.is_empty() {
            used += 1;
        }
        used += cell_w;
        current.push((colour, text));
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
        .into_iter()
        .map(|group| {
            let mut parts: Vec<(&str, String)> = Vec::new();
            for (index, (colour, text)) in group.iter().enumerate() {
                if index > 0 {
                    parts.push((colour.as_str(), " ".into()));
                }
                parts.push((colour.as_str(), text.clone()));
            }
            if !tint.is_empty() {
                let taken: usize = parts.iter().map(|(_, text)| tc::display_width(text)).sum();
                if taken < width {
                    parts.push((tint, " ".repeat(width - taken)));
                }
            }
            tc::seg(&parts, width)
        })
        .collect()
}

fn footnote_lines(view: &View, snap: &Snap, width: usize, p: &Palette) -> Vec<String> {
    let indexes = ordered(snap, view.sort, view.usage, view.window_days, view.now);
    let row = indexes
        .iter()
        .copied()
        .find(|&index| snap.rows[index].mount.mount_point == view.cursor)
        .or(indexes.first().copied())
        .map(|index| &snap.rows[index]);
    let days = view.window_days;
    let window_s = days as f64 * 86_400.0;
    let fit = row
        .and_then(|row| series_for(view.usage, row))
        .map(|series| state::fit_window(series, window_s, view.now));
    let (fit_start, retained, span) = fit
        .map(|fit| (fit.start, fit.retained_since, fit.span_s))
        .unwrap_or((0.0, 0.0, 0.0));
    let labelled = |t: f64| {
        if t > 0.0 {
            stamp(t)
        } else {
            "—".into()
        }
    };
    // `retained` is how far back the file goes. The rate itself starts at
    // the first sample inside the window, which is later once history is
    // older than the window on screen.
    let body = if retained > 0.0 && span + 60.0 >= window_s {
        format!(
            "FULL IN at the {days}d rate of this widget's own samples (kept since {})",
            labelled(retained)
        )
    } else {
        format!(
            "FULL IN at the rate since {} ({of} of {days}d)",
            labelled(fit_start),
            of = tc::age(span)
        )
    };
    // The mark is built here so the sentence on screen still reads
    // `· ↑↓ = …` while no one source line is both a footer and a bare `d`.
    let text = format!(
        "{body} {}",
        "\u{b7} \u{2191}\u{2193} = 1h rate sooner/later"
    );
    let words: Vec<Vec<(&str, String)>> = text
        .split(' ')
        .filter(|word| !word.is_empty())
        .map(|word| vec![(p.dim.as_str(), word.to_string())])
        .collect();
    let room = width.saturating_sub(1).max(1);
    tc::pack_hints(&words, room, " ")
        .into_iter()
        .map(|line| {
            let padded = format!(" {line}");
            if tc::display_width(&padded) <= width {
                padded
            } else {
                line
            }
        })
        .collect()
}

fn axis_stamp(t: f64) -> String {
    chrono::DateTime::from_timestamp(t.floor() as i64, 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%b %d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "—".into())
}

fn stamp(t: f64) -> String {
    chrono::DateTime::from_timestamp(t.floor() as i64, 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%a %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "—".into())
}

fn push_usage(
    row: &Row,
    view: &View,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    plots: &mut Vec<tc::Plot>,
) {
    let window_s = view.window_days as f64 * 86_400.0;
    let path = state::usage_path();
    let Some(series) = series_for(view.usage, row) else {
        for line in wrap("no samples yet", width.saturating_sub(1)) {
            rows.push(tc::seg(&[(p.dim.as_str(), format!(" {line}"))], width));
        }
        push_wrapped(&path.display().to_string(), width, p, rows);
        return;
    };
    // The open minute is rewritten on every poll. Plot the minutes that
    // have already closed so the line and its scale stay put until the
    // next one lands.
    let plotted = state::settled(series, view.now);
    let end = plotted
        .samples
        .last()
        .map(|(t, _)| (t / 60.0).round() * 60.0 + 30.0)
        .unwrap_or(view.now);
    let (raw, positions) = state::trace(&plotted, window_s, end);
    let seen: Vec<f64> = raw.iter().copied().flatten().collect();
    let (hour, window) = slopes(Some(series), view.window_days, view.now);
    let (span, count) = state::covered(series, window_s, view.now);
    let summary = format!(
        "{count} samples over {} · 1h {} · {}d {}",
        tc::age(span),
        slope_text(hour, 3_600.0),
        view.window_days,
        slope_text(window, 86_400.0)
    );
    push_wrapped(&summary, width, p, rows);
    if seen.is_empty() {
        push_wrapped(&path.display().to_string(), width, p, rows);
        return;
    }
    let lo = seen.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = seen.iter().copied().fold(0.0_f64, f64::max);
    let start = end - window_s;
    push_axis(axis_stamp(start), axis_stamp(end), width, p, rows);
    let y = rows.len();
    let tall = 6usize;
    let plot_w = width.saturating_sub(2).max(1);
    for _ in 0..tall {
        rows.push(tc::seg(&[(p.dim.as_str(), " ".into())], width));
    }
    let scale = (hi - lo).max(1.0);
    let values: Vec<Option<f64>> = raw
        .iter()
        .map(|value| value.map(|n| ((n - lo) / scale).clamp(0.0, 1.0)))
        .collect();
    let slots = values.len().max(1);
    plots.push(
        tc::Plot::new(
            1,
            y,
            plot_w,
            tall,
            tc::LineChart {
                traces: vec![tc::Trace {
                    values,
                    positions: Some(positions),
                    colour: p.accent.clone(),
                    baseline: None,
                }],
                slots,
                focus: Some(0),
            },
        )
        .under_text()
        .on_axis(lo, hi),
    );
    push_wrapped(&path.display().to_string(), width, p, rows);
}

fn slope_text(slope: Option<state::Slope>, seconds: f64) -> String {
    match slope {
        Some(slope) => signed_rate(slope.per_s, seconds),
        None => "—".into(),
    }
}

fn push_axis(left: String, right: String, width: usize, p: &Palette, rows: &mut Vec<String>) {
    let left_w = left.chars().count();
    let right_w = right.chars().count();
    if left_w + right_w + 2 <= width {
        let gap = width.saturating_sub(left_w + right_w + 1);
        let text = format!(" {left}{}{right}", " ".repeat(gap));
        rows.push(tc::seg(&[(p.dim.as_str(), text)], width));
    } else {
        push_wrapped(&format!("{left}  {right}"), width, p, rows);
    }
}

fn push_wrapped(text: &str, width: usize, p: &Palette, rows: &mut Vec<String>) {
    for line in wrap(text, width.saturating_sub(1)) {
        rows.push(tc::seg(&[(p.dim.as_str(), format!(" {line}"))], width));
    }
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
                let cw = tc::display_width(ch.encode_utf8(&mut [0u8; 4]));
                if used + cw > width && !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                line.push(ch);
                used += cw;
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

fn board_from(snap: &parse::IoSnapshot, memory: &mut RateMemory) -> DiskBoard {
    let now = Instant::now();
    let elapsed = memory
        .at
        .map(|at| now.saturating_duration_since(at).as_secs_f64())
        .unwrap_or(0.0);
    let mut rows = Vec::with_capacity(snap.disks.len());
    let mut live = std::collections::BTreeSet::new();
    for disk in &snap.disks {
        live.insert(disk.name.clone());
        let (rate, read_point, write_point) = if snap.combined {
            match disk.mac {
                Some(sample) => {
                    let bytes = sample.mb_per_s * 1024.0 * 1024.0;
                    (ShownRate::Mac(sample), Some(bytes), None)
                }
                None => (ShownRate::Measuring, None, None),
            }
        } else if let Some(stat) = disk.stat.clone() {
            let rate = match memory.prev.get(&disk.name) {
                Some(prev) => match parse::disk_rates(prev, &stat, elapsed) {
                    Some(rates) => ShownRate::Linux(rates),
                    None => ShownRate::Gap,
                },
                None => ShownRate::Measuring,
            };
            let points = match &rate {
                ShownRate::Linux(rates) => (Some(rates.read_bps), Some(rates.write_bps)),
                _ => (None, None),
            };
            memory.prev.insert(disk.name.clone(), stat);
            (rate, points.0, points.1)
        } else {
            // A missing counter is not a previous sample. Keeping `prev`
            // while the shared timestamp moves would divide the whole gap
            // by the next poll alone.
            memory.prev.remove(&disk.name);
            (ShownRate::Measuring, None, None)
        };
        let series = memory.series.entry(disk.name.clone()).or_default();
        push_sample(&mut series.0, read_point);
        push_sample(&mut series.1, write_point);
        rows.push(DiskViewRow {
            disk: disk.clone(),
            rate,
            read: series.0.clone(),
            write: series.1.clone(),
        });
    }
    memory.series.retain(|name, _| live.contains(name));
    memory.prev.retain(|name, _| live.contains(name));
    memory.at = Some(now);
    DiskBoard {
        rows,
        show_pressure: snap.show_pressure,
        pressure: snap.pressure,
        pressure_absent: snap.pressure_absent,
        pressure_err: snap.pressure_err.clone(),
        host_devices: snap.host_devices,
        combined: snap.combined,
        missing: snap.missing.clone(),
    }
}

fn push_sample(series: &mut Vec<Option<f64>>, value: Option<f64>) {
    series.push(value);
    if series.len() > SERIES_CAP {
        let extra = series.len() - SERIES_CAP;
        series.drain(0..extra);
    }
}

fn ordered_disks(
    board: &DiskBoard,
    sort: DiskSort,
    show_idle: bool,
    hide: &[String],
) -> Vec<usize> {
    let lives: Vec<parse::LiveDisk> = board.rows.iter().map(|row| row.disk.clone()).collect();
    let (mut indexes, _) = parse::visible_disk_indexes(&lives, show_idle, hide);
    indexes.sort_by(|&left, &right| match sort {
        DiskSort::Name => board.rows[left].disk.name.cmp(&board.rows[right].disk.name),
        DiskSort::Read => rate_key(&board.rows[right], false)
            .total_cmp(&rate_key(&board.rows[left], false))
            .then(left.cmp(&right)),
        DiskSort::Write => rate_key(&board.rows[right], true)
            .total_cmp(&rate_key(&board.rows[left], true))
            .then(left.cmp(&right)),
    });
    indexes
}

fn rate_key(row: &DiskViewRow, write: bool) -> f64 {
    match &row.rate {
        ShownRate::Linux(rates) => {
            if write {
                rates.write_bps
            } else {
                rates.read_bps
            }
        }
        ShownRate::Mac(sample) => {
            if write {
                sample.mb_per_s
            } else {
                sample.tps
            }
        }
        _ => -1.0,
    }
}

fn paint_disks(
    view: &View,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    placed: &mut Vec<(usize, usize)>,
    plots: &mut Vec<tc::Plot>,
) -> usize {
    let combined = view.board.is_some_and(|board| board.combined);
    let cols = disk_columns(width);
    let heading = disks_heading(view, combined, cols.queue);
    for line in wrap(&heading, width.saturating_sub(1)) {
        rows.push(tc::seg(&[(p.lbl.as_str(), format!(" {line}"))], width));
    }
    if !view.disk_err.is_empty() {
        for line in wrap(view.disk_err, width.saturating_sub(1)) {
            rows.push(tc::seg(&[(p.accent.as_str(), format!(" {line}"))], width));
        }
    }
    let Some(board) = view.board else {
        rows.push(tc::seg(&[(p.dim.as_str(), " measuring".into())], width));
        return 0;
    };
    if !board.missing.is_empty() {
        for line in wrap(&board.missing, width.saturating_sub(1)) {
            rows.push(tc::seg(&[(p.txt.as_str(), format!(" {line}"))], width));
        }
        return 0;
    }
    if view.screen == Screen::DiskDetail {
        paint_disk_detail(view, board, width, p, rows, plots);
        return 0;
    }
    let lives: Vec<parse::LiveDisk> = board.rows.iter().map(|row| row.disk.clone()).collect();
    let (visible, hidden) = parse::visible_disk_indexes(&lives, view.show_idle, view.hide_devices);
    let labels = hidden.labels();
    if let Some(line) = tc::filter_row(visible.len(), lives.len(), &labels) {
        for line in wrap(&line, width.saturating_sub(1)) {
            rows.push(tc::seg(&[(p.dim.as_str(), format!(" {line}"))], width));
        }
    }
    let indexes = ordered_disks(board, view.disk_sort, view.show_idle, view.hide_devices);
    if indexes.is_empty() {
        if let Some(line) = tc::filtered_to_nothing(lives.len(), &labels) {
            for line in wrap(&line, width.saturating_sub(1)) {
                rows.push(tc::seg(&[(p.txt.as_str(), format!(" {line}"))], width));
            }
        } else if view.disk_err.is_empty() {
            rows.push(tc::seg(&[(p.dim.as_str(), " no disks".into())], width));
        }
        return 0;
    }
    rows.push(disk_header(board.combined, &cols, width, p));
    for (item, index) in indexes.iter().copied().enumerate() {
        let row = &board.rows[index];
        let here = view.focus == 1 && row.disk.name == view.disk_cursor;
        let from = rows.len();
        rows.extend(disk_data_rows(row, board.combined, &cols, width, here, p));
        placed.extend((from..rows.len()).map(|at| (at, DISK_BASE + item)));
    }
    if !board.combined {
        let note = "* BUSY = share of time with at least one request in flight (field 10); an NVMe reaches 100% with headroom";
        for line in wrap(note, width.saturating_sub(1)) {
            rows.push(tc::seg(&[(p.dim.as_str(), format!(" {line}"))], width));
        }
    }
    if let Some(row) = indexes
        .iter()
        .copied()
        .find(|&index| board.rows[index].disk.name == view.disk_cursor)
        .map(|index| &board.rows[index])
    {
        push_chart(row, 4, board.combined, width, p, rows, plots);
    }
    indexes.len()
}

fn disks_heading(view: &View, combined: bool, queue: bool) -> String {
    if combined {
        return format!(
            "DISKS  iostat: read and write combined · sorted: {} · AWAIT, BUSY and QUEUE need IOKit (OPS-193)",
            view.disk_sort.label(true)
        );
    }
    let mut line = format!(
        "DISKS  whole disks only · /proc/diskstats · 512-byte sectors · sorted: {}",
        view.disk_sort.label(false)
    );
    if view.board.is_some_and(|board| board.host_devices) {
        line.push_str(" · host devices, container filesystems");
    }
    if queue {
        line.push_str(" · QUEUE is instantaneous");
    }
    line
}

fn disk_header(combined: bool, cols: &DiskCols, width: usize, p: &Palette) -> String {
    if combined {
        let cells = vec![
            tc::pad("DISK", cols.disk),
            format!("{:>width$}", "TPS", width = TPS_W),
            format!("{:>width$}", "MB/s", width = MB_W),
        ];
        return tc::seg(&[(p.dim.as_str(), cells.join(" "))], width);
    }
    let mut cells = vec![tc::pad("DISK", cols.disk)];
    if cols.typ {
        cells.push(tc::pad("TYPE", DISK_TYPE_W));
    }
    if cols.size {
        cells.push(tc::pad("SIZE", DISK_SIZE_W));
    }
    cells.push(format!("{:>width$}", "READ", width = READ_W));
    cells.push(format!("{:>width$}", "WRITE", width = WRITE_W));
    if cols.iops {
        cells.push(format!("{:>width$}", "IOPS", width = IOPS_W));
    }
    if cols.await_ms {
        cells.push(format!("{:>width$}", "AWAIT", width = AWAIT_W));
    }
    cells.push(format!("{:>width$}", "BUSY*", width = BUSY_W));
    if cols.queue {
        cells.push(format!("{:>width$}", "QUEUE", width = QUEUE_W));
    }
    tc::seg(&[(p.dim.as_str(), cells.join(" "))], width)
}

fn disk_data_rows(
    row: &DiskViewRow,
    combined: bool,
    cols: &DiskCols,
    width: usize,
    here: bool,
    p: &Palette,
) -> Vec<String> {
    let tint = if here {
        tc::bg(38, 56, 76)
    } else {
        String::new()
    };
    let c = |colour: &str| {
        let colour = if tint.is_empty() {
            colour
        } else if colour == p.dim {
            p.dim_lit.as_str()
        } else {
            colour
        };
        format!("{}{}", tint, colour)
    };
    let room = cols.disk.saturating_sub(2).max(1);
    let chunks = chunks(&row.disk.name, room);
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
        let name = tc::pad(&format!("{marker}{}", tc::pad(chunk, room)), cols.disk);
        if index > 0 {
            out.push(tc::seg(&[(c(p.txt.as_str()).as_str(), name)], width));
            continue;
        }
        let mut cells = vec![(
            c(if here {
                p.accent.as_str()
            } else {
                p.txt.as_str()
            }),
            name,
        )];
        if combined {
            match &row.rate {
                ShownRate::Mac(sample) => {
                    cells.push((
                        c(p.txt.as_str()),
                        format!("{:>width$}", tps_text(sample.tps), width = TPS_W),
                    ));
                    cells.push((
                        c(p.txt.as_str()),
                        format!("{:>width$}", mb_text(sample.mb_per_s), width = MB_W),
                    ));
                }
                ShownRate::Gap => {
                    cells.push((c(p.dim.as_str()), format!("{:>width$}", "—", width = TPS_W)));
                    cells.push((c(p.dim.as_str()), format!("{:>width$}", "—", width = MB_W)));
                }
                ShownRate::Measuring | ShownRate::Linux(_) => {
                    cells.push((c(p.accent.as_str()), "measuring".into()));
                }
            }
        } else {
            match &row.rate {
                ShownRate::Linux(rates) => {
                    push_linux_rates(&mut cells, row, cols, rates, &c, p);
                }
                ShownRate::Gap => {
                    push_gap(&mut cells, row, cols, &c, p);
                }
                ShownRate::Measuring | ShownRate::Mac(_) => {
                    if cols.typ {
                        cells.push((
                            c(p.dim.as_str()),
                            tc::pad(&clip_chars(&row.disk.kind, DISK_TYPE_W), DISK_TYPE_W),
                        ));
                    }
                    if cols.size {
                        cells.push((
                            c(p.txt.as_str()),
                            format!("{:>width$}", human(row.disk.size), width = DISK_SIZE_W),
                        ));
                    }
                    cells.push((c(p.accent.as_str()), "measuring".into()));
                }
            }
        }
        out.push(join_cells(&cells, width, here, &tint));
    }
    out
}

fn push_linux_rates(
    cells: &mut Vec<(String, String)>,
    row: &DiskViewRow,
    cols: &DiskCols,
    rates: &parse::Rates,
    c: &impl Fn(&str) -> String,
    p: &Palette,
) {
    if cols.typ {
        cells.push((
            c(p.dim.as_str()),
            tc::pad(&clip_chars(&row.disk.kind, DISK_TYPE_W), DISK_TYPE_W),
        ));
    }
    if cols.size {
        cells.push((
            c(p.txt.as_str()),
            format!("{:>width$}", human(row.disk.size), width = DISK_SIZE_W),
        ));
    }
    cells.push((
        c(p.txt.as_str()),
        format!(
            "{:>width$}",
            human(rates.read_bps.round() as u64),
            width = READ_W
        ),
    ));
    cells.push((
        c(p.txt.as_str()),
        format!(
            "{:>width$}",
            human(rates.write_bps.round() as u64),
            width = WRITE_W
        ),
    ));
    if cols.iops {
        cells.push((
            c(p.txt.as_str()),
            format!("{:>width$}", iops_text(rates.iops), width = IOPS_W),
        ));
    }
    if cols.await_ms {
        cells.push((
            c(p.txt.as_str()),
            format!("{:>width$}", await_text(rates.await_ms), width = AWAIT_W),
        ));
    }
    cells.push((
        c(p.txt.as_str()),
        format!("{:>width$}", busy_text(rates.busy), width = BUSY_W),
    ));
    if cols.queue {
        cells.push((
            c(p.txt.as_str()),
            format!("{:>width$}", queue_text(rates.queue), width = QUEUE_W),
        ));
    }
}

fn push_gap(
    cells: &mut Vec<(String, String)>,
    row: &DiskViewRow,
    cols: &DiskCols,
    c: &impl Fn(&str) -> String,
    p: &Palette,
) {
    if cols.typ {
        cells.push((
            c(p.dim.as_str()),
            tc::pad(&clip_chars(&row.disk.kind, DISK_TYPE_W), DISK_TYPE_W),
        ));
    }
    if cols.size {
        cells.push((
            c(p.txt.as_str()),
            format!("{:>width$}", human(row.disk.size), width = DISK_SIZE_W),
        ));
    }
    let dash = |width: usize| format!("{:>width$}", "—", width = width);
    cells.push((c(p.dim.as_str()), dash(READ_W)));
    cells.push((c(p.dim.as_str()), dash(WRITE_W)));
    if cols.iops {
        cells.push((c(p.dim.as_str()), dash(IOPS_W)));
    }
    if cols.await_ms {
        cells.push((c(p.dim.as_str()), dash(AWAIT_W)));
    }
    cells.push((c(p.dim.as_str()), dash(BUSY_W)));
    if cols.queue {
        let queue = row
            .disk
            .stat
            .as_ref()
            .map(|stat| stat.in_flight)
            .unwrap_or(0);
        cells.push((
            c(p.txt.as_str()),
            format!("{:>width$}", queue_text(queue), width = QUEUE_W),
        ));
    }
}

fn join_cells(cells: &[(String, String)], width: usize, here: bool, tint: &str) -> String {
    let mut parts: Vec<(&str, String)> = cells
        .iter()
        .enumerate()
        .flat_map(|(index, (colour, text))| {
            let mut piece = Vec::new();
            if index > 0 {
                piece.push((colour.as_str(), " ".into()));
            }
            piece.push((colour.as_str(), text.clone()));
            piece
        })
        .collect();
    if here {
        let used: usize = parts.iter().map(|(_, text)| tc::display_width(text)).sum();
        if used < width {
            parts.push((tint, " ".repeat(width - used)));
        }
    }
    tc::seg(&parts, width)
}

fn paint_disk_detail(
    view: &View,
    board: &DiskBoard,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    plots: &mut Vec<tc::Plot>,
) {
    let Some(row) = board
        .rows
        .iter()
        .find(|row| row.disk.name == view.disk_cursor)
    else {
        rows.push(tc::seg(
            &[(p.dim.as_str(), " nothing selected".into())],
            width,
        ));
        return;
    };
    let disk = &row.disk;
    let mut lines = vec![
        format!("disk {}", disk.name),
        if disk.model.is_empty() {
            if board.combined {
                "model not provided by iostat".into()
            } else {
                "model unavailable".into()
            }
        } else {
            format!("model {}", disk.model)
        },
        if disk.kind.is_empty() {
            if board.combined {
                "type not provided by iostat".into()
            } else {
                "type unavailable".into()
            }
        } else {
            format!("type {}", disk.kind)
        },
        if board.combined {
            "size not provided by iostat".into()
        } else {
            format!("size {}", human(disk.size))
        },
    ];
    if !disk.mounts.is_empty() {
        lines.push(format!("mounted {}", disk.mounts.join(", ")));
    }
    if disk.partitions.is_empty() && !board.combined {
        lines.push("no partitions".into());
    }
    for part in &disk.partitions {
        let mounts = if part.mounts.is_empty() {
            "unmounted".into()
        } else {
            part.mounts.join(", ")
        };
        lines.push(format!("partition {} {}", part.name, mounts));
    }
    if !disk.slaves.is_empty() {
        lines.push(format!("slaves {}", disk.slaves.join(", ")));
    }
    if let Some(stat) = &disk.stat {
        lines.push(format!(
            "since boot read {}  written {}  ios {}",
            human(stat.sectors_read.saturating_mul(parse::SECTOR_BYTES)),
            human(stat.sectors_written.saturating_mul(parse::SECTOR_BYTES)),
            stat.reads.saturating_add(stat.writes)
        ));
    } else if board.combined {
        lines.push("totals since boot are not in iostat".into());
    }
    match &row.rate {
        ShownRate::Linux(rates) => lines.push(format!(
            "read {}/s  write {}/s  iops {}  await {}  busy {}  queue {}",
            human(rates.read_bps.round() as u64),
            human(rates.write_bps.round() as u64),
            iops_text(rates.iops),
            await_text(rates.await_ms),
            busy_text(rates.busy),
            queue_text(rates.queue)
        )),
        ShownRate::Mac(sample) => lines.push(format!(
            "tps {}  MB/s {} combined",
            tps_text(sample.tps),
            mb_text(sample.mb_per_s)
        )),
        ShownRate::Gap => lines.push("counter reset —".into()),
        ShownRate::Measuring => lines.push("measuring".into()),
    }
    for line in lines {
        for wrapped in wrap(&line, width.saturating_sub(1)) {
            rows.push(tc::seg(&[(p.txt.as_str(), format!(" {wrapped}"))], width));
        }
    }
    push_chart(row, 8, board.combined, width, p, rows, plots);
}

fn paint_pressure(
    board: &DiskBoard,
    focus: usize,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    placed: &mut Vec<(usize, usize)>,
) {
    let heading = "/proc/pressure/io · stall time, not busy time";
    for line in wrap(heading, width.saturating_sub(1)) {
        rows.push(tc::seg(&[(p.lbl.as_str(), format!(" {line}"))], width));
    }
    let text = if !board.pressure_err.is_empty() {
        board.pressure_err.clone()
    } else if board.pressure_absent || board.pressure.is_none() {
        "PSI not available on this kernel (CONFIG_PSI or psi=0)".into()
    } else {
        let pressure = board.pressure.unwrap();
        format!(
            "some {:.1}% avg10 · {:.1}% avg60 · {:.1}% avg300   full {:.1}% · {:.1}% · {:.1}%",
            pressure.some[0],
            pressure.some[1],
            pressure.some[2],
            pressure.full[0],
            pressure.full[1],
            pressure.full[2]
        )
    };
    let tint = if focus == 2 {
        tc::bg(38, 56, 76)
    } else {
        String::new()
    };
    let colour = if focus == 2 { &p.accent } else { &p.txt };
    for (index, line) in wrap(&text, width.saturating_sub(1)).into_iter().enumerate() {
        let ink = format!("{tint}{colour}");
        rows.push(tc::seg(&[(ink.as_str(), format!(" {line}"))], width));
        if index == 0 {
            placed.push((rows.len() - 1, PRESSURE_ID));
        }
    }
}

fn push_chart(
    row: &DiskViewRow,
    tall: usize,
    combined: bool,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    plots: &mut Vec<tc::Plot>,
) {
    let peak = peak_of(&row.read, &row.write).max(1.0);
    let caption = chart_caption(&row.read, &row.write, combined);
    for line in wrap(&caption, width.saturating_sub(1)) {
        rows.push(tc::seg(&[(p.dim.as_str(), format!(" {line}"))], width));
    }
    let y = rows.len();
    let plot_w = width.saturating_sub(2).max(1);
    for _ in 0..tall {
        rows.push(tc::seg(&[(p.dim.as_str(), " ".into())], width));
    }
    let mut traces = vec![tc::Trace {
        values: row
            .read
            .iter()
            .map(|value| chart_frac(*value, peak, false))
            .collect(),
        positions: None,
        colour: p.accent.clone(),
        baseline: Some(0.0),
    }];
    if !combined {
        traces.push(tc::Trace {
            values: row
                .write
                .iter()
                .map(|value| chart_frac(*value, peak, true))
                .collect(),
            positions: None,
            colour: p.down.clone(),
            baseline: Some(1.0),
        });
    }
    plots.push(
        tc::Plot::new(
            1,
            y,
            plot_w,
            tall,
            tc::LineChart {
                traces,
                slots: SERIES_CAP,
                focus: Some(0),
            },
        )
        .under_text(),
    );
}

/// The latest sample is `now`. A gap there stays a dash; an older rate is
/// not borrowed, and an empty series is not drawn as zero with a 1 B/s peak.
fn chart_caption(read: &[Option<f64>], write: &[Option<f64>], combined: bool) -> String {
    let any = read.iter().chain(write.iter()).any(|value| value.is_some());
    if !any {
        return "no rate yet".into();
    }
    let peak = human(peak_of(read, write).round() as u64);
    if combined {
        format!("peak {peak}/s   now {} combined", rate_now(read))
    } else {
        format!(
            "peak {peak}/s   now {} read  {} write",
            rate_now(read),
            rate_now(write)
        )
    }
}

fn rate_now(series: &[Option<f64>]) -> String {
    match series.last() {
        Some(Some(value)) if value.is_finite() => format!("{}/s", human(value.round() as u64)),
        _ => "—".into(),
    }
}

fn chart_frac(value: Option<f64>, peak: f64, invert: bool) -> Option<f64> {
    let value = value?;
    if !value.is_finite() || peak <= 0.0 {
        return None;
    }
    let fraction = (value / peak).clamp(0.0, 1.0);
    Some(if invert { 1.0 - fraction } else { fraction })
}

fn peak_of(read: &[Option<f64>], write: &[Option<f64>]) -> f64 {
    read.iter()
        .chain(write.iter())
        .filter_map(|value| *value)
        .filter(|value| value.is_finite())
        .fold(0.0, f64::max)
}

fn iops_text(iops: f64) -> String {
    if iops >= 10_000.0 {
        format!("{:.0}K", iops / 1000.0)
    } else if iops >= 100.0 {
        format!("{:.0}", iops)
    } else {
        format!("{:.1}", iops)
    }
}

fn await_text(ms: Option<f64>) -> String {
    match ms {
        None => "—".into(),
        Some(ms) if ms >= 10_000.0 => format!("{:.0}s", ms / 1000.0),
        Some(ms) if ms >= 1000.0 => format!("{:.1}s", ms / 1000.0),
        Some(ms) => format!("{:.0}ms", ms),
    }
}

fn busy_text(share: f64) -> String {
    format!("{:.0}%", (share * 100.0).round())
}

fn queue_text(queue: u64) -> String {
    if queue >= 100_000 {
        format!("{}K", queue / 1000)
    } else {
        queue.to_string()
    }
}

fn tps_text(tps: f64) -> String {
    if tps >= 100.0 {
        format!("{:.0}", tps)
    } else {
        format!("{:.1}", tps)
    }
}

fn mb_text(mb: f64) -> String {
    if mb >= 100.0 {
        format!("{:.0}", mb)
    } else if mb >= 10.0 {
        format!("{:.1}", mb)
    } else {
        format!("{:.2}", mb)
    }
}

fn footer(
    w: usize,
    screen: Screen,
    sort: Sort,
    disk_sort: DiskSort,
    focus: usize,
    show_idle: bool,
    paused: bool,
    window_days: u64,
    combined: bool,
    p: &Palette,
) -> (tc::Footer, usize) {
    let mut hints: Vec<Vec<(&str, String)>> = Vec::new();
    if screen != Screen::List {
        hints.push(vec![(p.dim.as_str(), "esc back".into())]);
    } else {
        hints.push(vec![(p.dim.as_str(), "↑↓ select".into())]);
        hints.push(vec![(p.dim.as_str(), "[↵] detail".into())]);
        if focus != 2 {
            let hint = if focus == 1 {
                disk_sort.hint(combined)
            } else {
                sort.hint()
            };
            hints.push(vec![(p.dim.as_str(), hint.into())]);
        }
        hints.push(vec![(p.dim.as_str(), "tab section".into())]);
        hints.push(vec![(p.dim.as_str(), idle_hint(show_idle).into())]);
    }
    let next = tc::cycle(&WINDOWS, window_days);
    hints.push(vec![(p.dim.as_str(), format!("[w] {next}d"))]);
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
    fn a_wide_word_on_the_detail_screen_wraps_by_cells() {
        let word: String = "\u{6570}".repeat(40);
        let lines = wrap(&word, 28);
        assert!(lines.len() > 1, "a 80-cell word fits a 28-cell line");
        assert_eq!(lines.concat(), word, "nothing lost");
        for line in &lines {
            assert!(
                tc::display_width(line) <= 28,
                "{line:?} is {} cells",
                tc::display_width(line)
            );
        }
    }

    #[test]
    fn a_wrapped_status_line_keeps_its_last_character() {
        let rows = vec![row("/", "ext4", Figures::Ready(space(1, 1, 10, 9)), 0)];
        let snap = snap(rows, Hidden::default());
        let mut shown = view(Some(&snap), "/");
        shown.paused = true;
        shown.age = Some(Duration::from_secs(12));
        let p = palette();
        let drawn = draw_body(&shown, 20, &p, None);
        for line in &drawn.rows {
            assert!(
                tc::display_width(line) <= 20,
                "{} cells: {line:?}",
                tc::display_width(line)
            );
        }
        let plain = drawn
            .rows
            .iter()
            .map(|line| plain(line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            plain.contains("12s"),
            "the prefixed wrap cut the status line:\n{plain}"
        );
        assert!(plain.contains("every"), "{plain}");
    }

    #[test]
    fn a_refresh_that_is_not_a_number_of_seconds_falls_back() {
        let at =
            |value: serde_json::Value| refresh_seconds(&serde_json::json!({ "refresh": value }));
        assert_eq!(
            at(serde_json::json!(1e300)),
            86_400.0,
            "capped, so Duration cannot panic"
        );
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
            blocks: used + usable + 10,
        }
    }

    fn row(point: &str, fstype: &str, figures: Figures, order: usize) -> Row {
        Row {
            mount: Mount {
                dev: "9:0".into(),
                root: "/".into(),
                mount_point: point.into(),
                path: point.as_bytes().to_vec(),
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
            board: None,
            disk_err: "",
            disk_sort: DiskSort::Write,
            focus: 0,
            show_idle: false,
            disk_cursor: "",
            hide_devices: &[],
            usage: empty_usage(),
            window_days: 7,
            now: 1_700_000_000.0,
            history_note: "",
            save_err: "",
        }
    }

    fn empty_usage() -> &'static state::Store {
        static STORE: std::sync::OnceLock<state::Store> = std::sync::OnceLock::new();
        STORE.get_or_init(state::Store::default)
    }

    #[test]
    fn columns_drop_hour_then_inodes_then_type_then_size() {
        let at = |width| {
            let cols = columns_for(width);
            (cols.hour, cols.inodes, cols.typ, cols.size)
        };
        assert!(!at(28).0, "the hour rate stays at 28 columns");
        assert_eq!(at(160), (true, true, true, true));
        let mut previous = (true, true, true, true);
        for width in (28..=160).rev() {
            let now = at(width);
            assert!(previous.0 || !now.0, "hour returned at {width}");
            assert!(previous.1 || !now.1, "inodes returned at {width}");
            assert!(previous.2 || !now.2, "type returned at {width}");
            assert!(previous.3 || !now.3, "size returned at {width}");
            if !now.3 {
                assert!(!now.2 && !now.1 && !now.0);
            }
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
            assert!(flat.contains("FULL IN"), "{width}");
            assert!(flat.contains("7d"), "{width}");
            let (packed, _) = footer(
                width,
                Screen::List,
                Sort::Mount,
                DiskSort::Write,
                0,
                false,
                false,
                7,
                false,
                &p,
            );
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
        assert_eq!(Sort::Mount.hint(), "[s] sort by fills soonest");
        assert_eq!(Sort::Mount.next().hint(), "[s] sort by % used");
        assert_eq!(Sort::Fills.hint(), "[s] sort by % used");
        assert_eq!(Sort::Inodes.hint(), "[s] sort by mount");
    }

    #[test]
    fn percent_sort_puts_the_fullest_first_and_a_missing_row_last() {
        let rows = vec![
            row("/low", "ext4", Figures::Ready(space(10, 90, 10, 9)), 0),
            row("/high", "ext4", Figures::Ready(space(90, 10, 10, 1)), 1),
            row("/wait", "ext4", Figures::NoAnswer, 2),
        ];
        let snap = snap(rows, Hidden::default());
        let order = ordered(&snap, Sort::Percent, empty_usage(), 7, 0.0);
        let names: Vec<&str> = order
            .iter()
            .map(|&index| snap.rows[index].mount.mount_point.as_str())
            .collect();
        assert_eq!(names, vec!["/high", "/low", "/wait"]);
    }

    #[test]
    fn an_unanswered_row_sorts_after_one_that_is_not_filling() {
        let now = 1_700_000_000.0;
        let quiet = space(1_000_000, 500_000_000, 10, 9);
        let mut series = state::Series::default();
        for minute in 0..20 {
            state::record(&mut series, now - (19 - minute) as f64 * 60.0, 1_000_000);
        }
        let mut store = state::Store::default();
        store
            .series
            .insert(state::key("/steady", "ext4", quiet.blocks), series);
        let snap = snap(
            vec![
                row("/wait", "ext4", Figures::NoAnswer, 0),
                row("/steady", "ext4", Figures::Ready(quiet), 1),
            ],
            Hidden::default(),
        );
        let order = ordered(&snap, Sort::Fills, &store, 7, now);
        let names: Vec<&str> = order
            .iter()
            .map(|&index| snap.rows[index].mount.mount_point.as_str())
            .collect();
        assert_eq!(names, vec!["/steady", "/wait"]);
    }

    #[test]
    fn a_rising_series_names_its_rate_and_the_soonest_fill_sorts_first() {
        let now = 1_700_000_000.0;
        let fast = space(1_000_000, 50_000_000, 10, 9);
        let slow = space(1_000_000, 500_000_000, 10, 9);
        let mut store = state::Store::default();
        for (point, blocks, step) in [("/", fast.blocks, 10_000u64), ("/slow", slow.blocks, 1_000)]
        {
            let mut series = state::Series::default();
            for minute in 0..20 {
                state::record(
                    &mut series,
                    now - (19 - minute) as f64 * 60.0,
                    1_000_000 + minute * step,
                );
            }
            store
                .series
                .insert(state::key(point, "ext4", blocks), series);
        }
        let rows = vec![
            row("/slow", "ext4", Figures::Ready(slow), 0),
            row("/", "ext4", Figures::Ready(fast), 1),
            row("/wait", "ext4", Figures::NoAnswer, 2),
        ];
        let snap = snap(rows, Hidden::default());
        let order = ordered(&snap, Sort::Fills, &store, 7, now);
        let names: Vec<&str> = order
            .iter()
            .map(|&index| snap.rows[index].mount.mount_point.as_str())
            .collect();
        assert_eq!(names, vec!["/", "/slow", "/wait"]);
        let mut frame = view(Some(&snap), "/");
        frame.usage = &store;
        frame.now = now;
        let p = palette();
        let drawn = draw_body(&frame, 120, &p, None);
        let plain_flat = drawn
            .rows
            .iter()
            .map(|line| plain(line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plain_flat.contains("FULL IN"), "{plain_flat}");
        assert!(plain_flat.contains('+'), "{plain_flat}");
        assert!(plain_flat.contains("of 7d"), "{plain_flat}");
        frame.screen = Screen::Detail;
        let drawn = draw_body(&frame, 80, &p, None);
        let detail = drawn
            .rows
            .iter()
            .map(|line| plain(line))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(drawn.plots.len(), 1, "{detail}");
        assert!(detail.contains("samples over"), "{detail}");
        assert!(detail.contains("disk/usage.json"), "{detail}");
    }

    #[test]
    fn the_footnote_names_the_fitted_start_and_a_full_window_keeps_its_own() {
        let now = 1_700_000_000.0;
        let day = 86_400.0;
        let kept = now - 10.0 * day;
        let fit_at = now - 7.0 * day + 15.0 * 60.0;
        let space = space(1_000, 1_000, 10, 9);
        let mut series = state::Series::default();
        state::record(&mut series, kept, 1);
        state::record(&mut series, fit_at, 2);
        state::record(&mut series, now, 3);
        let mut store = state::Store::default();
        store
            .series
            .insert(state::key("/", "ext4", space.blocks), series);
        let snap = snap(
            vec![row("/", "ext4", Figures::Ready(space), 0)],
            Hidden::default(),
        );
        let p = palette();
        let flat = {
            let mut frame = view(Some(&snap), "/");
            frame.usage = &store;
            frame.now = now;
            draw_body(&frame, 140, &p, None)
                .rows
                .iter()
                .map(|line| plain(line))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(
            flat.contains(&format!("rate since {}", stamp(fit_at))),
            "{flat}"
        );
        assert!(!flat.contains(&stamp(kept)), "{flat}");

        let mut full = state::Series::default();
        state::record(&mut full, kept, 1);
        state::record(&mut full, now - 7.0 * day, 2);
        state::record(&mut full, now, 3);
        store
            .series
            .insert(state::key("/", "ext4", space.blocks), full);
        let flat = {
            let mut frame = view(Some(&snap), "/");
            frame.usage = &store;
            frame.now = now;
            draw_body(&frame, 140, &p, None)
                .rows
                .iter()
                .map(|line| plain(line))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(
            flat.contains(&format!("kept since {}", stamp(kept))),
            "{flat}"
        );
        assert!(
            !flat.contains(&format!("rate since {}", stamp(now - 7.0 * day))),
            "{flat}"
        );
    }

    #[test]
    fn the_filesystem_trace_holds_still_through_the_open_minute() {
        let now = 1_700_000_040.0;
        let space = space(1_000, 1_000, 10, 9);
        let mut series = state::Series::default();
        state::record(&mut series, now - 120.0, 10);
        state::record(&mut series, now - 60.0, 20);
        state::record(&mut series, now, 30);
        let mut store = state::Store::default();
        store
            .series
            .insert(state::key("/", "ext4", space.blocks), series);
        let snap = snap(
            vec![row("/", "ext4", Figures::Ready(space), 0)],
            Hidden::default(),
        );
        let p = palette();
        let draw = |store: &state::Store| {
            let mut frame = view(Some(&snap), "/");
            frame.screen = Screen::Detail;
            frame.usage = store;
            frame.now = now;
            draw_body(&frame, 80, &p, None)
        };
        let first = draw(&store);
        state::record(store.series.values_mut().next().unwrap(), now + 10.0, 9_000);
        let second = draw(&store);
        assert_eq!(first.plots.len(), 1, "closed minutes should still draw");
        assert_eq!(
            first.plots[0].chart.traces[0].values,
            second.plots[0].chart.traces[0].values
        );
        assert_eq!(
            first.plots[0].chart.traces[0].positions,
            second.plots[0].chart.traces[0].positions
        );
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

    #[test]
    fn a_history_note_is_a_row() {
        let p = palette();
        let mut pending = view(None, "");
        pending.history_note = "saved history would not read (expected value) · started fresh · the old file is usage.json.bad";
        pending.save_err = "could not save history: permission denied";
        let drawn = draw_body(&pending, 40, &p, None);
        let flat = drawn.rows.join("\n");
        assert!(flat.contains("usage.json.bad"), "{flat}");
        assert!(flat.contains("could not save history:"), "{flat}");
        for line in &drawn.rows {
            assert!(tc::display_width(line) <= 40, "{line:?}");
        }
    }

    fn disk_row(name: &str, reads: u64, writes: u64, write_bps: f64) -> DiskViewRow {
        let stat = parse::DiskStat {
            major: 8,
            minor: 0,
            name: name.into(),
            reads,
            reads_merged: 0,
            sectors_read: reads,
            ms_reading: reads,
            writes,
            writes_merged: 0,
            sectors_written: writes,
            ms_writing: writes,
            in_flight: 1,
            ms_io: 10,
            weighted_ms_io: 10,
        };
        DiskViewRow {
            disk: parse::LiveDisk {
                name: name.into(),
                kind: "ssd".into(),
                size: 1 << 40,
                model: "Example SSD 1TB".into(),
                dev: "8:0".into(),
                stat: Some(stat),
                mac: None,
                idle: reads == 0 && writes == 0,
                partitions: vec![parse::PartMount {
                    name: format!("{name}1"),
                    dev: "8:1".into(),
                    mounts: vec!["/".into()],
                }],
                mounts: vec![],
                slaves: vec![],
            },
            rate: ShownRate::Linux(parse::Rates {
                read_bps: 512.0,
                write_bps,
                iops: 1.0,
                await_ms: Some(2.0),
                busy: 0.1,
                queue: 1,
            }),
            read: vec![Some(512.0); 3],
            write: vec![Some(write_bps); 3],
        }
    }

    fn board(rows: Vec<DiskViewRow>) -> DiskBoard {
        DiskBoard {
            rows,
            show_pressure: true,
            pressure: Some(parse::Pressure {
                some: [23.1, 18.4, 9.7],
                full: [7.2, 5.1, 2.0],
                some_total: 1,
                full_total: 1,
            }),
            pressure_absent: false,
            pressure_err: String::new(),
            host_devices: false,
            combined: false,
            missing: String::new(),
        }
    }

    #[test]
    fn disk_columns_drop_queue_then_await_then_type_then_size_then_iops() {
        let flags = |width| {
            let cols = disk_columns(width);
            (cols.queue, cols.await_ms, cols.typ, cols.size, cols.iops)
        };
        assert_eq!(flags(140), (true, true, true, true, true));
        assert_eq!(flags(80), (true, true, true, true, true));
        let narrow = flags(28);
        assert!(!narrow.0 && !narrow.1 && !narrow.2 && !narrow.3 && !narrow.4);
        let mut dropped = [None; 5];
        let mut previous = (true, true, true, true, true);
        for width in (28..=140).rev() {
            let now = flags(width);
            let present = [now.0, now.1, now.2, now.3, now.4];
            let before = [previous.0, previous.1, previous.2, previous.3, previous.4];
            for (index, on) in present.iter().copied().enumerate() {
                assert!(before[index] || !on, "a dropped column returned at {width}");
                if !on && dropped[index].is_none() {
                    dropped[index] = Some(width);
                }
            }
            previous = now;
        }
        // Queue leaves first, then await, type, size, iops. A higher width
        // is a column that gave up while the pane was still wider.
        for pair in dropped.windows(2) {
            assert!(pair[0] > pair[1], "drop order {dropped:?}");
        }
    }

    #[test]
    fn a_disk_and_two_partitions_render_one_row() {
        let stats = parse::parse_diskstats(
            "8 0 sda 10 0 20 1 4 0 8 1 0 1 1\n8 1 sda1 1 0 2 1 1 0 2 1 0 1 1\n8 2 sda2 1 0 2 1 1 0 2 1 0 1 1\n",
        );
        let inventory = vec![parse::InventoryDisk {
            name: "sda".into(),
            kind: "ssd".into(),
            size: 1024,
            model: "Example SSD 1TB".into(),
            dev: "8:0".into(),
            partitions: vec![
                parse::PartMount {
                    name: "sda1".into(),
                    dev: "8:1".into(),
                    mounts: vec!["/".into()],
                },
                parse::PartMount {
                    name: "sda2".into(),
                    dev: "8:2".into(),
                    mounts: vec![],
                },
            ],
            slaves: vec![],
        }];
        let live = parse::live_disks(&stats, &inventory);
        assert_eq!(live.len(), 1);
        let mut row = disk_row("sda", 10, 4, 4096.0);
        row.disk = live.into_iter().next().unwrap();
        row.disk.model = "Example SSD 1TB".into();
        let board = board(vec![row]);
        let p = palette();
        let mut frame = view(None, "");
        frame.board = Some(&board);
        frame.disk_cursor = "sda";
        frame.focus = 1;
        for width in [28, 40, 60, 80, 100, 140] {
            let drawn = draw_body(&frame, width, &p, None);
            for line in &drawn.rows {
                assert!(
                    tc::display_width(line) <= width,
                    "width {width} drew {} cells",
                    tc::display_width(line)
                );
            }
            let plain = drawn
                .rows
                .iter()
                .map(|line| plain(line))
                .collect::<Vec<_>>()
                .join("\n");
            for phrase in ["whole disks only", "/proc/diskstats", "512-byte", "sectors"] {
                assert!(plain.contains(phrase), "{width} missing {phrase}");
            }
            assert!(!plain.contains("sda1"), "partition rendered at {width}");
            assert!(!plain.contains("sda2"), "partition rendered at {width}");
            assert_eq!(
                drawn
                    .placed
                    .iter()
                    .filter(|(_, item)| *item >= DISK_BASE && *item < PRESSURE_ID)
                    .map(|(_, item)| *item)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len(),
                1,
                "width {width}"
            );
        }
        assert!(draw_body(&frame, 80, &p, None).rows.len() > 10);
        let drawn = draw_body(&frame, 100, &p, None);
        assert_eq!(drawn.plots.len(), 1);
        assert_eq!(drawn.plots[0].height, 4);
        assert_eq!(drawn.plots[0].chart.traces.len(), 2);
        assert_eq!(drawn.plots[0].chart.traces[0].baseline, Some(0.0));
        assert_eq!(drawn.plots[0].chart.traces[1].baseline, Some(1.0));
        assert!(drawn.plots[0]
            .clone()
            .in_viewport(0, drawn.head, 40)
            .is_some());
        assert!(plain_has(&drawn, "23.1%"));
        assert!(plain_has(&drawn, "stall time, not busy time"));
    }

    fn plain_has(drawn: &Drawn, text: &str) -> bool {
        drawn.rows.iter().any(|line| plain(line).contains(text))
    }

    #[test]
    fn idle_and_hide_devices_are_named_in_the_filter_row() {
        let rows = vec![disk_row("loop0", 0, 0, 0.0), disk_row("sda", 4, 9, 9000.0)];
        let board = board(rows);
        let p = palette();
        let mut frame = view(None, "");
        frame.board = Some(&board);
        frame.disk_cursor = "sda";
        frame.show_idle = false;
        let drawn = draw_body(&frame, 100, &p, None);
        assert!(plain_has(&drawn, "1 idle hidden"));
        assert_eq!(idle_hint(false), "[v] show idle disks");
        assert_eq!(idle_hint(true), "[v] hide idle disks");
        frame.show_idle = true;
        let hidden = vec!["sda".to_string()];
        frame.hide_devices = &hidden;
        let drawn = draw_body(&frame, 100, &p, None);
        assert!(plain_has(&drawn, "1 hide_devices hidden"));
        assert!(plain_has(&drawn, "loop0"));
    }

    #[test]
    fn a_missing_psi_file_is_named_and_a_reset_is_a_dash() {
        let mut rows = board(vec![disk_row("sda", 1, 1, 100.0)]);
        rows.pressure = None;
        rows.pressure_absent = true;
        let p = palette();
        {
            let mut frame = view(None, "");
            frame.board = Some(&rows);
            let drawn = draw_body(&frame, 80, &p, None);
            assert!(plain_has(
                &drawn,
                "PSI not available on this kernel (CONFIG_PSI or psi=0)"
            ));
        }
        rows.rows[0].rate = ShownRate::Gap;
        rows.pressure_absent = false;
        rows.show_pressure = false;
        let mut frame = view(None, "");
        frame.board = Some(&rows);
        frame.disk_err = "diskstats unavailable: gone";
        let drawn = draw_body(&frame, 120, &p, None);
        assert!(plain_has(&drawn, "—"));
        assert!(plain_has(&drawn, "diskstats unavailable:"));
    }

    #[test]
    fn the_chart_series_stays_at_sixty_samples() {
        let mut series = Vec::new();
        for index in 0..80 {
            push_sample(&mut series, Some(index as f64));
        }
        assert_eq!(series.len(), 60);
        assert_eq!(series[0], Some(20.0));
        assert_eq!(series[59], Some(79.0));
    }

    #[test]
    fn sort_hints_name_the_next_disk_order() {
        assert_eq!(DiskSort::Write.hint(false), "[s] sort by read");
        assert_eq!(DiskSort::Read.hint(false), "[s] sort by name");
        assert_eq!(DiskSort::Name.hint(false), "[s] sort by write");
        assert_eq!(DiskSort::Write.hint(true), "[s] sort by tps");
        assert_eq!(DiskSort::Read.hint(true), "[s] sort by name");
        assert_eq!(DiskSort::Name.hint(true), "[s] sort by MB/s");
        assert_eq!(DiskSort::Write.label(true), "MB/s");
        assert_eq!(DiskSort::Read.label(true), "tps");
        assert_eq!(DiskSort::Write.label(false), "write");
        assert_eq!(DiskSort::Write.next(), DiskSort::Read);
    }

    #[test]
    fn a_gap_at_the_end_of_the_chart_is_not_labelled_as_now() {
        let caption = chart_caption(&[Some(100.0), None], &[Some(50.0), None], false);
        assert!(caption.contains("peak 100B/s"), "{caption}");
        assert!(caption.contains("now — read"), "{caption}");
        assert!(caption.contains("— write"), "{caption}");
        assert!(!caption.contains("50B"), "{caption}");
        assert_eq!(chart_caption(&[], &[], false), "no rate yet");
        assert_eq!(chart_caption(&[None, None], &[None], true), "no rate yet");
        let zero = chart_caption(&[Some(0.0)], &[], true);
        assert!(zero.contains("now 0B/s"), "{zero}");
        assert!(!zero.contains("1B"), "{zero}");
    }

    #[test]
    fn a_missing_counter_does_not_become_a_peak_when_it_returns() {
        let mut memory = RateMemory::default();
        let mut disk = disk_row("sda", 0, 0, 0.0).disk;
        let first = parse::IoSnapshot {
            disks: vec![disk.clone()],
            ..parse::IoSnapshot::default()
        };
        let _ = board_from(&first, &mut memory);
        assert!(memory.prev.contains_key("sda"));
        disk.stat = None;
        let hole = parse::IoSnapshot {
            disks: vec![disk.clone()],
            ..parse::IoSnapshot::default()
        };
        let _ = board_from(&hole, &mut memory);
        assert!(!memory.prev.contains_key("sda"));
        memory.at = Some(Instant::now() - Duration::from_secs(1));
        let mut jumped = disk_row("sda", 0, 10_000_000, 0.0).disk;
        if let Some(stat) = jumped.stat.as_mut() {
            stat.sectors_written = 10_000_000;
        }
        let back = parse::IoSnapshot {
            disks: vec![jumped],
            ..parse::IoSnapshot::default()
        };
        let board = board_from(&back, &mut memory);
        assert!(matches!(board.rows[0].rate, ShownRate::Measuring));
    }

    #[test]
    fn a_whole_disk_mount_is_named_in_the_detail() {
        let mut row = disk_row("sdb", 1, 1, 1.0);
        row.disk.mounts = vec!["/data".into()];
        row.disk.partitions.clear();
        let rows = board(vec![row]);
        let p = palette();
        let mut frame = view(None, "");
        frame.screen = Screen::DiskDetail;
        frame.board = Some(&rows);
        frame.disk_cursor = "sdb";
        frame.focus = 1;
        let drawn = draw_body(&frame, 80, &p, None);
        let plain = drawn
            .rows
            .iter()
            .map(|line| plain(line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plain.contains("mounted /data"), "{plain}");
        assert!(plain.contains("no partitions"), "{plain}");
    }
}
