// opscope — Copyright (C) 2026 William Li — AGPL-3.0-or-later
//! Local coding-agent processes and the work below them, without hooks or a mux.
use opscope_core as tc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod host;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod host;
mod model;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod parse;
mod view;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod host {
    pub fn cwds(pids: &[u32]) -> std::collections::HashMap<u32, Result<String, String>> {
        pids.iter()
            .map(|&p| (p, Err(opscope_core::unsupported())))
            .collect()
    }
}

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "agent-activity",
    section: "agent_activity",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};

struct Control {
    every: f64,
    requested: bool,
    stop: bool,
}
struct Wake {
    control: Mutex<Control>,
    ready: Condvar,
}
impl Wake {
    fn request(&self, every: Option<f64>) {
        let mut control = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(every) = every {
            control.every = every;
        }
        control.requested = true;
        self.ready.notify_all();
    }
}

fn refresh_seconds(cfg: &serde_json::Value) -> f64 {
    let default = tc::cfg_f64(cfg, "refresh", 2.0);
    let args: Vec<_> = std::env::args().collect();
    let command = args
        .windows(2)
        .find(|a| a[0] == "-n" || a[0] == "--refresh")
        .and_then(|a| a[1].parse().ok())
        .unwrap_or(default);
    tc::poll_secs(command, 2.0).clamp(0.5, 86_400.0)
}

