// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! What this machine is running: every process, what it costs in CPU and
//! memory, its state and how long it has been up, sorted, filtered, and
//! opened one at a time.
//!
//! Linux reads `/proc` through `tc::procfs` and measures %CPU as the delta
//! of `utime + stime` over the poll, as a share of every core. macOS reads
//! one `ps -axo` a poll, whose `%cpu` is a decaying average and is labelled
//! as one. The parsers, the sort, the filter and the drawing compile and
//! are tested on every target; only the reads are behind `cfg(target_os)`.
//!
//!     procs [-n SECONDS]
//!
//! Keys: up/down select, enter opens, s cycles the sort, / filters, u
//! switches between all users and yours, r refreshes, comma opens
//! settings, q quits.

use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use opscope_core as tc;

mod model;
mod parse;
mod view;

use model::{Facts, Proc, Sample, Sort};
use view::{Opened, Screen};

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod host;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod host;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod host {
    use super::model::{Facts, Proc, Sample};

    #[derive(Default)]
    pub struct Source;

    pub fn sample(_source: &mut Source) -> Result<Sample, String> {
        Err(opscope_core::unsupported())
    }

    pub fn facts(_source: &mut Source, proc: &Proc) -> Facts {
        Facts {
            pid: proc.pid,
            start: proc.start,
            cmdline: Err(opscope_core::unsupported()),
            cwd: Err(opscope_core::unsupported()),
            gid: None,
            io: super::model::Io::NotRead,
        }
    }

    pub fn forget(_source: &mut Source) {}
}

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "procs",
    section: "procs",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};

/// What the poller hands the frame.
#[derive(Default)]
struct Shared {
    sample: Option<Arc<Sample>>,
    /// Why the last poll has no table, or why the poller stopped. Drawn
    /// above the table, which stays at its last good reading.
    err: String,
    sampled: Option<Instant>,
    /// The open process's facts.
    facts: Option<Facts>,
}

/// What the frame asks of the poller between polls.
#[derive(Default)]
struct Asked {
    /// `r`: poll now.
    resample: bool,
    /// A detail opened: read its facts now, without moving the %CPU
    /// window.
    facts: bool,
}

struct Wake {
    asked: Mutex<Asked>,
    cond: Condvar,
}

impl Wake {
    fn ask(&self, set: impl FnOnce(&mut Asked)) {
        let mut asked = self.asked.lock().unwrap_or_else(|p| p.into_inner());
        set(&mut asked);
        self.cond.notify_all();
    }
}

