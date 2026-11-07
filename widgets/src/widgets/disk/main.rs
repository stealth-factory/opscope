// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Which filesystems can still take a write, which disks are doing I/O, and, on Linux, whether I/O is stalling.
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

mod by_process;
mod parse;
mod state;
mod writers;
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

    #[derive(Default)]
    pub struct WriterSource;

    pub fn sample_writers(
        _source: &mut WriterSource,
        _wanted: super::writers::Wanted,
    ) -> Option<super::writers::Writers> {
        None
    }

    pub fn process_facts(_pid: i32) -> (String, String) {
        (String::new(), String::new())
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
const STACK_INDENT: usize = 3;
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
/// How far the bar grows into width nothing else wants. Past this the
/// spare columns stay after the last figure, at the pane's right edge,
/// rather than opening a gap between a mount point and its figures.
const BAR_WIDE: usize = 30;
/// Hours of samples the window rate, FULL IN and the trace fit. The
/// longest is a day: a projection from last week says little about today.
const WINDOWS: [u64; 3] = [6, 12, 24];
const DEFAULT_WINDOW: u64 = 24;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Columns {
    mount: usize,
    /// The mount point is a line of its own and the figures sit under it,
    /// indented by `mount`: the pane cannot hold the longest mount point
    /// beside the figures without breaking it into pieces.
    stacked: bool,
    typ: bool,
    size: bool,
    inodes: bool,
    hour: bool,
    bar: usize,
}

/// Which of 1h Δ, inodes, type and size a row carries, widest first.
const COLUMN_SETS: [(bool, bool, bool, bool); 5] = [
    (true, true, true, true),
    (false, true, true, true),
    (false, false, true, true),
    (false, false, false, true),
    (false, false, false, false),
];

fn columns_for(width: usize) -> Columns {
    // Drop order as the pane shrinks: 1h Δ, then inodes, then type, then
    // size. The window Δ and FULL IN stay; they are why the section exists.
    // The sets are that order reversed, so the first one that fits is the
    // widest the pane can hold.
    for &(hour, inodes, typ, size) in &COLUMN_SETS {
        for bar in (BAR_MIN..=BAR_MAX).rev() {
            let need = column_need(hour, inodes, typ, size, bar);
            if need <= width {
                return Columns {
                    mount: MOUNT_MIN + width - need,
                    stacked: false,
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
        stacked: false,
        typ: false,
        size: false,
        inodes: false,
        hour: false,
        bar: BAR_MIN,
    }
}

/// Mount points beside their figures when the widest one fits whole there
/// at the cost of no more than one column the stacked layout would keep;
/// otherwise each mount point gets a line and its figures go under it,
/// where they have the full width. The bar narrows first to make room.
fn layout_for(width: usize, longest: usize) -> Columns {
    let mut stacked = columns_for((width + MOUNT_MIN).saturating_sub(STACK_INDENT));
    let mount = (longest + 2).max(MOUNT_MIN);
    let from = COLUMN_SETS
        .iter()
        .position(|&set| set == (stacked.hour, stacked.inodes, stacked.typ, stacked.size))
        .unwrap_or(COLUMN_SETS.len() - 1);
    for &(hour, inodes, typ, size) in COLUMN_SETS.iter().skip(from).take(2) {
        for bar in (BAR_MIN..=BAR_MAX).rev() {
            let need = column_need(hour, inodes, typ, size, bar) - MOUNT_MIN + mount;
            if need <= width {
                // The mount column is as wide as the longest mount point
                // and no wider. Spare width widens the bar, which shows
                // more, and whatever is left over trails the last figure.
                let grow = (width - need).min(BAR_WIDE.saturating_sub(bar));
                return Columns {
                    mount,
                    stacked: false,
                    typ,
                    size,
                    inodes,
                    hour,
                    bar: bar + grow,
                };
            }
        }
    }
    stacked.mount = 0;
    stacked.stacked = true;
    stacked
}

fn longest_mount(snap: &Snap) -> usize {
    snap.rows
        .iter()
        .map(|row| tc::display_width(&row.mount.mount_point))
        .max()
        .unwrap_or(0)
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

/// `longest` is the widest disk name. The DISK column holds it and its
/// marker and no more, so on a wide pane the figures sit beside the name
/// instead of at the far edge of the pane.
fn disk_columns(width: usize, longest: usize) -> DiskCols {
    let name = (longest + 2).max(DISK_MIN);
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
                disk: (DISK_MIN + width - need).min(name),
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
    /// Read and write of the disks that no other row names as a slave.
    /// Recorded once per sample, so a disk leaving later does not rewrite it.
    total_read: Vec<Option<f64>>,
    total_write: Vec<Option<f64>>,
    total_names: Vec<String>,
    total_covered: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    List,
    Detail,
    DiskDetail,
    /// `↵` on a BY SERVICE row.
    Service,
    /// `↵` on a WAITING ON DISK row.
    Process,
    /// `↵` on `+N more`: everyone in D.
    Waiters,
}

impl Screen {
    /// The screens drawn from WHO IS WRITING rather than the tables above it.
    fn writes(self) -> bool {
        matches!(self, Screen::Service | Screen::Process | Screen::Waiters)
    }

    /// The window is filesystem history; only the screens that draw it offer `w`.
    fn uses_window(self) -> bool {
        matches!(self, Screen::List | Screen::Detail)
    }
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
        txt: tc::ink(tc::Role::Fg),
        // The plain grey measured 3.81 on the selected-row tint. The lifted
        // twin is what the tint closure reaches for.
        dim: tc::ink(tc::Role::Muted),
        dim_lit: tc::ink_on(tc::Role::Muted, true),
        lbl: tc::ink(tc::Role::Muted),
        accent: tc::ink(tc::Role::Accent),
        down: tc::ink(tc::Role::Orange),
    }
}

impl Palette {
    /// `colour` as it is drawn on the selected-row tint: each role in its
    /// lifted form, so every one of them stays readable there. A colour
    /// that is not one of the palette's (the heat ramp, already asked for
    /// in its tinted form) is handed back as it came.
    fn lit(&self, colour: &str) -> String {
        if colour == self.dim || colour == self.lbl {
            return self.dim_lit.clone();
        }
        for (plain, role) in [
            (&self.txt, tc::Role::Fg),
            (&self.accent, tc::Role::Accent),
            (&self.down, tc::Role::Orange),
        ] {
            if colour == plain.as_str() {
                return tc::ink_on(role, true);
            }
        }
        colour.to_string()
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
    window_hours: u64,
    now: f64,
    history_note: &'a str,
    save_err: &'a str,
    writers: Option<&'a writers::Writers>,
    svc_sort: DiskSort,
    svc_all: bool,
    top_writers: usize,
    svc_cursor: &'a str,
    wait_sort: writers::WaitSort,
    wait_cursor: &'a str,
    proc_sort: DiskSort,
    proc_all: bool,
    proc_cursor: &'a str,
    show_kernel: bool,
    service_path: &'a str,
    opened: Option<&'a writers::Opened>,
    /// The DISKS cursor has been put on a disk with an arrow or a click.
    /// Until then WHO IS WRITING describes whichever disk the table shows
    /// first, which sorted by write is the busiest.
    disk_chosen: bool,
}

/// Sections, in the order drawn and stepped through: filesystems, disks,
/// pressure, by service, by process, waiting on disk.
const SECTIONS: usize = 6;
const BY_SERVICE: usize = 3;
const BY_PROCESS: usize = 4;
const WAITING: usize = 5;

struct Drawn {
    rows: Vec<String>,
    placed: Vec<(usize, usize)>,
    head: usize,
    selected: usize,
    disk_selected: usize,
    plots: Vec<tc::Plot>,
    lens: [usize; SECTIONS],
    /// BY SERVICE, BY PROCESS and WAITING ON DISK items in the order
    /// drawn, by key.
    svc_keys: Vec<String>,
    proc_keys: Vec<String>,
    wait_keys: Vec<String>,
    /// BY SERVICE rows before the top-N cut.
    svc_total: usize,
    /// BY PROCESS rows `a` would show.
    proc_total: usize,
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    tc::register_settings(SETTINGS);
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
    let mut window_hours = window_hours_of(&cfg);

    let top_writers = tc::cfg_usize(&cfg, "top_writers", 8).max(1);
    let show_kernel = cfg
        .get("show_kernel_threads")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let reconcile_seconds = reconcile_seconds_of(&cfg);

    let state = Arc::new(Mutex::new(State::default()));
    let pause = Arc::new(AtomicBool::new(false));
    // The cgroup and the process whose detail is open, so the poller reads
    // the cgroup's io.pressure and the process's facts and open files.
    let detail: Arc<Mutex<writers::Wanted>> = Arc::new(Mutex::new(writers::Wanted::default()));
    let refresh = tc::Refresh::default();
    {
        let state = Arc::clone(&state);
        let pause = Arc::clone(&pause);
        let refresh = refresh.clone();
        let detail = Arc::clone(&detail);
        let window = reconcile_seconds;
        thread::spawn(move || poll(state, pause, refresh, hide, every, detail, window));
    }

    let p = palette();
    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut sort = Sort::Mount;
    let mut disk_sort = DiskSort::Write;
    let mut svc_sort = DiskSort::Write;
    let mut wait_sort = writers::WaitSort::Waited;
    let mut proc_sort = DiskSort::Write;
    let mut svc_all = false;
    let mut proc_all = false;
    let mut screen = Screen::List;
    let mut cursor = String::new();
    let mut disk_cursor = String::new();
    let mut svc_cursor = String::new();
    let mut wait_cursor = String::new();
    let mut proc_cursor = String::new();
    let mut service_path = String::new();
    let mut opened: Option<writers::Opened> = None;
    let mut scroll = 0usize;
    let mut chase = true;
    let mut placed: Vec<(usize, usize)> = Vec::new();
    let mut head = 1usize;
    let mut shown_scroll = 0usize;
    // The row each section's cursor is on: filesystems, disks, pressure,
    // by service, by process, waiting on disk.
    let mut at = [0usize; SECTIONS];
    let mut focus = 0usize;
    let mut lens = [0usize; SECTIONS];
    let mut show_idle = false;
    // What BY SERVICE, BY PROCESS and WAITING drew last frame, which is what
    // a key or a click this frame was aimed at.
    let mut svc_keys: Vec<String> = Vec::new();
    let mut proc_keys: Vec<String> = Vec::new();
    let mut wait_keys: Vec<String> = Vec::new();
    let mut disk_chosen = false;

    loop {
        let mut keys = keyboard.poll();
        // Set by `enter`, acted on after the batch's arrows have moved the
        // cursor, so an arrow and an enter read together open the new row.
        let mut open = false;
        if screen == Screen::List {
            let current = item_id(focus, &at);
            if let Some(id) =
                tc::rows_clicked(&mut keys, Some(current), head, shown_scroll, &placed, None)
            {
                let (section, row) = section_of(id);
                focus = section;
                at[section] = row;
                disk_chosen |= section == 1;
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
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "up" | "k" | "K" => {
                    if screen == Screen::List {
                        if let Some((next, row)) =
                            tc::step_across_sections(focus, at[focus], &lens, false)
                        {
                            focus = next;
                            at[next] = row;
                            // An arrow onto a disk picks it for WHO IS
                            // WRITING; tabbing past the table does not.
                            disk_chosen |= next == 1;
                            chase = true;
                        }
                    }
                }
                "down" | "j" | "J" => {
                    if screen == Screen::List {
                        if let Some((next, row)) =
                            tc::step_across_sections(focus, at[focus], &lens, true)
                        {
                            focus = next;
                            at[next] = row;
                            disk_chosen |= next == 1;
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
                "a" | "A" => {
                    // BY PROCESS when it has the cursor; BY SERVICE
                    // otherwise, as before BY PROCESS existed.
                    if screen == Screen::List {
                        if focus == BY_PROCESS {
                            proc_all = !proc_all;
                        } else {
                            svc_all = !svc_all;
                        }
                    }
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
                        *detail
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                            writers::Wanted::default();
                    }
                }
                "s" | "S" => {
                    match focus {
                        0 => sort = sort.next(),
                        1 => disk_sort = disk_sort.next(),
                        BY_SERVICE => svc_sort = svc_sort.next(),
                        BY_PROCESS => proc_sort = proc_sort.next(),
                        WAITING => wait_sort = wait_sort.next(),
                        _ => {}
                    }
                    chase = false;
                }
                "w" | "W" => {
                    if screen.uses_window() {
                        window_hours = tc::cycle(&WINDOWS, window_hours);
                    }
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

        let (snap, err, sampled, board, disk_err, usage, history_note, save_err, writers_now) = {
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
                guard.writers.clone(),
            )
        };
        let age = sampled.map(|at| at.elapsed());
        let paused = pause.load(Ordering::Acquire);
        if let Some(snap) = snap.as_ref() {
            let count = snap.rows.len();
            let missing = !snap.rows.iter().any(|row| row.mount.mount_point == cursor);
            if count > 0 && ((screen == Screen::List && chase && focus == 0) || missing) {
                if let Some(path) = path_at(snap, sort, at[0].min(count - 1), &usage, window_hours)
                {
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
                let row = at[1].min(indexes.len() - 1);
                disk_cursor = board.rows[indexes[row]].disk.name.clone();
            }
        }
        for (section, keys, cursor) in [
            (BY_SERVICE, &svc_keys, &mut svc_cursor),
            (BY_PROCESS, &proc_keys, &mut proc_cursor),
            (WAITING, &wait_keys, &mut wait_cursor),
        ] {
            if screen == Screen::List && chase && focus == section && !keys.is_empty() {
                *cursor = keys[at[section].min(keys.len() - 1)].clone();
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
                // The root cgroup's key is the empty path, so a drawn row is
                // the test, not a non-empty cursor.
                BY_SERVICE if svc_keys.contains(&svc_cursor) => {
                    screen = Screen::Service;
                    service_path = svc_cursor.clone();
                    *detail
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = writers::Wanted {
                        cgroup: Some(service_path.clone()),
                        pid: None,
                    };
                    scroll = 0;
                    chase = true;
                }
                BY_PROCESS => {
                    let found = proc_cursor.parse::<i32>().ok().and_then(|pid| {
                        let procs = writers_now.as_ref()?.procs.as_ref()?;
                        procs
                            .rows
                            .iter()
                            .find(|r| r.pid == pid)
                            .map(|r| (r.pid, r.comm.clone(), r.cgroup.clone()))
                            .or_else(|| {
                                procs
                                    .locked
                                    .iter()
                                    .find(|l| l.pid == pid)
                                    .map(|l| (l.pid, l.comm.clone(), l.cgroup.clone()))
                            })
                    });
                    if let Some((pid, comm, cgroup)) = found {
                        let (cmdline, cwd) = host::process_facts(pid);
                        let start = writers_now.as_ref().and_then(|w| w.starts.get(&pid).copied());
                        opened = Some(writers::Opened {
                            pid,
                            comm,
                            cgroup,
                            cmdline,
                            cwd,
                            from_wait: false,
                            start,
                        });
                        *detail
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) = writers::Wanted {
                            cgroup: None,
                            pid: Some(pid),
                        };
                        screen = Screen::Process;
                        scroll = 0;
                        chase = true;
                    }
                }
                WAITING if wait_cursor == writers::MORE => {
                    screen = Screen::Waiters;
                    scroll = 0;
                    chase = true;
                }
                WAITING => {
                    let found = wait_cursor.parse::<i32>().ok().and_then(|pid| {
                        writers_now
                            .as_ref()
                            .and_then(|w| w.waiters.iter().find(|x| x.pid == pid).cloned())
                    });
                    if let Some(waiter) = found {
                        let (cmdline, cwd) = host::process_facts(waiter.pid);
                        *detail
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) = writers::Wanted {
                            cgroup: None,
                            pid: Some(waiter.pid),
                        };
                        let start = writers_now
                            .as_ref()
                            .and_then(|w| w.starts.get(&waiter.pid).copied());
                        opened = Some(writers::Opened {
                            pid: waiter.pid,
                            comm: waiter.comm,
                            cgroup: waiter.cgroup,
                            cmdline,
                            cwd,
                            from_wait: true,
                            start,
                        });
                        screen = Screen::Process;
                        scroll = 0;
                        chase = true;
                    }
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
            window_hours,
            now: tc::now(),
            history_note: &history_note,
            save_err: &save_err,
            writers: writers_now.as_deref(),
            svc_sort,
            svc_all,
            top_writers,
            svc_cursor: &svc_cursor,
            wait_sort,
            wait_cursor: &wait_cursor,
            proc_sort,
            proc_all,
            proc_cursor: &proc_cursor,
            show_kernel,
            service_path: &service_path,
            opened: opened.as_ref(),
            disk_chosen,
        };
        let (w, h) = tc::size();
        let drawn = draw_body(&view, w, &p, refresh.tick(tc::animation_tick()));
        at[0] = drawn.selected;
        at[1] = drawn.disk_selected;
        lens = drawn.lens;
        for (section, keys, cursor) in [
            (BY_SERVICE, &drawn.svc_keys, &mut svc_cursor),
            (BY_PROCESS, &drawn.proc_keys, &mut proc_cursor),
            (WAITING, &drawn.wait_keys, &mut wait_cursor),
        ] {
            match keys.iter().position(|key| key == cursor) {
                Some(row) => at[section] = row,
                None => {
                    // The row the cursor was on has gone (a cgroup went
                    // quiet, a process left D): the cursor takes whatever
                    // is now at its index rather than vanishing.
                    if let Some(key) = keys.get(at[section].min(keys.len().saturating_sub(1))) {
                        *cursor = key.clone();
                    }
                }
            }
        }
        if screen == Screen::List && focus >= BY_SERVICE && lens[focus] == 0 {
            // A section that emptied under the cursor is not a place to be.
            focus = tc::next_section(None, &lens).unwrap_or(0);
        }
        svc_keys = drawn.svc_keys.clone();
        proc_keys = drawn.proc_keys.clone();
        wait_keys = drawn.wait_keys.clone();
        let cursor_gone = snap
            .as_ref()
            .is_some_and(|snap| !snap.rows.iter().any(|row| row.mount.mount_point == cursor));
        if cursor.is_empty() || cursor_gone {
            if let Some(path) = snap
                .as_ref()
                .and_then(|snap| path_at(snap, sort, at[0], &usage, window_hours))
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
                    .get(at[1])
                    .map(|&index| board.rows[index].disk.name.clone())
            }) {
                disk_cursor = name;
            }
        }
        let combined = board.as_ref().is_some_and(|board| board.combined);
        // `a` acts on BY PROCESS while it has the cursor, and on BY SERVICE
        // otherwise; the hint names whichever one the press would change.
        let all = if focus == BY_PROCESS {
            (proc_all || drawn.proc_total > drawn.proc_keys.len())
                .then(|| by_process::all_hint(proc_all, top_writers))
        } else {
            (drawn.svc_total > top_writers).then(|| writers::all_hint(svc_all, top_writers))
        };
        let hints = WriterHints {
            all,
            svc_sort,
            proc_sort,
            wait_sort: writers_now
                .as_ref()
                .is_some_and(|w| w.wait_times)
                .then_some(wait_sort),
        };
        let (packed, indent) = footer(
            w,
            screen,
            sort,
            disk_sort,
            focus,
            show_idle,
            paused,
            window_hours,
            combined,
            &hints,
            &p,
        );
        let foot = packed.lines.clone();
        let room = h.saturating_sub(foot.len());
        let head_rows = drawn.head.min(drawn.rows.len());
        let rest = drawn.rows.len().saturating_sub(head_rows);
        let room_below = room.saturating_sub(head_rows).max(1);
        let follow_id = item_id(focus, &at);
        if chase {
            if let Some(row) = drawn
                .placed
                .iter()
                .find(|(_, item)| *item == follow_id)
                .map(|(row, _)| *row)
            {
                let in_rest = row.saturating_sub(head_rows);
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

/// The click id of the row a section's cursor is on.
fn item_id(focus: usize, at: &[usize; SECTIONS]) -> usize {
    match focus {
        0 => at[0],
        1 => DISK_BASE + at[1],
        2 => PRESSURE_ID,
        BY_SERVICE => writers::SERVICE_BASE + at[BY_SERVICE],
        BY_PROCESS => by_process::PROC_BASE + at[BY_PROCESS],
        _ => writers::WAIT_BASE + at[WAITING],
    }
}

/// The section and row a click id names.
fn section_of(id: usize) -> (usize, usize) {
    if id >= by_process::PROC_BASE {
        (BY_PROCESS, id - by_process::PROC_BASE)
    } else if id >= writers::WAIT_BASE {
        (WAITING, id - writers::WAIT_BASE)
    } else if id >= writers::SERVICE_BASE {
        (BY_SERVICE, id - writers::SERVICE_BASE)
    } else if id >= PRESSURE_ID {
        (2, 0)
    } else if id >= DISK_BASE {
        (1, id - DISK_BASE)
    } else {
        (0, id)
    }
}

/// What the footer says about WHO IS WRITING. `wait_sort` is `None` while
/// wait times are off: the order is then pid, and there is nothing to cycle.
struct WriterHints {
    all: Option<String>,
    svc_sort: DiskSort,
    proc_sort: DiskSort,
    wait_sort: Option<writers::WaitSort>,
}

/// `reconcile_seconds`: how long BY PROCESS sums both sides before it
/// compares them. At least 10, because under that the dirtying and the
/// submission of the same bytes fall in different windows more often than
/// not; at most a day.
fn reconcile_seconds_of(cfg: &serde_json::Value) -> f64 {
    let value = tc::cfg_f64(cfg, "reconcile_seconds", 60.0);
    if value.is_finite() {
        value.clamp(10.0, 86_400.0)
    } else {
        60.0
    }
}

fn path_at(
    snap: &Snap,
    sort: Sort,
    selected: usize,
    usage: &state::Store,
    window_hours: u64,
) -> Option<String> {
    let ordered = ordered(snap, sort, usage, window_hours, tc::now());
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
    writers: Option<Arc<writers::Writers>>,
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
            writers: None,
        }
    }
}

struct RateMemory {
    prev: HashMap<String, parse::DiskStat>,
    at: Option<Instant>,
    series: HashMap<String, (Vec<Option<f64>>, Vec<Option<f64>>)>,
    total_read: Vec<Option<f64>>,
    total_write: Vec<Option<f64>>,
}

impl Default for RateMemory {
    fn default() -> Self {
        Self {
            prev: HashMap::new(),
            at: None,
            series: HashMap::new(),
            total_read: Vec::new(),
            total_write: Vec::new(),
        }
    }
}

/// What the disks moved this poll, in the unit BY PROCESS compares with:
/// the summed write rate on Linux, iostat's combined rate on macOS. `None`
/// when any disk in the sum has no rate this poll.
fn device_bps(board: &DiskBoard) -> Option<f64> {
    let series = if board.combined {
        &board.total_read
    } else {
        &board.total_write
    };
    series.last().copied().flatten()
}

fn poll(
    state: Arc<Mutex<State>>,
    pause: Arc<AtomicBool>,
    refresh: tc::Refresh,
    hide: Vec<String>,
    every: f64,
    detail: Arc<Mutex<writers::Wanted>>,
    reconcile_seconds: f64,
) {
    tc::record_panics();
    let mut inflight: HashMap<Vec<u8>, Receiver<Result<Space, String>>> = HashMap::new();
    let mut rates = RateMemory::default();
    let mut source = host::WriterSource::default();
    // BY PROCESS's reconciliation window: per-poll sums covering
    // `reconcile_seconds` of measured time, and nothing older.
    let mut ring = by_process::Ring::new(reconcile_seconds);
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
            // One cgroup walk and one /proc pass, on this same poll, so the
            // by-service rates cover the interval the disk rates do.
            let wanted = detail
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let writers = host::sample_writers(&mut source, wanted);
            (filesystems, io, writers)
        }));
        match round {
            Ok((filesystems, io, writers)) => {
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
                    let device = match io {
                        Ok(snap) => {
                            guard.disk_err = snap.err.clone();
                            let board = board_from(&snap, &mut rates);
                            let device = device_bps(&board);
                            guard.board = Some(board);
                            device
                        }
                        Err(err) => {
                            guard.disk_err = err;
                            None
                        }
                    };
                    let mut writers = writers;
                    if let Some(procs) = writers.as_mut().and_then(|w| w.procs.as_mut()) {
                        // Both sides over the same poll, or neither: a poll
                        // where the disks have no rate is left out whole.
                        if let (Some(seconds), Some(moved), Some(device)) =
                            (procs.elapsed, procs.moved, device)
                        {
                            ring.push(seconds, moved, device * seconds);
                        }
                        procs.window = ring.window();
                    }
                    guard.writers = writers.map(Arc::new);
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

fn window_hours_of(cfg: &serde_json::Value) -> u64 {
    let hours = tc::cfg_usize(cfg, "window_hours", DEFAULT_WINDOW as usize) as u64;
    if WINDOWS.contains(&hours) {
        hours
    } else {
        DEFAULT_WINDOW
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
    if matches!(view.screen, Screen::List | Screen::DiskDetail) {
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
            let ordered = ordered(snap, view.sort, view.usage, view.window_hours, view.now);
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
    let mut painted = writers::Painted::default();
    if view.screen == Screen::List && view.writers.is_some() {
        let slot = Mutex::new((Vec::new(), writers::Painted::default()));
        let section = tc::guard_rows("who is writing", width, || {
            let mut body = vec![String::new()];
            let mut at = Vec::new();
            let out = writers::paint_section(view, width, p, &mut body, &mut at);
            if let Ok(mut slot) = slot.lock() {
                *slot = (at, out);
            }
            body
        });
        let failed = section.iter().any(|line| line.contains("could not draw"));
        if !failed {
            let (mut at, out) = slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let base = rows.len();
            for (row, _) in &mut at {
                *row += base;
            }
            placed.extend(at);
            painted = out;
        }
        rows.extend(section);
    }
    if view.screen.writes() {
        let section = tc::guard_rows("who is writing", width, || {
            let mut body = Vec::new();
            match view.screen {
                Screen::Service => {
                    writers::paint_service(view, view.service_path, width, p, &mut body)
                }
                Screen::Process => match view.opened {
                    Some(open) => writers::paint_process(view, open, width, p, &mut body),
                    None => body.push(tc::seg(
                        &[(p.dim.as_str(), " nothing selected".into())],
                        width,
                    )),
                },
                _ => writers::paint_waiters(view, width, p, &mut body),
            }
            body
        });
        rows.extend(section);
    }
    let lens = if view.screen == Screen::List {
        [
            fs_len,
            disk_len,
            pressure_len,
            painted.svc_keys.len(),
            painted.proc_keys.len(),
            painted.wait_keys.len(),
        ]
    } else {
        [0; SECTIONS]
    };
    Drawn {
        rows,
        placed,
        head,
        selected,
        disk_selected,
        plots,
        lens,
        svc_keys: painted.svc_keys,
        proc_keys: painted.proc_keys,
        wait_keys: painted.wait_keys,
        svc_total: painted.svc_total,
        proc_total: painted.proc_total,
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
    if view.screen == Screen::DiskDetail || view.screen.writes() {
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
    let cols = layout_for(width, longest_mount(snap));
    rows.extend(header_row(&cols, view.window_hours, width, p));
    let ordered = ordered(snap, view.sort, view.usage, view.window_hours, view.now);
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
            view.window_hours,
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
    let Some(row) = ordered(snap, view.sort, view.usage, view.window_hours, view.now)
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

fn header_row(cols: &Columns, window_hours: u64, width: usize, p: &Palette) -> Vec<String> {
    let dim = p.dim.clone();
    let left = |text: &str, width: usize| (dim.clone(), tc::pad(text, width));
    let right = |text: String, width: usize| (dim.clone(), align_right(text, width));
    let mut lines = Vec::new();
    let mut cells = Vec::new();
    if cols.stacked {
        lines.push(tc::seg(&[(dim.as_str(), "MOUNT".to_string())], width));
    } else {
        cells.push(left("MOUNT", cols.mount));
    }
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
    cells.push(right(format!("{window_hours}h Δ"), DAY_W));
    cells.push(right("FULL IN".into(), FULL_W));
    if cols.inodes {
        cells.push(right("INODES".into(), INODES_W));
    }
    if cols.stacked {
        lines.extend(indented(
            pack_cells(cells, width.saturating_sub(STACK_INDENT), ""),
            "",
        ));
    } else {
        lines.extend(pack_cells(cells, width, ""));
    }
    lines
}

/// The figures under a stacked mount point, pushed right by the indent.
/// Packed at the narrower width first, so a header and its rows wrap at
/// the same cells.
fn indented(lines: Vec<String>, tint: &str) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| format!("{tint}{}{line}", " ".repeat(STACK_INDENT)))
        .collect()
}

fn data_rows(
    row: &Row,
    cols: &Columns,
    width: usize,
    here: bool,
    warn: f64,
    alert: f64,
    usage: &state::Store,
    window_hours: u64,
    now: f64,
    p: &Palette,
) -> Vec<String> {
    let tint = if here {
        tc::tint(tc::Role::Slot)
    } else {
        String::new()
    };
    let c = |colour: &str| {
        // Every colour on this tint is swapped for its lifted twin, so it
        // clears AA there. The plain grey measured 3.81 on Classic's tint.
        if tint.is_empty() {
            format!("{}{}", tint, colour)
        } else {
            format!("{}{}", tint, p.lit(colour))
        }
    };
    let mut out = Vec::new();
    let name_ink = c(if here {
        p.accent.as_str()
    } else {
        p.txt.as_str()
    });
    let tint_fill = if here { tint.as_str() } else { "" };
    if cols.stacked {
        // The mount point on its own line or lines, the figures under it.
        let room = width.saturating_sub(2).max(1);
        for (index, chunk) in chunks(&row.mount.mount_point, room).iter().enumerate() {
            let marker = if index == 0 && here { "▸ " } else { "  " };
            out.push(tc::seg(
                &[(
                    name_ink.as_str(),
                    tc::pad(&format!("{marker}{chunk}"), width),
                )],
                width,
            ));
        }
        let room = width.saturating_sub(STACK_INDENT);
        let fill = if here { tint.as_str() } else { "" };
        let note = match &row.figures {
            Figures::NoAnswer => Some("no answer in 2s"),
            Figures::Failed(err) => Some(err.as_str()),
            Figures::Ready(_) => None,
        };
        if let Some(note) = note {
            // A sentence, so it wraps at words rather than being cut.
            let ink = c(p.accent.as_str());
            let lines = wrap(note, room)
                .into_iter()
                .map(|line| tc::seg(&[(ink.as_str(), tc::pad(&line, room))], room))
                .collect();
            out.extend(indented(lines, fill));
            return out;
        }
        let mut cells = Vec::new();
        figure_cells(
            row,
            cols,
            here,
            warn,
            alert,
            usage,
            window_hours,
            now,
            p,
            &c,
            &mut cells,
        );
        out.extend(indented(pack_cells(cells, room, tint_fill), fill));
        return out;
    }
    let room = cols.mount.saturating_sub(2).max(1);
    let chunks = chunks(&row.mount.mount_point, room);
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
        let mut cells = vec![(name_ink.clone(), mount)];
        figure_cells(
            row,
            cols,
            here,
            warn,
            alert,
            usage,
            window_hours,
            now,
            p,
            &c,
            &mut cells,
        );
        out.extend(pack_cells(cells, width, tint_fill));
    }
    out
}

/// Every cell after the mount point, in the columns `cols` keeps.
#[allow(clippy::too_many_arguments)]
fn figure_cells(
    row: &Row,
    cols: &Columns,
    here: bool,
    warn: f64,
    alert: f64,
    usage: &state::Store,
    window_hours: u64,
    now: f64,
    p: &Palette,
    c: &dyn Fn(&str) -> String,
    cells: &mut Vec<(String, String)>,
) {
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
            let (hour, window) = slopes(series, window_hours, now);
            if cols.hour {
                cells.push((c(p.txt.as_str()), rate_cell(hour, 3_600.0, HOUR_W)));
            }
            cells.push((c(p.txt.as_str()), rate_cell(window, 86_400.0, DAY_W)));
            cells.push((
                c(p.txt.as_str()),
                full_cell(space.usable, window, hour, series, window_hours, now),
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
}

fn ordered(
    snap: &Snap,
    sort: Sort,
    usage: &state::Store,
    window_hours: u64,
    now: f64,
) -> Vec<usize> {
    let mut index: Vec<usize> = (0..snap.rows.len()).collect();
    // One fit per row. Doing it inside the comparison refits the same
    // history for every pair, and the frame sorts again for the cursor
    // and the footnote.
    let fills = if sort == Sort::Fills {
        snap.rows
            .iter()
            .map(|row| fill_ord(usage, row, window_hours, now))
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

fn fill_ord(usage: &state::Store, row: &Row, hours: u64, now: f64) -> (u8, u64) {
    // 0 is a real projection, 1 is not filling or still measuring, 2 has
    // not answered. Mount order is the tie-break, so 2 has to be its own
    // rank or a quiet row with a later mount sorts after a row with no answer.
    let Figures::Ready(space) = &row.figures else {
        return (2, 0);
    };
    let Some(series) = series_for(usage, row) else {
        return (1, 0);
    };
    let slope = state::slope_bytes_per_s(series, hours as f64 * 3_600.0, now);
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
    hours: u64,
    now: f64,
) -> (Option<state::Slope>, Option<state::Slope>) {
    let Some(series) = series else {
        return (None, None);
    };
    (
        state::slope_bytes_per_s(series, 3_600.0, now),
        state::slope_bytes_per_s(series, hours as f64 * 3_600.0, now),
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
    hours: u64,
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
                    .map(|series| state::covered(series, hours as f64 * 3_600.0, now).0)
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
    let indexes = ordered(snap, view.sort, view.usage, view.window_hours, view.now);
    let row = indexes
        .iter()
        .copied()
        .find(|&index| snap.rows[index].mount.mount_point == view.cursor)
        .or(indexes.first().copied())
        .map(|index| &snap.rows[index]);
    let hours = view.window_hours;
    let window_s = hours as f64 * 3_600.0;
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
            "FULL IN at the {hours}h rate of this widget's own samples (kept since {})",
            labelled(retained)
        )
    } else {
        format!(
            "FULL IN at the rate since {} ({of} of {hours}h)",
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
    let window_s = view.window_hours as f64 * 3_600.0;
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
    let (hour, window) = slopes(Some(series), view.window_hours, view.now);
    let (span, count) = state::covered(series, window_s, view.now);
    let summary = format!(
        "{count} samples over {} · 1h {} · {}h {}",
        tc::age(span),
        slope_text(hour, 3_600.0),
        view.window_hours,
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
    let (members, covered) = total_membership(&rows);
    let (total_names, total_covered) = if members.is_empty() {
        memory.total_read.clear();
        memory.total_write.clear();
        (Vec::new(), Vec::new())
    } else {
        let read_point = total_point(&rows, &members, false);
        let write_point = if snap.combined {
            None
        } else {
            total_point(&rows, &members, true)
        };
        push_sample(&mut memory.total_read, read_point);
        push_sample(&mut memory.total_write, write_point);
        let mut names: Vec<String> = members
            .iter()
            .map(|&index| rows[index].disk.name.clone())
            .collect();
        names.sort();
        (names, covered)
    };
    DiskBoard {
        rows,
        show_pressure: snap.show_pressure,
        pressure: snap.pressure,
        pressure_absent: snap.pressure_absent,
        pressure_err: snap.pressure_err.clone(),
        host_devices: snap.host_devices,
        combined: snap.combined,
        missing: snap.missing.clone(),
        total_read: memory.total_read.clone(),
        total_write: memory.total_write.clone(),
        total_names,
        total_covered,
    }
}

/// Disks at the top of the stack, and the rows left out because one of
/// those disks names them as a slave, or names a partition of them.
/// Summing both would count the same bytes twice. A partition is not a
/// row, so the whole disk that owns it is the one left out.
fn total_membership(rows: &[DiskViewRow]) -> (Vec<usize>, Vec<String>) {
    let present: std::collections::BTreeSet<&str> =
        rows.iter().map(|row| row.disk.name.as_str()).collect();
    let mut parent_of: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for row in rows {
        for part in &row.disk.partitions {
            parent_of.insert(part.name.as_str(), row.disk.name.as_str());
        }
    }
    let mut covered: Vec<String> = Vec::new();
    for row in rows {
        for slave in &row.disk.slaves {
            if present.contains(slave.as_str()) {
                covered.push(slave.clone());
            } else if let Some(parent) = parent_of.get(slave.as_str()) {
                if present.contains(parent) && *parent != row.disk.name.as_str() {
                    covered.push((*parent).to_string());
                }
            }
        }
    }
    covered.sort();
    covered.dedup();
    let members = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| !covered.iter().any(|name| name == &row.disk.name))
        .map(|(index, _)| index)
        .collect();
    (members, covered)
}

/// The latest sample, summed. One missing rate makes the whole sample a
/// gap: a partial sum would read as the machine.
fn total_point(rows: &[DiskViewRow], members: &[usize], write: bool) -> Option<f64> {
    if members.is_empty() {
        return None;
    }
    let mut sum = 0.0;
    for &index in members {
        let series = if write {
            &rows[index].write
        } else {
            &rows[index].read
        };
        match series.last().copied().flatten() {
            Some(value) if value.is_finite() => sum += value,
            _ => return None,
        }
    }
    Some(sum)
}

fn total_label(names: &[String], covered: &[String]) -> String {
    let names = names.join(", ");
    if covered.is_empty() {
        format!("total of {names}")
    } else {
        format!("total of {names} · {} not added", covered.join(", "))
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
    let longest = view
        .board
        .map(|board| {
            board
                .rows
                .iter()
                .map(|row| tc::display_width(&row.disk.name))
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let cols = disk_columns(width, longest);
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
        // A disk hidden by name stays in the sum, so hiding every row
        // still leaves the machine's chart.
        push_io_chart(view, board, &indexes, width, p, rows, plots);
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
    push_io_chart(view, board, &indexes, width, p, rows, plots);
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
        tc::tint(tc::Role::Slot)
    } else {
        String::new()
    };
    let c = |colour: &str| {
        if tint.is_empty() {
            format!("{}{}", tint, colour)
        } else {
            format!("{}{}", tint, p.lit(colour))
        }
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
    push_chart(
        &row.read,
        &row.write,
        8,
        board.combined,
        width,
        p,
        rows,
        plots,
    );
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
        tc::tint(tc::Role::Slot)
    } else {
        String::new()
    };
    let colour = if focus == 2 {
        p.lit(&p.accent)
    } else {
        p.txt.clone()
    };
    for (index, line) in wrap(&text, width.saturating_sub(1)).into_iter().enumerate() {
        let ink = format!("{tint}{colour}");
        rows.push(tc::seg(&[(ink.as_str(), format!(" {line}"))], width));
        if index == 0 {
            placed.push((rows.len() - 1, PRESSURE_ID));
        }
    }
}

/// One chart under the disk table. A disk the cursor is on (the disk
/// section has focus) charts that disk; otherwise the chart is the total
/// of every disk, so the section never shows one disk's line as if it
/// were the machine's.
fn push_io_chart(
    view: &View,
    board: &DiskBoard,
    indexes: &[usize],
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    plots: &mut Vec<tc::Plot>,
) {
    let selected = (view.focus == 1)
        .then(|| {
            indexes
                .iter()
                .copied()
                .find(|&index| board.rows[index].disk.name == view.disk_cursor)
        })
        .flatten()
        .map(|index| &board.rows[index]);
    if let Some(row) = selected {
        push_wrapped(&row.disk.name, width, p, rows);
        push_chart(
            &row.read,
            &row.write,
            4,
            board.combined,
            width,
            p,
            rows,
            plots,
        );
        return;
    }
    if board.total_names.is_empty() {
        return;
    }
    push_wrapped(
        &total_label(&board.total_names, &board.total_covered),
        width,
        p,
        rows,
    );
    push_chart(
        &board.total_read,
        &board.total_write,
        4,
        board.combined,
        width,
        p,
        rows,
        plots,
    );
}

fn push_chart(
    read: &[Option<f64>],
    write: &[Option<f64>],
    tall: usize,
    combined: bool,
    width: usize,
    p: &Palette,
    rows: &mut Vec<String>,
    plots: &mut Vec<tc::Plot>,
) {
    if chart_caption(read, write, combined) == "no rate yet" {
        rows.push(tc::seg(&[(p.dim.as_str(), " no rate yet".into())], width));
        return;
    }
    // Same framed line chart as netwatch: Braille in text mode, Kitty pixels
    // under the frame. Read grows up from the zero line, write grows down,
    // and each half keeps its own peak so the quieter one still shows.
    let plot_w = width.saturating_sub(2).max(1);
    let up = if combined {
        tall.max(2)
    } else {
        (tall / 2).max(2)
    };
    let down = if combined {
        0
    } else {
        (tall - tall / 2).max(2)
    };
    let read_peak = peak_of(read, &[]).max(1.0);
    let write_peak = peak_of(&[], write).max(1.0);
    let top = if combined {
        format!(
            "peak {}/s  now {} combined",
            human(read_peak.round() as u64),
            rate_now(read)
        )
    } else {
        format!(
            "read  peak {}/s  now {}",
            human(read_peak.round() as u64),
            rate_now(read)
        )
    };
    push_border(rows, &top, width, &p.accent, &p.dim, false);
    let y = rows.len();
    for _ in 0..up {
        rows.push(chart_gutter(width, p));
    }
    push_trace(
        plots,
        read,
        read_peak,
        false,
        p.accent.clone(),
        1,
        y,
        plot_w,
        up,
    );
    if !combined {
        rows.push(chart_rule("├", "┤", "0", width, p));
        let y = rows.len();
        for _ in 0..down {
            rows.push(chart_gutter(width, p));
        }
        push_trace(
            plots,
            write,
            write_peak,
            true,
            p.down.clone(),
            1,
            y,
            plot_w,
            down,
        );
    }
    let bottom = if combined {
        String::new()
    } else {
        format!(
            "write  peak {}/s  now {}",
            human(write_peak.round() as u64),
            rate_now(write)
        )
    };
    push_border(rows, &bottom, width, &p.down, &p.dim, true);
}

fn push_trace(
    plots: &mut Vec<tc::Plot>,
    series: &[Option<f64>],
    peak: f64,
    invert: bool,
    colour: String,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) {
    let values: Vec<Option<f64>> = series
        .iter()
        .map(|value| chart_frac(*value, peak, invert))
        .collect();
    let slots = values.len().max(1);
    plots.push(
        tc::Plot::new(
            x,
            y,
            width,
            height,
            tc::LineChart {
                traces: vec![tc::Trace {
                    values,
                    positions: None,
                    colour,
                    baseline: Some(if invert { 1.0 } else { 0.0 }),
                }],
                slots,
                focus: Some(0),
            },
        )
        .under_text()
        .on_axis(0.0, peak),
    );
}

fn chart_gutter(width: usize, p: &Palette) -> String {
    let inner = width.saturating_sub(2);
    tc::seg(
        &[(p.dim.as_str(), format!("│{}│", " ".repeat(inner)))],
        width,
    )
}

fn chart_rule(left: &str, right: &str, label: &str, width: usize, p: &Palette) -> String {
    let gap =
        width.saturating_sub(left.chars().count() + right.chars().count() + label.chars().count());
    tc::seg(
        &[(
            p.dim.as_str(),
            format!("{left}{label}{}{right}", "─".repeat(gap)),
        )],
        width,
    )
}

/// A chart edge. The label takes the ink of its series and wraps onto more
/// rules when the pane is narrower than the words, without cutting one.
fn push_border(
    rows: &mut Vec<String>,
    label: &str,
    width: usize,
    ink: &str,
    grid: &str,
    lower: bool,
) {
    let inner = width.saturating_sub(2).max(1);
    let lines = if label.is_empty() {
        vec![String::new()]
    } else {
        wrap(label, inner)
    };
    let last = lines.len().saturating_sub(1);
    for (i, line) in lines.iter().enumerate() {
        let (left, right) = if lower {
            if i == last {
                ("└", "┘")
            } else {
                ("│", "│")
            }
        } else if i == 0 {
            ("┌", "┐")
        } else {
            ("│", "│")
        };
        let pad = inner.saturating_sub(line.chars().count());
        rows.push(tc::seg(
            &[
                (grid, left.into()),
                (ink, line.clone()),
                (grid, format!("{}{right}", "─".repeat(pad))),
            ],
            width,
        ));
    }
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
    window_hours: u64,
    combined: bool,
    writers: &WriterHints,
    p: &Palette,
) -> (tc::Footer, usize) {
    let mut hints: Vec<Vec<(&str, String)>> = Vec::new();
    if screen != Screen::List {
        hints.push(vec![(p.dim.as_str(), "esc back".into())]);
    } else {
        hints.push(vec![(p.dim.as_str(), "↑↓ select".into())]);
        hints.push(vec![(p.dim.as_str(), "[↵] detail".into())]);
        let hint = match focus {
            0 => Some(sort.hint()),
            1 => Some(disk_sort.hint(combined)),
            BY_SERVICE => Some(writers.svc_sort.hint(false)),
            BY_PROCESS => Some(writers.proc_sort.hint(false)),
            WAITING => writers.wait_sort.map(writers::WaitSort::hint),
            _ => None,
        };
        if let Some(hint) = hint {
            hints.push(vec![(p.dim.as_str(), hint.into())]);
        }
        if let Some(all) = &writers.all {
            hints.push(vec![(p.dim.as_str(), all.clone())]);
        }
        hints.push(vec![(p.dim.as_str(), "tab section".into())]);
        hints.push(vec![(p.dim.as_str(), idle_hint(show_idle).into())]);
    }
    // The window is filesystem history. The disk detail screen draws only
    // the in-memory I/O chart, so a window key there would change nothing.
    if screen.uses_window() {
        let next = tc::cycle(&WINDOWS, window_hours);
        hints.push(vec![(p.dim.as_str(), format!("[w] {next}h"))]);
    }
    hints.push(vec![(p.dim.as_str(), pause_hint(paused).into())]);
    hints.push(vec![(p.dim.as_str(), "[,] settings".into())]);
    hints.push(vec![(p.dim.as_str(), tc::PALETTE_HINT.into())]);
    hints.push(vec![(p.dim.as_str(), tc::THEME_HINT.into())]);
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
            window_hours: 24,
            now: 1_700_000_000.0,
            history_note: "",
            save_err: "",
            writers: None,
            svc_sort: DiskSort::Write,
            svc_all: false,
            top_writers: 8,
            svc_cursor: "",
            wait_sort: writers::WaitSort::Waited,
            wait_cursor: "",
            proc_sort: DiskSort::Write,
            proc_all: false,
            proc_cursor: "",
            show_kernel: false,
            service_path: "",
            opened: None,
            disk_chosen: false,
        }
    }

    fn no_writer_hints() -> WriterHints {
        WriterHints {
            all: None,
            svc_sort: DiskSort::Write,
            proc_sort: DiskSort::Write,
            wait_sort: None,
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
            let cols = layout_for(width, 2);
            if cols.inodes {
                assert!(flat.contains("n/a"), "inode n/a missing at {width}");
            }
            // A stacked header is MOUNT on a line of its own and the figure
            // names under it, down to the first mount.
            let lines: Vec<String> = drawn.rows.iter().map(|line| plain(line)).collect();
            let from = lines
                .iter()
                .position(|line| line.starts_with("MOUNT"))
                .unwrap_or_else(|| panic!("no header at {width}"));
            let to = lines[from..]
                .iter()
                .position(|line| line.starts_with("▸ "))
                .map(|at| from + at)
                .unwrap_or_else(|| panic!("no cursor row at {width}"));
            let header = lines[from..to].join("\n");
            assert!(header.contains("USED") && header.contains("USABLE"));
            assert_eq!(header.contains("INODES"), cols.inodes, "{width}");
            assert_eq!(header.contains("TYPE"), cols.typ, "{width}");
            assert_eq!(header.contains("SIZE"), cols.size, "{width}");
            assert!(flat.contains("FULL IN"), "{width}");
            assert!(flat.contains("24h"), "{width}");
            let (packed, _) = footer(
                width,
                Screen::List,
                Sort::Mount,
                DiskSort::Write,
                0,
                false,
                false,
                24,
                false,
                &no_writer_hints(),
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
        let path = "/mnt/example-data/with/a/long/tail";
        let rows = vec![row(path, "ext4", Figures::Ready(space(1, 1, 10, 9)), 0)];
        let snap = snap(rows, Hidden::default());
        let p = palette();
        let drawn = draw_body(&view(Some(&snap), path), 28, &p, None);
        for line in &drawn.rows {
            assert!(tc::display_width(line) <= 28, "{line:?}");
        }
        assert!(layout_for(28, path.len()).stacked);
        // Stacked, the mount point has the whole line less the marker.
        let pieces = chunks(path, 26);
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
        let order = ordered(&snap, Sort::Percent, empty_usage(), 24, 0.0);
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
        let order = ordered(&snap, Sort::Fills, &store, 24, now);
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
        let order = ordered(&snap, Sort::Fills, &store, 24, now);
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
        assert!(plain_flat.contains("of 24h"), "{plain_flat}");
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
        let hour = 3_600.0;
        let kept = now - 30.0 * hour;
        let fit_at = now - 24.0 * hour + 15.0 * 60.0;
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
        state::record(&mut full, now - 24.0 * hour, 2);
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
            !flat.contains(&format!("rate since {}", stamp(now - 24.0 * hour))),
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
            total_read: Vec::new(),
            total_write: Vec::new(),
            total_names: Vec::new(),
            total_covered: Vec::new(),
        }
    }

    #[test]
    fn disk_columns_drop_queue_then_await_then_type_then_size_then_iops() {
        let flags = |width| {
            let cols = disk_columns(width, 4);
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
            for phrase in [
                "whole disks only",
                "/proc/diskstats",
                "512-byte",
                "sectors",
                "peak",
                "┌",
                "├0",
            ] {
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
        assert_eq!(drawn.plots.len(), 2);
        assert_eq!(drawn.plots[0].height, 2);
        assert_eq!(drawn.plots[1].height, 2);
        assert_eq!(drawn.plots[0].chart.traces[0].baseline, Some(0.0));
        assert_eq!(drawn.plots[1].chart.traces[0].baseline, Some(1.0));
        assert!(plain_has(&drawn, "┌"));
        assert!(plain_has(&drawn, "├0"));
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
    fn the_total_leaves_a_slave_out_and_a_gap_is_not_a_partial() {
        let mut mapper = disk_row("dm-0", 0, 0, 0.0);
        mapper.disk.slaves = vec!["sda".into()];
        mapper.read = vec![Some(100.0)];
        mapper.write = vec![Some(40.0)];
        let mut sda = disk_row("sda", 0, 0, 0.0);
        sda.read = vec![Some(100.0)];
        sda.write = vec![Some(40.0)];
        let mut sdb = disk_row("sdb", 0, 0, 0.0);
        sdb.read = vec![Some(50.0)];
        sdb.write = vec![Some(10.0)];
        let rows = vec![mapper, sda, sdb];
        let (members, covered) = total_membership(&rows);
        assert_eq!(covered, vec!["sda".to_string()]);
        let mut names: Vec<_> = members
            .iter()
            .map(|&index| rows[index].disk.name.as_str())
            .collect();
        names.sort();
        assert_eq!(names, vec!["dm-0", "sdb"]);
        assert_eq!(total_point(&rows, &members, false), Some(150.0));
        assert_eq!(total_point(&rows, &members, true), Some(50.0));
        let mut gapped = rows.clone();
        gapped[0].read.push(None);
        gapped[0].write.push(None);
        gapped[2].read.push(Some(50.0));
        gapped[2].write.push(Some(10.0));
        assert_eq!(total_point(&gapped, &members, false), None);
        assert_eq!(
            total_label(&["dm-0".into(), "sdb".into()], &["sda".into()]),
            "total of dm-0, sdb · sda not added"
        );
    }

    #[test]
    fn a_partition_slave_leaves_its_disk_out_of_the_total() {
        let mut mapper = disk_row("dm-0", 0, 0, 0.0);
        mapper.disk.slaves = vec!["sda2".into()];
        mapper.disk.partitions.clear();
        mapper.read = vec![Some(100.0)];
        mapper.write = vec![Some(40.0)];
        let mut sda = disk_row("sda", 0, 0, 0.0);
        sda.disk.partitions = vec![
            parse::PartMount {
                name: "sda1".into(),
                dev: "8:1".into(),
                mounts: vec![],
            },
            parse::PartMount {
                name: "sda2".into(),
                dev: "8:2".into(),
                mounts: vec![],
            },
        ];
        sda.read = vec![Some(100.0)];
        sda.write = vec![Some(40.0)];
        let mut sdb = disk_row("sdb", 0, 0, 0.0);
        sdb.read = vec![Some(50.0)];
        sdb.write = vec![Some(10.0)];
        let rows = vec![mapper, sda, sdb];
        let (members, covered) = total_membership(&rows);
        assert_eq!(covered, vec!["sda".to_string()]);
        let mut names: Vec<_> = members
            .iter()
            .map(|&index| rows[index].disk.name.as_str())
            .collect();
        names.sort();
        assert_eq!(names, vec!["dm-0", "sdb"]);
        // 100 from dm-0 plus 50 from sdb. sda's 100 is the same bytes as dm-0.
        assert_eq!(total_point(&rows, &members, false), Some(150.0));
        assert_eq!(total_point(&rows, &members, true), Some(50.0));

        let mut raid = disk_row("md0", 0, 0, 0.0);
        raid.disk.slaves = vec!["sda2".into(), "sdb1".into()];
        raid.disk.partitions.clear();
        raid.read = vec![Some(80.0)];
        raid.write = vec![Some(20.0)];
        let pair = vec![raid, rows[1].clone(), rows[2].clone()];
        let (members, covered) = total_membership(&pair);
        assert_eq!(covered, vec!["sda".to_string(), "sdb".to_string()]);
        assert_eq!(
            members
                .iter()
                .map(|&index| pair[index].disk.name.as_str())
                .collect::<Vec<_>>(),
            vec!["md0"]
        );
        assert_eq!(total_point(&pair, &members, false), Some(80.0));
    }

    #[test]
    fn the_total_keeps_an_earlier_sum_when_a_disk_leaves() {
        let mut memory = RateMemory::default();
        let mut sda = disk_row("sda", 10, 4, 0.0).disk;
        let mut sdb = disk_row("sdb", 10, 4, 0.0).disk;
        let first = parse::IoSnapshot {
            disks: vec![sda.clone(), sdb.clone()],
            ..parse::IoSnapshot::default()
        };
        let board = board_from(&first, &mut memory);
        assert_eq!(board.total_read, vec![None]);
        assert_eq!(
            board.total_names,
            vec!["sda".to_string(), "sdb".to_string()]
        );
        memory.at = Some(Instant::now() - Duration::from_secs(1));
        if let Some(stat) = sda.stat.as_mut() {
            stat.sectors_read += 2;
        }
        if let Some(stat) = sdb.stat.as_mut() {
            stat.sectors_read += 4;
        }
        let second = parse::IoSnapshot {
            disks: vec![sda.clone(), sdb],
            ..parse::IoSnapshot::default()
        };
        let board = board_from(&second, &mut memory);
        let sda_rate = board.rows[0].read.last().copied().flatten().unwrap();
        let sdb_rate = board.rows[1].read.last().copied().flatten().unwrap();
        let summed = board.total_read.last().copied().flatten().unwrap();
        assert_eq!(summed, sda_rate + sdb_rate);
        assert!(sda_rate > 0.0 && sdb_rate > 0.0 && sdb_rate > sda_rate);
        memory.at = Some(Instant::now() - Duration::from_secs(1));
        if let Some(stat) = sda.stat.as_mut() {
            stat.sectors_read += 2;
        }
        let third = parse::IoSnapshot {
            disks: vec![sda],
            ..parse::IoSnapshot::default()
        };
        let board = board_from(&third, &mut memory);
        assert_eq!(board.total_read[1], Some(summed));
        assert_eq!(
            board.total_read[2],
            board.rows[0].read.last().copied().flatten()
        );
        assert_eq!(board.total_names, vec!["sda".to_string()]);
    }

    #[test]
    fn one_chart_sits_below_the_disk_table() {
        // The total and the cursor's disk used to be two charts, and with
        // one busy disk they drew the same line twice.
        let mut board = board(vec![disk_row("sda", 1, 1, 8.0), disk_row("sdb", 1, 1, 8.0)]);
        board.total_read = vec![Some(1000.0), Some(2000.0)];
        board.total_write = vec![Some(100.0), Some(200.0)];
        board.total_names = vec!["sda".into(), "sdb".into()];
        let p = palette();
        let mut frame = view(None, "");
        frame.board = Some(&board);
        frame.disk_cursor = "sda";
        for focus in [0usize, 1] {
            frame.focus = focus;
            for width in [28usize, 80] {
                let drawn = draw_body(&frame, width, &p, None);
                for line in &drawn.rows {
                    assert!(
                        tc::display_width(line) <= width,
                        "width {width} drew {} cells",
                        tc::display_width(line)
                    );
                }
                let lines: Vec<String> = drawn.rows.iter().map(|line| plain(line)).collect();
                let header_at = lines
                    .iter()
                    .position(|line| line.contains("READ"))
                    .unwrap_or_else(|| panic!("{width} missing the disk header"));
                let charts: Vec<usize> = lines
                    .iter()
                    .enumerate()
                    .filter(|(_, line)| line.contains("read") && line.contains("peak"))
                    .map(|(at, _)| at)
                    .collect();
                assert_eq!(charts.len(), 1, "focus {focus} width {width}: {lines:?}");
                assert!(charts[0] > header_at, "focus {focus} width {width}");
            }
            let drawn = draw_body(&frame, 100, &p, None);
            assert_eq!(drawn.plots.len(), 2, "focus {focus}");
        }
    }

    #[test]
    fn hiding_every_disk_row_keeps_the_total_chart() {
        let mut board = board(vec![disk_row("sda", 1, 1, 8.0), disk_row("sdb", 1, 1, 8.0)]);
        board.total_read = vec![Some(1000.0), Some(2000.0)];
        board.total_write = vec![Some(100.0), Some(200.0)];
        board.total_names = vec!["sda".into(), "sdb".into()];
        let hidden = vec!["sda".to_string(), "sdb".to_string()];
        let p = palette();
        let mut frame = view(None, "");
        frame.board = Some(&board);
        frame.disk_cursor = "sda";
        frame.hide_devices = &hidden;
        frame.focus = 1;
        let drawn = draw_body(&frame, 100, &p, None);
        assert!(!plain_has(&drawn, "READ"));
        assert!(plain_has(&drawn, "total of sda, sdb"));
        assert_eq!(drawn.plots.len(), 2);
    }

    #[test]
    fn the_chart_is_the_total_until_the_disk_section_has_focus() {
        let mut board = board(vec![disk_row("sda", 1, 1, 8.0), disk_row("sdb", 1, 1, 8.0)]);
        board.total_read = vec![Some(1000.0), Some(2000.0)];
        board.total_write = vec![Some(100.0), Some(200.0)];
        board.total_names = vec!["sda".into(), "sdb".into()];
        board.rows[0].read = vec![Some(10.0), Some(40.0)];
        board.rows[0].write = vec![Some(5.0), Some(20.0)];
        let p = palette();
        // Read grows up from zero, write grows down, each on its own peak.
        let traces = |drawn: &Drawn| -> Vec<Vec<Option<f64>>> {
            drawn
                .plots
                .iter()
                .map(|plot| plot.chart.traces[0].values.clone())
                .collect()
        };
        let mut frame = view(None, "");
        frame.board = Some(&board);
        frame.disk_cursor = "sda";
        for focus in [0usize, 2] {
            frame.focus = focus;
            let drawn = draw_body(&frame, 100, &p, None);
            assert!(plain_has(&drawn, "total of sda, sdb"), "focus {focus}");
            assert_eq!(
                traces(&drawn),
                vec![vec![Some(0.5), Some(1.0)], vec![Some(0.5), Some(0.0)]],
                "focus {focus}"
            );
        }
        frame.focus = 1;
        let drawn = draw_body(&frame, 100, &p, None);
        assert!(!plain_has(&drawn, "total of"));
        assert_eq!(
            traces(&drawn),
            vec![vec![Some(0.25), Some(1.0)], vec![Some(0.75), Some(0.0)]]
        );
        let lines: Vec<String> = drawn.rows.iter().map(|line| plain(line)).collect();
        let chart_at = lines
            .iter()
            .position(|line| line.contains("read") && line.contains("peak"))
            .unwrap();
        assert_eq!(lines[chart_at - 1].trim(), "sda");
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
    #[test]
    fn a_mount_point_too_wide_to_sit_beside_its_figures_gets_its_own_line() {
        let long = "/home/example/GoogleDrive";
        let rows = vec![
            row("/", "ext4", Figures::Ready(space(80, 20, 100, 90)), 0),
            row(long, "fuse.rclone", Figures::Ready(space(40, 60, 0, 0)), 1),
            row(
                "/run/user/1000/doc",
                "fuse.portal",
                Figures::Failed("statvfs: Operation not permitted (os error 1)".into()),
                2,
            ),
        ];
        let snap = snap(rows, Hidden::default());
        let p = palette();
        for width in [28usize, 40, 60, 80, 100, 160] {
            let cols = layout_for(width, long.len());
            let drawn = draw_body(&view(Some(&snap), long), width, &p, None);
            let lines: Vec<String> = drawn.rows.iter().map(|line| plain(line)).collect();
            for line in &drawn.rows {
                assert!(tc::display_width(line) <= width, "{width}: {line:?}");
            }
            if cols.stacked {
                assert!(
                    lines
                        .iter()
                        .any(|line| line.trim_end() == format!("▸ {long}")),
                    "{width}: {lines:?}"
                );
                assert!(
                    lines.iter().any(|line| line.trim_end() == "MOUNT"),
                    "{width}"
                );
                // The error is a sentence, so it wraps rather than being cut.
                let all = lines.join(" ");
                assert!(all.contains("Operation not"), "{width}");
                assert!(all.contains("error 1)"), "{width}");
            } else {
                let at = lines
                    .iter()
                    .position(|line| line.starts_with(&format!("▸ {long}")))
                    .unwrap_or_else(|| panic!("{width}: {lines:?}"));
                assert!(lines[at].contains('%'), "{width}: figures beside the mount");
            }
            // Every line of the selected mount is that mount's item.
            let item = drawn
                .placed
                .iter()
                .find(|(row, _)| lines[*row].starts_with("▸ "))
                .map(|(_, item)| *item)
                .unwrap();
            let rows_of = drawn.placed.iter().filter(|(_, at)| *at == item).count();
            assert!(rows_of >= if cols.stacked { 2 } else { 1 }, "{width}");
        }
        assert!(layout_for(60, long.len()).stacked);
        assert!(!layout_for(160, long.len()).stacked);
        // Short mount points stay beside their figures once the figures fit.
        assert!(!layout_for(80, 2).stacked);
        assert!(layout_for(40, 2).stacked);
    }

    #[test]
    fn a_wide_pane_keeps_each_name_beside_its_figures() {
        // At 400 columns the name columns used to take every spare cell,
        // which put / at the left edge and its figures at the right one.
        let rows = vec![
            row("/", "ext4", Figures::Ready(space(80, 20, 100, 90)), 0),
            row("/boot/efi", "vfat", Figures::Ready(space(10, 90, 0, 0)), 1),
            row("/mnt/slow", "nfs4", Figures::NoAnswer, 2),
        ];
        let snap = snap(rows, Hidden::default());
        let board = board(vec![
            disk_row("sda", 1, 1, 8.0),
            disk_row("loop10", 1, 1, 8.0),
        ]);
        let p = palette();
        let mut frame = view(Some(&snap), "/");
        frame.board = Some(&board);
        for width in [160usize, 400] {
            let cols = layout_for(width, "/boot/efi".len());
            assert_eq!(cols.mount, "/boot/efi".len() + 2, "{width}");
            assert!(cols.bar <= BAR_WIDE, "{width}");
            assert_eq!(
                disk_columns(width, "loop10".len()).disk,
                DISK_MIN,
                "{width}"
            );
            let drawn = draw_body(&frame, width, &p, None);
            let lines: Vec<String> = drawn.rows.iter().map(|line| plain(line)).collect();
            for line in &drawn.rows {
                assert!(tc::display_width(line) <= width, "{width}: {line:?}");
            }
            // The first figure follows the name after the column's own
            // padding, not after the rest of the pane.
            for name in ["/boot/efi", "/mnt/slow", "sda", "loop10"] {
                let line = lines
                    .iter()
                    .find(|line| line.trim_start_matches(['▸', ' ']).starts_with(name))
                    .unwrap_or_else(|| panic!("{width}: no row for {name}: {lines:?}"));
                let after = &line[line.find(name).unwrap() + name.len()..];
                let gap = after.len() - after.trim_start().len();
                assert!(
                    gap <= 12,
                    "{width}: {gap} blank cells after {name}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn the_window_is_hours_and_tops_out_at_a_day() {
        assert_eq!(window_hours_of(&serde_json::json!({})), 24);
        assert_eq!(
            window_hours_of(&serde_json::json!({ "window_hours": 6 })),
            6
        );
        // The old day windows, or any value not on offer, start on a day.
        assert_eq!(
            window_hours_of(&serde_json::json!({ "window_hours": 7 })),
            24
        );
        assert_eq!(
            window_hours_of(&serde_json::json!({ "window_hours": 48 })),
            24
        );
        assert_eq!(tc::cycle(&WINDOWS, 24), 6);
    }

    #[test]
    fn only_screens_that_use_the_window_offer_w() {
        let p = palette();
        let has_w = |screen| {
            let (packed, _) = footer(
                120,
                screen,
                Sort::Mount,
                DiskSort::Write,
                1,
                false,
                false,
                24,
                false,
                &no_writer_hints(),
                &p,
            );
            packed.lines.join("\n").contains("[w]")
        };
        assert!(has_w(Screen::List));
        assert!(has_w(Screen::Detail));
        assert!(!has_w(Screen::DiskDetail));
        assert!(!has_w(Screen::Service));
        assert!(!has_w(Screen::Process));
        assert!(!has_w(Screen::Waiters));
    }

    // ── WHO IS WRITING ───────────────────────────────────────────────

    const MIB: u64 = 1 << 20;

    fn cg(path: &str, io: bool, wbytes: Option<u64>, procs: Vec<i32>) -> tc::cgroup::Node {
        tc::cgroup::Node {
            path: path.into(),
            enabled: if io {
                vec!["io".into()]
            } else {
                vec!["memory".into()]
            },
            io: wbytes.map(|w| {
                vec![tc::cgroup::IoStat {
                    dev: "8:0".into(),
                    wbytes: w,
                    rbytes: w / 4,
                    ..tc::cgroup::IoStat::default()
                }]
            }),
            cpu: None,
            memory: None,
            procs: Some(procs),
        }
    }

    /// Ten services under system.slice, each writing (i + 1) MiB over the
    /// interval, user.slice 2 MiB, and the root 1 MiB of its own: 58 MiB.
    fn cgroup_tree(step: u64) -> tc::cgroup::Tree {
        let services: Vec<u64> = (0..10).map(|i| (i + 1) * MIB * step).collect();
        let sys: u64 = services.iter().sum();
        let user = 2 * MIB * step;
        let mut nodes = vec![
            cg("", true, Some(sys + user + MIB * step), vec![1]),
            cg("system.slice", true, Some(sys), vec![]),
            cg("user.slice", true, Some(user), vec![]),
            cg(
                "user.slice/session-1.scope",
                false,
                None,
                (500..509).collect(),
            ),
        ];
        for (i, w) in services.iter().enumerate() {
            nodes.push(cg(
                &format!("system.slice/example-svc-{i}.service"),
                true,
                Some(*w),
                vec![100 + i as i32],
            ));
        }
        tc::cgroup::Tree { nodes }
    }

    /// Seven processes in D in a session scope, two kernel threads in D, a
    /// sleeping one; half of them readable.
    fn pid_samples(ticks: u64) -> Vec<writers::PidSample> {
        let mut out = Vec::new();
        for pid in 500..507 {
            out.push(writers::PidSample {
                pid,
                comm: "example-sync".into(),
                state: 'D',
                ppid: 1,
                starttime: 9,
                blkio_ticks: Some(ticks * (pid as u64 - 499)),
                io: (pid % 2 == 0).then(tc::procfs::PidIo::default),
                cgroup: None,
            });
        }
        for pid in [40, 41] {
            out.push(writers::PidSample {
                pid,
                comm: "kworker/u8:0+flush-8:0".into(),
                state: 'D',
                ppid: 2,
                starttime: 1,
                blkio_ticks: Some(0),
                io: None,
                cgroup: None,
            });
        }
        out.push(writers::PidSample {
            pid: 100,
            comm: "example-db".into(),
            state: 'S',
            ppid: 1,
            starttime: 3,
            blkio_ticks: Some(0),
            io: None,
            cgroup: None,
        });
        out
    }

    fn writers_fixture(delayacct: bool) -> writers::Writers {
        let mut memory = writers::Memory::default();
        let _ = writers::step(
            &mut memory,
            tc::cgroup::State::Ok(cgroup_tree(1)),
            pid_samples(10),
            String::new(),
            delayacct,
            0.0,
            100.0,
        );
        writers::step(
            &mut memory,
            tc::cgroup::State::Ok(cgroup_tree(2)),
            pid_samples(40),
            String::new(),
            delayacct,
            2.0,
            100.0,
        )
    }

    fn writing_board(write_bps: f64) -> DiskBoard {
        board(vec![disk_row("sda", 10, 10, write_bps)])
    }

    fn rows_fit(drawn: &Drawn, width: usize) {
        for line in &drawn.rows {
            assert!(
                tc::display_width(&plain(line)) <= width,
                "row wider than {width}: {:?}",
                plain(line)
            );
        }
    }

    fn waiting_header(drawn: &Drawn) -> String {
        drawn
            .rows
            .iter()
            .map(|line| plain(line))
            .find(|line| line.starts_with("  PID"))
            .unwrap_or_default()
    }

    #[test]
    fn who_is_writing_fits_every_width_and_cgroup_leaves_first() {
        let p = palette();
        let board = writing_board(29.0 * MIB as f64);
        let writers = writers_fixture(true);
        for width in [28, 40, 60, 80, 100, 140] {
            let mut frame = view(None, "/");
            frame.board = Some(&board);
            frame.writers = Some(&writers);
            frame.focus = 3;
            frame.svc_cursor = "system.slice/example-svc-9.service";
            let drawn = draw_body(&frame, width, &p, None);
            rows_fit(&drawn, width);
            assert!(plain_has(&drawn, "WHO IS WRITING"), "{width}");
            assert!(plain_has(&drawn, "+2 more  [↵] list all"), "{width}");
            assert!(plain_has(&drawn, "accounted:"), "{width}");
            let header = waiting_header(&drawn);
            assert!(header.contains("WAITED"), "{width}: {header}");
            if width >= 100 {
                assert!(header.contains("CGROUP"), "{width}: {header}");
                assert!(header.contains("WAITED (last 2s)"), "{width}: {header}");
            }
            if width <= 40 {
                assert!(
                    !header.contains("CGROUP"),
                    "CGROUP drops first at {width}: {header}"
                );
            }
            // Eight services, five waiters and the `+N more` row.
            assert_eq!(drawn.lens[3], 8, "{width}");
            assert_eq!(drawn.lens[WAITING], 6, "{width}");
            assert_eq!(drawn.svc_total, 12, "{width}");
        }
    }

    #[test]
    fn the_cgroup_rows_tie_back_to_the_device_and_the_root_is_a_row() {
        let p = palette();
        // 58 MiB over 2 s is 29 MiB/s; the device saw 32.
        let board = writing_board(32.0 * MIB as f64);
        let writers = writers_fixture(false);
        let mut frame = view(None, "/");
        frame.board = Some(&board);
        frame.writers = Some(&writers);
        frame.svc_all = true;
        let drawn = draw_body(&frame, 120, &p, None);
        assert!(plain_has(
            &drawn,
            "WHO IS WRITING  sda · last 2s · 32M/s written"
        ));
        assert!(plain_has(
            &drawn,
            "accounted: 29M/s of 32M/s · the rest, 3M/s, is writeback on filesystems without cgroup writeback, or root's own"
        ));
        assert!(
            drawn.rows.iter().any(|l| plain(l).starts_with("  /  ")),
            "the root's own I/O is a row"
        );
        assert!(plain_has(&drawn, "user.slice"));
        assert!(
            plain_has(&drawn, "9 · 4 readable"),
            "the session scope's pids roll up"
        );
        assert_eq!(drawn.lens[3], 12, "[a] shows every cgroup");
        // Above the device by more than the two reads explain.
        let board = writing_board(20.0 * MIB as f64);
        frame.board = Some(&board);
        let drawn = draw_body(&frame, 120, &p, None);
        assert!(plain_has(
            &drawn,
            "cgroups report 29M/s against 20M/s at the device · dirtying runs ahead of submission"
        ));
    }

    #[test]
    fn the_top_n_cut_is_named_and_the_hints_name_the_next_state() {
        let p = palette();
        let board = writing_board(29.0 * MIB as f64);
        let writers = writers_fixture(true);
        let mut frame = view(None, "/");
        frame.board = Some(&board);
        frame.writers = Some(&writers);
        let drawn = draw_body(&frame, 120, &p, None);
        assert!(plain_has(&drawn, "8 of 12 shown · top 8 by write"));
        let hints = WriterHints {
            all: Some(writers::all_hint(false, 8)),
            svc_sort: DiskSort::Write,
            proc_sort: DiskSort::Write,
            wait_sort: Some(writers::WaitSort::Waited),
        };
        let text = |focus| {
            let (packed, _) = footer(
                120,
                Screen::List,
                Sort::Mount,
                DiskSort::Write,
                focus,
                false,
                false,
                24,
                false,
                &hints,
                &p,
            );
            packed.lines.join("\n")
        };
        assert!(text(3).contains("[s] sort by read") && text(3).contains("[a] show all cgroups"));
        assert!(text(WAITING).contains("[s] sort by pid"));
        let off = WriterHints {
            wait_sort: None,
            ..hints
        };
        let (packed, _) = footer(
            120,
            Screen::List,
            Sort::Mount,
            DiskSort::Write,
            WAITING,
            false,
            false,
            24,
            false,
            &off,
            &p,
        );
        assert!(
            !packed.lines.join("\n").contains("[s]"),
            "pid order is all there is with wait times off"
        );
        assert_eq!(
            tc::next_section(Some(2), &drawn.lens),
            Some(3),
            "tab reaches the section"
        );
    }

    #[test]
    fn wait_times_off_drop_the_column_and_say_what_root_would_add() {
        let p = palette();
        let board = writing_board(29.0 * MIB as f64);
        let writers = writers_fixture(false);
        let mut frame = view(None, "/");
        frame.board = Some(&board);
        frame.writers = Some(&writers);
        let drawn = draw_body(&frame, 200, &p, None);
        assert!(plain_has(
            &drawn,
            "WAITING ON DISK  9 in D state now · wait times off (needs root: sysctl kernel.task_delayacct=1)"
        ));
        assert!(plain_has(&drawn, "2 kernel threads hidden"));
        assert!(!waiting_header(&drawn).contains("WAITED"));
        assert!(
            !drawn.rows.iter().any(|l| plain(l).contains("0.0s")),
            "never a column of zeros"
        );
        // Pid order, as a sample.
        let first = drawn
            .rows
            .iter()
            .map(|l| plain(l))
            .find(|l| l.contains("example-sync"))
            .unwrap();
        assert!(first.contains("500"), "{first}");
        // Every pid shows its cgroup, readable or not.
        let shown = drawn
            .rows
            .iter()
            .filter(|l| plain(l).contains("user.slice/session-1.scope"))
            .count();
        assert_eq!(shown, 5);
    }

    #[test]
    fn wait_times_on_rank_by_the_field_42_delta() {
        let p = palette();
        let board = writing_board(29.0 * MIB as f64);
        let writers = writers_fixture(true);
        let mut frame = view(None, "/");
        frame.board = Some(&board);
        frame.writers = Some(&writers);
        let drawn = draw_body(&frame, 140, &p, None);
        assert!(plain_has(
            &drawn,
            "wait times on (task_delayacct=1) · by time waited"
        ));
        // pid 506: 7 × (40 - 10) ticks at 100 Hz over 2 s.
        let first = drawn
            .rows
            .iter()
            .map(|l| plain(l))
            .find(|l| l.contains("example-sync"))
            .unwrap();
        assert!(
            first.contains("506") && first.contains("2.1s of 2s"),
            "{first}"
        );
    }

    #[test]
    fn more_lists_everyone_and_a_row_opens_its_process() {
        let p = palette();
        let board = writing_board(29.0 * MIB as f64);
        let writers = writers_fixture(true);
        let mut frame = view(None, "/");
        frame.board = Some(&board);
        frame.writers = Some(&writers);
        frame.screen = Screen::Waiters;
        frame.show_kernel = true;
        let drawn = draw_body(&frame, 100, &p, None);
        let listed = drawn
            .rows
            .iter()
            .filter(|l| plain(l).contains("example-sync") || plain(l).contains("kworker"))
            .count();
        assert_eq!(listed, 9, "everyone in D, kernel threads shown when asked");
        assert_eq!(drawn.lens, [0; SECTIONS]);
        let open = writers::Opened {
            pid: 501,
            comm: "example-sync".into(),
            cgroup: "user.slice/session-1.scope".into(),
            cmdline: "example-sync --flush".into(),
            cwd: String::new(),
            from_wait: true,
            start: None,
        };
        frame.screen = Screen::Process;
        frame.opened = Some(&open);
        let drawn = draw_body(&frame, 60, &p, None);
        rows_fit(&drawn, 60);
        assert!(plain_has(&drawn, "cgroup user.slice/session-1.scope"));
        assert!(plain_has(&drawn, "cwd not readable as you"));
        assert!(plain_has(&drawn, "the rest needs root"));
        // The same pid, started again since it was opened: a stranger.
        let start = writers.starts.get(&501).copied().expect("start of 501");
        let reused = writers::Opened {
            pid: 501,
            comm: "example-sync".into(),
            from_wait: true,
            start: Some(start + 1),
            ..writers::Opened::default()
        };
        frame.opened = Some(&reused);
        let drawn = draw_body(&frame, 60, &p, None);
        assert!(plain_has(&drawn, "exited since it was in D"));
        assert!(!plain_has(&drawn, "waiting on disk now"));
        frame.opened = Some(&open);
        frame.screen = Screen::Service;
        frame.service_path = "user.slice";
        let drawn = draw_body(&frame, 60, &p, None);
        rows_fit(&drawn, 60);
        assert!(plain_has(&drawn, "cgroup /user.slice"));
        assert!(plain_has(&drawn, "controllers enabled here io"));
        assert!(plain_has(&drawn, "1 cgroup below without it rolls up"));
        assert!(plain_has(&drawn, "cgroup.procs 9 · 4 readable as you"));
        assert!(plain_has(&drawn, "501  example-sync  not readable"));
    }

    #[test]
    fn named_states_replace_the_table_and_an_idle_disk_still_reconciles() {
        let p = palette();
        let board = writing_board(0.0);
        let mut memory = writers::Memory::default();
        let flat = tc::cgroup::Tree {
            nodes: vec![
                cg("", true, Some(0), vec![]),
                cg("system.slice", true, Some(0), vec![]),
            ],
        };
        let _ = writers::step(
            &mut memory,
            tc::cgroup::State::Ok(flat.clone()),
            vec![],
            String::new(),
            false,
            0.0,
            100.0,
        );
        let idle = writers::step(
            &mut memory,
            tc::cgroup::State::Ok(flat),
            vec![],
            String::new(),
            false,
            2.0,
            100.0,
        );
        let mut frame = view(None, "/");
        frame.board = Some(&board);
        frame.writers = Some(&idle);
        let drawn = draw_body(&frame, 80, &p, None);
        assert!(
            plain_has(&drawn, "CGROUP"),
            "the header stays over an idle disk"
        );
        assert!(plain_has(&drawn, "accounted: 0B/s of 0B/s"));
        assert!(plain_has(&drawn, "WAITING ON DISK  none in D state now"));
        for (state, says) in [
            (tc::cgroup::State::V1, "by service needs cgroup v2"),
            (
                tc::cgroup::State::NotEnabled("io".into()),
                "io accounting is off everywhere · systemctl set-property <unit> IOAccounting=yes enables it per unit",
            ),
        ] {
            let w = writers::step(&mut memory, state, vec![], String::new(), false, 4.0, 100.0);
            let mut named = view(None, "/");
            named.board = Some(&board);
            named.writers = Some(&w);
            let drawn = draw_body(&named, 140, &p, None);
            assert!(plain_has(&drawn, says), "{says}");
            assert!(!plain_has(&drawn, "accounted"));
        }
        let mut memory = writers::Memory::default();
        let _ = writers::step(
            &mut memory,
            tc::cgroup::State::Container(cgroup_tree(1)),
            vec![],
            String::new(),
            false,
            0.0,
            100.0,
        );
        let inside = writers::step(
            &mut memory,
            tc::cgroup::State::Container(cgroup_tree(2)),
            vec![],
            String::new(),
            false,
            2.0,
            100.0,
        );
        frame.writers = Some(&inside);
        let drawn = draw_body(&frame, 200, &p, None);
        assert!(plain_has(
            &drawn,
            "rows are this container's cgroups; the device total is the host's"
        ));
    }

    #[test]
    fn without_a_source_the_section_is_not_drawn() {
        // A Mac never receives a Writers: no heading, no empty table.
        let p = palette();
        let board = writing_board(1.0);
        let mut frame = view(None, "/");
        frame.board = Some(&board);
        let drawn = draw_body(&frame, 100, &p, None);
        assert!(!plain_has(&drawn, "WHO IS WRITING"));
        assert_eq!(&drawn.lens[3..], &[0, 0, 0]);
    }

    #[test]
    fn a_click_id_names_its_section_and_row() {
        let mut at = [0usize; SECTIONS];
        for (section, row) in [(0, 3), (1, 2), (2, 0), (3, 7), (4, 4), (5, 5)] {
            at[section] = row;
            assert_eq!(section_of(item_id(section, &at)), (section, row));
        }
    }

    /// Twelve readable workers writing 1..12 MiB/s, the even session pids
    /// readable and idle, and three processes that need root.
    fn procs_fixture(source: by_process::Source) -> by_process::Procs {
        let mut rows: Vec<by_process::ProcRate> = (0..12)
            .map(|i| by_process::ProcRate {
                pid: 600 + i,
                comm: format!("example-worker-{i}"),
                cgroup: "user.slice".into(),
                read_bps: Some(0.0),
                write_bps: Some(((i + 1) as u64 * MIB) as f64),
                read_total: 0,
                write_total: 100 * MIB,
            })
            .collect();
        for pid in [500, 502, 504, 506] {
            rows.push(by_process::ProcRate {
                pid,
                comm: "example-sync".into(),
                cgroup: "user.slice".into(),
                read_bps: Some(0.0),
                write_bps: Some(if pid == 502 { 2.0 * MIB as f64 } else { 0.0 }),
                read_total: 0,
                write_total: 0,
            });
        }
        let locked = (0..3)
            .map(|i| by_process::Locked {
                pid: 100 + i,
                comm: "example-db".into(),
                cgroup: "system.slice/example-db.service".into(),
            })
            .collect();
        let mut ring = by_process::Ring::new(60.0);
        for _ in 0..30 {
            ring.push(2.0, 30.0 * MIB as f64, 376.0 * MIB as f64);
        }
        by_process::Procs {
            source,
            rows,
            locked,
            elapsed: Some(2.0),
            root: false,
            visible: false,
            moved: Some(0.0),
            window: ring.window(),
        }
    }

    fn writers_with_procs() -> writers::Writers {
        let mut w = writers_fixture(true);
        w.procs = Some(procs_fixture(by_process::Source::Procfs));
        w
    }

    #[test]
    fn by_process_fits_every_width_and_cgroup_leaves_first() {
        let p = palette();
        let board = writing_board(29.0 * MIB as f64);
        let writers = writers_with_procs();
        for width in [28, 40, 60, 80, 100, 140] {
            let mut frame = view(None, "/");
            frame.board = Some(&board);
            frame.writers = Some(&writers);
            frame.focus = BY_PROCESS;
            frame.proc_cursor = "611";
            let drawn = draw_body(&frame, width, &p, None);
            rows_fit(&drawn, width);
            let header = drawn
                .rows
                .iter()
                .map(|l| plain(l))
                .find(|l| l.starts_with("  PID") && l.contains("WRITE"))
                .unwrap_or_default();
            assert!(header.contains("COMMAND"), "{width}: {header}");
            if width >= 80 {
                assert!(header.contains("CGROUP"), "{width}: {header}");
                assert!(header.contains("READ"), "{width}: {header}");
            }
            if width <= 40 {
                assert!(!header.contains("CGROUP"), "CGROUP drops first at {width}");
            }
            assert_eq!(drawn.lens[BY_PROCESS], 8, "top 8 at {width}");
            assert_eq!(drawn.proc_keys[0], "611", "the busiest leads at {width}");
            assert_eq!(drawn.proc_total, 19);
            let text: String = drawn
                .rows
                .iter()
                .map(|l| plain(l))
                .collect::<Vec<_>>()
                .join(" ");
            assert!(text.contains("readable (yours)"), "{width}");
            assert!(text.contains('≈'), "{width}");
        }
    }

    #[test]
    fn by_process_states_its_coverage_and_reconciles_over_the_window() {
        let p = palette();
        let board = writing_board(29.0 * MIB as f64);
        let writers = writers_with_procs();
        let mut frame = view(None, "/");
        frame.board = Some(&board);
        frame.writers = Some(&writers);
        let drawn = draw_body(&frame, 240, &p, None);
        assert!(plain_has(
            &drawn,
            "BY PROCESS  /proc/<pid>/io · every disk · sorted: write · 16 of 19 processes readable (yours) · the other 3 need root"
        ));
        assert!(plain_has(
            &drawn,
            "top 8 shown · 13 of 16 readable moved bytes over the last 2s"
        ));
        assert!(plain_has(
            &drawn,
            "readable processes account for ≈ 15M/s of 188M/s (8%)"
        ));
        assert!(plain_has(&drawn, "write_bytes counts at dirtying, devices at submission"));
        // `a`: every readable process, then the ones that need root, each
        // still with its cgroup.
        frame.proc_all = true;
        let drawn = draw_body(&frame, 140, &p, None);
        rows_fit(&drawn, 140);
        assert_eq!(drawn.lens[BY_PROCESS], 19);
        assert!(plain_has(&drawn, "3 need root · their I/O is not readable as you"));
        assert!(drawn.rows.iter().any(|l| {
            let l = plain(l);
            l.contains("100") && l.contains("example-db") && l.contains("system.slice/example-db.service")
        }));
        let hints = WriterHints {
            all: Some(by_process::all_hint(false, 8)),
            svc_sort: DiskSort::Write,
            proc_sort: DiskSort::Read,
            wait_sort: None,
        };
        let (packed, _) = footer(
            140,
            Screen::List,
            Sort::Mount,
            DiskSort::Write,
            BY_PROCESS,
            false,
            false,
            24,
            false,
            &hints,
            &p,
        );
        let text = packed.lines.join("\n");
        assert!(text.contains("[s] sort by name") && text.contains("[a] show all processes"));
    }

    #[test]
    fn the_process_detail_shows_file_growth_and_the_service_detail_rates() {
        let p = palette();
        let board = writing_board(29.0 * MIB as f64);
        let mut writers = writers_with_procs();
        let mut memory = None;
        let file = |size| tc::procfs::OpenFile {
            path: "/tmp/example/out.bin".into(),
            size,
        };
        let _ = by_process::file_growth(&mut memory, 611, &[file(MIB)], 10.0);
        let files = by_process::file_growth(&mut memory, 611, &[file(13 * MIB)], 12.0);
        writers.names.insert(611, "example-worker-11".into());
        writers.readable.insert(611);
        writers.detail = Some(by_process::ProcDetail {
            pid: 611,
            state: Some('R'),
            cmdline: "example-worker --out /tmp/example/out.bin".into(),
            cwd: "/tmp/example".into(),
            files: Some(Ok(files)),
        });
        let open = writers::Opened {
            pid: 611,
            comm: "example-worker-11".into(),
            cgroup: "user.slice".into(),
            ..writers::Opened::default()
        };
        let mut frame = view(None, "/");
        frame.board = Some(&board);
        frame.writers = Some(&writers);
        frame.screen = Screen::Process;
        frame.opened = Some(&open);
        for width in [28, 40, 60, 80, 100, 140] {
            let drawn = draw_body(&frame, width, &p, None);
            rows_fit(&drawn, width);
        }
        let drawn = draw_body(&frame, 100, &p, None);
        assert!(plain_has(&drawn, "state R"));
        assert!(plain_has(&drawn, "cmdline example-worker --out /tmp/example/out.bin"));
        assert!(plain_has(&drawn, "now write 12M/s · read 0B/s over the last 2s"));
        assert!(plain_has(&drawn, "written 100M · read 0B since it started"));
        assert!(plain_has(&drawn, "OPEN FILES  1 · size and growth since the previous sample"));
        assert!(plain_has(&drawn, "+12M in 2s"));
        // The service detail: per-process rates for the pids it can read.
        frame.screen = Screen::Service;
        frame.service_path = "user.slice";
        let drawn = draw_body(&frame, 100, &p, None);
        rows_fit(&drawn, 100);
        assert!(plain_has(&drawn, "502  example-sync  write 2M/s · read 0B/s"));
        assert!(plain_has(&drawn, "501  example-sync  not readable"));
    }

    #[test]
    fn on_macos_by_process_is_the_whole_section() {
        let p = palette();
        let board = writing_board(29.0 * MIB as f64);
        let writers = writers::by_process_only(
            procs_fixture(by_process::Source::Rusage),
            HashMap::new(),
            String::new(),
            None,
        );
        for width in [28, 40, 60, 80, 100, 140] {
            let mut frame = view(None, "/");
            frame.board = Some(&board);
            frame.writers = Some(&writers);
            let drawn = draw_body(&frame, width, &p, None);
            rows_fit(&drawn, width);
            assert!(!plain_has(&drawn, "BY SERVICE"), "{width}");
            assert!(!plain_has(&drawn, "WAITING ON DISK"), "{width}");
            assert!(!plain_has(&drawn, "CGROUP"), "no cgroups on a Mac: {width}");
            assert_eq!(drawn.lens[BY_SERVICE], 0);
            assert_eq!(drawn.lens[WAITING], 0);
            assert_eq!(drawn.lens[BY_PROCESS], 8);
        }
        let mut frame = view(None, "/");
        frame.board = Some(&board);
        frame.writers = Some(&writers);
        let drawn = draw_body(&frame, 200, &p, None);
        assert!(plain_has(&drawn, "WHO IS WRITING  by process · proc_pid_rusage"));
        assert!(plain_has(
            &drawn,
            "every disk · sorted: write · 16 of 19 processes readable (yours) · the other 3 need root"
        ));
        assert!(plain_has(&drawn, "≈ 15M/s of 188M/s combined (8%)"));
        let open = writers::Opened {
            pid: 611,
            comm: "example-worker-11".into(),
            ..writers::Opened::default()
        };
        frame.screen = Screen::Process;
        frame.opened = Some(&open);
        let drawn = draw_body(&frame, 100, &p, None);
        assert!(plain_has(&drawn, "cmdline, cwd and open files are not read on macOS"));
        assert!(plain_has(&drawn, "(bytes handed to the disk layer)"));
        assert!(!plain_has(&drawn, "cgroup"));
    }
}