fn poll(shared: Arc<Mutex<model::Snapshot>>, wake: Arc<Wake>, refresh: tc::Refresh) {
    let mut reader = tc::agent_work::LogReader::default();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loop {
        {
            let mut control = wake.control.lock().unwrap_or_else(|p| p.into_inner());
            if control.stop {
                break;
            }
            control.requested = false;
        }
        let reading = refresh.start();
        let sample = tc::agent_work::read_process_table().map(|table| {
            let pids: Vec<_> = table
                .agents(model::NAMES)
                .iter()
                .map(|(pid, _)| *pid)
                .collect();
            let cwds = host::cwds(&pids);
            model::collect(&table, &mut reader, &cwds)
        });
        {
            let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
            match sample {
                Ok(agents) => {
                    state.agents = agents;
                    state.sampled = Some(std::time::Instant::now());
                    state.err = None;
                }
                Err(why) => state.err = Some(why),
            }
        }
        drop(reading);
        let control = wake.control.lock().unwrap_or_else(|p| p.into_inner());
        let delay = Duration::from_secs_f64(control.every);
        let _wait = wake
            .ready
            .wait_timeout_while(control, delay, |c| !c.requested && !c.stop)
            .unwrap_or_else(|p| p.into_inner());
    }));
    if let Err(panic) = result {
        let why = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown panic".into());
        shared.lock().unwrap_or_else(|p| p.into_inner()).err =
            Some(format!("poller stopped: {why}"));
    }
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    tc::register_settings(SETTINGS);
    if !tc::dependencies_available(
        "agent-activity",
        include_str!("dependencies.json"),
        Some(SETTINGS),
    ) {
        return;
    }
    let cfg = tc::load_config("agent_activity");
    let mut recent = tc::cfg_usize(&cfg, "finished_per_agent", 3).min(100);
    let shared = Arc::new(Mutex::new(model::Snapshot::default()));
    let wake = Arc::new(Wake {
        control: Mutex::new(Control {
            every: refresh_seconds(&cfg),
            requested: true,
            stop: false,
        }),
        ready: Condvar::new(),
    });
    let refresh = tc::Refresh::default();
    {
        let (shared, wake, refresh) = (Arc::clone(&shared), Arc::clone(&wake), refresh.clone());
        std::thread::spawn(move || poll(shared, wake, refresh));
    }
    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut at = 0usize;
    let mut selected: Option<(u32, String)> = None;
    let mut opened: Option<model::Agent> = None;
    let mut scroll = 0usize;
    let mut list_scroll = 0usize;
    let mut room = 1usize;
    let mut placed = Vec::new();
    let mut drawn: Vec<model::Agent> = Vec::new();
    'screen: loop {
        let mut chase = false;
        let mut keys = keyboard.poll();
        if opened.is_none() {
            if let Some(index) = tc::rows_clicked(&mut keys, Some(at), 1, scroll, &placed, None) {
                at = index;
                selected = drawn.get(at).map(model::Agent::key);
                chase = true;
            }
        }
        for key in keys {
            match key.as_str() {
                "q" | "Q" | "ctrl-c" => break 'screen,
                "esc" => {
                    if opened.take().is_some() {
                        scroll = list_scroll;
                    }
                }
                "up" | "k" if opened.is_none() => {
                    at = at.saturating_sub(1);
                    selected = drawn.get(at).map(model::Agent::key);
                    chase = true;
                }
                "down" | "j" if opened.is_none() => {
                    at = (at + 1).min(drawn.len().saturating_sub(1));
                    selected = drawn.get(at).map(model::Agent::key);
                    chase = true;
                }
                "up" | "k" | "ctrl-y" | "wheel-up" => scroll = scroll.saturating_sub(1),
                "down" | "j" | "ctrl-e" | "wheel-down" => scroll = scroll.saturating_add(1),
                "pgup" => scroll = scroll.saturating_sub(room.max(1)),
                "pgdn" => scroll = scroll.saturating_add(room.max(1)),
                "home" => scroll = 0,
                "end" => scroll = usize::MAX,
                "enter" if opened.is_none() => {
                    if let Some(a) = drawn.get(at) {
                        opened = Some(a.clone());
                        list_scroll = scroll;
                        scroll = 0;
                    }
                }
                "r" | "R" => wake.request(None),
                "," => {
                    tc::run_settings(&mut keyboard, SETTINGS);
                    let cfg = tc::load_config("agent_activity");
                    recent = tc::cfg_usize(&cfg, "finished_per_agent", 3).min(100);
                    wake.request(Some(refresh_seconds(&cfg)));
                }
                _ => {}
            }
        }
        let snapshot = shared.lock().unwrap_or_else(|p| p.into_inner()).clone();
        drawn = snapshot.agents.clone();
        at = selected
            .as_ref()
            .and_then(|key| drawn.iter().position(|a| a.key() == *key))
            .unwrap_or_else(|| at.min(drawn.len().saturating_sub(1)));
        selected = drawn.get(at).map(model::Agent::key);
        let (width, height) = tc::size();
        let fg = tc::ink(tc::Role::Muted);
        let mut hints = vec![
            vec![(fg.as_str(), "↑↓ select".into())],
            vec![(fg.as_str(), "[↵]sources".into())],
        ];
        if opened.is_some() {
            hints = vec![
                vec![(fg.as_str(), "[esc]back".into())],
                vec![(fg.as_str(), "↑↓ scroll".into())],
            ];
        }
        hints.extend([
            vec![(fg.as_str(), "[r]refresh".into())],
            vec![(fg.as_str(), "[,]settings".into())],
            vec![(fg.as_str(), "[q]quit".into())],
            vec![(fg.as_str(), tc::PALETTE_HINT.into())],
            vec![(fg.as_str(), tc::THEME_HINT.into())],
        ]);
        let footer = tc::pack_hints_placed(&hints, width, "  ");
        room = height.saturating_sub(1 + footer.lines.len());
        let body = if let Some(captured) = opened.as_mut() {
            let current = drawn.iter().find(|a| a.key() == captured.key());
            if let Some(a) = current {
                *captured = a.clone();
            }
            view::detail(&snapshot, captured, current.is_some(), width, recent)
        } else {
            view::board(&snapshot, at, width, recent)
        };
        scroll = view::viewport(&body, at, scroll, room, chase && opened.is_none());
        let mut rows = vec![tc::title_busy(
            "AGENT ACTIVITY",
            width,
            &tc::ink(tc::Role::Accent),
            refresh.tick(tc::animation_tick()),
        )];
        rows.extend(body.rows.iter().skip(scroll).take(room).cloned());
        while rows.len() < 1 + room {
            rows.push(String::new());
        }
        let footer_top = rows.len();
        rows.extend(footer.lines.clone());
        tc::draw(&rows, width, height);
        keyboard.footer_at(&footer, footer_top, 0);
        placed = if opened.is_none() {
            body.placed
                .into_iter()
                .map(|(row, i)| (row + 1, i))
                .collect()
        } else {
            Vec::new()
        };
        keyboard.wait(Duration::from_millis(200));
    }
    wake.control.lock().unwrap_or_else(|p| p.into_inner()).stop = true;
    wake.ready.notify_all();
    tc::restore_screen();
}