fn refresh_seconds(cfg: &serde_json::Value) -> f64 {
    let mut seconds = tc::cfg_f64(cfg, "refresh", 2.0);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "-n" || args[index] == "--refresh" {
            if let Some(value) = args.get(index + 1).and_then(|t| t.parse::<f64>().ok()) {
                seconds = value;
            }
            index += 2;
            continue;
        }
        index += 1;
    }
    // NaN, infinity and anything past a day would panic in
    // `Duration::from_secs_f64` outside the poller's guard.
    tc::poll_secs(seconds, 2.0).clamp(0.5, 86_400.0)
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    tc::register_settings(SETTINGS);
    if !tc::dependencies_available("procs", include_str!("dependencies.json"), Some(SETTINGS)) {
        return;
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        tc::cannot_start_because(
            "procs",
            &tc::unsupported(),
            &["Processes are read from /proc on Linux and from ps on macOS."],
            "",
        );
        return;
    }

    let cfg = tc::load_config("procs");
    let every = refresh_seconds(&cfg);
    let mut all_users = cfg
        .get("show_all_users")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let show_kernel = cfg
        .get("show_kernel_threads")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    let shared = Arc::new(Mutex::new(Shared::default()));
    let wake = Arc::new(Wake {
        asked: Mutex::new(Asked::default()),
        cond: Condvar::new(),
    });
    let wanted: Arc<Mutex<Option<Proc>>> = Arc::new(Mutex::new(None));
    let refresh = tc::Refresh::default();
    {
        let shared = Arc::clone(&shared);
        let wake = Arc::clone(&wake);
        let wanted = Arc::clone(&wanted);
        let refresh = refresh.clone();
        thread::spawn(move || poll(shared, wake, wanted, refresh, every));
    }

    let p = view::palette();
    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut screen = Screen::List;
    let mut sort = Sort::Cpu;
    let mut needle = String::new();
    let mut typing = false;
    // The list cursor is a pid, so a refresh that reorders the table
    // leaves it on the same process; `at` is where that was last drawn.
    let mut cursor: Option<i32> = None;
    let mut at = 0usize;
    let mut drawn_keys: Vec<i32> = Vec::new();
    let mut opened: Option<Opened> = None;
    let mut scroll = 0usize;
    let mut list_scroll = 0usize;
    let mut chase = true;
    let mut placed: Vec<(usize, usize)> = Vec::new();
    let mut head = 1usize;
    let mut shown_scroll = 0usize;
    let mut table_cache: Option<(TableKey, view::Table)> = None;

    loop {
        // The filter box takes letters, `!` included, as text.
        if typing {
            keyboard.typing();
        }
        let mut keys = keyboard.poll();
        if screen == Screen::List {
            if let Some(index) =
                tc::rows_clicked(&mut keys, Some(at), head, shown_scroll, &placed, None)
            {
                at = index;
                cursor = drawn_keys.get(index).copied();
                chase = true;
            }
        } else {
            let _ = tc::rows_clicked(&mut keys, None, 0, 0, &[], None);
        }
        let step = |at: usize, down: bool, keys: &[i32]| -> (usize, Option<i32>) {
            if keys.is_empty() {
                return (0, None);
            }
            let next = if down {
                (at + 1).min(keys.len() - 1)
            } else {
                at.saturating_sub(1)
            };
            (next, Some(keys[next]))
        };
        let mut open = false;
        for key in keys {
            if typing {
                match key.as_str() {
                    "esc" => {
                        needle.clear();
                        typing = false;
                    }
                    "enter" => typing = false,
                    "backspace" => {
                        needle.pop();
                    }
                    "up" => (at, cursor) = step(at, false, &drawn_keys),
                    "down" => (at, cursor) = step(at, true, &drawn_keys),
                    other if other.chars().count() == 1 => needle.push_str(other),
                    _ => {}
                }
                chase = true;
                continue;
            }
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
                        (at, cursor) = step(at, false, &drawn_keys);
                        chase = true;
                    }
                }
                "down" | "j" | "J" => {
                    if screen == Screen::List {
                        (at, cursor) = step(at, true, &drawn_keys);
                        chase = true;
                    }
                }
                // Acted on after this batch's arrows have moved the cursor,
                // so an arrow and an enter read together open the new row.
                "enter" => open = screen == Screen::List,
                "esc" => {
                    if screen == Screen::Detail {
                        screen = Screen::List;
                        opened = None;
                        *wanted.lock().unwrap_or_else(|p| p.into_inner()) = None;
                        scroll = list_scroll;
                        chase = true;
                    } else if !needle.is_empty() {
                        needle.clear();
                        chase = true;
                    }
                }
                "s" | "S" => {
                    if screen == Screen::List {
                        sort = sort.next();
                        chase = true;
                    }
                }
                "/" => {
                    if screen == Screen::List {
                        typing = true;
                    }
                }
                "u" | "U" => {
                    if screen == Screen::List {
                        all_users = !all_users;
                        chase = true;
                    }
                }
                "r" | "R" => wake.ask(|asked| asked.resample = true),
                "ctrl-y" | "wheel-up" => {
                    scroll = scroll.saturating_sub(1);
                    chase = false;
                }
                "ctrl-e" | "wheel-down" => {
                    scroll = scroll.saturating_add(1);
                    chase = false;
                }
                "pgup" => {
                    scroll = scroll.saturating_sub(tc::size().1.saturating_sub(4).max(1));
                    chase = false;
                }
                "pgdn" => {
                    scroll = scroll.saturating_add(tc::size().1.saturating_sub(4).max(1));
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

        let (sample, err, sampled, facts) = {
            let guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            (
                guard.sample.clone(),
                guard.err.clone(),
                guard.sampled,
                guard.facts.clone(),
            )
        };
        if open {
            let found = cursor.and_then(|pid| {
                sample
                    .as_ref()
                    .and_then(|s| s.procs.iter().find(|p| p.pid == pid).cloned())
            });
            if let Some(proc) = found {
                *wanted.lock().unwrap_or_else(|p| p.into_inner()) = Some(proc.clone());
                wake.ask(|asked| asked.facts = true);
                opened = Some(Opened {
                    pid: proc.pid,
                    start: proc.start,
                    last: proc,
                    gone: false,
                    facts: None,
                });
                list_scroll = scroll;
                scroll = 0;
                screen = Screen::Detail;
            }
        }
        if let (Some(open), Some(sample)) = (opened.as_mut(), sample.as_ref()) {
            match sample
                .procs
                .iter()
                .find(|p| p.pid == open.pid && p.start == open.start)
            {
                Some(now) if !open.gone => open.last = now.clone(),
                _ => open.gone = true,
            }
        }
        if let Some(open) = opened.as_mut() {
            // Facts for the same process, and only while it is there: a
            // read after it left could be of a pid already reused.
            if let Some(read) = facts.filter(|f| f.pid == open.pid && f.start == open.start) {
                if !open.gone {
                    open.facts = Some(read);
                }
            }
        }

        let (w, h) = tc::size();
        let state = view::View {
            sample: sample.as_deref(),
            err: &err,
            age: sampled.map(|at| at.elapsed()),
            every,
            sort,
            needle: &needle,
            typing,
            all_users,
            show_kernel,
            cursor,
            opened: opened.as_ref(),
        };
        let busy = refresh.tick(tc::animation_tick());
        let mut drawn = match screen {
            Screen::List => {
                let table = cached(&mut table_cache, &state, w, &p);
                view::compose(view::list_head(&state, w, &p, busy), table)
            }
            Screen::Detail => view::draw_detail(&state, w, &p, busy),
        };
        if screen == Screen::List {
            // The cursor's process left, or the filter took it, or there was
            // none yet: the cursor takes whatever now stands where it was,
            // and the frame is drawn again with that row tinted.
            let before = cursor;
            match cursor.and_then(|pid| drawn.keys.iter().position(|k| *k == pid)) {
                Some(index) => at = index,
                None => {
                    at = at.min(drawn.keys.len().saturating_sub(1));
                    cursor = drawn.keys.get(at).copied();
                }
            }
            if cursor != before {
                let state = view::View { cursor, ..state };
                let table = cached(&mut table_cache, &state, w, &p);
                drawn = view::compose(view::list_head(&state, w, &p, busy), table);
            }
            drawn_keys = drawn.keys.clone();
        }
        let (packed, indent) = view::footer(
            w,
            &view::Keys {
                screen,
                sort,
                typing,
                all_users,
                filtered: !needle.is_empty(),
            },
            &p,
        );
        let foot = packed.lines.clone();
        let room = h.saturating_sub(foot.len());
        let head_rows = drawn.head.min(drawn.rows.len());
        let rest = drawn.rows.len().saturating_sub(head_rows);
        let room_below = room.saturating_sub(head_rows).max(1);
        if chase && screen == Screen::List {
            if let Some(row) = drawn
                .placed
                .iter()
                .filter(|(_, item)| *item == at)
                .map(|(row, _)| *row)
                .min()
            {
                let last = drawn
                    .placed
                    .iter()
                    .filter(|(_, item)| *item == at)
                    .map(|(row, _)| *row)
                    .max()
                    .unwrap_or(row);
                let top = row.saturating_sub(head_rows);
                let bottom = last.saturating_sub(head_rows);
                scroll = tc::follow(scroll, bottom, room_below);
                scroll = tc::follow(scroll, top, room_below);
            }
        }
        scroll = scroll.min(rest.saturating_sub(room_below.min(rest)));
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
        // Frame-local: only the frame a key moved the cursor follows it, or
        // the follow drags the view back from wherever the wheel put it.
        chase = false;
        keyboard.wait(Duration::from_millis(200));
    }
}

/// Everything the table's rows depend on. The table is most of a frame's
/// cost and changes once a poll; the frame is drawn five times a second
/// for the clock and the spinner, so the rows are kept until one of these
/// moves.
#[derive(Clone, Debug, PartialEq)]
struct TableKey {
    sample: Option<usize>,
    sort: Sort,
    needle: String,
    all_users: bool,
    cursor: Option<i32>,
    width: usize,
}

fn cached<'a>(
    cache: &'a mut Option<(TableKey, view::Table)>,
    state: &view::View,
    width: usize,
    p: &view::Palette,
) -> &'a view::Table {
    let key = TableKey {
        sample: state.sample.map(|s| s as *const Sample as usize),
        sort: state.sort,
        needle: state.needle.to_string(),
        all_users: state.all_users,
        cursor: state.cursor,
        width,
    };
    if cache.as_ref().is_none_or(|(held, _)| *held != key) {
        *cache = Some((key, view::list_table(state, width, p)));
    }
    &cache.as_ref().expect("just filled").1
}

/// The open process as the latest table has it, if it is still the same
/// process.
fn still_there(sample: Option<&Sample>, wanted: &Proc) -> Option<Proc> {
    sample?
        .procs
        .iter()
        .find(|p| p.pid == wanted.pid && p.start == wanted.start)
        .cloned()
}

fn poll(
    shared: Arc<Mutex<Shared>>,
    wake: Arc<Wake>,
    wanted: Arc<Mutex<Option<Proc>>>,
    refresh: tc::Refresh,
    every: f64,
) {
    tc::record_panics();
    let mut source = host::Source::default();
    let mut last: Option<Arc<Sample>> = None;
    loop {
        let work = refresh.start();
        let want = wanted.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let round = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let sample = host::sample(&mut source);
            let facts = match (&want, &sample) {
                (Some(proc), Ok(table)) => {
                    // A process that left is not read again: its pid may
                    // already be someone else's.
                    still_there(Some(table), proc).map(|now| host::facts(&mut source, &now))
                }
                _ => {
                    host::forget(&mut source);
                    None
                }
            };
            (sample, facts)
        }));
        match round {
            Ok((sample, facts)) => {
                let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
                match sample {
                    Ok(table) => {
                        let table = Arc::new(table);
                        last = Some(Arc::clone(&table));
                        guard.sample = Some(table);
                        guard.err.clear();
                        guard.sampled = Some(Instant::now());
                    }
                    Err(why) => guard.err = why,
                }
                guard.facts = facts;
            }
            Err(_) => {
                let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
                guard.err = tc::take_panic_reason()
                    .map(|why| format!("stopped: {why}"))
                    .unwrap_or_else(|| "stopped: poller panicked".into());
                drop(work);
                return;
            }
        }
        drop(work);

        // Until the next poll, answering a detail that opens in between
        // without moving the %CPU window.
        let until = Instant::now() + Duration::from_secs_f64(every);
        loop {
            let asked = {
                let mut asked = wake.asked.lock().unwrap_or_else(|p| p.into_inner());
                while !asked.resample && !asked.facts {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    asked = match wake.cond.wait_timeout(asked, left) {
                        Ok((guard, _)) => guard,
                        Err(poisoned) => poisoned.into_inner().0,
                    };
                }
                std::mem::take(&mut *asked)
            };
            if asked.resample || Instant::now() >= until {
                break;
            }
            if asked.facts {
                let work = refresh.start();
                let want = wanted.lock().unwrap_or_else(|p| p.into_inner()).clone();
                let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    want.as_ref().and_then(|proc| {
                        still_there(last.as_deref(), proc).map(|now| host::facts(&mut source, &now))
                    })
                }));
                let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
                match read {
                    Ok(facts) => guard.facts = facts,
                    Err(_) => {
                        guard.err = tc::take_panic_reason()
                            .map(|why| format!("stopped: {why}"))
                            .unwrap_or_else(|| "stopped: poller panicked".into());
                        drop(work);
                        return;
                    }
                }
                drop(work);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_is_never_under_half_a_second() {
        let cfg = serde_json::json!({ "refresh": 0.1 });
        assert_eq!(refresh_seconds(&cfg), 0.5);
        let cfg = serde_json::json!({});
        assert_eq!(refresh_seconds(&cfg), 2.0);
    }
}
